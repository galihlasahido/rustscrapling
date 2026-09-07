//! The crawl loop, a port of `scrapling/spiders/engine.py` onto `tokio`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::error::Result;
use crate::response::{Response, META_CALLBACK, META_PRIORITY, META_SID};

use super::cache::ResponseCache;
use super::checkpoint::{CheckpointData, CheckpointManager};
use super::items::Items;
use super::request::{Callback, Request};
use super::robots::RobotsManager;
use super::scheduler::Scheduler;
use super::session::{Session, SessionManager};
use super::spider::{Output, Spider, SpiderConfig};
use super::stats::{CrawlResult, CrawlStats};
use super::throttle::{parse_retry_after, AutoThrottle};

/// How long the loop naps when it has nothing to start.
const IDLE_TICK: Duration = Duration::from_millis(50);
/// How long the loop naps when it is at its concurrency ceiling.
const BUSY_TICK: Duration = Duration::from_millis(10);
/// A sanity ceiling on a single computed delay, so a hostile header cannot stall a crawl.
const MAX_SLEEP_SECONDS: f64 = 3600.0;

/// Whether a per-domain map may take one more entry.
///
/// The hosts a crawl sees come out of scraped links, so every map keyed by domain is capped by
/// `SpiderConfig::max_tracked_domains`; a full map keeps working for the domains already in it
/// and stops memoizing the rest. `0` means "no ceiling".
fn room_for_a_domain(tracked: usize, ceiling: usize, domain: &str) -> bool {
    if ceiling == 0 || tracked < ceiling {
        return true;
    }
    tracing::debug!(
        domain,
        ceiling,
        "not tracking another domain; the per-domain map is at its ceiling"
    );
    false
}

/// How the engine runs a spider.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EngineOptions {
    /// Where checkpoints are written; `None` disables pause/resume.
    pub crawldir: Option<PathBuf>,
    /// Seconds between periodic checkpoint saves. Default 300.0; 0 disables them.
    pub checkpoint_interval: f64,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            crawldir: None,
            checkpoint_interval: 300.0,
        }
    }
}

impl EngineOptions {
    /// Options with the defaults.
    pub fn new() -> EngineOptions {
        EngineOptions::default()
    }

    /// Write checkpoints into this directory, enabling pause and resume.
    pub fn crawldir(mut self, crawldir: impl Into<PathBuf>) -> Self {
        self.crawldir = Some(crawldir.into());
        self
    }

    /// Seconds between periodic checkpoint saves; 0 disables them.
    pub fn checkpoint_interval(mut self, interval: f64) -> Self {
        self.checkpoint_interval = interval;
        self
    }
}

/// Take a lock, recovering the value even when another task panicked while holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Log a finished crawl task. A panic inside a spider callback is reported here rather than
/// taking the whole crawl down; its slot was already released by [`TaskSlot`].
fn report_task(finished: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(error) = finished {
        if error.is_panic() {
            tracing::error!(%error, "a crawl task panicked");
        } else {
            tracing::debug!(%error, "a crawl task was cancelled");
        }
    }
}

/// Seconds since the Unix epoch, as Python's `time.time()`.
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_secs_f64())
        .unwrap_or(0.0)
}

/// A running request's slot in the crawl: dropping it frees the concurrency slot and takes the
/// request out of the in-flight set, however the task ended.
struct TaskSlot {
    inner: Arc<Inner>,
    task_id: u64,
}

