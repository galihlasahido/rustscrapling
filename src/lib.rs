//! `rustscrapling` is a Rust port of the Python **Scrapling** library.
//!
//! It gives you three layers that stack on top of each other:
//!
//! * **Parsing** — [`Selector`] wraps an HTML document and lets you query it with CSS
//!   (including the `::text` and `::attr(name)` pseudo-elements), by text, by regex, or by
//!   structural similarity. Results come back as [`Selectors`], strings as [`Text`]/[`Texts`].
//! * **Adaptive selection** — an element can be fingerprinted ([`ElementFingerprint`]) and
//!   stored, so that when the page structure changes the same element is relocated by
//!   similarity instead of breaking the scraper.
//! * **Fetching and crawling** — [`Fetcher`]/[`FetcherSession`] for HTTP, [`DynamicFetcher`]
//!   for a real browser, and a small Scrapy-shaped crawl framework built around the
//!   [`Spider`] trait and [`CrawlerEngine`].
//!
//! # Feature flags
//!
//! | Feature   | Default | What it adds |
//! |-----------|---------|--------------|
//! | `http`    | yes     | the `reqwest`-based [`Fetcher`] and [`FetcherSession`] |
//! | `spider`  | yes     | the crawl framework (implies `http`) |
//! | `browser` | no      | the `chromiumoxide`-based [`DynamicFetcher`] |
//!
//! The parser, the adaptive layer, and [`Response`] are always available.
//!
//! # Status
//!
//! In development. The public API is defined by `CONTRACT.md` in the repository root.

// `CONTRACT.md` ground rule: no `unsafe` without a sign-off. The lint makes any new `unsafe`
// a compile error; the single reviewed exception is the `unsafe impl Sync for Document` in
// `parser::selector`, which opts out explicitly and carries its safety argument there.
#![deny(unsafe_code)]

pub mod adaptive;
pub mod error;
pub mod parser;
pub mod response;
pub mod text;

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "spider")]
pub mod spider;

#[cfg(feature = "browser")]
pub mod browser;

pub use crate::error::{Error, Result};
pub use crate::text::{Attributes, ReOptions, Text, Texts};

pub use crate::parser::{Filter, Selector, SelectorKind, Selectors};

pub use crate::adaptive::{
    fingerprint, relocate, similarity_score, Adaptive, ElementFingerprint, SqliteStorage, Storage,
};

pub use crate::response::{Cookie, HeaderMap, Response, StatusText};

/// `FollowOptions` only exists when there is a crawl `Request` for `Response::follow` to build.
#[cfg(feature = "spider")]
pub use crate::response::FollowOptions;

#[cfg(feature = "http")]
pub use crate::http::{
    BrowserProfile, Fetcher, FetcherBuilder, FetcherSession, FollowRedirects, ProxyRotator,
    RequestBuilder,
};

#[cfg(feature = "spider")]
pub use crate::spider::{
    crawl_rules, run, AutoThrottle, AutoThrottleConfig, Callback, CheckpointManager, CrawlResult,
    CrawlRule, CrawlStats, CrawlerEngine, EngineOptions, Items, LinkExtractor, Output, Request,
    RequestOptions, ResponseCache, RobotsManager, Scheduler, Session, SessionManager, Spider,
    SpiderConfig,
};

#[cfg(feature = "browser")]
pub use crate::browser::{
    DynamicFetcher, DynamicSession, DynamicSessionBuilder, DynamicSessionOptions, WaitState,
};
