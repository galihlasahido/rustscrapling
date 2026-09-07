//! Fetching through a real Chromium, on top of [`chromiumoxide`] and the Chrome DevTools
//! Protocol.
//!
//! This is the port of Scrapling's dynamic (Playwright) engine —
//! `scrapling/engines/_browsers/_controllers.py`. A [`DynamicSession`] owns one browser
//! process and fetches pages through it; [`DynamicFetcher`] is the one-shot form that
//! launches, fetches and shuts down again.
//!
//! ```no_run
//! # async fn demo() -> rustscrapling::Result<()> {
//! use std::time::Duration;
//! use rustscrapling::browser::{DynamicSession, WaitState};
//!
//! let session = DynamicSession::builder()
//!     .disable_resources(true)
//!     .block_ads(true)
//!     .wait_selector("main .product")
//!     .wait_selector_state(WaitState::Visible)
//!     .network_idle(true)
//!     .timeout(Duration::from_secs(45))
//!     .build();
//!
//! session.start().await?;
//! let page = session.fetch("https://example.com/").await?;
//! println!("{} {}", page.status, page.url);
//! session.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # What is not ported
//!
//! There is no Cloudflare or other anti-bot challenge solver: `stealth(true)` only adds the
//! command-line flags of [`STEALTH_ARGS`] and removes [`HARMFUL_ARGS`].
//!
//! Three more of Scrapling's behaviours are out of reach here, and each is called out where it
//! matters in the code:
//!
//! * The response body is always the serialized DOM, decoded as UTF-8. Scrapling returns raw
//!   bytes with their declared charset for anything that is not HTML; the DevTools Protocol
//!   cannot hand back a body the renderer has already consumed.
//! * `--disable-extensions` reaches the command line even though it is in [`HARMFUL_ARGS`],
//!   because `chromiumoxide` appends it after the caller's flags whenever no extension is
//!   loaded.
//! * `page_action`, `page_setup`, XHR capture, the redirect history and the retry loop have no
//!   equivalent; a failed fetch is returned as an error and retrying is the caller's business.

mod blocking;
mod constants;
mod session;

pub use crate::browser::blocking::{
    is_blocked_resource, is_domain_blocked, normalize_resource_type, RequestFilter,
};
pub use crate::browser::constants::{
    AD_DOMAINS, DEFAULT_ARGS, EXTRA_RESOURCES, HARMFUL_ARGS, STEALTH_ARGS,
};
pub use crate::browser::session::{
    launch_args, DynamicFetcher, DynamicSession, DynamicSessionBuilder, DynamicSessionOptions,
    ProxyConfig, WaitState,
};
