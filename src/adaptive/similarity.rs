//! Scoring and relocation — a port of `Selector.__calculate_similarity_score`,
//! `Selector.__calculate_dict_diff` and `Selector.relocate`.

use std::collections::BTreeMap;

use similar::TextDiff;

use crate::adaptive::fingerprint::{
    fingerprint, truncate_chars, ElementFingerprint, MAX_LIST_ITEMS, MAX_TEXT_CHARS,
};
use crate::parser::{Selector, Selectors};

/// The attributes Python scores a second time on their own, so that a full structural
/// rewrite still leaves something recognizable behind.
const SINGLED_OUT_ATTRIBUTES: [&str; 4] = ["class", "id", "href", "src"];

/// Score how well `candidate` matches `original`, from 0.0 to 100.0.
///
/// Each check contributes at most `1.0` to the running score and `1` to the number of
/// checks; the result is `round((score / checks) * 100, 2)`, exactly as in Python. Every
/// "similarity" is `difflib.SequenceMatcher(None, a, b).ratio()`, computed here with
/// [`similar::TextDiff::ratio`], which uses the same `2 * matches / total` formula.
///
/// A stored fingerprint can be arbitrarily large — it may have been written by the Python
/// library, which applies no limits — so every value handed to the diff is cut down first:
/// at most 512 characters per value and 256 entries per sequence. Both sides are cut the
/// same way, so an element still scores 100 against its own fingerprint.
pub fn similarity_score(original: &ElementFingerprint, candidate: &Selector) -> f64 {
    let data = fingerprint(candidate);

    let mut score = 0.0_f64;
    let mut checks = 0_u32;

    // 1. The tag name.
    if original.tag == data.tag {
        score += 1.0;
    }
    checks += 1;

    // 2. The element's own text, only when the original had some.
    if let Some(text) = non_empty(original.text.as_deref()) {
        score += ratio_chars(text, data.text.as_deref().unwrap_or_default());
        checks += 1;
    }

    // 3. The attribute map. If neither element has attributes, that still counts for
    //    something — two empty sequences have a ratio of 1.0.
    score += dict_diff(&original.attributes, &data.attributes);
    checks += 1;

    // 4. A separate check per singled-out attribute the original carried.
    for name in SINGLED_OUT_ATTRIBUTES {
        if let Some(value) = non_empty(original.attribute(name)) {
            score += ratio_chars(value, data.attribute(name).unwrap_or_default());
            checks += 1;
        }
    }

    // 5. The path of tag names from the root.
    score += ratio_slices(&original.path, &data.path);
    checks += 1;

    // 6. The parent, when the original had one and the candidate has one too. When the
    //    candidate has no parent Python adds nothing and counts nothing, so the missing
    //    parent neither helps nor hurts; that is deliberate on their side and kept here.
    if original.has_parent() {
        if let Some(parent_name) = non_empty(data.parent_name.as_deref()) {
            let original_parent = original.parent_name.as_deref().unwrap_or_default();
            score += ratio_chars(original_parent, parent_name);
            checks += 1;

            score += dict_diff(&original.parent_attribs, &data.parent_attribs);
            checks += 1;

            if let Some(parent_text) = non_empty(original.parent_text.as_deref()) {
                score += ratio_chars(parent_text, data.parent_text.as_deref().unwrap_or_default());
                checks += 1;
            }
        }
    }

    // 7. The siblings' tag names, only when the original had siblings.
    if !original.siblings.is_empty() {
        score += ratio_slices(&original.siblings, &data.siblings);
        checks += 1;
    }

    // Unreachable — the tag, attribute and path checks always run — but dividing by zero
    // the way Python would is not an improvement.
    if checks == 0 {
        return 0.0;
    }

    round2((score / f64::from(checks)) * 100.0)
}