impl Drop for TaskSlot {
    fn drop(&mut self) {
        lock(&self.inner.inflight).remove(&self.task_id);
        self.inner.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Everything a running crawl shares between its tasks.
struct Inner {
    spider: Arc<dyn Spider>,
    sessions: Arc<SessionManager>,
    config: SpiderConfig,
    allowed_domains: Vec<String>,
    default_sid: String,

    scheduler: Mutex<Scheduler>,
    stats: Mutex<CrawlStats>,
    items: Mutex<Items>,
    item_tx: Mutex<Option<mpsc::Sender<Value>>>,
    inflight: Mutex<HashMap<u64, Request>>,
    next_task_id: AtomicU64,

    throttle: Option<Mutex<AutoThrottle>>,
    robots: Option<RobotsManager>,
    cache: Option<ResponseCache>,
    checkpoints: Option<CheckpointManager>,
    last_checkpoint: Mutex<Instant>,

    global_permits: Arc<Semaphore>,
    domain_permits: Mutex<HashMap<String, Arc<Semaphore>>>,
    domain_delays: Mutex<HashMap<String, f64>>,

    active: AtomicUsize,
    pause_requested: AtomicBool,
    force_stop: AtomicBool,
    paused: AtomicBool,
}

impl Inner {
    fn with_stats<R>(&self, apply: impl FnOnce(&mut CrawlStats) -> R) -> R {
        apply(&mut lock(&self.stats))
    }

    fn sid_of(&self, request: &Request) -> String {
        if request.sid.is_empty() {
            self.default_sid.clone()
        } else {
            request.sid.clone()
        }
    }

    /// Resolve an empty session id before the request is fingerprinted, so two requests that
    /// use the same session really do hash the same.
    fn normalize(&self, mut request: Request) -> Request {
        if request.sid.is_empty() {
            request.sid = self.default_sid.clone();
        }
        request
    }

    fn enqueue(&self, request: Request) -> bool {
        lock(&self.scheduler).enqueue(request)
    }

    fn dequeue(&self) -> Option<Request> {
        lock(&self.scheduler).dequeue()
    }

    fn queue_is_empty(&self) -> bool {
        lock(&self.scheduler).is_empty()
    }

    fn fingerprint(&self, request: &Request) -> [u8; 20] {
        lock(&self.scheduler).fingerprint(request)
    }

    fn is_domain_allowed(&self, request: &Request) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
        let domain = request.domain();
        self.allowed_domains
            .iter()
            .any(|allowed| domain == *allowed || domain.ends_with(&format!(".{allowed}")))
    }

    /// The per-domain concurrency limiter, when the spider asked for one.
    ///
    /// Hosts come from scraped links, so the map is capped: past `max_tracked_domains` a new
    /// domain gets no entry of its own and falls back to the global limiter, rather than adding
    /// a `Semaphore` per hostname an attacker can invent.
    fn domain_permits(&self, domain: &str) -> Option<Arc<Semaphore>> {
        let limit = self.config.concurrent_requests_per_domain;
        if limit == 0 {
            return None;
        }
        let mut permits = lock(&self.domain_permits);
        if let Some(existing) = permits.get(domain) {
            return Some(Arc::clone(existing));
        }
        if !room_for_a_domain(permits.len(), self.config.max_tracked_domains, domain) {
            return None;
        }
        Some(Arc::clone(
            permits
                .entry(domain.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(limit))),
        ))
    }

    /// The delay floor for a domain: the spider's own delay, raised by robots.txt's
    /// `Crawl-delay` when the spider obeys robots.
    async fn domain_delay(&self, domain: &str, url: &str) -> f64 {
        let Some(robots) = &self.robots else {
            return self.config.download_delay;
        };
        if let Some(delay) = lock(&self.domain_delays).get(domain) {
            return *delay;
        }

        let mut delay = self.config.download_delay;
        if let Some(crawl_delay) = robots.crawl_delay(url).await {
            delay = delay.max(crawl_delay);
        }
        // Same ceiling as `domain_permits`: the delay is still applied, it just stops being
        // memoized once the map is full.
        let mut delays = lock(&self.domain_delays);
        if room_for_a_domain(delays.len(), self.config.max_tracked_domains, domain) {
            delays.insert(domain.to_string(), delay);
        }
        delay
    }

    fn request_pause(&self) {
        if self.force_stop.load(Ordering::SeqCst) {
            return;
        }
        if self.pause_requested.swap(true, Ordering::SeqCst) {
            self.force_stop.store(true, Ordering::SeqCst);
            tracing::warn!("force stop requested, cancelling immediately");
        } else {
            tracing::info!(
                "pause requested, waiting for in-flight requests to finish (ask again to force a stop)"
            );
        }
    }

    /// The queued requests plus the ones still in flight, so a pause loses nothing.
    fn checkpoint_data(&self) -> CheckpointData {
        let (mut requests, seen) = lock(&self.scheduler).snapshot();
        requests.extend(lock(&self.inflight).values().cloned());
        CheckpointData::from_snapshot(requests, seen)
    }

    async fn save_checkpoint(&self) {
        let Some(manager) = &self.checkpoints else {
            return;
        };
        let data = self.checkpoint_data();
        if let Err(error) = manager.save(&data).await {
            tracing::error!(%error, "failed to save the checkpoint");
        }
        *lock(&self.last_checkpoint) = Instant::now();
    }

    fn is_checkpoint_time(&self) -> bool {
        let Some(manager) = &self.checkpoints else {
            return false;
        };
        let interval = manager.interval();
        if interval <= 0.0 {
            return false;
        }
        lock(&self.last_checkpoint).elapsed().as_secs_f64() >= interval
    }

    async fn restore_checkpoint(&self) -> bool {
        let Some(manager) = &self.checkpoints else {
            return false;
        };
        let Some(data) = manager.load().await else {
            return false;
        };
        let seen = data.seen_fingerprints();
        lock(&self.scheduler).restore(data.requests, seen);
        true
    }

