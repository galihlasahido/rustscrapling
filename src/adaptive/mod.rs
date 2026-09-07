//! Adaptive element selection: fingerprint an element now, find it again after the page
//! around it has been rewritten.
//!
//! This is the port of Scrapling's `adaptive` feature — `_StorageTools.element_to_dict`,
//! `Selector.__calculate_similarity_score`, `Selector.relocate` and
//! `scrapling/core/storage.py`. The three pieces are:
//!
//! * [`fingerprint`] turns a [`Selector`] into an [`ElementFingerprint`]: its tag, its
//!   attributes, its own text, the path of tag names down to it, and the same information
//!   about its parent, siblings and children.
//! * [`similarity_score`] scores a candidate element against a stored fingerprint, and
//!   [`relocate`] scores every element on a page and returns the best matches.
//! * [`Storage`] keeps fingerprints between runs; [`SqliteStorage`] is the default backend
//!   and writes the same table Python does, so a database file works with either.
//!
//! [`Adaptive`] ties them together:
//!
//! ```ignore
//! use rustscrapling::{Adaptive, Selector, SqliteStorage};
//!
//! let storage = SqliteStorage::open("adaptive.db")?;
//! let page = Adaptive::new(Selector::with_url(old_html, "https://example.com")?, storage);
//!
//! // The first run finds `#p1` and remembers what it looked like.
//! let product = page.css_adaptive("#p1", "", true, 40.0)?;
//!
//! // A later run, after the site was rebuilt and `#p1` no longer exists: the same call
//! // relocates the element by similarity instead of coming back empty.
//! ```

#![forbid(unsafe_code)]

mod fingerprint;
mod similarity;
mod storage;

pub use self::fingerprint::{fingerprint, ElementFingerprint};
pub use self::similarity::{relocate, similarity_score};
pub use self::storage::{SqliteStorage, Storage};

use crate::error::Result;
use crate::parser::{Selector, Selectors};

/// The percentage Scrapling relocates at unless the caller says otherwise.
pub const DEFAULT_PERCENTAGE: f64 = 40.0;

/// A [`Selector`] plus a [`Storage`], giving the `adaptive=True` behaviour of Python's
/// `Selector`.
#[derive(Debug)]
pub struct Adaptive<S: Storage> {
    root: Selector,
    storage: S,
    url: String,
}

impl<S: Storage> Adaptive<S> {
    /// Wrap a parsed document with a storage backend.
    ///
    /// The document's own URL decides which site's fingerprints are read and written; a
    /// document parsed without a URL shares the `"default"` row key with every other one.
    pub fn new(root: Selector, storage: S) -> Self {
        let url = root.url().to_string();
        Adaptive { root, storage, url }
    }

    /// Wrap a parsed document, overriding the URL its fingerprints are filed under.
    ///
    /// This is Python's `adaptive_domain`: two documents fetched from different hosts — a
    /// live site and an archived copy of it, say — can be told to share one set of stored
    /// fingerprints by giving them the same URL here.
    pub fn with_url(root: Selector, storage: S, url: impl Into<String>) -> Self {
        Adaptive {
            root,
            storage,
            url: url.into(),
        }
    }

    /// The document being queried.
    pub fn root(&self) -> &Selector {
        &self.root
    }

    /// The storage backend.
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// The URL the fingerprints of this document are filed under.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Query with CSS; if nothing matches, relocate the element saved under `identifier`.
    ///
    /// When the selector *does* match and `auto_save` is set, the first match is saved under
    /// `identifier` so a later structural change can be recovered from. When it does not
    /// match, the stored fingerprint is looked up and every element on the page is scored
    /// against it; the elements sharing the highest score come back, provided that score is
    /// at least `percentage` (Scrapling's default is [`DEFAULT_PERCENTAGE`]). If relocation
    /// succeeded and `auto_save` is set, the relocated element replaces what was stored.
    ///
    /// `identifier` is used when it is non-empty, otherwise the selector string itself —
    /// same as Python.
    pub fn css_adaptive(
        &self,
        selector: &str,
        identifier: &str,
        auto_save: bool,
        percentage: f64,
    ) -> Result<Selectors> {
        // Python's `css` refuses to run on a `::text` / `::attr()` result before it touches
        // the storage at all, so a wrapped text node never reaches the database.
        if self.root.is_text_node() {
            return Ok(Selectors::default());
        }

        let key = Self::key_for(selector, identifier);
        let found = self.root.css(selector)?;

        if !found.is_empty() {
            if auto_save {
                if let Some(first) = found.first() {
                    self.save(first, key)?;
                }
            }
            return Ok(found);
        }

        let Some(stored) = self.retrieve(key)? else {
            return Ok(Selectors::default());
        };

        let relocated = relocate(&self.root, &stored, percentage);
        if auto_save {
            if let Some(first) = relocated.first() {
                self.save(first, key)?;
            }
        }
        Ok(relocated)
    }

