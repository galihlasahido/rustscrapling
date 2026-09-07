# rustscrapling — public API contract

This document is the single source of truth for the crate's public API. It exists so that the
modules can be implemented **in parallel, by different people, without compiling against each
other**. Implement the signatures below exactly as written; if a signature is wrong or
impossible, change it *here first* and tell the other owners.

The crate is a port of the Python library **Scrapling**. Wherever a name maps onto a Python
name the mapping is given, so behaviour can be checked against the original.

| Python (Scrapling)            | Rust (rustscrapling)              |
|-------------------------------|-----------------------------------|
| `TextHandler`                 | `text::Text`                      |
| `TextHandlers`                | `text::Texts`                     |
| `AttributesHandler`           | `text::Attributes`                |
| `Selector` / `Adaptor`        | `parser::Selector`                |
| `Selectors` / `Adaptors`      | `parser::Selectors`               |
| `_StorageTools.element_to_dict` | `adaptive::fingerprint`         |
| `SQLiteStorageSystem`         | `adaptive::SqliteStorage`         |
| `Response`                    | `response::Response`              |
| `FetcherSession`              | `http::FetcherSession`            |
| `Spider` (ABC)                | `spider::Spider` (trait)          |
| `CrawlerEngine`               | `spider::CrawlerEngine`           |
| `DynamicSession`              | `browser::DynamicSession`         |

---

## 0. Ground rules

* Edition 2021, stable toolchain (1.98), no nightly features, no `unsafe` unless a reviewer
  agreed to it in advance.
* Everything fallible returns `crate::error::Result<T>` (`Error` is the crate-wide enum in
  `src/error.rs`). Do not add new error enums; add a variant or use `Error::Other`.
* Async code is `tokio`-based. Public async trait methods use `#[async_trait::async_trait]`.
* Public items carry a one-line doc comment. Doc comments below are the minimum text.
* `#[non_exhaustive]` on every public *options/config* struct (`SpiderConfig`, `RequestOptions`,
  `EngineOptions`, `DynamicSessionOptions`, `AutoThrottleConfig`, `Cookie`) so fields can be
  added later; behaviour structs stay exhaustive.
* Derive `Debug` and `Clone` everywhere it is cheap and meaningful.

### File layout and ownership

A module owner touches **only their own files**. Nobody edits `src/lib.rs`, `src/error.rs` or
this file without telling the others — those three are shared.

```
Cargo.toml                     shared
README.md                      shared
CONTRACT.md                    shared (this file)
src/lib.rs                     shared (module decls + re-exports only)
src/error.rs                   shared (Error, Result)

src/text.rs                    owner: text
src/parser/mod.rs              owner: parser
src/parser/selector.rs         owner: parser   (Selector)
src/parser/selectors.rs        owner: parser   (Selectors)
src/parser/css.rs              owner: parser   (CSS parsing, ::text / ::attr, comma lists)
src/parser/filter.rs           owner: parser   (Filter builder for find/find_all)
src/parser/generate.rs         owner: parser   (generate_css_selector, full variant)
src/adaptive/mod.rs            owner: adaptive
src/adaptive/fingerprint.rs    owner: adaptive (ElementFingerprint, fingerprint())
src/adaptive/similarity.rs     owner: adaptive (similarity_score(), relocate())
src/adaptive/storage.rs        owner: adaptive (Storage trait, SqliteStorage)
src/response.rs                owner: response
src/http/mod.rs                owner: http
src/http/fetcher.rs            owner: http     (Fetcher, RequestBuilder)
src/http/session.rs            owner: http     (FetcherSession)
src/http/headers.rs            owner: http     (BrowserProfile, stealth headers)
src/http/proxy.rs              owner: http     (ProxyRotator, is_proxy_error)
src/http/redirect.rs           owner: http     (FollowRedirects, private-IP guard)
src/spider/mod.rs              owner: spider
src/spider/request.rs          owner: spider   (Request, RequestOptions, Callback, fingerprint)
src/spider/url.rs              owner: spider   (canonicalize_url)
src/spider/scheduler.rs        owner: spider   (Scheduler)
src/spider/throttle.rs         owner: spider   (AutoThrottle, parse_retry_after)
src/spider/links.rs            owner: spider   (LinkExtractor, IGNORED_EXTENSIONS)
src/spider/robots.rs           owner: spider   (RobotsManager)
src/spider/checkpoint.rs       owner: spider   (CheckpointManager, CheckpointData)
src/spider/cache.rs            owner: spider   (ResponseCache)
src/spider/stats.rs            owner: spider   (CrawlStats, CrawlResult)
src/spider/items.rs            owner: spider   (Items)
src/spider/spider.rs           owner: spider   (Spider trait, SpiderConfig, Output)
src/spider/session.rs          owner: spider   (SessionManager, Session)
src/spider/engine.rs           owner: spider   (CrawlerEngine, EngineOptions, run)
src/spider/templates.rs        owner: spider   (CrawlRule, crawl_rules)
src/browser/mod.rs             owner: browser
src/browser/session.rs         owner: browser  (DynamicSession, DynamicFetcher)
src/browser/constants.rs       owner: browser  (DEFAULT_ARGS, STEALTH_ARGS, ...)
src/browser/blocking.rs        owner: browser  (resource/domain blocking)

tests/<module>.rs              owner of that module
examples/<name>.rs             anyone, one file per example
```

Each `mod.rs` declares its private submodules and `pub use`s the public items, so the outside
world only ever sees `rustscrapling::parser::Selector`, never `parser::selector::Selector`.

---

## 1. `text` — `src/text.rs`

Port of `scrapling/core/custom_types.py`.

```rust
/// An owned string with the scraping helpers Scrapling's `TextHandler` adds to `str`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Text(String);

impl Text {
    /// Wrap an existing string.
    pub fn new(value: impl Into<String>) -> Self;
    /// Borrow the inner string.
    pub fn as_str(&self) -> &str;
    /// Consume and return the inner `String`.
    pub fn into_string(self) -> String;

    /// Collapse `\t\r\n` and runs of spaces into single spaces, then trim.
    pub fn clean(&self) -> Text;
    /// Same as `clean` but also replaces HTML character entities.
    pub fn clean_with_entities(&self) -> Text;

    /// Every match (or capture group) of `pattern` against this text.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts>;
    /// Like `re` but with an already-compiled pattern; cannot fail on the pattern.
    pub fn re_compiled(&self, pattern: &regex::Regex, opts: ReOptions) -> Texts;
    /// The first match of `pattern`, or `None`.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>>;
    /// Like `re_first` with an already-compiled pattern.
    pub fn re_first_compiled(&self, pattern: &regex::Regex, opts: ReOptions) -> Option<Text>;
    /// Whether `pattern` matches at all (Python's `check_match=True`).
    pub fn re_matches(&self, pattern: &regex::Regex, opts: ReOptions) -> bool;

    /// Parse this text as JSON into any deserializable type.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T>;
    /// Parse this text as a free-form `serde_json::Value`.
    pub fn json_value(&self) -> Result<serde_json::Value>;

    /// Replace HTML character entities (`&amp;`, `&#38;`, ...) with their characters.
    pub fn replace_entities(&self) -> Text;
}

/// Options shared by every regex helper; mirrors the Python keyword arguments.
#[derive(Debug, Clone, Copy)]
pub struct ReOptions {
    /// Replace HTML entities in the results. Python default: `true`.
    pub replace_entities: bool,
    /// Run the regex against `clean()`ed text. Python default: `false`.
    pub clean_match: bool,
    /// Case-sensitive matching. Python default: `true`.
    pub case_sensitive: bool,
}

impl Default for ReOptions { /* replace_entities: true, clean_match: false, case_sensitive: true */ }

impl ReOptions {
    /// Builder-style setters, so call sites read like the Python kwargs.
    pub fn replace_entities(self, yes: bool) -> Self;
    pub fn clean_match(self, yes: bool) -> Self;
    pub fn case_sensitive(self, yes: bool) -> Self;
}

// Required trait impls on `Text`
impl std::ops::Deref for Text { type Target = str; }
impl AsRef<str> for Text {}
impl std::borrow::Borrow<str> for Text {}
impl std::fmt::Display for Text {}
impl From<String> for Text {}
impl From<&str> for Text {}
impl PartialEq<str> for Text {}
impl PartialEq<&str> for Text {}
impl serde::Serialize for Text {}
impl<'de> serde::Deserialize<'de> for Text {}
```

```rust
/// A list of `Text` values with the same regex/extraction helpers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Texts(Vec<Text>);

impl Texts {
    /// Build from anything that iterates into `Text`.
    pub fn new(items: impl IntoIterator<Item = Text>) -> Self;
    /// Apply `re` to every element and flatten the results.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts>;
    /// The first match found across all elements.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>>;
    /// The element at `index`, or `None`.
    pub fn get(&self, index: usize) -> Option<&Text>;
    /// All elements as a plain `Vec<String>` (Scrapy's `getall()`).
    pub fn getall(&self) -> Vec<String>;
    /// The first element, or `None`.
    pub fn first(&self) -> Option<&Text>;
    /// The last element, or `None`.
    pub fn last(&self) -> Option<&Text>;
    /// Number of elements.
    pub fn len(&self) -> usize;
    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool;
    /// Iterate over the elements.
    pub fn iter(&self) -> std::slice::Iter<'_, Text>;
    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<Text>;
}