    /// Warm the robots.txt cache for one seed URL per start domain.
    async fn prefetch_robots(&self) {
        let Some(robots) = &self.robots else {
            return;
        };
        let mut seen: Vec<String> = Vec::new();
        let mut seeds: Vec<String> = Vec::new();
        for url in self.spider.start_urls() {
            if let Ok(parsed) = ::url::Url::parse(&url) {
                let mut authority = parsed.host_str().unwrap_or("").to_string();
                if let Some(port) = parsed.port() {
                    authority.push(':');
                    authority.push_str(&port.to_string());
                }
                if authority.is_empty() || seen.contains(&authority) {
                    continue;
                }
                seen.push(authority.clone());
                seeds.push(format!("{}://{}/", parsed.scheme(), authority));
            }
        }
        robots.prefetch(&seeds).await;
    }

    fn reset(&self) {
        lock(&self.items).clear();
        lock(&self.domain_permits).clear();
        lock(&self.domain_delays).clear();
        lock(&self.inflight).clear();
        self.paused.store(false, Ordering::SeqCst);
        self.pause_requested.store(false, Ordering::SeqCst);
        self.force_stop.store(false, Ordering::SeqCst);
        self.active.store(0, Ordering::SeqCst);
        if let Some(throttle) = &self.throttle {
            lock(throttle).reset();
        }

        let mut stats = CrawlStats {
            start_time: now_seconds(),
            concurrent_requests: self.config.concurrent_requests,
            concurrent_requests_per_domain: self.config.concurrent_requests_per_domain,
            download_delay: self.config.download_delay,
            autothrottle_enabled: self.throttle.is_some(),
            ..CrawlStats::default()
        };
        std::mem::swap(&mut *lock(&self.stats), &mut stats);
    }

    /// Fetch one request and hand the response to the spider.
    async fn process_request(&self, request: Request) {
        let domain = request.domain();
        let sid = self.sid_of(&request);

        let floor = if let Some(robots) = &self.robots {
            if !robots.can_fetch(&request.url).await {
                self.with_stats(|stats| stats.robots_disallowed_count += 1);
                tracing::info!(url = %request.url, "request disallowed by robots.txt");
                return;
            }
            self.domain_delay(&domain, &request.url).await
        } else {
            self.config.download_delay
        };

        let fingerprint = self.fingerprint(&request);

        if let Some(cache) = &self.cache {
            if let Some(mut cached) = cache.get(&fingerprint).await {
                for (key, value) in &request.meta {
                    cached
                        .meta
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                let bytes = cached.body.len() as u64;
                let status = cached.status;
                self.with_stats(|stats| {
                    stats.cache_hits += 1;
                    stats.increment_requests_count(&sid);
                    stats.increment_response_bytes(&domain, bytes);
                    stats.increment_status(status);
                });
                tracing::debug!(url = %request.url, "cache hit");
                self.run_callbacks(&request, cached).await;
                return;
            }
        }

        let fetched = {
            let Ok(_global) = Arc::clone(&self.global_permits).acquire_owned().await else {
                return;
            };
            let _per_domain = match self.domain_permits(&domain) {
                Some(semaphore) => match semaphore.acquire_owned().await {
                    Ok(permit) => Some(permit),
                    Err(_) => return,
                },
                None => None,
            };

            let mut delay = floor;
            if let Some(throttle) = &self.throttle {
                delay = lock(throttle).delay_for(&domain, floor);
            }
            if delay.is_finite() && delay > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(delay.min(MAX_SLEEP_SECONDS))).await;
            }

            if let Some(proxy) = &request.options.proxy {
                let proxy = proxy.clone();
                self.with_stats(|stats| stats.proxies.push(proxy));
            }

            let started = Instant::now();
            match self.sessions.fetch(&request).await {
                Ok(response) => {
                    let latency = started.elapsed().as_secs_f64();
                    let bytes = response.body.len() as u64;
                    let status = response.status;
                    self.with_stats(|stats| {
                        stats.increment_requests_count(&sid);
                        stats.increment_response_bytes(&domain, bytes);
                        stats.increment_status(status);
                    });
                    (response, latency)
                }
                Err(error) => {
                    self.with_stats(|stats| stats.failed_requests_count += 1);
                    tracing::warn!(url = %request.url, %error, "request failed");
                    self.spider.on_error(&request, &error).await;
                    return;
                }
            }
        };
        let (response, latency) = fetched;

        if let Some(cache) = &self.cache {
            self.with_stats(|stats| stats.cache_misses += 1);
            if let Err(error) = cache
                .put(&fingerprint, &response, request.options.method_or_get())
                .await
            {
                tracing::warn!(%error, "failed to cache a response");
            }
        }

