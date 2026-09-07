//! A browser session driven over the Chrome DevTools Protocol.
//!
//! Port of Scrapling's `DynamicSession` (`scrapling/engines/_browsers/_controllers.py`) onto
//! [`chromiumoxide`]. Playwright's page/route API has no direct equivalent here, so
//! navigation, request interception and the various waits are expressed with CDP events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::auth::Credentials;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EventRequestPaused, FailRequestParams,
};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
    EventResponseReceived, Headers, SetExtraHttpHeadersParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::Page;
use futures::StreamExt;

use crate::browser::blocking::RequestFilter;
use crate::browser::constants::{AD_DOMAINS, DEFAULT_ARGS, HARMFUL_ARGS, STEALTH_ARGS};
use crate::error::{Error, Result};
use crate::response::{Cookie, HeaderMap, Response, StatusText, META_PROXY};

/// How long the network must stay quiet before `network_idle` is satisfied.
///
/// Playwright's `networkidle` state, which Scrapling waits for, uses the same 500 ms window.
const NETWORK_IDLE_WINDOW: Duration = Duration::from_millis(500);

/// How often the selector and network-idle waits re-check their condition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long the main document's `Network.responseReceived` may still be in flight after the
/// navigation itself has settled.
///
/// Playwright's `page.goto` hands the navigation response back directly; over CDP it arrives on
/// a separate event stream, so the two can be observed out of order by a few milliseconds. This
/// is the grace period before a fetch is declared response-less, and it is deliberately short:
/// it only covers the hand-off, never a slow server.
const RESPONSE_GRACE: Duration = Duration::from_millis(250);

/// What `wait_selector` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitState {
    /// Present in the DOM.
    #[default]
    Attached,
    /// Removed from the DOM.
    Detached,
    /// Present and rendered.
    Visible,
    /// Present but not rendered.
    Hidden,
}

impl WaitState {
    /// The Playwright name of this state, as Scrapling spells it in `wait_selector_state`.
    pub fn as_str(&self) -> &'static str {
        match self {
            WaitState::Attached => "attached",
            WaitState::Detached => "detached",
            WaitState::Visible => "visible",
            WaitState::Hidden => "hidden",
        }
    }

    /// Whether an element reported as `observed` ("detached", "visible" or "hidden") satisfies
    /// this state.
    fn is_satisfied_by(&self, observed: &str) -> bool {
        match self {
            WaitState::Attached => observed != "detached",
            WaitState::Detached => observed == "detached",
            WaitState::Visible => observed == "visible",
            WaitState::Hidden => observed != "visible",
        }
    }
}

/// The resolved options of a [`DynamicSession`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DynamicSessionOptions {
    /// Run without a visible window.
    pub headless: bool,
    /// Block fonts, images, media and the other `EXTRA_RESOURCES` types.
    pub disable_resources: bool,
    /// Abort requests to these domains; subdomains match too.
    pub blocked_domains: Vec<String>,
    /// Also block the built-in ad and tracker domain list.
    pub block_ads: bool,
    /// How long a navigation, and each wait after it, may take.
    pub timeout: Duration,
    /// Extra wait after the page settles.
    pub wait: Duration,
    /// Wait for this CSS selector before returning.
    pub wait_selector: Option<String>,
    /// Which state that selector must reach.
    pub wait_selector_state: WaitState,
    /// Wait for the network to go idle.
    pub network_idle: bool,
    /// Override the user agent.
    pub useragent: Option<String>,
    /// Headers added to every navigation.
    pub extra_headers: HeaderMap,
    /// Route the browser through a proxy.
    pub proxy: Option<String>,
    /// Extra Chromium command-line flags.
    pub extra_flags: Vec<String>,
    /// Add `STEALTH_ARGS` and drop `HARMFUL_ARGS`.
    pub stealth: bool,
}

impl Default for DynamicSessionOptions {
    fn default() -> Self {
        DynamicSessionOptions {
            headless: true,
            disable_resources: false,
            blocked_domains: Vec::new(),
            block_ads: false,
            timeout: Duration::from_secs(30),
            wait: Duration::ZERO,
            wait_selector: None,
            wait_selector_state: WaitState::default(),
            network_idle: false,
            useragent: None,
            extra_headers: HeaderMap::new(),
            proxy: None,
            extra_flags: Vec::new(),
            stealth: true,
        }
    }
}

impl DynamicSessionOptions {
    /// The blocking rules these options describe, `block_ads` already folded in.
    pub fn request_filter(&self) -> RequestFilter {
        let mut domains: Vec<String> = self.blocked_domains.clone();
        if self.block_ads {
            domains.extend(AD_DOMAINS.iter().map(|domain| domain.to_string()));
        }
        RequestFilter::new(self.disable_resources, domains)
    }
}

/// Builder for [`DynamicSession`].
#[derive(Debug, Clone, Default)]
pub struct DynamicSessionBuilder {
    options: DynamicSessionOptions,
}

impl DynamicSessionBuilder {
    /// Run without a visible window. Default `true`.
    pub fn headless(mut self, yes: bool) -> Self {
        self.options.headless = yes;
        self
    }

    /// Block fonts, images, media and other non-essential resources. Default `false`.
    pub fn disable_resources(mut self, yes: bool) -> Self {
        self.options.disable_resources = yes;
        self
    }