impl std::ops::Deref for Texts { type Target = [Text]; }
impl IntoIterator for Texts { type Item = Text; }
impl<'a> IntoIterator for &'a Texts { type Item = &'a Text; }
impl FromIterator<Text> for Texts {}
impl std::ops::Index<usize> for Texts { type Output = Text; }
impl From<Vec<Text>> for Texts {}
```

```rust
/// An element's attributes: an insertion-ordered, case-sensitive map of name -> value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attributes(/* insertion-ordered Vec<(String, Text)> or equivalent */);

impl Attributes {
    /// Build from name/value pairs, keeping the given order.
    pub fn new(pairs: impl IntoIterator<Item = (String, Text)>) -> Self;
    /// The value of `name`, or `None`.
    pub fn get(&self, name: &str) -> Option<&Text>;
    /// Whether `name` is present.
    pub fn contains_key(&self, name: &str) -> bool;
    /// Attributes whose value equals `keyword`, or contains it when `partial` is true.
    pub fn search_values(&self, keyword: &str, partial: bool) -> Attributes;
    /// The attributes as a JSON object string (Python's `json_string`).
    pub fn json_string(&self) -> Result<String>;
    /// Iterate over `(name, value)` in document order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Text)>;
    /// Number of attributes.
    pub fn len(&self) -> usize;
    /// Whether there are no attributes.
    pub fn is_empty(&self) -> bool;
    /// Attribute names in document order.
    pub fn keys(&self) -> impl Iterator<Item = &str>;
    /// Attribute values in document order.
    pub fn values(&self) -> impl Iterator<Item = &Text>;
}

impl std::ops::Index<&str> for Attributes { type Output = Text; /* panics if absent */ }
impl<'a> IntoIterator for &'a Attributes { type Item = (&'a str, &'a Text); }
impl serde::Serialize for Attributes {}   // as a JSON object
impl<'de> serde::Deserialize<'de> for Attributes {}
```

**Behaviour notes for the owner**

* `clean()` maps `\t`, `\r`, `\n` to spaces, collapses runs of spaces, then trims — matching
  `__CLEANING_TABLE__` + `__CONSECUTIVE_SPACES_REGEX__`.
* `re()` returns capture group 1..n when the pattern has groups (Python's `findall` flattens
  them), otherwise the whole match.
* `replace_entities` is applied to results by default, matching Python.

---

## 2. `parser` — `src/parser/`

Port of `scrapling/parser.py` and `scrapling/core/mixins.py`. Backed by the `scraper` crate
(`scraper::Html`, `ego_tree::NodeId`).

```rust
/// A node in a parsed HTML document: an element, a text node, or an attribute value.
#[derive(Debug, Clone)]
pub struct Selector {
    // Arc<scraper::Html> + ego_tree::NodeId + SelectorKind + Arc<SelectorContext>
}

/// What a `Selector` points at. `::text` / `::attr()` results are `Selector`s too,
/// so the API mirrors Python's `_ElementUnicodeResult` handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorKind {
    /// A real element node.
    Element,
    /// A text node produced by `::text` or by a text child.
    TextNode(Text),
    /// An attribute value produced by `::attr(name)`.
    AttrNode { name: String, value: Text },
}

impl Selector {
    /// Parse an HTML document.
    pub fn new(html: &str) -> Result<Selector>;
    /// Parse an HTML document and remember the URL it came from (used by `urljoin`).
    pub fn with_url(html: &str, url: &str) -> Result<Selector>;
    /// Parse an HTML fragment rather than a full document.
    pub fn fragment(html: &str) -> Result<Selector>;

    /// The URL this document was fetched from, or `""`.
    pub fn url(&self) -> &str;
    /// What this selector points at.
    pub fn kind(&self) -> &SelectorKind;
    /// Whether this is a `::text` / `::attr()` result rather than an element.
    pub fn is_text_node(&self) -> bool;

    /// The tag name; `"#text"` for text nodes.
    pub fn tag(&self) -> &str;
    /// The element's own (direct) text, or the value for text/attr nodes.
    pub fn text(&self) -> Text;
    /// All descendant text joined by `sep`.
    pub fn get_all_text(&self, sep: &str, strip: bool, ignore_tags: &[&str]) -> Text;
    /// The element's attributes; empty for text nodes.
    pub fn attrib(&self) -> Attributes;
    /// The value of a single attribute.
    pub fn attr(&self, name: &str) -> Option<Text>;
    /// Whether the element carries `class_name` in its class list.
    pub fn has_class(&self, class_name: &str) -> bool;
    /// The element's inner+outer HTML (Python's `html_content`).
    pub fn html_content(&self) -> Text;
    /// A pretty-printed version of `html_content`.
    pub fn prettify(&self) -> Text;
    /// The raw body the document was parsed from; empty for nodes below the root.
    pub fn body(&self) -> &[u8];
    /// Join `relative_url` onto this selector's URL.
    pub fn urljoin(&self, relative_url: &str) -> Result<String>;

    /// The parent element, or `None` at the root.
    pub fn parent(&self) -> Option<Selector>;
    /// The direct child elements (comments and processing instructions skipped).
    pub fn children(&self) -> Selectors;
    /// The parent's other children.
    pub fn siblings(&self) -> Selectors;
    /// The next sibling element, or `None`.
    pub fn next(&self) -> Option<Selector>;
    /// The previous sibling element, or `None`.
    pub fn previous(&self) -> Option<Selector>;
    /// Every ancestor, closest first.
    pub fn iterancestors(&self) -> impl Iterator<Item = Selector> + '_;
    /// The first ancestor for which `pred` returns true.
    pub fn find_ancestor(&self, pred: impl Fn(&Selector) -> bool) -> Option<Selector>;
    /// The ancestors as a `Selectors` list (Python's `path`).
    pub fn path(&self) -> Selectors;
    /// Every element below this one, in document order.
    pub fn below_elements(&self) -> Selectors;

    /// Query with a CSS selector. Supports `::text`, `::attr(name)` and comma-separated lists.
    pub fn css(&self, selector: &str) -> Result<Selectors>;
    /// Query with a CSS selector and return only the first match.
    pub fn css_first(&self, selector: &str) -> Result<Option<Selector>>;

    /// Every descendant matching `filter`.
    pub fn find_all(&self, filter: &Filter) -> Result<Selectors>;
    /// The first descendant matching `filter`.
    pub fn find(&self, filter: &Filter) -> Result<Option<Selector>>;

    /// Elements whose own text matches `text`.
    pub fn find_by_text(
        &self,
        text: &str,
        first_match: bool,
        partial: bool,
        case_sensitive: bool,
        clean_match: bool,
    ) -> Selectors;
    /// Elements whose own text matches the regex `pattern`.
    pub fn find_by_regex(
        &self,
        pattern: &str,
        first_match: bool,
        case_sensitive: bool,
        clean_match: bool,
    ) -> Result<Selectors>;

    /// Sibling-shaped elements that look like this one (Python's `find_similar`).
    pub fn find_similar(
        &self,
        threshold: f64,
        ignore_attributes: &[&str],
        match_text: bool,
    ) -> Selectors;

    /// A short CSS selector pointing at this element (stops at the nearest `id`).
    pub fn generate_css_selector(&self) -> String;
    /// A CSS selector spelled out from the document root.
    pub fn generate_full_css_selector(&self) -> String;

    /// Serialize this node: outer HTML for elements, the value for text/attr nodes.
    pub fn get(&self) -> Text;
    /// `get()` wrapped in a one-element list, for Scrapy-shaped call sites.
    pub fn getall(&self) -> Texts;

    /// Regex over this node's text.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts>;
    /// First regex match over this node's text.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>>;
    /// Parse this node's text (or the document body at the root) as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T>;
}

impl std::fmt::Display for Selector {}  // html_content, or the value for text nodes
```

Defaults to use when porting call sites: `get_all_text(sep = "\n", strip = false,
ignore_tags = ["script", "style"])`, `find_by_text(first_match = true, partial = false,
case_sensitive = false, clean_match = true)`, `find_similar(threshold = 0.2,
ignore_attributes = ["href", "src"], match_text = false)`.

```rust
/// A list of `Selector`s with the chaining helpers Scrapling puts on `Selectors`.
#[derive(Debug, Clone, Default)]
pub struct Selectors(Vec<Selector>);

impl Selectors {
    /// Build from anything that iterates into `Selector`.
    pub fn new(items: impl IntoIterator<Item = Selector>) -> Self;
    /// Run `css` on every element and flatten the results.
    pub fn css(&self, selector: &str) -> Result<Selectors>;
    /// Run `re` on every element and flatten the results.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts>;
    /// The first regex match across all elements.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>>;
    /// The first element for which `pred` returns true.
    pub fn search(&self, pred: impl Fn(&Selector) -> bool) -> Option<&Selector>;
    /// Keep only the elements for which `pred` returns true.
    pub fn filter(&self, pred: impl Fn(&Selector) -> bool) -> Selectors;
    /// Serialize the first element, or `None` when empty.
    pub fn get(&self) -> Option<Text>;
    /// Serialize every element.
    pub fn getall(&self) -> Texts;
    /// The first element, or `None`.
    pub fn first(&self) -> Option<&Selector>;
    /// The last element, or `None`.
    pub fn last(&self) -> Option<&Selector>;
    /// Number of elements (Python's `length`).
    pub fn length(&self) -> usize;
    /// Number of elements.
    pub fn len(&self) -> usize;
    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool;
    /// Iterate over the elements.
    pub fn iter(&self) -> std::slice::Iter<'_, Selector>;
    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<Selector>;
}