        let blocked = self.spider.is_blocked(&response).await;

        if let Some(throttle) = &self.throttle {
            let ok = (200..300).contains(&response.status) && !blocked;
            let retry_after = if ok {
                None
            } else {
                parse_retry_after(&response.headers)
            };
            lock(throttle).record(&domain, latency, ok, floor, retry_after);
        }

        if blocked {
            self.with_stats(|stats| stats.blocked_requests_count += 1);
            if request.retry_count < self.config.max_blocked_retries {
                let mut retry = request.clone();
                retry.retry_count = retry.retry_count.saturating_add(1);
                // A retry goes behind everything already queued, and skips the dupe filter.
                retry.priority = retry.priority.saturating_sub(1);
                retry.dont_filter = true;
                retry.options.proxy = None;

                let attempt = retry.retry_count;
                let retry = self.spider.retry_blocked_request(retry, &response).await;
                let retry = self.normalize(retry);
                self.enqueue(retry);
                tracing::info!(
                    url = %request.url,
                    attempt,
                    limit = self.config.max_blocked_retries,
                    "scheduled a blocked request for a retry"
                );
            } else {
                tracing::warn!(url = %request.url, "max retries exceeded for a blocked request");
            }
            return;
        }

        self.run_callbacks(&request, response).await;
    }

    /// Record on the response which request produced it, so that `Response::follow` can
    /// inherit the session, the callback and the priority the way Python's does from
    /// `response.request`.
    fn stamp(&self, request: &Request, response: &mut Response) {
        response
            .meta
            .insert(META_SID.to_string(), Value::String(self.sid_of(request)));
        if let Ok(callback) = serde_json::to_value(&request.callback) {
            response.meta.insert(META_CALLBACK.to_string(), callback);
        }
        response
            .meta
            .insert(META_PRIORITY.to_string(), Value::from(request.priority));
    }

    /// Dispatch a response to its callback and act on what the callback yields.
    async fn run_callbacks(&self, request: &Request, mut response: Response) {
        self.stamp(request, &mut response);

        let outputs = match &request.callback {
            Callback::Parse => self.spider.parse(response).await,
            Callback::Named(name) => self.spider.callback(name.as_str(), response).await,
        };

        let outputs = match outputs {
            Ok(outputs) => outputs,
            Err(error) => {
                tracing::error!(url = %request.url, %error, "spider error while processing a response");
                self.spider.on_error(request, &error).await;
                return;
            }
        };

        for output in outputs {
            match output {
                Output::Request(next) => {
                    if self.is_domain_allowed(&next) {
                        let next = self.normalize(next);
                        self.enqueue(next);
                    } else {
                        self.with_stats(|stats| stats.offsite_requests_count += 1);
                        tracing::debug!(url = %next.url, "filtered an offsite request");
                    }
                }
                Output::Item(item) => match self.spider.on_scraped_item(item).await {
                    Some(item) => {
                        self.with_stats(|stats| stats.items_scraped += 1);
                        let sender = { lock(&self.item_tx).clone() };
                        match sender {
                            Some(sender) => {
                                if sender.send(item).await.is_err() {
                                    tracing::debug!("nobody is reading the item stream any more");
                                }
                            }
                            None => lock(&self.items).push(item),
                        }
                    }
                    None => self.with_stats(|stats| stats.items_dropped += 1),
                },
            }
        }
    }

    /// One spawned task: track it while it runs so a checkpoint can pick it up.
    ///
    /// The bookkeeping is undone by a guard rather than by the last two statements, so a
    /// callback that panics — or a task cancelled by a forced stop — still releases its slot
    /// instead of leaving the crawl loop waiting on a task that will never report back.
    async fn run_task(self: Arc<Self>, request: Request) {
        let task_id = self.next_task_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.inflight).insert(task_id, request.clone());
        let _slot = TaskSlot {
            inner: Arc::clone(&self),
            task_id,
        };
        self.process_request(request).await;
    }

    /// The whole crawl, from the first request to the final stats.
    async fn crawl(self: Arc<Self>) -> Result<CrawlResult> {
        self.reset();

        let resuming = self.restore_checkpoint().await;
        *lock(&self.last_checkpoint) = Instant::now();

        // Every early return has to release the item stream as well, or a caller reading
        // `stream()` would wait on a channel nobody will ever write to again.
        if let Err(error) = self.sessions.start().await {
            *lock(&self.item_tx) = None;
            return Err(error);
        }
        if let Err(error) = self.spider.on_start(resuming).await {
            *lock(&self.item_tx) = None;
            if let Err(error) = self.sessions.close().await {
                tracing::warn!(%error, "failed to close a session cleanly");
            }
            return Err(error);
        }
        self.prefetch_robots().await;

        if resuming {
            tracing::info!("resuming from a checkpoint, skipping start_requests()");
        } else {
            let mut queued = 0usize;
            for request in self.spider.start_requests().await {
                let request = self.normalize(request);
                if self.enqueue(request) {
                    queued += 1;
                }
            }
            if queued == 0 {
                tracing::warn!(
                    spider = self.spider.name(),
                    "the spider produced no start requests: set `start_urls` or override `start_requests`"
                );
            }
        }

        // First Ctrl+C asks for a graceful pause, the second one forces a stop.
        let signal_target = Arc::clone(&self);
        let signals = tokio::spawn(async move {
            for _ in 0..2 {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                signal_target.request_pause();
            }
        });

        let concurrency = self.config.concurrent_requests.max(1);
        let mut tasks: JoinSet<()> = JoinSet::new();

        loop {
            while let Some(finished) = tasks.try_join_next() {
                report_task(finished);
            }

            if self.pause_requested.load(Ordering::SeqCst) {
                let forced = self.force_stop.load(Ordering::SeqCst);
                if self.active.load(Ordering::SeqCst) == 0 || forced {
                    if self.checkpoints.is_some() {
                        self.save_checkpoint().await;
                        self.paused.store(true, Ordering::SeqCst);
                        tracing::info!("spider paused, checkpoint saved");
                    } else {
                        tracing::info!("spider stopped gracefully");
                    }
                    if forced {
                        tracing::warn!(
                            active = self.active.load(Ordering::SeqCst),
                            "force stopping with active tasks"
                        );
                        tasks.shutdown().await;
                    }
                    break;
                }
                tokio::time::sleep(IDLE_TICK).await;
                continue;
            }

            if self.is_checkpoint_time() {
                self.save_checkpoint().await;
            }

            if self.queue_is_empty() {
                if self.active.load(Ordering::SeqCst) == 0 {
                    tracing::debug!("spider idle");
                    break;
                }
                tokio::time::sleep(IDLE_TICK).await;
                continue;
            }

            if self.active.load(Ordering::SeqCst) >= concurrency {
                tokio::time::sleep(BUSY_TICK).await;
                continue;
            }

            let Some(request) = self.dequeue() else {
                continue;
            };
            self.active.fetch_add(1, Ordering::SeqCst);
            tasks.spawn(Arc::clone(&self).run_task(request));
        }

        signals.abort();
        if !self.force_stop.load(Ordering::SeqCst) {
            while let Some(finished) = tasks.join_next().await {
                report_task(finished);
            }
        }

        if let Err(error) = self.spider.on_close().await {
            tracing::error!(%error, "the spider's on_close hook failed");
        }

        let paused = self.paused.load(Ordering::SeqCst);
        if !paused {
            if let Some(manager) = &self.checkpoints {
                let _ = manager.cleanup().await;
            }
        }

        if let Err(error) = self.sessions.close().await {
            tracing::warn!(%error, "failed to close a session cleanly");
        }

        if let Some(throttle) = &self.throttle {
            let delays = lock(throttle).delays();
            self.with_stats(|stats| stats.autothrottle_delays = delays);
        }
        self.with_stats(|stats| stats.end_time = now_seconds());

        let stats = lock(&self.stats).clone();
        match stats.to_json() {
            Ok(rendered) => tracing::info!("{rendered}"),
            Err(error) => tracing::warn!(%error, "could not render the crawl stats"),
        }

        // Dropping the sender ends the item stream for whoever is reading it.
        *lock(&self.item_tx) = None;
        let items = std::mem::take(&mut *lock(&self.items));

        Ok(CrawlResult {
            stats,
            items,
            paused,
        })
    }
}

