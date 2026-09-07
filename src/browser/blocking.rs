//! Request blocking: which resource types and which hosts are aborted before they leave the
//! browser.
//!
//! Port of `_is_domain_blocked` and `create_async_intercept_handler` from
//! `scrapling/engines/toolbelt/navigation.py`.

use std::collections::HashSet;

use crate::browser::constants::EXTRA_RESOURCES;

/// Whether `hostname`, or any of its parent domains, is in `domains`.
///
/// Walks the hostname's suffix chain: for `tracker.ads.doubleclick.net` it checks
/// `tracker.ads.doubleclick.net`, then `ads.doubleclick.net`, then `doubleclick.net`.
/// A suffix without a dot (the bare TLD) is never treated as a match, exactly like Python.
pub fn is_domain_blocked(hostname: &str, domains: &HashSet<String>) -> bool {
    if domains.is_empty() {
        return false;
    }
    if domains.contains(hostname) {
        return true;
    }
    let mut from = 0usize;
    while let Some(offset) = hostname[from..].find('.') {
        let dot = from + offset;
        let suffix = &hostname[dot + 1..];
        if suffix.contains('.') && domains.contains(suffix) {
            return true;
        }
        from = dot + 1;
    }
    false
}

/// Translate a Chrome DevTools Protocol resource type (`"Stylesheet"`, `"CSPViolationReport"`)
/// into the lower-case Playwright name Scrapling's [`EXTRA_RESOURCES`] list is written in.
///
/// The mapping is Playwright's own, so that the same requests are blocked here as there.
/// Unknown names fall back to their lower-case spelling, and names that are already
/// Playwright-shaped pass through unchanged.
///
/// Three entries of `EXTRA_RESOURCES` — `beacon`, `object` and `imageset` — have no Chrome
/// DevTools Protocol counterpart at all, so nothing ever normalises to them. That is not an
/// omission: Playwright cannot produce those names either, so Scrapling does not block them in
/// practice, and neither does this port. In particular `Ping`, which carries `<a ping>` and
/// `navigator.sendBeacon` traffic, is `"ping"` and not `"beacon"`.
pub fn normalize_resource_type(resource_type: &str) -> String {
    match resource_type {
        "Document" => "document".to_string(),
        "Stylesheet" => "stylesheet".to_string(),
        "Image" => "image".to_string(),
        "Media" => "media".to_string(),
        "Font" => "font".to_string(),
        "Script" => "script".to_string(),
        "TextTrack" => "texttrack".to_string(),
        "XHR" => "xhr".to_string(),
        "Fetch" => "fetch".to_string(),
        "Prefetch" => "prefetch".to_string(),
        "EventSource" => "eventsource".to_string(),
        "WebSocket" => "websocket".to_string(),
        "Manifest" => "manifest".to_string(),
        "SignedExchange" => "signedexchange".to_string(),
        "Ping" => "ping".to_string(),
        "CSPViolationReport" => "csp_report".to_string(),
        "Preflight" => "preflight".to_string(),
        "FedCM" => "fedcm".to_string(),
        "Other" => "other".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// Whether a resource of this type is dropped by `disable_resources`.
pub fn is_blocked_resource(resource_type: &str) -> bool {
    let normalized = normalize_resource_type(resource_type);
    EXTRA_RESOURCES.contains(&normalized.as_str())
}

/// The blocking rules of one session, applied to every intercepted request.
#[derive(Debug, Clone, Default)]
pub struct RequestFilter {
    disable_resources: bool,
    blocked_domains: HashSet<String>,
}

impl RequestFilter {
    /// Build a filter from the two session options that drive interception.
    ///
    /// Domains are trimmed and lower-cased, and blank entries are dropped. Python keeps the set
    /// exactly as given, which means an entry written `DoubleClick.net` there silently never
    /// matches, because the hostname it is compared against is always lower-case.
    pub fn new(disable_resources: bool, blocked_domains: impl IntoIterator<Item = String>) -> Self {
        RequestFilter {
            disable_resources,
            blocked_domains: blocked_domains
                .into_iter()
                .map(|domain| domain.trim().to_ascii_lowercase())
                .filter(|domain| !domain.is_empty())
                .collect(),
        }
    }

    /// Whether this filter blocks anything at all; when it does not, interception stays off.
    pub fn is_active(&self) -> bool {
        self.disable_resources || !self.blocked_domains.is_empty()
    }

    /// Whether resource types are being dropped.
    pub fn disable_resources(&self) -> bool {
        self.disable_resources
    }

    /// The blocked domains, lower-cased.
    pub fn blocked_domains(&self) -> &HashSet<String> {
        &self.blocked_domains
    }

    /// Whether this request must be aborted.
    ///
    /// Resource types are checked first and domains second, matching the `if`/`elif` order of
    /// Python's route handler. A URL that cannot be parsed has no host, so it is never blocked
    /// by the domain rules.
    pub fn should_block(&self, url: &str, resource_type: &str) -> bool {
        if self.disable_resources && is_blocked_resource(resource_type) {
            return true;
        }
        if self.blocked_domains.is_empty() {
            return false;
        }
        let hostname = host_of(url);
        is_domain_blocked(&hostname, &self.blocked_domains)
    }
}

/// The lower-case host of a URL, or `""` when it has none — Python's `urlparse(...).hostname`.
fn host_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => parsed.host_str().unwrap_or_default().to_ascii_lowercase(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domains(items: &[&str]) -> HashSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn exact_hostname_matches() {
        let blocked = domains(&["doubleclick.net"]);
        assert!(is_domain_blocked("doubleclick.net", &blocked));
    }

    #[test]
    fn subdomains_match_through_the_suffix_chain() {
        let blocked = domains(&["doubleclick.net"]);
        assert!(is_domain_blocked("tracker.ads.doubleclick.net", &blocked));
        assert!(is_domain_blocked("ads.doubleclick.net", &blocked));
    }

    #[test]
    fn a_deeper_entry_matches_only_below_itself() {
        let blocked = domains(&["ads.doubleclick.net"]);
        assert!(is_domain_blocked("tracker.ads.doubleclick.net", &blocked));
        assert!(!is_domain_blocked("doubleclick.net", &blocked));
        assert!(!is_domain_blocked("other.doubleclick.net", &blocked));
    }

    #[test]
    fn unrelated_hosts_and_suffix_lookalikes_do_not_match() {
        let blocked = domains(&["example.com"]);
        assert!(!is_domain_blocked("example.org", &blocked));
        assert!(!is_domain_blocked("notexample.com", &blocked));
        assert!(!is_domain_blocked("example.com.evil.net", &blocked));
    }

    #[test]
    fn a_bare_tld_never_matches() {
        // Python requires the walked suffix to still contain a dot, so "net" cannot block.
        let blocked = domains(&["net"]);
        assert!(!is_domain_blocked("ads.doubleclick.net", &blocked));
        // ... unless the hostname is literally the entry.
        assert!(is_domain_blocked("net", &blocked));
    }

    #[test]
    fn an_empty_set_blocks_nothing() {
        assert!(!is_domain_blocked("doubleclick.net", &HashSet::new()));
        assert!(!is_domain_blocked("", &HashSet::new()));
    }

    #[test]
    fn cdp_resource_names_map_onto_the_python_list() {
        assert_eq!(normalize_resource_type("Stylesheet"), "stylesheet");
        assert_eq!(normalize_resource_type("TextTrack"), "texttrack");
        assert_eq!(normalize_resource_type("CSPViolationReport"), "csp_report");
        assert_eq!(normalize_resource_type("Ping"), "ping");
        assert_eq!(normalize_resource_type("XHR"), "xhr");
        assert_eq!(normalize_resource_type("Frobnicate"), "frobnicate");
        // Already Playwright-shaped names survive untouched.
        assert_eq!(normalize_resource_type("stylesheet"), "stylesheet");
    }

    #[test]
    fn the_extra_resources_are_the_blocked_ones() {
        for blocked in [
            "Font",
            "Image",
            "Media",
            "TextTrack",
            "WebSocket",
            "Stylesheet",
        ] {
            assert!(is_blocked_resource(blocked), "{blocked} should be blocked");
        }
        for allowed in ["Document", "Script", "XHR", "Fetch", "Other"] {
            assert!(!is_blocked_resource(allowed), "{allowed} should pass");
        }
        // Playwright calls this one "ping", so it does not hit the "beacon" entry — and
        // Scrapling therefore does not block it either.
        assert!(!is_blocked_resource("Ping"));
        // CSP violation reports do map onto an entry of the list.
        assert!(is_blocked_resource("CSPViolationReport"));
    }

    #[test]
    fn an_inactive_filter_blocks_nothing() {
        let filter = RequestFilter::new(false, Vec::new());
        assert!(!filter.is_active());
        assert!(!filter.should_block("https://example.com/a.png", "Image"));
    }

    #[test]
    fn resource_blocking_ignores_the_host() {
        let filter = RequestFilter::new(true, Vec::new());
        assert!(filter.is_active());
        assert!(filter.should_block("https://example.com/a.png", "Image"));
        assert!(!filter.should_block("https://example.com/", "Document"));
    }

    #[test]
    fn domain_blocking_ignores_the_resource_type() {
        let filter = RequestFilter::new(false, vec!["doubleclick.net".to_string()]);
        assert!(filter.should_block("https://ads.doubleclick.net/x.js", "Script"));
        assert!(!filter.should_block("https://example.com/x.js", "Script"));
    }

    #[test]
    fn domains_are_lowercased_and_trimmed() {
        let filter = RequestFilter::new(false, vec!["  DoubleClick.NET ".to_string()]);
        assert!(filter.should_block("https://ADS.DoubleClick.net/x.js", "Script"));
    }

    #[test]
    fn blank_domain_entries_are_dropped() {
        let filter = RequestFilter::new(false, vec!["".to_string(), "   ".to_string()]);
        assert!(!filter.is_active());
    }

    #[test]
    fn an_unparsable_url_is_never_domain_blocked() {
        let filter = RequestFilter::new(false, vec!["doubleclick.net".to_string()]);
        assert!(!filter.should_block("not a url", "Script"));
        assert!(!filter.should_block("data:text/html,<p>hi", "Document"));
    }

    #[test]
    fn both_rules_apply_together() {
        let filter = RequestFilter::new(true, vec!["doubleclick.net".to_string()]);
        assert!(filter.should_block("https://example.com/a.woff2", "Font"));
        assert!(filter.should_block("https://ads.doubleclick.net/x.js", "Script"));
        assert!(!filter.should_block("https://example.com/x.js", "Script"));
    }
}
