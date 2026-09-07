//! The parser walkthrough from Scrapling's `docs/overview.md`, ported one query at a time.
//!
//! The HTML document below is the one used throughout that page, so every assertion here can
//! be diffed against the Python output printed in the documentation. Where the Python API is
//! dynamic (`find_all('section', {'id*': 'product'})`, `find_all(['h3', 'h2'], regex)`) the
//! Rust equivalent goes through the [`Filter`] builder, as `CONTRACT.md` requires.

use rustscrapling::{Error, Filter, ReOptions, Result, Selector, SelectorKind, Selectors};

/// The example document from `docs/overview.md`, verbatim.
const PAGE: &str = r##"<html>
  <head>
    <title>Complex Web Page</title>
    <style>
      .hidden { display: none; }
    </style>
  </head>
  <body>
    <header>
      <nav>
        <ul>
          <li> <a href="#home">Home</a> </li>
          <li> <a href="#about">About</a> </li>
          <li> <a href="#contact">Contact</a> </li>
        </ul>
      </nav>
    </header>
    <main>
      <section id="products" schema='{"jsonable": "data"}'>
        <h2>Products</h2>
        <div class="product-list">
          <article class="product" data-id="1">
            <h3>Product 1</h3>
            <p class="description">This is product 1</p>
            <span class="price">$10.99</span>
            <div class="hidden stock">In stock: 5</div>
          </article>

          <article class="product" data-id="2">
            <h3>Product 2</h3>
            <p class="description">This is product 2</p>
            <span class="price">$20.99</span>
            <div class="hidden stock">In stock: 3</div>
          </article>

          <article class="product" data-id="3">
            <h3>Product 3</h3>
            <p class="description">This is product 3</p>
            <span class="price">$15.99</span>
            <div class="hidden stock">Out of stock</div>
          </article>
        </div>
      </section>

      <section id="reviews">
        <h2>Customer Reviews</h2>
        <div class="review-list">
          <div class="review" data-rating="5">
            <p class="review-text">Great product!</p>
            <span class="reviewer">John Doe</span>
          </div>
          <div class="review" data-rating="4">
            <p class="review-text">Good value for money.</p>
            <span class="reviewer">Jane Smith</span>
          </div>
        </div>
      </section>
    </main>
    <script id="page-data" type="application/json">
      {
        "lastUpdated": "2024-09-22T10:30:00Z",
        "totalProducts": 3
      }
    </script>
  </body>
</html>"##;

/// The URL the fixture pretends to come from, so `urljoin` has something to work with.
const PAGE_URL: &str = "https://example.com/shop/index.html";

/// The text the documentation says `get_all_text(ignore_tags=('script', 'style'))` produces.
const ALL_TEXT: &[&str] = &[
    "Complex Web Page",
    "Home",
    "About",
    "Contact",
    "Products",
    "Product 1",
    "This is product 1",
    "$10.99",
    "In stock: 5",
    "Product 2",
    "This is product 2",
    "$20.99",
    "In stock: 3",
    "Product 3",
    "This is product 3",
    "$15.99",
    "Out of stock",
    "Customer Reviews",
    "Great product!",
    "John Doe",
    "Good value for money.",
    "Jane Smith",
];

/// Parse the fixture, remembering the URL it came from.
fn page() -> Result<Selector> {
    Selector::with_url(PAGE, PAGE_URL)
}

/// Turn a missing value into an [`Error`] instead of panicking.
fn need<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| Error::other(format!("expected to find {what}")))
}

/// The cleaned direct text of every selector in the list.
fn texts(items: &Selectors) -> Vec<String> {
    items
        .iter()
        .map(|item| item.text().clean().into_string())
        .collect()
}

/// The document parses and the head is reachable.
#[test]
fn parses_the_document() -> Result<()> {
    let page = page()?;
    assert_eq!(page.url(), PAGE_URL);
    assert!(page.body().starts_with(b"<html"));

    let title = need(page.css_first("title")?, "the title element")?;
    assert_eq!(title.text().as_str(), "Complex Web Page");
    Ok(())
}