/// Drives a spider: schedules, fetches, throttles, dispatches, and counts.
pub struct CrawlerEngine {
    inner: Arc<Inner>,
}

impl CrawlerEngine {
    /// Build an engine for a spider and its sessions.
    pub fn new(
        spider: Arc<dyn Spider>,
        sessions: Arc<SessionManager>,
        options: EngineOptions,
    ) -> CrawlerEngine {
        let config = spider.config();
        let default_sid = sessions
            .default_session_id()
            .map(|sid| sid.to_string())
            .unwrap_or_default();

        let robots = if config.robots_txt_obey {
            Some(RobotsManager::new(
                Arc::clone(&sessions),
                default_sid.clone(),
            ))
        } else {
            None
        };

        let cache = if config.development_mode {
            let dir = config
                .dev_cache_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".scrapling_cache").join(spider.name()));
            tracing::warn!(
                dir = %dir.display(),
                "development mode is on -- responses are cached to disk and replayed"
            );
            Some(ResponseCache::new(dir))
        } else {
            None
        };

        // Python resolves the target concurrency as
        // `autothrottle_target_concurrency or concurrent_requests_per_domain or 1.0`.
        // Rust has no "unset" float, so anything that is not a positive number means "resolve
        // it for me" and gets the same treatment.
        let throttle = config.autothrottle.map(|mut throttle_config| {
            let target = throttle_config.target_concurrency;
            if target.is_nan() || target <= 0.0 {
                throttle_config.target_concurrency = if config.concurrent_requests_per_domain > 0 {
                    config.concurrent_requests_per_domain as f64
                } else {
                    1.0
                };
            }
            throttle_config
        });
        let throttle = match throttle {
            Some(throttle_config) => match AutoThrottle::new(throttle_config) {
                Ok(throttle) => Some(Mutex::new(throttle)),
                Err(error) => {
                    tracing::warn!(%error, "autothrottle disabled: unusable configuration");
                    None
                }
            },
            None => None,
        };

        let checkpoints = options
            .crawldir
            .clone()
            .map(|crawldir| CheckpointManager::new(crawldir, options.checkpoint_interval));

        let scheduler = Scheduler::with_limits(
            config.fp_include_kwargs,
            config.fp_include_headers,
            config.fp_keep_fragments,
            config.max_queued_requests,
            config.max_requests,
        );
        let global_permits = Arc::new(Semaphore::new(config.concurrent_requests.max(1)));
        let allowed_domains = spider
            .allowed_domains()
            .into_iter()
            .map(|domain| domain.to_lowercase())
            .collect();

        CrawlerEngine {
            inner: Arc::new(Inner {
                spider,
                sessions,
                config,
                allowed_domains,
                default_sid,
                scheduler: Mutex::new(scheduler),
                stats: Mutex::new(CrawlStats::default()),
                items: Mutex::new(Items::new()),
                item_tx: Mutex::new(None),
                inflight: Mutex::new(HashMap::new()),
                next_task_id: AtomicU64::new(0),
                throttle,
                robots,
                cache,
                checkpoints,
                last_checkpoint: Mutex::new(Instant::now()),
                global_permits,
                domain_permits: Mutex::new(HashMap::new()),
                domain_delays: Mutex::new(HashMap::new()),
                active: AtomicUsize::new(0),
                pause_requested: AtomicBool::new(false),
                force_stop: AtomicBool::new(false),
                paused: AtomicBool::new(false),
            }),
        }
    }

    /// Run the crawl to completion (or to a pause) and return everything it produced.
    pub async fn crawl(&self) -> Result<CrawlResult> {
        Arc::clone(&self.inner).crawl().await
    }

    /// Ask the crawl to stop once its in-flight requests finish. Calling it twice stops now.
    pub fn request_pause(&self) {
        self.inner.request_pause();
    }

    /// Run the crawl in the background and receive items as they are scraped.
    ///
    /// The channel closes when the crawl finishes; the run's counters stay readable through
    /// [`CrawlerEngine::stats`] while it goes.
    pub fn stream(&self) -> mpsc::Receiver<Value> {
        let (sender, receiver) = mpsc::channel(100);
        *lock(&self.inner.item_tx) = Some(sender);

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            if let Err(error) = inner.crawl().await {
                tracing::error!(%error, "the crawl failed");
            }
        });
        receiver
    }

    /// The counters as they stand right now; useful while streaming.
    pub fn stats(&self) -> CrawlStats {
        lock(&self.inner.stats).clone()
    }

    /// Whether the last run stopped on a pause request.
    pub fn paused(&self) -> bool {
        self.inner.paused.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for CrawlerEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CrawlerEngine")
            .field("spider", &self.inner.spider.name())
            .field("sessions", &self.inner.sessions.session_ids())
            .finish()
    }
}

