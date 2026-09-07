//! The [`Spider`] trait: where a crawl starts and what it makes of every page.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::response::Response;

use super::request::Request;
use super::throttle::AutoThrottleConfig;

/// Status codes that count as "we were blocked", as Python's `BLOCKED_CODES`.
pub const BLOCKED_CODES: &[u16] = &[401, 403, 407, 429, 444, 500, 502, 503, 504];

/// Default ceiling on the scheduler queue: far above any ordinary crawl, low enough that a link
/// bomb cannot grow the queue until the process is killed.
const DEFAULT_MAX_QUEUED_REQUESTS: usize = 100_000;
/// Default ceiling on how many requests one crawl schedules in total.
const DEFAULT_MAX_REQUESTS: usize = 1_000_000;
/// Default ceiling on how many distinct domains a crawl keeps per-domain state for.
const DEFAULT_MAX_TRACKED_DOMAINS: usize = 10_000;

/// What a callback yields.
///
/// `Request` is much larger than `Item`, but CONTRACT.md fixes the variant as an unboxed
/// [`Request`], so the size difference is accepted rather than hidden behind a `Box`.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Output {
    /// A scraped item.
    Item(serde_json::Value),
    /// Another request to schedule.
    Request(Request),
}

impl From<Request> for Output {
    fn from(request: Request) -> Self {
        Output::Request(request)
    }
}

impl From<serde_json::Value> for Output {
    fn from(item: serde_json::Value) -> Self {
        Output::Item(item)
    }
}

/// How a spider is allowed to crawl.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpiderConfig {
    /// Requests in flight overall. Default 4.
    pub concurrent_requests: usize,
    /// Requests in flight per domain; 0 means "no per-domain limit". Default 0.
    pub concurrent_requests_per_domain: usize,
    /// Seconds to wait before each request to a domain. Default 0.0.
    pub download_delay: f64,
    /// How many times a blocked request is retried. Default 3.
    pub max_blocked_retries: u32,
    /// Enable adaptive per-domain delays. Default `None` (off).
    pub autothrottle: Option<AutoThrottleConfig>,
    /// Honour robots.txt. Default `false`.
    pub robots_txt_obey: bool,
    /// Cache responses to disk and replay them. Default `false`.
    pub development_mode: bool,
    /// Where the development cache lives. Default `.scrapling_cache/<spider name>`.
    pub dev_cache_dir: Option<PathBuf>,
    /// Include [`super::RequestOptions`] in the deduplication fingerprint. Default `false`.
    pub fp_include_kwargs: bool,
    /// Include headers in the deduplication fingerprint. Default `false`.
    pub fp_include_headers: bool,
    /// Keep URL fragments in the deduplication fingerprint. Default `false`.
    pub fp_keep_fragments: bool,
    /// The most requests allowed to sit in the scheduler queue at once; `0` removes the ceiling.
    /// Default 100 000.
    ///
    /// Python has no equivalent setting. Queued requests come from links found in remote markup,
    /// so a page that links to enough unique URLs — or a few pages that cross-link — grows the
    /// queue, the duplicate-filter set and the per-domain bookkeeping until the process dies.
    /// Past the ceiling a request is dropped with a warning instead of being queued.
    pub max_queued_requests: usize,
    /// The most requests a single crawl will schedule in total; `0` removes the ceiling.
    /// Default 1 000 000.
    ///
    /// This is what bounds the duplicate-filter set, which keeps one 20-byte fingerprint per
    /// accepted request for the life of the crawl.
    pub max_requests: usize,
    /// The most distinct domains a crawl keeps per-domain state for; `0` removes the ceiling.
    /// Default 10 000.
    ///
    /// Caps the per-domain concurrency permits, the per-domain delays and the per-domain
    /// response-byte counters, which are otherwise one map entry per host seen in a link.
    pub max_tracked_domains: usize,
}

impl Default for SpiderConfig {
    fn default() -> Self {
        SpiderConfig {
            concurrent_requests: 4,
            concurrent_requests_per_domain: 0,
            download_delay: 0.0,
            max_blocked_retries: 3,
            autothrottle: None,
            robots_txt_obey: false,
            development_mode: false,
            dev_cache_dir: None,
            fp_include_kwargs: false,
            fp_include_headers: false,
            fp_keep_fragments: false,
            max_queued_requests: DEFAULT_MAX_QUEUED_REQUESTS,
            max_requests: DEFAULT_MAX_REQUESTS,
            max_tracked_domains: DEFAULT_MAX_TRACKED_DOMAINS,
        }
    }
}