/// `page.get_all_text(ignore_tags=('script', 'style'))`.
#[test]
fn get_all_text_matches_the_documented_output() -> Result<()> {
    let page = page()?;
    let all = page.get_all_text("\n", false, &["script", "style"]);

    // Compared line by line so an implementation that keeps the source indentation of a text
    // node still reads as "the documented output".
    let lines: Vec<String> = all
        .as_str()
        .split('\n')
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, ALL_TEXT);

    // Stripped, the join is exactly the string the documentation prints.
    let stripped = page.get_all_text("\n", true, &["script", "style"]);
    assert_eq!(stripped.as_str(), ALL_TEXT.join("\n"));

    // The ignored tags really are ignored.
    assert!(!all.as_str().contains("display: none"));
    assert!(!all.as_str().contains("totalProducts"));
    Ok(())
}

/// `page.find('section')` and `page.find_all('section')`.
#[test]
fn find_and_find_all_by_tag() -> Result<()> {
    let page = page()?;

    let section = need(
        page.find(&Filter::new().tag("section"))?,
        "the first section",
    )?;
    assert_eq!(section.tag(), "section");
    assert_eq!(need(section.attr("id"), "section id")?.as_str(), "products");

    let sections = page.find_all(&Filter::new().tag("section"))?;
    assert_eq!(sections.len(), 2);
    assert_eq!(sections.length(), 2);
    let ids: Vec<String> = sections
        .iter()
        .filter_map(|s| s.attr("id"))
        .map(|id| id.into_string())
        .collect();
    assert_eq!(ids, vec!["products", "reviews"]);
    Ok(())
}

/// `page.find_all('section', {'id': "products"})` and the `{'id*': "product"}` variant.
#[test]
fn find_all_by_attribute() -> Result<()> {
    let page = page()?;

    let exact = page.find_all(&Filter::new().tag("section").attr("id", "products"))?;
    assert_eq!(exact.len(), 1);

    // Python's `{'id*': "product"}` (attribute *contains*) becomes a predicate here.
    let contains = page.find_all(
        &Filter::new()
            .tag("section")
            .predicate(|element| element.attr("id").is_some_and(|id| id.contains("product"))),
    )?;
    assert_eq!(contains.len(), 1);
    assert_eq!(
        need(contains.first(), "the products section")?.tag(),
        "section"
    );

    // `has_attr` keeps every section, both carry an id.
    let with_id = page.find_all(&Filter::new().tag("section").has_attr("id"))?;
    assert_eq!(with_id.len(), 2);

    // `class` is matched name by name, as in Python.
    let hidden = page.find_all(&Filter::new().attr("class", "hidden"))?;
    assert_eq!(hidden.len(), 3);
    Ok(())
}

/// `page.find_all('h3', re.compile(r'Product \d'))`.
#[test]
fn find_all_by_tag_and_regex() -> Result<()> {
    let page = page()?;

    let filter = Filter::new().tag("h3").regex(r"Product \d")?;
    let found = page.find_all(&filter)?;
    assert_eq!(texts(&found), vec!["Product 1", "Product 2", "Product 3"]);
    Ok(())
}

/// `page.find_all(['h3', 'h2'], re.compile(r'Product'))`.
#[test]
fn find_all_by_several_tags_and_regex() -> Result<()> {
    let page = page()?;

    let filter = Filter::new().tags(["h3", "h2"]).regex("Product")?;
    let found = page.find_all(&filter)?;

    // Python groups its results tag by tag; the order is not part of the contract, so the
    // set of matches is what is checked.
    let mut found = texts(&found);
    found.sort();
    assert_eq!(
        found,
        vec!["Product 1", "Product 2", "Product 3", "Products"]
    );
    Ok(())
}

/// `page.find_by_text('Products', first_match=False)`.
#[test]
fn find_by_text() -> Result<()> {
    let page = page()?;

    let found = page.find_by_text("Products", false, false, false, true);
    assert_eq!(found.len(), 1);
    let heading = need(found.first(), "the Products heading")?;
    assert_eq!(heading.tag(), "h2");
    assert_eq!(heading.text().clean().as_str(), "Products");

    // `first_match = true` stops at the first hit.
    let first = page.find_by_text("Products", true, false, false, true);
    assert_eq!(first.len(), 1);

    // Partial matching finds the three product headings plus the "Products" heading.
    let partial = page.find_by_text("Product", false, true, true, true);
    assert!(partial.len() >= 4);

    // Exact matching is exact: no element's own text is just "Product".
    let exact = page.find_by_text("Product", false, false, true, true);
    assert!(exact.is_empty());
    Ok(())
}

