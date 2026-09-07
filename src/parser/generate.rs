//! Selector generation — the port of `SelectorsGeneration` in `scrapling/core/mixins.py`.
//!
//! The algorithm walks up the tree collecting one part per ancestor. An `id` is treated as
//! enough on its own: the short variant stops there, the full variant keeps walking to the
//! document root. Classes are deliberately *not* used, because sites share exact class names
//! between unrelated elements — the same reasoning (and the same commented-out code) is in the
//! Python original.

use crate::parser::selector::Selector;

/// How many ancestors the walk climbs before giving up.
///
/// The tree is acyclic, so the walk always terminates on its own; the cap is there so that a
/// remote page nesting elements hundreds of thousands deep cannot turn one selector into a
/// multi-megabyte string.
const MAX_SELECTOR_DEPTH: usize = 512;

/// Build a CSS selector for `start`.
///
/// With `full_path` disabled the walk stops at the nearest ancestor carrying an `id`; with it
/// enabled the walk always continues up to (but excluding) `<html>`.
pub(super) fn general_selection(start: &Selector, full_path: bool) -> String {
    if start.is_text_node() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();
    let mut target = start.clone();

    for _ in 0..MAX_SELECTOR_DEPTH {
        let Some(parent) = target.parent() else { break };

        let id = target
            .attr("id")
            .map(|value| value.into_string())
            .filter(|value| !value.is_empty());

        match id {
            Some(value) => {
                parts.push(format!("#{value}"));
                if !full_path {
                    parts.reverse();
                    return parts.join(" > ");
                }
            }
            None => parts.push(numbered_part(&target)),
        }

        if parent.tag() == "html" {
            break;
        }
        target = parent;
    }

    parts.reverse();
    parts.join(" > ")
}

/// The tag name of `target`, with `:nth-of-type(n)` appended when it is not the first sibling
/// of its own tag.
///
/// Faithful to Python: the counter only looks at the siblings *before* `target`, so the very
/// first element of a repeated group is written without an index. Counted by walking backwards
/// from `target` rather than by listing the parent's children, so nothing is allocated for a
/// container with a very large number of children.
fn numbered_part(target: &Selector) -> String {
    let tag = target.tag();
    let mut position = 1usize;
    let mut previous = target.previous();

    while let Some(sibling) = previous {
        if sibling.tag() == tag {
            position += 1;
        }
        previous = sibling.previous();
    }

    if position > 1 {
        format!("{tag}:nth-of-type({position})")
    } else {
        tag.to_string()
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{Selector, OVERVIEW_HTML};

    #[test]
    fn an_id_is_enough_for_the_short_selector() {
        let page = Selector::new(OVERVIEW_HTML).unwrap();
        let section = page.css_first("#products").unwrap().unwrap();

        assert_eq!(section.generate_css_selector(), "#products");
        assert_eq!(
            section.generate_full_css_selector(),
            "body > main > #products"
        );
    }

    #[test]
    fn repeated_siblings_get_an_nth_of_type_index() {
        let page = Selector::new("<div><p>a</p><p>b</p><p>c</p></div>").unwrap();
        let paragraphs = page.css("p").unwrap();

        assert_eq!(paragraphs[0].generate_css_selector(), "body > div > p");
        assert_eq!(
            paragraphs[1].generate_css_selector(),
            "body > div > p:nth-of-type(2)"
        );
        assert_eq!(
            paragraphs[2].generate_css_selector(),
            "body > div > p:nth-of-type(3)"
        );
    }

    #[test]
    fn text_nodes_have_no_selector() {
        let page = Selector::new("<h1>hello</h1>").unwrap();
        let text = page.css("h1::text").unwrap();

        assert_eq!(text[0].generate_css_selector(), "");
        assert_eq!(text[0].generate_full_css_selector(), "");
    }
}
