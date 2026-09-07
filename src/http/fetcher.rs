//! The stateless [`Fetcher`], its [`FetcherBuilder`] and the per-request [`RequestBuilder`].
//!
//! Port of `scrapling/engines/static.py` on top of `reqwest`. Python's `**kwargs` become builder
//! methods here: everything the session can configure has a `FetcherBuilder` setter, and
//! everything a single call could override in Python has a `RequestBuilder` setter.

use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::response::{encoding_from_content_type, Cookie, HeaderMap, Response, META_PROXY};

use super::headers::{headers_job, BrowserProfile};
use super::proxy::ProxyRotator;
use super::redirect::FollowRedirects;
use super::session::FetcherSession;

/// Scrapling's default per-request timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Scrapling's default retry count (`retries=3`).
const DEFAULT_RETRIES: u32 = 3;
/// Scrapling's default pause between retries (`retry_delay=1`).
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Scrapling's default redirect budget (`max_redirects=30`).
const DEFAULT_MAX_REDIRECTS: usize = 30;
/// How much of a response body is read before the request is abandoned.
///
/// Python has no such limit; `reqwest` has none either, so without it a hostile or misbehaving
/// server could make the process allocate until it dies. 64 MiB is far above any HTML page and
/// can be raised (or switched off with `0`) through [`FetcherBuilder::max_response_bytes`].
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// The largest buffer pre-allocated from a `Content-Length` the server claims.
const MAX_PREALLOCATED_BODY: usize = 1024 * 1024;
/// How far `describe` walks an error's source chain; a cyclic chain must not hang the caller.
const MAX_ERROR_SOURCE_DEPTH: usize = 16;

/// Everything a fetcher or a session was configured with.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) impersonate: Option<BrowserProfile>,
    pub(crate) stealthy_headers: bool,
    pub(crate) proxy: Option<String>,
    pub(crate) proxy_rotator: Option<ProxyRotator>,
    pub(crate) timeout: Duration,
    pub(crate) headers: HeaderMap,
    pub(crate) retries: u32,
    pub(crate) retry_delay: Duration,
    pub(crate) follow_redirects: FollowRedirects,
    pub(crate) max_redirects: usize,
    pub(crate) verify_tls: bool,
    /// `0` means "no limit"; anything else caps how much body is read.
    pub(crate) max_response_bytes: usize,
    /// Whether a request may be aimed at a loopback, link-local or private address.
    pub(crate) allow_private_addresses: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // Python defaults to `impersonate="chrome"`.
            impersonate: Some(BrowserProfile::Chrome),
            stealthy_headers: true,
            proxy: None,
            proxy_rotator: None,
            timeout: DEFAULT_TIMEOUT,
            headers: HeaderMap::new(),
            retries: DEFAULT_RETRIES,
            retry_delay: DEFAULT_RETRY_DELAY,
            follow_redirects: FollowRedirects::Safe,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            verify_tls: true,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            allow_private_addresses: false,
        }
    }
}

/// The shared state behind a [`Fetcher`] or a [`FetcherSession`].
#[derive(Debug)]
pub(crate) struct Inner {
    pub(crate) config: Config,
    pub(crate) client: reqwest::Client,
    /// `Some` only for sessions: the cookie jar every client built from this state shares.
    pub(crate) jar: Option<Arc<reqwest::cookie::Jar>>,
}

/// Build a `reqwest` client for one proxy/redirect combination.
pub(crate) fn build_client(
    config: &Config,
    proxy: Option<&str>,
    redirect: FollowRedirects,
    jar: Option<&Arc<reqwest::cookie::Jar>>,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(config.timeout)
        .redirect(super::redirect::policy(redirect, config.max_redirects))
        .tls_danger_accept_invalid_certs(!config.verify_tls);

    if !config.allow_private_addresses {
        // Checks what a hostname actually resolves to, which the redirect policy — a synchronous
        // callback — cannot do. Applies to the first hop and to every redirect.
        builder = builder.dns_resolver(Arc::new(super::redirect::PrivateAddressGuard));
    }

    if let Some(proxy) = proxy {
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|e| Error::Http(format!("invalid proxy `{proxy}`: {e}")))?;
        builder = builder.proxy(proxy);
    }

    if let Some(jar) = jar {
        builder = builder.cookie_provider(Arc::clone(jar));
    }

    builder
        .build()
        .map_err(|e| Error::Http(format!("could not build the HTTP client: {e}")))
}

