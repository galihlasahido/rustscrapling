//! Proxy rotation and proxy-error detection.
//!
//! Port of `scrapling/engines/toolbelt/proxy_rotation.py`.

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};

/// The substrings Python's `is_proxy_error` looks for, lowercased.
const PROXY_ERROR_INDICATORS: [&str; 7] = [
    "net::err_proxy",
    "net::err_tunnel",
    "connection refused",
    "connection reset",
    "connection timed out",
    "failed to connect",
    "could not resolve proxy",
];

/// Whether an error looks like the proxy's fault rather than the site's.
///
/// Matches the same indicator substrings as Python, case-insensitively, against the error's
/// rendered message. The HTTP fetcher folds the whole `reqwest` source chain into that message,
/// so transport-level causes such as `Connection refused (os error 61)` are visible here.
pub fn is_proxy_error(error: &crate::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    PROXY_ERROR_INDICATORS
        .iter()
        .any(|indicator| message.contains(indicator))
}

/// The default rotation strategy: hand the proxies out sequentially, wrapping at the end.
///
/// Port of `cyclic_rotation`.
pub fn cyclic_rotation(proxies: &[String], current_index: usize) -> (String, usize) {
    if proxies.is_empty() {
        // `ProxyRotator::new` rejects an empty pool, so this is unreachable in practice; return
        // an empty proxy rather than panicking on an out-of-range index.
        return (String::new(), 0);
    }
    let index = current_index % proxies.len();
    (proxies[index].clone(), (index + 1) % proxies.len())
}

type Strategy = dyn Fn(&[String], usize) -> (String, usize) + Send + Sync + 'static;

/// Rotates through a pool of proxies.
///
/// Cheap to clone: every clone shares one pool, one strategy and one rotation cursor, so a
/// rotator handed to several fetchers keeps handing out distinct proxies.
#[derive(Clone)]
pub struct ProxyRotator {
    proxies: Arc<Vec<String>>,
    strategy: Arc<Strategy>,
    current_index: Arc<Mutex<usize>>,
}

impl ProxyRotator {
    /// A rotator that hands out the proxies cyclically.
    ///
    /// Returns [`Error::Http`] when `proxies` is empty, as Python raises `ValueError`.
    pub fn new(proxies: Vec<String>) -> Result<ProxyRotator> {
        ProxyRotator::with_strategy(proxies, cyclic_rotation)
    }

    /// A rotator with a custom strategy: `(proxies, current_index) -> (proxy, next_index)`.
    pub fn with_strategy(
        proxies: Vec<String>,
        strategy: impl Fn(&[String], usize) -> (String, usize) + Send + Sync + 'static,
    ) -> Result<ProxyRotator> {
        if proxies.is_empty() {
            return Err(Error::Http(
                "at least one proxy must be provided".to_string(),
            ));
        }
        Ok(ProxyRotator {
            proxies: Arc::new(proxies),
            strategy: Arc::new(strategy),
            current_index: Arc::new(Mutex::new(0)),
        })
    }

    /// The next proxy according to the strategy.
    ///
    /// A poisoned lock is recovered from rather than panicked on, so a proxy is always returned.
    pub fn get_proxy(&self) -> String {
        let mut index = match self.current_index.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let (proxy, next) = (self.strategy)(&self.proxies, *index);
        *index = next;
        proxy
    }

    /// A copy of the configured proxies.
    pub fn proxies(&self) -> Vec<String> {
        self.proxies.as_ref().clone()
    }

    /// Number of configured proxies.
    pub fn len(&self) -> usize {
        self.proxies.len()
    }

    /// Whether the pool is empty (it never is; [`ProxyRotator::new`] rejects that).
    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty()
    }
}

impl fmt::Debug for ProxyRotator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyRotator(proxies={})", self.proxies.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pools_are_rejected() {
        assert!(ProxyRotator::new(Vec::new()).is_err());
    }

    #[test]
    fn cyclic_rotation_wraps_around() {
        let rotator = ProxyRotator::new(vec![
            "http://a:8080".to_string(),
            "http://b:8080".to_string(),
        ])
        .expect("rotator");
        assert_eq!(rotator.len(), 2);
        assert!(!rotator.is_empty());
        assert_eq!(rotator.get_proxy(), "http://a:8080");
        assert_eq!(rotator.get_proxy(), "http://b:8080");
        assert_eq!(rotator.get_proxy(), "http://a:8080");
        assert_eq!(rotator.proxies().len(), 2);
    }

    #[test]
    fn clones_share_the_cursor() {
        let rotator = ProxyRotator::new(vec!["a".to_string(), "b".to_string()]).expect("rotator");
        let clone = rotator.clone();
        assert_eq!(rotator.get_proxy(), "a");
        assert_eq!(clone.get_proxy(), "b");
    }

    #[test]
    fn custom_strategies_are_honoured() {
        let rotator = ProxyRotator::with_strategy(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            |proxies: &[String], _: usize| (proxies[proxies.len() - 1].clone(), 0),
        )
        .expect("rotator");
        assert_eq!(rotator.get_proxy(), "c");
        assert_eq!(rotator.get_proxy(), "c");
    }

    #[test]
    fn debug_matches_python_repr() {
        let rotator = ProxyRotator::new(vec!["a".to_string()]).expect("rotator");
        assert_eq!(format!("{rotator:?}"), "ProxyRotator(proxies=1)");
    }

    #[test]
    fn proxy_errors_are_recognised() {
        assert!(is_proxy_error(&Error::Http(
            "error sending request: Connection refused (os error 61)".to_string()
        )));
        assert!(is_proxy_error(&Error::Http(
            "net::ERR_PROXY_CONNECTION_FAILED".to_string()
        )));
        assert!(is_proxy_error(&Error::Browser(
            "net::ERR_TUNNEL_CONNECTION_FAILED".to_string()
        )));
        assert!(is_proxy_error(&Error::Http(
            "Could not resolve proxy: nope.invalid".to_string()
        )));
        assert!(!is_proxy_error(&Error::Http("404 Not Found".to_string())));
    }
}
