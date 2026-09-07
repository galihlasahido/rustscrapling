//! [`Selector`] — the port of Python Scrapling's `Selector`/`Adaptor`.

use std::fmt;
use std::sync::Arc;

use ego_tree::{NodeId, NodeRef};
use scraper::node::Element as HtmlElement;
use scraper::{CaseSensitivity, ElementRef, Html, Node};

use crate::error::{Error, Result};
use crate::parser::css::{self, CompiledQuery, Pseudo};
use crate::parser::filter::Filter;
use crate::parser::generate;
use crate::parser::selectors::Selectors;
use crate::text::{Attributes, ReOptions, Text, Texts};

/// How deep [`Selector::prettify`] re-indents before falling back to plain serialization.
///
/// `prettify` is the only recursive walk in this module and the documents it walks come off the
/// network, so the depth is capped rather than trusted: a page nesting a few hundred thousand
/// `<div>`s is cheap to serve and would otherwise overflow the stack.
const MAX_PRETTY_DEPTH: usize = 256;

/// How many characters of a value [`Selector::find_similar`] hands to the fuzzy matcher.
///
/// The diff behind `find_similar` costs roughly `O(len(a) * len(b))`, and both sides come from
/// remote markup where an attribute value or a text run can be megabytes long. Both sides are
/// cut the same way, so an element still scores 1.0 against itself.
const MAX_DIFF_CHARS: usize = 512;

/// A parsed document, shared by every [`Selector`] pointing into it.
#[derive(Debug)]
struct Document {
    /// The parsed tree.
    html: Html,
    /// The URL the document was fetched from, or `""`.
    url: String,
    /// The bytes the document was parsed from.
    body: Vec<u8>,
    /// The node the document's own [`Selector`] points at — normally `<html>`.
    root: NodeId,
}

impl Document {
    /// Take ownership of a freshly parsed tree and freeze it.
    ///
    /// `scraper::node::Element` memoizes its `id` and its class list in two `OnceCell`s, filled
    /// the first time [`scraper::node::Element::id`] or [`scraper::node::Element::classes`] is
    /// called through a shared reference. Filling them here, once, while the tree is still
    /// owned by this function, is what lets the `unsafe impl Sync` below be sound: afterwards
    /// nothing in the tree can change through a `&Document`.
    fn new(html: Html, url: String, body: Vec<u8>, root: NodeId) -> Document {
        for node in html.tree.values() {
            if let Node::Element(element) = node {
                let _ = element.id();
                let _ = element.classes().count();
            }
        }

        Document {
            html,
            url,
            body,
            root,
        }
    }
}

/// `Html` is only `Send` when `scraper` is built with its `atomic` feature, which swaps the
/// tree's strings from a `Cell`-refcounted tendril to an `AtomicUsize`-refcounted one. The
/// feature is enabled in `Cargo.toml`; this assertion turns removing it into one readable
/// compile error here instead of an unsound `unsafe impl Sync` below.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Html>();
};

// SAFETY: a `Document` is only ever built by `Document::new`, is never mutated afterwards (no
// method in this crate takes a `&mut Document`, and it is reachable only through `Arc`), and its
// two sources of interior mutability are handled:
//
// * `scraper::node::Element`'s `id`/`classes` `OnceCell`s are all initialized by
//   `Document::new` before the value is shared, so every later `get_or_init` is a plain read.
// * every string in the tree is a `Tendril<UTF8, Atomic>` (scraper's `atomic` feature, asserted
//   above), whose refcount is an `AtomicUsize`, so cloning one out of the tree from two threads
//   at once is race-free.
//
// This is the crate's only `unsafe`. It exists because `Response` — which owns a `Selector` —
// is moved between `tokio` tasks by the spider engine, and `Arc<T>: Send` requires `T: Sync`.
//
// Both bullets are claims about crate-internal details of `scraper` and `ego-tree`, which no
// compiler check can restate: the `assert_send` above only catches the `atomic` feature going
// away, not a new `Cell`, `OnceCell` or internal cache appearing on a tree node. `Cargo.toml`
// therefore pins `scraper = "=0.27.0"` and `ego-tree = "=0.11.0"` exactly. Moving either pin
// means re-reading `scraper::node::Element`, `scraper::Html` and `ego_tree::Tree`/`NodeRef` for
// interior mutability first, and extending `Document::new` to pre-fill anything new, before the
// version in `Cargo.toml` is changed.
#[allow(unsafe_code)]
unsafe impl Sync for Document {}

/// What a [`Selector`] points at.
///
/// `::text` / `::attr()` results are `Selector`s too, so the API mirrors Python's
/// `_ElementUnicodeResult` handling: every element-only method degrades on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorKind {
    /// A real element node.
    Element,
    /// A text node produced by `::text` or by a text child.
    TextNode(Text),
    /// An attribute value produced by `::attr(name)`.
    AttrNode {
        /// The attribute's name.
        name: String,
        /// The attribute's value.
        value: Text,
    },
}

/// A node in a parsed HTML document: an element, a text node, or an attribute value.
///
/// Cloning is cheap — a `Selector` is an `Arc` to the shared document plus a node id — so
/// passing them around and building [`Selectors`] lists costs almost nothing.
///
/// ```no_run
/// # fn main() -> rustscrapling::Result<()> {
/// use rustscrapling::Selector;
///
/// let page = Selector::new("<article class=\"product\"><h3>Product 1</h3></article>")?;
/// let title = page.css_first("h3")?.expect("an h3");
/// assert_eq!(title.text(), "Product 1");
/// assert_eq!(title.parent().map(|p| p.tag().to_string()), Some("article".to_string()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Selector {
    doc: Arc<Document>,
    id: NodeId,
    kind: SelectorKind,
}

impl Selector {
    /// Parse an HTML document.
    pub fn new(html: &str) -> Result<Selector> {
        Selector::build(html, "", false)
    }

    /// Parse an HTML document and remember the URL it came from (used by `urljoin`).
    pub fn with_url(html: &str, url: &str) -> Result<Selector> {
        Selector::build(html, url, false)
    }

    /// Parse an HTML fragment rather than a full document.
    pub fn fragment(html: &str) -> Result<Selector> {
        Selector::build(html, "", true)
    }