/// A configured HTTP client that builds requests. Cheap to clone.
///
/// Every request made through a `Fetcher` is independent: there is no cookie jar, matching
/// Scrapling's module-level `Fetcher`. Use [`FetcherSession`] when cookies must persist.
#[derive(Debug, Clone)]
pub struct Fetcher {
    inner: Arc<Inner>,
}

impl Fetcher {
    /// A fetcher with Scrapling's defaults (stealth headers on, 3 retries, safe redirects).
    pub fn new() -> Result<Fetcher> {
        Fetcher::builder().build()
    }

    /// Start configuring a fetcher.
    pub fn builder() -> FetcherBuilder {
        FetcherBuilder::default()
    }

    /// Begin a GET request.
    pub fn get(&self, url: &str) -> RequestBuilder {
        self.request("GET", url)
    }

    /// Begin a POST request.
    pub fn post(&self, url: &str) -> RequestBuilder {
        self.request("POST", url)
    }

    /// Begin a PUT request.
    pub fn put(&self, url: &str) -> RequestBuilder {
        self.request("PUT", url)
    }

    /// Begin a DELETE request.
    ///
    /// Sending a body with `DELETE` makes some servers reject the request
    /// (RFC 7231 §4.3.5), exactly as Python warns.
    pub fn delete(&self, url: &str) -> RequestBuilder {
        self.request("DELETE", url)
    }

    /// Begin a request with any method.
    pub fn request(&self, method: &str, url: &str) -> RequestBuilder {
        RequestBuilder::new(Arc::clone(&self.inner), method, url)
    }
}

impl Default for Fetcher {
    /// A fetcher with Scrapling's defaults.
    ///
    /// # Panics
    ///
    /// Only if the TLS backend fails to start, which cannot depend on user input. Use
    /// [`Fetcher::new`] when you want that as an error instead.
    fn default() -> Self {
        match Fetcher::new() {
            Ok(fetcher) => fetcher,
            Err(error) => panic!("could not start the default HTTP client: {error}"),
        }
    }
}

/// Builder for [`Fetcher`] and [`FetcherSession`].
#[derive(Debug, Clone, Default)]
pub struct FetcherBuilder {
    config: Config,
}

impl FetcherBuilder {
    /// Send the static header profile of this browser.
    pub fn impersonate(mut self, profile: BrowserProfile) -> Self {
        self.config.impersonate = Some(profile);
        self
    }

    /// Send no browser profile at all; only the headers you set yourself go out.
    pub fn no_impersonate(mut self) -> Self {
        self.config.impersonate = None;
        self
    }

    /// Add realistic browser headers and a Google referer. Default `true`.
    pub fn stealthy_headers(mut self, yes: bool) -> Self {
        self.config.stealthy_headers = yes;
        self
    }

    /// Route every request through this proxy URL, e.g.
    /// `http://user:pass@localhost:8030`.
    pub fn proxy(mut self, proxy: &str) -> Self {
        self.config.proxy = Some(proxy.to_string());
        self
    }

    /// Pick a proxy per request from this rotator (mutually exclusive with
    /// [`FetcherBuilder::proxy`]; building fails when both are set, as Python raises).
    pub fn proxy_rotator(mut self, rotator: ProxyRotator) -> Self {
        self.config.proxy_rotator = Some(rotator);
        self
    }

