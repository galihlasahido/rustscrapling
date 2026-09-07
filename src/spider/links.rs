//! Pulling URLs out of a page and filtering them, a port of `scrapling/spiders/links.py`.

use std::collections::HashSet;

use regex::Regex;

use crate::error::{Error, Result};
use crate::parser::Selector;
use crate::response::Response;

use super::url::{canonicalize_url, safe_url_string, strip_html5_whitespace, url_extensions};

/// The only URL schemes a link extractor will hand back.
///
/// Scrapy's list also carries `file`, but the URLs here come out of remote markup and are handed
/// straight to a session: a `file://` link scraped off a hostile page would point the HTTP
/// fetcher (which refuses the scheme outright) or, worse, a browser session at the local
/// filesystem. Nothing in a crawl needs `<a href="file:///etc/passwd">` to survive extraction.
const VALID_SCHEMES: &[&str] = &["http", "https"];

/// How many links a single page may contribute. A hostile or generated page can carry
/// millions of anchors; past this many the rest are dropped with a warning rather than
/// grown into an unbounded queue.
const MAX_LINKS_PER_RESPONSE: usize = 100_000;

/// The longest attribute value still treated as a URL. Browsers stop well before this and
/// no server serves a path this long, so anything larger is a `data:` blob or junk.
const MAX_URL_BYTES: usize = 8 * 1024;

/// The file extensions dropped by default, as Scrapy's `IGNORED_EXTENSIONS`.
pub const IGNORED_EXTENSIONS: &[&str] = &[
    // archives
    "7z", "7zip", "bz2", "rar", "tar", "tar.gz", "xz", "zip", // images
    "mng", "pct", "bmp", "gif", "jpg", "jpeg", "png", "pst", "psp", "tif", "tiff", "ai", "drw",
    "dxf", "eps", "ps", "svg", "cdr", "ico", "webp", // audio
    "mp3", "wma", "ogg", "wav", "ra", "aac", "mid", "au", "aiff", // video
    "3gp", "asf", "asx", "avi", "mov", "mp4", "mpg", "qt", "rm", "swf", "wmv", "m4a", "m4v", "flv",
    "webm", // office suites
    "xls", "xlsm", "xlsx", "xltm", "xltx", "potm", "potx", "ppt", "pptm", "pptx", "pps", "doc",
    "docb", "docm", "docx", "dotm", "dotx", "odt", "ods", "odg", "odp", // other
    "css", "pdf", "exe", "bin", "rss", "dmg", "iso", "apk", "jar", "sh", "rb", "js", "hta", "bat",
    "cpl", "msi", "msp", "py",
];

/// Pulls URLs out of a response and filters them.
#[derive(Debug, Clone)]
pub struct LinkExtractor {
    allow: Vec<Regex>,
    deny: Vec<Regex>,
    allow_domains: Vec<String>,
    deny_domains: Vec<String>,
    restrict_css: Vec<String>,
    tags: Vec<String>,
    attrs: Vec<String>,
    canonicalize: bool,
    strip: bool,
    keep_fragment: bool,
    deny_extensions: Vec<String>,
}

impl Default for LinkExtractor {
    fn default() -> Self {
        LinkExtractor::new()
    }
}

impl LinkExtractor {
    /// An extractor that keeps every `<a href>` and `<area href>`.
    pub fn new() -> LinkExtractor {
        LinkExtractor {
            allow: Vec::new(),
            deny: Vec::new(),
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            restrict_css: Vec::new(),
            tags: vec!["a".to_string(), "area".to_string()],
            attrs: vec!["href".to_string()],
            canonicalize: true,
            strip: true,
            keep_fragment: false,
            deny_extensions: IGNORED_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
        }
    }

    /// Keep only URLs matching at least one of these regexes.
    pub fn allow(mut self, patterns: &[&str]) -> Result<Self> {
        self.allow = compile(patterns)?;
        Ok(self)
    }

    /// Drop URLs matching any of these regexes; takes precedence over `allow`.
    pub fn deny(mut self, patterns: &[&str]) -> Result<Self> {
        self.deny = compile(patterns)?;
        Ok(self)
    }