    fn build(input: &str, url: &str, fragment: bool) -> Result<Selector> {
        // Python does `content.strip().replace("\x00", "") or "<html/>"`; NUL bytes upset the
        // parsers and an empty body has to stay queryable rather than blow up.
        let cleaned = input.trim().replace('\0', "");
        let source: &str = if cleaned.is_empty() {
            "<html></html>"
        } else {
            cleaned.as_str()
        };

        let html = if fragment {
            Html::parse_fragment(source)
        } else {
            Html::parse_document(source)
        };
        let root = root_node_id(&html);

        Ok(Selector {
            doc: Arc::new(Document::new(
                html,
                url.to_string(),
                input.as_bytes().to_vec(),
                root,
            )),
            id: root,
            kind: SelectorKind::Element,
        })
    }

    // -- internals ---------------------------------------------------------------------

    fn node(&self) -> Option<NodeRef<'_, Node>> {
        self.doc.html.tree.get(self.id)
    }

    /// The element this selector points at, or `None` for text/attribute results.
    fn element(&self) -> Option<ElementRef<'_>> {
        if self.is_text_node() {
            return None;
        }
        self.node().and_then(ElementRef::wrap)
    }

    fn wrap_element(&self, id: NodeId) -> Selector {
        Selector {
            doc: Arc::clone(&self.doc),
            id,
            kind: SelectorKind::Element,
        }
    }

    fn wrap_text(&self, id: NodeId, value: &str) -> Selector {
        Selector {
            doc: Arc::clone(&self.doc),
            id,
            kind: SelectorKind::TextNode(Text::new(value)),
        }
    }

    fn wrap_attr(&self, id: NodeId, name: &str, value: &str) -> Selector {
        Selector {
            doc: Arc::clone(&self.doc),
            id,
            kind: SelectorKind::AttrNode {
                name: name.to_string(),
                value: Text::new(value),
            },
        }
    }

    /// Whether both selectors point at the very same node of the very same document.
    pub(crate) fn is_same_node(&self, other: &Selector) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.doc, &other.doc)
    }

    /// Every element strictly below this one, in document order, without building a list first.
    ///
    /// [`Selector::below_elements`] is this iterator collected; the searches that can stop at
    /// their first hit use the iterator instead, so a hostile page cannot make them allocate one
    /// `Selector` per element before the first match is even looked at.
    fn descendant_elements(&self) -> impl Iterator<Item = Selector> + '_ {
        let id = self.id;
        self.element()
            .into_iter()
            .flat_map(|element| element.descendants())
            .filter(move |node| node.id() != id && node.value().is_element())
            .map(move |node| self.wrap_element(node.id()))
    }

    // -- identity ----------------------------------------------------------------------

    /// The URL this document was fetched from, or `""`.
    pub fn url(&self) -> &str {
        &self.doc.url
    }

    /// What this selector points at.
    pub fn kind(&self) -> &SelectorKind {
        &self.kind
    }

    /// Whether this is a `::text` / `::attr()` result rather than an element.
    pub fn is_text_node(&self) -> bool {
        !matches!(self.kind, SelectorKind::Element)
    }

    /// The tag name; `"#text"` for text nodes.
    pub fn tag(&self) -> &str {
        if self.is_text_node() {
            return "#text";
        }
        self.node()
            .and_then(|node| node.value().as_element())
            .map(HtmlElement::name)
            .unwrap_or("")
    }

    // -- content -----------------------------------------------------------------------

    /// The element's own (direct) text, or the value for text/attr nodes.
    ///
    /// This is lxml's `element.text`: the text that opens the element, not the text of its
    /// descendants. A whitespace-only run reads as empty, matching the `remove_blank_text`
    /// parser option Scrapling turns on.
    pub fn text(&self) -> Text {
        match &self.kind {
            SelectorKind::TextNode(value) => value.clone(),
            SelectorKind::AttrNode { value, .. } => value.clone(),
            SelectorKind::Element => Text::new(self.node().map(direct_text).unwrap_or_default()),
        }
    }

    /// All descendant text joined by `sep`.
    ///
    /// Text under any of `ignore_tags` is dropped, and so is any run that is only whitespace.
    /// Scrapling's defaults are `sep = "\n"`, `strip = false`,
    /// `ignore_tags = ["script", "style"]`.
    pub fn get_all_text(&self, sep: &str, strip: bool, ignore_tags: &[&str]) -> Text {
        if self.is_text_node() {
            return self.text();
        }
        let Some(root) = self.node() else {
            return Text::default();
        };

        // Built straight into one buffer rather than into a `Vec<String>` that is joined
        // afterwards: the input is a whole remote document, so the intermediate list would be a
        // second copy of every string on the page.
        let mut out = String::new();
        let mut written = false;
        for descendant in root.descendants() {
            let Some(raw) = descendant.value().as_text() else {
                continue;
            };
            let raw: &str = raw;
            let processed = if strip { raw.trim() } else { raw };
            // Python parses with `remove_blank_text=True`, so a whitespace-only run is not a
            // text node there at all; `valid_values=True` then drops the rest.
            if processed.trim().is_empty() {
                continue;
            }
            if is_under_ignored_tag(descendant, root.id(), ignore_tags) {
                continue;
            }
            if written {
                out.push_str(sep);
            }
            out.push_str(processed);
            written = true;
        }

        Text::new(out)
    }

    /// The element's attributes; empty for text nodes.
    pub fn attrib(&self) -> Attributes {
        match self.element() {
            Some(element) => Attributes::new(
                element
                    .value()
                    .attrs()
                    .map(|(name, value)| (name.to_string(), Text::new(value))),
            ),
            None => Attributes::default(),
        }
    }

    /// The value of a single attribute.
    pub fn attr(&self, name: &str) -> Option<Text> {
        self.element()
            .and_then(|element| element.value().attr(name))
            .map(Text::new)
    }

    /// Whether the element carries `class_name` in its class list.
    pub fn has_class(&self, class_name: &str) -> bool {
        self.element()
            .map(|element| {
                element
                    .value()
                    .has_class(class_name, CaseSensitivity::CaseSensitive)
            })
            .unwrap_or(false)
    }

    /// The element's inner+outer HTML (Python's `html_content`).
    pub fn html_content(&self) -> Text {
        if self.is_text_node() {
            return self.text();
        }
        Text::new(
            self.element()
                .map(|element| element.html())
                .unwrap_or_default(),
        )
    }

    /// A pretty-printed version of [`Selector::html_content`].
    pub fn prettify(&self) -> Text {
        if self.is_text_node() {
            return self.text();
        }
        let Some(node) = self.node() else {
            return Text::default();
        };
        let mut out = String::new();
        pretty_print(node, 0, &mut out);
        Text::new(out.trim_end().to_string())
    }

    /// The raw body the document was parsed from; empty for nodes below the root.
    pub fn body(&self) -> &[u8] {
        if self.is_text_node() || self.id != self.doc.root {
            return &[];
        }
        &self.doc.body
    }

    /// Join `relative_url` onto this selector's URL.
    ///
    /// With no URL on the document this returns `relative_url` unchanged, matching Python's
    /// `urljoin("", relative)`.
    pub fn urljoin(&self, relative_url: &str) -> Result<String> {
        let base = self.url();
        if base.is_empty() {
            return Ok(relative_url.to_string());
        }
        let parsed = url::Url::parse(base)
            .map_err(|error| Error::other(format!("invalid base url `{base}`: {error}")))?;
        parsed
            .join(relative_url)
            .map(String::from)
            .map_err(|error| {
                Error::other(format!(
                    "cannot join `{relative_url}` onto `{base}`: {error}"
                ))
            })
    }

    // -- navigation --------------------------------------------------------------------

    /// The parent element, or `None` at the root.
    ///
    /// An `::attr()` result reports the element that carries the attribute, and a `::text`
    /// result reports the element that contains it — the same as lxml's smart strings.
    pub fn parent(&self) -> Option<Selector> {
        if matches!(self.kind, SelectorKind::AttrNode { .. }) {
            return Some(self.wrap_element(self.id));
        }
        let parent = self.node()?.parent()?;
        if parent.value().is_element() {
            Some(self.wrap_element(parent.id()))
        } else {
            None
        }
    }

    /// The direct child elements (comments and processing instructions skipped).
    pub fn children(&self) -> Selectors {
        let Some(element) = self.element() else {
            return Selectors::default();
        };
        Selectors::new(
            element
                .children()
                .filter(|child| child.value().is_element())
                .map(|child| self.wrap_element(child.id())),
        )
    }

    /// The parent's other children.
    pub fn siblings(&self) -> Selectors {
        if self.is_text_node() {
            return Selectors::default();
        }
        let Some(parent) = self.parent() else {
            return Selectors::default();
        };
        Selectors::new(
            parent
                .children()
                .into_vec()
                .into_iter()
                .filter(|child| !child.is_same_node(self)),
        )
    }

    /// The next sibling element, or `None`.
    pub fn next(&self) -> Option<Selector> {
        let mut sibling = self.element()?.next_sibling();
        while let Some(node) = sibling {
            if node.value().is_element() {
                return Some(self.wrap_element(node.id()));
            }
            sibling = node.next_sibling();
        }
        None
    }

    /// The previous sibling element, or `None`.
    pub fn previous(&self) -> Option<Selector> {
        let mut sibling = self.element()?.prev_sibling();
        while let Some(node) = sibling {
            if node.value().is_element() {
                return Some(self.wrap_element(node.id()));
            }
            sibling = node.prev_sibling();
        }
        None
    }

    /// Every ancestor, closest first.
    pub fn iterancestors(&self) -> impl Iterator<Item = Selector> + '_ {
        let mut current = if self.is_text_node() {
            None
        } else {
            self.parent()
        };
        std::iter::from_fn(move || {
            let ancestor = current.take()?;
            current = ancestor.parent();
            Some(ancestor)
        })
    }

    /// The first ancestor for which `pred` returns true.
    pub fn find_ancestor(&self, pred: impl Fn(&Selector) -> bool) -> Option<Selector> {
        self.iterancestors().find(|ancestor| pred(ancestor))
    }

    /// The ancestors as a [`Selectors`] list (Python's `path`).
    pub fn path(&self) -> Selectors {
        Selectors::new(self.iterancestors())
    }

    /// Every element below this one, in document order.
    pub fn below_elements(&self) -> Selectors {
        Selectors::new(self.descendant_elements())
    }

    // -- querying ----------------------------------------------------------------------

    /// Query with a CSS selector.
    ///
    /// Supports `::text`, `::attr(name)` and comma-separated lists; the members of a list are
    /// evaluated in the order they were written and their results concatenated. Like lxml's
    /// `descendant-or-self::` prefix, this selector itself can match.
    pub fn css(&self, selector: &str) -> Result<Selectors> {
        // Python bails out before the selector is even parsed, so a broken selector run against
        // a `::text` result is an empty list there rather than an error. Same here.
        if self.is_text_node() {
            return Ok(Selectors::default());
        }
        let queries = css::compile(selector)?;
        Ok(self.css_compiled(&queries))
    }

    /// Run already-compiled queries against this node.
    ///
    /// Split out of [`Selector::css`] so that [`Selectors::css`] compiles a selector once for a
    /// whole list instead of once per element.
    pub(super) fn css_compiled(&self, queries: &[CompiledQuery]) -> Selectors {
        if self.is_text_node() {
            return Selectors::default();
        }
        let Some(scope) = self.element() else {
            return Selectors::default();
        };

        let mut out: Vec<Selector> = Vec::new();
        for query in queries {
            // `cssselect` translates with a `descendant-or-self::` prefix, so the context node
            // itself is a candidate and comes first in document order.
            let mut matched: Vec<ElementRef<'_>> = Vec::new();
            if query.selector.matches(&scope) {
                matched.push(scope);
            }
            matched.extend(scope.select(&query.selector));

            for element in matched {
                match &query.pseudo {
                    Pseudo::None => out.push(self.wrap_element(element.id())),
                    Pseudo::Text => {
                        for child in element.children() {
                            if let Some(value) = child.value().as_text() {
                                // lxml parses with `remove_blank_text=True`, so a
                                // whitespace-only run is not a text node in Python and
                                // `/text()` never yields one.
                                if value.trim().is_empty() {
                                    continue;
                                }
                                out.push(self.wrap_text(child.id(), value));
                            }
                        }
                    }
                    Pseudo::Attr(name) => {
                        if let Some(value) = element.value().attr(name) {
                            out.push(self.wrap_attr(element.id(), name, value));
                        }
                    }
                }
            }
        }

        Selectors::from(out)
    }

    /// Query with a CSS selector and return only the first match.
    pub fn css_first(&self, selector: &str) -> Result<Option<Selector>> {
        Ok(self.css(selector)?.into_vec().into_iter().next())
    }

    /// Every descendant matching `filter`.
    pub fn find_all(&self, filter: &Filter) -> Result<Selectors> {
        if self.is_text_node() {
            return Ok(Selectors::default());
        }
        filter.apply(self)
    }

    /// The first descendant matching `filter`.
    pub fn find(&self, filter: &Filter) -> Result<Option<Selector>> {
        Ok(self.find_all(filter)?.into_vec().into_iter().next())
    }

    /// Elements whose own text matches `text`.
    ///
    /// Scrapling's defaults are `first_match = true`, `partial = false`,
    /// `case_sensitive = false`, `clean_match = true`.
    pub fn find_by_text(
        &self,
        text: &str,
        first_match: bool,
        partial: bool,
        case_sensitive: bool,
        clean_match: bool,
    ) -> Selectors {
        if self.is_text_node() {
            return Selectors::default();
        }

        let wanted = if case_sensitive {
            text.to_string()
        } else {
            text.to_lowercase()
        };

        let mut results: Vec<Selector> = Vec::new();
        // Python narrows the candidates with `.//*[normalize-space(text())]` first; skipping the
        // elements whose own text is blank is the same set.
        for node in self.descendant_elements() {
            let candidate = normalized_text(&node, case_sensitive, clean_match);
            if candidate.is_empty() {
                continue;
            }
            let hit = if partial {
                candidate.contains(&wanted)
            } else {
                candidate == wanted
            };
            if hit {
                results.push(node);
                if first_match {
                    break;
                }
            }
        }

        Selectors::from(results)
    }

    /// Elements whose own text matches the regex `pattern`.
    ///
    /// Scrapling's defaults are `first_match = true`, `case_sensitive = false`,
    /// `clean_match = true`.
    pub fn find_by_regex(
        &self,
        pattern: &str,
        first_match: bool,
        case_sensitive: bool,
        clean_match: bool,
    ) -> Result<Selectors> {
        if self.is_text_node() {
            return Ok(Selectors::default());
        }

        let compiled = regex::RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|error| Error::other(format!("invalid regex `{pattern}`: {error}")))?;

        let mut results: Vec<Selector> = Vec::new();
        for node in self.descendant_elements() {
            // Case is baked into the compiled pattern, so the text itself is left alone.
            let candidate = normalized_text(&node, true, clean_match);
            if candidate.is_empty() {
                continue;
            }
            if compiled.is_match(&candidate) {
                results.push(node);
                if first_match {
                    break;
                }
            }
        }

        Ok(Selectors::from(results))
    }

    /// Sibling-shaped elements that look like this one (Python's `find_similar`).
    ///
    /// A candidate must first share this element's tag, its parent's tag, its grandparent's
    /// tag and its ancestor depth; only then are the attributes scored, with
    /// `similar::TextDiff` standing in for Python's `difflib.SequenceMatcher`. The score's
    /// denominator is `max(len(original), len(candidate))`, so a candidate carrying extra
    /// attributes is penalised and one carrying fewer is not flattered.
    ///
    /// Scrapling's defaults are `threshold = 0.2`, `ignore_attributes = ["href", "src"]`,
    /// `match_text = false`.
    pub fn find_similar(
        &self,
        threshold: f64,
        ignore_attributes: &[&str],
        match_text: bool,
    ) -> Selectors {
        let Some(element) = self.element() else {
            return Selectors::default();
        };

        let depth = element_ancestor_count(element);
        let tag = element.value().name().to_string();
        let parent = element_parent(element);
        let parent_tag = parent.map(|node| node.value().name().to_string());
        let grandparent_tag = parent
            .and_then(element_parent)
            .map(|node| node.value().name().to_string());

        let original_attrs = filtered_attrs(element.value(), ignore_attributes);
        let original_text = if match_text {
            self.text().clean().into_string()
        } else {
            String::new()
        };

        let mut similar: Vec<Selector> = Vec::new();
        for node in self.doc.html.tree.root().descendants() {
            if node.id() == self.id {
                continue;
            }
            let Some(candidate) = ElementRef::wrap(node) else {
                continue;
            };
            if candidate.value().name() != tag {
                continue;
            }
            if let Some(wanted) = &parent_tag {
                match element_parent(candidate) {
                    Some(found) if found.value().name() == wanted => {}
                    _ => continue,
                }
            }
            if let Some(wanted) = &grandparent_tag {
                match element_parent(candidate).and_then(element_parent) {
                    Some(found) if found.value().name() == wanted => {}
                    _ => continue,
                }
            }
            if element_ancestor_count(candidate) != depth {
                continue;
            }

            let candidate_attrs = filtered_attrs(candidate.value(), ignore_attributes);
            let candidate_text = if match_text {
                Text::new(direct_text(*candidate)).clean().into_string()
            } else {
                String::new()
            };

            if are_alike(
                &original_attrs,
                &candidate_attrs,
                threshold,
                match_text,
                &original_text,
                &candidate_text,
            ) {
                similar.push(self.wrap_element(node.id()));
            }
        }

        Selectors::from(similar)
    }

    // -- generation and extraction ------------------------------------------------------

    /// A short CSS selector pointing at this element (stops at the nearest `id`).
    pub fn generate_css_selector(&self) -> String {
        generate::general_selection(self, false)
    }

    /// A CSS selector spelled out from the document root.
    pub fn generate_full_css_selector(&self) -> String {
        generate::general_selection(self, true)
    }

    /// Serialize this node: outer HTML for elements, the value for text/attr nodes.
    pub fn get(&self) -> Text {
        if self.is_text_node() {
            return self.text();
        }
        self.html_content()
    }

    /// `get()` wrapped in a one-element list, for Scrapy-shaped call sites.
    pub fn getall(&self) -> Texts {
        Texts::new([self.get()])
    }

    /// Regex over this node's text.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts> {
        self.text().re(pattern, opts)
    }

    /// First regex match over this node's text.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>> {
        self.text().re_first(pattern, opts)
    }

    /// Parse this node's text (or the document body at the root) as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        if self.is_text_node() {
            return self.text().json();
        }
        if self.id == self.doc.root && !self.doc.body.is_empty() {
            // Straight off the stored bytes: `serde_json` caps nesting depth itself, so a
            // hostile body is a `Error::Serde`, not a stack overflow, and no second copy of the
            // whole document is made on the way in.
            return serde_json::from_slice(&self.doc.body).map_err(Error::from);
        }
        let text = self.text();
        if !text.as_str().is_empty() {
            return text.json();
        }
        self.get_all_text("\n", true, &["script", "style"]).json()
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.get().as_str())
    }
}

