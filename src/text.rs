//! String types with the scraping helpers Scrapling adds to `str`, `list[str]` and an
//! element's attribute mapping.
//!
//! This is a port of `scrapling/core/custom_types.py`:
//!
//! | Python              | Rust           |
//! |---------------------|----------------|
//! | `TextHandler`       | [`Text`]       |
//! | `TextHandlers`      | [`Texts`]      |
//! | `AttributesHandler` | [`Attributes`] |
//!
//! Python passes the regex behaviour around as keyword arguments; here the same knobs live in
//! [`ReOptions`], which has builder-style setters so call sites read the same way:
//!
//! ```
//! use rustscrapling::text::{ReOptions, Text};
//!
//! let t = Text::new("Price: 10 USD / 20 usd");
//! let found = t.re(r"(\d+) usd", ReOptions::new().case_sensitive(false))?;
//! assert_eq!(found.getall(), vec!["10".to_string(), "20".to_string()]);
//! # Ok::<(), rustscrapling::Error>(())
//! ```

#![forbid(unsafe_code)]

use std::borrow::{Borrow, Cow};
use std::collections::HashMap;
use std::fmt;
use std::ops::{Deref, Index};

use regex::{Regex, RegexBuilder};
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------------------------
// HTML character entities
// ---------------------------------------------------------------------------------------------

/// `html.entities.name2codepoint` from the Python standard library, sorted by name so that it
/// can be binary-searched. This is exactly the table `w3lib.html.replace_entities` consults.
static NAMED_ENTITIES: &[(&str, u32)] = &[
    ("AElig", 198),
    ("Aacute", 193),
    ("Acirc", 194),
    ("Agrave", 192),
    ("Alpha", 913),
    ("Aring", 197),
    ("Atilde", 195),
    ("Auml", 196),
    ("Beta", 914),
    ("Ccedil", 199),
    ("Chi", 935),
    ("Dagger", 8225),
    ("Delta", 916),
    ("ETH", 208),
    ("Eacute", 201),
    ("Ecirc", 202),
    ("Egrave", 200),
    ("Epsilon", 917),
    ("Eta", 919),
    ("Euml", 203),
    ("Gamma", 915),
    ("Iacute", 205),
    ("Icirc", 206),
    ("Igrave", 204),
    ("Iota", 921),
    ("Iuml", 207),
    ("Kappa", 922),
    ("Lambda", 923),
    ("Mu", 924),
    ("Ntilde", 209),
    ("Nu", 925),
    ("OElig", 338),
    ("Oacute", 211),
    ("Ocirc", 212),
    ("Ograve", 210),
    ("Omega", 937),
    ("Omicron", 927),
    ("Oslash", 216),
    ("Otilde", 213),
    ("Ouml", 214),
    ("Phi", 934),
    ("Pi", 928),
    ("Prime", 8243),
    ("Psi", 936),
    ("Rho", 929),
    ("Scaron", 352),
    ("Sigma", 931),
    ("THORN", 222),
    ("Tau", 932),
    ("Theta", 920),
    ("Uacute", 218),
    ("Ucirc", 219),
    ("Ugrave", 217),
    ("Upsilon", 933),
    ("Uuml", 220),
    ("Xi", 926),
    ("Yacute", 221),
    ("Yuml", 376),
    ("Zeta", 918),
    ("aacute", 225),
    ("acirc", 226),
    ("acute", 180),
    ("aelig", 230),
    ("agrave", 224),
    ("alefsym", 8501),
    ("alpha", 945),
    ("amp", 38),
    ("and", 8743),
    ("ang", 8736),
    ("aring", 229),
    ("asymp", 8776),
    ("atilde", 227),
    ("auml", 228),
    ("bdquo", 8222),
    ("beta", 946),
    ("brvbar", 166),
    ("bull", 8226),
    ("cap", 8745),
    ("ccedil", 231),
    ("cedil", 184),
    ("cent", 162),
    ("chi", 967),
    ("circ", 710),
    ("clubs", 9827),
    ("cong", 8773),
    ("copy", 169),
    ("crarr", 8629),
    ("cup", 8746),
    ("curren", 164),
    ("dArr", 8659),
    ("dagger", 8224),
    ("darr", 8595),
    ("deg", 176),
    ("delta", 948),
    ("diams", 9830),
    ("divide", 247),
    ("eacute", 233),
    ("ecirc", 234),
    ("egrave", 232),
    ("empty", 8709),
    ("emsp", 8195),
    ("ensp", 8194),
    ("epsilon", 949),
    ("equiv", 8801),
    ("eta", 951),
    ("eth", 240),
    ("euml", 235),
    ("euro", 8364),
    ("exist", 8707),
    ("fnof", 402),
    ("forall", 8704),
    ("frac12", 189),
    ("frac14", 188),
    ("frac34", 190),
    ("frasl", 8260),
    ("gamma", 947),
    ("ge", 8805),
    ("gt", 62),
    ("hArr", 8660),
    ("harr", 8596),
    ("hearts", 9829),
    ("hellip", 8230),
    ("iacute", 237),
    ("icirc", 238),
    ("iexcl", 161),
    ("igrave", 236),
    ("image", 8465),
    ("infin", 8734),
    ("int", 8747),
    ("iota", 953),
    ("iquest", 191),
    ("isin", 8712),
    ("iuml", 239),
    ("kappa", 954),
    ("lArr", 8656),
    ("lambda", 955),
    ("lang", 9001),
    ("laquo", 171),
    ("larr", 8592),
    ("lceil", 8968),
    ("ldquo", 8220),
    ("le", 8804),
    ("lfloor", 8970),
    ("lowast", 8727),
    ("loz", 9674),
    ("lrm", 8206),
    ("lsaquo", 8249),
    ("lsquo", 8216),
    ("lt", 60),
    ("macr", 175),
    ("mdash", 8212),
    ("micro", 181),
    ("middot", 183),
    ("minus", 8722),
    ("mu", 956),
    ("nabla", 8711),
    ("nbsp", 160),
    ("ndash", 8211),
    ("ne", 8800),
    ("ni", 8715),
    ("not", 172),
    ("notin", 8713),
    ("nsub", 8836),
    ("ntilde", 241),
    ("nu", 957),
    ("oacute", 243),
    ("ocirc", 244),
    ("oelig", 339),
    ("ograve", 242),
    ("oline", 8254),
    ("omega", 969),
    ("omicron", 959),
    ("oplus", 8853),
    ("or", 8744),
    ("ordf", 170),
    ("ordm", 186),
    ("oslash", 248),
    ("otilde", 245),
    ("otimes", 8855),
    ("ouml", 246),
    ("para", 182),
    ("part", 8706),
    ("permil", 8240),
    ("perp", 8869),
    ("phi", 966),
    ("pi", 960),
    ("piv", 982),
    ("plusmn", 177),
    ("pound", 163),
    ("prime", 8242),
    ("prod", 8719),
    ("prop", 8733),
    ("psi", 968),
    ("quot", 34),
    ("rArr", 8658),
    ("radic", 8730),
    ("rang", 9002),
    ("raquo", 187),
    ("rarr", 8594),
    ("rceil", 8969),
    ("rdquo", 8221),
    ("real", 8476),
    ("reg", 174),
    ("rfloor", 8971),
    ("rho", 961),
    ("rlm", 8207),
    ("rsaquo", 8250),
    ("rsquo", 8217),
    ("sbquo", 8218),
    ("scaron", 353),
    ("sdot", 8901),
    ("sect", 167),
    ("shy", 173),
    ("sigma", 963),
    ("sigmaf", 962),
    ("sim", 8764),
    ("spades", 9824),
    ("sub", 8834),
    ("sube", 8838),
    ("sum", 8721),
    ("sup", 8835),
    ("sup1", 185),
    ("sup2", 178),
    ("sup3", 179),
    ("supe", 8839),
    ("szlig", 223),
    ("tau", 964),
    ("there4", 8756),
    ("theta", 952),
    ("thetasym", 977),
    ("thinsp", 8201),
    ("thorn", 254),
    ("tilde", 732),
    ("times", 215),
    ("trade", 8482),
    ("uArr", 8657),
    ("uacute", 250),
    ("uarr", 8593),
    ("ucirc", 251),
    ("ugrave", 249),
    ("uml", 168),
    ("upsih", 978),
    ("upsilon", 965),
    ("uuml", 252),
    ("weierp", 8472),
    ("xi", 958),
    ("yacute", 253),
    ("yen", 165),
    ("yuml", 255),
    ("zeta", 950),
    ("zwj", 8205),
    ("zwnj", 8204),
];

