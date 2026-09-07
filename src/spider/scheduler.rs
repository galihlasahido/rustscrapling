//! The priority queue and duplicate filter a crawl schedules through.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use super::request::Request;

/// One queued request, ordered so that a `BinaryHeap` pops the highest priority first and,
/// within a priority, the request that was queued first.
#[derive(Debug, Clone)]
struct Entry {
    priority: i32,
    counter: u64,
    request: Request,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.counter == other.counter
    }
}

impl Eq for Entry {}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Highest priority first, then the lowest counter (FIFO) first.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.counter.cmp(&self.counter))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A priority queue of requests with a fingerprint-based duplicate filter.
///
/// Both the queue and the duplicate filter are fed from links found in remote markup, so both
/// can be given a ceiling — see [`Scheduler::with_limits`]. Past a ceiling a request is refused
/// (`enqueue` returns `false`) rather than queued; the seen set only ever grows by one entry per
/// accepted request, so bounding the accepted total bounds it too.
#[derive(Debug)]
pub struct Scheduler {
    queue: BinaryHeap<Entry>,
    seen: HashSet<[u8; 20]>,
    counter: u64,
    include_kwargs: bool,
    include_headers: bool,
    keep_fragments: bool,
    /// The most requests that may sit in the queue at once; `0` means "no ceiling".
    max_queued: usize,
    /// The most requests this scheduler will ever accept; `0` means "no ceiling".
    max_total: usize,
    /// How many requests have been accepted so far, for `max_total`.
    accepted: u64,
    /// Whether a ceiling has already been reported, so the log says it once and not once a link.
    reported_full: bool,
}

impl Scheduler {
    /// A scheduler with the spider's fingerprint settings and no ceilings.
    pub fn new(include_kwargs: bool, include_headers: bool, keep_fragments: bool) -> Scheduler {
        Scheduler::with_limits(include_kwargs, include_headers, keep_fragments, 0, 0)
    }

    /// A scheduler that refuses to grow past `max_queued` queued requests, or to accept more
    /// than `max_total` requests in total. Either ceiling is switched off with `0`.
    pub fn with_limits(
        include_kwargs: bool,
        include_headers: bool,
        keep_fragments: bool,
        max_queued: usize,
        max_total: usize,
    ) -> Scheduler {
        Scheduler {
            queue: BinaryHeap::new(),
            seen: HashSet::new(),
            counter: 0,
            include_kwargs,
            include_headers,
            keep_fragments,
            max_queued,
            max_total,
            accepted: 0,
            reported_full: false,
        }
    }

    /// Whether one more request may be accepted, logging the first refusal.
    fn has_room(&mut self, request: &Request) -> bool {
        let full = if self.max_queued > 0 && self.queue.len() >= self.max_queued {
            Some(("the queue is full", self.max_queued))
        } else if self.max_total > 0 && self.accepted >= self.max_total as u64 {
            Some(("the crawl hit its request ceiling", self.max_total))
        } else {
            None
        };
        let Some((why, limit)) = full else {
            return true;
        };
        if !self.reported_full {
            self.reported_full = true;
            tracing::warn!(
                url = %request.url,
                limit,
                "{why}; further requests are dropped"
            );
        } else {
            tracing::debug!(url = %request.url, limit, "{why}; request dropped");
        }
        false
    }

    /// The fingerprint this scheduler would compute for a request.
    pub fn fingerprint(&self, request: &Request) -> [u8; 20] {
        request.fingerprint(
            self.include_kwargs,
            self.include_headers,
            self.keep_fragments,
        )
    }

    /// Queue a request; returns false when it was dropped as a duplicate or refused because a
    /// ceiling set by [`Scheduler::with_limits`] was reached.
    pub fn enqueue(&mut self, request: Request) -> bool {
        let fingerprint = self.fingerprint(&request);

        if !request.dont_filter && self.seen.contains(&fingerprint) {
            tracing::debug!(url = %request.url, "dropped duplicate request");
            return false;
        }
        if !self.has_room(&request) {
            return false;
        }
        self.seen.insert(fingerprint);
        self.accepted = self.accepted.saturating_add(1);

        let counter = self.counter;
        self.counter = self.counter.wrapping_add(1);
        self.queue.push(Entry {
            priority: request.priority,
            counter,
            request,
        });
        true
    }

