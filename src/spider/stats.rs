//! The counters a crawl keeps and the result it returns, ported from
//! `CrawlStats` / `CrawlResult` in `scrapling/spiders/result.py`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

use super::items::Items;

/// How many distinct domains the per-domain breakdowns keep an entry for.
///
/// The domains come from scraped links, so the maps below would otherwise grow one entry per
/// hostname a hostile page can invent. Past this many, the totals still count every response;
/// only the per-domain breakdown stops taking new keys. This is deliberately generous — a real
/// crawl never reaches it — and matches `SpiderConfig::max_tracked_domains`'s default.
const MAX_TRACKED_DOMAINS: usize = 10_000;

/// Counters for one crawl run; the field set of Python's `CrawlStats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrawlStats {
    /// How many requests were sent.
    pub requests_count: u64,
    /// The global concurrency the run was configured with.
    pub concurrent_requests: usize,
    /// The per-domain concurrency the run was configured with; 0 means unlimited.
    pub concurrent_requests_per_domain: usize,
    /// How many requests raised a transport error.
    pub failed_requests_count: u64,
    /// How many requests were dropped for pointing outside `allowed_domains`.
    pub offsite_requests_count: u64,
    /// How many requests robots.txt refused.
    pub robots_disallowed_count: u64,
    /// How many responses were replayed from the development cache.
    pub cache_hits: u64,
    /// How many responses had to be fetched because the cache had nothing.
    pub cache_misses: u64,
    /// Total bytes of response body received.
    pub response_bytes: u64,
    /// How many items the spider yielded and kept.
    pub items_scraped: u64,
    /// How many items `on_scraped_item` dropped.
    pub items_dropped: u64,
    /// When the run started, in seconds since the Unix epoch.
    pub start_time: f64,
    /// When the run finished, in seconds since the Unix epoch.
    pub end_time: f64,
    /// The spider's configured per-domain delay.
    pub download_delay: f64,
    /// Whether the adaptive throttle was on.
    pub autothrottle_enabled: bool,
    /// How many responses looked like a block.
    pub blocked_requests_count: u64,
    /// The delay the throttle settled on per domain.
    pub autothrottle_delays: HashMap<String, f64>,
    /// Anything the spider recorded itself.
    pub custom_stats: HashMap<String, Value>,
    /// How many responses came back with each status, keyed `status_<code>`.
    pub response_status_count: HashMap<String, u64>,
    /// Response bytes per domain.
    pub domains_response_bytes: HashMap<String, u64>,
    /// Requests per session id.
    pub sessions_requests_count: HashMap<String, u64>,
    /// The proxies the run went through.
    pub proxies: Vec<String>,
}

impl CrawlStats {
    /// Wall-clock seconds the crawl took.
    pub fn elapsed_seconds(&self) -> f64 {
        self.end_time - self.start_time
    }

    /// Requests per second over the whole run.
    pub fn requests_per_second(&self) -> f64 {
        let elapsed = self.elapsed_seconds();
        if elapsed == 0.0 {
            return 0.0;
        }
        self.requests_count as f64 / elapsed
    }

    /// Count one response status.
    pub fn increment_status(&mut self, status: u16) {
        *self
            .response_status_count
            .entry(format!("status_{status}"))
            .or_insert(0) += 1;
    }

    /// Add a response's size, globally and for its domain.
    ///
    /// The global total always counts; the per-domain breakdown stops taking new domains once it
    /// holds [`MAX_TRACKED_DOMAINS`] of them, so scraped links cannot grow it without bound.
    pub fn increment_response_bytes(&mut self, domain: &str, count: u64) {
        self.response_bytes += count;
        if let Some(seen) = self.domains_response_bytes.get_mut(domain) {
            *seen += count;
        } else if self.domains_response_bytes.len() < MAX_TRACKED_DOMAINS {
            self.domains_response_bytes
                .insert(domain.to_string(), count);
        }
    }

    /// Count one request, globally and for its session.
    pub fn increment_requests_count(&mut self, sid: &str) {
        self.requests_count += 1;
        *self
            .sessions_requests_count
            .entry(sid.to_string())
            .or_insert(0) += 1;
    }