/// Written by hand rather than derived: deriving would print the whole parsed document for
/// every node. This is Python's `Selector.__repr__` — a clipped preview plus the parent's.
impl fmt::Debug for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Python only runs `clean_spaces` over the HTML branch; a text node is previewed raw.
        if self.is_text_node() {
            return write!(f, "<text='{}'>", preview(self.text().as_str()));
        }
        write!(
            f,
            "<data='{}'",
            preview(self.html_content().clean().as_str())
        )?;
        if let Some(parent) = self.parent() {
            write!(
                f,
                " parent='{}'",
                preview(parent.html_content().clean().as_str())
            )?;
        }
        f.write_str(">")
    }
}

/// Clip a value to the 40 characters Python's `__repr__` shows: `value[:40].strip() + "..."`.
fn preview(value: &str) -> String {
    const LIMIT: usize = 40;
    if value.chars().count() > LIMIT {
        let head: String = value.chars().take(LIMIT).collect();
        format!("{}...", head.trim())
    } else {
        value.to_string()
    }
}

// -- free helpers -----------------------------------------------------------------------

/// The document's `<html>` element, falling back to the tree root for degenerate input.
fn root_node_id(html: &Html) -> NodeId {
    let root = html.tree.root();
    match root.children().find(|child| child.value().is_element()) {
        Some(child) => child.id(),
        None => root.id(),
    }
}