    /// Save an element's fingerprint under `identifier`.
    ///
    /// A `::text` or `::attr()` result is saved as the element it came from, matching
    /// Python's "if it is a text node, store its parent" rule. Python passes the missing
    /// parent straight to the storage backend and crashes there; a text node with no parent
    /// is stored as itself here instead.
    pub fn save(&self, element: &Selector, identifier: &str) -> Result<()> {
        let target = if element.is_text_node() {
            element.parent().unwrap_or_else(|| element.clone())
        } else {
            element.clone()
        };

        let print = fingerprint(&target);
        self.storage.save(&self.url, identifier, &print)
    }

    /// Load the fingerprint stored under `identifier`.
    pub fn retrieve(&self, identifier: &str) -> Result<Option<ElementFingerprint>> {
        self.storage.retrieve(&self.url, identifier)
    }

    /// The identifier if the caller gave one, otherwise the selector itself.
    fn key_for<'a>(selector: &'a str, identifier: &'a str) -> &'a str {
        if identifier.is_empty() {
            selector
        } else {
            identifier
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The "before" page from `docs/parsing/adaptive.md`.
    const BEFORE: &str = r#"
        <div class="container">
            <section class="products">
                <article class="product" id="p1">
                    <h3>Product 1</h3>
                    <p class="description">Description 1</p>
                </article>
                <article class="product" id="p2">
                    <h3>Product 2</h3>
                    <p class="description">Description 2</p>
                </article>
            </section>
        </div>
    "#;

    /// The same page after the owner restructured it: `#p1` is now `[data-id="p1"]`.
    const AFTER: &str = r#"
        <div class="new-container">
            <div class="product-wrapper">
                <section class="products">
                    <article class="product new-class" data-id="p1">
                        <div class="product-info">
                            <h3>Product 1</h3>
                            <p class="new-description">Description 1</p>
                        </div>
                    </article>
                    <article class="product new-class" data-id="p2">
                        <div class="product-info">
                            <h3>Product 2</h3>
                            <p class="new-description">Description 2</p>
                        </div>
                    </article>
                </section>
            </div>
        </div>
    "#;

    const SITE: &str = "https://example.com/products";

    fn shared_storage() -> Arc<SqliteStorage> {
        Arc::new(SqliteStorage::in_memory().expect("the in-memory database opens"))
    }

    fn page(html: &str, storage: &Arc<SqliteStorage>) -> Adaptive<Arc<SqliteStorage>> {
        let root = Selector::new(html).expect("the test document parses");
        Adaptive::with_url(root, Arc::clone(storage), SITE)
    }

    #[test]
    fn relocates_the_element_after_the_page_was_restructured() {
        let storage = shared_storage();

        let old_page = page(BEFORE, &storage);
        let first_run = old_page
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the first query runs");
        assert_eq!(
            first_run.len(),
            1,
            "the selector still works on the old page"
        );

        let new_page = page(AFTER, &storage);
        assert!(
            new_page
                .root()
                .css("#p1")
                .expect("the selector parses")
                .is_empty(),
            "the plain selector no longer matches after the change"
        );

        let second_run = new_page
            .css_adaptive("#p1", "", false, DEFAULT_PERCENTAGE)
            .expect("the second query runs");

        assert_eq!(second_run.len(), 1, "one element relocated");
        let element = second_run.first().expect("the relocated element");
        assert_eq!(element.tag(), "article");
        assert_eq!(
            element.attr("data-id").map(|value| value.to_string()),
            Some("p1".to_string()),
            "the relocated element is the first product, not the second"
        );
    }

    #[test]
    fn a_custom_identifier_is_used_instead_of_the_selector() {
        let storage = shared_storage();

        let old_page = page(BEFORE, &storage);
        old_page
            .css_adaptive("#p1", "first-product", true, DEFAULT_PERCENTAGE)
            .expect("the first query runs");

        assert!(old_page
            .retrieve("first-product")
            .expect("the lookup runs")
            .is_some());
        assert!(old_page.retrieve("#p1").expect("the lookup runs").is_none());

        let new_page = page(AFTER, &storage);
        let relocated = new_page
            .css_adaptive(
                "article.missing",
                "first-product",
                false,
                DEFAULT_PERCENTAGE,
            )
            .expect("the second query runs");
        assert_eq!(relocated.len(), 1);
    }

    #[test]
    fn nothing_comes_back_when_nothing_was_saved() {
        let storage = shared_storage();
        let new_page = page(AFTER, &storage);

        let found = new_page
            .css_adaptive("#p1", "", false, DEFAULT_PERCENTAGE)
            .expect("the query runs");
        assert!(found.is_empty());
    }

    #[test]
    fn auto_save_refreshes_the_stored_fingerprint_after_a_relocation() {
        let storage = shared_storage();

        page(BEFORE, &storage)
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the first query runs");

        let new_page = page(AFTER, &storage);
        new_page
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the second query runs");

        let stored = new_page
            .retrieve("#p1")
            .expect("the lookup runs")
            .expect("a fingerprint is stored");
        assert_eq!(
            stored.attributes.get("data-id").map(String::as_str),
            Some("p1")
        );
    }

    #[test]
    fn a_failed_relocation_leaves_the_stored_fingerprint_alone() {
        let storage = shared_storage();

        page(BEFORE, &storage)
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the first query runs");

        let new_page = page(AFTER, &storage);
        assert!(new_page
            .css_adaptive("#p1", "", true, 99.0)
            .expect("the second query runs")
            .is_empty());

        let stored = new_page
            .retrieve("#p1")
            .expect("the lookup runs")
            .expect("the original fingerprint is still there");
        assert_eq!(stored.attributes.get("id").map(String::as_str), Some("p1"));
    }

    #[test]
    fn a_too_high_threshold_relocates_nothing() {
        let storage = shared_storage();

        page(BEFORE, &storage)
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the first query runs");

        let found = page(AFTER, &storage)
            .css_adaptive("#p1", "", false, 99.0)
            .expect("the query runs");
        assert!(found.is_empty());
    }

    #[test]
    fn saving_and_retrieving_by_hand() {
        let storage = shared_storage();
        let document = page(BEFORE, &storage);

        let element = document
            .root()
            .css_first("#p2")
            .expect("the selector parses")
            .expect("the selector matches");
        document
            .save(&element, "my_special_element")
            .expect("the element is saved");

        let stored = document
            .retrieve("my_special_element")
            .expect("the lookup runs")
            .expect("a fingerprint is stored");
        assert_eq!(stored.tag, "article");
        assert_eq!(stored.attributes.get("id").map(String::as_str), Some("p2"));

        let relocated = relocate(document.root(), &stored, DEFAULT_PERCENTAGE);
        assert_eq!(relocated.len(), 1);
        assert_eq!(
            relocated
                .first()
                .and_then(|element| element.attr("id"))
                .map(|value| value.to_string()),
            Some("p2".to_string())
        );
    }

    #[test]
    fn a_text_node_is_saved_as_the_element_it_came_from() {
        let storage = shared_storage();
        let document = page(BEFORE, &storage);

        let text = document
            .root()
            .css_first("#p1 h3::text")
            .expect("the selector parses")
            .expect("the selector matches");
        assert!(text.is_text_node());

        document.save(&text, "heading").expect("the text is saved");
        let stored = document
            .retrieve("heading")
            .expect("the lookup runs")
            .expect("a fingerprint is stored");
        assert_eq!(stored.tag, "h3");
    }

    #[test]
    fn documents_without_a_url_share_the_default_row_key() {
        let storage = shared_storage();
        let root = Selector::new(BEFORE).expect("the test document parses");
        let document = Adaptive::new(root, Arc::clone(&storage));

        assert_eq!(document.url(), "");
        document
            .css_adaptive("#p1", "", true, DEFAULT_PERCENTAGE)
            .expect("the query runs");
        assert!(document.retrieve("#p1").expect("the lookup runs").is_some());
    }

    #[test]
    fn the_identifier_defaults_to_the_selector() {
        assert_eq!(Adaptive::<Arc<SqliteStorage>>::key_for("#p1", ""), "#p1");
        assert_eq!(
            Adaptive::<Arc<SqliteStorage>>::key_for("#p1", "named"),
            "named"
        );
    }
}
