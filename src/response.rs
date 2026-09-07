//! The unified fetch result shared by every fetcher.
//!
//! Port of `scrapling/engines/toolbelt/custom.py` (`Response`, `StatusText`).
//!
//! [`Response`] is deliberately feature-independent: the HTTP fetcher, the browser fetcher and
//! the crawl framework all hand back the same type. It derefs to [`Selector`], so every parsing
//! helper (`css`, `find_all`, `re`, ...) is available directly on a response, exactly like the
//! Python subclass.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use crate::error::{Error, Result};
use crate::parser::Selector;

/// Meta key under which the crawl engine stores the session id a response was fetched with.
///
/// [`Response::follow`] reads it so a followed request keeps using the same session.
pub const META_SID: &str = "scrapling.sid";

/// Meta key under which the crawl engine stores the [`crate::spider::Callback`] of the request
/// that produced this response, serialized with `serde`.
pub const META_CALLBACK: &str = "scrapling.callback";

/// Meta key under which the crawl engine stores the scheduling priority of the request that
/// produced this response.
pub const META_PRIORITY: &str = "scrapling.priority";

/// Meta key under which the HTTP fetcher records the proxy a request went through.
pub const META_PROXY: &str = "proxy";

// ---------------------------------------------------------------------------------------------
// HeaderMap
// ---------------------------------------------------------------------------------------------

/// A case-insensitive, insertion-ordered header map.
///
/// Lookups ignore case; iteration yields the names with the casing they were inserted with, in
/// insertion order, which is what Python's `dict(response.headers)` effectively gives you.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderMap(Vec<(String, String)>);

impl HeaderMap {
    /// An empty map.
    pub fn new() -> Self {
        HeaderMap(Vec::new())
    }

    /// Build from name/value pairs, later duplicates replacing earlier ones.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name, value);
        }
        map
    }

    /// Look a header up, ignoring case.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Insert or replace a header, ignoring case. Replacing keeps the original position.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        match self
            .0
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }

    /// Whether a header is present, ignoring case.
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Remove a header, ignoring case, returning the value it had.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        let index = self
            .0
            .iter()
            .position(|(key, _)| key.eq_ignore_ascii_case(name))?;
        Some(self.0.remove(index).1)
    }

    /// Iterate in insertion order, with the original casing.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Number of headers.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Merge `other` into this map; values in `other` win.
    pub(crate) fn merge(&mut self, other: &HeaderMap) {
        for (name, value) in other.iter() {
            self.insert(name, value);
        }
    }
}

impl FromIterator<(String, String)> for HeaderMap {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        HeaderMap::from_pairs(iter)
    }
}

impl serde::Serialize for HeaderMap {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for HeaderMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct HeaderMapVisitor;

        impl<'de> serde::de::Visitor<'de> for HeaderMapVisitor {
            type Value = HeaderMap;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of header names to values")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<HeaderMap, A::Error> {
                let mut map = HeaderMap::new();
                while let Some((name, value)) = access.next_entry::<String, String>()? {
                    map.insert(name, value);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(HeaderMapVisitor)
    }
}

// ---------------------------------------------------------------------------------------------
// Cookie
// ---------------------------------------------------------------------------------------------

/// One cookie from a response.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Cookie {
    /// The cookie name.
    pub name: String,
    /// The cookie value.
    pub value: String,
    /// The domain it applies to.
    pub domain: String,
    /// The path it applies to.
    pub path: String,
}

impl Cookie {
    /// A cookie with just a name and a value; domain and path stay empty.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: String::new(),
            path: String::new(),
        }
    }

    /// Parse one `Set-Cookie` header value.
    ///
    /// `default_domain` and `default_path` are used when the header carries no `Domain=` or
    /// `Path=` attribute, mirroring what a real cookie jar would store. Returns `None` when the
    /// value has no `name=value` pair at all.
    pub fn parse_set_cookie(
        value: &str,
        default_domain: &str,
        default_path: &str,
    ) -> Option<Cookie> {
        let mut parts = value.split(';');
        let (name, val) = parts.next()?.split_once('=')?;
        let mut cookie = Cookie {
            name: name.trim().to_string(),
            value: val.trim().to_string(),
            domain: default_domain.to_string(),
            path: default_path.to_string(),
        };
        if cookie.name.is_empty() {
            return None;
        }
        for attribute in parts {
            let (key, val) = match attribute.split_once('=') {
                Some(pair) => pair,
                None => continue,
            };
            let key = key.trim();
            let val = val.trim();
            if key.eq_ignore_ascii_case("domain") {
                cookie.domain = val.trim_start_matches('.').to_string();
            } else if key.eq_ignore_ascii_case("path") {
                cookie.path = val.to_string();
            }
        }
        Some(cookie)
    }
}