impl std::ops::Deref for Selectors { type Target = [Selector]; }
impl IntoIterator for Selectors { type Item = Selector; }
impl<'a> IntoIterator for &'a Selectors { type Item = &'a Selector; }
impl FromIterator<Selector> for Selectors {}
impl std::ops::Index<usize> for Selectors { type Output = Selector; }
impl From<Vec<Selector>> for Selectors {}
impl Extend<Selector> for Selectors {}
```

```rust
/// The builder that replaces Python's `find_all(*args, **kwargs)`.
///
/// ```ignore
/// let f = Filter::new().tags(["h3", "h2"]).attr("class", "product").regex(r"Product \d")?;
/// let found = page.find_all(&f)?;
/// ```
#[derive(Default)]
pub struct Filter { /* private */ }

impl Filter {
    /// An empty filter that matches every element.
    pub fn new() -> Self;
    /// Restrict to a single tag name.
    pub fn tag(self, tag: impl Into<String>) -> Self;
    /// Restrict to any of these tag names.
    pub fn tags<I, S>(self, tags: I) -> Self where I: IntoIterator<Item = S>, S: Into<String>;
    /// Require an attribute to equal `value` (`class` is matched name-by-name, like Python).
    pub fn attr(self, name: impl Into<String>, value: impl Into<String>) -> Self;
    /// Require several attributes at once.
    pub fn attrs<I, K, V>(self, attrs: I) -> Self
    where I: IntoIterator<Item = (K, V)>, K: Into<String>, V: Into<String>;
    /// Require an attribute to merely be present.
    pub fn has_attr(self, name: impl Into<String>) -> Self;
    /// Require the element's text to match this regex.
    pub fn regex(self, pattern: &str) -> Result<Self>;
    /// Require the element's text to match this compiled regex.
    pub fn regex_compiled(self, pattern: regex::Regex) -> Self;
    /// Require a user predicate to return true.
    pub fn predicate(self, f: impl Fn(&Selector) -> bool + Send + Sync + 'static) -> Self;
    /// Whether a given element satisfies every condition in this filter.
    pub fn matches(&self, element: &Selector) -> bool;
}

impl std::fmt::Debug for Filter {}  // hand-written; closures are not Debug
```

**Behaviour notes for the owner**

* `css` must accept `div.product::text`, `a::attr(href)`, and `h2, h3` (comma lists, results
  concatenated in the order the selectors were given).
* `::text` yields one `Selector` per direct text child of each match; `::attr(name)` yields one
  per match that carries the attribute. Both come back as `SelectorKind::TextNode`/`AttrNode`,
  and on those every element-only method degrades exactly like Python: `tag()` is `"#text"`,
  `attrib()` is empty, `children()`/`css()`/`find_*` return empty lists, `has_class()` is false.
* `find_similar` requires the candidate to share the tag, the parent tag, the grandparent tag,
  and the ancestor depth of the original before attributes are scored.
* XPath is **out of scope**: `scraper` has no XPath engine, so `Selector::xpath` is not part of
  this contract. Anything Python does through XPath is expressed with CSS or with `Filter`.

---

## 3. `adaptive` — `src/adaptive/`

Port of `_StorageTools.element_to_dict`, `Selector.__calculate_similarity_score`,
`Selector.relocate` and `scrapling/core/storage.py`.

```rust
/// The stored shape of an element, used to find it again after the page changes.
/// Mirrors `_StorageTools.element_to_dict` field for field.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ElementFingerprint {
    /// The element's tag name.
    pub tag: String,
    /// The element's attributes, blank values dropped and values trimmed.
    pub attributes: std::collections::BTreeMap<String, String>,
    /// The element's own text, trimmed; `None` when it had none.
    pub text: Option<String>,
    /// Tag names from the document root down to and including this element.
    pub path: Vec<String>,
    /// The parent's tag name, when there is a parent.
    pub parent_name: Option<String>,
    /// The parent's attributes.
    pub parent_attribs: std::collections::BTreeMap<String, String>,
    /// The parent's own text, trimmed.
    pub parent_text: Option<String>,
    /// The tag names of the element's siblings.
    pub siblings: Vec<String>,
    /// The tag names of the element's children.
    pub children: Vec<String>,
}

/// Capture an element's fingerprint.
pub fn fingerprint(element: &Selector) -> ElementFingerprint;

/// Score how well `candidate` matches `original`, from 0.0 to 100.0.
pub fn similarity_score(original: &ElementFingerprint, candidate: &Selector) -> f64;

/// Search `root` for the elements that best match `fingerprint`, keeping only scores at or
/// above `percentage` (Scrapling's default is `40.0`).
pub fn relocate(root: &Selector, fingerprint: &ElementFingerprint, percentage: f64) -> Selectors;
```

`similarity_score` must reproduce Python's weighting exactly — each check adds at most 1.0 to
`score` and 1 to `checks`, and the result is `round((score / checks) * 100, 2)`:

1. tag equality (1.0 or 0.0);
2. text similarity, only when the original had text;
3. attribute-map similarity: `keys_ratio * 0.5 + values_ratio * 0.5`;
4. one text-similarity check per present original `class`, `id`, `href`, `src`;
5. path similarity;
6. when the original had a parent *and* the candidate has one: parent-name similarity,
   parent-attribute similarity, and parent-text similarity when the original had parent text;
7. siblings similarity, only when the original had siblings.

Every "similarity" above is Python's `difflib.SequenceMatcher(None, a, b).ratio()`; use
`similar::TextDiff::from_slices(...).ratio()` (or `from_chars` for strings), which implements
the same ratio.

```rust
/// Somewhere to keep element fingerprints between runs.
pub trait Storage: Send + Sync {
    /// Store `fingerprint` under `identifier` for the site `url` belongs to.
    fn save(&self, url: &str, identifier: &str, fingerprint: &ElementFingerprint) -> Result<()>;
    /// Load the fingerprint stored for `identifier` on the site `url` belongs to.
    fn retrieve(&self, url: &str, identifier: &str) -> Result<Option<ElementFingerprint>>;
}

/// The default `Storage`: one SQLite file, safe to share between threads.
#[derive(Debug)]
pub struct SqliteStorage { /* Mutex<rusqlite::Connection> */ }

impl SqliteStorage {
    /// Open (creating it if needed) the SQLite file backing this storage.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self>;
    /// Open an in-memory database, for tests.
    pub fn in_memory() -> Result<Self>;
    /// The registrable base domain of `url`, or `"default"` — the storage's row key.
    pub fn base_domain(url: &str) -> String;
}

impl Storage for SqliteStorage { /* ... */ }
```

The table is the one Python creates, so the files are interchangeable:

```sql
CREATE TABLE IF NOT EXISTS storage (
    id INTEGER PRIMARY KEY,
    url TEXT,
    identifier TEXT,
    element_data TEXT,
    UNIQUE (url, identifier)
);
```

`element_data` holds the JSON of `ElementFingerprint`. `save` is an `INSERT OR REPLACE`, and
the connection is opened with `PRAGMA journal_mode=WAL`. `base_domain` lowercases the URL and
returns the registrable domain (`shop.example.co.uk` -> `example.co.uk` where the owner can
tell, otherwise the host); when there is no URL it returns `"default"`.

```rust
/// A `Selector` plus a `Storage`, giving the `adaptive=True` behaviour of Python's `Selector`.
pub struct Adaptive<S: Storage> { /* root: Selector, storage: S, url: String */ }

impl<S: Storage> Adaptive<S> {
    /// Wrap a parsed document with a storage backend.
    pub fn new(root: Selector, storage: S) -> Self;
    /// The document being queried.
    pub fn root(&self) -> &Selector;
    /// The storage backend.
    pub fn storage(&self) -> &S;

    /// Query with CSS; if nothing matches, relocate the element saved under `identifier`.
    ///
    /// When the selector *does* match and `auto_save` is set, the first match is saved under
    /// `identifier` so a later structural change can be recovered from.
    pub fn css_adaptive(
        &self,
        selector: &str,
        identifier: &str,
        auto_save: bool,
        percentage: f64,
    ) -> Result<Selectors>;

    /// Save an element's fingerprint under `identifier`.
    pub fn save(&self, element: &Selector, identifier: &str) -> Result<()>;
    /// Load the fingerprint stored under `identifier`.
    pub fn retrieve(&self, identifier: &str) -> Result<Option<ElementFingerprint>>;
}
```

Use `identifier` when it is non-empty, otherwise the selector string itself — same as Python.

---

## 4. `response` — `src/response.rs`

Port of `scrapling/engines/toolbelt/custom.py` (`Response`, `StatusText`).

```rust
/// A fetched page: a parsed `Selector` plus everything the transport knew about it.
///
/// `Response` derefs to `Selector`, so `response.css(...)`, `response.find_all(...)` and
/// friends work directly, exactly like the Python subclass.
#[derive(Debug, Clone)]
pub struct Response {
    /// The final URL, after redirects.
    pub url: String,
    /// The HTTP status code.
    pub status: u16,
    /// The status reason phrase as the server sent it.
    pub reason: String,
    /// The response headers.
    pub headers: HeaderMap,
    /// The headers that were sent with the request.
    pub request_headers: HeaderMap,
    /// The cookies the response set.
    pub cookies: Vec<Cookie>,
    /// The raw response body.
    pub body: Vec<u8>,
    /// The encoding the body was decoded with.
    pub encoding: String,
    /// The HTTP method used.
    pub method: String,
    /// The redirect chain that led here, oldest first.
    pub history: Vec<Response>,
    /// Free-form metadata (the proxy used, the spider's request meta, ...).
    pub meta: std::collections::HashMap<String, serde_json::Value>,
    // private: the parsed Selector
}

impl Response {
    /// Build a response from a body and its transport metadata.
    pub fn new(url: impl Into<String>, body: Vec<u8>, status: u16) -> Result<Response>;
    /// Builder-style setters used by the fetchers.
    pub fn with_reason(self, reason: impl Into<String>) -> Self;
    pub fn with_headers(self, headers: HeaderMap) -> Self;
    pub fn with_request_headers(self, headers: HeaderMap) -> Self;
    pub fn with_cookies(self, cookies: Vec<Cookie>) -> Self;
    pub fn with_encoding(self, encoding: impl Into<String>) -> Self;
    pub fn with_method(self, method: impl Into<String>) -> Self;
    pub fn with_history(self, history: Vec<Response>) -> Self;
    pub fn with_meta(self, meta: std::collections::HashMap<String, serde_json::Value>) -> Self;

    /// The parsed document.
    pub fn selector(&self) -> &Selector;
    /// The body decoded with `encoding`.
    pub fn text_body(&self) -> std::borrow::Cow<'_, str>;
    /// The reason phrase for a status code, from the table below.
    pub fn status_text(status: u16) -> &'static str;
    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T>;
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool;

    /// Build a crawl `Request` for a URL found on this page, inheriting this response's
    /// session id, callback, priority and meta, and setting `referer` to this page.
    #[cfg(feature = "spider")]
    pub fn follow(&self, url: &str, opts: FollowOptions) -> Result<crate::spider::Request>;
}

impl std::ops::Deref for Response { type Target = Selector; }
impl std::fmt::Display for Response {}  // "<200 https://example.com>"

/// What `follow` may override; anything left at its default is inherited.
#[cfg(feature = "spider")]
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FollowOptions {
    /// The session to use; empty means "same as this response".
    pub sid: Option<String>,
    /// The callback to dispatch the new response to.
    pub callback: Option<crate::spider::Callback>,
    /// The scheduling priority.
    pub priority: Option<i32>,
    /// Skip the duplicate filter for this request.
    pub dont_filter: bool,
    /// Extra meta merged over this response's meta.
    pub meta: std::collections::HashMap<String, serde_json::Value>,
    /// Set `referer` to this response's URL. Default `true`.
    pub referer_flow: bool,
}

/// One cookie from a response.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Cookie {
    /// The cookie name.
    pub name: String,
    /// The cookie value.
    pub value: String,
    /// The domain it applies to.
    pub domain: String,
    /// The path it applies to.
    pub path: String,
}

