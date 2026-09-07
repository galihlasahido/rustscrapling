//! Parsing a page: CSS, filters, text search, and finding similar elements.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example parse
//! ```

use rustscrapling::{Filter, ReOptions, Result, Selector};

/// The example document from Scrapling's documentation overview.
const PAGE: &str = r#"<html>
  <head><title>Complex Web Page</title></head>
  <body>
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
    </main>
  </body>
</html>"#;

fn main() -> Result<()> {
    // Parse, remembering where the document came from so `urljoin` works.
    let page = Selector::with_url(PAGE, "https://example.com/shop/index.html")?;

    // Everything on the page as text, scripts and styles left out.
    println!("--- all text ---");
    println!("{}", page.get_all_text("\n", true, &["script", "style"]));

    // CSS, including the `::text` and `::attr(name)` pseudo-elements.
    println!("\n--- css ---");
    for product in page.css(".product-list article")?.iter() {
        let name = product
            .css("h3::text")?
            .get()
            .map(|text| text.into_string())
            .unwrap_or_default();
        let price = product
            .css(".price")?
            .re_first(r"[\d.]+", ReOptions::default())?
            .map(|text| text.into_string())
            .unwrap_or_default();
        let id = product
            .attr("data-id")
            .map(|value| value.into_string())
            .unwrap_or_default();
        let in_stock = product
            .get_all_text("\n", true, &[])
            .as_str()
            .contains("In stock");
        println!("{id}: {name} costs {price} (in stock: {in_stock})");
    }

    // Filters replace Python's `find_all(*args, **kwargs)`.
    println!("\n--- filters ---");
    let headings = page.find_all(&Filter::new().tag("h3").regex(r"Product \d")?)?;
    println!("{} headings match `Product \\d`", headings.len());

    let sections = page.find_all(
        &Filter::new()
            .tag("section")
            .predicate(|element| element.attr("id").is_some_and(|id| id.contains("product"))),
    )?;
    println!(
        "{} sections have an id containing `product`",
        sections.len()
    );

    // Text and regex search.
    println!("\n--- text search ---");
    let first = page.find_by_regex(r"Product \d", true, true, true)?;
    if let Some(target) = first.first() {
        println!("found {} by regex", target.text().clean());

        // Everything that looks like it: the sibling headings.
        for similar in target.find_similar(0.2, &["href", "src"], false).iter() {
            println!("  similar: {}", similar.text().clean());
        }

        // Walk up to the card that contains it.
        if let Some(card) = target.find_ancestor(|element| element.has_class("product")) {
            println!("  inside: {}", card.generate_css_selector());
        }
    }

    // Attribute values can be JSON.
    println!("\n--- attributes ---");
    if let Some(section) = page.css_first("#products")? {
        let attributes = section.attrib();
        println!("{}", attributes.json_string()?);
        if let Some(schema) = attributes.get("schema") {
            println!("schema.jsonable = {}", schema.json_value()?["jsonable"]);
        }
        println!("path: {} ancestors", section.path().len());
        println!("selector: {}", section.generate_css_selector());
    }

    // Relative links resolve against the document URL.
    println!("\n--- urls ---");
    println!("{}", page.urljoin("catalogue/product-1.html")?);
    Ok(())
}
