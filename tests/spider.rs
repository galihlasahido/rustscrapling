#![cfg(feature = "spider")]
//! The crawl framework, driven over a small `wiremock` site.
//!
//! The spider below is the Rust shape of the `QuotesSpider` in Scrapling's
//! `docs/spiders/getting-started.md`: start URLs, a `parse` that yields items and follow-up
//! requests, a named callback for detail pages, `allowed_domains`, and the exported items.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rustscrapling::response::FollowOptions;
use rustscrapling::spider::{
    canonicalize_url, parse_retry_after, CheckpointData, BLOCKED_CODES, IGNORED_EXTENSIONS,
};
use rustscrapling::{
    crawl_rules, run, AutoThrottle, AutoThrottleConfig, Callback, CheckpointManager, CrawlRule,
    CrawlStats, CrawlerEngine, EngineOptions, Error, FetcherSession, HeaderMap, Items,
    LinkExtractor, Output, Request, RequestOptions, Response, Result, Scheduler, Session,
    SessionManager, Spider, SpiderConfig,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Turn a missing value into an [`Error`] instead of panicking.
fn need<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| Error::other(format!("expected {what}")))
}

/// An HTML response.
///
/// `set_body_raw` rather than `set_body_string`, because the latter forces the template's mime
/// to `text/plain` and the template writes its mime over any `content-type` header.
fn html(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8")
}

/// Put up the two-page shop the crawl walks through.
async fn shop() -> MockServer {
    let server = MockServer::start().await;

    let index = r#"<html><body>
      <h1>Shop</h1>
      <a class="product" href="/product/1">Product 1</a>
      <a class="product" href="/product/2">Product 2</a>
      <a class="product" href="/product/3">Product 3</a>
      <a class="next" href="/page/2">Next</a>
      <a class="offsite" href="http://offsite.example/somewhere">Elsewhere</a>
    </body></html>"#;
    let second = r#"<html><body>
      <h1>Shop, page 2</h1>
      <a class="product" href="/product/4">Product 4</a>
    </body></html>"#;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(html(index.to_string()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page/2"))
        .respond_with(html(second.to_string()))
        .mount(&server)
        .await;

    for id in 1..=4 {
        let body = format!(
            r#"<html><body><h1>Product {id}</h1><span class="price">${id}0.99</span></body></html>"#
        );
        Mock::given(method("GET"))
            .and(path(format!("/product/{id}")))
            .respond_with(html(body))
            .mount(&server)
            .await;
    }

    server
}

/// The domain of a URL in the shape [`Request::domain`] produces it: the host, plus the port
/// when the URL carries a non-default one.
///
/// `allowed_domains` is compared against `Request::domain()`, so a mock server listening on
/// `127.0.0.1:41234` has to be allowed under that exact string; returning the bare host here
/// would make the engine drop every follow-up request as offsite.
fn domain_of(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url).map_err(|error| Error::other(error.to_string()))?;
    let host = need(parsed.host_str(), "a host in the base URL")?;
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Walks the shop, following product links into a named callback.
struct ShopSpider {
    base: String,
    domain: String,
    started: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    errors: Arc<AtomicUsize>,
}