/// Search `root` for the elements that best match `fingerprint`, keeping only scores at or
/// above `percentage` (Scrapling's default is `40.0`).
///
/// Every element below `root` is scored — the search does not stop at the first perfect
/// match — and all the elements sharing the highest score come back together, in document
/// order. An empty list means nothing reached `percentage`, which is also what a `NaN`
/// threshold gives, since Python's `>=` is false against `NaN` too.
#[allow(clippy::float_cmp)]
pub fn relocate(root: &Selector, fingerprint: &ElementFingerprint, percentage: f64) -> Selectors {
    let candidates = root.below_elements();

    let mut scores = Vec::with_capacity(candidates.len());
    let mut highest = f64::NEG_INFINITY;
    for candidate in candidates.iter() {
        let score = similarity_score(fingerprint, candidate);
        if score > highest {
            highest = score;
        }
        scores.push(score);
    }

    // `>=` is false against a `NaN` threshold, which is how Python's comparison behaves too.
    let reached_threshold = highest >= percentage;
    if scores.is_empty() || !reached_threshold {
        return Selectors::default();
    }

    // Every score has been through `round2`, so equal scores are bit-identical and this
    // groups exactly the way Python's `score_table` dictionary does.
    Selectors::new(
        candidates
            .iter()
            .zip(scores)
            .filter(|(_, score)| *score == highest)
            .map(|(candidate, _)| candidate.clone()),
    )
}

/// Python's `__calculate_dict_diff`: half the score for the keys, half for the values.
///
/// Python compares the maps in lxml's attribute order; the fingerprint stores them in a
/// `BTreeMap`, so both sides are compared in sorted order instead. The ordering is the same
/// on both sides of every comparison, which is what the ratio depends on.
fn dict_diff(left: &BTreeMap<String, String>, right: &BTreeMap<String, String>) -> f64 {
    let left_keys: Vec<&str> = left
        .keys()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();
    let right_keys: Vec<&str> = right
        .keys()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();
    let left_values: Vec<&str> = left
        .values()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();
    let right_values: Vec<&str> = right
        .values()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();

    ratio_str_slices(&left_keys, &right_keys) * 0.5
        + ratio_str_slices(&left_values, &right_values) * 0.5
}

/// `SequenceMatcher(None, a, b).ratio()` over two strings, compared character by character.
///
/// Both sides are cut to [`MAX_TEXT_CHARS`] first: the diff is quadratic and runs once per
/// candidate element, so an unbounded value taken from a remote page would be a way to make
/// a single relocation take minutes.
fn ratio_chars(left: &str, right: &str) -> f64 {
    let left = truncate_chars(left, MAX_TEXT_CHARS);
    let right = truncate_chars(right, MAX_TEXT_CHARS);
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    f64::from(TextDiff::from_chars(left, right).ratio())
}

/// `SequenceMatcher(None, a, b).ratio()` over two sequences of strings.
fn ratio_slices(left: &[String], right: &[String]) -> f64 {
    let left: Vec<&str> = left
        .iter()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();
    let right: Vec<&str> = right
        .iter()
        .take(MAX_LIST_ITEMS)
        .map(String::as_str)
        .collect();
    ratio_str_slices(&left, &right)
}

/// `SequenceMatcher(None, a, b).ratio()` over two already-borrowed sequences of strings.
///
/// Like [`ratio_chars`] this bounds what it is given: at most [`MAX_LIST_ITEMS`] entries,
/// each cut to [`MAX_TEXT_CHARS`] characters.
fn ratio_str_slices(left: &[&str], right: &[&str]) -> f64 {
    let left: Vec<&str> = left
        .iter()
        .take(MAX_LIST_ITEMS)
        .copied()
        .map(|value| truncate_chars(value, MAX_TEXT_CHARS))
        .collect();
    let right: Vec<&str> = right
        .iter()
        .take(MAX_LIST_ITEMS)
        .copied()
        .map(|value| truncate_chars(value, MAX_TEXT_CHARS))
        .collect();

    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    f64::from(TextDiff::from_slices(&left, &right).ratio())
}