    /// The stats as pretty-printed JSON, in the key order Python logs.
    pub fn to_json(&self) -> Result<String> {
        let rounded_delays: HashMap<String, f64> = self
            .autothrottle_delays
            .iter()
            .map(|(domain, delay)| (domain.clone(), round2(*delay)))
            .collect();

        let entries: Vec<(&str, Value)> = vec![
            ("items_scraped", Value::from(self.items_scraped)),
            ("items_dropped", Value::from(self.items_dropped)),
            (
                "elapsed_seconds",
                Value::from(round2(self.elapsed_seconds())),
            ),
            ("download_delay", Value::from(round2(self.download_delay))),
            (
                "autothrottle_enabled",
                Value::from(self.autothrottle_enabled),
            ),
            ("autothrottle_delays", serde_json::to_value(rounded_delays)?),
            ("concurrent_requests", Value::from(self.concurrent_requests)),
            (
                "concurrent_requests_per_domain",
                Value::from(self.concurrent_requests_per_domain),
            ),
            ("requests_count", Value::from(self.requests_count)),
            (
                "requests_per_second",
                Value::from(round2(self.requests_per_second())),
            ),
            (
                "sessions_requests_count",
                serde_json::to_value(&self.sessions_requests_count)?,
            ),
            (
                "failed_requests_count",
                Value::from(self.failed_requests_count),
            ),
            (
                "offsite_requests_count",
                Value::from(self.offsite_requests_count),
            ),
            (
                "robots_disallowed_count",
                Value::from(self.robots_disallowed_count),
            ),
            ("cache_hits", Value::from(self.cache_hits)),
            ("cache_misses", Value::from(self.cache_misses)),
            (
                "blocked_requests_count",
                Value::from(self.blocked_requests_count),
            ),
            (
                "response_status_count",
                serde_json::to_value(&self.response_status_count)?,
            ),
            ("response_bytes", Value::from(self.response_bytes)),
            (
                "domains_response_bytes",
                serde_json::to_value(&self.domains_response_bytes)?,
            ),
            ("proxies", serde_json::to_value(&self.proxies)?),
            ("custom_stats", serde_json::to_value(&self.custom_stats)?),
        ];

        let mut out = String::from("{\n");
        for (index, (key, value)) in entries.iter().enumerate() {
            let rendered = serde_json::to_string_pretty(value)?.replace('\n', "\n    ");
            out.push_str("    ");
            out.push_str(&serde_json::to_string(key)?);
            out.push_str(": ");
            out.push_str(&rendered);
            if index + 1 < entries.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push('}');
        Ok(out)
    }
}

fn round2(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    (value * 100.0).round() / 100.0
}

/// Everything a finished crawl produced.
#[derive(Debug, Default)]
pub struct CrawlResult {
    /// The run's counters.
    pub stats: CrawlStats,
    /// The scraped items.
    pub items: Items,
    /// Whether the crawl stopped early on a pause request.
    pub paused: bool,
}

impl CrawlResult {
    /// Whether the crawl ran to completion.
    pub fn completed(&self) -> bool {
        !self.paused
    }

    /// How many items were scraped.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing was scraped.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl IntoIterator for CrawlResult {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_add_up() {
        let mut stats = CrawlStats::default();
        stats.increment_requests_count("default");
        stats.increment_requests_count("default");
        stats.increment_requests_count("browser");
        assert_eq!(stats.requests_count, 3);
        assert_eq!(stats.sessions_requests_count.get("default"), Some(&2));
        assert_eq!(stats.sessions_requests_count.get("browser"), Some(&1));

        stats.increment_status(200);
        stats.increment_status(200);
        stats.increment_status(404);
        assert_eq!(stats.response_status_count.get("status_200"), Some(&2));
        assert_eq!(stats.response_status_count.get("status_404"), Some(&1));

        stats.increment_response_bytes("example.com", 100);
        stats.increment_response_bytes("example.com", 50);
        assert_eq!(stats.response_bytes, 150);
        assert_eq!(stats.domains_response_bytes.get("example.com"), Some(&150));
    }

    /// The domains come from scraped links, so the per-domain breakdown is capped; the totals
    /// still count every byte.
    #[test]
    fn the_per_domain_breakdown_stops_at_its_ceiling() {
        let mut stats = CrawlStats::default();
        for n in 0..MAX_TRACKED_DOMAINS + 500 {
            stats.increment_response_bytes(&format!("host{n}.example"), 10);
        }
        assert_eq!(stats.domains_response_bytes.len(), MAX_TRACKED_DOMAINS);
        assert_eq!(
            stats.response_bytes,
            (MAX_TRACKED_DOMAINS as u64 + 500) * 10
        );

        // A domain already in the map keeps accumulating after the ceiling is reached.
        stats.increment_response_bytes("host0.example", 5);
        assert_eq!(stats.domains_response_bytes.get("host0.example"), Some(&15));
    }

    #[test]
    fn rates_handle_a_zero_length_run() {
        let mut stats = CrawlStats::default();
        assert_eq!(stats.elapsed_seconds(), 0.0);
        assert_eq!(stats.requests_per_second(), 0.0);

        stats.start_time = 10.0;
        stats.end_time = 20.0;
        stats.requests_count = 50;
        assert_eq!(stats.elapsed_seconds(), 10.0);
        assert_eq!(stats.requests_per_second(), 5.0);
    }

    #[test]
    fn json_keeps_the_python_key_order() {
        let stats = CrawlStats::default();
        let rendered = stats.to_json().expect("json");
        let scraped = rendered.find("items_scraped").expect("a key");
        let delay = rendered.find("download_delay").expect("a key");
        let custom = rendered.find("custom_stats").expect("a key");
        assert!(scraped < delay && delay < custom);
        let _: serde_json::Value = serde_json::from_str(&rendered).expect("valid json");
    }

    #[test]
    fn a_result_reports_completion() {
        let result = CrawlResult::default();
        assert!(result.completed());
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);

        let paused = CrawlResult {
            paused: true,
            ..Default::default()
        };
        assert!(!paused.completed());
    }
}