// ---------------------------------------------------------------------------------------------
// StatusText
// ---------------------------------------------------------------------------------------------

/// The IANA reason phrases, as Python's `StatusText`.
///
/// Reference: <https://developer.mozilla.org/en-US/docs/Web/HTTP/Status>
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusText;

impl StatusText {
    /// The phrase for a status code, or `"Unknown Status Code"`.
    pub fn get(status: u16) -> &'static str {
        match status {
            100 => "Continue",
            101 => "Switching Protocols",
            102 => "Processing",
            103 => "Early Hints",
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            203 => "Non-Authoritative Information",
            204 => "No Content",
            205 => "Reset Content",
            206 => "Partial Content",
            207 => "Multi-Status",
            208 => "Already Reported",
            226 => "IM Used",
            300 => "Multiple Choices",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            305 => "Use Proxy",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",
            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            406 => "Not Acceptable",
            407 => "Proxy Authentication Required",
            408 => "Request Timeout",
            409 => "Conflict",
            410 => "Gone",
            411 => "Length Required",
            412 => "Precondition Failed",
            413 => "Payload Too Large",
            414 => "URI Too Long",
            415 => "Unsupported Media Type",
            416 => "Range Not Satisfiable",
            417 => "Expectation Failed",
            418 => "I'm a teapot",
            421 => "Misdirected Request",
            422 => "Unprocessable Entity",
            423 => "Locked",
            424 => "Failed Dependency",
            425 => "Too Early",
            426 => "Upgrade Required",
            428 => "Precondition Required",
            429 => "Too Many Requests",
            431 => "Request Header Fields Too Large",
            451 => "Unavailable For Legal Reasons",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            505 => "HTTP Version Not Supported",
            506 => "Variant Also Negotiates",
            507 => "Insufficient Storage",
            508 => "Loop Detected",
            510 => "Not Extended",
            511 => "Network Authentication Required",
            _ => "Unknown Status Code",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------------------------

/// The longest encoding label taken out of a `Content-Type`; the real ones are far shorter, and
/// the header is remote input.
const MAX_CHARSET_LABEL: usize = 64;

/// Pull the `charset` out of a `Content-Type` header value, e.g.
/// `text/html; charset=utf-8` -> `utf-8`. Falls back to `"utf-8"`.
///
/// Port of `ResponseFactory.__extract_browser_encoding` and its `charset=["']?([\w-]+)` regex,
/// except that the match is case-insensitive here (`Charset=UTF-8` is legal HTTP and Python's
/// regex misses it).
pub fn encoding_from_content_type(content_type: Option<&str>) -> String {
    let fallback = || "utf-8".to_string();
    let content_type = match content_type {
        Some(value) => value,
        None => return fallback(),
    };
    // `to_ascii_lowercase` maps ASCII only, so byte indexes into it are byte indexes into the
    // original, and the byte just past `charset=` is always a character boundary.
    let lowered = content_type.to_ascii_lowercase();
    let start = match lowered.find("charset=") {
        Some(index) => index + "charset=".len(),
        None => return fallback(),
    };
    // `get` rather than `[..]`: a slice index is never worth a panic on a remote header.
    let rest = match content_type.get(start..) {
        Some(rest) => rest.trim_start(),
        None => return fallback(),
    };
    let rest = rest.trim_start_matches(['"', '\'']);
    let charset: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(MAX_CHARSET_LABEL)
        .collect();
    if charset.is_empty() {
        fallback()
    } else {
        charset
    }
}

/// Decode `body` with `label`, falling back to lossy UTF-8 when the label is unknown.
fn decode_body<'a>(body: &'a [u8], label: &str) -> Cow<'a, str> {
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) => encoding.decode(body).0,
        None => String::from_utf8_lossy(body),
    }
}