/// `Some(value)` when the value is present and not empty — Python's truthiness test.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Python's `round(value, 2)` for the values this module produces.
///
/// Python rounds half to even and this rounds half away from zero; the two differ only on
/// exact halves of a hundredth, which changes nothing about which bucket wins.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn page(html: &str) -> Selector {
        Selector::new(html).expect("the test document parses")
    }

    fn first(page: &Selector, selector: &str) -> Selector {
        page.css_first(selector)
            .expect("the test selector parses")
            .expect("the test selector matches")
    }

    #[test]
    fn an_element_is_a_perfect_match_for_itself() {
        let document = page(BEFORE);
        let element = first(&document, "#p1");
        let print = fingerprint(&element);
        assert_eq!(similarity_score(&print, &element), 100.0);
    }

    #[test]
    fn a_sibling_scores_lower_than_the_element_itself() {
        let document = page(BEFORE);
        let element = first(&document, "#p1");
        let sibling = first(&document, "#p2");
        let print = fingerprint(&element);

        assert!(similarity_score(&print, &sibling) < similarity_score(&print, &element));
    }

    #[test]
    fn every_score_stays_inside_the_documented_range() {
        let document = page(BEFORE);
        let print = fingerprint(&first(&document, "#p1"));
        for candidate in document.below_elements().iter() {
            let score = similarity_score(&print, candidate);
            assert!(
                (0.0..=100.0).contains(&score),
                "score out of range: {score}"
            );
        }
    }

    #[test]
    fn relocates_the_renamed_product_across_a_structural_change() {
        let old_page = page(BEFORE);
        let print = fingerprint(&first(&old_page, "#p1"));

        let new_page = page(AFTER);
        let found = relocate(&new_page, &print, 40.0);

        assert_eq!(found.len(), 1, "expected exactly one best match");
        let element = found.first().expect("one element was relocated");
        assert_eq!(element.tag(), "article");
        assert_eq!(
            element.attr("data-id").map(|value| value.to_string()),
            Some("p1".to_string())
        );
    }

    #[test]
    fn relocation_returns_nothing_when_the_threshold_is_out_of_reach() {
        let old_page = page(BEFORE);
        let print = fingerprint(&first(&old_page, "#p1"));

        let new_page = page(AFTER);
        assert!(relocate(&new_page, &print, 100.0).is_empty());
    }

    #[test]
    fn a_nan_threshold_relocates_nothing() {
        let old_page = page(BEFORE);
        let print = fingerprint(&first(&old_page, "#p1"));
        assert!(relocate(&old_page, &print, f64::NAN).is_empty());
    }

    #[test]
    fn a_degenerate_fingerprint_is_handled_without_panicking() {
        let empty = page("");
        let print = ElementFingerprint::default();
        // A record with no tag, no path and no attributes still divides by a non-zero
        // number of checks and indexes nothing out of bounds.
        let _ = relocate(&empty, &print, 40.0);
        assert!(relocate(&empty, &print, 100.0).is_empty());
    }

    #[test]
    fn identical_pages_relocate_to_the_same_element() {
        let old_page = page(BEFORE);
        let print = fingerprint(&first(&old_page, "#p1"));

        let same_page = page(BEFORE);
        let found = relocate(&same_page, &print, 40.0);

        assert_eq!(found.len(), 1);
        let element = found.first().expect("one element was relocated");
        assert_eq!(
            element.attr("id").map(|value| value.to_string()),
            Some("p1".to_string())
        );
    }

    #[test]
    fn dict_diff_is_half_keys_and_half_values() {
        let mut left = BTreeMap::new();
        left.insert("class".to_string(), "product".to_string());
        left.insert("id".to_string(), "p1".to_string());

        assert_eq!(dict_diff(&left, &left), 1.0);
        assert_eq!(dict_diff(&BTreeMap::new(), &BTreeMap::new()), 1.0);

        let mut right = BTreeMap::new();
        right.insert("class".to_string(), "product".to_string());
        right.insert("id".to_string(), "p2".to_string());
        // The keys match completely; one of the two values does not.
        assert_eq!(dict_diff(&left, &right), 0.5 + 0.5 * 0.5);
    }

    #[test]
    fn ratios_match_the_python_sequence_matcher() {
        assert_eq!(ratio_chars("abcd", "bcde"), 0.75);
        assert_eq!(ratio_chars("", ""), 1.0);
        assert_eq!(ratio_chars("abc", ""), 0.0);
        assert_eq!(ratio_str_slices(&["a", "b"], &["a", "b"]), 1.0);
        assert_eq!(ratio_str_slices(&["a", "b"], &["a", "c"]), 0.5);
        assert_eq!(ratio_str_slices(&[], &[]), 1.0);
        assert_eq!(ratio_str_slices(&["a"], &[]), 0.0);
    }

    #[test]
    fn oversized_values_are_cut_before_the_diff_runs() {
        let long = "x".repeat(MAX_TEXT_CHARS * 4);
        // Both sides are cut to the same prefix, so they still read as identical.
        assert_eq!(ratio_chars(&long, &long), 1.0);
        // And the cut is what keeps the comparison affordable.
        let longer = format!("{long}{long}");
        assert_eq!(ratio_chars(&long, &longer), 1.0);
    }

    #[test]
    fn rounding_keeps_two_decimals() {
        assert_eq!(round2(74.9166), 74.92);
        assert_eq!(round2(100.0), 100.0);
    }
}