    /// Keep only these hosts and their subdomains.
    pub fn allow_domains(mut self, domains: &[&str]) -> Self {
        self.allow_domains = domains.iter().map(|d| d.to_lowercase()).collect();
        self
    }

    /// Drop these hosts and their subdomains.
    pub fn deny_domains(mut self, domains: &[&str]) -> Self {
        self.deny_domains = domains.iter().map(|d| d.to_lowercase()).collect();
        self
    }

    /// Only look inside the elements matched by these CSS selectors.
    pub fn restrict_css(mut self, selectors: &[&str]) -> Self {
        self.restrict_css = selectors.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Which tags to read URLs from. Default `["a", "area"]`.
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|t| t.to_string()).collect();
        self
    }

    /// Which attributes to read URLs from. Default `["href"]`.
    pub fn attrs(mut self, attrs: &[&str]) -> Self {
        self.attrs = attrs.iter().map(|a| a.to_string()).collect();
        self
    }

    /// Canonicalize the extracted URLs. Default `true`.
    pub fn canonicalize(mut self, yes: bool) -> Self {
        self.canonicalize = yes;
        self
    }

    /// Strip HTML5 whitespace from the raw attribute values. Default `true`.
    pub fn strip(mut self, yes: bool) -> Self {
        self.strip = yes;
        self
    }

    /// Keep the fragment when canonicalizing. Default `false`.
    pub fn keep_fragment(mut self, yes: bool) -> Self {
        self.keep_fragment = yes;
        self
    }

    /// Override the dropped extensions. Default [`IGNORED_EXTENSIONS`].
    pub fn deny_extensions(mut self, extensions: &[&str]) -> Self {
        self.deny_extensions = extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();
        self
    }

    /// Absolute, filtered, deduplicated URLs from `response`, in document order.
    ///
    /// Anything that cannot be parsed — a broken selector, an href that is not a URL — is
    /// skipped rather than reported, exactly like the Python implementation.
    pub fn extract(&self, response: &Response) -> Vec<String> {
        let root = response.selector();

        if self.tags.is_empty() || self.attrs.is_empty() {
            return Vec::new();
        }

        let mut scopes: Vec<Selector> = Vec::new();
        for selector in &self.restrict_css {
            match root.css(selector) {
                Ok(found) => scopes.extend(found.iter().cloned()),
                Err(error) => {
                    tracing::debug!(%selector, %error, "ignoring an unusable restrict_css selector");
                }
            }
        }
        // Python falls back to the whole document whenever the restricting selectors matched
        // nothing at all, so a selector that stops matching does not silently stop the crawl.
        let scopes = if scopes.is_empty() {
            vec![root.clone()]
        } else {
            scopes
        };

        let tag_selector = self.tags.join(", ");
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut truncated = false;
        for scope in &scopes {
            let elements = match scope.css(&tag_selector) {
                Ok(elements) => elements,
                Err(error) => {
                    tracing::debug!(%error, "ignoring an unusable tag selector");
                    continue;
                }
            };
            for element in elements.iter() {
                for attribute in &self.attrs {
                    let Some(value) = element.attr(attribute) else {
                        continue;
                    };
                    let Some(url) = self.normalize(response, value.as_str()) else {
                        continue;
                    };
                    if out.len() >= MAX_LINKS_PER_RESPONSE {
                        truncated = true;
                        break;
                    }
                    if seen.insert(url.clone()) {
                        out.push(url);
                    }
                }
                if truncated {
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        if truncated {
            tracing::warn!(
                url = %response.url,
                limit = MAX_LINKS_PER_RESPONSE,
                "stopped extracting links: the page carries more than the per-page limit"
            );
        }
        out
    }

    /// Whether a single URL passes the filters, without a response.
    pub fn matches(&self, url: &str) -> bool {
        let candidate = if self.canonicalize {
            canonicalize_url(url, self.keep_fragment)
        } else {
            url.to_string()
        };
        self.url_passes(&candidate)
    }

    /// Turn one raw attribute value into an absolute, canonical, safe URL.
    fn normalize(&self, response: &Response, raw: &str) -> Option<String> {
        let mut value = raw;
        if self.strip {
            value = strip_html5_whitespace(value);
        }
        if value.is_empty() {
            return None;
        }
        // A `data:` URI or a generated attribute can be arbitrarily long; nothing fetchable is,
        // so drop it before it is joined, canonicalized and copied around.
        if value.len() > MAX_URL_BYTES {
            tracing::debug!(
                bytes = value.len(),
                limit = MAX_URL_BYTES,
                "skipping an over-long link"
            );
            return None;
        }

        let mut url = match response.urljoin(value) {
            Ok(url) => url,
            Err(error) => {
                tracing::debug!(%error, href = %value, "skipping a link that could not be resolved");
                return None;
            }
        };

        if self.canonicalize {
            url = canonicalize_url(&url, self.keep_fragment);
        }

        let url = match safe_url_string(&url) {
            Some(url) => url,
            None => {
                tracing::debug!(%url, "skipping the extraction of a bad URL");
                return None;
            }
        };

        if self.url_passes(&url) {
            Some(url)
        } else {
            None
        }
    }

    fn url_passes(&self, url: &str) -> bool {
        let scheme = match url.split_once("://") {
            Some((scheme, _)) => scheme.to_lowercase(),
            None => return false,
        };
        if !VALID_SCHEMES.contains(&scheme.as_str()) {
            return false;
        }

        if !self.deny_extensions.is_empty() {
            for extension in url_extensions(url) {
                if self.deny_extensions.contains(&extension) {
                    return false;
                }
            }
        }

        if !self.allow.is_empty() && !self.allow.iter().any(|pattern| pattern.is_match(url)) {
            return false;
        }
        if self.deny.iter().any(|pattern| pattern.is_match(url)) {
            return false;
        }

        if !self.allow_domains.is_empty() || !self.deny_domains.is_empty() {
            let host = ::url::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(|host| host.to_lowercase()))
                .unwrap_or_default();
            if !self.allow_domains.is_empty() && !host_matches(&host, &self.allow_domains) {
                return false;
            }
            if host_matches(&host, &self.deny_domains) {
                return false;
            }
        }

        true
    }
}

fn host_matches(host: &str, domains: &[String]) -> bool {
    domains
        .iter()
        .any(|domain| host == domain || host.ends_with(&format!(".{domain}")))
}

fn compile(patterns: &[&str]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| {
            Regex::new(pattern)
                .map_err(|error| Error::Other(format!("invalid link pattern `{pattern}`: {error}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
        <html><body>
          <div class="main">
            <a href="/one">one</a>
            <a href="/two?b=2&amp;a=1">two</a>
            <a href=" /three ">spaced</a>
            <a href="https://other.example.org/four">offsite</a>
            <a href="/manual.pdf">a document</a>
            <a href="mailto:someone@example.com">mail</a>
            <a href="file:///etc/passwd">a local file</a>
            <a href="/one">a duplicate</a>
            <area href="/area" />
          </div>
          <div class="aside"><a href="/aside">aside</a></div>
        </body></html>
    "#;

    fn page() -> Response {
        Response::new(
            "http://example.com/index.html",
            PAGE.as_bytes().to_vec(),
            200,
        )
        .expect("the fixture parses")
    }

    #[test]
    fn extracts_absolute_canonical_deduplicated_links() {
        let links = LinkExtractor::new().extract(&page());
        assert_eq!(
            links,
            vec![
                "http://example.com/one".to_string(),
                "http://example.com/two?a=1&b=2".to_string(),
                "http://example.com/three".to_string(),
                "https://other.example.org/four".to_string(),
                "http://example.com/aside".to_string(),
                "http://example.com/area".to_string(),
            ]
        );
    }

    #[test]
    fn allow_and_deny_patterns_filter() {
        let extractor = LinkExtractor::new().allow(&[r"/t"]).expect("valid pattern");
        assert_eq!(
            extractor.extract(&page()),
            vec![
                "http://example.com/two?a=1&b=2".to_string(),
                "http://example.com/three".to_string(),
            ]
        );

        let extractor = LinkExtractor::new()
            .allow(&[r"/t"])
            .expect("valid pattern")
            .deny(&[r"three"])
            .expect("valid pattern");
        assert_eq!(
            extractor.extract(&page()),
            vec!["http://example.com/two?a=1&b=2".to_string()]
        );
    }

    #[test]
    fn domain_filters_use_subdomain_matching() {
        let extractor = LinkExtractor::new().allow_domains(&["example.org"]);
        assert_eq!(
            extractor.extract(&page()),
            vec!["https://other.example.org/four".to_string()]
        );

        let extractor = LinkExtractor::new().deny_domains(&["example.org"]);
        assert!(!extractor
            .extract(&page())
            .contains(&"https://other.example.org/four".to_string()));
    }

    #[test]
    fn restrict_css_scopes_the_search() {
        let extractor = LinkExtractor::new().restrict_css(&["div.aside"]);
        assert_eq!(
            extractor.extract(&page()),
            vec!["http://example.com/aside".to_string()]
        );
    }

    #[test]
    fn a_restrict_css_that_matches_nothing_falls_back_to_the_page() {
        // Python's `extract` does `if not scopes: scopes = [response]`.
        let extractor = LinkExtractor::new().restrict_css(&["div.missing"]);
        assert!(extractor
            .extract(&page())
            .contains(&"http://example.com/one".to_string()));
    }

    #[test]
    fn without_tags_or_attrs_nothing_is_extracted() {
        assert!(LinkExtractor::new().tags(&[]).extract(&page()).is_empty());
        assert!(LinkExtractor::new().attrs(&[]).extract(&page()).is_empty());
    }

    #[test]
    fn tags_and_attrs_are_configurable() {
        let extractor = LinkExtractor::new().tags(&["area"]);
        assert_eq!(
            extractor.extract(&page()),
            vec!["http://example.com/area".to_string()]
        );
    }

    #[test]
    fn ignored_extensions_and_schemes_are_dropped() {
        let links = LinkExtractor::new().extract(&page());
        assert!(!links.iter().any(|link| link.ends_with("manual.pdf")));
        assert!(!links.iter().any(|link| link.starts_with("mailto:")));

        let extractor = LinkExtractor::new().deny_extensions(&[]);
        assert!(extractor
            .extract(&page())
            .contains(&"http://example.com/manual.pdf".to_string()));
    }

    /// A `file://` link on a scraped page must never become a scheduled request: the crawl
    /// would hand it to a session, and a browser session would happily read the local file.
    #[test]
    fn local_file_links_never_survive_extraction() {
        let links = LinkExtractor::new().extract(&page());
        assert!(
            !links.iter().any(|link| link.starts_with("file:")),
            "extracted {links:?}"
        );

        let extractor = LinkExtractor::new().deny_extensions(&[]);
        assert!(!extractor.matches("file:///etc/passwd"));
        assert!(!extractor.matches("file://localhost/etc/hosts"));
        assert!(!extractor.matches("view-source://example.com/"));
        assert!(!extractor.matches("chrome://settings/"));
        assert!(extractor.matches("http://example.com/etc/passwd"));
    }

    #[test]
    fn matches_filters_a_bare_url() {
        let extractor = LinkExtractor::new()
            .allow(&[r"/product/"])
            .expect("valid pattern")
            .allow_domains(&["example.com"]);
        assert!(extractor.matches("http://shop.example.com/product/1"));
        assert!(!extractor.matches("http://shop.example.com/about"));
        assert!(!extractor.matches("http://elsewhere.test/product/1"));
        assert!(!extractor.matches("ftp://example.com/product/1"));
        assert!(!extractor.matches("http://example.com/product/manual.pdf"));
    }

    #[test]
    fn every_trailing_extension_counts() {
        let extractor = LinkExtractor::new().deny_extensions(&["gz"]);
        assert!(!extractor.matches("http://example.com/archive.tar.gz"));
        let extractor = LinkExtractor::new().deny_extensions(&["tar.gz"]);
        assert!(!extractor.matches("http://example.com/archive.tar.gz"));
    }

    #[test]
    fn a_broken_pattern_is_an_error_not_a_panic() {
        assert!(LinkExtractor::new().allow(&["("]).is_err());
    }
}