/// Parse `html`, remembering `url` when there is one.
fn parse(html: &str, url: &str) -> Result<Selector> {
    if url.is_empty() {
        Selector::new(html)
    } else {
        Selector::with_url(html, url)
    }
}

// ---------------------------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------------------------

/// A fetched page: a parsed [`Selector`] plus everything the transport knew about it.
///
/// `Response` derefs to `Selector`, so `response.css(...)`, `response.find_all(...)` and friends
/// work directly, exactly like the Python subclass.
///
/// The parsed document is built when the response is created and rebuilt by
/// [`Response::with_encoding`]. Assigning to the public `body` or `encoding` fields afterwards
/// does *not* re-parse; use the builder methods if you need the document to follow.
#[derive(Debug, Clone)]
pub struct Response {
    /// The final URL, after redirects.
    pub url: String,
    /// The HTTP status code.
    pub status: u16,
    /// The status reason phrase as the server sent it.
    pub reason: String,
    /// The response headers.
    pub headers: HeaderMap,
    /// The headers that were sent with the request.
    pub request_headers: HeaderMap,
    /// The cookies the response set.
    pub cookies: Vec<Cookie>,
    /// The raw response body.
    pub body: Vec<u8>,
    /// The encoding the body was decoded with.
    pub encoding: String,
    /// The HTTP method used.
    pub method: String,
    /// The redirect chain that led here, oldest first.
    pub history: Vec<Response>,
    /// Free-form metadata (the proxy used, the spider's request meta, ...).
    pub meta: HashMap<String, serde_json::Value>,
    /// The parsed document.
    selector: Selector,
}

impl Response {
    /// Build a response from a body and its transport metadata.
    ///
    /// The body is decoded as UTF-8 and parsed straight away; call [`Response::with_encoding`]
    /// when the transport reported a different charset.
    pub fn new(url: impl Into<String>, body: Vec<u8>, status: u16) -> Result<Response> {
        let url = url.into();
        let encoding = "utf-8".to_string();
        let selector = {
            let text = decode_body(&body, &encoding);
            parse(&text, &url)?
        };
        Ok(Response {
            url,
            status,
            reason: StatusText::get(status).to_string(),
            headers: HeaderMap::new(),
            request_headers: HeaderMap::new(),
            cookies: Vec::new(),
            body,
            encoding,
            method: "GET".to_string(),
            history: Vec::new(),
            meta: HashMap::new(),
            selector,
        })
    }

    /// Set the reason phrase.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    /// Set the response headers.
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Set the headers that were sent with the request.
    pub fn with_request_headers(mut self, headers: HeaderMap) -> Self {
        self.request_headers = headers;
        self
    }

    /// Set the cookies the response carried.
    pub fn with_cookies(mut self, cookies: Vec<Cookie>) -> Self {
        self.cookies = cookies;
        self
    }

    /// Set the encoding and re-parse the body with it.
    ///
    /// An unknown label keeps the lossy UTF-8 decoding; a body that cannot be re-parsed keeps
    /// the document that was already there.
    pub fn with_encoding(mut self, encoding: impl Into<String>) -> Self {
        self.encoding = encoding.into();
        let reparsed = {
            let text = decode_body(&self.body, &self.encoding);
            parse(&text, &self.url)
        };
        if let Ok(selector) = reparsed {
            self.selector = selector;
        }
        self
    }

    /// Set the HTTP method the request used.
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Set the redirect chain that led here.
    pub fn with_history(mut self, history: Vec<Response>) -> Self {
        self.history = history;
        self
    }

    /// Set the free-form metadata.
    pub fn with_meta(mut self, meta: HashMap<String, serde_json::Value>) -> Self {
        self.meta = meta;
        self
    }

    /// The parsed document.
    pub fn selector(&self) -> &Selector {
        &self.selector
    }

