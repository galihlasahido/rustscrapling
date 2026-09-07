//! CSS query preprocessing: comma-separated selector lists and the `::text` / `::attr(name)`
//! pseudo-elements.
//!
//! Python Scrapling gets these two pseudo-elements by subclassing `cssselect`'s
//! `HTMLTranslator` (`scrapling/core/translator.py`): the pseudo-element is peeled off the
//! parsed selector and turned into a trailing `/text()` or `/@attr` XPath step. `scraper` has
//! no such hook, so we do the same peeling textually — strip the trailing pseudo-element,
//! compile what is left with [`scraper::Selector`], and map the matched elements to their
//! direct child text nodes or to an attribute value afterwards.

use crate::error::{Error, Result};

/// The trailing pseudo-element of a single CSS query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pseudo {
    /// No pseudo-element: the query yields elements.
    None,
    /// `::text`: the query yields the direct child text nodes of every match.
    Text,
    /// `::attr(name)`: the query yields the named attribute value of every match.
    Attr(String),
}

/// One compiled member of a comma-separated CSS selector list.
pub(super) struct CompiledQuery {
    /// The element part of the query, compiled by `scraper`.
    pub(super) selector: scraper::Selector,
    /// What to do with the elements it matches.
    pub(super) pseudo: Pseudo,
}

impl std::fmt::Debug for CompiledQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledQuery")
            .field("pseudo", &self.pseudo)
            .finish_non_exhaustive()
    }
}

/// Split a CSS selector group on its top-level commas.
///
/// Commas inside quotes, `[...]` attribute selectors and `:pseudo(...)` arguments are left
/// alone, so `a[title="x,y"], b:not(c, d)` splits into exactly two members.
pub(super) fn split_selector_list(query: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut parens: i32 = 0;
    let mut brackets: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in query.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                current.push(ch);
                escaped = true;
            }
            '\'' | '"' => {
                match quote {
                    Some(open) if open == ch => quote = None,
                    Some(_) => {}
                    None => quote = Some(ch),
                }
                current.push(ch);
            }
            '(' if quote.is_none() => {
                parens += 1;
                current.push(ch);
            }
            ')' if quote.is_none() => {
                parens -= 1;
                current.push(ch);
            }
            '[' if quote.is_none() => {
                brackets += 1;
                current.push(ch);
            }
            ']' if quote.is_none() => {
                brackets -= 1;
                current.push(ch);
            }
            ',' if quote.is_none() && parens <= 0 && brackets <= 0 => {
                parts.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    parts.push(current.trim().to_string());
    parts.retain(|part| !part.is_empty());
    parts
}

/// Split one selector into its element part and its trailing pseudo-element.
///
/// `div.product::text` becomes `("div.product", Pseudo::Text)`, `a::attr(href)` becomes
/// `("a", Pseudo::Attr("href"))`, and `::attr(href)` becomes `("", Pseudo::Attr("href"))` —
/// the caller substitutes `*` for an empty element part, exactly like `cssselect` does.
pub(super) fn strip_pseudo_element(part: &str) -> Result<(String, Pseudo)> {
    let trimmed = part.trim();
    // `to_ascii_lowercase` never changes the byte length, so indices stay valid in `trimmed`.
    let lowered = trimmed.to_ascii_lowercase();

    if trimmed.ends_with(')') {
        if let Some(index) = lowered.rfind("::attr(") {
            let open = index + "::attr(".len();
            let argument = &trimmed[open..trimmed.len() - 1];
            let name = argument
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim()
                .to_string();
            if name.is_empty() {
                return Err(Error::selector(
                    part,
                    "::attr() expects a single attribute name",
                ));
            }
            return Ok((trimmed[..index].trim().to_string(), Pseudo::Attr(name)));
        }
    }

    if lowered.ends_with("::text") {
        let cut = trimmed.len() - "::text".len();
        return Ok((trimmed[..cut].trim().to_string(), Pseudo::Text));
    }

    Ok((trimmed.to_string(), Pseudo::None))
}

