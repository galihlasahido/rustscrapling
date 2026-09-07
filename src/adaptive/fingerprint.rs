//! The stored shape of an element — a port of `_StorageTools.element_to_dict`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::parser::{Selector, Selectors};

/// The longest run of characters kept for a single text or attribute value.
///
/// A fingerprint is built from remote HTML and is then fed to a quadratic diff once per
/// candidate element, so both the stored record and the scoring cost have to stay bounded.
/// The cut is applied to the original and to every candidate alike, so scores stay
/// comparable; Python keeps whole values and pays for it.
pub(super) const MAX_TEXT_CHARS: usize = 512;

/// The largest number of attributes kept for one element.
pub(super) const MAX_ATTRIBUTES: usize = 64;

/// The largest number of entries kept in `path`, `siblings` and `children`.
///
/// A page can legitimately put thousands of children under one node; keeping every tag name
/// would make both the stored row and the sequence diff grow without a limit, and the first
/// few hundred already tell two candidates apart.
pub(super) const MAX_LIST_ITEMS: usize = 256;

/// The stored shape of an element, used to find it again after the page changes.
/// Mirrors `_StorageTools.element_to_dict` field for field.
///
/// Every field carries a serde default so a record written by the Python library — which
/// omits the parent, sibling and children keys entirely when they are empty — deserializes
/// into the same value it would have had here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ElementFingerprint {
    /// The element's tag name.
    pub tag: String,
    /// The element's attributes, blank values dropped and values trimmed.
    pub attributes: BTreeMap<String, String>,
    /// The element's own text, trimmed; `None` when it had none.
    pub text: Option<String>,
    /// Tag names from the document root down to and including this element.
    pub path: Vec<String>,
    /// The parent's tag name, when there is a parent.
    pub parent_name: Option<String>,
    /// The parent's attributes.
    pub parent_attribs: BTreeMap<String, String>,
    /// The parent's own text, trimmed.
    pub parent_text: Option<String>,
    /// The tag names of the element's siblings.
    pub siblings: Vec<String>,
    /// The tag names of the element's children.
    pub children: Vec<String>,
}

impl ElementFingerprint {
    /// The value of one of the element's own attributes, when it has one.
    ///
    /// A convenience over [`ElementFingerprint::attributes`]; the similarity scorer uses it
    /// for the per-attribute checks on `class`, `id`, `href` and `src`.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }

    /// Whether the element had a parent when it was fingerprinted.
    pub fn has_parent(&self) -> bool {
        self.parent_name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
    }
}

/// Capture an element's fingerprint.
///
/// Text and attribute nodes are fingerprinted as they come; callers that want Python's
/// "save the parent of a text node" behaviour do that before calling this.
pub fn fingerprint(element: &Selector) -> ElementFingerprint {
    let own_text = element.text();

    let mut result = ElementFingerprint {
        tag: element.tag().to_string(),
        attributes: clean_attributes(element),
        text: non_empty_trimmed(own_text.as_str()),
        path: element_path(element),
        ..ElementFingerprint::default()
    };

    if let Some(parent) = element.parent() {
        let parent_text = parent.text();
        result.parent_name = Some(parent.tag().to_string());
        result.parent_attribs = raw_attributes(&parent);
        result.parent_text = non_empty_trimmed(parent_text.as_str());
        result.siblings = tag_names(element.siblings());
    }

    result.children = tag_names(element.children());
    result
}

/// The tag names of the document root down to and including `element`.
///
/// Python builds this by recursing on `getparent()` up to the root and reversing; the
/// ancestor iterator here is closest-first and iterative, so the collected list is reversed
/// for the same order and a pathologically deep document cannot blow the stack. Only the
/// closest [`MAX_LIST_ITEMS`] ancestors are kept — those are the ones that tell two
/// candidates apart.
fn element_path(element: &Selector) -> Vec<String> {
    let mut path: Vec<String> = element
        .iterancestors()
        .take(MAX_LIST_ITEMS)
        .map(|ancestor| ancestor.tag().to_string())
        .collect();
    path.reverse();
    path.push(element.tag().to_string());
    path
}

/// The element's attributes with blank values dropped and the rest trimmed
/// (`_StorageTools.__clean_attributes`).
fn clean_attributes(element: &Selector) -> BTreeMap<String, String> {
    collect_attributes(element, true)
}

/// The element's attributes exactly as they are, matching Python's `dict(parent.attrib)`.
fn raw_attributes(element: &Selector) -> BTreeMap<String, String> {
    collect_attributes(element, false)
}

/// The shared body of [`clean_attributes`] and [`raw_attributes`].
///
/// `clean` drops blank values and trims the rest; either way at most [`MAX_ATTRIBUTES`]
/// attributes are kept and every value is cut to [`MAX_TEXT_CHARS`], so a page that hides a
/// megabyte in a `data-` attribute cannot bloat the stored row.
fn collect_attributes(element: &Selector, clean: bool) -> BTreeMap<String, String> {
    let attributes = element.attrib();
    let mut result = BTreeMap::new();
    for (name, value) in attributes.iter() {
        if result.len() >= MAX_ATTRIBUTES {
            break;
        }
        let value = if clean {
            value.as_str().trim()
        } else {
            value.as_str()
        };
        if clean && value.is_empty() {
            continue;
        }
        result.insert(
            name.to_string(),
            truncate_chars(value, MAX_TEXT_CHARS).to_string(),
        );
    }
    result
}

