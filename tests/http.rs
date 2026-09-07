#![cfg(feature = "http")]
//! The HTTP layer, exercised against a local `wiremock` server.
//!
//! These are the Rust equivalents of Scrapling's `Fetcher.get/post/put/delete`, its
//! `stealthy_headers` and `impersonate` options, its proxy rotation, and its redirect policy.

use std::time::Duration;

use rustscrapling::http::is_proxy_error;
use rustscrapling::{
    BrowserProfile, Error, Fetcher, FetcherSession, FollowRedirects, HeaderMap, ProxyRotator,
    Response, Result, StatusText,
};
use wiremock::matchers::{
    body_string_contains, header, header_exists, header_regex, method, path, query_param,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A small page every test can query.
const PAGE: &str = r#"<html><head><title>Shop</title></head><body>
<div class="product"><h2>Product 1</h2><span class="price">$10.99</span></div>
<div class="product"><h2>Product 2</h2><span class="price">$20.99</span></div>
<a href="/page/2">next</a>
</body></html>"#;

/// Turn a missing value into an [`Error`] instead of panicking.
fn need<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| Error::other(format!("expected {what}")))
}

/// A builder aimed at the local mock server.
///
/// A fetcher refuses to be pointed at a loopback, link-local or private address unless it is told
/// those are fine — that is what `allow_private_addresses` is for — and every `MockServer` here
/// listens on `127.0.0.1`, so each test opts in explicitly.
fn local() -> rustscrapling::http::FetcherBuilder {
    Fetcher::builder().allow_private_addresses(true)
}

/// A fetcher aimed at the local mock server.
fn local_fetcher() -> Result<Fetcher> {
    local().build()
}

/// A session aimed at the local mock server.
fn local_session() -> Result<FetcherSession> {
    local().build_session()
}

/// An HTML response template with the header a real server would send.
///
/// The body has to go in through `set_body_raw`: `set_body_string` forces the template's mime
/// to `text/plain`, and the template writes its mime over any `content-type` header set here.
fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_string(), "text/html; charset=utf-8")
}

/// A `GET` returns a parsed page: the `Response` is a `Selector` too.
#[tokio::test]
async fn get_returns_a_parsed_page() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/shop"))
        .respond_with(html(PAGE))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/shop", server.uri());
    let response = local_fetcher()?.get(&url).send().await?;

    assert_eq!(response.status, 200);
    assert!(response.is_success());
    assert_eq!(response.method, "GET");
    assert_eq!(response.url, url);
    assert!(!response.reason.is_empty());
    assert!(!response.body.is_empty());
    assert!(response.text_body().contains("Product 1"));
    assert_eq!(
        need(response.headers.get("content-type"), "a content type")?,
        "text/html; charset=utf-8"
    );

    // Deref to `Selector`: every parser method is available on the response.
    assert_eq!(response.css(".product")?.len(), 2);
    assert_eq!(
        response.css(".price::text")?.getall().getall(),
        vec!["$10.99", "$20.99"]
    );
    assert_eq!(
        need(response.css_first("title")?, "a title")?
            .text()
            .as_str(),
        "Shop"
    );
    assert_eq!(response.selector().css(".product")?.len(), 2);

    // The relative link resolves against the response URL.
    let href = need(response.css("a::attr(href)")?.get(), "the next link")?;
    assert_eq!(
        response.urljoin(href.as_str())?,
        format!("{}/page/2", server.uri())
    );

    server.verify().await;
    Ok(())
}

/// A 404 is a response, not an error.
#[tokio::test]
async fn error_statuses_are_returned() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("gone"))
        .mount(&server)
        .await;

    let response = local_fetcher()?
        .get(&format!("{}/missing", server.uri()))
        .send()
        .await?;
    assert_eq!(response.status, 404);
    assert!(!response.is_success());
    Ok(())
}