    /// Per-request timeout. Default 30s.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.timeout = timeout;
        self
    }

    /// Headers added to every request.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.config.headers = headers;
        self
    }

    /// How many times to retry a failed request. Default 3.
    ///
    /// The request is always attempted once, so anything below 1 means "no retry".
    pub fn retries(mut self, retries: u32) -> Self {
        self.config.retries = retries;
        self
    }

    /// How long to wait between retries. Default 1s.
    pub fn retry_delay(mut self, delay: Duration) -> Self {
        self.config.retry_delay = delay;
        self
    }

    /// Redirect policy. Default [`FollowRedirects::Safe`].
    pub fn follow_redirects(mut self, policy: FollowRedirects) -> Self {
        self.config.follow_redirects = policy;
        self
    }

    /// Maximum redirects to follow. Default 30.
    pub fn max_redirects(mut self, max: usize) -> Self {
        self.config.max_redirects = max;
        self
    }

    /// Verify TLS certificates. Default `true`.
    pub fn verify_tls(mut self, yes: bool) -> Self {
        self.config.verify_tls = yes;
        self
    }

    /// How much of a response body to read before giving up. Default 64 MiB; `0` removes the
    /// limit.
    ///
    /// Python has no equivalent setting — this exists so that a hostile or broken server cannot
    /// make the process allocate without bound. A body that goes over the limit is an
    /// [`Error::Http`], not a truncated [`Response`].
    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.config.max_response_bytes = max;
        self
    }

    /// Allow requests aimed at loopback, link-local, private and unspecified addresses.
    /// Default `false`.
    ///
    /// Python has no equivalent setting. With the default, a request is refused before it is
    /// sent when its URL names such an address — as an IP literal, as one of the internal-looking
    /// names (`localhost`, `*.local`, `*.internal`, …), or through DNS, because the client
    /// resolves each host and checks the addresses it got back. That matters because URLs reach
    /// the fetcher straight from scraped markup in a crawl, and `http://169.254.169.254/` or
    /// `http://127.0.0.1:6379/` would otherwise be fetched and handed to the spider.
    ///
    /// Turn it on to point the fetcher at a service on your own machine or network. It does not
    /// change [`FollowRedirects::Safe`], which independently refuses redirect *hops* into those
    /// ranges.
    pub fn allow_private_addresses(mut self, yes: bool) -> Self {
        self.config.allow_private_addresses = yes;
        self
    }

    /// Build the fetcher.
    pub fn build(self) -> Result<Fetcher> {
        let inner = self.into_inner(false)?;
        Ok(Fetcher {
            inner: Arc::new(inner),
        })
    }

    /// Build a session (one client, one cookie jar) instead of a stateless fetcher.
    pub fn build_session(self) -> Result<FetcherSession> {
        let inner = self.into_inner(true)?;
        Ok(FetcherSession::from_inner(Arc::new(inner)))
    }

    fn into_inner(self, with_jar: bool) -> Result<Inner> {
        let config = self.config;
        if config.proxy_rotator.is_some() && config.proxy.is_some() {
            return Err(Error::Http(
                "cannot use `proxy_rotator` together with `proxy`; use either a static proxy or \
                 proxy rotation, not both"
                    .to_string(),
            ));
        }
        let jar = if with_jar {
            Some(Arc::new(reqwest::cookie::Jar::default()))
        } else {
            None
        };
        let client = build_client(
            &config,
            config.proxy.as_deref(),
            config.follow_redirects,
            jar.as_ref(),
        )?;
        Ok(Inner {
            config,
            client,
            jar,
        })
    }
}

/// The body a request carries, if any.
#[derive(Debug, Clone)]
enum BodyKind {
    /// A raw byte body; no content type is implied.
    Raw(Vec<u8>),
    /// `application/x-www-form-urlencoded` fields.
    Form(Vec<(String, String)>),
    /// An already-serialized `application/json` body.
    Json(Vec<u8>),
}

/// One in-flight request, with per-request overrides of the fetcher's settings.
#[derive(Debug)]
pub struct RequestBuilder {
    inner: Arc<Inner>,
    method: String,
    url: String,
    query: Vec<(String, String)>,
    headers: HeaderMap,
    body: Option<BodyKind>,
    timeout: Option<Duration>,
    proxy: Option<String>,
    retries: Option<u32>,
    follow_redirects: Option<FollowRedirects>,
    impersonate: Option<BrowserProfile>,
}

impl RequestBuilder {
    pub(crate) fn new(inner: Arc<Inner>, method: &str, url: &str) -> RequestBuilder {
        RequestBuilder {
            inner,
            method: method.to_string(),
            url: url.to_string(),
            query: Vec::new(),
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            proxy: None,
            retries: None,
            follow_redirects: None,
            impersonate: None,
        }
    }

    /// Add query-string parameters.
    pub fn query<I, K, V>(mut self, params: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.query
            .extend(params.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Set or replace a header for this request.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Merge a whole header map into this request.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (name, value) in headers.iter() {
            self.headers.insert(name, value);
        }
        self
    }

    /// Send a URL-encoded form body.
    pub fn form<I, K, V>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.body = Some(BodyKind::Form(
            fields
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        ));
        self
    }

    /// Send a JSON body.
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self> {
        let bytes = serde_json::to_vec(value)?;
        self.body = Some(BodyKind::Json(bytes));
        Ok(self)
    }

