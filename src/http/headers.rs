//! Static browser header profiles.
//!
//! Scrapling leans on `curl_cffi`'s TLS/HTTP impersonation and on `browserforge` to synthesize a
//! header set at runtime (`scrapling/engines/toolbelt/fingerprints.py`). `reqwest` cannot
//! impersonate a browser's TLS fingerprint, so this port ships a small table of realistic,
//! self-consistent desktop header sets instead: one per [`BrowserProfile`]. The header *values*
//! are what a fresh desktop browser sends for a top-level navigation.

use crate::response::HeaderMap;

/// The referer `stealthy_headers` adds when the caller did not set one, as in Python's
/// `_headers_job`.
pub(crate) const GOOGLE_REFERER: &str = "https://www.google.com/";

/// The `User-Agent` sent when neither impersonation nor stealth headers are on and the caller
/// supplied none, standing in for Python's `__default_useragent__`.
pub(crate) const DEFAULT_USER_AGENT: &str = BrowserProfile::CHROME_UA;

/// The browser whose header set to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserProfile {
    /// Recent desktop Chrome (Windows).
    #[default]
    Chrome,
    /// Recent desktop Firefox (Windows).
    Firefox,
    /// Recent desktop Safari (macOS).
    Safari,
    /// Recent desktop Edge (Windows).
    Edge,
}

