//! The adaptive layer: fingerprint an element on one version of a page, then find it again
//! after the site has been redesigned.
//!
//! This is the Rust shape of Scrapling's `page.css('.product', auto_save=True)` followed by
//! `page.css('.product', adaptive=True)` on the next run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rustscrapling::{
    fingerprint, relocate, similarity_score, Adaptive, ElementFingerprint, Error, Result, Selector,
    SqliteStorage, Storage,
};

/// The page as it looks today.
const BEFORE: &str = r#"<html>
  <body>
    <main>
      <section id="products">
        <h2>Products</h2>
        <div class="product-list">
          <article class="product" data-id="1">
            <h3>Product 1</h3>
            <p class="description">This is product 1</p>
            <span class="price">$10.99</span>
          </article>
          <article class="product" data-id="2">
            <h3>Product 2</h3>
            <p class="description">This is product 2</p>
            <span class="price">$20.99</span>
          </article>
          <article class="product" data-id="3">
            <h3>Product 3</h3>
            <p class="description">This is product 3</p>
            <span class="price">$15.99</span>
          </article>
        </div>
      </section>
    </main>
  </body>
</html>"#;

/// The same page after a redesign: the class names and the wrapper changed, the shape of the
/// document did not. Every CSS selector written against `BEFORE` is now broken.
const AFTER: &str = r#"<html>
  <body>
    <main>
      <section id="catalogue">
        <h2>Products</h2>
        <div class="grid products">
          <article class="card product-card" data-id="1" data-sku="P1">
            <h3>Product 1</h3>
            <p class="description">This is product 1</p>
            <span class="amount">$10.99</span>
          </article>
          <article class="card product-card" data-id="2" data-sku="P2">
            <h3>Product 2</h3>
            <p class="description">This is product 2</p>
            <span class="amount">$20.99</span>
          </article>
          <article class="card product-card" data-id="3" data-sku="P3">
            <h3>Product 3</h3>
            <p class="description">This is product 3</p>
            <span class="amount">$15.99</span>
          </article>
        </div>
      </section>
    </main>
  </body>
</html>"#;

/// The selector the scraper was originally written with.
const SELECTOR: &str = ".product-list article.product";
/// The name the element is remembered under.
const IDENTIFIER: &str = "product-card";
/// The site the two documents belong to.
const URL: &str = "https://example.com/shop";

/// Turn a missing value into an [`Error`] instead of panicking.
fn need<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| Error::other(format!("expected to find {what}")))
}

/// A [`Storage`] that keeps everything in memory, shared between clones.
///
/// It doubles as a check that the trait is implementable outside the crate.
#[derive(Debug, Clone, Default)]
struct MemoryStorage {
    entries: Arc<Mutex<HashMap<(String, String), ElementFingerprint>>>,
}

impl MemoryStorage {
    /// How many fingerprints are stored.
    fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }
}

impl Storage for MemoryStorage {
    fn save(&self, url: &str, identifier: &str, fingerprint: &ElementFingerprint) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|error| Error::Storage(error.to_string()))?;
        entries.insert(
            (url.to_string(), identifier.to_string()),
            fingerprint.clone(),
        );
        Ok(())
    }

    fn retrieve(&self, url: &str, identifier: &str) -> Result<Option<ElementFingerprint>> {
        let entries = self
            .entries
            .lock()
            .map_err(|error| Error::Storage(error.to_string()))?;
        Ok(entries
            .get(&(url.to_string(), identifier.to_string()))
            .cloned())
    }
}

