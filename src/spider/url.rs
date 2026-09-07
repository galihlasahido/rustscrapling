//! URL canonicalization, a port of `w3lib.url.canonicalize_url` / `safe_url_string`.

use percent_encoding::{percent_decode, percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// RFC 3986 "reserved + unreserved", plus `|` and `%`, the set `w3lib` keeps unescaped.
const SAFE_CHARS: &AsciiSet = &NON_ALPHANUMERIC
    // unreserved
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    // gen-delims
    .remove(b':')
    .remove(b'/')
    .remove(b'?')
    .remove(b'#')
    .remove(b'[')
    .remove(b']')
    .remove(b'@')
    // sub-delims
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    // w3lib extras
    .remove(b'|')
    .remove(b'%');

/// The same set as [`SAFE_CHARS`] minus `#`, used for the path component.
const PATH_SAFE_CHARS: &AsciiSet = &SAFE_CHARS.add(b'#');

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Python's `urllib.parse.quote_plus` with the default (empty) safe set.
fn quote_plus(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(HEX_UPPER[(byte >> 4) as usize] as char);
                out.push(HEX_UPPER[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Python's `urllib.parse.unquote_plus`, returning raw bytes so no decoding can fail.
fn unquote_plus(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high << 4) | low);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    out
}

/// `w3lib.url._unquotepath`: decode the percent escapes of a path, but leave `%2F` and `%3F`
/// encoded — decoding them would turn a literal slash or question mark inside a segment into
/// a structural one and point the URL somewhere else.
fn unquote_path(path: &str) -> Vec<u8> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Some((high, low)) = hex_pair(bytes[index + 1], bytes[index + 2]) {
                let byte = (high << 4) | low;
                if byte == b'/' || byte == b'?' {
                    // Keep the escape as the three literal characters `%`, `2`, `F`: the
                    // re-encoding step below leaves `%` alone, so the escape survives.
                    out.push(b'%');
                    out.push(HEX_UPPER[high as usize]);
                    out.push(HEX_UPPER[low as usize]);
                } else {
                    out.push(byte);
                }
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

/// Both hex digits of a percent escape, when they really are hex digits.
fn hex_pair(high: u8, low: u8) -> Option<(u8, u8)> {
    Some((hex_value(high)?, hex_value(low)?))
}

/// `parse_qsl_to_bytes(query, keep_blank_values=True)` followed by a sort and `urlencode`.
fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let (name, value) = match field.find('=') {
            Some(position) => (&field[..position], &field[position + 1..]),
            None => (field, ""),
        };
        pairs.push((unquote_plus(name), unquote_plus(value)));
    }
    pairs.sort();

    let mut out = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&quote_plus(name));
        out.push('=');
        out.push_str(&quote_plus(value));
    }
    out
}

/// Normalize a URL for deduplication: sort the query parameters, normalize the percent
/// encoding and the path, and drop the fragment unless `keep_fragments`. Port of w3lib's
/// `canonicalize_url`.
///
/// The scheme and host are lowercased, a default port for the scheme is dropped, an empty
/// path becomes `/`, and percent escapes are rewritten in upper case. A URL that cannot be
/// parsed at all is returned unchanged, so this never panics and never loses data.
pub fn canonicalize_url(url: &str, keep_fragments: bool) -> String {
    let parsed = match ::url::Url::parse(url.trim()) {
        Ok(parsed) => parsed,
        Err(_) => return url.to_string(),
    };

    if parsed.cannot_be_a_base() {
        let mut out = parsed.clone();
        if !keep_fragments {
            out.set_fragment(None);
        }
        return out.to_string();
    }

    let mut authority = String::new();
    if !parsed.username().is_empty() || parsed.password().is_some() {
        authority.push_str(parsed.username());
        if let Some(password) = parsed.password() {
            authority.push(':');
            authority.push_str(password);
        }
        authority.push('@');
    }
    if let Some(host) = parsed.host_str() {
        authority.push_str(host);
    }
    // `Url::port()` already returns `None` for the scheme's default port.
    if let Some(port) = parsed.port() {
        authority.push(':');
        authority.push_str(&port.to_string());
    }
    let authority = authority.to_lowercase();

    let decoded_path = unquote_path(parsed.path());
    let mut path = percent_encode(&decoded_path, PATH_SAFE_CHARS).to_string();
    if path.is_empty() {
        path.push('/');
    }

    let mut out = String::with_capacity(url.len() + 8);
    out.push_str(&parsed.scheme().to_lowercase());
    out.push_str("://");
    out.push_str(&authority);
    out.push_str(&path);

    let query = parsed.query().map(canonical_query).unwrap_or_default();
    if !query.is_empty() {
        out.push('?');
        out.push_str(&query);
    }

    if keep_fragments {
        if let Some(fragment) = parsed.fragment() {
            let decoded = percent_decode(fragment.as_bytes()).collect::<Vec<u8>>();
            out.push('#');
            out.push_str(&percent_encode(&decoded, SAFE_CHARS).to_string());
        }
    }

    out
}