/// Compile a whole CSS selector group into one [`CompiledQuery`] per list member, keeping the
/// order the members were written in.
pub(super) fn compile(query: &str) -> Result<Vec<CompiledQuery>> {
    let parts = split_selector_list(query);
    if parts.is_empty() {
        return Err(Error::selector(query, "the selector is empty"));
    }

    let mut compiled = Vec::with_capacity(parts.len());
    for part in parts {
        let (element_part, pseudo) = strip_pseudo_element(&part)?;
        let element_part = if element_part.is_empty() {
            "*".to_string()
        } else {
            element_part
        };
        let selector = scraper::Selector::parse(&element_part)
            .map_err(|error| Error::selector(query, error.to_string()))?;
        compiled.push(CompiledQuery { selector, pseudo });
    }
    Ok(compiled)
}

/// Escape the characters that cannot appear literally inside a CSS double-quoted string.
///
/// Follows `_escape_css_string` in `scrapling/parser.py` — line breaks become hexadecimal escapes
/// with a terminating space — with one deliberate difference: the backslash itself is escaped
/// first. Python leaves it alone, which lets a value ending in `\` swallow the closing quote of
/// the string `Filter::to_css` builds (`[data-x="a\"]`), and lets a value
/// carrying `\"` continue the selector with syntax of its own. Filter values routinely come from
/// page content, so the escape has to hold for any input.
pub(super) fn escape_css_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\A ")
        .replace('\r', "\\D ")
        .replace('\u{c}', "\\C ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_commas_only() {
        assert_eq!(split_selector_list("h2, h3"), vec!["h2", "h3"]);
        assert_eq!(
            split_selector_list(r#"a[title="x,y"], b:not(c, d)"#),
            vec![r#"a[title="x,y"]"#, "b:not(c, d)"]
        );
        assert_eq!(split_selector_list("div"), vec!["div"]);
        assert!(split_selector_list("   ").is_empty());
    }

    #[test]
    fn peels_the_trailing_pseudo_element() {
        assert_eq!(
            strip_pseudo_element("div.product::text").unwrap(),
            ("div.product".to_string(), Pseudo::Text)
        );
        assert_eq!(
            strip_pseudo_element("a::attr(href)").unwrap(),
            ("a".to_string(), Pseudo::Attr("href".to_string()))
        );
        assert_eq!(
            strip_pseudo_element("::attr('href')").unwrap(),
            (String::new(), Pseudo::Attr("href".to_string()))
        );
        assert_eq!(
            strip_pseudo_element("li:nth-child(2)").unwrap(),
            ("li:nth-child(2)".to_string(), Pseudo::None)
        );
    }

    #[test]
    fn rejects_an_empty_attribute_name() {
        assert!(strip_pseudo_element("a::attr()").is_err());
    }

    #[test]
    fn compiles_every_list_member_in_order() {
        let compiled = compile("h3, h2::text").unwrap();
        assert_eq!(compiled.len(), 2);
        assert_eq!(compiled[0].pseudo, Pseudo::None);
        assert_eq!(compiled[1].pseudo, Pseudo::Text);
    }

    #[test]
    fn reports_a_broken_selector_as_a_crate_error() {
        let error = compile("div..").unwrap_err();
        assert!(matches!(error, Error::Selector { .. }));
    }

    #[test]
    fn escapes_css_string_literals() {
        assert_eq!(escape_css_string("a\"b"), "a\\\"b");
        assert_eq!(escape_css_string("a\nb"), "a\\A b");
        // A backslash is escaped, so it cannot swallow the closing quote of the string
        // `Filter::to_css` wraps the value in.
        assert_eq!(escape_css_string("a\\b"), "a\\\\b");
        assert_eq!(escape_css_string("a\\"), "a\\\\");
        assert_eq!(
            escape_css_string("a\\\" i], [onclick"),
            "a\\\\\\\" i], [onclick"
        );
    }
}