/// `fingerprint` mirrors `_StorageTools.element_to_dict` field for field.
#[test]
fn fingerprint_describes_the_element_and_its_surroundings() -> Result<()> {
    let page = Selector::with_url(BEFORE, URL)?;
    let article = need(page.css_first(SELECTOR)?, "the first product")?;

    let captured = fingerprint(&article);
    assert_eq!(captured.tag, "article");
    assert_eq!(
        captured.attributes.get("class").map(String::as_str),
        Some("product")
    );
    assert_eq!(
        captured.attributes.get("data-id").map(String::as_str),
        Some("1")
    );
    assert_eq!(captured.parent_name.as_deref(), Some("div"));
    assert_eq!(
        captured.parent_attribs.get("class").map(String::as_str),
        Some("product-list")
    );
    assert_eq!(captured.children, vec!["h3", "p", "span"]);
    assert_eq!(captured.siblings, vec!["article", "article"]);
    assert_eq!(
        captured.path.last().map(String::as_str),
        Some("article"),
        "the path ends at the element itself"
    );
    assert!(captured.path.contains(&"body".to_string()));

    // The fingerprint round-trips through JSON, which is how it reaches SQLite.
    let json = serde_json::to_string(&captured)?;
    let restored: ElementFingerprint = serde_json::from_str(&json)?;
    assert_eq!(restored, captured);
    Ok(())
}

/// `similarity_score` is 100 for the element itself and lower for anything else.
#[test]
fn similarity_score_ranks_candidates() -> Result<()> {
    let page = Selector::with_url(BEFORE, URL)?;
    let article = need(page.css_first(SELECTOR)?, "the first product")?;
    let captured = fingerprint(&article);

    let itself = similarity_score(&captured, &article);
    assert!(
        itself >= 99.0,
        "an element must match its own fingerprint, got {itself}"
    );

    let products = page.css(SELECTOR)?;
    let sibling = need(products.iter().nth(1).cloned(), "the second product")?;
    let sibling_score = similarity_score(&captured, &sibling);
    assert!(
        sibling_score > 40.0 && sibling_score < itself,
        "a sibling should score high but below the original, got {sibling_score}"
    );

    let heading = need(page.css_first("h2")?, "the section heading")?;
    let heading_score = similarity_score(&captured, &heading);
    assert!(
        heading_score < sibling_score,
        "an unrelated element should score lower, got {heading_score}"
    );

    // Scores stay inside the documented range.
    for score in [itself, sibling_score, heading_score] {
        assert!(
            (0.0..=100.0).contains(&score),
            "score out of range: {score}"
        );
    }
    Ok(())
}

/// `relocate` finds the element again on the redesigned page.
#[test]
fn relocate_finds_the_element_after_a_redesign() -> Result<()> {
    let before = Selector::with_url(BEFORE, URL)?;
    let article = need(before.css_first(SELECTOR)?, "the first product")?;
    let captured = fingerprint(&article);

    let after = Selector::with_url(AFTER, URL)?;
    assert!(
        after.css(SELECTOR)?.is_empty(),
        "the original selector must be broken on the new page"
    );

    let found = relocate(&after, &captured, 40.0);
    assert!(!found.is_empty(), "relocation found nothing");
    for element in found.iter() {
        assert_eq!(element.tag(), "article");
        assert!(element.has_class("product-card"));
    }

    // A threshold nobody can reach returns nothing rather than failing.
    assert!(relocate(&after, &captured, 100.0).len() <= found.len());
    Ok(())
}

/// The full `auto_save` / `adaptive` round trip, over a SQLite file that survives the run.
#[test]
fn sqlite_backed_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("adaptive.db");

    // First run: the selector still works, so the element is remembered.
    {
        let storage = SqliteStorage::open(&path)?;
        let adaptive = Adaptive::new(Selector::with_url(BEFORE, URL)?, storage);
        let found = adaptive.css_adaptive(SELECTOR, IDENTIFIER, true, 40.0)?;
        assert_eq!(found.len(), 3);
        assert_eq!(adaptive.root().url(), URL);

        let stored = need(adaptive.retrieve(IDENTIFIER)?, "the stored fingerprint")?;
        assert_eq!(stored.tag, "article");
    }

    // Second run, new process, same database file: the selector is broken and the element is
    // relocated by similarity instead.
    {
        let storage = SqliteStorage::open(&path)?;
        let adaptive = Adaptive::new(Selector::with_url(AFTER, URL)?, storage);
        assert!(adaptive.root().css(SELECTOR)?.is_empty());

        let relocated = adaptive.css_adaptive(SELECTOR, IDENTIFIER, false, 40.0)?;
        assert!(!relocated.is_empty(), "the element was not relocated");
        let first = need(relocated.first(), "the relocated element")?;
        assert_eq!(first.tag(), "article");
        assert!(first
            .get_all_text("\n", true, &[])
            .as_str()
            .contains("Product"));
    }

    // Nothing was ever stored under this name, so there is nothing to fall back to.
    {
        let storage = SqliteStorage::open(&path)?;
        let adaptive = Adaptive::new(Selector::with_url(AFTER, URL)?, storage);
        assert!(adaptive.retrieve("never-saved")?.is_none());
        assert!(adaptive
            .css_adaptive(".gone", "never-saved", false, 40.0)?
            .is_empty());
    }
    Ok(())
}