    /// Abort requests to these domains.
    pub fn blocked_domains(mut self, domains: &[&str]) -> Self {
        self.options.blocked_domains = domains.iter().map(|domain| domain.to_string()).collect();
        self
    }

    /// Block known ad and tracker domains. Default `false`.
    pub fn block_ads(mut self, yes: bool) -> Self {
        self.options.block_ads = yes;
        self
    }

    /// How long a navigation may take. Default 30s.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.options.timeout = timeout;
        self
    }

    /// Extra wait after the page settles. Default zero.
    pub fn wait(mut self, wait: Duration) -> Self {
        self.options.wait = wait;
        self
    }

    /// Wait for this CSS selector before returning.
    pub fn wait_selector(mut self, selector: &str) -> Self {
        self.options.wait_selector = Some(selector.to_string());
        self
    }

    /// Which state that selector must reach.
    pub fn wait_selector_state(mut self, state: WaitState) -> Self {
        self.options.wait_selector_state = state;
        self
    }

    /// Wait for the network to go idle. Default `false`.
    pub fn network_idle(mut self, yes: bool) -> Self {
        self.options.network_idle = yes;
        self
    }

    /// Override the user agent.
    pub fn useragent(mut self, useragent: &str) -> Self {
        self.options.useragent = Some(useragent.to_string());
        self
    }

    /// Headers added to every navigation.
    pub fn extra_headers(mut self, headers: HeaderMap) -> Self {
        self.options.extra_headers = headers;
        self
    }

    /// Route the browser through a proxy.
    pub fn proxy(mut self, proxy: &str) -> Self {
        self.options.proxy = Some(proxy.to_string());
        self
    }

    /// Extra Chromium command-line flags.
    pub fn extra_flags(mut self, flags: &[&str]) -> Self {
        self.options.extra_flags = flags.iter().map(|flag| flag.to_string()).collect();
        self
    }

    /// Add `STEALTH_ARGS` and drop `HARMFUL_ARGS`. Default `true`.
    pub fn stealth(mut self, yes: bool) -> Self {
        self.options.stealth = yes;
        self
    }

    /// Build the session.
    pub fn build(self) -> DynamicSession {
        DynamicSession::from(self.options)
    }

    /// The options this builder describes.
    pub fn options(self) -> DynamicSessionOptions {
        self.options
    }
}

/// A live browser session that fetches pages through a real Chromium.
#[derive(Debug, Clone)]
pub struct DynamicSession {
    options: Arc<DynamicSessionOptions>,
    filter: Arc<RequestFilter>,
    inner: Arc<SessionInner>,
}

#[derive(Debug)]
struct SessionInner {
    state: tokio::sync::Mutex<Option<Running>>,
    alive: AtomicBool,
}

#[derive(Debug)]
struct Running {
    browser: Browser,
    handler: tokio::task::JoinHandle<()>,
}

impl Default for DynamicSession {
    fn default() -> Self {
        DynamicSession::new()
    }
}

impl From<DynamicSessionOptions> for DynamicSession {
    fn from(options: DynamicSessionOptions) -> Self {
        let filter = options.request_filter();
        DynamicSession {
            options: Arc::new(options),
            filter: Arc::new(filter),
            inner: Arc::new(SessionInner {
                state: tokio::sync::Mutex::new(None),
                alive: AtomicBool::new(false),
            }),
        }
    }
}

impl DynamicSession {
    /// A session with the default options.
    pub fn new() -> DynamicSession {
        DynamicSession::from(DynamicSessionOptions::default())
    }

    /// Start configuring a session.
    pub fn builder() -> DynamicSessionBuilder {
        DynamicSessionBuilder::default()
    }

    /// The options this session was built with.
    pub fn options(&self) -> &DynamicSessionOptions {
        &self.options
    }

    /// Whether the browser is running.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// Launch the browser. Must be called before [`DynamicSession::fetch`].
    pub async fn start(&self) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        if state.is_some() {
            return Err(Error::Browser("session has already been started".into()));
        }

        let args = launch_args(&self.options)?;
        let mut config = BrowserConfig::builder()
            // Chromium's own defaults are replaced wholesale: puppeteer's list, which
            // `chromiumoxide` ships, carries several `HARMFUL_ARGS`.
            //
            // One harmful flag survives this: `chromiumoxide` appends `--disable-extensions`
            // itself whenever no extension is loaded, and that happens after the caller's own
            // flags. There is no switch to turn it off, so it is the one entry of
            // `HARMFUL_ARGS` this port cannot keep off the command line.
            .disable_default_args()
            .request_timeout(self.options.timeout);

        if self.options.headless {
            config = config.new_headless_mode();
        } else {
            config = config.with_head();
        }

        for flag in &args {
            let flag = flag.trim_start_matches('-');
            config = match flag.split_once('=') {
                Some((key, value)) => config.arg((key, value)),
                None => config.arg(flag),
            };
        }

        // Interception is only turned on when something is actually blocked; otherwise every
        // request would have to make a round trip through us for nothing.
        if self.filter.is_active() {
            config = config.enable_request_intercept();
        }

