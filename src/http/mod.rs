//! The `reqwest`-based HTTP fetchers.
//!
//! Port of `scrapling/engines/static.py` and `scrapling/engines/toolbelt/proxy_rotation.py`.
//!
//! Two entry points, matching Scrapling's:
//!
//! * [`Fetcher`] — stateless. Every request is independent; there is no cookie jar.
//! * [`FetcherSession`] — one client and one cookie jar reused across requests.
//!
//! Both are configured through [`FetcherBuilder`] and produce a [`crate::Response`].
//!
//! ```no_run
//! # async fn run() -> rustscrapling::Result<()> {
//! use rustscrapling::http::{BrowserProfile, Fetcher, FollowRedirects};
//!
//! let fetcher = Fetcher::builder()
//!     .impersonate(BrowserProfile::Firefox)
//!     .follow_redirects(FollowRedirects::Safe)
//!     .retries(3)
//!     .build()?;
//!
//! let response = fetcher.get("https://example.com/").send().await?;
//! println!("{} -> {:?}", response.status, response.css_first("h1::text")?);
//! # Ok(())
//! # }
//! ```
//!
//! # Differences from the Python original
//!
//! Scrapling uses `curl_cffi`, which impersonates a browser's TLS and HTTP/2 fingerprint as well
//! as its headers. `reqwest` cannot do that, so [`BrowserProfile`] only selects a static,
//! realistic *header* set. Anti-bot systems that fingerprint the TLS handshake
//! will still tell this client apart from a real browser.

mod fetcher;
mod headers;
mod proxy;
mod redirect;
mod session;

pub use fetcher::{Fetcher, FetcherBuilder, RequestBuilder};
pub use headers::BrowserProfile;
pub use proxy::{cyclic_rotation, is_proxy_error, ProxyRotator};
pub use redirect::{is_blocked_ip, is_blocked_redirect_target, FollowRedirects};
pub use session::FetcherSession;
