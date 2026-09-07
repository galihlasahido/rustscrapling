//! A small crawl: follow the pagination of a site, scrape every quote, export the items.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example spider -- https://quotes.toscrape.com/
//! ```

#[cfg(feature = "spider")]
mod quotes {
    use std::sync::Arc;

    use async_trait::async_trait;
    use rustscrapling::response::FollowOptions;
    use rustscrapling::{
        Callback, CrawlerEngine, EngineOptions, FetcherSession, Output, Response, Result, Session,
        SessionManager, Spider, SpiderConfig,
    };
    use serde_json::json;

    /// How many listing pages the crawl follows before it stops. A cap keeps a site with an
    /// endless "next" link from turning this example into an unbounded crawl.
    const MAX_PAGES: u64 = 10;

    /// Scrapes quotes and follows the "next page" link.
    pub struct QuotesSpider {
        /// Where the crawl starts.
        pub start_url: String,
        /// The only domain the crawl may visit, in the shape `Request::domain()` produces:
        /// the host, plus the port when the URL carries a non-default one.
        pub domain: String,
    }

    #[async_trait]
    impl Spider for QuotesSpider {
        fn name(&self) -> &str {
            "quotes"
        }

        fn start_urls(&self) -> Vec<String> {
            vec![self.start_url.clone()]
        }

        fn allowed_domains(&self) -> Vec<String> {
            // An empty list means "no restriction", so a URL we could not parse must not turn
            // into an empty allowed domain that would drop every follow-up request.
            if self.domain.is_empty() {
                Vec::new()
            } else {
                vec![self.domain.clone()]
            }
        }

        fn config(&self) -> SpiderConfig {
            let mut config = SpiderConfig::default();
            config.concurrent_requests = 4;
            config.download_delay = 0.25;
            config.robots_txt_obey = true;
            config
        }

        async fn parse(&self, response: Response) -> Result<Vec<Output>> {
            let mut output = Vec::new();

            for quote in response.css("div.quote")?.iter() {
                let text = quote
                    .css("span.text::text")?
                    .get()
                    .map(|value| value.clean().into_string())
                    .unwrap_or_default();
                let author = quote
                    .css("small.author::text")?
                    .get()
                    .map(|value| value.clean().into_string())
                    .unwrap_or_default();
                let tags: Vec<String> = quote.css("a.tag::text")?.getall().getall();
                output.push(Output::Item(json!({
                    "text": text,
                    "author": author,
                    "tags": tags,
                })));
            }

            // `follow` resolves the relative URL and keeps the session, meta and referer.
            //
            // The page counter travels in the request meta and caps the crawl: a site whose
            // "next" link never runs out would otherwise keep the engine going forever and
            // pile items up in memory until the process dies.
            let page = response
                .meta
                .get("page")
                .and_then(|value| value.as_u64())
                .unwrap_or(1);
            if page < MAX_PAGES {
                if let Some(next) = response.css("li.next a::attr(href)")?.get() {
                    let mut options = FollowOptions::default();
                    options.callback = Some(Callback::Parse);
                    options.meta.insert("page".to_string(), json!(page + 1));
                    output.push(Output::Request(response.follow(next.as_str(), options)?));
                }
            }

            Ok(output)
        }

        async fn on_close(&self) -> Result<()> {
            println!("done crawling {}", self.start_url);
            Ok(())
        }
    }

    /// Run the spider with one HTTP session and print what it found.
    pub async fn crawl(start_url: String) -> Result<()> {
        let domain = url::Url::parse(&start_url)
            .ok()
            .and_then(|parsed| {
                parsed.host_str().map(|host| match parsed.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_string(),
                })
            })
            .unwrap_or_default();

        let spider = Arc::new(QuotesSpider { start_url, domain });

        let mut sessions = SessionManager::new();
        sessions.add_default("http", Session::Http(FetcherSession::new()?))?;

        let engine = CrawlerEngine::new(spider, Arc::new(sessions), EngineOptions::default());
        let result = engine.crawl().await?;

        for item in result.items.iter().take(5) {
            println!(
                "{} - {}",
                item["author"].as_str().unwrap_or_default(),
                item["text"].as_str().unwrap_or_default()
            );
        }

        println!(
            "scraped {} items with {} requests in {:.1}s (completed: {})",
            result.stats.items_scraped,
            result.stats.requests_count,
            result.stats.elapsed_seconds(),
            result.completed()
        );

        // The exporters write JSON, JSON Lines and CSV.
        result.items.to_json("quotes.json", true)?;
        result.items.to_jsonl("quotes.jsonl")?;
        result.items.to_csv("quotes.csv", None)?;
        println!("wrote quotes.json, quotes.jsonl and quotes.csv");
        Ok(())
    }
}

#[cfg(feature = "spider")]
#[tokio::main]
async fn main() -> rustscrapling::Result<()> {
    let start_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://quotes.toscrape.com/".to_string());
    quotes::crawl(start_url).await
}

#[cfg(not(feature = "spider"))]
fn main() {
    eprintln!(
        "this example needs the `spider` feature: cargo run --features spider --example spider"
    );
}
