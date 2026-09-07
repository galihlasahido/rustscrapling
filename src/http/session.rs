//! [`FetcherSession`]: one client and one cookie jar reused across requests.
//!
//! Port of Scrapling's `FetcherSession` (`scrapling/engines/static.py`). Python enters the
//! session with a context manager (`with FetcherSession(...) as session:`); in Rust the session
//! is just a value whose lifetime is the "session", so there is nothing to enter or exit.

use std::sync::Arc;

use crate::error::Result;
use crate::response::{Cookie, HeaderMap};

use super::fetcher::{FetcherBuilder, Inner, RequestBuilder};

/// A fetcher that keeps one `reqwest::Client` and one cookie jar across requests.
///
/// Cloning a session is cheap and shares the jar, so clones stay logged in together.
#[derive(Debug, Clone)]
pub struct FetcherSession {
    inner: Arc<Inner>,
}

impl FetcherSession {
    /// A session with the default settings.
    pub fn new() -> Result<FetcherSession> {
        FetcherSession::builder().build_session()
    }

    /// Start configuring a session; finish with [`FetcherBuilder::build_session`].
    pub fn builder() -> FetcherBuilder {
        FetcherBuilder::default()
    }

    pub(crate) fn from_inner(inner: Arc<Inner>) -> FetcherSession {
        FetcherSession { inner }
    }

    /// Begin a GET request on this session.
    pub fn get(&self, url: &str) -> RequestBuilder {
        self.request("GET", url)
    }

    /// Begin a POST request on this session.
    pub fn post(&self, url: &str) -> RequestBuilder {
        self.request("POST", url)
    }

    /// Begin a PUT request on this session.
    pub fn put(&self, url: &str) -> RequestBuilder {
        self.request("PUT", url)
    }

    /// Begin a DELETE request on this session.
    pub fn delete(&self, url: &str) -> RequestBuilder {
        self.request("DELETE", url)
    }

    /// Begin a request with any method on this session.
    pub fn request(&self, method: &str, url: &str) -> RequestBuilder {
        RequestBuilder::new(Arc::clone(&self.inner), method, url)
    }

    /// The headers this session adds to every request.
    pub fn headers(&self) -> &HeaderMap {
        &self.inner.config.headers
    }

    /// The cookies the jar currently holds for a URL.
    ///
    /// The jar stores only what a request would send, so the returned cookies carry the URL's
    /// host as their domain and `/` as their path rather than the exact attributes the server
    /// sent; read the `cookies` field of a [`crate::Response`] when you need those.
    ///
    /// An unparsable URL yields an empty list rather than an error.
    pub fn cookies(&self, url: &str) -> Vec<Cookie> {
        use reqwest::cookie::CookieStore;

        let jar = match &self.inner.jar {
            Some(jar) => jar,
            None => return Vec::new(),
        };
        let parsed = match url::Url::parse(url) {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };
        let header = match jar.cookies(&parsed) {
            Some(header) => header,
            None => return Vec::new(),
        };
        let value = match header.to_str() {
            Ok(value) => value.to_string(),
            Err(_) => return Vec::new(),
        };
        let domain = parsed.host_str().unwrap_or_default().to_string();

        value
            .split(';')
            .filter_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                let name = name.trim();
                if name.is_empty() {
                    return None;
                }
                Some(Cookie {
                    name: name.to_string(),
                    value: value.trim().to_string(),
                    domain: domain.clone(),
                    path: "/".to_string(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn cookies_persist_across_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/set"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<html>set</html>", "text/html")
                    .append_header("set-cookie", "sid=abc123; Path=/"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/check"))
            .and(header("cookie", "sid=abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html>ok</html>", "text/html"))
            .mount(&server)
            .await;

        // The mock server listens on `127.0.0.1`, which a session refuses to be aimed at unless
        // it is told those addresses are fine.
        let session = FetcherSession::builder()
            .allow_private_addresses(true)
            .build_session()
            .expect("session");
        let first = session
            .get(&format!("{}/set", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(first.status, 200);

        let stored = session.cookies(&server.uri());
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "sid");
        assert_eq!(stored[0].value, "abc123");

        let second = session
            .get(&format!("{}/check", server.uri()))
            .send()
            .await
            .expect("response");
        assert_eq!(second.status, 200);
    }

    #[test]
    fn unknown_urls_yield_no_cookies() {
        let session = FetcherSession::new().expect("session");
        assert!(session.cookies("not a url").is_empty());
        assert!(session.cookies("https://never-visited.test/").is_empty());
    }

    #[test]
    fn session_headers_are_visible() {
        let headers = HeaderMap::from_pairs([("X-Api-Key".to_string(), "k".to_string())]);
        let session = FetcherSession::builder()
            .headers(headers)
            .build_session()
            .expect("session");
        assert_eq!(session.headers().get("x-api-key"), Some("k"));
    }
}