    /// The body decoded with [`Response::encoding`].
    pub fn text_body(&self) -> Cow<'_, str> {
        decode_body(&self.body, &self.encoding)
    }

    /// The reason phrase for a status code.
    pub fn status_text(status: u16) -> &'static str {
        StatusText::get(status)
    }

    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Resolve `url` against this response's URL.
    ///
    /// Only [`Response::follow`] and the tests need this, so it is dead code in a build with
    /// neither the `spider` feature nor `cfg(test)`.
    #[cfg_attr(not(any(feature = "spider", test)), allow(dead_code))]
    fn resolve(&self, url: &str) -> Result<String> {
        if !self.url.is_empty() {
            if let Ok(base) = url::Url::parse(&self.url) {
                return base
                    .join(url)
                    .map(|joined| joined.to_string())
                    .map_err(|e| {
                        Error::other(format!("cannot join `{url}` onto `{}`: {e}", self.url))
                    });
            }
        }
        url::Url::parse(url)
            .map(|parsed| parsed.to_string())
            .map_err(|e| Error::other(format!("invalid url `{url}`: {e}")))
    }

    /// Build a crawl [`Request`](crate::spider::Request) for a URL found on this page.
    ///
    /// Anything left at its default in `opts` is inherited from this response: the session id,
    /// the callback and the priority come from the [`META_SID`], [`META_CALLBACK`] and
    /// [`META_PRIORITY`] meta entries the crawl engine stores, and the new request's meta is
    /// this response's meta with `opts.meta` merged over it. With `referer_flow` on (the
    /// default) the new request gets a `referer` header pointing at this page.
    #[cfg(feature = "spider")]
    pub fn follow(&self, url: &str, opts: FollowOptions) -> Result<crate::spider::Request> {
        let target = self.resolve(url)?;

        let sid = match opts.sid {
            Some(sid) => sid,
            None => self
                .meta
                .get(META_SID)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        let callback = match opts.callback {
            Some(callback) => callback,
            None => self
                .meta
                .get(META_CALLBACK)
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default(),
        };
        let priority = match opts.priority {
            Some(priority) => priority,
            None => self
                .meta
                .get(META_PRIORITY)
                .and_then(|value| value.as_i64())
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0),
        };

        let mut options = crate::spider::RequestOptions::default();
        if opts.referer_flow {
            options
                .headers
                .insert("referer".to_string(), self.url.clone());
        }

        let mut request = crate::spider::Request::new(target)
            .sid(sid)
            .callback(callback)
            .priority(priority)
            .dont_filter(opts.dont_filter)
            .options(options);

        let mut meta = self.meta.clone();
        for (key, value) in opts.meta {
            meta.insert(key, value);
        }
        for (key, value) in meta {
            request = request.meta(key, value);
        }

        Ok(request)
    }
}

impl std::ops::Deref for Response {
    type Target = Selector;

    fn deref(&self) -> &Selector {
        &self.selector
    }
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} {}>", self.status, self.url)
    }
}

/// What [`Response::follow`] may override; anything left at its default is inherited.
#[cfg(feature = "spider")]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FollowOptions {
    /// The session to use; `None` means "same as this response".
    pub sid: Option<String>,
    /// The callback to dispatch the new response to.
    pub callback: Option<crate::spider::Callback>,
    /// The scheduling priority.
    pub priority: Option<i32>,
    /// Skip the duplicate filter for this request.
    pub dont_filter: bool,
    /// Extra meta merged over this response's meta.
    pub meta: HashMap<String, serde_json::Value>,
    /// Set `referer` to this response's URL. Default `true`.
    pub referer_flow: bool,
}

#[cfg(feature = "spider")]
impl Default for FollowOptions {
    fn default() -> Self {
        FollowOptions {
            sid: None,
            callback: None,
            priority: None,
            dont_filter: false,
            meta: HashMap::new(),
            referer_flow: true,
        }
    }
}

#[cfg(feature = "spider")]
impl FollowOptions {
    /// Options with everything inherited from the response.
    pub fn new() -> Self {
        FollowOptions::default()
    }

    /// Use a specific session.
    pub fn sid(mut self, sid: impl Into<String>) -> Self {
        self.sid = Some(sid.into());
        self
    }

    /// Dispatch the response to a specific callback.
    pub fn callback(mut self, callback: crate::spider::Callback) -> Self {
        self.callback = Some(callback);
        self
    }

    /// Schedule with a specific priority.
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Skip the duplicate filter.
    pub fn dont_filter(mut self, yes: bool) -> Self {
        self.dont_filter = yes;
        self
    }

