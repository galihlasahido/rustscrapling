//! Fetching a page over HTTP and parsing the response.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example fetch -- https://quotes.toscrape.com/
//! ```

#[cfg(feature = "http")]
#[tokio::main]
async fn main() -> rustscrapling::Result<()> {
    use std::time::Duration;

    use rustscrapling::{BrowserProfile, Fetcher, FollowRedirects, HeaderMap};

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://quotes.toscrape.com/".to_string());

    // A fetcher with Scrapling's defaults: browser-shaped headers, retries, safe redirects.
    let fetcher = Fetcher::builder()
        .impersonate(BrowserProfile::Chrome)
        .stealthy_headers(true)
        .timeout(Duration::from_secs(20))
        .retries(2)
        .retry_delay(Duration::from_millis(500))
        .follow_redirects(FollowRedirects::Safe)
        .build()?;

    let response = fetcher.get(&url).send().await?;

    println!("{} {} {}", response.status, response.reason, response.url);
    println!(
        "{} bytes, encoding {}",
        response.body.len(),
        response.encoding
    );
    if let Some(content_type) = response.headers.get("content-type") {
        println!("content-type: {content_type}");
    }

    // The response is a parsed document: every `Selector` method works on it directly.
    if let Some(title) = response.css_first("title")? {
        println!("title: {}", title.text().clean());
    }
    println!("{} links on the page", response.css("a")?.len());
    for quote in response.css(".quote .text::text")?.iter().take(3) {
        println!("  {}", quote.text().clean());
    }

    // Per-request overrides: extra headers, a query string, a different timeout.
    let mut headers = HeaderMap::new();
    headers.insert("Accept-Language", "en-GB,en;q=0.9");

    let searched = fetcher
        .get(&url)
        .headers(headers)
        .query([("page", "1")])
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    println!("second request: {}", searched.status);

    // A session keeps one client and one cookie jar across requests.
    let session = Fetcher::builder().build_session()?;
    let first = session.get(&url).send().await?;
    println!(
        "session request: {} ({} cookies)",
        first.status,
        session.cookies(&url).len()
    );
    Ok(())
}

#[cfg(not(feature = "http"))]
fn main() {
    eprintln!("this example needs the `http` feature: cargo run --features http --example fetch");
}