/// Windows-1252 replacements for numeric references in the `0x80..=0x9F` range, which browsers
/// (and therefore `w3lib`) interpret as cp1252 rather than as C1 control characters. `None`
/// marks the byte values cp1252 leaves undefined.
static CP1252_C1: [Option<char>; 32] = [
    Some('\u{20AC}'), // 0x80
    None,             // 0x81
    Some('\u{201A}'), // 0x82
    Some('\u{0192}'), // 0x83
    Some('\u{201E}'), // 0x84
    Some('\u{2026}'), // 0x85
    Some('\u{2020}'), // 0x86
    Some('\u{2021}'), // 0x87
    Some('\u{02C6}'), // 0x88
    Some('\u{2030}'), // 0x89
    Some('\u{0160}'), // 0x8A
    Some('\u{2039}'), // 0x8B
    Some('\u{0152}'), // 0x8C
    None,             // 0x8D
    Some('\u{017D}'), // 0x8E
    None,             // 0x8F
    None,             // 0x90
    Some('\u{2018}'), // 0x91
    Some('\u{2019}'), // 0x92
    Some('\u{201C}'), // 0x93
    Some('\u{201D}'), // 0x94
    Some('\u{2022}'), // 0x95
    Some('\u{2013}'), // 0x96
    Some('\u{2014}'), // 0x97
    Some('\u{02DC}'), // 0x98
    Some('\u{2122}'), // 0x99
    Some('\u{0161}'), // 0x9A
    Some('\u{203A}'), // 0x9B
    Some('\u{0153}'), // 0x9C
    None,             // 0x9D
    Some('\u{017E}'), // 0x9E
    Some('\u{0178}'), // 0x9F
];

/// Look a named entity up, first case-sensitively and then lower-cased, exactly like
/// `name2codepoint.get(name) or name2codepoint.get(name.lower())`.
fn named_entity(name: &str) -> Option<u32> {
    fn lookup(name: &str) -> Option<u32> {
        NAMED_ENTITIES
            .binary_search_by(|(candidate, _)| (*candidate).cmp(name))
            .ok()
            .map(|index| NAMED_ENTITIES[index].1)
    }

    if let Some(codepoint) = lookup(name) {
        return Some(codepoint);
    }
    lookup(&name.to_ascii_lowercase())
}

/// Try to read one entity reference at the start of `input` (which must start with `&`).
///
/// Returns how many bytes the reference occupied together with the text it expands to. An
/// unknown reference expands to the empty string when it ended in a semicolon and to itself
/// otherwise — the `remove_illegal=True` behaviour of `w3lib.html.replace_entities`.
fn parse_entity(input: &str) -> Option<(usize, String)> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'&') {
        return None;
    }

    // `&((?P<named>[a-z\d]+)|#(?P<dec>\d+)|#x(?P<hex>[a-f\d]+))(?P<semicolon>;?)`, ignoring case.
    let mut index = 1usize;
    let number: Option<u32>;

    if index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        number = named_entity(&input[start..index]);
    } else if index < bytes.len() && bytes[index] == b'#' {
        index += 1;
        if index < bytes.len() && bytes[index].is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            number = input[start..index].parse::<u32>().ok();
        } else if index < bytes.len() && (bytes[index] == b'x' || bytes[index] == b'X') {
            index += 1;
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
                index += 1;
            }
            if index == start {
                return None;
            }
            number = u32::from_str_radix(&input[start..index], 16).ok();
        } else {
            return None;
        }
    } else {
        return None;
    }

    let has_semicolon = index < bytes.len() && bytes[index] == b';';
    let consumed = if has_semicolon { index + 1 } else { index };

    let replacement = match number {
        Some(codepoint @ 0x80..=0x9F) => CP1252_C1[(codepoint - 0x80) as usize],
        Some(codepoint) => char::from_u32(codepoint),
        None => None,
    };

    match replacement {
        Some(character) => Some((consumed, character.to_string())),
        None if has_semicolon => Some((consumed, String::new())),
        None => Some((consumed, input[..consumed].to_string())),
    }
}