/// `page.find_by_regex(r'Product \d', first_match=False)`.
///
/// The documentation prints only the three `<h3>` elements for that call, but Python's default
/// is `case_sensitive=False`, which also matches `<p>This is product 1</p>`. Both readings are
/// checked below: the documented list with case sensitivity on, and the larger set with it off.
#[test]
fn find_by_regex() -> Result<()> {
    let page = page()?;

    // Case-sensitive, so the `<p>This is product 1</p>` paragraphs stay out of the result.
    let found = page.find_by_regex(r"Product \d", false, true, true)?;
    assert_eq!(texts(&found), vec!["Product 1", "Product 2", "Product 3"]);

    let first = page.find_by_regex(r"Product \d", true, true, true)?;
    assert_eq!(first.len(), 1);
    assert_eq!(
        need(first.first(), "the first product heading")?
            .text()
            .clean()
            .as_str(),
        "Product 1"
    );

    // Case-insensitively the descriptions match too.
    let insensitive = page.find_by_regex(r"Product \d", false, false, true)?;
    assert!(insensitive.len() >= 6);
    Ok(())
}

/// `target_element.find_similar()`.
#[test]
fn find_similar_finds_the_sibling_headings() -> Result<()> {
    let page = page()?;

    let first = page.find_by_regex(r"Product \d", true, true, true)?;
    let target = need(first.first(), "the first product heading")?;

    let similar = target.find_similar(0.2, &["href", "src"], false);
    assert_eq!(texts(&similar), vec!["Product 2", "Product 3"]);

    // The element itself is never part of its own result, exactly like Python.
    assert!(!texts(&similar).contains(&"Product 1".to_string()));

    // The same trick one level up finds the other product cards.
    let card = need(target.parent(), "the article around the heading")?;
    let cards = card.find_similar(0.2, &["href", "src"], false);
    assert_eq!(cards.len(), 2);
    for other in cards.iter() {
        assert_eq!(other.tag(), "article");
        assert!(other.has_class("product"));
    }
    Ok(())
}

