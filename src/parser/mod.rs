//! HTML parsing: [`Selector`], [`Selectors`] and the [`Filter`] builder.
//!
//! This module is the Rust port of Python Scrapling's `scrapling/parser.py`,
//! `scrapling/core/translator.py` and `scrapling/core/mixins.py`. It is backed by the
//! [`scraper`] crate: a document is parsed once into a [`scraper::Html`], which is then shared
//! by every [`Selector`] through an [`std::sync::Arc`], so cloning a [`Selector`] or a
//! [`Selectors`] list only copies an `Arc` pointer and an `ego_tree::NodeId`.
//!
//! # What you get
//!
//! * CSS querying with the two non-standard pseudo-elements Scrapling inherits from
//!   Scrapy/Parsel — `::text` and `::attr(name)` — plus comma-separated selector lists.
//! * Filter-based searching ([`Filter`] replaces Python's `find_all(*args, **kwargs)`).
//! * Text- and regex-based searching (`find_by_text`, `find_by_regex`).
//! * Structural similarity searching (`find_similar`).
//! * Selector generation (`generate_css_selector`, `generate_full_css_selector`).
//!
//! ```no_run
//! use rustscrapling::{Filter, Selector};
//!
//! # fn main() -> rustscrapling::Result<()> {
//! let page = Selector::new("<h1>Hello</h1>")?;
//! assert_eq!(page.css("h1::text")?.get().map(|t| t.into_string()), Some("Hello".to_string()));
//! assert_eq!(page.find_all(&Filter::new().tag("h1"))?.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! XPath is deliberately out of scope: `scraper` ships no XPath engine, so everything Python
//! expresses with XPath is expressed here with CSS or with [`Filter`].
//!
//! # Two things to know about whitespace and threads
//!
//! * Python parses with lxml's `remove_blank_text=True`, so a run of pure whitespace is not a
//!   text node there at all. `scraper` keeps those runs, so this module drops them itself:
//!   `::text` never yields one, [`Selector::text`] reads a blank run as `""`, and
//!   [`Selector::get_all_text`] skips them.
//! * A [`Selector`] shares its parsed document through an `Arc` and is `Send + Sync`, so it can
//!   be moved into a `tokio` task — which the crawl engine relies on, because
//!   [`crate::Response`] owns one. Getting there needs `scraper`'s `atomic` feature (enabled in
//!   `Cargo.toml`) plus the single reviewed `unsafe impl Sync` in `selector.rs`, whose safety
//!   argument is written out at the impl. [`Filter`] is `Send + Sync` too.

mod css;
mod filter;
mod generate;
mod selector;
mod selectors;

pub use crate::parser::filter::Filter;
pub use crate::parser::selector::{Selector, SelectorKind};
pub use crate::parser::selectors::Selectors;

/// The crawl engine moves a [`crate::Response`] — and therefore a [`Selector`] — between
/// `tokio` tasks, so a regression here has to fail as one readable error naming the type rather
/// than as a page of `async_trait` diagnostics further downstream.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Selector>();
    assert_send_sync::<Selectors>();
    assert_send_sync::<SelectorKind>();
    assert_send_sync::<Filter>();
};

/// The HTML document used by the unit tests of this module. It is the example document from
/// `Scrapling/docs/overview.md`, so the Rust results can be diffed against the documented
/// Python output.
#[cfg(test)]
pub(crate) const OVERVIEW_HTML: &str = r##"<html>
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
