//! [`Selectors`], the list type every query returns.

use std::ops::{Deref, Index};

use regex::{Regex, RegexBuilder};

use crate::error::{Error, Result};
use crate::parser::css;
use crate::parser::selector::Selector;
use crate::text::{ReOptions, Text, Texts};

/// A list of [`Selector`]s with the chaining helpers Scrapling puts on `Selectors`.
///
/// Cloning is cheap: every element shares the same parsed document through an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct Selectors(Vec<Selector>);

impl Selectors {
    /// Build from anything that iterates into [`Selector`].
    pub fn new(items: impl IntoIterator<Item = Selector>) -> Self {
        Selectors(items.into_iter().collect())
    }

    /// Run `css` on every element and flatten the results.
    ///
    /// The selector is compiled once for the whole list, not once per element, and a broken one
    /// is reported even when the list is empty.
    pub fn css(&self, selector: &str) -> Result<Selectors> {
        let queries = css::compile(selector)?;
        let mut out: Vec<Selector> = Vec::new();
        for element in &self.0 {
            out.extend(element.css_compiled(&queries).into_vec());
        }
        Ok(Selectors(out))
    }

    /// Run `re` on every element and flatten the results.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        let mut out: Vec<Text> = Vec::new();
        for element in &self.0 {
            out.extend(element.text().re_compiled(&compiled, opts).into_vec());
        }
        Ok(Texts::new(out))
    }

    /// The first regex match across all elements.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        for element in &self.0 {
            if let Some(first) = element.text().re_first_compiled(&compiled, opts) {
                return Ok(Some(first));
            }
        }
        Ok(None)
    }

    /// The first element for which `pred` returns true.
    pub fn search(&self, pred: impl Fn(&Selector) -> bool) -> Option<&Selector> {
        self.0.iter().find(|element| pred(element))
    }

    /// Keep only the elements for which `pred` returns true.
    pub fn filter(&self, pred: impl Fn(&Selector) -> bool) -> Selectors {
        Selectors(
            self.0
                .iter()
                .filter(|element| pred(element))
                .cloned()
                .collect(),
        )
    }

    /// Serialize the first element, or `None` when empty.
    pub fn get(&self) -> Option<Text> {
        self.0.first().map(|element| element.get())
    }

    /// Serialize every element.
    pub fn getall(&self) -> Texts {
        Texts::new(self.0.iter().map(|element| element.get()))
    }

    /// The first element, or `None`.
    pub fn first(&self) -> Option<&Selector> {
        self.0.first()
    }

    /// The last element, or `None`.
    pub fn last(&self) -> Option<&Selector> {
        self.0.last()
    }

    /// Number of elements (Python's `length`).
    pub fn length(&self) -> usize {
        self.0.len()
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over the elements.
    pub fn iter(&self) -> std::slice::Iter<'_, Selector> {
        self.0.iter()
    }

    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<Selector> {
        self.0
    }
}

/// Compile `pattern` once for a whole list, honouring [`ReOptions::case_sensitive`].
///
/// The same message `text::Text` produces, so a caller cannot tell which layer compiled it.
fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| Error::other(format!("invalid regex `{pattern}`: {error}")))
}

impl Deref for Selectors {
    type Target = [Selector];

    fn deref(&self) -> &[Selector] {
        &self.0
    }
}

impl IntoIterator for Selectors {
    type Item = Selector;
    type IntoIter = std::vec::IntoIter<Selector>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Selectors {
    type Item = &'a Selector;
    type IntoIter = std::slice::Iter<'a, Selector>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<Selector> for Selectors {
    fn from_iter<I: IntoIterator<Item = Selector>>(iter: I) -> Self {
        Selectors(iter.into_iter().collect())
    }
}

impl Index<usize> for Selectors {
    type Output = Selector;

    fn index(&self, index: usize) -> &Selector {
        &self.0[index]
    }
}

impl From<Vec<Selector>> for Selectors {
    fn from(items: Vec<Selector>) -> Self {
        Selectors(items)
    }
}

impl From<Selectors> for Vec<Selector> {
    fn from(items: Selectors) -> Self {
        items.0
    }
}

impl Extend<Selector> for Selectors {
    fn extend<I: IntoIterator<Item = Selector>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{Selector, Selectors};

    #[test]
    fn list_helpers_behave_like_the_python_container() {
        let page = Selector::new("<ul><li>a</li><li>b</li><li>c</li></ul>").unwrap();
        let items = page.css("li").unwrap();

        assert_eq!(items.len(), 3);
        assert_eq!(items.length(), 3);
        assert!(!items.is_empty());
        assert_eq!(items[0].text(), "a");
        assert_eq!(items.first().map(|e| e.text()), Some("a".into()));
        assert_eq!(items.last().map(|e| e.text()), Some("c".into()));
        assert_eq!(items.get(), Some("<li>a</li>".into()));
        assert_eq!(items.getall().len(), 3);
        assert_eq!(items.iter().count(), 3);

        let filtered = items.filter(|element| element.text() == "b");
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            items
                .search(|element| element.text() == "c")
                .map(|e| e.text()),
            Some("c".into())
        );

        let mut collected: Selectors = filtered.clone();
        collected.extend(items.clone());
        assert_eq!(collected.len(), 4);

        let round_trip: Selectors = items.clone().into_iter().collect();
        assert_eq!(round_trip.len(), 3);
        assert_eq!(Vec::from(round_trip).len(), 3);
    }

    #[test]
    fn css_and_regex_are_flattened_over_every_element() {
        let page = Selector::new("<div><p>one 1</p></div><div><p>two 2</p></div>").unwrap();
        let divs = page.css("div").unwrap();

        assert_eq!(
            divs.css("p::text").unwrap().getall().getall(),
            ["one 1", "two 2"]
        );
        assert_eq!(
            divs.css("p")
                .unwrap()
                .re(r"\d", Default::default())
                .unwrap()
                .getall(),
            ["1", "2"]
        );
        assert_eq!(
            divs.css("p")
                .unwrap()
                .re_first(r"\d", Default::default())
                .unwrap(),
            Some("1".into())
        );
    }

    #[test]
    fn an_empty_list_is_harmless() {
        let empty = Selectors::default();
        assert!(empty.is_empty());
        assert!(empty.get().is_none());
        assert!(empty.first().is_none());
        assert!(empty.last().is_none());
        assert!(empty.getall().is_empty());
        assert!(empty.css("div").unwrap().is_empty());
        assert!(empty.re(r"\d", Default::default()).unwrap().is_empty());
        assert!(empty.re_first(r"\d", Default::default()).unwrap().is_none());
    }

    #[test]
    fn a_broken_pattern_is_reported_even_for_an_empty_list() {
        // The selector and the regex are compiled once, up front, so a mistake in either is a
        // crate error rather than something that only shows up on a non-empty page.
        let empty = Selectors::default();
        assert!(empty.css("div[").is_err());
        assert!(empty.re("(", Default::default()).is_err());
        assert!(empty.re_first("(", Default::default()).is_err());
    }
}