/// A case-insensitive, insertion-ordered header map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderMap(/* private */);

impl HeaderMap {
    /// An empty map.
    pub fn new() -> Self;
    /// Build from name/value pairs.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self;
    /// Look a header up, ignoring case.
    pub fn get(&self, name: &str) -> Option<&str>;
    /// Insert or replace a header, ignoring case.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>);
    /// Whether a header is present, ignoring case.
    pub fn contains_key(&self, name: &str) -> bool;
    /// Remove a header, ignoring case.
    pub fn remove(&mut self, name: &str) -> Option<String>;
    /// Iterate in insertion order, with the original casing.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)>;
    /// Number of headers.
    pub fn len(&self) -> usize;
    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool;
}

impl serde::Serialize for HeaderMap {}
impl<'de> serde::Deserialize<'de> for HeaderMap {}
impl FromIterator<(String, String)> for HeaderMap {}

/// The IANA reason phrases, as Python's `StatusText`.
pub struct StatusText;

impl StatusText {
    /// The phrase for a status code, or `"Unknown Status Code"`.
    pub fn get(status: u16) -> &'static str;
}
```

The phrase table is the one in `custom.py` (100-103, 200-208, 226, 300-308, 400-418, 421-431,
451, 500-508, 510-511) with `"Unknown Status Code"` as the fallback.

---

## 5. `http` (feature `http`) — `src/http/`

Port of `scrapling/engines/static.py` and `toolbelt/proxy_rotation.py`, on `reqwest`.

```rust
/// A configured HTTP client that builds requests. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Fetcher { /* private */ }

impl Fetcher {
    /// A fetcher with Scrapling's defaults (stealth headers on, 3 retries, safe redirects).
    pub fn new() -> Result<Fetcher>;
    /// Start configuring a fetcher.
    pub fn builder() -> FetcherBuilder;

    /// Begin a GET request.
    pub fn get(&self, url: &str) -> RequestBuilder;
    /// Begin a POST request.
    pub fn post(&self, url: &str) -> RequestBuilder;
    /// Begin a PUT request.
    pub fn put(&self, url: &str) -> RequestBuilder;
    /// Begin a DELETE request.
    pub fn delete(&self, url: &str) -> RequestBuilder;
    /// Begin a request with any method.
    pub fn request(&self, method: &str, url: &str) -> RequestBuilder;
}

impl Default for Fetcher { /* panics only if the TLS backend fails to start */ }

/// Builder for `Fetcher`.
#[derive(Debug, Clone, Default)]
pub struct FetcherBuilder { /* private */ }

impl FetcherBuilder {
    /// Send the static header profile of this browser.
    pub fn impersonate(self, profile: BrowserProfile) -> Self;
    /// Add realistic browser headers and a Google referer. Default `true`.
    pub fn stealthy_headers(self, yes: bool) -> Self;
    /// Route every request through this proxy URL.
    pub fn proxy(self, proxy: &str) -> Self;
    /// Pick a proxy per request from this rotator (mutually exclusive with `proxy`).
    pub fn proxy_rotator(self, rotator: ProxyRotator) -> Self;
    /// Per-request timeout. Default 30s.
    pub fn timeout(self, timeout: std::time::Duration) -> Self;
    /// Headers added to every request.
    pub fn headers(self, headers: HeaderMap) -> Self;
    /// How many times to retry a failed request. Default 3.
    pub fn retries(self, retries: u32) -> Self;
    /// How long to wait between retries. Default 1s.
    pub fn retry_delay(self, delay: std::time::Duration) -> Self;
    /// Redirect policy. Default `FollowRedirects::Safe`.
    pub fn follow_redirects(self, policy: FollowRedirects) -> Self;
    /// Maximum redirects to follow. Default 30.
    pub fn max_redirects(self, max: usize) -> Self;
    /// Verify TLS certificates. Default `true`.
    pub fn verify_tls(self, yes: bool) -> Self;
    /// Allow requests aimed at loopback, link-local, private and unspecified addresses.
    /// Default `false`.
    pub fn allow_private_addresses(self, yes: bool) -> Self;
    /// Build the fetcher.
    pub fn build(self) -> Result<Fetcher>;
    /// Build a session (one client, one cookie jar) instead of a stateless fetcher.
    pub fn build_session(self) -> Result<FetcherSession>;
}

/// One in-flight request, with per-request overrides of the fetcher's settings.
#[derive(Debug)]
pub struct RequestBuilder { /* private */ }

impl RequestBuilder {
    /// Add query-string parameters.
    pub fn query<I, K, V>(self, params: I) -> Self
    where I: IntoIterator<Item = (K, V)>, K: Into<String>, V: Into<String>;
    /// Set or replace a header for this request.
    pub fn header(self, name: &str, value: &str) -> Self;
    /// Merge a whole header map into this request.
    pub fn headers(self, headers: HeaderMap) -> Self;
    /// Send a URL-encoded form body.
    pub fn form<I, K, V>(self, fields: I) -> Self
    where I: IntoIterator<Item = (K, V)>, K: Into<String>, V: Into<String>;
    /// Send a JSON body.
    pub fn json<T: serde::Serialize>(self, value: &T) -> Result<Self>;
    /// Send a raw body.
    pub fn body(self, body: impl Into<Vec<u8>>) -> Self;
    /// Override the timeout for this request.
    pub fn timeout(self, timeout: std::time::Duration) -> Self;
    /// Override the proxy for this request.
    pub fn proxy(self, proxy: &str) -> Self;
    /// Override the retry count for this request.
    pub fn retries(self, retries: u32) -> Self;
    /// Override the redirect policy for this request.
    pub fn follow_redirects(self, policy: FollowRedirects) -> Self;
    /// Override the impersonated browser profile for this request.
    pub fn impersonate(self, profile: BrowserProfile) -> Self;
    /// Send the request, retrying as configured, and parse the result.
    pub async fn send(self) -> Result<Response>;
}

/// A fetcher that keeps one `reqwest::Client` and one cookie jar across requests.
#[derive(Debug, Clone)]
pub struct FetcherSession { /* private */ }

impl FetcherSession {
    /// A session with the default settings.
    pub fn new() -> Result<FetcherSession>;
    /// Start configuring a session.
    pub fn builder() -> FetcherBuilder;
    /// Begin a GET request on this session.
    pub fn get(&self, url: &str) -> RequestBuilder;
    /// Begin a POST request on this session.
    pub fn post(&self, url: &str) -> RequestBuilder;
    /// Begin a PUT request on this session.
    pub fn put(&self, url: &str) -> RequestBuilder;
    /// Begin a DELETE request on this session.
    pub fn delete(&self, url: &str) -> RequestBuilder;
    /// Begin a request with any method on this session.
    pub fn request(&self, method: &str, url: &str) -> RequestBuilder;
    /// The cookies the jar currently holds for a URL.
    pub fn cookies(&self, url: &str) -> Vec<Cookie>;
}

/// What to do with 3xx responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowRedirects {
    /// Follow redirects, but refuse ones pointing at loopback, link-local or private IPs.
    #[default]
    Safe,
    /// Follow every redirect.
    All,
    /// Follow none; return the 3xx response as-is.
    None,
}

/// Rotates through a pool of proxies.
#[derive(Clone)]
pub struct ProxyRotator { /* Arc<Mutex<...>> */ }

impl ProxyRotator {
    /// A rotator that hands out the proxies cyclically.
    pub fn new(proxies: Vec<String>) -> Result<ProxyRotator>;
    /// A rotator with a custom strategy: `(proxies, current_index) -> (proxy, next_index)`.
    pub fn with_strategy(
        proxies: Vec<String>,
        strategy: impl Fn(&[String], usize) -> (String, usize) + Send + Sync + 'static,
    ) -> Result<ProxyRotator>;
    /// The next proxy according to the strategy.
    pub fn get_proxy(&self) -> String;
    /// A copy of the configured proxies.
    pub fn proxies(&self) -> Vec<String>;
    /// Number of configured proxies.
    pub fn len(&self) -> usize;
    /// Whether the pool is empty (it never is; `new` rejects that).
    pub fn is_empty(&self) -> bool;
}

impl std::fmt::Debug for ProxyRotator {}

/// Whether an error looks like the proxy's fault rather than the site's.
pub fn is_proxy_error(error: &crate::Error) -> bool;

/// The browser whose header set to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserProfile {
    /// Recent desktop Chrome.
    #[default]
    Chrome,
    /// Recent desktop Firefox.
    Firefox,
    /// Recent desktop Safari.
    Safari,
    /// Recent desktop Edge.
    Edge,
}

impl BrowserProfile {
    /// The headers this profile sends: `User-Agent`, `Accept`, `Accept-Language`,
    /// `Accept-Encoding`, `sec-ch-ua*`, `Sec-Fetch-*`, `Upgrade-Insecure-Requests`.
    pub fn headers(&self) -> HeaderMap;
    /// Just this profile's `User-Agent`.
    pub fn user_agent(&self) -> &'static str;
}
```

**Behaviour notes for the owner**

* `is_proxy_error` matches the substrings Python looks for, case-insensitively:
  `net::err_proxy`, `net::err_tunnel`, `connection refused`, `connection reset`,
  `connection timed out`, `failed to connect`, `could not resolve proxy`.
* Retries: the request is always attempted once; `retries` below 1 means "no retry". Between
  attempts wait `retry_delay`; when a rotator is configured, take a fresh proxy each attempt.
* `stealthy_headers` adds a `referer: https://www.google.com/` when the caller did not set one,
  plus the `BrowserProfile` headers for any header the caller did not already set.