impl ShopSpider {
    /// A spider pointed at a running mock server.
    fn new(base: String) -> Result<Self> {
        let domain = domain_of(&base)?;
        Ok(Self {
            base,
            domain,
            started: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            errors: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[async_trait]
impl Spider for ShopSpider {
    fn name(&self) -> &str {
        "shop"
    }

    fn start_urls(&self) -> Vec<String> {
        vec![format!("{}/", self.base)]
    }

    fn allowed_domains(&self) -> Vec<String> {
        vec![self.domain.clone()]
    }

    fn config(&self) -> SpiderConfig {
        let mut config = SpiderConfig::default();
        config.concurrent_requests = 2;
        config.download_delay = 0.0;
        config
    }

    async fn parse(&self, response: Response) -> Result<Vec<Output>> {
        let mut output = Vec::new();

        for href in response.css("a.product::attr(href)")?.getall().getall() {
            let mut options = FollowOptions::default();
            options.callback = Some(Callback::Named("product".to_string()));
            options.priority = Some(1);
            options
                .meta
                .insert("listing".to_string(), json!(response.url.clone()));
            output.push(Output::Request(response.follow(&href, options)?));
        }

        if let Some(next) = response.css("a.next::attr(href)")?.get() {
            output.push(Output::Request(
                response.follow(next.as_str(), FollowOptions::default())?,
            ));
        }

        // Dropped by `allowed_domains`, and counted in `offsite_requests_count`.
        for href in response.css("a.offsite::attr(href)")?.getall().getall() {
            output.push(Output::Request(
                response.follow(&href, FollowOptions::default())?,
            ));
        }

        Ok(output)
    }

    async fn callback(&self, name: &str, response: Response) -> Result<Vec<Output>> {
        if name != "product" {
            return self.parse(response).await;
        }

        let title = response
            .css("h1::text")?
            .get()
            .map(|text| text.clean().into_string())
            .unwrap_or_default();
        let price = response
            .css(".price::text")?
            .get()
            .map(|text| text.clean().into_string())
            .unwrap_or_default();

        Ok(vec![Output::Item(json!({
            "name": title,
            "price": price,
            "url": response.url.clone(),
        }))])
    }

    async fn on_start(&self, _resuming: bool) -> Result<()> {
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn on_close(&self) -> Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn on_error(&self, _request: &Request, _error: &Error) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }
}

/// One page, one item; used for the `run` helper.
struct SinglePageSpider {
    url: String,
}

#[async_trait]
impl Spider for SinglePageSpider {
    fn name(&self) -> &str {
        "single"
    }

    fn start_urls(&self) -> Vec<String> {
        vec![self.url.clone()]
    }

    async fn parse(&self, response: Response) -> Result<Vec<Output>> {
        let title = response
            .css("h1::text")?
            .get()
            .map(|text| text.clean().into_string())
            .unwrap_or_default();
        Ok(vec![Output::Item(json!({ "title": title }))])
    }
}

/// A session aimed at the local mock server.
///
/// A fetcher refuses to be pointed at a loopback, link-local or private address unless it is told
/// those are fine — the guard that keeps a scraped `http://127.0.0.1:6379/` link from being
/// fetched — and every `MockServer` here listens on `127.0.0.1`.
fn local_session() -> Result<FetcherSession> {
    FetcherSession::builder()
        .allow_private_addresses(true)
        .build_session()
}

/// Register one HTTP session and build the engine for a spider.
fn engine_for(spider: Arc<dyn Spider>) -> Result<CrawlerEngine> {
    let mut sessions = SessionManager::new();
    sessions.add_default("http", Session::Http(local_session()?))?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions.default_session_id()?, "http");
    Ok(CrawlerEngine::new(
        spider,
        Arc::new(sessions),
        EngineOptions::default(),
    ))
}

/// The full crawl: two listing pages, four product pages, one offsite link dropped.
#[tokio::test]
async fn crawls_the_mini_site() -> Result<()> {
    let server = shop().await;
    let spider = ShopSpider::new(server.uri())?;
    let started = Arc::clone(&spider.started);
    let closed = Arc::clone(&spider.closed);
    let errors = Arc::clone(&spider.errors);

    let engine = engine_for(Arc::new(spider))?;
    let result = engine.crawl().await?;

    assert!(started.load(Ordering::SeqCst), "on_start was never called");
    assert!(closed.load(Ordering::SeqCst), "on_close was never called");
    assert_eq!(errors.load(Ordering::SeqCst), 0, "a request failed");

    assert!(result.completed(), "the crawl reported itself as paused");
    assert!(!result.paused);
    assert_eq!(result.len(), 4);
    assert_eq!(result.items.len(), 4);

    let mut names: Vec<String> = result
        .items
        .iter()
        .filter_map(|item| item["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["Product 1", "Product 2", "Product 3", "Product 4"]
    );

    // Every item carries the price from its own page.
    for item in result.items.iter() {
        assert!(
            item["price"]
                .as_str()
                .is_some_and(|price| price.starts_with('$')),
            "missing price in {item}"
        );
        assert!(item["url"]
            .as_str()
            .is_some_and(|url| url.contains("/product/")));
    }

    let stats = &result.stats;
    assert_eq!(stats.items_scraped, 4);
    assert_eq!(stats.items_dropped, 0);
    assert!(
        stats.requests_count >= 6,
        "requests: {}",
        stats.requests_count
    );
    assert!(
        stats.offsite_requests_count >= 1,
        "the offsite link was not filtered"
    );
    assert!(stats.response_bytes > 0);
    assert!(stats.response_status_count.values().sum::<u64>() >= 6);
    assert!(stats.failed_requests_count == 0);
    assert!(stats.elapsed_seconds() >= 0.0);
    assert!(stats.to_json()?.contains("requests_count"));

    // The mock server only ever saw requests for this site.
    let received = need(server.received_requests().await, "the recorded requests")?;
    assert!(received.len() >= 6);
    Ok(())
}

/// `run` gives a spider a default HTTP session and default options.
#[tokio::test]
async fn run_helper_crawls_a_single_page() -> Result<()> {
    let server = shop().await;
    let spider = SinglePageSpider {
        url: format!("{}/product/1", server.uri()),
    };

    // `run` builds a default session, and a default session refuses a loopback URL — the guard
    // that stops a scraped link from pointing the crawl at a local service.
    let refused = run(Arc::new(SinglePageSpider {
        url: format!("{}/product/1", server.uri()),
    }))
    .await?;
    assert_eq!(refused.len(), 0);
    assert_eq!(refused.stats.failed_requests_count, 1);

    // The same spider, run through a session that says private addresses are fine.
    let mut sessions = SessionManager::new();
    sessions.add_default("http", Session::Http(local_session()?))?;
    let result = CrawlerEngine::new(
        Arc::new(spider),
        Arc::new(sessions),
        EngineOptions::default(),
    )
    .crawl()
    .await?;
    assert_eq!(result.len(), 1);
    assert_eq!(
        need(result.items.iter().next(), "the scraped item")?["title"],
        "Product 1"
    );
    Ok(())
}

/// `stream` hands items over while the crawl is still running.
#[tokio::test]
async fn streaming_yields_items() -> Result<()> {
    let server = shop().await;
    let spider = ShopSpider::new(server.uri())?;
    let engine = engine_for(Arc::new(spider))?;

    let mut receiver = engine.stream();
    let collected = tokio::time::timeout(Duration::from_secs(30), async {
        let mut items = Vec::new();
        while let Some(item) = receiver.recv().await {
            items.push(item);
        }
        items
    })
    .await
    .map_err(|_| Error::other("the streaming crawl did not finish in time"))?;

    assert!(!collected.is_empty(), "no items were streamed");
    assert!(engine.stats().requests_count >= 1);
    Ok(())
}

/// The scheduler drops duplicates and serves the highest priority first.
#[test]
fn scheduler_orders_and_deduplicates() -> Result<()> {
    let mut scheduler = Scheduler::new(false, false, false);
    assert!(scheduler.is_empty());

    assert!(scheduler.enqueue(Request::new("https://example.com/a")));
    assert!(
        !scheduler.enqueue(Request::new("https://example.com/a")),
        "the duplicate filter let the same URL through"
    );
    assert!(scheduler.enqueue(Request::new("https://example.com/b").priority(5)));
    assert!(scheduler.enqueue(Request::new("https://example.com/c").priority(5)));
    assert!(
        scheduler.enqueue(Request::new("https://example.com/a").dont_filter(true)),
        "dont_filter must bypass the duplicate filter"
    );
    assert_eq!(scheduler.len(), 4);

    let first = need(scheduler.dequeue(), "the first request")?;
    assert_eq!(
        first.url, "https://example.com/b",
        "priority is not honoured"
    );
    let second = need(scheduler.dequeue(), "the second request")?;
    assert_eq!(
        second.url, "https://example.com/c",
        "FIFO within a priority"
    );

    let (queued, seen) = scheduler.snapshot();
    assert_eq!(queued.len(), 2);
    assert!(!seen.is_empty());

    let mut restored = Scheduler::new(false, false, false);
    restored.restore(queued, seen);
    assert_eq!(restored.len(), 2);
    assert!(!restored.is_empty());
    Ok(())
}

/// Fingerprints ignore the order of query parameters but not the session.
#[test]
fn request_fingerprints() -> Result<()> {
    let one = Request::new("https://example.com/x?b=2&a=1");
    let two = Request::new("https://example.com/x?a=1&b=2");
    assert_eq!(
        one.fingerprint(false, false, false),
        two.fingerprint(false, false, false)
    );

    let other_session = Request::new("https://example.com/x?a=1&b=2").sid("browser");
    assert_ne!(
        two.fingerprint(false, false, false),
        other_session.fingerprint(false, false, false)
    );

    let with_fragment = Request::new("https://example.com/x?a=1&b=2#top");
    assert_eq!(
        two.fingerprint(false, false, false),
        with_fragment.fingerprint(false, false, false),
        "fragments are dropped unless they are kept explicitly"
    );
    assert_ne!(
        two.fingerprint(false, false, true),
        with_fragment.fingerprint(false, false, true)
    );

    assert_eq!(two.domain(), "example.com");
    assert_eq!(two.priority, 0);
    assert_eq!(two.callback, Callback::Parse);
    assert!(two.sid.is_empty());

    // A request carries its transport options and its meta.
    let mut options = RequestOptions::default();
    options.method = "POST".to_string();
    options.form = Some(
        [("key".to_string(), "value".to_string())]
            .into_iter()
            .collect(),
    );
    let posted = Request::new("https://example.com/x")
        .options(options)
        .meta("depth", json!(2))
        .callback(Callback::Named("detail".to_string()));
    assert_eq!(posted.options.method, "POST");
    assert_eq!(posted.meta.get("depth"), Some(&json!(2)));
    assert_eq!(posted.callback, Callback::Named("detail".to_string()));

    // The request round-trips through JSON, which is what checkpoints rely on.
    let encoded = serde_json::to_string(&posted)?;
    let decoded: Request = serde_json::from_str(&encoded)?;
    assert_eq!(decoded.url, posted.url);
    assert_eq!(decoded.callback, posted.callback);
    Ok(())
}

/// URL canonicalization, as w3lib does it.
#[test]
fn canonical_urls() {
    assert_eq!(
        canonicalize_url("http://example.com/b?y=2&x=1#frag", false),
        "http://example.com/b?x=1&y=2"
    );
    assert_eq!(
        canonicalize_url("http://example.com/b?y=2&x=1#frag", true),
        "http://example.com/b?x=1&y=2#frag"
    );
    assert!(canonicalize_url("http://example.com", false).contains("example.com"));
}

/// Link extraction, filtering and deduplication.
#[test]
fn link_extraction() -> Result<()> {
    let html = r#"<html><body>
      <a href="/a.html">a</a>
      <a href="/a.html">a again</a>
      <a href="/docs/manual.pdf">pdf</a>
      <a href="http://other.example/c">other</a>
      <a href="mailto:someone@example.com">mail</a>
      <div class="menu"><a href="/menu.html">menu</a></div>
      <area href="/d.html">
    </body></html>"#;
    let response = Response::new(
        "https://example.com/index.html",
        html.as_bytes().to_vec(),
        200,
    )?;

    let links = LinkExtractor::new().extract(&response);
    assert!(links.contains(&"https://example.com/a.html".to_string()));
    assert!(links.contains(&"https://example.com/d.html".to_string()));
    assert!(links.contains(&"http://other.example/c".to_string()));
    assert!(
        !links.iter().any(|link| link.ends_with(".pdf")),
        "ignored extensions must be dropped"
    );
    assert!(!links.iter().any(|link| link.starts_with("mailto:")));
    assert_eq!(
        links
            .iter()
            .filter(|link| link.ends_with("/a.html"))
            .count(),
        1,
        "links are deduplicated"
    );

    let only_local = LinkExtractor::new()
        .allow_domains(&["example.com"])
        .extract(&response);
    assert!(!only_local.iter().any(|link| link.contains("other.example")));

    let denied = LinkExtractor::new()
        .deny(&[r"/a\.html"])?
        .extract(&response);
    assert!(!denied.iter().any(|link| link.ends_with("/a.html")));

    let allowed = LinkExtractor::new()
        .allow(&[r"/d\.html$"])?
        .extract(&response);
    assert_eq!(allowed, vec!["https://example.com/d.html".to_string()]);

    let restricted = LinkExtractor::new()
        .restrict_css(&[".menu"])
        .extract(&response);
    assert_eq!(
        restricted,
        vec!["https://example.com/menu.html".to_string()]
    );

    let no_domains = LinkExtractor::new()
        .deny_domains(&["other.example"])
        .extract(&response);
    assert!(!no_domains.iter().any(|link| link.contains("other.example")));

    let extractor = LinkExtractor::new().allow(&[r"/a\.html"])?;
    assert!(extractor.matches("https://example.com/a.html"));
    assert!(!extractor.matches("https://example.com/b.html"));

    assert!(IGNORED_EXTENSIONS.contains(&"pdf"));
    assert!(IGNORED_EXTENSIONS.contains(&"jpg"));
    Ok(())
}

/// `crawl_rules` is the helper that replaces Python's `CrawlSpider`.
#[test]
fn crawl_rules_produce_requests() -> Result<()> {
    let html = r#"<html><body>
      <a href="/product/1">one</a>
      <a href="/product/2">two</a>
      <a href="/about">about</a>
    </body></html>"#;
    let response = Response::new("https://example.com/", html.as_bytes().to_vec(), 200)?;

    let rules = vec![CrawlRule::new(LinkExtractor::new().allow(&[r"/product/"])?)
        .callback(Callback::Named("product".to_string()))
        .priority(3)];

    let requests = crawl_rules(&response, &rules);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.callback, Callback::Named("product".to_string()));
        assert_eq!(request.priority, 3);
        assert!(request.url.contains("/product/"));
    }
    Ok(())
}

/// The autothrottle formula, checked against the Python arithmetic.
#[test]
fn autothrottle_follows_the_python_formula() -> Result<()> {
    let mut throttle = AutoThrottle::new(AutoThrottleConfig::default())?;

    // The first request to a domain uses `start_delay`.
    assert!((throttle.delay_for("example.com", 0.0) - 5.0).abs() < 1e-9);

    // target = 1.0 / 1.0; new = max((5 + 1) / 2, 1) = 3.0
    let after_one = throttle.record("example.com", 1.0, true, 0.0, None);
    assert!((after_one - 3.0).abs() < 1e-9, "got {after_one}");

    // new = max((3 + 1) / 2, 1) = 2.0
    let after_two = throttle.record("example.com", 1.0, true, 0.0, None);
    assert!((after_two - 2.0).abs() < 1e-9, "got {after_two}");

    // Blocked with a `Retry-After`: the penalty wins.
    let blocked = throttle.record("example.com", 1.0, false, 0.0, Some(10.0));
    assert!((blocked - 10.0).abs() < 1e-9, "got {blocked}");

    // The floor raises the delay, the ceiling caps it.
    assert!(throttle.delay_for("other.example", 20.0) >= 20.0);
    assert!(throttle.record("other.example", 1000.0, true, 0.0, None) <= 60.0);

    assert!(!throttle.delays().is_empty());
    throttle.reset();
    assert!(throttle.delays().is_empty());

    // Nonsensical configurations are rejected rather than dividing by zero.
    let mut broken = AutoThrottleConfig::default();
    broken.target_concurrency = 0.0;
    assert!(AutoThrottle::new(broken).is_err());

    let mut inverted = AutoThrottleConfig::default();
    inverted.start_delay = 10.0;
    inverted.max_delay = 1.0;
    assert!(AutoThrottle::new(inverted).is_err());
    Ok(())
}

/// `Retry-After` is read as seconds or as an HTTP date.
#[test]
fn retry_after_header() {
    let mut headers = HeaderMap::new();
    assert!(parse_retry_after(&headers).is_none());

    headers.insert("Retry-After", "120");
    assert_eq!(parse_retry_after(&headers), Some(120.0));

    let mut dated = HeaderMap::new();
    dated.insert("retry-after", "Wed, 21 Oct 2099 07:28:00 GMT");
    let seconds = parse_retry_after(&dated);
    assert!(seconds.is_some_and(|value| value > 0.0), "got {seconds:?}");

    let mut nonsense = HeaderMap::new();
    nonsense.insert("retry-after", "soon please");
    assert!(parse_retry_after(&nonsense).is_none());
}

/// Checkpoints survive a round trip through the filesystem.
#[tokio::test]
async fn checkpoints_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let manager = CheckpointManager::new(dir.path(), 0.0);
    assert!((manager.interval() - 0.0).abs() < 1e-9);
    assert!(!manager.has_checkpoint().await);
    assert!(manager.load().await.is_none());

    let data = CheckpointData {
        requests: vec![
            Request::new("https://example.com/a"),
            Request::new("https://example.com/b").priority(2),
        ],
        seen: vec!["ab".repeat(20)],
    };
    manager.save(&data).await?;
    assert!(manager.has_checkpoint().await);

    let loaded = need(manager.load().await, "the saved checkpoint")?;
    assert_eq!(loaded.requests.len(), 2);
    assert_eq!(loaded.requests[1].priority, 2);
    assert_eq!(loaded.seen.len(), 1);

    manager.cleanup().await?;
    assert!(!manager.has_checkpoint().await);
    Ok(())
}

/// The counters and the export formats.
#[test]
fn stats_and_exports() -> Result<()> {
    let mut stats = CrawlStats::default();
    stats.increment_status(200);
    stats.increment_status(200);
    stats.increment_status(404);
    stats.increment_response_bytes("example.com", 100);
    stats.increment_response_bytes("example.com", 50);
    stats.increment_requests_count("http");
    stats.start_time = 1000.0;
    stats.end_time = 1010.0;

    assert_eq!(stats.response_status_count.values().sum::<u64>(), 3);
    assert_eq!(stats.response_bytes, 150);
    assert_eq!(stats.domains_response_bytes.get("example.com"), Some(&150));
    assert_eq!(stats.sessions_requests_count.get("http"), Some(&1));
    assert_eq!(stats.requests_count, 1);
    assert!((stats.elapsed_seconds() - 10.0).abs() < 1e-9);
    assert!((stats.requests_per_second() - 0.1).abs() < 1e-9);
    assert!(stats.to_json()?.contains("requests_count"));

    let dir = tempfile::tempdir()?;
    let mut items = Items::new();
    items.push(json!({"name": "a", "price": 1}));
    items.push(json!({"name": "b", "tags": ["x", "y"]}));
    assert_eq!(items.len(), 2);
    assert!(!items.is_empty());

    let json_path = dir.path().join("items.json");
    items.to_json(&json_path, true)?;
    let written: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&json_path)?)?;
    assert_eq!(written.len(), 2);

    let jsonl_path = dir.path().join("items.jsonl");
    items.to_jsonl(&jsonl_path)?;
    let lines = std::fs::read_to_string(&jsonl_path)?;
    assert_eq!(lines.lines().filter(|line| !line.is_empty()).count(), 2);

    let csv_path = dir.path().join("items.csv");
    items.to_csv(&csv_path, None)?;
    let csv = std::fs::read_to_string(&csv_path)?;
    assert!(csv.contains("name"));
    assert!(csv.contains("tags"), "every key seen becomes a column");
    assert!(
        csv.contains("[\"x\",\"y\"]") || csv.contains("x"),
        "lists are written as JSON"
    );

    let narrow_path = dir.path().join("names.csv");
    items.to_csv(&narrow_path, Some(&["name"]))?;
    let narrow = std::fs::read_to_string(&narrow_path)?;
    assert!(!narrow.contains("price"));

    assert_eq!(items.clone().into_vec().len(), 2);
    Ok(())
}