/// A port of `w3lib.url.safe_url_string`: percent-encode whatever a URL still needs encoded,
/// returning `None` when the value is not a URL at all.
pub(crate) fn safe_url_string(url: &str) -> Option<String> {
    ::url::Url::parse(url.trim())
        .ok()
        .map(|parsed| parsed.to_string())
}

/// Strip the HTML5 whitespace characters from both ends, as `w3lib.html.strip_html5_whitespace`.
pub(crate) fn strip_html5_whitespace(value: &str) -> &str {
    value.trim_matches(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\x0c')
}

/// Every trailing extension of a URL's last path segment, longest first:
/// `archive.tar.gz` yields `tar.gz` and then `gz`, the order `w3lib`'s `_url_extensions`
/// produces them in.
pub(crate) fn url_extensions(url: &str) -> Vec<String> {
    let path = match ::url::Url::parse(url) {
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => url.split(['?', '#']).next().unwrap_or("").to_string(),
    };
    let last = match path.rfind('/') {
        Some(position) => &path[position + 1..],
        None => path.as_str(),
    };
    if !last.contains('.') {
        return Vec::new();
    }
    let lowered = last.to_lowercase();
    let parts: Vec<&str> = lowered.split('.').collect();
    let mut out = Vec::new();
    for index in 1..parts.len() {
        if parts[index].is_empty() {
            continue;
        }
        out.push(parts[index..].join("."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_query_parameters() {
        assert_eq!(
            canonicalize_url("http://www.example.com/do?c=3&b=5&b=2&a=50", false),
            "http://www.example.com/do?a=50&b=2&b=5&c=3"
        );
    }

    #[test]
    fn encodes_spaces_as_plus_and_sorts() {
        assert_eq!(
            canonicalize_url("http://www.example.com/do?q=a space&a=1", false),
            "http://www.example.com/do?a=1&q=a+space"
        );
    }

    #[test]
    fn lowercases_scheme_and_host_and_adds_root_path() {
        assert_eq!(
            canonicalize_url("HTTP://www.Example.COM", false),
            "http://www.example.com/"
        );
    }

    #[test]
    fn drops_default_ports() {
        assert_eq!(
            canonicalize_url("http://www.example.com:80/a", false),
            "http://www.example.com/a"
        );
        assert_eq!(
            canonicalize_url("https://www.example.com:443/a", false),
            "https://www.example.com/a"
        );
        assert_eq!(
            canonicalize_url("http://www.example.com:8080/a", false),
            "http://www.example.com:8080/a"
        );
    }

    #[test]
    fn drops_the_fragment_unless_asked_to_keep_it() {
        assert_eq!(
            canonicalize_url("http://www.example.com/do#frag", false),
            "http://www.example.com/do"
        );
        assert_eq!(
            canonicalize_url("http://www.example.com/do#frag", true),
            "http://www.example.com/do#frag"
        );
    }

    #[test]
    fn normalizes_percent_escapes_to_upper_case() {
        assert_eq!(
            canonicalize_url("http://www.example.com/a%a3do", false),
            "http://www.example.com/a%A3do"
        );
    }

    #[test]
    fn keeps_an_encoded_slash_encoded() {
        // Decoding `%2F` here would move the URL to another path.
        assert_eq!(
            canonicalize_url("http://www.example.com/a%2fb/c", false),
            "http://www.example.com/a%2Fb/c"
        );
        assert_eq!(
            canonicalize_url("http://www.example.com/a%3Fb", false),
            "http://www.example.com/a%3Fb"
        );
        // Everything else is decoded and re-encoded the way w3lib does it.
        assert_eq!(
            canonicalize_url("http://www.example.com/a%20b", false),
            "http://www.example.com/a%20b"
        );
    }

    #[test]
    fn keeps_blank_values() {
        assert_eq!(
            canonicalize_url("http://www.example.com/do?b=&a=2", false),
            "http://www.example.com/do?a=2&b="
        );
    }

    #[test]
    fn leaves_an_unparsable_value_alone() {
        assert_eq!(canonicalize_url("not a url", false), "not a url");
    }

    #[test]
    fn extracts_every_trailing_extension() {
        assert_eq!(
            url_extensions("http://e.com/archive.tar.gz"),
            vec!["tar.gz".to_string(), "gz".to_string()]
        );
        assert!(url_extensions("http://e.com/page").is_empty());
        assert_eq!(
            url_extensions("http://e.com/a/IMAGE.PNG?x=1"),
            vec!["png".to_string()]
        );
    }

    #[test]
    fn strips_html5_whitespace_only() {
        assert_eq!(strip_html5_whitespace(" \t\n/a/b \r"), "/a/b");
    }
}