/// `stealthy_headers` is on by default: browser headers plus a Google referer.
#[tokio::test]
async fn stealthy_headers_are_sent_by_default() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stealth"))
        .and(header_exists("user-agent"))
        .and(header_exists("accept"))
        .and(header("referer", "https://www.google.com/"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let response = local_fetcher()?
        .get(&format!("{}/stealth", server.uri()))
        .send()
        .await?;
    assert_eq!(
        response.status, 200,
        "the stealth headers were not sent as documented"
    );
    assert!(response.request_headers.contains_key("user-agent"));
    Ok(())
}

/// A caller-set header wins over the profile's, and `impersonate` picks the profile.
#[tokio::test]
async fn per_request_headers_and_impersonation() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/headers"))
        .and(header("x-test", "1"))
        .and(header("referer", "https://example.com/from"))
        .and(header_regex("user-agent", "Firefox"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let mut extra = HeaderMap::new();
    extra.insert("Referer", "https://example.com/from");

    let response = local_fetcher()?
        .get(&format!("{}/headers", server.uri()))
        .header("x-test", "1")
        .headers(extra)
        .impersonate(BrowserProfile::Firefox)
        .send()
        .await?;
    assert_eq!(
        response.status, 200,
        "the request headers were not honoured"
    );
    Ok(())
}

/// Query parameters are appended to the URL.
#[tokio::test]
async fn query_parameters() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust"))
        .and(query_param("page", "2"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let response = local_fetcher()?
        .get(&format!("{}/search", server.uri()))
        .query([("q", "rust"), ("page", "2")])
        .send()
        .await?;
    assert_eq!(response.status, 200);
    assert!(response.url.contains("q=rust"));
    Ok(())
}

/// `POST`, `PUT` and `DELETE`, with form, JSON and raw bodies.
#[tokio::test]
async fn the_other_http_methods() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/post"))
        .and(body_string_contains("key=value"))
        .respond_with(ResponseTemplate::new(200).set_body_string("posted"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/json"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/put"))
        .and(body_string_contains("raw body"))
        .respond_with(ResponseTemplate::new(200).set_body_string("put"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/delete"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/patch"))
        .respond_with(ResponseTemplate::new(200).set_body_string("patched"))
        .mount(&server)
        .await;

    let fetcher = local_fetcher()?;

    let posted = fetcher
        .post(&format!("{}/post", server.uri()))
        .form([("key", "value")])
        .send()
        .await?;
    assert_eq!(posted.status, 200);
    assert_eq!(posted.method, "POST");

    let json = fetcher
        .post(&format!("{}/json", server.uri()))
        .json(&serde_json::json!({"key": "value"}))?
        .send()
        .await?;
    assert_eq!(json.status, 200);
    let payload: serde_json::Value = json.json()?;
    assert_eq!(payload["ok"], true);

    let put = fetcher
        .put(&format!("{}/put", server.uri()))
        .body("raw body")
        .send()
        .await?;
    assert_eq!(put.status, 200);

    let deleted = fetcher
        .delete(&format!("{}/delete", server.uri()))
        .send()
        .await?;
    assert_eq!(deleted.status, 204);

    let patched = fetcher
        .request("PATCH", &format!("{}/patch", server.uri()))
        .send()
        .await?;
    assert_eq!(patched.status, 200);
    assert_eq!(patched.method, "PATCH");
    Ok(())
}

/// Redirects are followed, and the final URL is the one on the response.
#[tokio::test]
async fn redirects_are_followed() -> Result<()> {
    let server = MockServer::start().await;
    let target = format!("{}/new", server.uri());
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let response = local()
        .follow_redirects(FollowRedirects::All)
        .build()?
        .get(&format!("{}/old", server.uri()))
        .send()
        .await?;
    assert_eq!(response.status, 200);
    assert_eq!(response.url, target);
    assert!(!response.css(".product")?.is_empty());
    Ok(())
}

/// `FollowRedirects::None` hands the 3xx back untouched.
#[tokio::test]
async fn redirects_can_be_disabled() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/new"))
        .mount(&server)
        .await;

    let response = local_fetcher()?
        .get(&format!("{}/old", server.uri()))
        .follow_redirects(FollowRedirects::None)
        .send()
        .await?;
    assert_eq!(response.status, 302);
    assert_eq!(
        need(response.headers.get("location"), "a location header")?,
        "/new"
    );
    Ok(())
}

/// `FollowRedirects::Safe` refuses to be walked into a private address.
#[tokio::test]
async fn safe_redirects_refuse_private_addresses() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:1/secret"),
        )
        .mount(&server)
        .await;

    let result = local_fetcher()?
        .get(&format!("{}/private", server.uri()))
        .follow_redirects(FollowRedirects::Safe)
        .retries(0)
        .send()
        .await;
    assert!(
        result.is_err(),
        "a redirect into loopback must not produce a successful response"
    );
    Ok(())
}