/// Run a spider with a default HTTP session and default options.
pub async fn run(spider: Arc<dyn Spider>) -> Result<CrawlResult> {
    let mut sessions = SessionManager::new();
    sessions.add(
        "default",
        Session::Http(crate::http::FetcherSession::new()?),
    )?;
    CrawlerEngine::new(spider, Arc::new(sessions), EngineOptions::default())
        .crawl()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::spider::links::LinkExtractor;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The hosts a crawl sees come from scraped links, so every per-domain map is capped.
    #[test]
    fn per_domain_maps_stop_taking_new_domains_at_their_ceiling() {
        // Below the ceiling, and with the ceiling switched off, a new domain is tracked.
        assert!(room_for_a_domain(9, 10, "example.com"));
        assert!(room_for_a_domain(1_000_000, 0, "example.com"));
        // At and above it, it is not.
        assert!(!room_for_a_domain(10, 10, "a.attacker.test"));
        assert!(!room_for_a_domain(11, 10, "b.attacker.test"));
    }

    /// A session aimed at the local mock server.
    ///
    /// A fetcher refuses loopback addresses unless it is told they are fine, and every mock
    /// server here listens on `127.0.0.1`.
    fn local_session() -> crate::http::FetcherSession {
        crate::http::FetcherSession::builder()
            .allow_private_addresses(true)
            .build_session()
            .expect("a session")
    }

    /// A spider that scrapes the title of every page and follows every link it finds.
    struct Linked {
        start: String,
        config: SpiderConfig,
    }

    #[async_trait::async_trait]
    impl Spider for Linked {
        fn name(&self) -> &str {
            "linked"
        }

        fn start_urls(&self) -> Vec<String> {
            vec![self.start.clone()]
        }

        fn config(&self) -> SpiderConfig {
            self.config.clone()
        }

        async fn parse(&self, response: Response) -> Result<Vec<Output>> {
            let title = response
                .css_first("h1")?
                .map(|element| element.text().to_string())
                .unwrap_or_default();

            let mut outputs = vec![Output::Item(json!({"url": response.url, "title": title}))];
            for url in LinkExtractor::new().extract(&response) {
                outputs.push(Output::Request(Request::new(url)));
            }
            Ok(outputs)
        }
    }

    fn page(title: &str, links: &[&str]) -> String {
        let anchors: String = links
            .iter()
            .map(|link| format!("<a href=\"{link}\">{link}</a>"))
            .collect();
        format!("<html><body><h1>{title}</h1>{anchors}</body></html>")
    }

    async fn linked_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("home", &["/b", "/c"])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("bee", &["/c"])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/c"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("cee", &["/"])))
            .mount(&server)
            .await;
        server
    }

    fn engine_for(spider: Arc<dyn Spider>) -> Result<CrawlerEngine> {
        let mut sessions = SessionManager::new();
        sessions.add("default", Session::Http(local_session()))?;
        Ok(CrawlerEngine::new(
            spider,
            Arc::new(sessions),
            EngineOptions::default(),
        ))
    }

    #[tokio::test]
    async fn crawls_three_linked_pages_exactly_once_each() {
        let server = linked_server().await;
        let spider = Arc::new(Linked {
            start: format!("{}/", server.uri()),
            config: SpiderConfig::default(),
        });

        let engine = engine_for(spider).expect("an engine");
        let result = engine.crawl().await.expect("a crawl");

        assert!(result.completed());
        assert_eq!(result.len(), 3, "one item per page");
        assert_eq!(
            result.stats.requests_count, 3,
            "the dupe filter stops re-fetches"
        );
        assert_eq!(result.stats.items_scraped, 3);
        assert_eq!(result.stats.failed_requests_count, 0);
        assert_eq!(
            result.stats.response_status_count.get("status_200"),
            Some(&3)
        );
        assert!(result.stats.response_bytes > 0);
        assert_eq!(
            result.stats.sessions_requests_count.get("default"),
            Some(&3)
        );

        let mut titles: Vec<String> = result
            .items
            .iter()
            .filter_map(|item| item.get("title").and_then(|t| t.as_str()).map(String::from))
            .collect();
        titles.sort();
        assert_eq!(titles, vec!["bee", "cee", "home"]);
    }

    #[tokio::test]
    async fn streams_items_as_they_are_scraped() {
        let server = linked_server().await;
        let spider = Arc::new(Linked {
            start: format!("{}/", server.uri()),
            config: SpiderConfig::default(),
        });

        let engine = engine_for(spider).expect("an engine");
        let mut stream = engine.stream();

        let mut count = 0;
        while stream.recv().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn offsite_requests_are_counted_and_dropped() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(page("home", &["https://elsewhere.invalid/x"])),
            )
            .mount(&server)
            .await;

        struct Restricted {
            start: String,
            allowed: String,
        }

        #[async_trait::async_trait]
        impl Spider for Restricted {
            fn name(&self) -> &str {
                "restricted"
            }
            fn start_urls(&self) -> Vec<String> {
                vec![self.start.clone()]
            }
            fn allowed_domains(&self) -> Vec<String> {
                vec![self.allowed.clone()]
            }
            async fn parse(&self, response: Response) -> Result<Vec<Output>> {
                Ok(LinkExtractor::new()
                    .extract(&response)
                    .into_iter()
                    .map(|url| Output::Request(Request::new(url)))
                    .collect())
            }
        }

        let uri = server.uri();
        let host = ::url::Url::parse(&uri)
            .ok()
            .and_then(|parsed| parsed.host_str().map(String::from))
            .unwrap_or_default();

        let spider = Arc::new(Restricted {
            start: format!("{uri}/"),
            allowed: host,
        });
        let result = engine_for(spider)
            .expect("an engine")
            .crawl()
            .await
            .expect("a crawl");

        assert_eq!(result.stats.requests_count, 1);
        assert_eq!(result.stats.offsite_requests_count, 1);
    }

    #[tokio::test]
    async fn a_blocked_response_is_retried_then_given_up_on() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .mount(&server)
            .await;

        struct Blocked {
            start: String,
        }

        #[async_trait::async_trait]
        impl Spider for Blocked {
            fn name(&self) -> &str {
                "blocked"
            }
            fn start_urls(&self) -> Vec<String> {
                vec![self.start.clone()]
            }
            fn config(&self) -> SpiderConfig {
                SpiderConfig {
                    max_blocked_retries: 2,
                    ..SpiderConfig::default()
                }
            }
            async fn parse(&self, _response: Response) -> Result<Vec<Output>> {
                Err(Error::Spider("the callback must never run".to_string()))
            }
        }

        let spider = Arc::new(Blocked {
            start: format!("{}/", server.uri()),
        });
        let result = engine_for(spider)
            .expect("an engine")
            .crawl()
            .await
            .expect("a crawl");

        // The first attempt plus two retries, and no item ever reaches the callback.
        assert_eq!(result.stats.requests_count, 3);
        assert_eq!(result.stats.blocked_requests_count, 3);
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn a_pause_writes_a_checkpoint_that_a_later_run_resumes_from() {
        let dir = tempfile::tempdir().expect("temp dir");
        let server = linked_server().await;

        struct Pausing {
            start: String,
        }

        #[async_trait::async_trait]
        impl Spider for Pausing {
            fn name(&self) -> &str {
                "pausing"
            }
            fn start_urls(&self) -> Vec<String> {
                vec![self.start.clone()]
            }
            fn config(&self) -> SpiderConfig {
                SpiderConfig {
                    concurrent_requests: 1,
                    // Every request waits, so the pause lands mid-crawl every time.
                    download_delay: 0.4,
                    ..SpiderConfig::default()
                }
            }
            async fn parse(&self, response: Response) -> Result<Vec<Output>> {
                Ok(LinkExtractor::new()
                    .extract(&response)
                    .into_iter()
                    .map(|url| Output::Request(Request::new(url)))
                    .collect())
            }
        }

        let spider = Arc::new(Pausing {
            start: format!("{}/", server.uri()),
        });
        let mut sessions = SessionManager::new();
        sessions
            .add("default", Session::Http(local_session()))
            .expect("add");
        let engine = Arc::new(CrawlerEngine::new(
            spider,
            Arc::new(sessions),
            EngineOptions::default().crawldir(dir.path()),
        ));

        let running = Arc::clone(&engine);
        let handle = tokio::spawn(async move { running.crawl().await });

        // Ask for a pause while the first request is still waiting out its download delay,
        // so the links it finds are still queued when the checkpoint is written.
        tokio::time::sleep(Duration::from_millis(100)).await;
        engine.request_pause();

        let result = handle.await.expect("the crawl task").expect("a crawl");
        assert!(result.paused);
        assert!(!result.completed());

        let manager = CheckpointManager::new(dir.path(), 300.0);
        assert!(manager.has_checkpoint().await);
        let saved = manager.load().await.expect("a checkpoint");
        assert!(!saved.requests.is_empty());

        // A second run picks the queue back up instead of starting over.
        let resumed_spider = Arc::new(Pausing {
            start: format!("{}/", server.uri()),
        });
        let mut sessions = SessionManager::new();
        sessions
            .add("default", Session::Http(local_session()))
            .expect("add");
        let resumed = CrawlerEngine::new(
            resumed_spider,
            Arc::new(sessions),
            EngineOptions::default().crawldir(dir.path()),
        );
        let result = resumed.crawl().await.expect("a crawl");
        assert!(result.completed());
        assert!(result.stats.requests_count >= 1);
        assert!(
            !manager.has_checkpoint().await,
            "a completed crawl cleans up"
        );
    }

    #[test]
    fn engine_options_default_to_a_five_minute_interval() {
        let options = EngineOptions::default();
        assert!(options.crawldir.is_none());
        assert_eq!(options.checkpoint_interval, 300.0);

        let options = EngineOptions::new()
            .crawldir("/tmp/crawl")
            .checkpoint_interval(0.0);
        assert_eq!(options.checkpoint_interval, 0.0);
        assert!(options.crawldir.is_some());
    }
}