/// `page.css('.product-list [data-id="1"]')[0]` and `page.css('.product-list article')`.
#[test]
fn css_selectors() -> Result<()> {
    let page = page()?;

    let first = need(
        page.css_first(r#".product-list [data-id="1"]"#)?,
        "product 1",
    )?;
    assert_eq!(first.tag(), "article");
    assert!(first.has_class("product"));

    let articles = page.css(".product-list article")?;
    assert_eq!(articles.len(), 3);
    assert_eq!(
        articles
            .iter()
            .filter_map(|a| a.attr("data-id"))
            .map(|id| id.into_string())
            .collect::<Vec<_>>(),
        vec!["1", "2", "3"]
    );

    // Comma-separated lists are concatenated in the order the selectors were written.
    let headings = page.css("h3, h2")?;
    assert_eq!(headings.len(), 5);
    assert_eq!(
        texts(&headings)[..3].to_vec(),
        vec!["Product 1", "Product 2", "Product 3"]
    );

    // Nesting and chaining works, as in `page.css('.product')[0].css('h3::text')`.
    let nested = need(articles.first(), "the first article")?.css("h3::text")?;
    assert_eq!(nested.getall().getall(), vec!["Product 1"]);

    // `Selectors::css` runs the selector on every element and flattens the result.
    assert_eq!(articles.css(".price::text")?.getall().getall().len(), 3);
    Ok(())
}

/// The `::text` and `::attr(name)` pseudo-elements.
#[test]
fn text_and_attribute_pseudo_elements() -> Result<()> {
    let page = page()?;

    let prices: Vec<String> = page.css(".product .price::text")?.getall().getall();
    assert_eq!(prices, vec!["$10.99", "$20.99", "$15.99"]);

    let links: Vec<String> = page.css("nav a::attr(href)")?.getall().getall();
    assert_eq!(links, vec!["#home", "#about", "#contact"]);

    // `get()` on the list is the first value.
    let first_price = need(page.css(".price::text")?.get(), "the first price")?;
    assert_eq!(first_price.as_str(), "$10.99");
    Ok(())
}

/// Text and attribute results are `Selector`s that degrade exactly like Python's.
#[test]
fn text_nodes_degrade_like_python() -> Result<()> {
    let page = page()?;

    let text_node = need(page.css_first("h2::text")?, "an h2 text node")?;
    assert!(text_node.is_text_node());
    assert_eq!(text_node.tag(), "#text");
    assert_eq!(text_node.text().as_str(), "Products");
    assert!(text_node.attrib().is_empty());
    assert!(text_node.children().is_empty());
    assert!(text_node.css("h2")?.is_empty());
    assert!(text_node.find_all(&Filter::new().tag("h2"))?.is_empty());
    assert!(!text_node.has_class("product"));
    match text_node.kind() {
        SelectorKind::TextNode(value) => assert_eq!(value.as_str(), "Products"),
        other => return Err(Error::other(format!("expected a text node, got {other:?}"))),
    }

    let attr_node = need(page.css_first("nav a::attr(href)")?, "an href value")?;
    assert!(attr_node.is_text_node());
    assert_eq!(attr_node.text().as_str(), "#home");
    match attr_node.kind() {
        SelectorKind::AttrNode { name, value } => {
            assert_eq!(name.as_str(), "href");
            assert_eq!(value.as_str(), "#home");
        }
        other => {
            return Err(Error::other(format!(
                "expected an attr node, got {other:?}"
            )))
        }
    }
    Ok(())
}

/// The "Accessing elements' data" section.
#[test]
fn element_data() -> Result<()> {
    let page = page()?;
    let section = need(page.css_first("#products")?, "the products section")?;

    assert_eq!(section.tag(), "section");

    let attributes = section.attrib();
    assert_eq!(attributes.len(), 2);
    assert_eq!(attributes.keys().collect::<Vec<_>>(), vec!["id", "schema"]);
    assert_eq!(need(attributes.get("id"), "the id")?.as_str(), "products");
    assert!(attributes.contains_key("schema"));

    // `section_element.attrib['schema'].json()`
    let schema = need(attributes.get("schema"), "the schema attribute")?.json_value()?;
    assert_eq!(schema["jsonable"], "data");

    // The attribute map serializes to a JSON object.
    let json_string = attributes.json_string()?;
    assert!(json_string.contains("\"id\""));

    // `section_element.text` is the direct text only, which here is whitespace.
    assert_eq!(section.text().clean().as_str(), "");

    // `section_element.get_all_text()`
    let all = section.get_all_text("\n", true, &["script", "style"]);
    assert_eq!(all.as_str(), ALL_TEXT[4..17].join("\n"));

    // `section_element.html_content` and `.prettify()`
    let html = section.html_content();
    assert!(html.as_str().starts_with("<section"));
    assert!(html.as_str().contains("<h2>Products</h2>"));
    assert!(section.prettify().as_str().contains("Product 1"));

    // `section_element.path`
    let path = section.path();
    assert!(path.len() >= 3);
    assert_eq!(
        path.iter()
            .take(3)
            .map(|p| p.tag().to_string())
            .collect::<Vec<_>>(),
        vec!["main", "body", "html"]
    );

    // `section_element.generate_css_selector`
    assert_eq!(section.generate_css_selector(), "#products");
    assert!(!section.generate_full_css_selector().is_empty());

    // A selector generated for an element without an id still finds it again.
    let article = need(
        page.css_first(".product-list article")?,
        "the first article",
    )?;
    let generated = article.generate_css_selector();
    assert!(!generated.is_empty());
    assert!(!page.css(&generated)?.is_empty());
    Ok(())
}

/// The "Navigation" section.
#[test]
fn navigation() -> Result<()> {
    let page = page()?;
    let section = need(page.css_first("#products")?, "the products section")?;

    let parent = need(section.parent(), "the section's parent")?;
    assert_eq!(parent.tag(), "main");
    assert_eq!(need(parent.parent(), "main's parent")?.tag(), "body");

    let children = section.children();
    assert_eq!(children.len(), 2);
    assert_eq!(
        children
            .iter()
            .map(|c| c.tag().to_string())
            .collect::<Vec<_>>(),
        vec!["h2", "div"]
    );
    assert_eq!(
        children.css("h2::text")?.getall().getall(),
        vec!["Products"]
    );

    let siblings = section.siblings();
    assert_eq!(siblings.len(), 1);
    assert_eq!(
        need(siblings.first(), "the reviews section")?
            .attr("id")
            .map(|id| id.into_string()),
        Some("reviews".to_string())
    );

    let next = need(section.next(), "the next section")?;
    assert_eq!(
        need(next.attr("id"), "the next section's id")?.as_str(),
        "reviews"
    );
    let previous = need(next.previous(), "the previous section")?;
    assert_eq!(
        need(previous.attr("id"), "the previous section's id")?.as_str(),
        "products"
    );

    // `page.css('[data-id="1"]')[0].has_class('product')`
    let article = need(page.css_first(r#"[data-id="1"]"#)?, "product 1")?;
    assert!(article.has_class("product"));
    assert!(!article.has_class("review"));

    // `for ancestor in section_element.iterancestors()`
    let ancestors: Vec<String> = section
        .iterancestors()
        .map(|a| a.tag().to_string())
        .collect();
    assert!(ancestors.len() >= 3);
    assert_eq!(ancestors[..3].to_vec(), vec!["main", "body", "html"]);

    // `section_element.find_ancestor(lambda ancestor: ancestor.css('nav'))`
    let with_nav = need(
        section
            .find_ancestor(|ancestor| ancestor.css("nav").map(|n| !n.is_empty()).unwrap_or(false)),
        "an ancestor that contains the nav",
    )?;
    assert_eq!(with_nav.tag(), "body");

    // Everything below the section, in document order.
    let below = section.below_elements();
    assert!(below.len() >= 14);
    assert_eq!(need(below.first(), "the first descendant")?.tag(), "h2");
    Ok(())
}

/// `page.urljoin(...)`, used on the relative links of a fetched page.
#[test]
fn urljoin_uses_the_document_url() -> Result<()> {
    let page = page()?;
    assert_eq!(
        page.urljoin("catalogue/product.html")?,
        "https://example.com/shop/catalogue/product.html"
    );
    assert_eq!(page.urljoin("/cart")?, "https://example.com/cart");
    assert_eq!(
        page.urljoin("https://other.example/x")?,
        "https://other.example/x"
    );
    Ok(())
}

/// The regex helpers on a node and on a list of nodes.
#[test]
fn regex_helpers() -> Result<()> {
    let page = page()?;

    let price = need(page.css_first(".price")?, "the first price")?;
    let amount = need(
        price.re_first(r"[\d.]+", ReOptions::default())?,
        "an amount",
    )?;
    assert_eq!(amount.as_str(), "10.99");

    let all: Vec<String> = page
        .css(".price")?
        .re(r"[\d.]+", ReOptions::default())?
        .getall();
    assert_eq!(all, vec!["10.99", "20.99", "15.99"]);

    let first = need(
        page.css(".price")?
            .re_first(r"[\d.]+", ReOptions::default())?,
        "the first amount",
    )?;
    assert_eq!(first.as_str(), "10.99");

    // Capture groups come back flattened, as Python's `findall` does.
    let stock = need(page.css_first(".stock")?, "the first stock line")?;
    let count = stock.re(r"In stock: (\d+)", ReOptions::default())?;
    assert_eq!(count.getall(), vec!["5"]);
    Ok(())
}

/// The `<script type="application/json">` payload at the bottom of the page.
#[test]
fn json_payload() -> Result<()> {
    #[derive(serde::Deserialize)]
    struct PageData {
        #[serde(rename = "lastUpdated")]
        last_updated: String,
        #[serde(rename = "totalProducts")]
        total_products: u32,
    }

    let page = page()?;
    let script = need(page.css_first("script#page-data")?, "the page-data script")?;

    let value = script.text().json_value()?;
    assert_eq!(value["totalProducts"], 3);

    let data: PageData = script.json()?;
    assert_eq!(data.total_products, 3);
    assert_eq!(data.last_updated, "2024-09-22T10:30:00Z");
    Ok(())
}

/// The list helpers on `Selectors`.
#[test]
fn selectors_helpers() -> Result<()> {
    let page = page()?;
    let articles = page.css(".product-list article")?;

    assert!(!articles.is_empty());
    assert_eq!(articles.len(), articles.length());
    assert_eq!(
        need(articles.first(), "the first article")?
            .attr("data-id")
            .map(|id| id.into_string()),
        Some("1".to_string())
    );
    assert_eq!(
        need(articles.last(), "the last article")?
            .attr("data-id")
            .map(|id| id.into_string()),
        Some("3".to_string())
    );

    let third = need(
        articles.search(|article| article.attr("data-id").is_some_and(|id| id.as_str() == "3")),
        "the third article",
    )?;
    assert!(third
        .get_all_text("\n", true, &[])
        .as_str()
        .contains("Out of stock"));

    let in_stock = articles.filter(|article| {
        article
            .css(".stock")
            .map(|stock| {
                stock
                    .getall()
                    .iter()
                    .any(|html| html.as_str().contains("In stock"))
            })
            .unwrap_or(false)
    });
    assert_eq!(in_stock.len(), 2);

    // Indexing and iteration.
    assert_eq!(articles[0].tag(), "article");
    assert_eq!(articles.iter().count(), 3);
    assert_eq!(articles.clone().into_vec().len(), 3);

    // `get()` / `getall()` serialize the elements themselves.
    let serialized = need(articles.get(), "the first article's html")?;
    assert!(serialized.as_str().starts_with("<article"));
    assert_eq!(articles.getall().len(), 3);
    Ok(())
}

/// The review block used by the "extract reviews" example in `docs/parsing/selection.md`.
#[test]
fn review_extraction_example() -> Result<()> {
    let page = page()?;

    let first_review = page.find_by_text("Great product!", true, false, false, true);
    let anchor = need(first_review.first(), "the first review paragraph")?;
    let container = need(
        anchor.find_ancestor(|element| element.has_class("review")),
        "the review container",
    )?;

    // `find_similar` never returns the element it was called on — in Python either — so the
    // container has to be put back in front of its own matches to get both reviews.
    let mut reviews = vec![container.clone()];
    reviews.extend(
        container
            .find_similar(0.2, &["href", "src"], false)
            .into_vec(),
    );
    assert_eq!(reviews.len(), 2);

    let extracted: Vec<(String, String, String)> = reviews
        .iter()
        .map(|review| {
            let text = review
                .css(".review-text::text")
                .ok()
                .and_then(|found| found.get())
                .map(|value| value.into_string())
                .unwrap_or_default();
            let author = review
                .css(".reviewer::text")
                .ok()
                .and_then(|found| found.get())
                .map(|value| value.into_string())
                .unwrap_or_default();
            let rating = review
                .attr("data-rating")
                .map(|value| value.into_string())
                .unwrap_or_default();
            (text, author, rating)
        })
        .collect();

    assert_eq!(
        extracted,
        vec![
            (
                "Great product!".to_string(),
                "John Doe".to_string(),
                "5".to_string()
            ),
            (
                "Good value for money.".to_string(),
                "Jane Smith".to_string(),
                "4".to_string()
            ),
        ]
    );
    Ok(())
}

/// A fragment parses without the implied `<html>`/`<body>` wrapper getting in the way.
#[test]
fn fragments_parse() -> Result<()> {
    let fragment = Selector::fragment(r#"<div class="a"><span>hello</span></div>"#)?;
    assert_eq!(
        need(fragment.css_first("span")?, "the span")?
            .text()
            .as_str(),
        "hello"
    );
    assert_eq!(fragment.url(), "");
    Ok(())
}

/// A bad selector is an error, never a panic.
#[test]
fn invalid_selectors_are_errors() -> Result<()> {
    let page = page()?;
    let error = page.css("div[");
    assert!(matches!(error, Err(Error::Selector { .. })));

    let bad_regex = Filter::new().regex("(");
    assert!(bad_regex.is_err());
    Ok(())
}

/// Malformed input still parses; html5-style parsing never fails on garbage.
#[test]
fn malformed_html_does_not_panic() -> Result<()> {
    let page = Selector::new("<div><p>unclosed<span>deep</div>&amp;<<<")?;
    assert!(!page.css("p")?.is_empty());
    assert!(page
        .get_all_text("\n", true, &[])
        .as_str()
        .contains("unclosed"));
    Ok(())
}
