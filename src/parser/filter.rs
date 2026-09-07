//! [`Filter`], the builder that replaces Python's `find_all(*args, **kwargs)`.
//!
//! Python collects tag names, attribute dictionaries, regex patterns and callables out of
//! `*args`/`**kwargs` by `isinstance` branching. Rust cannot do that, so every kind of
//! condition gets its own builder method here. The execution strategy is the Python one:
//! whatever can be said in CSS (tags and attributes) is compiled into a single selector group
//! — "it's easier and faster to build a selector than traversing the tree" — and the regexes
//! and closures are applied afterwards as a post-filter.

use regex::Regex;

use crate::error::{Error, Result};
use crate::parser::css;
use crate::parser::selector::Selector;
use crate::parser::selectors::Selectors;

/// How an attribute value is compared. The operator is written as a suffix on the attribute
/// name, exactly like Python Scrapling lets you pass `{'id*': 'product'}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrOp {
    /// `name` — the value must be equal.
    Equals,
    /// `name*` — the value must contain the given substring.
    Contains,
    /// `name^` — the value must start with the given prefix.
    StartsWith,
    /// `name$` — the value must end with the given suffix.
    EndsWith,
    /// `name~` — the whitespace-separated list must contain the given word.
    Includes,
    /// `name|` — the value must equal it or start with it followed by `-`.
    DashMatch,
}

fn split_operator(key: &str) -> (&str, AttrOp) {
    let mut chars = key.chars();
    match chars.next_back() {
        Some('*') => (chars.as_str(), AttrOp::Contains),
        Some('^') => (chars.as_str(), AttrOp::StartsWith),
        Some('$') => (chars.as_str(), AttrOp::EndsWith),
        Some('~') => (chars.as_str(), AttrOp::Includes),
        Some('|') => (chars.as_str(), AttrOp::DashMatch),
        _ => (key, AttrOp::Equals),
    }
}

/// The builder that replaces Python's `find_all(*args, **kwargs)`.
///
/// ```no_run
/// # fn main() -> rustscrapling::Result<()> {
/// use rustscrapling::{Filter, Selector};
///
/// let page = Selector::new("<h3 class=\"product\">Product 1</h3>")?;
/// let filter = Filter::new()
///     .tags(["h3", "h2"])
///     .attr("class", "product")
///     .regex(r"Product \d")?;
/// assert_eq!(page.find_all(&filter)?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct Filter {
    tags: Vec<String>,
    attrs: Vec<(String, String)>,
    has_attrs: Vec<String>,
    patterns: Vec<Regex>,
    #[allow(clippy::type_complexity)]
    predicates: Vec<Box<dyn Fn(&Selector) -> bool + Send + Sync>>,
}

impl Filter {
    /// An empty filter that matches every element.
    pub fn new() -> Self {
        Filter::default()
    }

    /// Restrict to a single tag name.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Restrict to any of these tag names.
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    /// Require an attribute to equal `value` (`class` is matched name-by-name, like Python).
    ///
    /// The name may carry a CSS comparison suffix: `attr("id*", "product")` keeps elements
    /// whose `id` merely *contains* `product`.
    pub fn attr(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.push((name.into(), value.into()));
        self
    }

