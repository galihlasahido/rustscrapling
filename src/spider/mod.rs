//! The crawl framework: a Scrapy-shaped spider engine built on `tokio`.
//!
//! Port of `scrapling/spiders/`. A crawl is described by an implementation of the [`Spider`]
//! trait and driven by a [`CrawlerEngine`], which schedules requests through a [`Scheduler`],
//! fetches them through a [`SessionManager`], throttles them per domain, dispatches the
//! responses back to the spider, and collects the items it yields.
//!
//! ```ignore
//! use std::sync::Arc;
//! use rustscrapling::spider::{run, Output, Response, Spider};
//! use rustscrapling::Result;
//!
//! struct Quotes;
//!
//! #[async_trait::async_trait]
//! impl Spider for Quotes {
//!     fn name(&self) -> &str { "quotes" }
//!
//!     fn start_urls(&self) -> Vec<String> { vec!["https://quotes.toscrape.com/".into()] }
//!
//!     async fn parse(&self, response: Response) -> Result<Vec<Output>> {
//!         let title = response.css_first("h1")?.map(|h| h.text().into_string());
//!         Ok(vec![Output::Item(serde_json::json!({ "title": title }))])
//!     }
//! }
//!
//! # async fn demo() -> Result<()> {
//! let result = run(Arc::new(Quotes)).await?;
//! println!("{} items", result.len());
//! # Ok(()) }
//! ```

mod cache;
mod checkpoint;
mod engine;
mod items;
mod links;
mod request;
mod robots;
mod scheduler;
mod session;
// The module layout CONTRACT.md mandates puts `spider.rs` inside `spider/`; renaming it would
// break the documented paths, so the inception lint is silenced instead.
#[allow(clippy::module_inception)]
mod spider;
mod stats;
mod templates;
mod throttle;
mod url;

/// Re-exported so a spider can `use rustscrapling::spider::{Response, Spider, ...}` the way a
/// Python spider does `from scrapling.spiders import Response, Spider`.
pub use crate::response::Response;

pub use self::cache::ResponseCache;
pub use self::checkpoint::{CheckpointData, CheckpointManager};
pub use self::engine::{run, CrawlerEngine, EngineOptions};
pub use self::items::Items;
pub use self::links::{LinkExtractor, IGNORED_EXTENSIONS};
pub use self::request::{Callback, Request, RequestOptions};
pub use self::robots::RobotsManager;
pub use self::scheduler::Scheduler;
pub use self::session::{Session, SessionManager};
pub use self::spider::{Output, Spider, SpiderConfig, BLOCKED_CODES};
pub use self::stats::{CrawlResult, CrawlStats};
pub use self::templates::{crawl_rules, CrawlRule};
pub use self::throttle::{parse_retry_after, AutoThrottle, AutoThrottleConfig};
pub use self::url::canonicalize_url;

/// The crawl runs its requests as `tokio` tasks and hands the responses to `async` trait
/// methods whose futures must be `Send`, so everything that travels with a request has to be
/// `Send + Sync`. Asserting it here turns a later breakage into one readable error naming the
/// type that stopped being shareable, instead of a page of `async_trait` diagnostics.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Request>();
    assert_send_sync::<crate::response::Response>();
    assert_send_sync::<Session>();
    assert_send_sync::<SessionManager>();
    assert_send_sync::<Scheduler>();
    assert_send_sync::<CrawlStats>();
    assert_send_sync::<Items>();
    assert_send_sync::<CrawlerEngine>();
};