        let config = config.build().map_err(|message| {
            Error::Browser(format!("browser configuration failed: {message}"))
        })?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|error| Error::Browser(format!("failed to launch chromium: {error}")))?;

        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });

        *state = Some(Running { browser, handler });
        self.inner.alive.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Shut the browser down. Closing a session that never started is not an error.
    pub async fn close(&self) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        self.inner.alive.store(false, Ordering::SeqCst);
        let Some(mut running) = state.take() else {
            return Ok(());
        };
        let closed = running.browser.close().await;
        let _ = running.browser.wait().await;
        running.handler.abort();
        closed.map_err(|error| Error::Browser(format!("failed to close chromium: {error}")))?;
        Ok(())
    }

    /// Load a URL and return the rendered page.
    ///
    /// Only `http` and `https` URLs are loaded; anything else is an [`Error::Browser`] before a
    /// tab is opened. See [`check_navigable`].
    ///
    /// The session lock is only held while the tab is being opened, so several fetches on the
    /// same (cloned) session run concurrently, exactly as Playwright's page pool lets them.
    pub async fn fetch(&self, url: &str) -> Result<Response> {
        check_navigable(url)?;
        let page = {
            let state = self.inner.state.lock().await;
            let running = state
                .as_ref()
                .ok_or_else(|| Error::Browser("session has not been started".into()))?;
            running
                .browser
                .new_page("about:blank")
                .await
                .map_err(|error| Error::Browser(format!("failed to open a page: {error}")))?
        };

        let result = self.fetch_on(&page, url).await;
        // Scrapling closes a tab that errored and recycles the rest; a tab per fetch is the
        // simpler equivalent, so this one always goes.
        let _ = page.close().await;
        result
    }

    /// The body of [`DynamicSession::fetch`], with the page already open so that it can always
    /// be closed afterwards.
    async fn fetch_on(&self, page: &Page, url: &str) -> Result<Response> {
        let options = &self.options;
        let mut tasks = TaskGuard::default();

        if let Some(useragent) = &options.useragent {
            page.set_user_agent(useragent.as_str())
                .await
                .map_err(|error| {
                    Error::Browser(format!("failed to set the user agent: {error}"))
                })?;
        }

        if !options.extra_headers.is_empty() {
            let mut object = serde_json::Map::new();
            for (name, value) in options.extra_headers.iter() {
                object.insert(
                    name.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
            page.execute(SetExtraHttpHeadersParams::new(Headers::new(
                serde_json::Value::Object(object),
            )))
            .await
            .map_err(|error| Error::Browser(format!("failed to set extra headers: {error}")))?;
        }

        let proxy = match &options.proxy {
            Some(proxy) => Some(ProxyConfig::parse(proxy)?),
            None => None,
        };
        if let Some(proxy) = &proxy {
            if let Some(credentials) = proxy.credentials() {
                page.authenticate(credentials).await.map_err(|error| {
                    Error::Browser(format!("failed to set proxy credentials: {error}"))
                })?;
            }
        }

        // Interception: abort blocked resource types and blocked hosts, forward the rest.
        if self.filter.is_active() {
            let mut paused =
                page.event_listener::<EventRequestPaused>()
                    .await
                    .map_err(|error| {
                        Error::Browser(format!("failed to listen for paused requests: {error}"))
                    })?;
            let filter = Arc::clone(&self.filter);
            let intercept_page = page.clone();
            tasks.push(tokio::spawn(async move {
                while let Some(event) = paused.next().await {
                    // Responses paused at the response stage are always forwarded; only the
                    // request stage decides whether a request happens at all.
                    let blocked = event.response_status_code.is_none() && {
                        let resource_type: &str = event.resource_type.as_ref();
                        filter.should_block(&event.request.url, resource_type)
                    };
                    if blocked {
                        tracing::debug!(url = %event.request.url, "blocking background request");
                        // Playwright's `route.abort()` defaults to the `failed` error code,
                        // which is what Scrapling's handler produces; keep the same one so a
                        // blocked request looks identical to the page.
                        let _ = intercept_page
                            .execute(FailRequestParams::new(
                                event.request_id.clone(),
                                ErrorReason::Failed,
                            ))
                            .await;
                    } else {
                        let _ = intercept_page
                            .execute(ContinueRequestParams::new(event.request_id.clone()))
                            .await;
                    }
                }
            }));
        }

        // The main document's response, which supplies the status, reason and headers.
        let main_frame = page.mainframe().await.ok().flatten();
        let document: Arc<Mutex<Option<MainDocument>>> = Arc::new(Mutex::new(None));
        {
            let mut responses = page
                .event_listener::<EventResponseReceived>()
                .await
                .map_err(|error| {
                    Error::Browser(format!("failed to listen for responses: {error}"))
                })?;
            let document = Arc::clone(&document);
            tasks.push(tokio::spawn(async move {
                while let Some(event) = responses.next().await {
                    let resource_type: &str = event.r#type.as_ref();
                    let same_frame =
                        main_frame.is_none() || event.frame_id.as_ref() == main_frame.as_ref();
                    if resource_type != "Document" || !same_frame {
                        continue;
                    }
                    if let Ok(mut slot) = document.lock() {
                        *slot = Some(MainDocument::from_event(&event));
                    }
                }
            }));
        }

        let activity = Arc::new(Mutex::new(NetworkActivity::new()));
        if options.network_idle {
            self.spawn_activity_trackers(page, &activity, &mut tasks)
                .await?;
        }

        page.goto(url)
            .await
            .map_err(|error| Error::Browser(format!("failed to navigate to {url}: {error}")))?;
        // `wait_for_navigation` is the CDP equivalent of Playwright's `load` state. It is
        // allowed to time out: Scrapling treats the stability waits as best effort.
        let _ = tokio::time::timeout(options.timeout, page.wait_for_navigation()).await;
        if options.network_idle {
            wait_for_network_idle(&activity, options.timeout).await;
        }

        // Scrapling raises `Failed to get response for {url}` here, before the selector and
        // settle waits, so a navigation that produced nothing fails fast instead of burning the
        // whole timeout first.
        if !wait_for_document(&document, RESPONSE_GRACE).await {
            return Err(Error::Browser(format!("failed to get response for {url}")));
        }

        if let Some(selector) = &options.wait_selector {
            wait_for_selector(page, selector, options.wait_selector_state, options.timeout).await;
            if options.network_idle {
                wait_for_network_idle(&activity, options.timeout).await;
            }
        }

        if !options.wait.is_zero() {
            tokio::time::sleep(options.wait).await;
        }

        let content = page
            .content()
            .await
            .map_err(|error| Error::Browser(format!("failed to read the page content: {error}")))?;
        let final_url = page
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| url.to_string());
        let cookies = page
            .get_cookies()
            .await
            .map(|cookies| {
                cookies
                    .into_iter()
                    .map(|cookie| Cookie {
                        name: cookie.name,
                        value: cookie.value,
                        domain: cookie.domain,
                        path: cookie.path,
                        ..Cookie::default()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        drop(tasks);

        // Re-read rather than reusing the early check: a client-side navigation during the
        // waits above replaces the main document, and Scrapling reports the last one. A
        // poisoned lock is read through rather than propagated — the value behind it is a
        // plain `Option`, so a listener that panicked cannot have left it inconsistent.
        let captured = match document.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let captured =
            captured.ok_or_else(|| Error::Browser(format!("failed to get response for {url}")))?;

        // Scrapling passes `meta={"proxy": proxy}`; the same key the HTTP fetcher writes.
        let mut meta: HashMap<String, serde_json::Value> = HashMap::new();
        meta.insert(
            META_PROXY.to_string(),
            match &proxy {
                Some(proxy) => serde_json::Value::String(proxy.server.clone()),
                None => serde_json::Value::Null,
            },
        );

        // CDP types the status as a signed 64-bit integer, so it is clamped rather than cast.
        let status = captured.status.clamp(0, i64::from(u16::MAX)) as u16;
        // Playwright hands back an empty status text often enough that Scrapling falls back to
        // the IANA phrase; do the same.
        let reason = if captured.status_text.is_empty() {
            StatusText::get(status).to_string()
        } else {
            captured.status_text.clone()
        };

        // The body is always the serialized DOM, which the browser hands over as text, so the
        // encoding is always UTF-8. Scrapling only does this for documents whose content type
        // says `html` and returns the raw bytes with their declared charset otherwise; CDP
        // cannot hand back a body that has already been consumed by the renderer, so the DOM
        // is used for every content type here. Reporting the declared charset instead would be
        // actively wrong, because `Response` re-decodes the body with whatever it is told.
        let response = Response::new(final_url, content.into_bytes(), status)?
            .with_reason(reason)
            .with_headers(captured.headers)
            .with_request_headers(captured.request_headers)
            .with_cookies(cookies)
            .with_encoding("utf-8")
            .with_method("GET")
            .with_meta(meta);
        Ok(response)
    }

    /// Register the three network events the `network_idle` approximation counts.
    async fn spawn_activity_trackers(
        &self,
        page: &Page,
        activity: &Arc<Mutex<NetworkActivity>>,
        tasks: &mut TaskGuard,
    ) -> Result<()> {
        let mut started = page
            .event_listener::<EventRequestWillBeSent>()
            .await
            .map_err(|error| Error::Browser(format!("failed to listen for requests: {error}")))?;
        let started_activity = Arc::clone(activity);
        tasks.push(tokio::spawn(async move {
            while let Some(event) = started.next().await {
                // A redirect re-uses the request id, so counting it again would leave the
                // gauge permanently above zero.
                if event.redirect_response.is_some() {
                    continue;
                }
                if let Ok(mut state) = started_activity.lock() {
                    state.request_started();
                }
            }
        }));

        let mut finished = page
            .event_listener::<EventLoadingFinished>()
            .await
            .map_err(|error| {
                Error::Browser(format!("failed to listen for finished requests: {error}"))
            })?;
        let finished_activity = Arc::clone(activity);
        tasks.push(tokio::spawn(async move {
            while finished.next().await.is_some() {
                if let Ok(mut state) = finished_activity.lock() {
                    state.request_ended();
                }
            }
        }));

        let mut failed = page
            .event_listener::<EventLoadingFailed>()
            .await
            .map_err(|error| {
                Error::Browser(format!("failed to listen for failed requests: {error}"))
            })?;
        let failed_activity = Arc::clone(activity);
        tasks.push(tokio::spawn(async move {
            while failed.next().await.is_some() {
                if let Ok(mut state) = failed_activity.lock() {
                    state.request_ended();
                }
            }
        }));

        Ok(())
    }
}

/// One-shot fetching: launch a browser, load a page, shut it down.
#[derive(Debug, Clone, Copy, Default)]
pub struct DynamicFetcher;

impl DynamicFetcher {
    /// Fetch one URL with a throwaway browser session.
    pub async fn fetch(url: &str, options: DynamicSessionOptions) -> Result<Response> {
        let session = DynamicSession::from(options);
        session.start().await?;
        let result = session.fetch(url).await;
        let closed = session.close().await;
        match result {
            Ok(response) => {
                closed?;
                Ok(response)
            }
            Err(error) => Err(error),
        }
    }
}

/// The Chromium command line these options describe, in the order Chromium receives it.
///
/// `DEFAULT_ARGS` first, then `STEALTH_ARGS` when stealth is on, then the caller's own flags,
/// then `--proxy-server` when a proxy is configured. Exact duplicates are dropped and every
/// flag named in `HARMFUL_ARGS` is removed, wherever it came from.
pub fn launch_args(options: &DynamicSessionOptions) -> Result<Vec<String>> {
    let mut flags: Vec<String> = DEFAULT_ARGS.iter().map(|flag| flag.to_string()).collect();
    if options.stealth {
        flags.extend(STEALTH_ARGS.iter().map(|flag| flag.to_string()));
    }
    flags.extend(options.extra_flags.iter().cloned());
    if let Some(proxy) = &options.proxy {
        let proxy = ProxyConfig::parse(proxy)?;
        flags.push(format!("--proxy-server={}", proxy.server));
    }

    let harmful: Vec<&str> = HARMFUL_ARGS.iter().copied().map(flag_key).collect();
    let mut seen: Vec<String> = Vec::with_capacity(flags.len());
    for flag in flags {
        let flag = flag.trim().to_string();
        if flag.is_empty() || harmful.contains(&flag_key(&flag)) || seen.contains(&flag) {
            continue;
        }
        seen.push(flag);
    }
    Ok(seen)
}

/// The switch name of a flag: `--disable-features=TranslateUI` -> `disable-features`.
fn flag_key(flag: &str) -> &str {
    let flag = flag.trim_start_matches('-');
    match flag.split_once('=') {
        Some((key, _)) => key,
        None => flag,
    }
}

/// A proxy split the way Chromium wants it: a server for the command line, credentials for
/// the DevTools authentication handler. Port of `construct_proxy_dict`.
///
/// One deliberate difference from Python: the user name and password are percent-decoded, so
/// `http://bob:s%40cret@proxy:8080` authenticates as `s@cret`. `urlparse` hands the raw,
/// still-encoded text through, which silently breaks any password containing `@`, `:` or `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    /// `scheme://host[:port]`, with any credentials stripped.
    pub server: String,
    /// The user name, or `""`.
    pub username: String,
    /// The password, or `""`.
    pub password: String,
}

impl ProxyConfig {
    /// Parse a proxy URL, rejecting anything Chromium cannot route through.
    pub fn parse(proxy: &str) -> Result<ProxyConfig> {
        let parsed = url::Url::parse(proxy.trim())
            .map_err(|error| Error::Browser(format!("invalid proxy string: {error}")))?;
        let scheme = parsed.scheme();
        if !matches!(scheme, "http" | "https" | "socks4" | "socks5") {
            return Err(Error::Browser(format!(
                "invalid proxy string: unsupported scheme `{scheme}`"
            )));
        }
        // `socks4`/`socks5` are not "special" schemes, so the URL parser leaves their host as
        // written; Python's `urlparse(...).hostname` always lower-cases, so do the same.
        let host = parsed
            .host_str()
            .ok_or_else(|| Error::Browser("invalid proxy string: no host".to_string()))?
            .to_ascii_lowercase();
        let mut server = format!("{scheme}://{host}");
        if let Some(port) = parsed.port() {
            server.push_str(&format!(":{port}"));
        }
        Ok(ProxyConfig {
            server,
            username: percent_decode(parsed.username()),
            password: parsed.password().map(percent_decode).unwrap_or_default(),
        })
    }

    /// The credentials to answer a proxy authentication challenge with, when there are any.
    fn credentials(&self) -> Option<Credentials> {
        if self.username.is_empty() && self.password.is_empty() {
            return None;
        }
        Some(Credentials {
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

/// What the main document's `Network.responseReceived` told us.
#[derive(Debug, Clone)]
struct MainDocument {
    status: i64,
    status_text: String,
    headers: HeaderMap,
    request_headers: HeaderMap,
}

impl MainDocument {
    fn from_event(event: &EventResponseReceived) -> MainDocument {
        MainDocument {
            status: event.response.status,
            status_text: event.response.status_text.clone(),
            headers: headers_from_cdp(event.response.headers.inner()),
            request_headers: event
                .response
                .request_headers
                .as_ref()
                .map(|headers| headers_from_cdp(headers.inner()))
                .unwrap_or_default(),
        }
    }
}

/// CDP models headers as a free-form JSON object; flatten it into the crate's header map.
///
/// The object comes from the remote server by way of Chromium, which already enforces its own
/// header-count and header-size limits, so this copies whatever survived that rather than
/// imposing a second cap and silently dropping headers the caller asked for.
fn headers_from_cdp(value: &serde_json::Value) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(object) = value.as_object() {
        for (name, value) in object {
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            headers.insert(name.clone(), value);
        }
    }
    headers
}

/// Whether the main document's response has been observed, waiting up to `grace` for it.
///
/// Returns as soon as it is there, so the common case costs one lock and no sleep.
async fn wait_for_document(document: &Arc<Mutex<Option<MainDocument>>>, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        let seen = match document.lock() {
            Ok(slot) => slot.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        };
        if seen {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The in-flight request gauge behind the `network_idle` approximation.
#[derive(Debug)]
struct NetworkActivity {
    in_flight: i64,
    idle_since: Instant,
}

impl NetworkActivity {
    fn new() -> NetworkActivity {
        NetworkActivity {
            in_flight: 0,
            idle_since: Instant::now(),
        }
    }

    fn request_started(&mut self) {
        self.in_flight += 1;
    }

    fn request_ended(&mut self) {
        self.in_flight = (self.in_flight - 1).max(0);
        if self.in_flight == 0 {
            self.idle_since = Instant::now();
        }
    }

    /// How long nothing has been in flight, or `None` while something still is.
    fn idle_for(&self) -> Option<Duration> {
        if self.in_flight > 0 {
            None
        } else {
            Some(self.idle_since.elapsed())
        }
    }
}

/// Wait until nothing has been in flight for 500 ms, or until `timeout` runs out.
async fn wait_for_network_idle(activity: &Arc<Mutex<NetworkActivity>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        // The guard is scoped to this statement so that it is never held across an `await`.
        let idle = match activity.lock() {
            Ok(state) => state
                .idle_for()
                .is_some_and(|idle| idle >= NETWORK_IDLE_WINDOW),
            Err(_) => return,
        };
        if idle {
            return;
        }
        if Instant::now() >= deadline {
            tracing::debug!("network did not go idle within the timeout");
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the page until `selector` reaches `state`. Failures are logged, never raised — the
/// same lenient behaviour Scrapling's `wait_selector` has.
async fn wait_for_selector(page: &Page, selector: &str, state: WaitState, timeout: Duration) {
    // The selector is interpolated into a script, so it goes in as a JSON string literal and
    // never as raw source; `querySelector` then rejects a malformed one inside the page.
    let Ok(literal) = serde_json::to_string(selector) else {
        tracing::error!("could not encode the wait selector");
        return;
    };
    let script = format!(
        "(function() {{ \
           var element = document.querySelector({literal}); \
           if (!element) {{ return 'detached'; }} \
           var rect = element.getBoundingClientRect(); \
           var style = window.getComputedStyle(element); \
           var shown = (rect.width > 0 || rect.height > 0) \
             && style.visibility !== 'hidden' && style.display !== 'none'; \
           return shown ? 'visible' : 'hidden'; \
         }})()"
    );
    // Built as an explicit `EvaluateParams` rather than passed as a string: `Page::evaluate`
    // guesses whether a string is an expression or a function declaration, and an immediately
    // invoked function expression is exactly the shape that guess can get wrong.
    let mut params = EvaluateParams::new(script);
    params.return_by_value = Some(true);

    let deadline = Instant::now() + timeout;
    loop {
        match page.evaluate(params.clone()).await {
            Ok(result) => match result.into_value::<String>() {
                Ok(observed) if state.is_satisfied_by(&observed) => return,
                Ok(_) => {}
                Err(error) => tracing::debug!("could not read the selector state: {error}"),
            },
            Err(error) => tracing::debug!("could not evaluate the selector state: {error}"),
        }
        if Instant::now() >= deadline {
            tracing::error!(
                "error waiting for selector {selector}: it never became {}",
                state.as_str()
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Background listeners that must not outlive the fetch that started them.
#[derive(Debug, Default)]
struct TaskGuard(Vec<tokio::task::JoinHandle<()>>);

impl TaskGuard {
    fn push(&mut self, handle: tokio::task::JoinHandle<()>) {
        self.0.push(handle);
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
}

/// Refuse a URL the browser must not be pointed at.
///
/// The HTTP fetcher rejects non-`http(s)` URLs before it sends anything, and a browser needs the
/// same guard for a stronger reason: `page.goto("file:///etc/passwd")` renders the local file and
/// hands its contents back as a [`Response`] body, and `view-source:`/`chrome://`/`devtools://`
/// reach privileged pages. URLs reach here straight from scraped markup when a spider fetches
/// through a browser session, so the scheme is checked rather than trusted.
fn check_navigable(url: &str) -> Result<()> {
    let parsed = ::url::Url::parse(url)
        .map_err(|error| Error::Browser(format!("invalid url `{url}`: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::Browser(format!(
            "unsupported url scheme `{}` in `{url}`; a browser session only loads http and https",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(Error::Browser(format!("the url `{url}` has no host")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_are_navigable() {
        assert!(check_navigable("http://example.com/").is_ok());
        assert!(check_navigable("https://example.com/page?q=1").is_ok());

        for refused in [
            "file:///etc/passwd",
            "FILE:///etc/passwd",
            "view-source:http://example.com/",
            "chrome://settings/",
            "devtools://devtools/bundled/inspector.html",
            "data:text/html,<h1>x</h1>",
            "about:blank",
            "javascript:alert(1)",
            "ftp://example.com/x",
            "not a url",
            "http://",
        ] {
            assert!(
                check_navigable(refused).is_err(),
                "`{refused}` must not be navigable"
            );
        }
    }

    fn options() -> DynamicSessionOptions {
        DynamicSessionOptions::default()
    }

    #[test]
    fn defaults_match_the_contract() {
        let defaults = options();
        assert!(defaults.headless);
        assert!(defaults.stealth);
        assert!(!defaults.disable_resources);
        assert!(!defaults.block_ads);
        assert!(!defaults.network_idle);
        assert_eq!(defaults.timeout, Duration::from_secs(30));
        assert_eq!(defaults.wait, Duration::ZERO);
        assert_eq!(defaults.wait_selector_state, WaitState::Attached);
        assert!(defaults.wait_selector.is_none());
        assert!(defaults.useragent.is_none());
        assert!(defaults.proxy.is_none());
    }

    #[test]
    fn the_builder_records_every_option() {
        let built = DynamicSession::builder()
            .headless(false)
            .disable_resources(true)
            .blocked_domains(&["doubleclick.net"])
            .block_ads(true)
            .timeout(Duration::from_secs(5))
            .wait(Duration::from_millis(250))
            .wait_selector("main .product")
            .wait_selector_state(WaitState::Visible)
            .network_idle(true)
            .useragent("Mozilla/5.0")
            .proxy("http://127.0.0.1:8080")
            .extra_flags(&["--lang=de-DE"])
            .stealth(false)
            .options();

        assert!(!built.headless);
        assert!(built.disable_resources);
        assert_eq!(built.blocked_domains, vec!["doubleclick.net".to_string()]);
        assert!(built.block_ads);
        assert_eq!(built.timeout, Duration::from_secs(5));
        assert_eq!(built.wait, Duration::from_millis(250));
        assert_eq!(built.wait_selector.as_deref(), Some("main .product"));
        assert_eq!(built.wait_selector_state, WaitState::Visible);
        assert!(built.network_idle);
        assert_eq!(built.useragent.as_deref(), Some("Mozilla/5.0"));
        assert_eq!(built.proxy.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(built.extra_flags, vec!["--lang=de-DE".to_string()]);
        assert!(!built.stealth);
    }

    #[test]
    fn stealth_adds_the_stealth_flags_after_the_default_ones() {
        let mut with_stealth = options();
        with_stealth.stealth = true;
        let args = launch_args(&with_stealth).expect("flags");
        for flag in DEFAULT_ARGS {
            assert!(args.contains(&flag.to_string()), "{flag} missing");
        }
        for flag in STEALTH_ARGS {
            assert!(args.contains(&flag.to_string()), "{flag} missing");
        }
        let first_stealth = args
            .iter()
            .position(|flag| flag == "--test-type")
            .expect("stealth flag");
        let last_default = args
            .iter()
            .position(|flag| flag == "--disable-search-engine-choice-screen")
            .expect("default flag");
        assert!(last_default < first_stealth);
    }

    #[test]
    fn without_stealth_only_the_default_flags_are_passed() {
        let mut plain = options();
        plain.stealth = false;
        let args = launch_args(&plain).expect("flags");
        assert_eq!(args.len(), DEFAULT_ARGS.len());
        for flag in STEALTH_ARGS {
            assert!(!args.contains(&flag.to_string()), "{flag} leaked in");
        }
    }

    #[test]
    fn harmful_flags_are_never_passed() {
        let mut sneaky = options();
        sneaky.extra_flags = HARMFUL_ARGS.iter().map(|flag| flag.to_string()).collect();
        sneaky
            .extra_flags
            .push("--disable-extensions=1".to_string());
        let args = launch_args(&sneaky).expect("flags");
        for flag in HARMFUL_ARGS {
            assert!(!args.contains(&flag.to_string()), "{flag} leaked in");
        }
        assert!(!args.contains(&"--disable-extensions=1".to_string()));
    }

    #[test]
    fn extra_flags_come_last_and_duplicates_are_dropped() {
        let mut extra = options();
        extra.stealth = false;
        extra.extra_flags = vec![
            "--lang=de-DE".to_string(),
            "--no-pings".to_string(),
            "--lang=de-DE".to_string(),
        ];
        let args = launch_args(&extra).expect("flags");
        assert_eq!(args.last().map(String::as_str), Some("--lang=de-DE"));
        assert_eq!(
            args.iter().filter(|flag| *flag == "--no-pings").count(),
            1,
            "duplicate flags survived"
        );
        assert_eq!(
            args.iter().filter(|flag| *flag == "--lang=de-DE").count(),
            1
        );
    }

    #[test]
    fn a_proxy_becomes_a_proxy_server_flag() {
        let mut proxied = options();
        proxied.stealth = false;
        proxied.proxy = Some("http://user:pass@127.0.0.1:8080".to_string());
        let args = launch_args(&proxied).expect("flags");
        assert_eq!(
            args.last().map(String::as_str),
            Some("--proxy-server=http://127.0.0.1:8080")
        );
    }

    #[test]
    fn an_invalid_proxy_is_an_error_not_a_panic() {
        let mut broken = options();
        broken.proxy = Some("ftp://example.com".to_string());
        assert!(launch_args(&broken).is_err());
        broken.proxy = Some("nonsense".to_string());
        assert!(launch_args(&broken).is_err());
    }

    #[test]
    fn proxies_are_split_into_server_and_credentials() {
        let parsed = ProxyConfig::parse("socks5://bob:s%40cret@proxy.example.com:1080")
            .expect("valid proxy");
        assert_eq!(parsed.server, "socks5://proxy.example.com:1080");
        assert_eq!(parsed.username, "bob");
        assert_eq!(parsed.password, "s@cret");
        assert!(parsed.credentials().is_some());

        let bare = ProxyConfig::parse("http://proxy.example.com").expect("valid proxy");
        assert_eq!(bare.server, "http://proxy.example.com");
        assert!(bare.credentials().is_none());

        // A non-special scheme keeps the host as written unless it is lower-cased explicitly.
        let shouty = ProxyConfig::parse("socks5://Proxy.Example.COM:1080").expect("valid proxy");
        assert_eq!(shouty.server, "socks5://proxy.example.com:1080");

        // A password-only proxy still authenticates.
        let secret = ProxyConfig::parse("http://:hunter2@proxy.example.com").expect("valid proxy");
        assert_eq!(secret.username, "");
        assert_eq!(secret.password, "hunter2");
        assert!(secret.credentials().is_some());
    }

    #[test]
    fn block_ads_widens_the_filter() {
        let mut ads = options();
        ads.block_ads = true;
        let filter = ads.request_filter();
        assert!(filter.is_active());
        assert!(filter.should_block("https://ads.doubleclick.net/x.js", "Script"));
        assert!(!filter.should_block("https://example.com/x.js", "Script"));
    }

    #[test]
    fn wait_states_read_element_reports_the_way_playwright_does() {
        assert!(WaitState::Attached.is_satisfied_by("visible"));
        assert!(WaitState::Attached.is_satisfied_by("hidden"));
        assert!(!WaitState::Attached.is_satisfied_by("detached"));

        assert!(WaitState::Detached.is_satisfied_by("detached"));
        assert!(!WaitState::Detached.is_satisfied_by("hidden"));

        assert!(WaitState::Visible.is_satisfied_by("visible"));
        assert!(!WaitState::Visible.is_satisfied_by("hidden"));

        assert!(WaitState::Hidden.is_satisfied_by("hidden"));
        assert!(WaitState::Hidden.is_satisfied_by("detached"));
        assert!(!WaitState::Hidden.is_satisfied_by("visible"));

        assert_eq!(WaitState::Attached.as_str(), "attached");
    }

    #[test]
    fn a_session_can_cross_task_boundaries() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<DynamicSession>();
        assert_send_sync::<DynamicSessionOptions>();
        assert_send_sync::<DynamicSessionBuilder>();
        assert_send_sync::<RequestFilter>();
    }

    #[tokio::test]
    async fn waiting_for_a_document_that_never_arrives_gives_up() {
        let slot: Arc<Mutex<Option<MainDocument>>> = Arc::new(Mutex::new(None));
        assert!(!wait_for_document(&slot, Duration::from_millis(50)).await);

        *slot.lock().expect("lock") = Some(MainDocument {
            status: 200,
            status_text: String::new(),
            headers: HeaderMap::new(),
            request_headers: HeaderMap::new(),
        });
        assert!(wait_for_document(&slot, Duration::ZERO).await);
    }

    #[test]
    fn cdp_headers_become_a_header_map() {
        let value = serde_json::json!({
            "content-type": "text/html; charset=utf-8",
            "content-length": 12,
        });
        let headers = headers_from_cdp(&value);
        assert_eq!(
            headers.get("Content-Type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(headers.get("content-length"), Some("12"));
        assert!(headers_from_cdp(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn the_activity_gauge_never_goes_negative() {
        let mut activity = NetworkActivity::new();
        activity.request_ended();
        assert!(activity.idle_for().is_some());
        activity.request_started();
        activity.request_started();
        assert!(activity.idle_for().is_none());
        activity.request_ended();
        assert!(activity.idle_for().is_none());
        activity.request_ended();
        assert!(activity.idle_for().is_some());
    }

    #[test]
    fn flag_keys_ignore_the_dashes_and_the_value() {
        assert_eq!(
            flag_key("--disable-features=TranslateUI"),
            "disable-features"
        );
        assert_eq!(flag_key("--no-pings"), "no-pings");
        assert_eq!(flag_key("no-pings"), "no-pings");
    }

    #[tokio::test]
    async fn fetching_before_start_is_an_error() {
        let session = DynamicSession::new();
        assert!(!session.is_alive());
        let error = session.fetch("https://example.com").await;
        assert!(error.is_err());
        // Closing a session that never started is a no-op, not a failure.
        assert!(session.close().await.is_ok());
    }

    /// Needs a real Chromium on the machine; run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires a local Chromium installation and network access"]
    async fn fetches_a_live_page() {
        let options = DynamicSession::builder()
            .headless(true)
            .disable_resources(true)
            .timeout(Duration::from_secs(60))
            .options();
        let response = DynamicFetcher::fetch("https://example.com/", options)
            .await
            .expect("the page should load");
        assert_eq!(response.status, 200);
        assert!(!response.body.is_empty());
    }
}
