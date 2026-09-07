//! The unit of work a crawl schedules: [`Request`], its transport [`RequestOptions`] and the
//! [`Callback`] that will receive the response.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::url::canonicalize_url;

/// Which spider method handles a response.
///
/// Callbacks are values rather than closures so that a queued request can be written to a
/// checkpoint file and read back on the next run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Callback {
    /// The spider's `parse`.
    #[default]
    Parse,
    /// The spider's `callback(name, response)`, dispatched by name.
    Named(String),
}

impl Callback {
    /// A callback dispatched by name.
    pub fn named(name: impl Into<String>) -> Callback {
        Callback::Named(name.into())
    }

    /// The name this callback dispatches on, or `"parse"`.
    pub fn name(&self) -> &str {
        match self {
            Callback::Parse => "parse",
            Callback::Named(name) => name.as_str(),
        }
    }
}

/// The transport options of a single request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RequestOptions {
    /// HTTP method; empty means GET.
    #[serde(default)]
    pub method: String,
    /// Extra headers for this request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// A URL-encoded form body.
    #[serde(default)]
    pub form: Option<BTreeMap<String, String>>,
    /// A JSON body.
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    /// A raw body.
    #[serde(default)]
    pub body: Option<Vec<u8>>,
    /// A proxy URL just for this request.
    #[serde(default)]
    pub proxy: Option<String>,
    /// A timeout just for this request.
    #[serde(default)]
    pub timeout: Option<Duration>,
}

impl RequestOptions {
    /// Options with every field at its default.
    pub fn new() -> RequestOptions {
        RequestOptions::default()
    }

    /// Set the HTTP method.
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Add one header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Send a URL-encoded form body.
    pub fn form<I, K, V>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.form = Some(
            fields
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        );
        self
    }

    /// Send a JSON body.
    pub fn json(mut self, value: serde_json::Value) -> Self {
        self.json = Some(value);
        self
    }

    /// Send a raw body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Route this request through a proxy.
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// Give this request its own timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The method to send, defaulting to `GET`.
    pub fn method_or_get(&self) -> &str {
        if self.method.is_empty() {
            "GET"
        } else {
            self.method.as_str()
        }
    }

    /// The request body as bytes, the way Python builds it for the fingerprint.
    ///
    /// Python looks at its `data` keyword first — a mapping is URL-encoded, a string or a
    /// buffer is used as-is — and only falls back to `json` when there is no `data` at all.
    /// [`RequestOptions::form`] and [`RequestOptions::body`] are the two shapes of that `data`,
    /// so they are checked first and in that order; an empty form or an empty raw body counts
    /// as "no data", exactly like Python's falsy check.
    fn body_bytes(&self) -> Vec<u8> {
        if let Some(form) = &self.form {
            if !form.is_empty() {
                let mut out = String::new();
                for (index, (key, value)) in form.iter().enumerate() {
                    if index > 0 {
                        out.push('&');
                    }
                    out.push_str(&urlencode_component(key));
                    out.push('=');
                    out.push_str(&urlencode_component(value));
                }
                return out.into_bytes();
            }
        }
        if let Some(body) = &self.body {
            if !body.is_empty() {
                return body.clone();
            }
        }
        if let Some(json) = &self.json {
            if !json.is_null() {
                return serde_json::to_vec(json).unwrap_or_default();
            }
        }
        Vec::new()
    }
}

/// `urllib.parse.quote_plus` for one form field.
fn urlencode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// One scheduled fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// The URL to fetch.
    pub url: String,
    /// Which registered session to fetch it with; empty means the default session.
    #[serde(default)]
    pub sid: String,
    /// Which spider method handles the response.
    #[serde(default)]
    pub callback: Callback,
    /// Higher runs first. Default 0.
    #[serde(default)]
    pub priority: i32,
    /// Skip the duplicate filter.
    #[serde(default)]
    pub dont_filter: bool,
    /// Free-form data carried to the callback through `Response::meta`.
    #[serde(default)]
    pub meta: HashMap<String, serde_json::Value>,
    /// How many times this request has already been retried after a block.
    #[serde(default)]
    pub retry_count: u32,
    /// Transport options for this request.
    #[serde(default)]
    pub options: RequestOptions,
}