    /// Merge one extra meta entry over the response's meta.
    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Whether to set `referer` to this response's URL.
    pub fn referer_flow(mut self, yes: bool) -> Self {
        self.referer_flow = yes;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_table() {
        assert_eq!(StatusText::get(200), "OK");
        assert_eq!(StatusText::get(418), "I'm a teapot");
        assert_eq!(StatusText::get(511), "Network Authentication Required");
        assert_eq!(StatusText::get(999), "Unknown Status Code");
        assert_eq!(Response::status_text(404), "Not Found");
    }

    #[test]
    fn header_map_is_case_insensitive_and_ordered() {
        let mut headers = HeaderMap::new();
        headers.insert("Content-Type", "text/html");
        headers.insert("X-Test", "1");
        assert_eq!(headers.get("content-type"), Some("text/html"));
        assert!(headers.contains_key("CONTENT-TYPE"));
        headers.insert("CONTENT-TYPE", "application/json");
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers.iter().collect::<Vec<_>>(),
            vec![("Content-Type", "application/json"), ("X-Test", "1")]
        );
        assert_eq!(headers.remove("x-test"), Some("1".to_string()));
        assert!(!headers.contains_key("x-test"));
    }

    #[test]
    fn header_map_round_trips_through_serde() {
        let headers = HeaderMap::from_pairs([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]);
        let json = serde_json::to_string(&headers).expect("serialize");
        let back: HeaderMap = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.get("a"), Some("1"));
        assert_eq!(back.get("b"), Some("2"));
    }

    #[test]
    fn set_cookie_parsing() {
        let cookie = Cookie::parse_set_cookie(
            "sid=abc123; Path=/app; Domain=.example.com; HttpOnly",
            "fallback.test",
            "/",
        )
        .expect("cookie");
        assert_eq!(cookie.name, "sid");
        assert_eq!(cookie.value, "abc123");
        assert_eq!(cookie.domain, "example.com");
        assert_eq!(cookie.path, "/app");

        let bare = Cookie::parse_set_cookie("a=b", "example.test", "/").expect("cookie");
        assert_eq!(bare.domain, "example.test");
        assert_eq!(bare.path, "/");

        assert!(Cookie::parse_set_cookie("nonsense", "d", "/").is_none());
    }

    #[test]
    fn charset_extraction() {
        assert_eq!(
            encoding_from_content_type(Some("text/html; charset=utf-8")),
            "utf-8"
        );
        assert_eq!(
            encoding_from_content_type(Some("text/html;charset=\"ISO-8859-1\"")),
            "ISO-8859-1"
        );
        assert_eq!(encoding_from_content_type(Some("text/html")), "utf-8");
        assert_eq!(encoding_from_content_type(None), "utf-8");
    }

    #[test]
    fn response_basics() {
        let response = Response::new(
            "https://example.com/page",
            b"<html><body><h1>Hi</h1></body></html>".to_vec(),
            200,
        )
        .expect("response")
        .with_method("GET");

        assert!(response.is_success());
        assert_eq!(response.reason, "OK");
        assert_eq!(response.to_string(), "<200 https://example.com/page>");
        assert!(response.text_body().contains("<h1>Hi</h1>"));
    }

    #[test]
    fn response_json_body() {
        let response =
            Response::new("https://example.com", br#"{"a": 1}"#.to_vec(), 200).expect("response");
        let value: serde_json::Value = response.json().expect("json");
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn response_decodes_latin1_bodies() {
        // 0xE9 is `é` in ISO-8859-1 but invalid UTF-8.
        let body = b"<html><body>caf\xe9</body></html>".to_vec();
        let response = Response::new("https://example.com", body, 200)
            .expect("response")
            .with_encoding("iso-8859-1");
        assert!(response.text_body().contains("café"));
    }

    #[test]
    fn response_resolves_relative_urls() {
        let response = Response::new("https://example.com/a/b", b"<html></html>".to_vec(), 200)
            .expect("response");
        assert_eq!(
            response.resolve("../c").expect("resolved"),
            "https://example.com/c"
        );
        assert_eq!(
            response.resolve("https://other.test/x").expect("resolved"),
            "https://other.test/x"
        );
    }
}
