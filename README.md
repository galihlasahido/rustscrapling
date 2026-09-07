# rustscrapling

A Rust port of the Python web-scraping library [Scrapling](https://github.com/D4Vinci/Scrapling).

It is three layers that stack:

- **Parse.** `Selector` wraps an HTML document and queries it with CSS (including the
  non-standard `::text` and `::attr(name)` pseudo-elements), with filters, by text, by regex,
  or by structural similarity to an element you already have. Strings come back as `Text` /
  `Texts`, which carry the cleaning, regex and JSON helpers Scrapling puts on `str`.
- **Adapt.** Any element can be fingerprinted and stored (SQLite by default, or your own
  `Storage`). When the site changes its markup, the element is relocated by similarity instead
  of the scraper simply breaking.
- **Fetch and crawl.** `Fetcher` / `FetcherSession` wrap `reqwest` with browser-shaped headers,
  proxy rotation, retries and a redirect policy that refuses to be walked into a private
  address. On top of that sits a small Scrapy-shaped crawl framework: implement the `Spider`
  trait, hand it to `CrawlerEngine`, and get concurrency limits, per-domain delays,
  autothrottling, a duplicate filter, robots.txt support, checkpoints for pause/resume,
  streaming, and JSON/JSONL/CSV export.

The public API is specified in [`CONTRACT.md`](CONTRACT.md), which also maps every Rust name
onto the Python name it came from.

```toml
[dependencies]
rustscrapling = "0.1"
```

## Feature flags

| Feature   | Default | What it enables |
|-----------|---------|-----------------|
| `http`    | yes     | the `reqwest`-based `Fetcher` and `FetcherSession` |
| `spider`  | yes     | the crawl framework (`Spider`, `CrawlerEngine`, …); implies `http` |
| `browser` | no      | the `chromiumoxide`-based `DynamicFetcher` and `DynamicSession` |

The parser, the adaptive layer and `Response` are always available.

```toml
# parsing only, no network stack
rustscrapling = { version = "0.1", default-features = false }

# with a real browser
rustscrapling = { version = "0.1", features = ["browser"] }
```

## Quick start

### Parsing

```rust
use rustscrapling::{Filter, ReOptions, Result, Selector};

const HTML: &str = r#"<html><body>
  <article class="product"><h3>Product 1</h3><span class="price">$10.99</span></article>
  <a href="/page/2">next</a>
</body></html>"#;

fn main() -> Result<()> {
    let page = Selector::with_url(HTML, "https://example.com/shop/")?;

    // CSS, with `::text` and `::attr(name)`.
    let names: Vec<String> = page.css("article.product h3::text")?.getall().getall();
    let links: Vec<String> = page.css("a::attr(href)")?.getall().getall();

    // The first match only.
    if let Some(price) = page.css_first(".price")? {
        let amount = price.re_first(r"[\d.]+", ReOptions::default())?;
        println!("{names:?} {links:?} {amount:?}");
    }

    // Filters, instead of Python's `find_all(*args, **kwargs)`.
    let headings = page.find_all(&Filter::new().tag("h3").regex(r"Product \d")?)?;

    // By text, by regex, and by similarity to an element you already found.
    let first = page.find_by_regex(r"Product \d", true, true, true)?;
    if let Some(target) = first.first() {
        let others = target.find_similar(0.2, &["href", "src"], false);
        println!("{} similar elements", others.len());
    }
    println!("{}", headings.len());
    Ok(())
}
```

### Adaptive selection

```rust
use rustscrapling::{Adaptive, Result, Selector, SqliteStorage};

const HTML: &str = r#"<html><body><div class="product-list">
  <article class="product" data-id="1"><h3>Product 1</h3></article>
</div></body></html>"#;

fn main() -> Result<()> {
    let storage = SqliteStorage::open("elements.db")?;
    let page = Adaptive::new(Selector::with_url(HTML, "https://example.com/")?, storage);

    // While the selector still works, remember what it matched.
    let products = page.css_adaptive(".product-list article.product", "product-card", true, 40.0)?;

    // After a redesign the same call relocates the element by similarity.
    println!("{} products", products.len());
    Ok(())
}
```

### Fetching

```rust
use std::time::Duration;
use rustscrapling::{BrowserProfile, Fetcher, FollowRedirects, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let fetcher = Fetcher::builder()
        .impersonate(BrowserProfile::Chrome)
        .stealthy_headers(true)
        .timeout(Duration::from_secs(20))
        .retries(2)
        .follow_redirects(FollowRedirects::Safe)
        .build()?;

    let page = fetcher.get("https://quotes.toscrape.com/").send().await?;
    println!("{} {}", page.status, page.url);

    // The response *is* a parsed document.
    for quote in page.css(".quote .text::text")?.iter() {
        println!("{}", quote.text().clean());
    }

    // Sessions keep one client and one cookie jar.
    let session = Fetcher::builder().build_session()?;
    session.post("https://example.com/login")
        .form([("user", "me"), ("password", "secret")])
        .send()
        .await?;
    Ok(())
}
```

A fetcher refuses to be aimed at a loopback, link-local or private address — as an IP literal, as
an internal-looking name, or through DNS — because in a crawl the URL often comes out of scraped
markup. Add `.allow_private_addresses(true)` to point it at a service on your own machine or
network.

### Crawling

```rust
use std::sync::Arc;

use async_trait::async_trait;
use rustscrapling::response::FollowOptions;
use rustscrapling::{
    CrawlerEngine, EngineOptions, FetcherSession, Output, Response, Result, Session,
    SessionManager, Spider,
};
use serde_json::json;

struct QuotesSpider;

#[async_trait]
impl Spider for QuotesSpider {
    fn name(&self) -> &str { "quotes" }

    fn start_urls(&self) -> Vec<String> {
        vec!["https://quotes.toscrape.com/".to_string()]
    }

    fn allowed_domains(&self) -> Vec<String> {
        vec!["quotes.toscrape.com".to_string()]
    }

    async fn parse(&self, response: Response) -> Result<Vec<Output>> {
        let mut output = Vec::new();
        for quote in response.css("div.quote")?.iter() {
            output.push(Output::Item(json!({
                "text": quote.css("span.text::text")?.get().map(|t| t.into_string()),
                "author": quote.css("small.author::text")?.get().map(|t| t.into_string()),
            })));
        }
        if let Some(next) = response.css("li.next a::attr(href)")?.get() {
            output.push(Output::Request(response.follow(next.as_str(), FollowOptions::default())?));
        }
        Ok(output)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut sessions = SessionManager::new();
    sessions.add_default("http", Session::Http(FetcherSession::new()?))?;

    let engine = CrawlerEngine::new(
        Arc::new(QuotesSpider),
        Arc::new(sessions),
        EngineOptions::default(),
    );
    let result = engine.crawl().await?;

    println!("{} items in {:.1}s", result.len(), result.stats.elapsed_seconds());
    result.items.to_json("quotes.json", true)?;
    Ok(())
}
```

Runnable versions of all four live in [`examples/`](examples): `parse.rs`, `adaptive.rs`,
`fetch.rs` and `spider.rs`.

`allowed_domains` is compared against `Request::domain()`, which is the URL's host plus its
port when the URL carries a non-default one — so a crawl of `http://localhost:8000/` has to
allow `"localhost:8000"`, not `"localhost"`.

## Compared to the Python API

Rust has no keyword arguments and no `isinstance` dispatch, so anything Python expresses with
`**kwargs` or with "pass a string, a list, a dict, a regex or a function" becomes a builder
struct or an enum. Everything fallible returns `Result<T, rustscrapling::Error>`.

### Parsing

| Scrapling (Python)                                   | rustscrapling (Rust)                                              |
|------------------------------------------------------|-------------------------------------------------------------------|
| `Selector(html)`                                      | `Selector::new(html)?`                                            |
| `Selector(html, url=...)`                             | `Selector::with_url(html, url)?`                                  |
| `page.css('.product')`                                | `page.css(".product")?`                                           |
| `page.css('.product')[0]`                             | `page.css_first(".product")?`                                     |
| `page.css('h1::text').get()`                          | `page.css("h1::text")?.get()`                                     |
| `page.css('a::attr(href)').getall()`                  | `page.css("a::attr(href)")?.getall().getall()`                    |
| `page.xpath('//h1')`                                  | *not ported* — see [Out of scope](#out-of-scope)                  |
| `page.find('section')`                                | `page.find(&Filter::new().tag("section"))?`                       |
| `page.find_all('div', {'class': 'quote'})`            | `page.find_all(&Filter::new().tag("div").attr("class", "quote"))?`|
| `page.find_all(['h3', 'h2'], re.compile('Product'))`  | `page.find_all(&Filter::new().tags(["h3", "h2"]).regex("Product")?)?` |
| `page.find_all('section', {'id*': 'product'})`        | `Filter::new().tag("section").predicate(\|e\| …)`                 |
| `page.find_all('div', lambda e: …)`                   | `Filter::new().tag("div").predicate(\|e\| …)`                     |
| `page.find_by_text('Products', first_match=False)`    | `page.find_by_text("Products", false, false, false, true)`        |
| `page.find_by_regex(r'£[\d.]+')`                      | `page.find_by_regex(r"£[\d.]+", true, false, true)?`              |
| `element.find_similar(ignore_attributes=['title'])`   | `element.find_similar(0.2, &["title"], false)`                    |
| `element.find_ancestor(lambda e: …)`                  | `element.find_ancestor(\|e\| …)`                                  |
| `element.iterancestors()` / `element.path`            | `element.iterancestors()` / `element.path()`                      |
| `element.tag` / `element.text` / `element.attrib`     | `element.tag()` / `element.text()` / `element.attrib()`           |
| `element.attrib['href']`                              | `element.attr("href")` (or `element.attrib()["href"]`)            |
| `element.get_all_text(ignore_tags=('script',))`       | `element.get_all_text("\n", false, &["script"])`                  |
| `element.html_content` / `element.prettify()`         | `element.html_content()` / `element.prettify()`                   |
| `element.has_class('product')`                        | `element.has_class("product")`                                    |
| `element.parent` / `.children` / `.siblings`          | `element.parent()` / `.children()` / `.siblings()`                |
| `element.next` / `element.previous`                   | `element.next()` / `element.previous()`                           |
| `element.generate_css_selector`                       | `element.generate_css_selector()`                                 |
| `page.urljoin(href)`                                  | `page.urljoin(href)?`                                             |

### Text helpers

| Scrapling (Python)                        | rustscrapling (Rust)                                  |
|-------------------------------------------|--------------------------------------------------------|
| `TextHandler` / `TextHandlers`            | `Text` / `Texts`                                       |
| `text.clean()`                            | `text.clean()`                                         |
| `text.re(pattern, replace_entities=True)` | `text.re(pattern, ReOptions::default())?`              |
| `text.re_first(pattern)`                  | `text.re_first(pattern, ReOptions::default())?`        |
| `text.json()`                             | `text.json::<T>()?` / `text.json_value()?`             |
| `texts.getall()` / `texts.get()`          | `texts.getall()` / `texts.first()`                     |
| `AttributesHandler.search_values(k, True)`| `attributes.search_values(k, true)`                    |

### Adaptive selection

| Scrapling (Python)                          | rustscrapling (Rust)                                        |
|---------------------------------------------|-------------------------------------------------------------|
| `page.css('.product', auto_save=True)`      | `adaptive.css_adaptive(".product", "id", true, 40.0)?`      |
| `page.css('.product', adaptive=True)`       | `adaptive.css_adaptive(".product", "id", false, 40.0)?`     |
| `_StorageTools.element_to_dict(element)`    | `fingerprint(&element)`                                     |
| `Selector.relocate(...)`                    | `relocate(&root, &fingerprint, 40.0)`                       |
| `SQLiteStorageSystem`                       | `SqliteStorage` (same table, files are interchangeable)     |
| a custom `storage` class                    | your own `impl Storage`                                     |

### Fetching

| Scrapling (Python)                                  | rustscrapling (Rust)                                       |
|-----------------------------------------------------|-------------------------------------------------------------|
| `Fetcher.get(url)` / `AsyncFetcher.get(url)`         | `fetcher.get(url).send().await?`                            |
| `Fetcher.post(url, data={...})`                      | `fetcher.post(url).form([...]).send().await?`               |
| `Fetcher.put(url, ...)` / `Fetcher.delete(url)`      | `fetcher.put(url)…` / `fetcher.delete(url)…`                |
| `stealthy_headers=True`                              | `Fetcher::builder().stealthy_headers(true)`                 |
| `impersonate="chrome"`                               | `.impersonate(BrowserProfile::Chrome)`                      |
| `proxy="http://…"`                                   | `.proxy("http://…")` or `.proxy_rotator(rotator)`           |
| `FetcherSession()`                                   | `FetcherSession::new()?`                                    |
| `page.status` / `.reason` / `.headers` / `.cookies`  | `response.status` / `.reason` / `.headers` / `.cookies`     |
| `page.body` / `.encoding` / `.history` / `.meta`     | `response.body` / `.encoding` / `.history` / `.meta`        |
| `DynamicFetcher.fetch(url, …)`                       | `DynamicFetcher::fetch(url, options).await?` (`browser`)    |
| `StealthyFetcher.fetch(url, solve_cloudflare=True)`  | *not ported* — see [Out of scope](#out-of-scope)            |

### Spiders

| Scrapling (Python)                            | rustscrapling (Rust)                                     |
|-----------------------------------------------|-----------------------------------------------------------|
| `class MySpider(Spider)`                      | `impl Spider for MySpider`                                |
| `name` / `start_urls` / `allowed_domains`     | `fn name()` / `fn start_urls()` / `fn allowed_domains()`  |
| `concurrent_requests`, `download_delay`, …    | `fn config() -> SpiderConfig`                             |
| `async def parse(self, response)` + `yield`   | `async fn parse(&self, response) -> Result<Vec<Output>>`  |
| `yield {...}` / `yield response.follow(...)`  | `Output::Item(json!({...}))` / `Output::Request(request)` |
| `response.follow(url, callback=self.parse)`   | `response.follow(url, FollowOptions::default())?`         |
| a second callback method                      | `Callback::Named("detail")` + `fn callback(name, …)`      |
| `MySpider().start()`                          | `CrawlerEngine::new(…).crawl().await?` or `run(spider)`   |
| `result.items` / `result.stats`               | `result.items` / `result.stats`                           |
| `result.items.to_json/to_jsonl/to_csv`        | `items.to_json/to_jsonl/to_csv`                           |
| `robots_txt_obey = True`                      | `SpiderConfig::robots_txt_obey`                           |
| `autothrottle_enabled = True`                 | `SpiderConfig::autothrottle = Some(AutoThrottleConfig…)`  |
| pause/resume via `crawldir`                   | `EngineOptions::crawldir` + `CheckpointManager`           |
| `CrawlSpider` + `Rule(LinkExtractor(...))`    | `crawl_rules(&response, &[CrawlRule::new(extractor)])`    |

## Status

**Pre-release.** Every module in `CONTRACT.md` is implemented and covered by tests. The API is
still allowed to change before `0.1.0` is published; `CONTRACT.md` is the source of truth for
it, and its section 9 records what the implementation added on top of the original sections.

| Area                          | State |
|-------------------------------|-------|
| `text`                        | complete — 56 unit tests |
| `parser`                      | complete — 53 unit tests, plus `tests/parser_overview.rs` (22) replaying the whole Python walkthrough |
| `adaptive`                    | complete — 41 unit tests, plus `tests/adaptive.rs` (8) covering the relocation scenario |
| `response`                    | complete — 9 unit tests |
| `http` (feature `http`)       | complete — 40 unit tests, plus `tests/http.rs` (18) against a local `wiremock` server |
| `spider` (feature `spider`)   | complete — 84 unit tests, plus `tests/spider.rs` (15) crawling a `wiremock` mini site end to end |
| `browser` (feature `browser`) | complete but least exercised — 39 unit tests cover the pure parts (launch flags, proxy parsing, request blocking); the one test that drives a real page is `#[ignore]`d, since it needs a local Chromium |

No XPath, no anti-bot solver; see [Out of scope](#out-of-scope).

```sh
cargo test --all-features      # 390 tests, 1 ignored
cargo test                     # default features: http + spider
cargo test --no-default-features   # the parser, adaptive and text layers alone
```

Nothing in the suite reaches the network — the HTTP and crawl tests run against a `wiremock`
server on localhost — and `cargo clippy --all-targets --all-features -- -D warnings` and
`cargo audit` are both clean. Of the examples, `parse` and `adaptive` work offline on embedded
HTML; `fetch` and `spider` take a URL and do go out to it.

## Out of scope

These parts of Scrapling are deliberately not ported:

- **XPath.** The parser is built on [`scraper`](https://crates.io/crates/scraper), which has no
  XPath engine. `xpath()`, `generate_xpath_selector` and friends do not exist; everything the
  Python library expresses in XPath is expressed here with CSS or with the `Filter` builder.
- **`StealthyFetcher` and the Cloudflare solver.** There is no anti-bot challenge solver, no
  Turnstile/Interstitial bypass, no CDP or WebRTC leak patching, and no canvas noise. On a
  browser session `stealth(true)` only applies Chromium command-line flags.
- **XML feeds** as a parsing target, and **XML** as an item export format.
- The Python project's **CLI**, **interactive shell**, **MCP server** and **Markdown/RAG
  conversion**.

## License

MIT. See [LICENSE](LICENSE).

This crate is a port, so parts of it are derived from
[Scrapling](https://github.com/D4Vinci/Scrapling), which is licensed under the BSD 3-Clause
License. Those portions stay under that license; the MIT License covers the original Rust work.
[NOTICE](NOTICE) lists what is derived, reproduces the upstream license text, and records the
lineage of the code Scrapling itself adapted from the Scrapy project. This is an independent
port and is not affiliated with or endorsed by any of those projects.