* `FollowRedirects::Safe` inspects each redirect target's *host* and rejects loopback
  (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, `fe80::/10`) and private ranges
  (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`) written as IP literals, plus the internal-looking
  names `localhost`, `*.localhost`, `*.local`, `*.internal` and `*.home.arpa`, returning
  `Error::Http`. A redirect policy is a synchronous callback, so it cannot itself resolve a name.
* Unless `allow_private_addresses(true)` is set, the same ranges are refused for the URL a request
  is *aimed at*, whatever the redirect policy — the URL reaching `Fetcher` often comes out of
  scraped markup — and every hostname the client connects to is resolved through a guard that
  rejects the connection when `getaddrinfo` returns an address in those ranges. That covers the
  DNS-rebinding case the host-only check above cannot see, on the first hop and on redirects.

---

## 6. `spider` (feature `spider`) — `src/spider/`

Port of `scrapling/spiders/`. Everything here is `tokio`-based.

### 6.1 Requests — `request.rs`, `url.rs`

```rust
/// One scheduled fetch.
#[derive(Debug, Clone)]
pub struct Request {
    /// The URL to fetch.
    pub url: String,
    /// Which registered session to fetch it with; empty means the default session.
    pub sid: String,
    /// Which spider method handles the response.
    pub callback: Callback,
    /// Higher runs first. Default 0.
    pub priority: i32,
    /// Skip the duplicate filter.
    pub dont_filter: bool,
    /// Free-form data carried to the callback through `Response::meta`.
    pub meta: std::collections::HashMap<String, serde_json::Value>,
    /// How many times this request has already been retried after a block.
    pub retry_count: u32,
    /// Transport options for this request.
    pub options: RequestOptions,
}

impl Request {
    /// A GET request for `url` handled by the spider's `parse`.
    pub fn new(url: impl Into<String>) -> Request;
    /// Builder-style setters.
    pub fn sid(self, sid: impl Into<String>) -> Self;
    pub fn callback(self, callback: Callback) -> Self;
    pub fn priority(self, priority: i32) -> Self;
    pub fn dont_filter(self, yes: bool) -> Self;
    pub fn meta(self, key: impl Into<String>, value: serde_json::Value) -> Self;
    pub fn options(self, options: RequestOptions) -> Self;

    /// The URL's host, used for per-domain limits and delays.
    pub fn domain(&self) -> String;
    /// The deduplication fingerprint: SHA-1 over the canonical URL, method, body and sid.
    pub fn fingerprint(
        &self,
        include_kwargs: bool,
        include_headers: bool,
        keep_fragments: bool,
    ) -> [u8; 20];
}

/// Which spider method handles a response.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Callback {
    /// The spider's `parse`.
    #[default]
    Parse,
    /// The spider's `callback(name, response)`, dispatched by name.
    Named(String),
}

/// The transport options of a single request.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct RequestOptions {
    /// HTTP method; empty means GET.
    pub method: String,
    /// Extra headers for this request.
    pub headers: std::collections::BTreeMap<String, String>,
    /// A URL-encoded form body.
    pub form: Option<std::collections::BTreeMap<String, String>>,
    /// A JSON body.
    pub json: Option<serde_json::Value>,
    /// A raw body.
    pub body: Option<Vec<u8>>,
    /// A proxy URL just for this request.
    pub proxy: Option<String>,
    /// A timeout just for this request.
    pub timeout: Option<std::time::Duration>,
}

/// Normalize a URL for deduplication: sort the query parameters, normalize the percent
/// encoding and the path, and drop the fragment unless `keep_fragments`. Port of w3lib's
/// `canonicalize_url`.
pub fn canonicalize_url(url: &str, keep_fragments: bool) -> String;
```

The fingerprint is `sha1` over a canonical JSON object with sorted keys, holding `sid`,
`body` (lowercase hex), `method` and the canonical `url`; `include_kwargs` adds a sorted
`kwargs` entry built from `options` minus the body fields, and `include_headers` adds a
`headers` entry of lowercased, hex-encoded name/value pairs. Same layout as Python, so the
two implementations agree on what counts as a duplicate.

### 6.2 Scheduler — `scheduler.rs`

```rust
/// A priority queue of requests with a fingerprint-based duplicate filter.
#[derive(Debug)]
pub struct Scheduler { /* BinaryHeap + HashSet<[u8; 20]> */ }

impl Scheduler {
    /// A scheduler with the spider's fingerprint settings and no ceilings.
    pub fn new(include_kwargs: bool, include_headers: bool, keep_fragments: bool) -> Scheduler;
    /// The same, refusing to hold more than `max_queued` requests or to accept more than
    /// `max_total` in all; `0` switches a ceiling off. This is what the engine builds from
    /// `SpiderConfig`.
    pub fn with_limits(
        include_kwargs: bool,
        include_headers: bool,
        keep_fragments: bool,
        max_queued: usize,
        max_total: usize,
    ) -> Scheduler;
    /// Queue a request; returns false when it was dropped as a duplicate or refused by a ceiling.
    pub fn enqueue(&mut self, request: Request) -> bool;
    /// Take the highest-priority request, FIFO within a priority.
    pub fn dequeue(&mut self) -> Option<Request>;
    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool;
    /// How many requests are queued.
    pub fn len(&self) -> usize;
    /// The queued requests plus the seen set, for a checkpoint.
    pub fn snapshot(&self) -> (Vec<Request>, std::collections::HashSet<[u8; 20]>);
    /// Reload a snapshot taken earlier.
    pub fn restore(&mut self, requests: Vec<Request>, seen: std::collections::HashSet<[u8; 20]>);
}
```

### 6.3 Throttle — `throttle.rs`

```rust
/// Per-domain delay that adapts to observed latency, as Scrapling's `AutoThrottle`.
#[derive(Debug, Clone)]
pub struct AutoThrottle { /* delays: HashMap<String, f64> */ }

/// How the throttle starts and how far it may go.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct AutoThrottleConfig {
    /// Delay used for a domain's first request. Default 5.0s.
    pub start_delay: f64,
    /// Highest delay allowed. Default 60.0s.
    pub max_delay: f64,
    /// Requests aimed to have in flight per domain. Default 1.0.
    pub target_concurrency: f64,
    /// Double the delay (or honour `Retry-After`) when a domain blocks us. Default `true`.
    pub block_backoff: bool,
}

impl Default for AutoThrottleConfig { /* the values above */ }

impl AutoThrottle {
    /// Build a throttle; errors when `target_concurrency <= 0` or `max_delay < start_delay`.
    pub fn new(config: AutoThrottleConfig) -> Result<AutoThrottle>;
    /// The current delay for a domain, seeded from `start_delay` the first time.
    pub fn delay_for(&mut self, domain: &str, floor: f64) -> f64;
    /// Feed a finished request back in and return the domain's new delay.
    pub fn record(
        &mut self,
        domain: &str,
        latency: f64,
        ok: bool,
        floor: f64,
        retry_after: Option<f64>,
    ) -> f64;
    /// Forget every learned delay.
    pub fn reset(&mut self);
    /// A snapshot of the learned delays, for the stats.
    pub fn delays(&self) -> std::collections::HashMap<String, f64>;
}

/// Seconds a `Retry-After` header asks for: a number of seconds, or an HTTP date.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<f64>;
```

`record` must reproduce the Python formula exactly:

```text
target  = latency / target_concurrency
new     = max((current + target) / 2, target)
if !ok  { penalty = if block_backoff { retry_after.unwrap_or(current * 2.0) } else { current };
          new = max(new, penalty, current) }
new     = min(max(new, floor), max_delay)
```

### 6.4 Link extraction — `links.rs`

```rust
/// The file extensions dropped by default, as Scrapy's `IGNORED_EXTENSIONS`.
pub const IGNORED_EXTENSIONS: &[&str] = &[/* the list from links.py */];

/// Pulls URLs out of a response and filters them.
#[derive(Debug, Default)]
pub struct LinkExtractor { /* private */ }

impl LinkExtractor {
    /// An extractor that keeps every `<a href>` and `<area href>`.
    pub fn new() -> LinkExtractor;
    /// Keep only URLs matching at least one of these regexes.
    pub fn allow(self, patterns: &[&str]) -> Result<Self>;
    /// Drop URLs matching any of these regexes; takes precedence over `allow`.
    pub fn deny(self, patterns: &[&str]) -> Result<Self>;
    /// Keep only these hosts and their subdomains.
    pub fn allow_domains(self, domains: &[&str]) -> Self;
    /// Drop these hosts and their subdomains.
    pub fn deny_domains(self, domains: &[&str]) -> Self;
    /// Only look inside the elements matched by these CSS selectors.
    pub fn restrict_css(self, selectors: &[&str]) -> Self;
    /// Which tags to read URLs from. Default `["a", "area"]`.
    pub fn tags(self, tags: &[&str]) -> Self;
    /// Which attributes to read URLs from. Default `["href"]`.
    pub fn attrs(self, attrs: &[&str]) -> Self;
    /// Canonicalize the extracted URLs. Default `true`.
    pub fn canonicalize(self, yes: bool) -> Self;
    /// Keep the fragment when canonicalizing. Default `false`.
    pub fn keep_fragment(self, yes: bool) -> Self;
    /// Override the dropped extensions. Default `IGNORED_EXTENSIONS`.
    pub fn deny_extensions(self, extensions: &[&str]) -> Self;

    /// Absolute, filtered, deduplicated URLs from `response`, in document order.
    pub fn extract(&self, response: &Response) -> Vec<String>;
    /// Whether a single URL passes the filters, without a response.
    pub fn matches(&self, url: &str) -> bool;
}
```

Only `http` and `https` URLs pass — Scrapy also keeps `file`, but these URLs come out of remote
markup and are handed to a session, and a browser session would render `file:///etc/passwd` and
return its contents. A URL is dropped when *any* of its trailing extensions (`archive.tar.gz`
yields `gz` and `tar.gz`) is in `deny_extensions`.

### 6.5 robots.txt — `robots.rs`

```rust
/// Fetches, parses and caches robots.txt per domain.
#[derive(Debug)]
pub struct RobotsManager { /* private */ }

impl RobotsManager {
    /// A manager that fetches robots.txt with the given session manager and session id.
    pub fn new(sessions: std::sync::Arc<SessionManager>, sid: String) -> RobotsManager;
    /// Whether `*` may fetch this URL; true when robots.txt is missing or unreadable.
    pub async fn can_fetch(&self, url: &str) -> bool;
    /// The `Crawl-delay` for `*` on this URL's domain, when it declares one.
    pub async fn crawl_delay(&self, url: &str) -> Option<f64>;
    /// Warm the cache for these domains, concurrently.
    pub async fn prefetch(&self, urls: &[String]);
}
```