    /// Take the highest-priority request, FIFO within a priority.
    pub fn dequeue(&mut self) -> Option<Request> {
        self.queue.pop().map(|entry| entry.request)
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// How many requests are queued.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// The queued requests plus the seen set, for a checkpoint. The requests come back in the
    /// order they would have been dequeued in.
    pub fn snapshot(&self) -> (Vec<Request>, HashSet<[u8; 20]>) {
        let mut entries: Vec<&Entry> = self.queue.iter().collect();
        entries.sort_by(|left, right| right.cmp(left));
        let requests = entries
            .into_iter()
            .map(|entry| entry.request.clone())
            .collect();
        (requests, self.seen.clone())
    }

    /// Reload a snapshot taken earlier.
    pub fn restore(&mut self, requests: Vec<Request>, seen: HashSet<[u8; 20]>) {
        self.seen = seen;
        for request in requests {
            let counter = self.counter;
            self.counter = self.counter.wrapping_add(1);
            self.queue.push(Entry {
                priority: request.priority,
                counter,
                request,
            });
        }
        tracing::info!(
            queued = self.queue.len(),
            seen = self.seen.len(),
            "scheduler restored"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(scheduler: &mut Scheduler) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(request) = scheduler.dequeue() {
            out.push(request.url);
        }
        out
    }

    #[test]
    fn higher_priority_runs_first_and_ties_are_fifo() {
        let mut scheduler = Scheduler::new(false, false, false);
        assert!(scheduler.enqueue(Request::new("http://e.com/a")));
        assert!(scheduler.enqueue(Request::new("http://e.com/b").priority(5)));
        assert!(scheduler.enqueue(Request::new("http://e.com/c")));
        assert!(scheduler.enqueue(Request::new("http://e.com/d").priority(5)));
        assert_eq!(scheduler.len(), 4);

        assert_eq!(
            urls(&mut scheduler),
            vec![
                "http://e.com/b".to_string(),
                "http://e.com/d".to_string(),
                "http://e.com/a".to_string(),
                "http://e.com/c".to_string(),
            ]
        );
        assert!(scheduler.is_empty());
        assert!(scheduler.dequeue().is_none());
    }

    #[test]
    fn negative_priorities_run_last() {
        let mut scheduler = Scheduler::new(false, false, false);
        scheduler.enqueue(Request::new("http://e.com/a"));
        scheduler.enqueue(
            Request::new("http://e.com/retry")
                .priority(-1)
                .dont_filter(true),
        );
        assert_eq!(
            urls(&mut scheduler),
            vec![
                "http://e.com/a".to_string(),
                "http://e.com/retry".to_string()
            ]
        );
    }

    #[test]
    fn duplicates_are_dropped_unless_dont_filter() {
        let mut scheduler = Scheduler::new(false, false, false);
        assert!(scheduler.enqueue(Request::new("http://e.com/a")));
        assert!(!scheduler.enqueue(Request::new("http://e.com/a")));
        // The same page with the query in another order is still the same page.
        assert!(scheduler.enqueue(Request::new("http://e.com/b?x=1&y=2")));
        assert!(!scheduler.enqueue(Request::new("http://e.com/b?y=2&x=1")));
        assert!(scheduler.enqueue(Request::new("http://e.com/a").dont_filter(true)));
        assert_eq!(scheduler.len(), 3);
    }

    #[test]
    fn snapshot_keeps_dequeue_order_and_restores() {
        let mut scheduler = Scheduler::new(false, false, false);
        scheduler.enqueue(Request::new("http://e.com/a"));
        scheduler.enqueue(Request::new("http://e.com/b").priority(9));
        scheduler.enqueue(Request::new("http://e.com/c"));

        let (requests, seen) = scheduler.snapshot();
        assert_eq!(
            requests.iter().map(|r| r.url.clone()).collect::<Vec<_>>(),
            vec![
                "http://e.com/b".to_string(),
                "http://e.com/a".to_string(),
                "http://e.com/c".to_string()
            ]
        );
        assert_eq!(seen.len(), 3);

        let mut restored = Scheduler::new(false, false, false);
        restored.restore(requests, seen);
        assert_eq!(restored.len(), 3);
        assert_eq!(
            urls(&mut restored),
            vec![
                "http://e.com/b".to_string(),
                "http://e.com/a".to_string(),
                "http://e.com/c".to_string()
            ]
        );
    }

    /// A link bomb must not grow the queue or the seen set without bound: past the ceilings the
    /// scheduler refuses the request instead of allocating for it.
    #[test]
    fn ceilings_bound_the_queue_and_the_seen_set() {
        let mut scheduler = Scheduler::with_limits(false, false, false, 3, 0);
        for n in 0..10 {
            scheduler.enqueue(Request::new(format!("http://e.com/{n}")));
        }
        assert_eq!(scheduler.len(), 3, "the queue must stop at its ceiling");
        let (_, seen) = scheduler.snapshot();
        assert_eq!(seen.len(), 3, "a refused request is not remembered either");

        // Dequeuing makes room again, up to the total ceiling.
        let mut scheduler = Scheduler::with_limits(false, false, false, 2, 5);
        for n in 0..10 {
            scheduler.enqueue(Request::new(format!("http://e.com/{n}")));
            let _ = scheduler.dequeue();
        }
        let (_, seen) = scheduler.snapshot();
        assert_eq!(seen.len(), 5, "the crawl-wide ceiling stops the seen set");
        assert!(!scheduler.enqueue(Request::new("http://e.com/late")));

        // `0` keeps the old, unbounded behaviour, which is what `new` gives.
        let mut unbounded = Scheduler::new(false, false, false);
        for n in 0..200 {
            assert!(unbounded.enqueue(Request::new(format!("http://e.com/{n}"))));
        }
        assert_eq!(unbounded.len(), 200);
    }

    #[test]
    fn a_restored_seen_set_still_filters() {
        let mut scheduler = Scheduler::new(false, false, false);
        scheduler.enqueue(Request::new("http://e.com/a"));
        let (_, seen) = scheduler.snapshot();

        let mut restored = Scheduler::new(false, false, false);
        restored.restore(Vec::new(), seen);
        assert!(!restored.enqueue(Request::new("http://e.com/a")));
    }
}