    /// Require several attributes at once.
    pub fn attrs<I, K, V>(mut self, attrs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.attrs
            .extend(attrs.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Require an attribute to merely be present.
    pub fn has_attr(mut self, name: impl Into<String>) -> Self {
        self.has_attrs.push(name.into());
        self
    }

    /// Require the element's text to match this regex.
    pub fn regex(self, pattern: &str) -> Result<Self> {
        let compiled = Regex::new(pattern)
            .map_err(|error| Error::other(format!("invalid regex `{pattern}`: {error}")))?;
        Ok(self.regex_compiled(compiled))
    }

    /// Require the element's text to match this compiled regex.
    pub fn regex_compiled(mut self, pattern: Regex) -> Self {
        self.patterns.push(pattern);
        self
    }

    /// Require a user predicate to return true.
    pub fn predicate(mut self, f: impl Fn(&Selector) -> bool + Send + Sync + 'static) -> Self {
        self.predicates.push(Box::new(f));
        self
    }

    /// Whether a given element satisfies every condition in this filter.
    pub fn matches(&self, element: &Selector) -> bool {
        // `*` is the wildcard `to_css` uses for "any tag", so it has to mean the same here or
        // `matches` would disagree with `find_all` on the very same filter.
        if !self.tags.is_empty()
            && !self
                .tags
                .iter()
                .any(|tag| tag == "*" || tag == element.tag())
        {
            return false;
        }
        for (name, value) in &self.attrs {
            if !attribute_matches(element, name, value) {
                return false;
            }
        }
        for name in &self.has_attrs {
            if element.attr(name).is_none() {
                return false;
            }
        }
        if !self.patterns.is_empty() {
            let text = element.text();
            if !self
                .patterns
                .iter()
                .all(|pattern| pattern.is_match(text.as_str()))
            {
                return false;
            }
        }
        self.predicates.iter().all(|predicate| predicate(element))
    }

    /// The CSS part of this filter, or `None` when there is nothing CSS can express.
    ///
    /// Mirrors the selector building inside Python's `find_all`, including the `[class~="x"]`
    /// trick that makes `class` match a single name out of a space-separated list.
    fn to_css(&self) -> Option<String> {
        let tags: Vec<&str> = if self.tags.is_empty() {
            vec!["*"]
        } else {
            self.tags.iter().map(String::as_str).collect()
        };

        let mut selectors: Vec<String> = Vec::new();
        for tag in tags {
            let mut selector = tag.to_string();
            for (key, value) in &self.attrs {
                let class_names: Vec<&str> = if key == "class" {
                    value.split_whitespace().collect()
                } else {
                    Vec::new()
                };
                if !class_names.is_empty() {
                    for name in class_names {
                        selector
                            .push_str(&format!("[class~=\"{}\"]", css::escape_css_string(name)));
                    }
                } else {
                    selector.push_str(&format!("[{}=\"{}\"]", key, css::escape_css_string(value)));
                }
            }
            for name in &self.has_attrs {
                selector.push_str(&format!("[{name}]"));
            }
            if selector != "*" {
                selectors.push(selector);
            }
        }

        if selectors.is_empty() {
            None
        } else {
            Some(selectors.join(", "))
        }
    }

    /// Run this filter against everything below `root`.
    pub(super) fn apply(&self, root: &Selector) -> Result<Selectors> {
        let mut results = match self.to_css() {
            Some(selector) => root.css(&selector)?,
            None => root.below_elements(),
        };

        if !self.patterns.is_empty() || !self.predicates.is_empty() {
            results = results.filter(|element| {
                let text = element.text();
                self.patterns
                    .iter()
                    .all(|pattern| pattern.is_match(text.as_str()))
                    && self.predicates.iter().all(|predicate| predicate(element))
            });
        }

        Ok(results)
    }
}

/// A `Filter` is handed to worker tasks by the crawl framework, so it has to stay thread-safe;
/// this fails to compile the day a field stops being `Send + Sync`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Filter>();
};

fn attribute_matches(element: &Selector, key: &str, value: &str) -> bool {
    let (name, operator) = split_operator(key);
    let Some(actual) = element.attr(name) else {
        return false;
    };
    let actual = actual.as_str();

    match operator {
        AttrOp::Equals => {
            if name == "class" {
                let wanted: Vec<&str> = value.split_whitespace().collect();
                if !wanted.is_empty() {
                    return wanted
                        .iter()
                        .all(|name| actual.split_whitespace().any(|have| have == *name));
                }
            }
            actual == value
        }
        AttrOp::Contains => actual.contains(value),
        AttrOp::StartsWith => actual.starts_with(value),
        AttrOp::EndsWith => actual.ends_with(value),
        AttrOp::Includes => actual.split_whitespace().any(|have| have == value),
        AttrOp::DashMatch => actual == value || actual.starts_with(&format!("{value}-")),
    }
}

impl std::fmt::Debug for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filter")
            .field("tags", &self.tags)
            .field("attrs", &self.attrs)
            .field("has_attrs", &self.has_attrs)
            .field(
                "patterns",
                &self
                    .patterns
                    .iter()
                    .map(Regex::as_str)
                    .collect::<Vec<&str>>(),
            )
            .field("predicates", &self.predicates.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{Filter, Selector, OVERVIEW_HTML};

    #[test]
    fn a_class_is_matched_name_by_name() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();

        // `class="hidden stock"` must be found by either of its names, which an exact
        // `[class="stock"]` match would miss.
        let found = page
            .find_all(&Filter::new().attr("class", "stock"))
            .unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(
            page.find_all(&Filter::new().attr("class", "hidden stock"))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn attribute_operators_are_read_off_the_name() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();

        assert_eq!(
            page.find_all(&Filter::new().tag("section").attr("id", "products"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            page.find_all(&Filter::new().tag("section").attr("id*", "product"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            page.find_all(&Filter::new().tag("section").attr("id^", "re"))
                .unwrap()
                .len(),
            1
        );
    }

    /// The value of an attribute filter is interpolated into a quoted CSS string, and filter
    /// values routinely come from page content, so it must not be able to end that string.
    #[test]
    fn an_attribute_value_cannot_break_out_of_the_generated_selector() {
        let page = Selector::new(
            r#"<html><body>
            <p id="target" data-x="a\">one</p>
            <p id="decoy" onclick="alert(1)">two</p>
            </body></html>"#,
        )
        .unwrap();

        // A value ending in a backslash used to swallow the closing quote of the generated
        // `[data-x="..."]`, which made the whole selector fail to compile.
        let found = page.find_all(&Filter::new().attr("data-x", "a\\")).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().and_then(|node| node.attr("id")).as_deref(),
            Some("target")
        );

        // And a value that closes the string and appends a selector of its own now matches
        // nothing, instead of the element that appended selector names.
        let injected = page
            .find_all(&Filter::new().attr("data-x", "a\\\" i], [onclick"))
            .unwrap();
        assert!(injected.is_empty(), "the injected selector must not match");
    }

    #[test]
    fn has_attr_only_requires_presence() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();
        let found = page
            .find_all(&Filter::new().tag("article").has_attr("data-id"))
            .unwrap();
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn regexes_and_closures_post_filter_the_css_results() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();

        let filter = Filter::new().tag("h3").regex(r"Product \d").unwrap();
        assert_eq!(page.find_all(&filter).unwrap().len(), 3);

        let filter = Filter::new()
            .tag("span")
            .predicate(|element| element.has_class("price"));
        assert_eq!(page.find_all(&filter).unwrap().len(), 3);
    }

    #[test]
    fn an_empty_filter_matches_every_descendant() {
        let page = Selector::new("<div><p>a</p></div>").unwrap();
        let found = page.find_all(&Filter::new()).unwrap();
        // head, body, div, p — every element below <html>.
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn matches_checks_one_element_without_querying() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();
        let article = page.css_first("article").unwrap().unwrap();

        assert!(Filter::new().tag("article").matches(&article));
        assert!(Filter::new().attr("class", "product").matches(&article));
        assert!(Filter::new().has_attr("data-id").matches(&article));
        assert!(!Filter::new().tag("section").matches(&article));
        assert!(!Filter::new().attr("data-id", "9").matches(&article));
    }

    #[test]
    fn a_broken_regex_is_a_crate_error() {
        assert!(Filter::new().regex("(").is_err());
    }

    #[test]
    fn debug_does_not_need_the_closures() {
        let filter = Filter::new().tag("a").predicate(|_| true);
        assert!(format!("{filter:?}").contains("predicates"));
    }
}