### 6.6 Checkpoints — `checkpoint.rs`

```rust
/// The crawl state written to disk so a paused crawl can resume.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CheckpointData {
    /// The requests that were still queued.
    pub requests: Vec<Request>,
    /// The fingerprints already seen, hex-encoded so the file stays readable JSON.
    pub seen: Vec<String>,
}

/// Reads and writes the checkpoint file.
#[derive(Debug, Clone)]
pub struct CheckpointManager { /* dir + interval */ }

impl CheckpointManager {
    /// A manager writing `checkpoint.json` into `crawldir` every `interval` seconds
    /// (0 disables the periodic save).
    pub fn new(crawldir: impl Into<std::path::PathBuf>, interval: f64) -> CheckpointManager;
    /// Whether a checkpoint file exists.
    pub async fn has_checkpoint(&self) -> bool;
    /// Write the state atomically: to a temp file, then rename over the real one.
    pub async fn save(&self, data: &CheckpointData) -> Result<()>;
    /// Read the state back; `None` when there is none or it cannot be read.
    pub async fn load(&self) -> Option<CheckpointData>;
    /// Delete the checkpoint file after a completed crawl.
    pub async fn cleanup(&self) -> Result<()>;
    /// The configured save interval in seconds.
    pub fn interval(&self) -> f64;
}
```

`Request` must therefore be `Serialize + Deserialize`; that is why `Callback` is a value
rather than a closure.

### 6.7 Development cache — `cache.rs`

```rust
/// Records responses to disk in development mode and replays them on later runs.
#[derive(Debug, Clone)]
pub struct ResponseCache { /* dir */ }

impl ResponseCache {
    /// A cache stored under `dir` (Scrapling uses `.scrapling_cache/<spider name>`).
    pub fn new(dir: impl Into<std::path::PathBuf>) -> ResponseCache;
    /// The cached response for a fingerprint, when there is a readable one.
    pub async fn get(&self, fingerprint: &[u8; 20]) -> Option<Response>;
    /// Store a response, written atomically as `<hex fingerprint>.json`.
    pub async fn put(&self, fingerprint: &[u8; 20], response: &Response, method: &str) -> Result<()>;
    /// Delete every cached response.
    pub async fn clear(&self) -> Result<()>;
}
```

The JSON file holds `url`, `content` (base64 of the body), `status`, `reason`, `encoding`,
`cookies`, `headers`, `request_headers` and `method` — the same keys Python writes.

### 6.8 Stats, results and items — `stats.rs`, `items.rs`

```rust
/// Counters for one crawl run; the field set of Python's `CrawlStats`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CrawlStats {
    pub requests_count: u64,
    pub concurrent_requests: usize,
    pub concurrent_requests_per_domain: usize,
    pub failed_requests_count: u64,
    pub offsite_requests_count: u64,
    pub robots_disallowed_count: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub response_bytes: u64,
    pub items_scraped: u64,
    pub items_dropped: u64,
    pub start_time: f64,
    pub end_time: f64,
    pub download_delay: f64,
    pub autothrottle_enabled: bool,
    pub blocked_requests_count: u64,
    pub autothrottle_delays: std::collections::HashMap<String, f64>,
    pub custom_stats: std::collections::HashMap<String, serde_json::Value>,
    pub response_status_count: std::collections::HashMap<String, u64>,
    pub domains_response_bytes: std::collections::HashMap<String, u64>,
    pub sessions_requests_count: std::collections::HashMap<String, u64>,
    pub proxies: Vec<String>,
}

impl CrawlStats {
    /// Wall-clock seconds the crawl took.
    pub fn elapsed_seconds(&self) -> f64;
    /// Requests per second over the whole run.
    pub fn requests_per_second(&self) -> f64;
    /// Count one response status.
    pub fn increment_status(&mut self, status: u16);
    /// Add a response's size, globally and for its domain.
    pub fn increment_response_bytes(&mut self, domain: &str, count: u64);
    /// Count one request, globally and for its session.
    pub fn increment_requests_count(&mut self, sid: &str);
    /// The stats as pretty-printed JSON, in the key order Python logs.
    pub fn to_json(&self) -> Result<String>;
}

/// Everything a finished crawl produced.
#[derive(Debug, Default)]
pub struct CrawlResult {
    /// The run's counters.
    pub stats: CrawlStats,
    /// The scraped items.
    pub items: Items,
    /// Whether the crawl stopped early on a pause request.
    pub paused: bool,
}

impl CrawlResult {
    /// Whether the crawl ran to completion.
    pub fn completed(&self) -> bool;
    /// How many items were scraped.
    pub fn len(&self) -> usize;
    /// Whether nothing was scraped.
    pub fn is_empty(&self) -> bool;
}

/// The scraped items, with the exporters Python's `ItemList` has.
#[derive(Debug, Clone, Default)]
pub struct Items(Vec<serde_json::Value>);

impl Items {
    /// An empty list.
    pub fn new() -> Items;
    /// Append an item.
    pub fn push(&mut self, item: serde_json::Value);
    /// Number of items.
    pub fn len(&self) -> usize;
    /// Whether there are no items.
    pub fn is_empty(&self) -> bool;
    /// Iterate over the items.
    pub fn iter(&self) -> std::slice::Iter<'_, serde_json::Value>;
    /// Write a JSON array; `indent` pretty-prints it.
    pub fn to_json(&self, path: impl AsRef<std::path::Path>, indent: bool) -> Result<()>;
    /// Write one JSON object per line.
    pub fn to_jsonl(&self, path: impl AsRef<std::path::Path>) -> Result<()>;
    /// Write a CSV; the columns default to every key seen, in first-seen order, and
    /// non-scalar values are written as JSON.
    pub fn to_csv(&self, path: impl AsRef<std::path::Path>, fields: Option<&[&str]>) -> Result<()>;
    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<serde_json::Value>;
}

impl std::ops::Deref for Items { type Target = [serde_json::Value]; }
impl IntoIterator for Items { type Item = serde_json::Value; }
impl FromIterator<serde_json::Value> for Items {}
```

XML export is out of scope for the first version.

### 6.9 The spider itself — `spider.rs`

```rust
/// Status codes that count as "we were blocked", as Python's `BLOCKED_CODES`.
pub const BLOCKED_CODES: &[u16] = &[401, 403, 407, 429, 444, 500, 502, 503, 504];

/// What a callback yields.
#[derive(Debug, Clone)]
pub enum Output {
    /// A scraped item.
    Item(serde_json::Value),
    /// Another request to schedule.
    Request(Request),
}

/// How a spider is allowed to crawl.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SpiderConfig {
    /// Requests in flight overall. Default 4.
    pub concurrent_requests: usize,
    /// Requests in flight per domain; 0 means "no per-domain limit". Default 0.
    pub concurrent_requests_per_domain: usize,
    /// Seconds to wait before each request to a domain. Default 0.0.
    pub download_delay: f64,
    /// How many times a blocked request is retried. Default 3.
    pub max_blocked_retries: u32,
    /// Enable adaptive per-domain delays. Default `None` (off).
    pub autothrottle: Option<AutoThrottleConfig>,
    /// Honour robots.txt. Default `false`.
    pub robots_txt_obey: bool,
    /// Cache responses to disk and replay them. Default `false`.
    pub development_mode: bool,
    /// Where the development cache lives. Default `.scrapling_cache/<spider name>`.
    pub dev_cache_dir: Option<std::path::PathBuf>,
    /// Include `RequestOptions` in the deduplication fingerprint. Default `false`.
    pub fp_include_kwargs: bool,
    /// Include headers in the deduplication fingerprint. Default `false`.
    pub fp_include_headers: bool,
    /// Keep URL fragments in the deduplication fingerprint. Default `false`.
    pub fp_keep_fragments: bool,
    /// Most requests allowed in the scheduler queue at once; `0` removes the ceiling.
    /// Default 100 000.
    pub max_queued_requests: usize,
    /// Most requests one crawl schedules in total; `0` removes the ceiling. Default 1 000 000.
    pub max_requests: usize,
    /// Most distinct domains per-domain state is kept for; `0` removes the ceiling.
    /// Default 10 000.
    pub max_tracked_domains: usize,
}

impl Default for SpiderConfig { /* the values above */ }

/// What a crawl does: where it starts and what it makes of each page.
#[async_trait::async_trait]
pub trait Spider: Send + Sync {
    /// The spider's name; used for logs and for the cache directory.
    fn name(&self) -> &str;

    /// The URLs the crawl starts from. Default: empty.
    fn start_urls(&self) -> Vec<String> { Vec::new() }

    /// Hosts (and their subdomains) the crawl may visit. Empty means no restriction.
    fn allowed_domains(&self) -> Vec<String> { Vec::new() }

    /// The crawl settings. Default: `SpiderConfig::default()`.
    fn config(&self) -> SpiderConfig { SpiderConfig::default() }

    /// The first requests. Default: a GET per `start_urls` entry handled by `parse`.
    async fn start_requests(&self) -> Vec<Request> { /* default impl */ }

    /// Turn a response into items and further requests.
    async fn parse(&self, response: Response) -> Result<Vec<Output>>;

    /// Dispatch a named callback. Default: everything goes to `parse`.
    async fn callback(&self, name: &str, response: Response) -> Result<Vec<Output>> {
        let _ = name;
        self.parse(response).await
    }

    /// Called once before the crawl starts; `resuming` is set when a checkpoint was loaded.
    async fn on_start(&self, resuming: bool) -> Result<()> { let _ = resuming; Ok(()) }

    /// Called once after the crawl finishes.
    async fn on_close(&self) -> Result<()> { Ok(()) }

    /// Called when a request fails or a callback returns an error.
    async fn on_error(&self, request: &Request, error: &Error) { let _ = (request, error); }

    /// Inspect or rewrite a scraped item; return `None` to drop it.
    async fn on_scraped_item(&self, item: serde_json::Value) -> Option<serde_json::Value> {
        Some(item)
    }

    /// Whether a response means we were blocked. Default: the status is in `BLOCKED_CODES`.
    async fn is_blocked(&self, response: &Response) -> bool {
        BLOCKED_CODES.contains(&response.status)
    }

    /// Prepare a blocked request before it is retried. Default: unchanged.
    async fn retry_blocked_request(&self, request: Request, response: &Response) -> Request {
        let _ = response;
        request
    }
}
```