impl Request {
    /// A GET request for `url` handled by the spider's `parse`.
    pub fn new(url: impl Into<String>) -> Request {
        Request {
            url: url.into(),
            sid: String::new(),
            callback: Callback::Parse,
            priority: 0,
            dont_filter: false,
            meta: HashMap::new(),
            retry_count: 0,
            options: RequestOptions::default(),
        }
    }

    /// Fetch this request with the session registered under `sid`.
    pub fn sid(mut self, sid: impl Into<String>) -> Self {
        self.sid = sid.into();
        self
    }

    /// Send the response to this callback.
    pub fn callback(mut self, callback: Callback) -> Self {
        self.callback = callback;
        self
    }

    /// Schedule this request at a given priority; higher runs first.
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Skip the duplicate filter for this request.
    pub fn dont_filter(mut self, yes: bool) -> Self {
        self.dont_filter = yes;
        self
    }

    /// Attach one metadata entry, carried through to the response.
    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Replace this request's transport options.
    pub fn options(mut self, options: RequestOptions) -> Self {
        self.options = options;
        self
    }

    /// The URL's host (with its port when the URL carries one), used for per-domain limits
    /// and delays. An unparsable URL has an empty domain.
    pub fn domain(&self) -> String {
        match ::url::Url::parse(&self.url) {
            Ok(parsed) => {
                let host = parsed.host_str().unwrap_or("").to_string();
                match parsed.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host,
                }
            }
            Err(_) => String::new(),
        }
    }

    /// The deduplication fingerprint: SHA-1 over the canonical URL, method, body and sid.
    ///
    /// The hashed value is a JSON object with sorted keys holding `sid`, the hex-encoded
    /// `body`, the `method` and the canonical `url`; `include_kwargs` adds a sorted `kwargs`
    /// entry built from [`RequestOptions`] minus the body fields, and `include_headers` adds
    /// a `headers` entry of lower-cased, hex-encoded name/value pairs. The layout matches the
    /// Python implementation, so both agree on what counts as a duplicate.
    pub fn fingerprint(
        &self,
        include_kwargs: bool,
        include_headers: bool,
        keep_fragments: bool,
    ) -> [u8; 20] {
        let mut data: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
        data.insert("sid", serde_json::Value::String(self.sid.clone()));
        data.insert(
            "body",
            serde_json::Value::String(hex::encode(self.options.body_bytes())),
        );
        data.insert(
            "method",
            serde_json::Value::String(self.options.method_or_get().to_string()),
        );
        data.insert(
            "url",
            serde_json::Value::String(canonicalize_url(&self.url, keep_fragments)),
        );

        if include_kwargs {
            let mut kwargs: BTreeMap<String, String> = BTreeMap::new();
            if !self.options.method.is_empty() {
                kwargs.insert("method".to_string(), stable_repr(&self.options.method));
            }
            if !self.options.headers.is_empty() {
                kwargs.insert("headers".to_string(), stable_repr(&self.options.headers));
            }
            if let Some(proxy) = &self.options.proxy {
                kwargs.insert("proxy".to_string(), stable_repr(proxy));
            }
            if let Some(timeout) = self.options.timeout {
                kwargs.insert("timeout".to_string(), stable_repr(&timeout.as_secs_f64()));
            }
            let pairs: Vec<serde_json::Value> = kwargs
                .into_iter()
                .map(|(key, value)| {
                    serde_json::Value::Array(vec![
                        serde_json::Value::String(key),
                        serde_json::Value::String(value),
                    ])
                })
                .collect();
            data.insert("kwargs", serde_json::Value::Array(pairs));
        }

        if include_headers {
            let pairs: Vec<serde_json::Value> = self
                .options
                .headers
                .iter()
                .map(|(name, value)| {
                    serde_json::Value::Array(vec![
                        serde_json::Value::String(hex::encode(name.to_lowercase().as_bytes())),
                        serde_json::Value::String(hex::encode(value.as_bytes())),
                    ])
                })
                .collect();
            data.insert("headers", serde_json::Value::Array(pairs));
        }

        let encoded = serde_json::to_vec(&data).unwrap_or_default();
        let digest = Sha1::digest(&encoded);
        let mut out = [0u8; 20];
        out.copy_from_slice(&digest[..]);
        out
    }
}