/// What a crawl does: where it starts and what it makes of each page.
///
/// The trait is object safe, so an engine takes an `Arc<dyn Spider>`. Only [`Spider::name`]
/// and [`Spider::parse`] have to be written; everything else has the default Python behaviour.
#[async_trait::async_trait]
pub trait Spider: Send + Sync {
    /// The spider's name; used for logs and for the cache directory.
    fn name(&self) -> &str;

    /// The URLs the crawl starts from. Default: empty.
    fn start_urls(&self) -> Vec<String> {
        Vec::new()
    }

    /// Hosts (and their subdomains) the crawl may visit. Empty means no restriction.
    fn allowed_domains(&self) -> Vec<String> {
        Vec::new()
    }

    /// The crawl settings. Default: [`SpiderConfig::default`].
    fn config(&self) -> SpiderConfig {
        SpiderConfig::default()
    }

    /// The first requests. Default: a GET per [`Spider::start_urls`] entry handled by `parse`.
    async fn start_requests(&self) -> Vec<Request> {
        self.start_urls().into_iter().map(Request::new).collect()
    }

    /// Turn a response into items and further requests.
    async fn parse(&self, response: Response) -> Result<Vec<Output>>;

    /// Dispatch a named callback. Default: everything goes to `parse`.
    async fn callback(&self, name: &str, response: Response) -> Result<Vec<Output>> {
        let _ = name;
        self.parse(response).await
    }

    /// Called once before the crawl starts; `resuming` is set when a checkpoint was loaded.
    async fn on_start(&self, resuming: bool) -> Result<()> {
        let _ = resuming;
        Ok(())
    }

    /// Called once after the crawl finishes.
    async fn on_close(&self) -> Result<()> {
        Ok(())
    }

    /// Called when a request fails or a callback returns an error.
    async fn on_error(&self, request: &Request, error: &Error) {
        let _ = (request, error);
    }

    /// Inspect or rewrite a scraped item; return `None` to drop it.
    async fn on_scraped_item(&self, item: serde_json::Value) -> Option<serde_json::Value> {
        Some(item)
    }

    /// Whether a response means we were blocked. Default: the status is in [`BLOCKED_CODES`].
    async fn is_blocked(&self, response: &Response) -> bool {
        BLOCKED_CODES.contains(&response.status)
    }

    /// Prepare a blocked request before it is retried. Default: unchanged.
    async fn retry_blocked_request(&self, request: Request, response: &Response) -> Request {
        let _ = response;
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Minimal;

    #[async_trait::async_trait]
    impl Spider for Minimal {
        fn name(&self) -> &str {
            "minimal"
        }

        fn start_urls(&self) -> Vec<String> {
            vec![
                "http://example.com/a".to_string(),
                "http://example.com/b".to_string(),
            ]
        }

        async fn parse(&self, response: Response) -> Result<Vec<Output>> {
            Ok(vec![Output::Item(serde_json::json!({"url": response.url}))])
        }
    }

    fn a_response() -> Response {
        Response::new("http://example.com/a", b"<html></html>".to_vec(), 403).expect("a response")
    }

    #[test]
    fn the_defaults_match_python() {
        let config = SpiderConfig::default();
        assert_eq!(config.concurrent_requests, 4);
        assert_eq!(config.concurrent_requests_per_domain, 0);
        assert_eq!(config.download_delay, 0.0);
        assert_eq!(config.max_blocked_retries, 3);
        assert!(config.autothrottle.is_none());
        assert!(!config.robots_txt_obey);
        assert!(!config.development_mode);
    }

    #[tokio::test]
    async fn the_trait_is_object_safe_and_has_working_defaults() {
        let spider: Arc<dyn Spider> = Arc::new(Minimal);
        assert_eq!(spider.name(), "minimal");
        assert!(spider.allowed_domains().is_empty());

        let requests = spider.start_requests().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, "http://example.com/a");
        assert_eq!(requests[0].callback, super::super::Callback::Parse);

        // A named callback falls through to `parse` by default.
        let outputs = spider
            .callback("parse_item", a_response())
            .await
            .expect("outputs");
        assert_eq!(outputs.len(), 1);

        assert!(spider.is_blocked(&a_response()).await);
        assert!(spider.on_start(false).await.is_ok());
        assert!(spider.on_close().await.is_ok());
        assert_eq!(
            spider.on_scraped_item(serde_json::json!({"a": 1})).await,
            Some(serde_json::json!({"a": 1}))
        );
    }
}
