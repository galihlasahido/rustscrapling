//! Adaptive selection: remember an element today, find it again after the site is redesigned.
//!
//! This is the Rust shape of Scrapling's `page.css('.product', auto_save=True)` followed by
//! `page.css('.product', adaptive=True)` on a later run.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example adaptive
//! ```

use std::sync::Arc;

use rustscrapling::{
    fingerprint, relocate, similarity_score, Adaptive, Result, Selector, SqliteStorage,
};

/// The shop as it looks today. The scraper is written against these class names.
const BEFORE: &str = r#"<html><body>
  <main>
    <section id="products">
      <h2>Products</h2>
      <div class="product-list">
        <article class="product" data-id="1">
          <h3>Product 1</h3>
          <span class="price">$10.99</span>
        </article>
        <article class="product" data-id="2">
          <h3>Product 2</h3>
          <span class="price">$20.99</span>
        </article>
      </div>
    </section>
  </main>
</body></html>"#;

/// The same shop after a redesign: new class names, a new section id, an extra attribute.
/// Every selector written against `BEFORE` is now broken, but the shape of the page is not.
const AFTER: &str = r#"<html><body>
  <main>
    <section id="catalogue">
      <h2>Products</h2>
      <div class="grid products">
        <article class="card product-card" data-id="1" data-sku="P1">
          <h3>Product 1</h3>
          <span class="amount">$10.99</span>
        </article>
        <article class="card product-card" data-id="2" data-sku="P2">
          <h3>Product 2</h3>
          <span class="amount">$20.99</span>
        </article>
      </div>
    </section>
  </main>
</body></html>"#;

/// The selector the scraper was originally written with.
const SELECTOR: &str = ".product-list article.product";
/// The name the element is remembered under; an empty one would key on `SELECTOR` itself.
const IDENTIFIER: &str = "product-card";
/// The site both documents belong to. Storage is keyed by the registrable domain, so every
/// page of one shop shares its remembered elements.
const URL: &str = "https://example.com/shop/";

fn main() -> Result<()> {
    // `SqliteStorage::open("elements.db")` would keep the fingerprints between runs; an
    // in-memory database keeps this example from leaving a file behind. The `Arc` is what
    // lets both wrappers below share one database — `Storage` is implemented for `Arc<S>`.
    let storage = Arc::new(SqliteStorage::in_memory()?);

    // --- first run: the selector still works, so remember what it matched ----------------
    let today = Adaptive::new(Selector::with_url(BEFORE, URL)?, Arc::clone(&storage));
    let found = today.css_adaptive(SELECTOR, IDENTIFIER, true, 40.0)?;
    println!("first run: {} products matched `{SELECTOR}`", found.len());

    let Some(stored) = today.retrieve(IDENTIFIER)? else {
        println!("nothing was stored, so there is nothing to relocate later");
        return Ok(());
    };
    println!(
        "remembered a <{}> with attributes {:?}, {} ancestors deep",
        stored.tag,
        stored.attributes,
        stored.path.len().saturating_sub(1)
    );

    // --- second run: the site changed, the selector no longer matches --------------------
    let tomorrow = Adaptive::new(Selector::with_url(AFTER, URL)?, Arc::clone(&storage));
    println!(
        "\nsecond run: `{SELECTOR}` now matches {} elements",
        tomorrow.root().css(SELECTOR)?.len()
    );

    // With `auto_save` off, a failed selector falls back to the stored fingerprint and
    // relocates the element by similarity instead of breaking the scraper.
    let relocated = tomorrow.css_adaptive(SELECTOR, IDENTIFIER, false, 40.0)?;
    for element in relocated.iter() {
        let name = element
            .css("h3::text")?
            .get()
            .map(|text| text.clean().into_string())
            .unwrap_or_default();
        println!(
            "relocated <{}> class={:?} -> {name}",
            element.tag(),
            element.attr("class").map(|value| value.into_string())
        );
    }

    // The same machinery is available piece by piece, without the `Adaptive` wrapper.
    println!("\n--- scoring by hand ---");
    let before = Selector::with_url(BEFORE, URL)?;
    let after = Selector::with_url(AFTER, URL)?;
    let Some(original) = before.css_first(SELECTOR)? else {
        return Ok(());
    };
    let print = fingerprint(&original);

    for candidate in after.css("article")?.iter() {
        let id = candidate
            .attr("data-id")
            .map(|value| value.into_string())
            .unwrap_or_default();
        println!(
            "data-id={id}: score {:.2}",
            similarity_score(&print, candidate)
        );
    }

    // `relocate` keeps only the best-scoring elements, and only at or above the percentage
    // given; nothing reaches 100% on a redesigned page.
    println!(
        "relocate kept {} element(s) at 40%, {} at 100%",
        relocate(&after, &print, 40.0).len(),
        relocate(&after, &print, 100.0).len()
    );
    Ok(())
}