### 6.10 Sessions — `session.rs`

```rust
/// One way of fetching a request.
#[derive(Debug, Clone)]
pub enum Session {
    /// A reusable HTTP session.
    Http(crate::http::FetcherSession),
    /// A browser session.
    #[cfg(feature = "browser")]
    Browser(crate::browser::DynamicSession),
}

/// The sessions a spider may fetch with, keyed by id.
#[derive(Debug, Default)]
pub struct SessionManager { /* private */ }

impl SessionManager {
    /// An empty manager.
    pub fn new() -> SessionManager;
    /// Register a session; the first one registered becomes the default.
    pub fn add(&mut self, id: impl Into<String>, session: Session) -> Result<()>;
    /// Register a session and make it the default.
    pub fn add_default(&mut self, id: impl Into<String>, session: Session) -> Result<()>;
    /// Remove a session and return it.
    pub fn remove(&mut self, id: &str) -> Option<Session>;
    /// The id requests fall back to when they name no session.
    pub fn default_session_id(&self) -> Result<&str>;
    /// Every registered session id.
    pub fn session_ids(&self) -> Vec<String>;
    /// Look a session up by id.
    pub fn get(&self, id: &str) -> Option<&Session>;
    /// Number of registered sessions.
    pub fn len(&self) -> usize;
    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool;
    /// Start every session that needs starting (browsers launch here).
    pub async fn start(&self) -> Result<()>;
    /// Close every session.
    pub async fn close(&self) -> Result<()>;
    /// Fetch a request with the session it names, or the default one.
    pub async fn fetch(&self, request: &Request) -> Result<Response>;
}
```

### 6.11 The engine — `engine.rs`

```rust
/// How the engine runs a spider.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EngineOptions {
    /// Where checkpoints are written; `None` disables pause/resume.
    pub crawldir: Option<std::path::PathBuf>,
    /// Seconds between periodic checkpoint saves. Default 300.0; 0 disables them.
    pub checkpoint_interval: f64,
}

/// Drives a spider: schedules, fetches, throttles, dispatches, and counts.
pub struct CrawlerEngine { /* private */ }

impl CrawlerEngine {
    /// Build an engine for a spider and its sessions.
    pub fn new(
        spider: std::sync::Arc<dyn Spider>,
        sessions: std::sync::Arc<SessionManager>,
        options: EngineOptions,
    ) -> CrawlerEngine;

    /// Run the crawl to completion (or to a pause) and return everything it produced.
    pub async fn crawl(&self) -> Result<CrawlResult>;

    /// Ask the crawl to stop once its in-flight requests finish. Calling it twice stops now.
    pub fn request_pause(&self);

    /// Run the crawl in the background and receive items as they are scraped.
    pub fn stream(&self) -> tokio::sync::mpsc::Receiver<serde_json::Value>;

    /// The counters as they stand right now; useful while streaming.
    pub fn stats(&self) -> CrawlStats;
}

/// Run a spider with a default HTTP session and default options.
pub async fn run(spider: std::sync::Arc<dyn Spider>) -> Result<CrawlResult>;
```

Engine behaviour, ported from `engine.py`, in the order it happens per request: robots check ->
per-domain delay (max of the spider's delay and robots' `Crawl-delay`) -> development-cache
lookup -> concurrency permit -> autothrottle delay -> fetch -> stats -> cache store ->
`is_blocked` check (retry with `priority - 1`, `dont_filter = true`, the proxy cleared, up to
`max_blocked_retries`) -> callback dispatch. Requests to hosts outside `allowed_domains` are
counted in `offsite_requests_count` and dropped.

### 6.12 Templates — `templates.rs`

Rust has no class inheritance, so Python's `CrawlSpider` becomes a helper function a spider
calls from its own `parse`.

```rust
/// One "extract these links and dispatch them" rule.
#[derive(Debug, Clone)]
pub struct CrawlRule {
    /// Which links to follow.
    pub link_extractor: LinkExtractor,
    /// Which callback handles them. Default `Callback::Parse`.
    pub callback: Callback,
    /// Override the priority of the requests produced.
    pub priority: Option<i32>,
}

impl CrawlRule {
    /// A rule that follows the extractor's links into the spider's `parse`.
    pub fn new(link_extractor: LinkExtractor) -> CrawlRule;
    /// Send the matched links to a named callback instead.
    pub fn callback(self, callback: Callback) -> Self;
    /// Give the produced requests a fixed priority.
    pub fn priority(self, priority: i32) -> Self;
}

/// Apply every rule to a response and return the requests to follow, deduplicated in order.
pub fn crawl_rules(response: &Response, rules: &[CrawlRule]) -> Vec<Request>;
```

---

## 7. `browser` (feature `browser`) — `src/browser/`

Port of Scrapling's dynamic (Playwright) engine onto `chromiumoxide`.

```rust
/// A live browser session that fetches pages through a real Chromium.
#[derive(Debug, Clone)]
pub struct DynamicSession { /* Arc of the browser handle + options */ }

impl DynamicSession {
    /// A session with the default options.
    pub fn new() -> DynamicSession;
    /// Start configuring a session.
    pub fn builder() -> DynamicSessionBuilder;
    /// Launch the browser. Must be called before `fetch`.
    pub async fn start(&self) -> Result<()>;
    /// Load a URL and return the rendered page. Only `http` and `https` URLs are loaded;
    /// anything else (`file:`, `view-source:`, `chrome:`, ...) is an `Error::Browser`.
    pub async fn fetch(&self, url: &str) -> Result<Response>;
    /// Shut the browser down.
    pub async fn close(&self) -> Result<()>;
    /// Whether the browser is running.
    pub fn is_alive(&self) -> bool;
}

/// Builder for `DynamicSession`.
#[derive(Debug, Clone, Default)]
pub struct DynamicSessionBuilder { /* private */ }

impl DynamicSessionBuilder {
    /// Run without a visible window. Default `true`.
    pub fn headless(self, yes: bool) -> Self;
    /// Block fonts, images, media and other non-essential resources. Default `false`.
    pub fn disable_resources(self, yes: bool) -> Self;
    /// Abort requests to these domains.
    pub fn blocked_domains(self, domains: &[&str]) -> Self;
    /// Block known ad and tracker domains. Default `false`.
    pub fn block_ads(self, yes: bool) -> Self;
    /// How long a navigation may take. Default 30s.
    pub fn timeout(self, timeout: std::time::Duration) -> Self;
    /// Extra wait after the page settles. Default zero.
    pub fn wait(self, wait: std::time::Duration) -> Self;
    /// Wait for this CSS selector before returning.
    pub fn wait_selector(self, selector: &str) -> Self;
    /// Which state that selector must reach.
    pub fn wait_selector_state(self, state: WaitState) -> Self;
    /// Wait for the network to go idle. Default `false`.
    pub fn network_idle(self, yes: bool) -> Self;
    /// Override the user agent.
    pub fn useragent(self, useragent: &str) -> Self;
    /// Headers added to every navigation.
    pub fn extra_headers(self, headers: HeaderMap) -> Self;
    /// Route the browser through a proxy.
    pub fn proxy(self, proxy: &str) -> Self;
    /// Extra Chromium command-line flags.
    pub fn extra_flags(self, flags: &[&str]) -> Self;
    /// Add `STEALTH_ARGS` and drop `HARMFUL_ARGS`. Default `true`.
    pub fn stealth(self, yes: bool) -> Self;
    /// Build the session.
    pub fn build(self) -> DynamicSession;
    /// The options this builder describes.
    pub fn options(self) -> DynamicSessionOptions;
}

/// The resolved options of a `DynamicSession`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DynamicSessionOptions {
    pub headless: bool,
    pub disable_resources: bool,
    pub blocked_domains: Vec<String>,
    pub block_ads: bool,
    pub timeout: std::time::Duration,
    pub wait: std::time::Duration,
    pub wait_selector: Option<String>,
    pub wait_selector_state: WaitState,
    pub network_idle: bool,
    pub useragent: Option<String>,
    pub extra_headers: HeaderMap,
    pub proxy: Option<String>,
    pub extra_flags: Vec<String>,
    pub stealth: bool,
}

impl Default for DynamicSessionOptions { /* headless: true, stealth: true, timeout: 30s, ... */ }

/// What `wait_selector` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitState {
    /// Present in the DOM.
    #[default]
    Attached,
    /// Removed from the DOM.
    Detached,
    /// Present and rendered.
    Visible,
    /// Present but not rendered.
    Hidden,
}

/// One-shot fetching: launch a browser, load a page, shut it down.
pub struct DynamicFetcher;

impl DynamicFetcher {
    /// Fetch one URL with a throwaway browser session.
    pub async fn fetch(url: &str, options: DynamicSessionOptions) -> Result<Response>;
}

/// Chromium flags Scrapling always passes.
pub const DEFAULT_ARGS: &[&str] = &[/* constants.py DEFAULT_ARGS */];
/// Chromium flags added when stealth is on.
pub const STEALTH_ARGS: &[&str] = &[/* constants.py STEALTH_ARGS */];
/// Chromium flags that give automation away; never passed.
pub const HARMFUL_ARGS: &[&str] = &[/* constants.py HARMFUL_ARGS */];
/// Resource types blocked by `disable_resources`.
pub const EXTRA_RESOURCES: &[&str] = &[/* constants.py EXTRA_RESOURCES */];
```

Copy the four constant lists verbatim from
`Scrapling/scrapling/engines/constants.py`.

**Out of scope for this module:** there is no Cloudflare / anti-bot challenge solver, and none
is planned. Scrapling's `StealthySession` "solve Cloudflare" path is deliberately not ported;
`stealth(true)` only applies the command-line flags above.

---

## 8. Tests and examples