/// The tag names of a list of elements, in document order, capped at [`MAX_LIST_ITEMS`].
fn tag_names(elements: Selectors) -> Vec<String> {
    elements
        .iter()
        .take(MAX_LIST_ITEMS)
        .map(|element| element.tag().to_string())
        .collect()
}

/// `Some(trimmed)` when the trimmed value is not empty, mirroring Python's
/// `element.text.strip() if element.text else None`.
fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_chars(trimmed, MAX_TEXT_CHARS).to_string())
    }
}

/// The first `limit` characters of `value`, cut on a character boundary.
pub(super) fn truncate_chars(value: &str, limit: usize) -> &str {
    match value.char_indices().nth(limit) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
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

    fn first(html: &str, selector: &str) -> Selector {
        let page = Selector::new(html).expect("the test document parses");
        page.css_first(selector)
            .expect("the test selector parses")
            .expect("the test selector matches")
    }

    #[test]
    fn fingerprints_tag_attributes_and_relations() {
        let element = first(PAGE, "#p1");
        let print = fingerprint(&element);

        assert_eq!(print.tag, "article");
        assert_eq!(print.attribute("class"), Some("product"));
        assert_eq!(print.attribute("id"), Some("p1"));
        assert_eq!(print.parent_name.as_deref(), Some("section"));
        assert!(print.has_parent());
        assert_eq!(
            print.parent_attribs.get("class").map(String::as_str),
            Some("products")
        );
        assert_eq!(print.siblings, vec!["article".to_string()]);
        assert_eq!(print.children, vec!["h3".to_string(), "p".to_string()]);
        assert_eq!(print.path.last().map(String::as_str), Some("article"));
        assert!(print.path.len() >= 2);
    }

    #[test]
    fn blank_attribute_values_are_dropped_and_values_trimmed() {
        let element = first(
            r#"<div><span class="  spaced  " data-empty="" id="x">hi</span></div>"#,
            "span",
        );
        let print = fingerprint(&element);

        assert_eq!(print.attribute("class"), Some("spaced"));
        assert_eq!(print.attribute("data-empty"), None);
        assert_eq!(print.attribute("id"), Some("x"));
        assert_eq!(print.text.as_deref(), Some("hi"));
    }

    #[test]
    fn parent_attributes_are_kept_exactly_as_they_are() {
        // Python stores `dict(parent.attrib)`, so blanks survive on the parent.
        let element = first(
            r#"<div id="wrapper" data-empty=""><span> child </span></div>"#,
            "span",
        );
        let print = fingerprint(&element);

        assert_eq!(
            print.parent_attribs.get("data-empty").map(String::as_str),
            Some("")
        );
        assert_eq!(
            print.parent_attribs.get("id").map(String::as_str),
            Some("wrapper")
        );
    }

    #[test]
    fn whitespace_only_text_becomes_none() {
        let element = first("<div><p class=\"blank\">  \t\n  </p></div>", "p");
        let print = fingerprint(&element);
        assert_eq!(print.text, None);
        assert_eq!(print.attribute("class"), Some("blank"));
    }

    #[test]
    fn long_values_and_long_lists_are_bounded() {
        let text = "x".repeat(MAX_TEXT_CHARS * 3);
        let children: String = (0..MAX_LIST_ITEMS + 50).map(|_| "<li>a</li>").collect();
        let html = format!(r#"<div><p data-blob="{text}">{text}</p><ul>{children}</ul></div>"#);

        let paragraph = first(&html, "p");
        let print = fingerprint(&paragraph);
        assert_eq!(print.text.as_deref().map(str::len), Some(MAX_TEXT_CHARS));
        assert_eq!(
            print.attribute("data-blob").map(str::len),
            Some(MAX_TEXT_CHARS)
        );

        let list = first(&html, "ul");
        assert_eq!(fingerprint(&list).children.len(), MAX_LIST_ITEMS);
    }

    #[test]
    fn truncation_lands_on_character_boundaries() {
        let value = "é".repeat(MAX_TEXT_CHARS + 10);
        let cut = truncate_chars(&value, MAX_TEXT_CHARS);
        assert_eq!(cut.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abc", 0), "");
    }

    #[test]
    fn round_trips_through_json() {
        let element = first(PAGE, "#p1");
        let print = fingerprint(&element);
        let encoded = serde_json::to_string(&print).expect("the fingerprint serializes");
        let decoded: ElementFingerprint =
            serde_json::from_str(&encoded).expect("the fingerprint deserializes");
        assert_eq!(print, decoded);
    }

    #[test]
    fn accepts_a_python_written_record_with_missing_keys() {
        let decoded: ElementFingerprint = serde_json::from_str(
            r#"{"tag":"a","attributes":{"href":"/x"},"text":"Next","path":["html","body","a"]}"#,
        )
        .expect("a partial record deserializes");

        assert_eq!(decoded.tag, "a");
        assert_eq!(decoded.parent_name, None);
        assert!(decoded.siblings.is_empty());
        assert!(decoded.children.is_empty());
        assert!(!decoded.has_parent());
    }
}