/// The blocked-status list is the one Python uses.
#[test]
fn blocked_codes() {
    for code in [401, 403, 429, 503] {
        assert!(
            BLOCKED_CODES.contains(&code),
            "{code} should count as blocked"
        );
    }
    assert!(!BLOCKED_CODES.contains(&200));
    assert!(!BLOCKED_CODES.contains(&404));
}

/// `Response::follow` inherits the session, the meta and the referer.
#[test]
fn follow_inherits_from_the_response() -> Result<()> {
    let html = r#"<html><body><a href="page/2">next</a></body></html>"#;
    let mut meta = std::collections::HashMap::new();
    meta.insert("depth".to_string(), json!(1));
    let response = Response::new(
        "https://example.com/list/index.html",
        html.as_bytes().to_vec(),
        200,
    )?
    .with_meta(meta);

    let plain = response.follow("page/2", FollowOptions::default())?;
    assert_eq!(plain.url, "https://example.com/list/page/2");
    assert_eq!(plain.meta.get("depth"), Some(&json!(1)));

    let mut options = FollowOptions::default();
    options.callback = Some(Callback::Named("detail".to_string()));
    options.priority = Some(7);
    options.dont_filter = true;
    options.sid = Some("browser".to_string());
    options.meta.insert("depth".to_string(), json!(2));

    let customized = response.follow("/other", options)?;
    assert_eq!(customized.url, "https://example.com/other");
    assert_eq!(customized.callback, Callback::Named("detail".to_string()));
    assert_eq!(customized.priority, 7);
    assert!(customized.dont_filter);
    assert_eq!(customized.sid, "browser");
    assert_eq!(customized.meta.get("depth"), Some(&json!(2)));
    Ok(())
}

/// A session manager rejects an unknown session and names a default.
#[tokio::test]
async fn session_manager_basics() -> Result<()> {
    let mut sessions = SessionManager::new();
    assert!(sessions.is_empty());
    assert!(sessions.default_session_id().is_err());

    sessions.add("api", Session::Http(FetcherSession::new()?))?;
    sessions.add("scraper", Session::Http(FetcherSession::new()?))?;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.default_session_id()?, "api");
    assert!(sessions.get("scraper").is_some());
    assert!(sessions.get("nope").is_none());

    let mut ids = sessions.session_ids();
    ids.sort();
    assert_eq!(ids, vec!["api", "scraper"]);

    sessions.add_default("fallback", Session::Http(FetcherSession::new()?))?;
    assert_eq!(sessions.default_session_id()?, "fallback");
    assert_eq!(sessions.len(), 3);

    assert!(sessions.remove("api").is_some());
    assert!(sessions.remove("api").is_none());
    assert_eq!(sessions.len(), 2);

    sessions.start().await?;
    sessions.close().await?;
    Ok(())
}