/// Replace every HTML character entity reference in `input` with the character it denotes.
fn replace_entities_str(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let tail = &rest[position..];
        match parse_entity(tail) {
            Some((consumed, replacement)) => {
                out.push_str(&replacement);
                rest = &tail[consumed..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------------
// Cleaning
// ---------------------------------------------------------------------------------------------

/// Whether `character` is whitespace for Python's `str.strip()`, which is what
/// `TextHandler.clean` finishes with.
///
/// `str::trim` trims the Unicode `White_Space` set; Python additionally treats the four
/// information separators `U+001C..=U+001F` as space, and remote HTML does contain them.
fn is_python_space(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

/// Port of `TextHandler.clean`: map `\t`, `\r` and `\n` to spaces, optionally replace entities,
/// collapse runs of spaces into one, and trim.
fn clean_str(input: &str, remove_entities: bool) -> String {
    let translated: String = input
        .chars()
        .map(|character| match character {
            '\t' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect();

    let data = if remove_entities {
        replace_entities_str(&translated)
    } else {
        translated
    };

    // `__CONSECUTIVE_SPACES_REGEX__` is `" +"`, so only U+0020 runs collapse.
    let mut collapsed = String::with_capacity(data.len());
    let mut previous_was_space = false;
    for character in data.chars() {
        if character == ' ' {
            if !previous_was_space {
                collapsed.push(' ');
            }
            previous_was_space = true;
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }

    collapsed.trim_matches(is_python_space).to_string()
}

// ---------------------------------------------------------------------------------------------
// ReOptions
// ---------------------------------------------------------------------------------------------

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

impl Default for ReOptions {
    fn default() -> Self {
        ReOptions {
            replace_entities: true,
            clean_match: false,
            case_sensitive: true,
        }
    }
}

impl ReOptions {
    /// The Python defaults: entities replaced, no cleaning, case-sensitive.
    pub fn new() -> Self {
        ReOptions::default()
    }

    /// Replace HTML entities in the results.
    pub fn replace_entities(self, yes: bool) -> Self {
        ReOptions {
            replace_entities: yes,
            ..self
        }
    }

    /// Match against the `clean()`ed text instead of the raw text.
    pub fn clean_match(self, yes: bool) -> Self {
        ReOptions {
            clean_match: yes,
            ..self
        }
    }

    /// Match case-sensitively. Only meaningful for the helpers that compile a pattern string.
    pub fn case_sensitive(self, yes: bool) -> Self {
        ReOptions {
            case_sensitive: yes,
            ..self
        }
    }
}

/// Compile `pattern`, honouring [`ReOptions::case_sensitive`].
fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| Error::Other(format!("invalid regex `{pattern}`: {error}")))
}

/// Python's `findall` semantics: the whole match when the pattern has no capture groups, and
/// every capture group of every match (flattened, in order) when it has some. Groups that did
/// not participate come back as empty strings, as they do in Python.
fn find_all(pattern: &Regex, haystack: &str, replace_entities: bool) -> Vec<Text> {
    let groups = pattern.captures_len().saturating_sub(1);
    let mut results: Vec<Text> = Vec::new();

    if groups == 0 {
        for matched in pattern.find_iter(haystack) {
            results.push(Text::new(matched.as_str()));
        }
    } else {
        for captures in pattern.captures_iter(haystack) {
            for group in 1..=groups {
                let value = captures.get(group).map_or("", |m| m.as_str());
                results.push(Text::new(value));
            }
        }
    }

    if replace_entities {
        for item in &mut results {
            let decoded = item.replace_entities();
            *item = decoded;
        }
    }
    results
}

/// The first element [`find_all`] would have produced, without scanning the rest of the input.
fn find_first(pattern: &Regex, haystack: &str, replace_entities: bool) -> Option<Text> {
    let groups = pattern.captures_len().saturating_sub(1);

    let raw = if groups == 0 {
        pattern.find(haystack).map(|m| m.as_str().to_string())
    } else {
        pattern
            .captures(haystack)
            .map(|captures| captures.get(1).map_or("", |m| m.as_str()).to_string())
    }?;

    Some(if replace_entities {
        Text::new(replace_entities_str(&raw))
    } else {
        Text::new(raw)
    })
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// An owned string with the scraping helpers Scrapling's `TextHandler` adds to `str`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Text(String);

impl Text {
    /// Wrap an existing string.
    pub fn new(value: impl Into<String>) -> Self {
        Text(value.into())
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner `String`.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Collapse `\t\r\n` and runs of spaces into single spaces, then trim.
    pub fn clean(&self) -> Text {
        Text(clean_str(&self.0, false))
    }

    /// Same as [`Text::clean`] but also replaces HTML character entities.
    pub fn clean_with_entities(&self) -> Text {
        Text(clean_str(&self.0, true))
    }

    /// Every match (or capture group) of `pattern` against this text.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        Ok(self.re_compiled(&compiled, opts))
    }

    /// Like [`Text::re`] but with an already-compiled pattern; cannot fail on the pattern.
    ///
    /// [`ReOptions::case_sensitive`] is ignored here — case handling is baked into the compiled
    /// regex, exactly as it is in Python when a compiled pattern is passed in.
    pub fn re_compiled(&self, pattern: &Regex, opts: ReOptions) -> Texts {
        let haystack = self.haystack(opts);
        Texts(find_all(pattern, &haystack, opts.replace_entities))
    }

    /// The text a regex helper should run against: the cleaned text when
    /// [`ReOptions::clean_match`] is set, the raw text otherwise.
    fn haystack(&self, opts: ReOptions) -> Cow<'_, str> {
        if opts.clean_match {
            Cow::Owned(clean_str(&self.0, false))
        } else {
            Cow::Borrowed(&self.0)
        }
    }

    /// The first match of `pattern`, or `None`.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        Ok(self.re_first_compiled(&compiled, opts))
    }

    /// Like [`Text::re_first`] with an already-compiled pattern.
    pub fn re_first_compiled(&self, pattern: &Regex, opts: ReOptions) -> Option<Text> {
        let haystack = self.haystack(opts);
        find_first(pattern, &haystack, opts.replace_entities)
    }

    /// Whether `pattern` matches at all (Python's `check_match=True`).
    pub fn re_matches(&self, pattern: &Regex, opts: ReOptions) -> bool {
        let haystack = self.haystack(opts);
        pattern.is_match(&haystack)
    }

    /// Like [`Text::re_matches`] with a pattern that still has to be compiled.
    pub fn re_matches_str(&self, pattern: &str, opts: ReOptions) -> Result<bool> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        Ok(self.re_matches(&compiled, opts))
    }

    /// Parse this text as JSON into any deserializable type.
    ///
    /// Safe on untrusted bodies: malformed input comes back as [`Error::Serde`] instead of a
    /// panic, and `serde_json` caps nesting depth, so a hostile document cannot blow the stack.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.0).map_err(Error::from)
    }

    /// Parse this text as a free-form `serde_json::Value`.
    pub fn json_value(&self) -> Result<serde_json::Value> {
        self.json()
    }

    /// Replace HTML character entities (`&amp;`, `&#38;`, ...) with their characters.
    pub fn replace_entities(&self) -> Text {
        Text(replace_entities_str(&self.0))
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Text {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<String> for Text {
    fn from(value: String) -> Self {
        Text(value)
    }
}

impl From<&str> for Text {
    fn from(value: &str) -> Self {
        Text(value.to_string())
    }
}

impl From<Text> for String {
    fn from(value: Text) -> Self {
        value.0
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Text {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Text> for str {
    fn eq(&self, other: &Text) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Text> for &str {
    fn eq(&self, other: &Text) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Text> for String {
    fn eq(&self, other: &Text) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Serialize for Text {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer).map(Text)
    }
}

// ---------------------------------------------------------------------------------------------
// Texts
// ---------------------------------------------------------------------------------------------

/// A list of [`Text`] values with the same regex/extraction helpers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Texts(Vec<Text>);

impl Texts {
    /// Build from anything that iterates into [`Text`].
    pub fn new(items: impl IntoIterator<Item = Text>) -> Self {
        Texts(items.into_iter().collect())
    }

    /// Apply [`Text::re`] to every element and flatten the results.
    pub fn re(&self, pattern: &str, opts: ReOptions) -> Result<Texts> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        Ok(self.re_compiled(&compiled, opts))
    }

    /// Like [`Texts::re`] with an already-compiled pattern.
    pub fn re_compiled(&self, pattern: &Regex, opts: ReOptions) -> Texts {
        let mut results: Vec<Text> = Vec::new();
        for item in &self.0 {
            results.extend(item.re_compiled(pattern, opts).0);
        }
        Texts(results)
    }

    /// The first match found across all elements.
    pub fn re_first(&self, pattern: &str, opts: ReOptions) -> Result<Option<Text>> {
        let compiled = build_regex(pattern, opts.case_sensitive)?;
        Ok(self.re_first_compiled(&compiled, opts))
    }

    /// Like [`Texts::re_first`] with an already-compiled pattern.
    pub fn re_first_compiled(&self, pattern: &Regex, opts: ReOptions) -> Option<Text> {
        self.0
            .iter()
            .find_map(|item| item.re_first_compiled(pattern, opts))
    }

    /// The element at `index`, or `None`.
    pub fn get(&self, index: usize) -> Option<&Text> {
        self.0.get(index)
    }

    /// All elements as a plain `Vec<String>` (Scrapy's `getall()`).
    pub fn getall(&self) -> Vec<String> {
        self.0.iter().map(|item| item.0.clone()).collect()
    }

    /// The first element, or `None`.
    pub fn first(&self) -> Option<&Text> {
        self.0.first()
    }

    /// The last element, or `None`.
    pub fn last(&self) -> Option<&Text> {
        self.0.last()
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
    pub fn iter(&self) -> std::slice::Iter<'_, Text> {
        self.0.iter()
    }

    /// Consume into the backing `Vec`.
    pub fn into_vec(self) -> Vec<Text> {
        self.0
    }
}

impl Deref for Texts {
    type Target = [Text];

    fn deref(&self) -> &[Text] {
        &self.0
    }
}

impl IntoIterator for Texts {
    type Item = Text;
    type IntoIter = std::vec::IntoIter<Text>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Texts {
    type Item = &'a Text;
    type IntoIter = std::slice::Iter<'a, Text>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<Text> for Texts {
    fn from_iter<I: IntoIterator<Item = Text>>(iter: I) -> Self {
        Texts(iter.into_iter().collect())
    }
}

impl Extend<Text> for Texts {
    fn extend<I: IntoIterator<Item = Text>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl Index<usize> for Texts {
    type Output = Text;

    fn index(&self, index: usize) -> &Text {
        &self.0[index]
    }
}

impl From<Vec<Text>> for Texts {
    fn from(items: Vec<Text>) -> Self {
        Texts(items)
    }
}

impl From<Texts> for Vec<Text> {
    fn from(items: Texts) -> Self {
        items.0
    }
}

impl Serialize for Texts {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Texts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Vec::<Text>::deserialize(deserializer).map(Texts)
    }
}

// ---------------------------------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------------------------------

/// Give a run of name/value pairs Python `dict` semantics: a repeated name keeps the position
/// of its first occurrence and takes the value of its last one.
///
/// Python backs `AttributesHandler` with a real mapping, so duplicates cannot survive there.
/// A plain `Vec` would keep both, which makes `len`, `get` and — worse — `json_string` disagree
/// with the original (a JSON object with the same key twice).
fn dedup_pairs(pairs: Vec<(String, Text)>) -> Vec<(String, Text)> {
    // An element has a handful of attributes, so the common case dedupes with a linear scan and
    // no extra allocation. Anything bigger can only come from deserialized (that is, remote)
    // data, and gets an index so the work stays linear instead of quadratic.
    const LINEAR_SCAN_LIMIT: usize = 16;

    if pairs.len() < 2 {
        return pairs;
    }

    let mut out: Vec<(String, Text)> = Vec::with_capacity(pairs.len());

    if pairs.len() <= LINEAR_SCAN_LIMIT {
        for (key, value) in pairs {
            // The lookup is resolved into an owned index first so the arm below can push.
            let seen = out.iter().position(|(existing, _)| existing == &key);
            match seen {
                Some(position) => out[position].1 = value,
                None => out.push((key, value)),
            }
        }
        return out;
    }

    let mut index: HashMap<String, usize> = HashMap::with_capacity(pairs.len());
    for (key, value) in pairs {
        let seen = index.get(&key).copied();
        match seen {
            Some(position) => out[position].1 = value,
            None => {
                index.insert(key.clone(), out.len());
                out.push((key, value));
            }
        }
    }
    out
}

/// An element's attributes: an insertion-ordered, case-sensitive map of name -> value.
///
/// Names are unique, as they are in the Python original: repeating one keeps the position of
/// the first occurrence and the value of the last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attributes(Vec<(String, Text)>);

impl Attributes {
    /// Build from name/value pairs, keeping the given order.
    pub fn new(pairs: impl IntoIterator<Item = (String, Text)>) -> Self {
        Attributes(dedup_pairs(pairs.into_iter().collect()))
    }

    /// The value of `name`, or `None`.
    pub fn get(&self, name: &str) -> Option<&Text> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// Whether `name` is present.
    pub fn contains_key(&self, name: &str) -> bool {
        self.0.iter().any(|(key, _)| key == name)
    }

    /// Attributes whose value equals `keyword`, or contains it when `partial` is true.
    pub fn search_values(&self, keyword: &str, partial: bool) -> Attributes {
        Attributes(
            self.0
                .iter()
                .filter(|(_, value)| {
                    if partial {
                        value.as_str().contains(keyword)
                    } else {
                        value.as_str() == keyword
                    }
                })
                .cloned()
                .collect(),
        )
    }

    /// The attributes as a JSON object string (Python's `json_string`).
    pub fn json_string(&self) -> Result<String> {
        serde_json::to_string(self).map_err(Error::from)
    }

    /// Iterate over `(name, value)` in document order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Text)> + '_ {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// Number of attributes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no attributes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Attribute names in document order.
    pub fn keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.0.iter().map(|(key, _)| key.as_str())
    }

    /// Attribute values in document order.
    pub fn values(&self) -> impl Iterator<Item = &Text> + '_ {
        self.0.iter().map(|(_, value)| value)
    }
}

impl Index<&str> for Attributes {
    type Output = Text;

    /// # Panics
    ///
    /// Panics when `name` is not present, like indexing a `HashMap`. Use
    /// [`Attributes::get`] for anything driven by untrusted input.
    fn index(&self, name: &str) -> &Text {
        match self.get(name) {
            Some(value) => value,
            None => panic!("no attribute named `{name}`"),
        }
    }
}

/// Borrowing iterator over an [`Attributes`] map, yielding `(name, value)` in document order.
#[derive(Debug, Clone)]
pub struct AttributesIter<'a> {
    inner: std::slice::Iter<'a, (String, Text)>,
}

impl<'a> Iterator for AttributesIter<'a> {
    type Item = (&'a str, &'a Text);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key.as_str(), value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for AttributesIter<'_> {}

impl<'a> IntoIterator for &'a Attributes {
    type Item = (&'a str, &'a Text);
    type IntoIter = AttributesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        AttributesIter {
            inner: self.0.iter(),
        }
    }
}

impl FromIterator<(String, Text)> for Attributes {
    fn from_iter<I: IntoIterator<Item = (String, Text)>>(iter: I) -> Self {
        Attributes::new(iter)
    }
}

impl Serialize for Attributes {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Attributes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct AttributesVisitor;

        impl<'de> Visitor<'de> for AttributesVisitor {
            type Value = Attributes;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of attribute names to attribute values")
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut access: M,
            ) -> std::result::Result<Attributes, M::Error> {
                // The hint comes from the input, so cap it rather than trusting it.
                let mut items: Vec<(String, Text)> =
                    Vec::with_capacity(access.size_hint().unwrap_or(0).min(64));
                while let Some((key, value)) = access.next_entry::<String, Text>()? {
                    items.push((key, value));
                }
                // A JSON object may repeat a key; collapse it the way Python's `dict` would.
                Ok(Attributes(dedup_pairs(items)))
            }
        }

        deserializer.deserialize_map(AttributesVisitor)
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- entities ------------------------------------------------------------------------------

    #[test]
    fn entity_table_is_sorted_and_complete() {
        assert_eq!(NAMED_ENTITIES.len(), 252);
        assert!(NAMED_ENTITIES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(named_entity("amp"), Some(38));
        assert_eq!(named_entity("nbsp"), Some(160));
        assert_eq!(named_entity("Yuml"), Some(376));
    }

    #[test]
    fn named_entity_falls_back_to_lowercase() {
        assert_eq!(named_entity("AMP"), Some(38));
        assert_eq!(named_entity("Amp"), Some(38));
        // `Aacute` and `aacute` are distinct entries and must not collapse.
        assert_eq!(named_entity("Aacute"), Some(193));
        assert_eq!(named_entity("aacute"), Some(225));
        assert_eq!(named_entity("definitely-not-an-entity"), None);
    }

    #[test]
    fn replaces_named_entities() {
        let text = Text::new("Tom &amp; Jerry &lt;3 &quot;hi&quot;");
        assert_eq!(text.replace_entities(), "Tom & Jerry <3 \"hi\"");
    }

    #[test]
    fn replaces_numeric_and_hex_entities() {
        assert_eq!(Text::new("&#38;").replace_entities(), "&");
        assert_eq!(Text::new("&#x26;").replace_entities(), "&");
        assert_eq!(Text::new("&#X26;").replace_entities(), "&");
        assert_eq!(Text::new("&#8364;").replace_entities(), "\u{20AC}");
    }

    #[test]
    fn replaces_entities_without_a_semicolon() {
        assert_eq!(Text::new("a &amp b").replace_entities(), "a & b");
        assert_eq!(Text::new("&#38x").replace_entities(), "&x");
    }

    #[test]
    fn numeric_c1_range_uses_cp1252() {
        // 0x92 is a right single quote in cp1252, not a C1 control character.
        assert_eq!(Text::new("&#146;").replace_entities(), "\u{2019}");
        assert_eq!(Text::new("&#x80;").replace_entities(), "\u{20AC}");
        // 0x81 is undefined in cp1252, so the illegal reference is dropped.
        assert_eq!(Text::new("&#129;").replace_entities(), "");
    }

    #[test]
    fn illegal_entities_follow_w3lib_rules() {
        // Unknown but terminated -> removed.
        assert_eq!(Text::new("a &nosuch; b").replace_entities(), "a  b");
        // Unknown and unterminated -> kept verbatim.
        assert_eq!(Text::new("a &nosuch b").replace_entities(), "a &nosuch b");
        // Out of range -> removed (semicolon present).
        assert_eq!(Text::new("&#1114112;").replace_entities(), "");
        // A bare ampersand is left alone.
        assert_eq!(Text::new("a & b").replace_entities(), "a & b");
        assert_eq!(Text::new("R&D").replace_entities(), "R&D");
    }

    #[test]
    fn oversized_numeric_entities_are_dropped_rather_than_panicking() {
        // A number too large for a code point is an illegal reference; Python's w3lib raises
        // `OverflowError` on the very large ones, which is not a useful answer for a scraper.
        assert_eq!(Text::new("&#99999999999999999999;").replace_entities(), "");
        assert_eq!(Text::new("&#4000000000;").replace_entities(), "");
        assert_eq!(Text::new("&#xFFFFFFFFFF;").replace_entities(), "");
        // Without the semicolon an illegal reference is kept verbatim, as w3lib does.
        assert_eq!(Text::new("&#4000000000").replace_entities(), "&#4000000000");
    }

    #[test]
    fn surrogate_references_are_dropped() {
        // Python keeps a lone surrogate; a Rust `char` cannot hold one, so it is illegal here.
        assert_eq!(Text::new("&#xD800;").replace_entities(), "");
        assert_eq!(Text::new("&#55296;").replace_entities(), "");
    }

    #[test]
    fn replace_entities_preserves_multibyte_text() {
        let text = Text::new("caf\u{e9} &amp; cr\u{e8}me \u{1f600}");
        assert_eq!(text.replace_entities(), "caf\u{e9} & cr\u{e8}me \u{1f600}");
    }

    // -- clean ---------------------------------------------------------------------------------

    #[test]
    fn clean_collapses_whitespace() {
        let text = Text::new("  hello\t\r\n   world  ");
        assert_eq!(text.clean(), "hello world");
    }

    #[test]
    fn clean_leaves_entities_alone() {
        let text = Text::new("  a &amp;\n b ");
        assert_eq!(text.clean(), "a &amp; b");
    }

    #[test]
    fn clean_with_entities_decodes_them() {
        let text = Text::new("  a &amp;\n b ");
        assert_eq!(text.clean_with_entities(), "a & b");
    }

    #[test]
    fn clean_strips_every_character_python_calls_whitespace() {
        // `str.strip()` also removes the information separators and the non-breaking space.
        assert_eq!(Text::new("\u{1c}\u{1f} hi \u{1e}").clean(), "hi");
        assert_eq!(Text::new("\u{a0}hi\u{a0}").clean(), "hi");
        // ... but only at the ends: the inner ones survive, like in Python.
        assert_eq!(Text::new(" a\u{1c}b ").clean(), "a\u{1c}b");
    }

    #[test]
    fn clean_of_empty_and_whitespace_only() {
        assert_eq!(Text::default().clean(), "");
        assert_eq!(Text::new(" \t\r\n ").clean(), "");
    }

    // -- re ------------------------------------------------------------------------------------

    #[test]
    fn re_without_groups_returns_whole_matches() {
        let text = Text::new("a1 b22 c333");
        let found = text.re(r"\d+", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["1", "22", "333"]);
    }

    #[test]
    fn re_with_one_group_returns_that_group() {
        let text = Text::new("price: 10, price: 20");
        let found = text.re(r"price: (\d+)", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["10", "20"]);
    }

    #[test]
    fn re_with_several_groups_flattens_them() {
        let text = Text::new("a=1 b=2");
        let found = text.re(r"(\w)=(\d)", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["a", "1", "b", "2"]);
    }

    #[test]
    fn re_reports_empty_string_for_a_group_that_did_not_participate() {
        let text = Text::new("x");
        let found = text.re(r"(y)?(x)", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["", "x"]);
    }

    #[test]
    fn re_ignores_non_capturing_groups() {
        let text = Text::new("ab ab");
        let found = text.re(r"(?:a)(b)", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["b", "b"]);
    }

    #[test]
    fn re_honours_case_sensitivity() {
        let text = Text::new("Hello HELLO hello");
        assert_eq!(text.re("hello", ReOptions::new()).unwrap().len(), 1);
        assert_eq!(
            text.re("hello", ReOptions::new().case_sensitive(false))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn re_honours_clean_match() {
        let text = Text::new("hello\n   world");
        assert!(text.re("hello world", ReOptions::new()).unwrap().is_empty());
        let found = text
            .re("hello world", ReOptions::new().clean_match(true))
            .unwrap();
        assert_eq!(found.getall(), vec!["hello world"]);
    }

    #[test]
    fn re_replaces_entities_in_results_by_default() {
        let text = Text::new("<b>Tom &amp; Jerry</b>");
        let found = text.re(r"<b>(.*?)</b>", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["Tom & Jerry"]);

        let raw = text
            .re(r"<b>(.*?)</b>", ReOptions::new().replace_entities(false))
            .unwrap();
        assert_eq!(raw.getall(), vec!["Tom &amp; Jerry"]);
    }

    #[test]
    fn re_rejects_an_invalid_pattern() {
        let error = Text::new("x").re("(", ReOptions::new()).unwrap_err();
        assert!(error.to_string().contains("invalid regex"));
        assert!(Text::new("x").re_first("(", ReOptions::new()).is_err());
        assert!(Text::new("x")
            .re_matches_str("(", ReOptions::new())
            .is_err());
    }

    #[test]
    fn re_compiled_uses_the_pattern_as_given() {
        let pattern = Regex::new("(?i)hello").unwrap();
        let text = Text::new("Hello hello");
        assert_eq!(text.re_compiled(&pattern, ReOptions::new()).len(), 2);
    }

    #[test]
    fn re_first_returns_the_first_result() {
        let text = Text::new("a=1 b=2");
        let first = text.re_first(r"(\w)=(\d)", ReOptions::new()).unwrap();
        assert_eq!(first, Some(Text::new("a")));
    }

    #[test]
    fn re_first_returns_none_without_a_match() {
        let text = Text::new("nothing here");
        assert_eq!(text.re_first(r"\d+", ReOptions::new()).unwrap(), None);
    }

    #[test]
    fn re_first_compiled_matches_re_first() {
        let pattern = Regex::new(r"\d+").unwrap();
        let text = Text::new("a 12 b 34");
        assert_eq!(
            text.re_first_compiled(&pattern, ReOptions::new()),
            Some(Text::new("12"))
        );
    }

    #[test]
    fn re_matches_reports_presence_only() {
        let pattern = Regex::new(r"\d+").unwrap();
        assert!(Text::new("abc 1").re_matches(&pattern, ReOptions::new()));
        assert!(!Text::new("abc").re_matches(&pattern, ReOptions::new()));

        let spaced = Regex::new("a b").unwrap();
        let text = Text::new("a\n\nb");
        assert!(!text.re_matches(&spaced, ReOptions::new()));
        assert!(text.re_matches(&spaced, ReOptions::new().clean_match(true)));
    }

    #[test]
    fn re_matches_str_compiles_the_pattern() {
        let text = Text::new("Hello");
        assert!(!text.re_matches_str("hello", ReOptions::new()).unwrap());
        assert!(text
            .re_matches_str("hello", ReOptions::new().case_sensitive(false))
            .unwrap());
    }

    // -- json ----------------------------------------------------------------------------------

    #[test]
    fn json_parses_into_a_typed_value() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Point {
            x: i32,
            y: i32,
        }
        let text = Text::new(r#"{"x": 1, "y": 2}"#);
        assert_eq!(text.json::<Point>().unwrap(), Point { x: 1, y: 2 });
    }

    #[test]
    fn json_value_parses_free_form_json() {
        let text = Text::new(r#"{"items": [1, 2, 3]}"#);
        let value = text.json_value().unwrap();
        assert_eq!(value["items"][2], serde_json::json!(3));
    }

    #[test]
    fn json_reports_an_error_instead_of_panicking() {
        let text = Text::new("<html>not json</html>");
        assert!(text.json_value().is_err());
    }

    // -- trait impls ---------------------------------------------------------------------------

    #[test]
    fn text_behaves_like_a_string() {
        let text = Text::new("Hello");
        assert_eq!(text.as_str(), "Hello");
        assert_eq!(text.len(), 5); // through Deref<Target = str>
        assert!(text.starts_with("Hel"));
        assert_eq!(text.to_uppercase(), "HELLO");
        assert_eq!(text.to_string(), "Hello");
        assert_eq!(AsRef::<str>::as_ref(&text), "Hello");
        assert_eq!(text.clone().into_string(), "Hello".to_string());
        assert_eq!(String::from(text.clone()), "Hello".to_string());
        assert_eq!(text, "Hello");
        assert_eq!(text, *"Hello");
        assert_eq!(text, "Hello".to_string());
        assert_eq!(Text::from("Hello"), Text::from("Hello".to_string()));
    }

    #[test]
    fn text_can_key_a_map_and_be_looked_up_by_str() {
        use std::collections::HashMap;
        let mut map: HashMap<Text, i32> = HashMap::new();
        map.insert(Text::new("k"), 7);
        assert_eq!(map.get("k"), Some(&7)); // needs Borrow<str>
    }

    #[test]
    fn text_orders_like_a_string() {
        let mut items = vec![Text::new("b"), Text::new("a"), Text::new("c")];
        items.sort();
        assert_eq!(items, vec![Text::new("a"), Text::new("b"), Text::new("c")]);
    }

    #[test]
    fn text_serde_roundtrip() {
        let text = Text::new("hi \"there\"");
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(json, "\"hi \\\"there\\\"\"");
        assert_eq!(serde_json::from_str::<Text>(&json).unwrap(), text);
    }

    // -- Texts ---------------------------------------------------------------------------------

    fn sample_texts() -> Texts {
        Texts::new([Text::new("a1"), Text::new("b2"), Text::new("c3")])
    }

    #[test]
    fn texts_accessors() {
        let texts = sample_texts();
        assert_eq!(texts.len(), 3);
        assert!(!texts.is_empty());
        assert_eq!(texts.get(1), Some(&Text::new("b2")));
        assert_eq!(texts.get(9), None);
        assert_eq!(texts.first(), Some(&Text::new("a1")));
        assert_eq!(texts.last(), Some(&Text::new("c3")));
        assert_eq!(texts[0], Text::new("a1"));
        assert_eq!(texts.getall(), vec!["a1", "b2", "c3"]);
        assert_eq!(texts.iter().count(), 3);
        assert_eq!(texts.clone().into_vec().len(), 3);
        assert!(Texts::default().is_empty());
    }

    #[test]
    fn texts_iteration_and_conversion() {
        let texts = sample_texts();
        let borrowed: Vec<&Text> = (&texts).into_iter().collect();
        assert_eq!(borrowed.len(), 3);
        let owned: Vec<Text> = texts.clone().into_iter().collect();
        assert_eq!(owned, texts.clone().into_vec());

        let collected: Texts = owned.iter().cloned().collect();
        assert_eq!(collected, texts);
        assert_eq!(Texts::from(owned.clone()), texts);
        assert_eq!(Vec::<Text>::from(texts.clone()), owned);

        // Deref to a slice.
        assert_eq!(
            texts.split_first().map(|(head, _)| head.clone()),
            Some(Text::new("a1"))
        );
    }

    #[test]
    fn texts_extend() {
        let mut texts = Texts::default();
        texts.extend([Text::new("x")]);
        assert_eq!(texts.getall(), vec!["x"]);
    }

    #[test]
    fn texts_re_flattens_across_elements() {
        let texts = sample_texts();
        let found = texts.re(r"\d", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["1", "2", "3"]);
    }

    #[test]
    fn texts_re_with_groups() {
        let texts = Texts::new([Text::new("a=1"), Text::new("b=2")]);
        let found = texts.re(r"(\w)=(\d)", ReOptions::new()).unwrap();
        assert_eq!(found.getall(), vec!["a", "1", "b", "2"]);
    }

    #[test]
    fn texts_re_first_skips_non_matching_elements() {
        let texts = Texts::new([Text::new("none"), Text::new("here 42"), Text::new("99")]);
        assert_eq!(
            texts.re_first(r"\d+", ReOptions::new()).unwrap(),
            Some(Text::new("42"))
        );
        assert_eq!(
            Texts::new([Text::new("none")])
                .re_first(r"\d+", ReOptions::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn texts_re_compiled_variants() {
        let pattern = Regex::new(r"\d").unwrap();
        let texts = sample_texts();
        assert_eq!(texts.re_compiled(&pattern, ReOptions::new()).len(), 3);
        assert_eq!(
            texts.re_first_compiled(&pattern, ReOptions::new()),
            Some(Text::new("1"))
        );
    }

    #[test]
    fn texts_re_propagates_pattern_errors() {
        assert!(sample_texts().re("(", ReOptions::new()).is_err());
        assert!(sample_texts().re_first("(", ReOptions::new()).is_err());
    }

    #[test]
    fn texts_serde_roundtrip() {
        let texts = sample_texts();
        let json = serde_json::to_string(&texts).unwrap();
        assert_eq!(json, r#"["a1","b2","c3"]"#);
        assert_eq!(serde_json::from_str::<Texts>(&json).unwrap(), texts);
    }

    // -- Attributes ----------------------------------------------------------------------------

    fn sample_attributes() -> Attributes {
        Attributes::new([
            ("class".to_string(), Text::new("product")),
            ("id".to_string(), Text::new("p1")),
            ("data-role".to_string(), Text::new("product-card")),
        ])
    }

    #[test]
    fn attributes_accessors() {
        let attributes = sample_attributes();
        assert_eq!(attributes.len(), 3);
        assert!(!attributes.is_empty());
        assert_eq!(attributes.get("id"), Some(&Text::new("p1")));
        assert_eq!(attributes.get("missing"), None);
        assert!(attributes.contains_key("class"));
        assert!(!attributes.contains_key("Class")); // case-sensitive, like lxml
        assert_eq!(attributes["class"], Text::new("product"));
        assert!(Attributes::default().is_empty());
    }

    #[test]
    fn attributes_keep_insertion_order() {
        let attributes = sample_attributes();
        assert_eq!(
            attributes.keys().collect::<Vec<_>>(),
            vec!["class", "id", "data-role"]
        );
        assert_eq!(
            attributes.values().map(|v| v.as_str()).collect::<Vec<_>>(),
            vec!["product", "p1", "product-card"]
        );
        assert_eq!(
            attributes.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            vec!["class", "id", "data-role"]
        );
        let borrowed: Vec<(&str, &Text)> = (&attributes).into_iter().collect();
        assert_eq!(borrowed[2].0, "data-role");
        assert_eq!((&attributes).into_iter().len(), 3);
    }

    #[test]
    fn attributes_search_values_exact_and_partial() {
        let attributes = sample_attributes();

        let exact = attributes.search_values("product", false);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact.keys().collect::<Vec<_>>(), vec!["class"]);

        let partial = attributes.search_values("product", true);
        assert_eq!(
            partial.keys().collect::<Vec<_>>(),
            vec!["class", "data-role"]
        );

        assert!(attributes.search_values("nope", true).is_empty());
    }

    #[test]
    fn attributes_json_string_preserves_order() {
        let json = sample_attributes().json_string().unwrap();
        assert_eq!(
            json,
            r#"{"class":"product","id":"p1","data-role":"product-card"}"#
        );
        assert_eq!(Attributes::default().json_string().unwrap(), "{}");
    }

    #[test]
    fn attributes_serde_roundtrip_preserves_order() {
        let attributes = sample_attributes();
        let json = serde_json::to_string(&attributes).unwrap();
        let back: Attributes = serde_json::from_str(&json).unwrap();
        assert_eq!(back, attributes);
        assert_eq!(
            back.keys().collect::<Vec<_>>(),
            vec!["class", "id", "data-role"]
        );
    }

    #[test]
    fn attributes_collapse_a_repeated_name_like_a_dict() {
        let attributes = Attributes::new([
            ("class".to_string(), Text::new("a")),
            ("id".to_string(), Text::new("x")),
            ("class".to_string(), Text::new("b")),
        ]);
        assert_eq!(attributes.len(), 2);
        assert_eq!(attributes.get("class"), Some(&Text::new("b")));
        assert_eq!(attributes.keys().collect::<Vec<_>>(), vec!["class", "id"]);
        assert_eq!(
            attributes.json_string().unwrap(),
            r#"{"class":"b","id":"x"}"#
        );
    }

    #[test]
    fn attributes_collapse_a_repeated_name_above_the_linear_scan_limit() {
        // Past the threshold the indexed path takes over; it has to agree with the scan.
        let mut pairs: Vec<(String, Text)> = (0..20)
            .map(|n| (format!("a{n}"), Text::new(n.to_string())))
            .collect();
        pairs.push(("a3".to_string(), Text::new("last")));

        let attributes = Attributes::new(pairs);
        assert_eq!(attributes.len(), 20);
        assert_eq!(attributes.get("a3"), Some(&Text::new("last")));
        assert_eq!(attributes.keys().next(), Some("a0"));
    }

    #[test]
    fn attributes_deserialize_collapses_repeated_keys() {
        let attributes: Attributes = serde_json::from_str(r#"{"a":"1","a":"2"}"#).unwrap();
        assert_eq!(attributes.len(), 1);
        assert_eq!(attributes.get("a"), Some(&Text::new("2")));
    }

    #[test]
    fn attributes_from_iterator() {
        let attributes: Attributes = vec![("a".to_string(), Text::new("1"))]
            .into_iter()
            .collect();
        assert_eq!(attributes.get("a"), Some(&Text::new("1")));
    }

    #[test]
    #[should_panic(expected = "no attribute named")]
    fn indexing_a_missing_attribute_panics() {
        let _ = &sample_attributes()["missing"];
    }
}