/// An in-memory database behaves the same way, and `save`/`retrieve` are usable directly.
#[test]
fn in_memory_storage_saves_and_retrieves() -> Result<()> {
    let storage = SqliteStorage::in_memory()?;
    let page = Selector::with_url(BEFORE, URL)?;
    let article = need(page.css_first(SELECTOR)?, "the first product")?;

    let captured = fingerprint(&article);
    storage.save(URL, IDENTIFIER, &captured)?;
    let restored = need(storage.retrieve(URL, IDENTIFIER)?, "the stored fingerprint")?;
    assert_eq!(restored, captured);

    // Saving twice replaces the row instead of failing on the unique constraint.
    storage.save(URL, IDENTIFIER, &captured)?;
    assert!(storage.retrieve(URL, IDENTIFIER)?.is_some());

    // Unknown identifiers are `None`, not an error.
    assert!(storage.retrieve(URL, "unknown")?.is_none());
    Ok(())
}

/// The storage key is the site, so every page of a shop shares its remembered elements.
#[test]
fn base_domain_groups_pages_of_one_site() {
    assert_eq!(SqliteStorage::base_domain(""), "default");
    assert_eq!(
        SqliteStorage::base_domain("https://example.com/a"),
        SqliteStorage::base_domain("http://example.com/b?x=1")
    );
    assert!(SqliteStorage::base_domain("https://www.example.com/x").contains("example"));
    // Case is normalized away.
    assert_eq!(
        SqliteStorage::base_domain("https://EXAMPLE.com/a"),
        SqliteStorage::base_domain("https://example.com/a")
    );
}

/// A user-supplied `Storage` works exactly like the built-in one.
#[test]
fn custom_storage_backend() -> Result<()> {
    let storage = MemoryStorage::default();

    {
        let adaptive = Adaptive::new(Selector::with_url(BEFORE, URL)?, storage.clone());
        let found = adaptive.css_adaptive(SELECTOR, IDENTIFIER, true, 40.0)?;
        assert_eq!(found.len(), 3);
        assert_eq!(storage.len(), 1);

        // `save` can also be called by hand for an element found some other way.
        let heading = need(adaptive.root().css_first("h2")?, "the heading")?;
        adaptive.save(&heading, "heading")?;
        assert_eq!(storage.len(), 2);
        assert_eq!(
            need(adaptive.retrieve("heading")?, "the stored heading")?.tag,
            "h2"
        );
    }

    {
        let adaptive = Adaptive::new(Selector::with_url(AFTER, URL)?, storage.clone());
        let relocated = adaptive.css_adaptive(SELECTOR, IDENTIFIER, false, 40.0)?;
        assert!(!relocated.is_empty());
        assert_eq!(adaptive.storage().len(), 2);
    }
    Ok(())
}

/// With an empty identifier the selector string itself is the key, as in Python.
#[test]
fn empty_identifier_falls_back_to_the_selector() -> Result<()> {
    let storage = MemoryStorage::default();
    let adaptive = Adaptive::new(Selector::with_url(BEFORE, URL)?, storage.clone());
    adaptive.css_adaptive(SELECTOR, "", true, 40.0)?;
    assert_eq!(storage.len(), 1);
    assert!(adaptive.retrieve(SELECTOR)?.is_some());
    Ok(())
}