* `tests/<module>.rs` — one integration test file per module, owned by that module's owner.
  HTTP tests use `wiremock`; storage tests use `tempfile`. Browser tests are behind
  `#[cfg(feature = "browser")]` and are skipped when no Chromium is installed.
* `examples/<name>.rs` — small, runnable, one topic each: `parse.rs`, `fetch.rs`,
  `adaptive.rs`, `spider.rs`.
* Use the HTML document from `Scrapling/docs/overview.md` as the shared parser fixture, so the
  Rust results can be diffed against the documented Python output.

---

## 9. Additions made while implementing

Everything in sections 1–8 is implemented with the signatures given there. The items below are
public API that the implementation added and that the sections above do not mention. They are
all additive — no signature in sections 1–8 changed — but this file is the source of truth for
the public surface, so they are recorded here.

Two cosmetic differences are *not* drift and are not listed item by item: builder setters are
written `fn name(mut self, …) -> Self` in the code where the sections write `self`, and the
code names imported types (`Path`, `Duration`, `HashSet`) where the sections write them fully
qualified. Both compile to the same public API.

### 9.1 `error` — `src/error.rs`

Section 0 points at this file without spelling it out. It is a shared file, so the enum is
fixed here too: add a variant rather than a new error type.

```rust
/// Everything that can go wrong inside `rustscrapling`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A CSS/XPath-like selector could not be parsed.
    Selector { selector: String, message: String },
    /// The adaptive-element storage backend failed.
    Storage(String),
    /// An HTTP request failed, timed out, or was rejected.
    Http(String),
    /// The browser engine failed to start, navigate, or evaluate.
    Browser(String),
    /// The crawl framework failed (scheduler, session, checkpoint, ...).
    Spider(String),
    /// A filesystem or other I/O operation failed.
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization failed.
    Serde(#[from] serde_json::Error),
    /// Anything that does not fit the variants above.
    Other(String),
}

impl Error {
    /// Build an `Error::Selector` from a selector string and a message.
    pub fn selector(selector: impl Into<String>, message: impl Into<String>) -> Self;
    /// Build an `Error::Other` from anything displayable.
    pub fn other(message: impl std::fmt::Display) -> Self;
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
```

`src/lib.rs` also sets `#![deny(unsafe_code)]`, which makes the ground rule "no `unsafe` without
a sign-off" a compile error. The one reviewed exception is the `unsafe impl Sync for Document` in
`parser::selector`, which opts out explicitly and carries its safety argument in place.

### 9.2 `text` — `src/text.rs`

```rust
impl Text {
    /// Like `re_matches` but compiles `pattern` first, so it can fail on a bad pattern.
    pub fn re_matches_str(&self, pattern: &str, opts: ReOptions) -> Result<bool>;
}

/// Borrowing iterator over an `Attributes` map, yielding `(name, value)` in document order.
///
/// The named type exists so `&Attributes` can implement `IntoIterator`; `Attributes::iter`
/// still returns `impl Iterator` as section 1 specifies.
#[derive(Debug, Clone)]
pub struct AttributesIter<'a>;
```

### 9.3 `response` — `src/response.rs`

```rust
/// Meta key under which the crawl engine stores the session id a response was fetched with.
pub const META_SID: &str = "scrapling.sid";
/// Meta key under which the crawl engine stores the `Callback` of the originating request.
pub const META_CALLBACK: &str = "scrapling.callback";
/// Meta key under which the crawl engine stores the originating request's priority.
pub const META_PRIORITY: &str = "scrapling.priority";
/// Meta key under which the HTTP fetcher records the proxy a request went through.
pub const META_PROXY: &str = "proxy";

impl Cookie {
    /// Parse one `Set-Cookie` header value.
    pub fn parse_set_cookie(value: &str, default_domain: &str, default_path: &str) -> Option<Cookie>;
}

/// The charset named by a `Content-Type`, lower-cased, defaulting to `utf-8`.
pub fn encoding_from_content_type(content_type: Option<&str>) -> String;
```

These constants are the string keys the engine and the fetcher already write into
`Response::meta`; naming them keeps callers from re-typing the literals.

### 9.4 `adaptive` — `src/adaptive/`

```rust
/// The default similarity threshold, as a percentage — Python's `percentage=40`.
pub const DEFAULT_PERCENTAGE: f64 = 40.0;

impl ElementFingerprint {
    /// One stored attribute by name.
    pub fn attribute(&self, name: &str) -> Option<&str>;
    /// Whether a parent was recorded.
    pub fn has_parent(&self) -> bool;
}
```

### 9.5 `http` — `src/http/`

```rust
/// Whether this address is one `FollowRedirects::Safe` refuses to be redirected to.
pub fn is_blocked_ip(ip: std::net::IpAddr) -> bool;
/// Whether this redirect target is blocked: a non-HTTP scheme, or a private/loopback host.
pub fn is_blocked_redirect_target(url: &url::Url) -> bool;
/// The next proxy in round-robin order, and the index to keep for the call after it.
pub fn cyclic_rotation(proxies: &[String], current_index: usize) -> (String, usize);

impl BrowserProfile {
    /// The name of the profile, as used in log lines and `Debug` output.
    pub fn name(&self) -> &'static str;
}

impl RequestBuilder {
    /// Send this one request without a browser profile's headers.
    pub fn no_impersonate(self) -> Self;
    /// Refuse a response body larger than `max` bytes.
    pub fn max_response_bytes(self, max: usize) -> Self;
}
```

`is_blocked_ip` and `is_blocked_redirect_target` are the redirect guard from section 5 exposed
as free functions, so the same rule can be applied outside a fetcher.

### 9.6 `spider` — `src/spider/`

```rust
impl RequestOptions {
    /// Options with every field at its default.
    pub fn new() -> RequestOptions;
    pub fn method(self, method: impl Into<String>) -> Self;
    pub fn header(self, name: impl Into<String>, value: impl Into<String>) -> Self;
    pub fn form<I, K, V>(self, fields: I) -> Self
    where I: IntoIterator<Item = (K, V)>, K: Into<String>, V: Into<String>;
    pub fn json(self, value: serde_json::Value) -> Self;
    pub fn body(self, body: impl Into<Vec<u8>>) -> Self;
    pub fn proxy(self, proxy: impl Into<String>) -> Self;
    pub fn timeout(self, timeout: std::time::Duration) -> Self;
    /// The method to send, defaulting to `GET`.
    pub fn method_or_get(&self) -> &str;
}

impl Callback {
    /// A `Callback::Named` from anything string-like.
    pub fn named(name: impl Into<String>) -> Callback;
    /// The method name this callback dispatches to (`"parse"` for `Callback::Parse`).
    pub fn name(&self) -> &str;
}

impl LinkExtractor {
    /// Strip whitespace from extracted URLs. Python's `strip=True`.
    pub fn strip(self, yes: bool) -> Self;
}

impl CheckpointData {
    /// Build checkpoint data from a scheduler snapshot.
    pub fn from_snapshot(requests: Vec<Request>, seen: std::collections::HashSet<[u8; 20]>) -> CheckpointData;
    /// The seen-fingerprint set, decoded back from its stored form.
    pub fn seen_fingerprints(&self) -> std::collections::HashSet<[u8; 20]>;
}

impl ResponseCache {
    /// The directory this cache writes to.
    pub fn dir(&self) -> &std::path::Path;
}

impl AutoThrottle {
    /// The configuration this throttle was built with.
    pub fn config(&self) -> AutoThrottleConfig;
}

impl Items {
    /// Drop every collected item.
    pub fn clear(&mut self);
}

impl EngineOptions {
    pub fn crawldir(self, crawldir: impl Into<std::path::PathBuf>) -> Self;
    pub fn checkpoint_interval(self, interval: f64) -> Self;
}

impl CrawlerEngine {
    /// Whether the crawl is currently paused.
    pub fn paused(&self) -> bool;
}
```

`RequestOptions` is `#[non_exhaustive]`, so a struct literal cannot be written outside the
crate; the builder setters are what make the type constructible at all, and the public fields
of section 6.1 are unchanged for reading and for in-crate construction.

### 9.7 `browser` — `src/browser/`

```rust
/// The ad/tracker hosts blocked when `block_ads` is on.
pub const AD_DOMAINS: &[&str];

/// A resource type as Chromium spells it, normalized to Playwright's lower-case name.
pub fn normalize_resource_type(resource_type: &str) -> String;
/// Whether this resource type is dropped when `disable_resources` is on.
pub fn is_blocked_resource(resource_type: &str) -> bool;
/// Whether `hostname`, or any of its parent domains, is in `domains`.
pub fn is_domain_blocked(hostname: &str, domains: &std::collections::HashSet<String>) -> bool;
/// The Chromium command line these options describe, in the order Chromium receives it.
pub fn launch_args(options: &DynamicSessionOptions) -> Result<Vec<String>>;

/// The blocking rules of one session, applied to every intercepted request.
#[derive(Debug, Clone, Default)]
pub struct RequestFilter;

impl RequestFilter {
    pub fn new(disable_resources: bool, blocked_domains: impl IntoIterator<Item = String>) -> Self;
    /// Whether this filter blocks anything at all; when it does not, interception stays off.
    pub fn is_active(&self) -> bool;
    pub fn disable_resources(&self) -> bool;
    pub fn blocked_domains(&self) -> &std::collections::HashSet<String>;
    /// Whether this request must be aborted.
    pub fn should_block(&self, url: &str, resource_type: &str) -> bool;
}

/// A proxy URL split the way Chromium wants it: server, then credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    /// `scheme://host[:port]`, with any credentials stripped.
    pub server: String,
    /// The user name, or `""`.
    pub username: String,
    /// The password, or `""`.
    pub password: String,
}

impl ProxyConfig {
    /// Parse a proxy URL, rejecting anything Chromium cannot route through.
    pub fn parse(proxy: &str) -> Result<ProxyConfig>;
}

impl DynamicSession {
    /// The blocking rules these options describe.
    pub fn request_filter(&self) -> RequestFilter;
}
```

These are the pieces of the session that are testable without a browser: the flag list, the
proxy split and the blocking predicates are pure functions, so `tests/` can check them on a
machine with no Chromium installed.