    /// Send a raw body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(BodyKind::Raw(body.into()));
        self
    }

    /// Override the timeout for this request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Override the proxy for this request.
    ///
    /// A per-request proxy wins over a configured rotator, matching Python's `static_proxy`.
    pub fn proxy(mut self, proxy: &str) -> Self {
        self.proxy = Some(proxy.to_string());
        self
    }

    /// Override the retry count for this request.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = Some(retries);
        self
    }

    /// Override the redirect policy for this request.
    pub fn follow_redirects(mut self, policy: FollowRedirects) -> Self {
        self.follow_redirects = Some(policy);
        self
    }

    /// Override the impersonated browser profile for this request.
    pub fn impersonate(mut self, profile: BrowserProfile) -> Self {
        self.impersonate = Some(profile);
        self
    }

    /// Send the request, retrying as configured, and parse the result.
    ///
    /// Transport failures are retried up to `retries` times with `retry_delay` in between; a
    /// configured [`ProxyRotator`] hands out a fresh proxy on every attempt, as Python does.
    /// An HTTP error *status* is not a transport failure and is returned as a [`Response`].
    pub async fn send(self) -> Result<Response> {
        let RequestBuilder {
            inner,
            method,
            url,
            query,
            headers,
            body,
            timeout,
            proxy,
            retries,
            follow_redirects,
            impersonate,
        } = self;
        let config = &inner.config;

        let method = method.trim().to_ascii_uppercase();
        let http_method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| Error::Http(format!("invalid HTTP method `{method}`: {e}")))?;

        let mut target =
            url::Url::parse(&url).map_err(|e| Error::Http(format!("invalid url `{url}`: {e}")))?;
        // `reqwest` would reject these too, but only from inside `send`, where the failure looks
        // like a transport error and gets retried `retries` times for nothing. Rejecting a
        // `file:`/`data:` URL here also keeps the fetcher from being pointed at the filesystem.
        if !matches!(target.scheme(), "http" | "https") {
            return Err(Error::Http(format!(
                "unsupported url scheme `{}` in `{url}`; only http and https are fetched",
                target.scheme()
            )));
        }
        if !target.has_host() {
            return Err(Error::Http(format!("the url `{url}` has no host")));
        }
        // The SSRF guard has to cover the URL the caller passed, not only redirect hops: in a
        // crawl this URL comes out of scraped markup, so without this an anchor pointing at
        // `http://169.254.169.254/latest/meta-data/` or at a service on localhost is fetched and
        // its body handed back to the spider. Hosts that are names rather than literals are
        // caught later, by the resolver guard `build_client` installs.
        if !config.allow_private_addresses && super::redirect::is_blocked_redirect_target(&target) {
            return Err(Error::Http(format!(
                "{}: {target}",
                super::redirect::BLOCKED_TARGET_MESSAGE
            )));
        }
        if !query.is_empty() {
            let mut pairs = target.query_pairs_mut();
            for (key, value) in &query {
                pairs.append_pair(key, value);
            }
        }

        // Which static header profile to send: the per-request override, else the session's,
        // else Chrome when stealth is on and impersonation was switched off.
        let stealth = config.stealthy_headers;
        let profile = impersonate.or(config.impersonate).or(if stealth {
            Some(BrowserProfile::default())
        } else {
            None
        });

        let mut request_headers = headers_job(&config.headers, &headers, profile, stealth);
        let body_bytes = match &body {
            None => None,
            Some(BodyKind::Raw(bytes)) => Some(bytes.clone()),
            Some(BodyKind::Json(bytes)) => {
                if !request_headers.contains_key("content-type") {
                    request_headers.insert("Content-Type", "application/json");
                }
                Some(bytes.clone())
            }
            Some(BodyKind::Form(fields)) => {
                if !request_headers.contains_key("content-type") {
                    request_headers.insert("Content-Type", "application/x-www-form-urlencoded");
                }
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                for (key, value) in fields {
                    serializer.append_pair(key, value);
                }
                Some(serializer.finish().into_bytes())
            }
        };

        let redirect = follow_redirects.unwrap_or(config.follow_redirects);
        let effective_timeout = timeout.unwrap_or(config.timeout);
        // Python: `max(1, retries or 1)` — the request always goes out once.
        let attempts = retries.unwrap_or(config.retries).max(1);
        let reqwest_headers = to_reqwest_headers(&request_headers);

        let mut last_error = Error::Http("the request was never attempted".to_string());
        for attempt in 0..attempts {
            let attempt_proxy: Option<String> = match &proxy {
                Some(proxy) => Some(proxy.clone()),
                None => match &config.proxy_rotator {
                    Some(rotator) => Some(rotator.get_proxy()),
                    None => config.proxy.clone(),
                },
            };

            let client = if attempt_proxy == config.proxy && redirect == config.follow_redirects {
                inner.client.clone()
            } else {
                build_client(
                    config,
                    attempt_proxy.as_deref(),
                    redirect,
                    inner.jar.as_ref(),
                )?
            };

            let mut request = client
                .request(http_method.clone(), target.clone())
                .headers(reqwest_headers.clone())
                .timeout(effective_timeout);
            if let Some(bytes) = &body_bytes {
                request = request.body(bytes.clone());
            }

            match run_attempt(request, config.max_response_bytes).await {
                Ok(fetched) => {
                    return build_response(fetched, &request_headers, &method, attempt_proxy);
                }
                // A body that blew the size limit is not a transport hiccup: retrying it would
                // just download the same thing again, so it fails straight away.
                Err(AttemptError::TooLarge(limit)) => {
                    return Err(Error::Http(format!(
                        "the response body went over the {limit} byte limit; raise it with \
                         `max_response_bytes`"
                    )));
                }
                Err(AttemptError::Transport(error)) => {
                    last_error = Error::Http(describe(&error));
                    if attempt + 1 < attempts {
                        if super::proxy::is_proxy_error(&last_error) {
                            tracing::warn!(
                                proxy = attempt_proxy.as_deref().unwrap_or("<none>"),
                                attempt = attempt + 1,
                                "proxy failed, retrying"
                            );
                        } else {
                            tracing::warn!(
                                attempt = attempt + 1,
                                error = %last_error,
                                "request failed, retrying"
                            );
                        }
                        tokio::time::sleep(config.retry_delay).await;
                        continue;
                    }
                    tracing::error!(attempts, error = %last_error, "request failed");
                    return Err(last_error);
                }
            }
        }

        Err(last_error)
    }
}