/// lxml's `element.text`: the run of text that opens the element, blank runs reading as empty.
fn direct_text(node: NodeRef<'_, Node>) -> String {
    let first = node
        .first_child()
        .and_then(|child| child.value().as_text().map(|text| String::from(&**text)));
    match first {
        Some(text) if !text.trim().is_empty() => text,
        _ => String::new(),
    }
}

/// The element's own text, optionally cleaned and lowercased, ready for comparison.
fn normalized_text(node: &Selector, case_sensitive: bool, clean_match: bool) -> String {
    let text = node.text();
    let text = if clean_match { text.clean() } else { text };
    if case_sensitive {
        text.into_string()
    } else {
        text.as_str().to_lowercase()
    }
}

/// Whether `node` sits under one of `ignore_tags`, looking no further up than `stop`.
fn is_under_ignored_tag(node: NodeRef<'_, Node>, stop: NodeId, ignore_tags: &[&str]) -> bool {
    if ignore_tags.is_empty() {
        return false;
    }
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if let Some(element) = ancestor.value().as_element() {
            if ignore_tags
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case(element.name()))
            {
                return true;
            }
        }
        if ancestor.id() == stop {
            return false;
        }
        current = ancestor.parent();
    }
    false
}

fn element_parent(element: ElementRef<'_>) -> Option<ElementRef<'_>> {
    element.parent().and_then(ElementRef::wrap)
}

fn element_ancestor_count(element: ElementRef<'_>) -> usize {
    element
        .ancestors()
        .filter(|ancestor| ancestor.value().is_element())
        .count()
}

fn filtered_attrs(element: &HtmlElement, ignore: &[&str]) -> Vec<(String, String)> {
    element
        .attrs()
        .filter(|(name, _)| !ignore.contains(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// Cut `value` to at most `limit` characters, never splitting one in half.
fn truncate_chars(value: &str, limit: usize) -> &str {
    match value.char_indices().nth(limit) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

/// Python's `difflib.SequenceMatcher(None, a, b).ratio()`.
///
/// Both sides are cut to [`MAX_DIFF_CHARS`] first: the diff is quadratic in the input length and
/// the inputs are attribute values and text runs taken straight from a remote page.
fn ratio(a: &str, b: &str) -> f64 {
    let a = truncate_chars(a, MAX_DIFF_CHARS);
    let b = truncate_chars(b, MAX_DIFF_CHARS);
    // `difflib` calls two empty sequences a perfect match; `similar` agrees, but say so plainly
    // rather than relying on it.
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    f64::from(similar::TextDiff::from_chars(a, b).ratio())
}

/// The port of `Selector.__are_alike`.
fn are_alike(
    original: &[(String, String)],
    candidate: &[(String, String)],
    threshold: f64,
    match_text: bool,
    original_text: &str,
    candidate_text: &str,
) -> bool {
    let mut score = 0.0f64;
    let mut checks = 0usize;

    if !original.is_empty() {
        for (name, value) in original {
            let found = candidate
                .iter()
                .find(|(other, _)| other == name)
                .map(|(_, other)| other.as_str())
                .unwrap_or("");
            score += ratio(value, found);
        }
        // `max` so candidates with extra attributes are penalised and candidates with fewer
        // don't get inflated scores from a smaller denominator.
        checks += original.len().max(candidate.len());
    } else if candidate.is_empty() {
        // Both sides carry no attributes; that must mean something.
        score += 1.0;
        checks += 1;
    }

    if match_text {
        score += ratio(original_text, candidate_text);
        checks += 1;
    }

    if checks == 0 {
        return false;
    }
    let average = score / checks as f64;
    (average * 100.0).round() / 100.0 >= threshold
}

/// Serialize an element's start tag, escaping attribute values.
fn open_tag(element: &HtmlElement) -> String {
    let mut out = String::new();
    out.push('<');
    out.push_str(element.name());
    for (name, value) in element.attrs() {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&escape_attribute(value));
        out.push('"');
    }
    out.push('>');
    out
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Re-indent a subtree, one element per line.
///
/// Elements with no element children are written inline so that `<h2>Products</h2>` stays on
/// one line, which is what lxml's `pretty_print` does too. Below [`MAX_PRETTY_DEPTH`] the rest
/// of the subtree is serialized in one piece instead of recursed into, so markup nested deeper
/// than the stack can follow is still printed rather than aborting the process.
fn pretty_print(node: NodeRef<'_, Node>, depth: usize, out: &mut String) {
    let Some(element) = ElementRef::wrap(node) else {
        return;
    };
    let indent = "  ".repeat(depth);

    if depth >= MAX_PRETTY_DEPTH || !node.children().any(|child| child.value().is_element()) {
        out.push_str(&indent);
        out.push_str(&element.html());
        out.push('\n');
        return;
    }

    out.push_str(&indent);
    out.push_str(&open_tag(element.value()));
    out.push('\n');

    for child in node.children() {
        match child.value() {
            Node::Element(_) => pretty_print(child, depth + 1, out),
            Node::Text(text) => {
                let raw: &str = text;
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    out.push_str(&"  ".repeat(depth + 1));
                    out.push_str(trimmed);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }

    out.push_str(&indent);
    out.push_str("</");
    out.push_str(element.value().name());
    out.push_str(">\n");
}

#[cfg(test)]
mod tests {
    use crate::parser::{Filter, Selector, SelectorKind, OVERVIEW_HTML};

    fn page() -> Selector {
        Selector::new(OVERVIEW_HTML).expect("the overview document parses")
    }

    /// The `unsafe impl Sync for Document` above is a manual proof about `scraper`'s and
    /// `ego-tree`'s internals, so both have to stay on the exact versions that proof was read
    /// against; a caret range would let `cargo update` invalidate it silently.
    #[test]
    fn the_manifest_pins_the_crates_the_unsafe_impl_reasons_about() {
        let manifest = include_str!("../../Cargo.toml");
        let declared = |name: &str| {
            manifest
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with(name))
                .unwrap_or_else(|| panic!("`{name}` is declared in Cargo.toml"))
                .to_string()
        };

        let scraper = declared("scraper =");
        assert!(
            scraper.contains("\"=0.27.0\""),
            "scraper must stay pinned to an exact version, got `{scraper}`"
        );
        let ego_tree = declared("ego-tree =");
        assert!(
            ego_tree.contains("\"=0.11.0\""),
            "ego-tree must stay pinned to an exact version, got `{ego_tree}`"
        );
    }

    fn texts(selectors: &crate::parser::Selectors) -> Vec<String> {
        selectors
            .iter()
            .map(|element| element.text().into_string())
            .collect()
    }

    fn tags(selectors: &crate::parser::Selectors) -> Vec<String> {
        selectors
            .iter()
            .map(|element| element.tag().to_string())
            .collect()
    }

    // -- the assertions documented in docs/overview.md --------------------------------

    #[test]
    fn the_root_is_the_html_element() {
        let page = page();
        assert_eq!(page.tag(), "html");
        assert!(!page.is_text_node());
        assert_eq!(page.kind(), &SelectorKind::Element);
        assert!(!page.body().is_empty());
    }

    #[test]
    fn get_all_text_reproduces_the_documented_output() {
        let page = page();
        let expected = "Complex Web Page\nHome\nAbout\nContact\nProducts\nProduct 1\n\
                        This is product 1\n$10.99\nIn stock: 5\nProduct 2\nThis is product 2\n\
                        $20.99\nIn stock: 3\nProduct 3\nThis is product 3\n$15.99\n\
                        Out of stock\nCustomer Reviews\nGreat product!\nJohn Doe\n\
                        Good value for money.\nJane Smith";
        assert_eq!(
            page.get_all_text("\n", false, &["script", "style"]),
            expected
        );
    }

    #[test]
    fn find_and_find_all_match_the_documented_results() {
        let page = page();

        let first = page.find(&Filter::new().tag("section")).unwrap().unwrap();
        assert_eq!(first.attr("id"), Some("products".into()));

        assert_eq!(
            page.find_all(&Filter::new().tag("section")).unwrap().len(),
            2
        );
        assert_eq!(
            page.find_all(&Filter::new().tag("section").attr("id", "products"))
                .unwrap()
                .len(),
            1
        );

        let h3s = page
            .find_all(&Filter::new().tag("h3").regex(r"Product \d").unwrap())
            .unwrap();
        assert_eq!(texts(&h3s), ["Product 1", "Product 2", "Product 3"]);

        // `find_all(['h3', 'h2'], re.compile(r'Product'))` — the h3 matches come first
        // because the selector list is evaluated member by member.
        let headings = page
            .find_all(&Filter::new().tags(["h3", "h2"]).regex("Product").unwrap())
            .unwrap();
        assert_eq!(
            texts(&headings),
            ["Product 1", "Product 2", "Product 3", "Products"]
        );
    }

    #[test]
    fn find_by_text_matches_the_documented_result() {
        let page = page();
        let found = page.find_by_text("Products", false, false, false, true);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tag(), "h2");
        assert_eq!(found[0].text(), "Products");
    }

    #[test]
    fn find_by_regex_matches_the_documented_result() {
        let page = page();
        // Case-sensitive, so that `<p>This is product 1</p>` stays out of the way and the
        // result is exactly the list printed in the docs.
        let found = page
            .find_by_regex(r"Product \d", false, true, true)
            .unwrap();

        assert_eq!(texts(&found), ["Product 1", "Product 2", "Product 3"]);
        assert_eq!(tags(&found), ["h3", "h3", "h3"]);

        let first = page.find_by_regex(r"Product \d", true, true, true).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text(), "Product 1");
    }

    #[test]
    fn find_similar_matches_the_documented_result() {
        let page = page();
        let target = page.find_by_regex(r"Product \d", true, true, true).unwrap();
        let target = target.first().expect("a first heading");

        let similar = target.find_similar(0.2, &["href", "src"], false);
        assert_eq!(texts(&similar), ["Product 2", "Product 3"]);
    }

    #[test]
    fn css_matches_the_documented_results() {
        let page = page();

        let article = page.css(r#".product-list [data-id="1"]"#).unwrap();
        assert_eq!(article.len(), 1);
        assert_eq!(article[0].tag(), "article");

        assert_eq!(page.css(".product-list article").unwrap().len(), 3);
        assert!(page.css(r#"[data-id="1"]"#).unwrap()[0].has_class("product"));
    }

    #[test]
    fn accessing_the_elements_data_matches_the_docs() {
        let page = page();
        let section = page.css_first("#products").unwrap().unwrap();

        assert_eq!(section.tag(), "section");

        let attrib = section.attrib();
        assert_eq!(attrib.len(), 2);
        assert_eq!(attrib.get("id"), Some(&"products".into()));
        assert_eq!(
            attrib.get("schema"),
            Some(&r#"{"jsonable": "data"}"#.into())
        );

        // The section opens with whitespace only, so its *direct* text is empty.
        assert_eq!(section.text(), "");

        let expected = "Products\nProduct 1\nThis is product 1\n$10.99\nIn stock: 5\n\
                        Product 2\nThis is product 2\n$20.99\nIn stock: 3\nProduct 3\n\
                        This is product 3\n$15.99\nOut of stock";
        assert_eq!(
            section.get_all_text("\n", false, &["script", "style"]),
            expected
        );

        assert!(section
            .html_content()
            .as_str()
            .starts_with("<section id=\"products\""));
        assert!(section.prettify().as_str().contains("<h2>Products</h2>"));

        assert_eq!(tags(&section.path()), ["main", "body", "html"]);
        assert_eq!(section.generate_css_selector(), "#products");
        assert_eq!(
            section.generate_full_css_selector(),
            "body > main > #products"
        );

        // Only the document root carries the raw body.
        assert!(section.body().is_empty());
    }

    #[test]
    fn navigation_matches_the_docs() {
        let page = page();
        let section = page.css_first("#products").unwrap().unwrap();

        assert_eq!(
            section.parent().map(|p| p.tag().to_string()),
            Some("main".into())
        );
        assert_eq!(
            section
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.tag().to_string()),
            Some("body".into())
        );

        assert_eq!(tags(&section.children()), ["h2", "div"]);

        let siblings = section.siblings();
        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0].attr("id"), Some("reviews".into()));

        assert_eq!(
            section.next().and_then(|n| n.attr("id")),
            Some("reviews".into())
        );
        assert!(section.previous().is_none());

        assert_eq!(
            section
                .children()
                .css("h2::text")
                .unwrap()
                .getall()
                .getall(),
            ["Products"]
        );

        let ancestor = section
            .find_ancestor(|a| a.css("nav").map(|found| !found.is_empty()).unwrap_or(false))
            .expect("body holds the nav");
        assert_eq!(ancestor.tag(), "body");

        assert_eq!(page.parent().map(|p| p.tag().to_string()), None);
        assert_eq!(section.iterancestors().count(), 3);
    }

    // -- pseudo-elements ---------------------------------------------------------------

    #[test]
    fn text_pseudo_element_yields_direct_child_text_nodes() {
        let page = page();
        let headings = page.css("h2::text").unwrap();

        assert_eq!(headings.getall().getall(), ["Products", "Customer Reviews"]);

        let first = &headings[0];
        assert!(first.is_text_node());
        assert_eq!(first.tag(), "#text");
        assert_eq!(first.text(), "Products");
        assert_eq!(first.kind(), &SelectorKind::TextNode("Products".into()));
        assert!(first.attrib().is_empty());
        assert!(first.children().is_empty());
        assert!(first.css("span").unwrap().is_empty());
        assert!(first.find_all(&Filter::new()).unwrap().is_empty());
        assert!(!first.has_class("anything"));
        assert_eq!(first.generate_css_selector(), "");
        assert!(first.body().is_empty());
        assert_eq!(first.to_string(), "Products");
    }

    #[test]
    fn text_pseudo_element_drops_whitespace_only_runs() {
        let page = page();
        // `<li> <a href="#home">Home</a> </li>` — Python parses with `remove_blank_text=True`,
        // so the blanks around the link are not text nodes there and `li::text` finds nothing.
        assert!(page.css("li::text").unwrap().is_empty());
        assert_eq!(
            page.css("a::text").unwrap().getall().getall(),
            ["Home", "About", "Contact"]
        );
    }

    #[test]
    fn text_pseudo_element_skips_descendants() {
        let page = Selector::new("<div>outer<span>inner</span>tail</div>").unwrap();
        // The direct text children are "outer" and "tail"; "inner" belongs to the span.
        assert_eq!(
            page.css("div::text").unwrap().getall().getall(),
            ["outer", "tail"]
        );
    }

    #[test]
    fn attr_pseudo_element_yields_attribute_values() {
        let page = page();
        let hrefs = page.css("a::attr(href)").unwrap();

        assert_eq!(hrefs.getall().getall(), ["#home", "#about", "#contact"]);

        let first = &hrefs[0];
        assert!(first.is_text_node());
        assert_eq!(first.tag(), "#text");
        assert_eq!(first.text(), "#home");
        assert_eq!(
            first.kind(),
            &SelectorKind::AttrNode {
                name: "href".into(),
                value: "#home".into()
            }
        );
        // An attribute result reports the element that carries it.
        assert_eq!(
            first.parent().map(|p| p.tag().to_string()),
            Some("a".into())
        );
    }

    #[test]
    fn a_bare_attr_pseudo_element_reads_the_element_itself() {
        let page = Selector::new(r#"<a href="/p/1">x</a>"#).unwrap();
        let link = page.css_first("a").unwrap().unwrap();
        assert_eq!(
            link.css("::attr(href)").unwrap().getall().getall(),
            ["/p/1"]
        );
    }

    #[test]
    fn comma_separated_lists_keep_their_order() {
        let page = page();
        let both = page.css("h3, h2").unwrap();
        assert_eq!(
            texts(&both),
            [
                "Product 1",
                "Product 2",
                "Product 3",
                "Products",
                "Customer Reviews"
            ]
        );
    }

    #[test]
    fn a_broken_selector_is_a_crate_error_not_a_panic() {
        let page = page();
        assert!(matches!(
            page.css("div[").unwrap_err(),
            crate::Error::Selector { .. }
        ));
        assert!(page.css_first(">>>").is_err());
    }

    // -- extraction ---------------------------------------------------------------------

    #[test]
    fn get_and_getall_serialize_the_node() {
        let page = Selector::new("<h1>Hello</h1>").unwrap();
        let heading = page.css_first("h1").unwrap().unwrap();

        assert_eq!(heading.get(), "<h1>Hello</h1>");
        assert_eq!(heading.getall().len(), 1);
        assert_eq!(heading.to_string(), "<h1>Hello</h1>");
    }

    #[test]
    fn regex_helpers_run_over_the_node_text() {
        let page = page();
        let price = page.css_first(".price").unwrap().unwrap();

        assert_eq!(
            price.re(r"[\d.]+", Default::default()).unwrap().getall(),
            ["10.99"]
        );
        assert_eq!(
            price.re_first(r"[\d.]+", Default::default()).unwrap(),
            Some("10.99".into())
        );
    }

    #[test]
    fn json_reads_a_script_payload() {
        let page = page();
        let script = page.css_first("#page-data").unwrap().unwrap();
        let value: serde_json::Value = script.json().unwrap();

        assert_eq!(value["totalProducts"], serde_json::json!(3));
    }

    #[test]
    fn an_attribute_value_can_be_json() {
        let page = page();
        let section = page.css_first("#products").unwrap().unwrap();
        let schema = section.attr("schema").expect("a schema attribute");
        let value: serde_json::Value = schema.json().unwrap();

        assert_eq!(value["jsonable"], serde_json::json!("data"));
    }

    // -- urls and degenerate input --------------------------------------------------------

    #[test]
    fn urljoin_uses_the_documents_url() {
        let page = Selector::with_url(
            r#"<a href="catalogue/x.html">x</a>"#,
            "https://books.toscrape.com/index.html",
        )
        .unwrap();

        assert_eq!(page.url(), "https://books.toscrape.com/index.html");
        let href = page.css("a::attr(href)").unwrap().get().unwrap();
        assert_eq!(
            page.urljoin(href.as_str()).unwrap(),
            "https://books.toscrape.com/catalogue/x.html"
        );
    }

    #[test]
    fn urljoin_without_a_url_returns_the_relative_url() {
        let page = Selector::new("<html></html>").unwrap();
        assert_eq!(page.url(), "");
        assert_eq!(page.urljoin("/a/b").unwrap(), "/a/b");
    }

    #[test]
    fn urljoin_reports_a_broken_base_instead_of_panicking() {
        let page = Selector::with_url("<html></html>", "not a url").unwrap();
        assert!(page.urljoin("/a").is_err());
    }

    #[test]
    fn untrusted_input_never_panics() {
        for input in [
            "",
            "   ",
            "\0\0\0",
            "<<<>>>",
            "<div class=",
            "<p><span>unclosed",
            "<!-- only a comment -->",
            "&#x1F600;<b>",
        ] {
            let page = Selector::new(input).expect("parsing never fails");
            let _ = page.tag();
            let _ = page.text();
            let _ = page.get_all_text("\n", true, &["script", "style"]);
            let _ = page.attrib();
            let _ = page.children();
            let _ = page.siblings();
            let _ = page.below_elements();
            let _ = page.prettify();
            let _ = page.generate_full_css_selector();
            let _ = page.css("*");
            let _ = page.find_all(&Filter::new());
            let _ = page.find_by_text("x", false, true, false, true);
            let _ = page.find_by_regex("x", false, false, true);
            let _ = page.find_similar(0.2, &["href", "src"], true);
            let _: crate::Result<serde_json::Value> = page.json();
        }
    }

    #[test]
    fn deeply_nested_markup_is_handled_without_recursing_that_far() {
        // Nesting like this costs a hostile server a few kilobytes and would otherwise put one
        // stack frame per level under `prettify` and one selector part per level under
        // `generate_full_css_selector`.
        const DEPTH: usize = 2_000;
        let html = format!("{}deep{}", "<div>".repeat(DEPTH), "</div>".repeat(DEPTH));
        let page = Selector::new(&html).expect("the document parses");

        assert!(page.prettify().as_str().contains("deep"));

        let deepest = page.find_by_text("deep", true, false, true, true);
        assert_eq!(deepest.len(), 1);
        assert_eq!(deepest[0].tag(), "div");

        // The generated selector is truncated rather than being one part per level.
        let selector = deepest[0].generate_full_css_selector();
        let parts = selector.split(" > ").count();
        assert!(
            parts > 1 && parts < DEPTH,
            "unbounded selector: {parts} parts"
        );
    }

    #[test]
    fn a_fragment_parses_without_a_document_wrapper() {
        let fragment = Selector::fragment("<li>a</li><li>b</li>").unwrap();
        assert_eq!(fragment.css("li").unwrap().len(), 2);
    }

    #[test]
    fn a_selector_is_cheap_to_clone_and_shares_its_document() {
        let page = page();
        let clone = page.clone();
        assert!(page.is_same_node(&clone));
        assert_eq!(clone.tag(), "html");
        assert!(!page.is_same_node(&page.css_first("body").unwrap().unwrap()));
    }

    #[test]
    fn debug_shows_a_python_shaped_preview() {
        let page = page();
        let heading = page.css_first("h3").unwrap().unwrap();

        let rendered = format!("{heading:?}");
        assert!(rendered.starts_with("<data='<h3>Product 1</h3>'"));
        assert!(rendered.contains(" parent='<article class=\"product\""));

        let text = page.css("h3::text").unwrap();
        assert_eq!(format!("{:?}", text[0]), "<text='Product 1'>");
    }

    #[test]
    fn find_similar_scores_attributes_when_they_differ() {
        let html = r#"<ul>
            <li class="item" data-x="1">a</li>
            <li class="item" data-x="2">b</li>
            <li class="other">c</li>
        </ul>"#;
        let page = Selector::new(html).unwrap();
        let first = page.css_first("li").unwrap().unwrap();

        // `b` shares both attribute names and the `class` value, scoring (1.0 + 0.0) / 2;
        // `c` shares neither the second attribute nor much of the first.
        assert_eq!(texts(&first.find_similar(0.4, &[], false)), ["b"]);

        // Nothing reaches 90% once `data-x` differs.
        assert!(texts(&first.find_similar(0.9, &[], false)).is_empty());
    }

    #[test]
    fn find_similar_can_take_the_text_into_account() {
        let html = r#"<ul><li>Product 1</li><li>Product 2</li><li>Nothing alike</li></ul>"#;
        let page = Selector::new(html).unwrap();
        let first = page.css_first("li").unwrap().unwrap();

        // Attributes are equal (there are none), so the text similarity decides.
        let similar = first.find_similar(0.8, &["href", "src"], true);
        assert_eq!(texts(&similar), ["Product 2"]);

        // Without `match_text` the two other items are structurally identical.
        let similar = first.find_similar(0.2, &["href", "src"], false);
        assert_eq!(texts(&similar), ["Product 2", "Nothing alike"]);
    }
}