/// Python's `_stable_value_repr`: a JSON dump with sorted keys.
fn stable_repr<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

impl std::fmt::Display for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_python() {
        let request = Request::new("https://example.com/a");
        assert_eq!(request.priority, 0);
        assert!(!request.dont_filter);
        assert_eq!(request.callback, Callback::Parse);
        assert_eq!(request.options.method_or_get(), "GET");
    }

    #[test]
    fn domain_includes_a_non_default_port() {
        assert_eq!(
            Request::new("https://a.example.com/x").domain(),
            "a.example.com"
        );
        assert_eq!(
            Request::new("http://a.example.com:8080/x").domain(),
            "a.example.com:8080"
        );
        assert_eq!(Request::new("nonsense").domain(), "");
    }

    #[test]
    fn fingerprint_is_stable_and_url_canonical() {
        let one = Request::new("http://example.com/a?b=2&a=1");
        let two = Request::new("http://example.com/a?a=1&b=2");
        assert_eq!(
            one.fingerprint(false, false, false),
            two.fingerprint(false, false, false)
        );
    }

    #[test]
    fn fingerprint_separates_fragments_only_when_asked() {
        let plain = Request::new("http://example.com/a");
        let fragment = Request::new("http://example.com/a#x");
        assert_eq!(
            plain.fingerprint(false, false, false),
            fragment.fingerprint(false, false, false)
        );
        assert_ne!(
            plain.fingerprint(false, false, true),
            fragment.fingerprint(false, false, true)
        );
    }

    #[test]
    fn fingerprint_separates_sessions_and_methods_and_bodies() {
        let base = Request::new("http://example.com/a");
        let other_session = Request::new("http://example.com/a").sid("browser");
        assert_ne!(
            base.fingerprint(false, false, false),
            other_session.fingerprint(false, false, false)
        );

        let posted = Request::new("http://example.com/a")
            .options(RequestOptions::new().method("POST").form([("a", "1")]));
        assert_ne!(
            base.fingerprint(false, false, false),
            posted.fingerprint(false, false, false)
        );
    }

    #[test]
    fn headers_only_count_when_requested() {
        let base = Request::new("http://example.com/a");
        let with_header = Request::new("http://example.com/a")
            .options(RequestOptions::new().header("X-Token", "abc"));
        assert_eq!(
            base.fingerprint(false, false, false),
            with_header.fingerprint(false, false, false)
        );
        assert_ne!(
            base.fingerprint(false, true, false),
            with_header.fingerprint(false, true, false)
        );
        assert_ne!(
            base.fingerprint(true, false, false),
            with_header.fingerprint(true, false, false)
        );
    }

    #[test]
    fn fingerprint_matches_a_known_sha1() {
        // sha1 of {"body":"","method":"GET","sid":"","url":"http://example.com/"}
        let request = Request::new("http://example.com/");
        let payload = br#"{"body":"","method":"GET","sid":"","url":"http://example.com/"}"#;
        let expected = Sha1::digest(payload);
        assert_eq!(request.fingerprint(false, false, false)[..], expected[..]);
    }

    #[test]
    fn requests_round_trip_through_json() {
        let request = Request::new("http://example.com/a")
            .callback(Callback::named("parse_item"))
            .priority(3)
            .meta("page", serde_json::json!(2));
        let encoded = serde_json::to_string(&request).expect("serialize");
        let decoded: Request = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.url, request.url);
        assert_eq!(decoded.callback, Callback::Named("parse_item".to_string()));
        assert_eq!(decoded.priority, 3);
        assert_eq!(decoded.meta.get("page"), Some(&serde_json::json!(2)));
    }
}