/// What one successful attempt kept from the transport.
struct Fetched {
    status: reqwest::StatusCode,
    final_url: String,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

/// Why one attempt did not produce a [`Fetched`].
enum AttemptError {
    /// The transport failed; worth retrying.
    Transport(reqwest::Error),
    /// The body went past the configured limit; retrying would not help.
    TooLarge(usize),
}

/// Send one request and read its body, refusing to buffer more than `max_response_bytes`.
///
/// `max_response_bytes` of `0` means "no limit". The body is read chunk by chunk rather than
/// through `Response::bytes` so that an over-long body is dropped as it arrives instead of being
/// fully allocated first, and so that a lying `Content-Length` cannot make us pre-allocate more
/// than `MAX_PREALLOCATED_BODY`.
async fn run_attempt(
    request: reqwest::RequestBuilder,
    max_response_bytes: usize,
) -> std::result::Result<Fetched, AttemptError> {
    let mut response = request.send().await.map_err(AttemptError::Transport)?;
    let status = response.status();
    let final_url = response.url().to_string();
    let headers = response.headers().clone();

    let limit = if max_response_bytes == 0 {
        usize::MAX
    } else {
        max_response_bytes
    };
    if let Some(claimed) = response.content_length() {
        if claimed > limit as u64 {
            return Err(AttemptError::TooLarge(limit));
        }
    }

    let hint = response
        .content_length()
        .unwrap_or(0)
        .min(MAX_PREALLOCATED_BODY as u64) as usize;
    let mut body: Vec<u8> = Vec::with_capacity(hint);
    while let Some(chunk) = response.chunk().await.map_err(AttemptError::Transport)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(AttemptError::TooLarge(limit));
        }
        body.extend_from_slice(&chunk);
    }

    Ok(Fetched {
        status,
        final_url,
        headers,
        body,
    })
}

/// Render a `reqwest` error together with its whole source chain.
///
/// `reqwest`'s own `Display` stops at "error sending request for url (...)", which hides the
/// transport cause that [`super::proxy::is_proxy_error`] needs to see.
///
/// The walk is bounded by `MAX_ERROR_SOURCE_DEPTH`: an error chain is remote-influenced data and
/// a self-referential one would otherwise loop forever.
fn describe(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    let mut depth = 0;
    while let Some(cause) = source {
        if depth >= MAX_ERROR_SOURCE_DEPTH {
            break;
        }
        depth += 1;
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

/// Convert our header map into `reqwest`'s, dropping any header the HTTP layer would reject
/// rather than panicking on it.
///
/// `reqwest::header::HeaderMap::with_capacity` panics past its own maximum, so the hint is
/// clamped; the map grows on its own if it really needs to.
fn to_reqwest_headers(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(headers.len().min(1024));
    for (name, value) in headers.iter() {
        let name = match reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                tracing::debug!(header = name, "dropping an invalid header name");
                continue;
            }
        };
        let value = match reqwest::header::HeaderValue::from_str(value) {
            Ok(value) => value,
            Err(_) => {
                tracing::debug!(header = %name, "dropping an invalid header value");
                continue;
            }
        };
        out.insert(name, value);
    }
    out
}