/// A session keeps its cookies between requests.
#[tokio::test]
async fn a_session_keeps_cookies() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(html(PAGE).insert_header("set-cookie", "sid=abc; Path=/"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/account"))
        .and(header("cookie", "sid=abc"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let session = local_session()?;
    let login = session
        .get(&format!("{}/login", server.uri()))
        .send()
        .await?;
    assert_eq!(login.status, 200);

    let cookies = session.cookies(&server.uri());
    assert!(
        cookies.iter().any(|cookie| cookie.name == "sid"),
        "the jar did not keep the cookie: {cookies:?}"
    );

    let account = session
        .get(&format!("{}/account", server.uri()))
        .send()
        .await?;
    assert_eq!(account.status, 200, "the cookie was not replayed");
    Ok(())
}

/// A session built from the builder shares the builder's settings.
#[tokio::test]
async fn sessions_can_be_configured() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .and(header("x-session", "yes"))
        .respond_with(html(PAGE))
        .mount(&server)
        .await;

    let mut headers = HeaderMap::new();
    headers.insert("x-session", "yes");
    let session = local()
        .headers(headers)
        .retries(1)
        .timeout(Duration::from_secs(10))
        .stealthy_headers(true)
        .build_session()?;

    let response = session.get(&format!("{}/x", server.uri())).send().await?;
    assert_eq!(response.status, 200);
    Ok(())
}

/// A request that outlives its timeout is an error, not a panic.
#[tokio::test]
async fn timeouts_are_errors() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(html(PAGE).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;

    let result = local_fetcher()?
        .get(&format!("{}/slow", server.uri()))
        .timeout(Duration::from_millis(100))
        .retries(0)
        .send()
        .await;
    assert!(
        matches!(result, Err(Error::Http(_))),
        "expected an http error"
    );
    Ok(())
}

/// A URL that cannot be parsed is rejected before anything is sent.
#[tokio::test]
async fn bad_urls_are_errors() -> Result<()> {
    let result = local_fetcher()?
        .get("not a url at all")
        .retries(0)
        .send()
        .await;
    assert!(result.is_err());
    Ok(())
}

/// Every browser profile has a plausible header set.
#[test]
fn browser_profiles() -> Result<()> {
    for (profile, marker) in [
        (BrowserProfile::Chrome, "Chrome"),
        (BrowserProfile::Firefox, "Firefox"),
        (BrowserProfile::Safari, "Safari"),
        (BrowserProfile::Edge, "Edg"),
    ] {
        let agent = profile.user_agent();
        assert!(
            agent.contains(marker),
            "{profile:?} should look like {marker}, got {agent}"
        );

        let headers = profile.headers();
        assert_eq!(
            need(headers.get("user-agent"), "a user agent header")?,
            agent
        );
        assert!(headers.contains_key("accept"));
        assert!(headers.contains_key("accept-language"));
    }

    assert_eq!(BrowserProfile::default(), BrowserProfile::Chrome);
    Ok(())
}

/// The rotator hands proxies out cyclically and refuses an empty pool.
#[test]
fn proxy_rotation() -> Result<()> {
    let rotator = ProxyRotator::new(vec![
        "http://one.example:8080".to_string(),
        "http://two.example:8080".to_string(),
    ])?;
    assert_eq!(rotator.len(), 2);
    assert!(!rotator.is_empty());
    assert_eq!(rotator.proxies().len(), 2);

    let picked: Vec<String> = (0..4).map(|_| rotator.get_proxy()).collect();
    assert_eq!(picked[0], picked[2]);
    assert_eq!(picked[1], picked[3]);
    assert_ne!(picked[0], picked[1]);

    assert!(ProxyRotator::new(Vec::new()).is_err());

    // A custom strategy: always the first proxy.
    let sticky = ProxyRotator::with_strategy(
        vec![
            "http://a.example".to_string(),
            "http://b.example".to_string(),
        ],
        |proxies, index| (proxies[0].clone(), index),
    )?;
    assert_eq!(sticky.get_proxy(), "http://a.example");
    assert_eq!(sticky.get_proxy(), "http://a.example");
    Ok(())
}

/// `is_proxy_error` recognises the messages Python looks for.
#[test]
fn proxy_error_detection() {
    for message in [
        "net::ERR_PROXY_CONNECTION_FAILED",
        "net::ERR_TUNNEL_CONNECTION_FAILED",
        "Connection refused",
        "connection reset by peer",
        "connection timed out",
        "failed to connect to proxy",
        "could not resolve proxy: example",
    ] {
        assert!(
            is_proxy_error(&Error::Http(message.to_string())),
            "{message} should look like a proxy failure"
        );
    }

    assert!(!is_proxy_error(&Error::Http("404 Not Found".to_string())));
    assert!(!is_proxy_error(&Error::Other(
        "nothing to do with it".to_string()
    )));
}

/// `HeaderMap` is case-insensitive but keeps insertion order and the original casing.
#[test]
fn header_map_behaviour() -> Result<()> {
    let mut headers = HeaderMap::new();
    assert!(headers.is_empty());

    headers.insert("Content-Type", "text/html");
    headers.insert("X-Trace", "1");
    assert_eq!(headers.len(), 2);
    assert_eq!(
        need(headers.get("content-type"), "content type")?,
        "text/html"
    );
    assert_eq!(
        need(headers.get("CONTENT-TYPE"), "content type")?,
        "text/html"
    );
    assert!(headers.contains_key("x-trace"));

    // Insert replaces, ignoring case.
    headers.insert("CONTENT-TYPE", "application/json");
    assert_eq!(headers.len(), 2);
    assert_eq!(
        need(headers.get("Content-Type"), "content type")?,
        "application/json"
    );

    let names: Vec<&str> = headers.iter().map(|(name, _)| name).collect();
    assert_eq!(names.len(), 2);

    assert_eq!(headers.remove("x-trace").as_deref(), Some("1"));
    assert!(!headers.contains_key("X-Trace"));
    assert_eq!(headers.len(), 1);

    let built = HeaderMap::from_pairs([
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
    ]);
    assert_eq!(built.len(), 2);
    let round_trip = serde_json::to_string(&built)?;
    let parsed: HeaderMap = serde_json::from_str(&round_trip)?;
    assert_eq!(parsed, built);
    Ok(())
}

/// A `Response` can also be built by hand, which is what the cache and the tests do.
#[test]
fn responses_can_be_built_by_hand() -> Result<()> {
    let response = Response::new("https://example.com/", PAGE.as_bytes().to_vec(), 200)?
        .with_reason("OK")
        .with_method("GET")
        .with_encoding("utf-8")
        .with_headers(HeaderMap::from_pairs([(
            "content-type".to_string(),
            "text/html".to_string(),
        )]));

    assert!(response.is_success());
    assert_eq!(response.encoding, "utf-8");
    assert_eq!(response.css(".product")?.len(), 2);
    let displayed = response.to_string();
    assert!(
        displayed.contains("200") && displayed.contains("https://example.com/"),
        "unexpected Display output: {displayed}"
    );

    // The status phrase table.
    assert_eq!(Response::status_text(200), "OK");
    assert_eq!(StatusText::get(200), "OK");
    assert_eq!(StatusText::get(404), "Not Found");
    assert_eq!(StatusText::get(429), "Too Many Requests");
    assert_eq!(StatusText::get(503), "Service Unavailable");
    assert_eq!(StatusText::get(299), "Unknown Status Code");
    Ok(())
}