impl BrowserProfile {
    /// Just this profile's `User-Agent`.
    pub fn user_agent(&self) -> &'static str {
        match self {
            BrowserProfile::Chrome => Self::CHROME_UA,
            BrowserProfile::Firefox => Self::FIREFOX_UA,
            BrowserProfile::Safari => Self::SAFARI_UA,
            BrowserProfile::Edge => Self::EDGE_UA,
        }
    }

    /// The name of the profile, as used in log lines and `Debug` output.
    pub fn name(&self) -> &'static str {
        match self {
            BrowserProfile::Chrome => "chrome",
            BrowserProfile::Firefox => "firefox",
            BrowserProfile::Safari => "safari",
            BrowserProfile::Edge => "edge",
        }
    }

    /// The headers this profile sends for a top-level navigation.
    ///
    /// Chromium-based profiles carry the `sec-ch-ua*` client hints and the full `Sec-Fetch-*`
    /// set; Firefox and Safari carry only the headers those browsers actually send.
    ///
    /// `Accept-Encoding` advertises `gzip, deflate, br` because that is what a real browser
    /// sends, and `reqwest` leaves an `Accept-Encoding` we set ourselves alone. Every algorithm
    /// named here must therefore have its `reqwest` feature enabled (`gzip`, `deflate`,
    /// `brotli`), otherwise a server answering with that encoding would leave
    /// [`crate::Response::body`] compressed.
    pub fn headers(&self) -> HeaderMap {
        let pairs: &[(&str, &str)] = match self {
            BrowserProfile::Chrome => &[
                ("User-Agent", Self::CHROME_UA),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,\
                     image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
                ),
                ("Accept-Language", "en-US,en;q=0.9"),
                ("Accept-Encoding", "gzip, deflate, br"),
                (
                    "sec-ch-ua",
                    "\"Chromium\";v=\"141\", \"Not?A_Brand\";v=\"24\", \"Google Chrome\";v=\"141\"",
                ),
                ("sec-ch-ua-mobile", "?0"),
                ("sec-ch-ua-platform", "\"Windows\""),
                ("Sec-Fetch-Site", "none"),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-User", "?1"),
                ("Sec-Fetch-Dest", "document"),
                ("Upgrade-Insecure-Requests", "1"),
            ],
            BrowserProfile::Edge => &[
                ("User-Agent", Self::EDGE_UA),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,\
                     image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
                ),
                ("Accept-Language", "en-US,en;q=0.9"),
                ("Accept-Encoding", "gzip, deflate, br"),
                (
                    "sec-ch-ua",
                    "\"Chromium\";v=\"141\", \"Not?A_Brand\";v=\"24\", \"Microsoft Edge\";v=\"141\"",
                ),
                ("sec-ch-ua-mobile", "?0"),
                ("sec-ch-ua-platform", "\"Windows\""),
                ("Sec-Fetch-Site", "none"),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-User", "?1"),
                ("Sec-Fetch-Dest", "document"),
                ("Upgrade-Insecure-Requests", "1"),
            ],
            BrowserProfile::Firefox => &[
                ("User-Agent", Self::FIREFOX_UA),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,\
                     */*;q=0.8",
                ),
                ("Accept-Language", "en-US,en;q=0.5"),
                ("Accept-Encoding", "gzip, deflate, br"),
                ("Sec-Fetch-Site", "none"),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-User", "?1"),
                ("Sec-Fetch-Dest", "document"),
                ("Upgrade-Insecure-Requests", "1"),
            ],
            BrowserProfile::Safari => &[
                ("User-Agent", Self::SAFARI_UA),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
                ("Accept-Language", "en-US,en;q=0.9"),
                ("Accept-Encoding", "gzip, deflate, br"),
                ("Sec-Fetch-Site", "none"),
                ("Sec-Fetch-Mode", "navigate"),
                ("Sec-Fetch-Dest", "document"),
                ("Upgrade-Insecure-Requests", "1"),
            ],
        };

        HeaderMap::from_pairs(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        )
    }

    const CHROME_UA: &'static str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/141.0.0.0 Safari/537.36";
    const EDGE_UA: &'static str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
    const FIREFOX_UA: &'static str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0";
    const SAFARI_UA: &'static str =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) \
         Version/18.6 Safari/605.1.15";
}

/// Build the headers actually sent for one request.
///
/// Port of `_ConfigurationLogic._headers_job`:
///
/// 1. session headers merged with the per-request headers, the request winning;
/// 2. when `stealth` is on and the caller set no `referer`, a Google referer is added;
/// 3. the profile's headers fill in every header the *caller* did not already set, so
///    user-supplied values are never overwritten;
/// 4. with no profile at all (impersonation off and stealth off), only a `User-Agent` is
///    filled in, which is Python's `__default_useragent__` branch.
///
/// The "did the caller set it?" test is taken against the merged session+request map *before*
/// anything is added, exactly like Python's `headers_keys` snapshot; otherwise a header this
/// function adds could suppress another one it is about to add.
pub(crate) fn headers_job(
    session_headers: &HeaderMap,
    request_headers: &HeaderMap,
    profile: Option<BrowserProfile>,
    stealth: bool,
) -> HeaderMap {
    let mut final_headers = session_headers.clone();
    final_headers.merge(request_headers);

    // Python: `headers_keys = {k.lower() for k in final_headers}`, computed once, up front.
    let caller_keys: Vec<String> = final_headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let was_set = |name: &str| {
        let lowered = name.to_ascii_lowercase();
        caller_keys.contains(&lowered)
    };

    let fill_in = |headers: &mut HeaderMap, profile: BrowserProfile| {
        let profile_headers = profile.headers();
        for (name, value) in profile_headers.iter() {
            if !was_set(name) {
                headers.insert(name, value);
            }
        }
    };

    if stealth {
        if !was_set("referer") {
            final_headers.insert("referer", GOOGLE_REFERER);
        }
        if let Some(profile) = profile {
            fill_in(&mut final_headers, profile);
        }
    } else if let Some(profile) = profile {
        // Impersonation with stealth off: `curl_cffi` still sends that browser's header set,
        // it just does not add the Google referer.
        fill_in(&mut final_headers, profile);
    } else if !was_set("user-agent") {
        // Python's `elif "user-agent" not in headers_keys and not impersonate_enabled` branch.
        final_headers.insert("User-Agent", DEFAULT_USER_AGENT);
    }

    final_headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_carries_the_documented_headers() {
        for profile in [
            BrowserProfile::Chrome,
            BrowserProfile::Firefox,
            BrowserProfile::Safari,
            BrowserProfile::Edge,
        ] {
            let headers = profile.headers();
            assert_eq!(headers.get("user-agent"), Some(profile.user_agent()));
            for required in [
                "accept",
                "accept-language",
                "accept-encoding",
                "sec-fetch-mode",
                "sec-fetch-dest",
                "upgrade-insecure-requests",
            ] {
                assert!(
                    headers.contains_key(required),
                    "{} is missing `{required}`",
                    profile.name()
                );
            }
        }
    }

    #[test]
    fn only_chromium_profiles_send_client_hints() {
        assert!(BrowserProfile::Chrome.headers().contains_key("sec-ch-ua"));
        assert!(BrowserProfile::Edge.headers().contains_key("sec-ch-ua"));
        assert!(!BrowserProfile::Firefox.headers().contains_key("sec-ch-ua"));
        assert!(!BrowserProfile::Safari.headers().contains_key("sec-ch-ua"));
    }

    #[test]
    fn stealth_adds_a_google_referer_and_profile_headers() {
        let headers = headers_job(
            &HeaderMap::new(),
            &HeaderMap::new(),
            Some(BrowserProfile::Chrome),
            true,
        );
        assert_eq!(headers.get("referer"), Some(GOOGLE_REFERER));
        assert_eq!(
            headers.get("user-agent"),
            Some(BrowserProfile::Chrome.user_agent())
        );
    }

    #[test]
    fn user_supplied_headers_are_never_overwritten() {
        let mut request = HeaderMap::new();
        request.insert("Referer", "https://mysite.test/");
        request.insert("user-agent", "custom-agent/1.0");

        let headers = headers_job(
            &HeaderMap::new(),
            &request,
            Some(BrowserProfile::Chrome),
            true,
        );
        assert_eq!(headers.get("referer"), Some("https://mysite.test/"));
        assert_eq!(headers.get("User-Agent"), Some("custom-agent/1.0"));
        // The rest of the profile still fills in.
        assert!(headers.contains_key("sec-ch-ua"));
    }

    #[test]
    fn request_headers_win_over_session_headers() {
        let session = HeaderMap::from_pairs([("X-Api-Key".to_string(), "session".to_string())]);
        let request = HeaderMap::from_pairs([("x-api-key".to_string(), "request".to_string())]);
        let headers = headers_job(&session, &request, None, false);
        assert_eq!(headers.get("x-api-key"), Some("request"));
        assert!(!headers.contains_key("referer"));
    }

    #[test]
    fn without_a_profile_only_a_user_agent_is_filled_in() {
        // Python's `elif "user-agent" not in headers_keys and not impersonate_enabled` branch.
        let headers = headers_job(&HeaderMap::new(), &HeaderMap::new(), None, false);
        assert_eq!(headers.get("user-agent"), Some(DEFAULT_USER_AGENT));
        assert_eq!(headers.len(), 1);

        let mine = HeaderMap::from_pairs([("User-Agent".to_string(), "mine/1.0".to_string())]);
        let headers = headers_job(&HeaderMap::new(), &mine, None, false);
        assert_eq!(headers.get("user-agent"), Some("mine/1.0"));
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn stealth_without_a_profile_only_adds_the_referer() {
        // Mirrors Python: the `user-agent` fallback lives in the `elif`, so stealth never
        // reaches it.
        let headers = headers_job(&HeaderMap::new(), &HeaderMap::new(), None, true);
        assert_eq!(headers.get("referer"), Some(GOOGLE_REFERER));
        assert!(!headers.contains_key("user-agent"));
    }

    #[test]
    fn a_profile_without_stealth_sends_no_referer() {
        let headers = headers_job(
            &HeaderMap::new(),
            &HeaderMap::new(),
            Some(BrowserProfile::Safari),
            false,
        );
        assert!(!headers.contains_key("referer"));
        assert_eq!(
            headers.get("user-agent"),
            Some(BrowserProfile::Safari.user_agent())
        );
    }
}