/// RFC 6265 §5.1.4 default-path: the request path up to (not including) its last `/`, or `/`.
fn default_cookie_path(path: &str) -> String {
    if !path.starts_with('/') {
        return "/".to_string();
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    }
}

/// Turn everything we kept from the transport into a [`Response`].
///
/// Port of `ResponseFactory.from_http_request`. The one field Python fills that this cannot is
/// `history`: `reqwest` follows redirects inside the client and never hands back the
/// intermediate responses, so [`Response::history`] stays empty here.
fn build_response(
    fetched: Fetched,
    request_headers: &HeaderMap,
    method: &str,
    proxy: Option<String>,
) -> Result<Response> {
    let Fetched {
        status,
        final_url,
        headers: response_headers,
        body,
    } = fetched;

    let mut headers = HeaderMap::new();
    for (name, value) in response_headers.iter() {
        match value.to_str() {
            Ok(value) => headers.insert(name.as_str(), value),
            Err(_) => headers.insert(
                name.as_str(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ),
        }
    }

    let encoding = encoding_from_content_type(headers.get("content-type"));
    let reason = status
        .canonical_reason()
        .map(|reason| reason.to_string())
        .unwrap_or_else(|| crate::response::StatusText::get(status.as_u16()).to_string());

    let (default_domain, default_path) = match url::Url::parse(&final_url) {
        Ok(parsed) => (
            parsed.host_str().unwrap_or_default().to_string(),
            default_cookie_path(parsed.path()),
        ),
        Err(_) => (String::new(), "/".to_string()),
    };
    let cookies: Vec<Cookie> = response_headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| Cookie::parse_set_cookie(value, &default_domain, &default_path))
        .collect();

    let mut meta = std::collections::HashMap::new();
    meta.insert(
        META_PROXY.to_string(),
        match &proxy {
            Some(proxy) => serde_json::Value::String(proxy.clone()),
            None => serde_json::Value::Null,
        },
    );

    tracing::info!(
        status = status.as_u16(),
        method,
        url = %final_url,
        referer = request_headers.get("referer").unwrap_or_default(),
        "fetched"
    );

    Ok(Response::new(final_url, body, status.as_u16())?
        .with_reason(reason)
        .with_headers(headers)
        .with_request_headers(request_headers.clone())
        .with_cookies(cookies)
        .with_method(method)
        .with_meta(meta)
        .with_encoding(encoding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use wiremock::matchers::{body_string, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn html(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.to_string(), "text/html; charset=utf-8")
    }

    /// A builder aimed at the local mock server.
    ///
    /// A `Fetcher` refuses loopback and private addresses unless it is told they are fine — that
    /// is what `allow_private_addresses` is for — and every server in these tests listens on
    /// `127.0.0.1`, so they all opt in explicitly.
    fn local() -> FetcherBuilder {
        Fetcher::builder().allow_private_addresses(true)
    }

    /// A port nothing listens on, so connecting to it fails immediately.
    fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    #[tokio::test]
    async fn get_parses_the_body_and_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hello"))
            .respond_with(
                html("<html><body><h1 class='t'>Hello</h1></body></html>")
                    .append_header("set-cookie", "sid=abc; Path=/; Domain=example.test"),
            )
            .mount(&server)
            .await;

        let fetcher = local().build().expect("fetcher");
        let response = fetcher
            .get(&format!("{}/hello", server.uri()))
            .send()
            .await
            .expect("response");

        assert_eq!(response.status, 200);
        assert_eq!(response.reason, "OK");
        assert_eq!(response.method, "GET");
        assert!(response.is_success());
        assert_eq!(response.encoding, "utf-8");
        assert!(response.text_body().contains("Hello"));
        assert_eq!(response.cookies.len(), 1);
        assert_eq!(response.cookies[0].name, "sid");
        assert_eq!(response.cookies[0].domain, "example.test");
        assert_eq!(
            response.meta.get(META_PROXY),
            Some(&serde_json::Value::Null)
        );
    }

    #[tokio::test]
    async fn stealthy_headers_are_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/h"))
            .and(header("referer", "https://www.google.com/"))
            .and(header("user-agent", BrowserProfile::Firefox.user_agent()))
            .and(header("upgrade-insecure-requests", "1"))
            .respond_with(html("<html></html>"))
            .mount(&server)
            .await;

        let fetcher = local()
            .impersonate(BrowserProfile::Firefox)
            .build()
            .expect("fetcher");
        let response = fetcher
            .get(&format!("{}/h", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(
            response.request_headers.get("referer"),
            Some("https://www.google.com/")
        );
    }

    #[tokio::test]
    async fn user_headers_beat_the_profile() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/h"))
            .and(header("user-agent", "mine/1.0"))
            .and(header("referer", "https://mysite.test/"))
            .respond_with(html("<html></html>"))
            .mount(&server)
            .await;

        let fetcher = local().build().expect("fetcher");
        let response = fetcher
            .get(&format!("{}/h", server.uri()))
            .header("User-Agent", "mine/1.0")
            .header("Referer", "https://mysite.test/")
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn query_parameters_are_appended() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "rust crate"))
            .and(query_param("page", "2"))
            .respond_with(html("<html>ok</html>"))
            .mount(&server)
            .await;

        let fetcher = local().build().expect("fetcher");
        let response = fetcher
            .get(&format!("{}/search", server.uri()))
            .query([("q", "rust crate"), ("page", "2")])
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn post_sends_a_form_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string("user=ada&pass=a+b%26c"))
            .respond_with(ResponseTemplate::new(201).set_body_raw("<html>ok</html>", "text/html"))
            .mount(&server)
            .await;

        let fetcher = local().build().expect("fetcher");
        let response = fetcher
            .post(&format!("{}/login", server.uri()))
            .form([("user", "ada"), ("pass", "a b&c")])
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 201);
        assert_eq!(response.reason, "Created");
    }

    #[tokio::test]
    async fn post_sends_a_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let fetcher = local().build().expect("fetcher");
        let response = fetcher
            .post(&format!("{}/api", server.uri()))
            .json(&serde_json::json!({"name": "ada"}))
            .expect("json body")
            .send()
            .await
            .expect("response");
        let value: serde_json::Value = response.json().expect("json");
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn safe_redirects_refuse_private_targets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/go"))
            .respond_with(
                ResponseTemplate::new(302)
                    .append_header("location", format!("{}/target", server.uri()).as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/target"))
            .respond_with(html("<html>arrived</html>"))
            .mount(&server)
            .await;

        // The mock server listens on 127.0.0.1, which `Safe` refuses to be redirected to.
        let safe = local().retries(1).build().expect("fetcher");
        let error = safe
            .get(&format!("{}/go", server.uri()))
            .send()
            .await
            .expect_err("safe mode must refuse a loopback redirect");
        assert!(matches!(error, Error::Http(_)), "got {error:?}");

        // `None` hands the 3xx back untouched.
        let none = local()
            .follow_redirects(FollowRedirects::None)
            .build()
            .expect("fetcher");
        let response = none
            .get(&format!("{}/go", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 302);

        // `All` follows it.
        let all = local()
            .follow_redirects(FollowRedirects::All)
            .build()
            .expect("fetcher");
        let response = all
            .get(&format!("{}/go", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 200);
        assert!(response.url.ends_with("/target"));
        assert!(response.text_body().contains("arrived"));
    }

    #[tokio::test]
    async fn transport_failures_are_retried_with_a_delay() {
        let url = format!("http://127.0.0.1:{}/", closed_port());
        let fetcher = local()
            .retries(3)
            .retry_delay(Duration::from_millis(40))
            .build()
            .expect("fetcher");

        let started = std::time::Instant::now();
        let error = fetcher.get(&url).send().await.expect_err("must fail");
        let elapsed = started.elapsed();

        assert!(matches!(error, Error::Http(_)), "got {error:?}");
        // Two pauses between three attempts.
        assert!(
            elapsed >= Duration::from_millis(80),
            "retries did not wait, took {elapsed:?}"
        );
        // The transport cause must survive into the message so `is_proxy_error` can see it.
        assert!(crate::http::is_proxy_error(&error), "got {error}");
    }

    #[tokio::test]
    async fn retries_below_one_still_send_once() {
        let url = format!("http://127.0.0.1:{}/", closed_port());
        let fetcher = local()
            .retries(0)
            .retry_delay(Duration::from_secs(30))
            .build()
            .expect("fetcher");

        let started = std::time::Instant::now();
        let error = fetcher.get(&url).send().await.expect_err("must fail");
        assert!(matches!(error, Error::Http(_)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_rotator_hands_out_a_fresh_proxy_per_attempt() {
        let rotator = ProxyRotator::new(vec![
            format!("http://127.0.0.1:{}", closed_port()),
            format!("http://127.0.0.1:{}", closed_port()),
        ])
        .expect("rotator");
        let fetcher = local()
            .proxy_rotator(rotator.clone())
            .retries(2)
            .retry_delay(Duration::from_millis(1))
            .build()
            .expect("fetcher");

        let error = fetcher
            .get("http://example.invalid/")
            .send()
            .await
            .expect_err("both proxies are dead");
        assert!(matches!(error, Error::Http(_)));
        // Two attempts consumed two proxies, so the cursor is back at the first one.
        assert_eq!(rotator.get_proxy(), rotator.proxies()[0]);
    }

    #[test]
    fn a_proxy_and_a_rotator_cannot_be_combined() {
        let rotator = ProxyRotator::new(vec!["http://a:1".to_string()]).expect("rotator");
        let error = local()
            .proxy("http://b:2")
            .proxy_rotator(rotator)
            .build()
            .expect_err("must be rejected");
        assert!(matches!(error, Error::Http(_)));
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused_rather_than_buffered() {
        let server = MockServer::start().await;
        let big = "x".repeat(64 * 1024);
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(html(&big))
            .mount(&server)
            .await;

        let fetcher = local()
            .max_response_bytes(1024)
            .retries(3)
            .retry_delay(Duration::from_secs(30))
            .build()
            .expect("fetcher");

        let started = std::time::Instant::now();
        let error = fetcher
            .get(&format!("{}/big", server.uri()))
            .send()
            .await
            .expect_err("the body is over the limit");
        assert!(matches!(error, Error::Http(_)), "got {error:?}");
        // It is not a transport failure, so it must not be retried.
        assert!(started.elapsed() < Duration::from_secs(5));

        // Without a limit the same body comes back whole.
        let unlimited = local().max_response_bytes(0).build().expect("fetcher");
        let response = unlimited
            .get(&format!("{}/big", server.uri()))
            .send()
            .await
            .expect("response");
        assert!(response.body.len() >= big.len());
    }

    #[test]
    fn cookie_default_paths_follow_rfc_6265() {
        assert_eq!(default_cookie_path("/"), "/");
        assert_eq!(default_cookie_path("/hello"), "/");
        assert_eq!(default_cookie_path("/a/b/c"), "/a/b");
        assert_eq!(default_cookie_path("relative"), "/");
    }

    #[test]
    fn invalid_headers_are_dropped_not_panicked_on() {
        let mut headers = HeaderMap::new();
        headers.insert("Good", "value");
        headers.insert("Bad\nName", "value");
        headers.insert("BadValue", "line\nbreak");
        let converted = to_reqwest_headers(&headers);
        assert_eq!(converted.len(), 1);
        assert!(converted.contains_key("good"));
    }

    #[tokio::test]
    async fn a_bad_url_is_an_error_not_a_panic() {
        let fetcher = local().build().expect("fetcher");
        let error = fetcher
            .get("not a url")
            .send()
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::Http(_)));
    }

    /// The SSRF guard has to cover the URL the caller passed, not only redirect hops: in a crawl
    /// that URL comes straight out of scraped markup.
    #[tokio::test]
    async fn private_targets_are_refused_before_anything_is_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(html("<html>a service on localhost</html>"))
            .mount(&server)
            .await;

        // A long retry budget, to show the refusal does not spend any of it.
        let guarded = Fetcher::builder()
            .retries(5)
            .retry_delay(Duration::from_secs(30))
            .build()
            .expect("fetcher");
        let started = std::time::Instant::now();
        for url in [
            format!("{}/", server.uri()),
            "http://169.254.169.254/latest/meta-data/".to_string(),
            "http://127.0.0.1:6379/".to_string(),
            "http://[::1]/".to_string(),
            "http://[::ffff:127.0.0.1]/".to_string(),
            "http://10.0.0.5/admin".to_string(),
            "http://redis.internal/".to_string(),
        ] {
            let error = guarded
                .get(&url)
                .send()
                .await
                .expect_err("a private target must be refused");
            let Error::Http(message) = &error else {
                panic!("{url}: got {error:?}");
            };
            assert!(
                message.contains("refused to request"),
                "{url}: got `{message}`"
            );
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            server
                .received_requests()
                .await
                .expect("the request log")
                .is_empty(),
            "nothing may reach a refused target"
        );

        // The same URL goes through once the caller says private addresses are fine.
        let response = local()
            .build()
            .expect("fetcher")
            .get(&format!("{}/", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn only_http_urls_are_fetched() {
        // Rejected before anything is sent, so the retry budget is never spent on it.
        let fetcher = local()
            .retries(5)
            .retry_delay(Duration::from_secs(30))
            .build()
            .expect("fetcher");

        let started = std::time::Instant::now();
        for url in ["file:///etc/passwd", "data:text/html,<html></html>"] {
            let error = fetcher.get(url).send().await.expect_err("must fail");
            assert!(matches!(error, Error::Http(_)), "{url}: got {error:?}");
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
