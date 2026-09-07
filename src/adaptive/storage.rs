//! Persistence for element fingerprints — a port of `scrapling/core/storage.py`.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};
use url::{Host, Url};

use crate::adaptive::fingerprint::ElementFingerprint;
use crate::error::{Error, Result};

/// The row key used when the caller has no URL, matching Python's `default_value`.
const DEFAULT_DOMAIN: &str = "default";

/// Second-level labels that are part of the public suffix rather than the registrable
/// domain, so `example.co.uk` is kept whole instead of collapsing to `co.uk`.
const SECOND_LEVEL_LABELS: [&str; 7] = ["co", "com", "net", "org", "gov", "ac", "edu"];

/// The schema Python creates, so a database file works with either implementation.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS storage (
    id INTEGER PRIMARY KEY,
    url TEXT,
    identifier TEXT,
    element_data TEXT,
    UNIQUE (url, identifier)
)";

/// Somewhere to keep element fingerprints between runs.
pub trait Storage: Send + Sync {
    /// Store `fingerprint` under `identifier` for the site `url` belongs to.
    fn save(&self, url: &str, identifier: &str, fingerprint: &ElementFingerprint) -> Result<()>;
    /// Load the fingerprint stored for `identifier` on the site `url` belongs to.
    fn retrieve(&self, url: &str, identifier: &str) -> Result<Option<ElementFingerprint>>;
}

impl<S: Storage + ?Sized> Storage for Arc<S> {
    fn save(&self, url: &str, identifier: &str, fingerprint: &ElementFingerprint) -> Result<()> {
        (**self).save(url, identifier, fingerprint)
    }

    fn retrieve(&self, url: &str, identifier: &str) -> Result<Option<ElementFingerprint>> {
        (**self).retrieve(url, identifier)
    }
}

/// The default [`Storage`]: one SQLite file, safe to share between threads.
///
/// The connection sits behind a mutex, which is what makes the type `Send + Sync`; the
/// Python class guards the same connection with an `RLock` for the same reason.
#[derive(Debug)]
pub struct SqliteStorage {
    connection: Mutex<Connection>,
}

impl SqliteStorage {
    /// Open (creating it if needed) the SQLite file backing this storage.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path.as_ref()).map_err(sqlite_error)?;
        Self::from_connection(connection)
    }

    /// Open an in-memory database, for tests.
    pub fn in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory().map_err(sqlite_error)?;
        Self::from_connection(connection)
    }

    /// Turn on write-ahead logging and make sure the table exists.
    fn from_connection(connection: Connection) -> Result<Self> {
        // `PRAGMA journal_mode` answers with a row, so it has to be run as a query rather
        // than with `execute`, which rejects statements that return rows. An in-memory
        // database always answers `memory`; that is not an error, it just cannot use WAL.
        let _ = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        connection.execute(SCHEMA, []).map_err(sqlite_error)?;
        Ok(SqliteStorage {
            connection: Mutex::new(connection),
        })
    }

    /// The registrable base domain of `url`, or `"default"` — the storage's row key.
    ///
    /// The URL is lowercased first and a missing scheme is filled in, the way Python's `tld`
    /// does it with `fix_protocol=True`: anything that is not already `http://` or `https://`
    /// gets `https://` put in front of it. The host is then reduced to its last two labels —
    /// or its last three when the second-to-last label is one of the common second-level ones
    /// (`co`, `com`, `net`, `org`, `gov`, `ac`, `edu`), so `shop.example.co.uk` becomes
    /// `example.co.uk`. IP addresses are used as they are, and anything with no host at all
    /// falls back to `"default"`.
    ///
    /// Unlike Python this does not consult the public suffix list, so a host it cannot break
    /// down — `localhost`, an IP literal, an unusual suffix — comes back whole instead of as
    /// `"default"`. Grouping is all the value is used for, and a whole host groups correctly.
    pub fn base_domain(url: &str) -> String {
        let lowered = url.trim().to_lowercase();
        if lowered.is_empty() {
            return DEFAULT_DOMAIN.to_string();
        }

        let candidate = with_protocol(&lowered);
        if authority_of(&candidate).is_empty() {
            // `https:///just/a/path` has no authority at all, but the WHATWG parser skips the
            // extra slash for a special scheme and reads `just` as the host. Python's `tld`
            // fails on such a URL, so reject it here rather than inventing a domain.
            return DEFAULT_DOMAIN.to_string();
        }

        let Ok(parsed) = Url::parse(&candidate) else {
            return DEFAULT_DOMAIN.to_string();
        };

        match parsed.host() {
            Some(Host::Domain(domain)) => {
                let host = domain.trim_end_matches('.');
                if host.is_empty() {
                    DEFAULT_DOMAIN.to_string()
                } else {
                    registrable_domain(host)
                }
            }
            Some(Host::Ipv4(address)) => address.to_string(),
            Some(Host::Ipv6(address)) => address.to_string(),
            None => DEFAULT_DOMAIN.to_string(),
        }
    }

    /// Run `body` with the connection locked.
    ///
    /// A poisoned mutex only means some other thread panicked while holding it; the
    /// connection itself is still usable, so the guard is recovered rather than dropped.
    fn with_connection<T>(
        &self,
        body: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        body(&connection).map_err(sqlite_error)
    }
}

impl Storage for SqliteStorage {
    fn save(&self, url: &str, identifier: &str, fingerprint: &ElementFingerprint) -> Result<()> {
        let domain = SqliteStorage::base_domain(url);
        let element_data = serde_json::to_string(fingerprint)?;

        self.with_connection(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO storage (url, identifier, element_data) VALUES (?1, ?2, ?3)",
                rusqlite::params![domain, identifier, element_data],
            )
        })?;
        Ok(())
    }

    fn retrieve(&self, url: &str, identifier: &str) -> Result<Option<ElementFingerprint>> {
        let domain = SqliteStorage::base_domain(url);

        let stored: Option<String> = self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT element_data FROM storage WHERE url = ?1 AND identifier = ?2",
                    rusqlite::params![domain, identifier],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })?;

        match stored {
            Some(element_data) => Ok(Some(serde_json::from_str(&element_data)?)),
            None => Ok(None),
        }
    }
}

/// Python's `tld.utils.fix_protocol`: give the URL a scheme when it has none.
fn with_protocol(url: &str) -> Cow<'_, str> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Cow::Borrowed(url)
    } else if let Some(rest) = url.strip_prefix("//") {
        Cow::Owned(format!("https://{rest}"))
    } else {
        Cow::Owned(format!("https://{url}"))
    }
}

/// The authority of a URL that already carries an `http://` or `https://` scheme.
///
/// Empty when the URL has none, which is how a path-only input is told apart from a real host.
fn authority_of(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    match rest.find(['/', '?', '#']) {
        Some(end) => &rest[..end],
        None => rest,
    }
}

/// The registrable part of a hostname, using the short second-level list above.
fn registrable_domain(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    match labels.len() {
        0 => DEFAULT_DOMAIN.to_string(),
        1 | 2 => labels.join("."),
        length => {
            let keep = if SECOND_LEVEL_LABELS.contains(&labels[length - 2]) {
                3
            } else {
                2
            };
            labels[length - keep..].join(".")
        }
    }
}

/// Every SQLite failure becomes [`Error::Storage`].
fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample() -> ElementFingerprint {
        let mut attributes = BTreeMap::new();
        attributes.insert("class".to_string(), "product".to_string());
        attributes.insert("id".to_string(), "p1".to_string());

        ElementFingerprint {
            tag: "article".to_string(),
            attributes,
            text: None,
            path: vec![
                "html".to_string(),
                "body".to_string(),
                "article".to_string(),
            ],
            parent_name: Some("section".to_string()),
            parent_attribs: BTreeMap::new(),
            parent_text: None,
            siblings: vec!["article".to_string()],
            children: vec!["h3".to_string()],
        }
    }

    #[test]
    fn saves_and_retrieves_a_fingerprint() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        let print = sample();

        storage
            .save("https://example.com/products", "#p1", &print)
            .expect("the fingerprint is stored");

        let loaded = storage
            .retrieve("https://example.com/other-page", "#p1")
            .expect("the query runs");
        assert_eq!(loaded, Some(print));
    }

    #[test]
    fn retrieving_an_unknown_identifier_returns_none() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        let loaded = storage
            .retrieve("https://example.com", "missing")
            .expect("the query runs");
        assert_eq!(loaded, None);
    }

    #[test]
    fn saving_twice_replaces_the_row() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        let mut print = sample();
        storage
            .save("https://example.com", "#p1", &print)
            .expect("the first save works");

        print.tag = "div".to_string();
        storage
            .save("https://example.com", "#p1", &print)
            .expect("the second save works");

        let loaded = storage
            .retrieve("https://example.com", "#p1")
            .expect("the query runs")
            .expect("a row is there");
        assert_eq!(loaded.tag, "div");
    }

    #[test]
    fn different_domains_are_kept_apart() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        storage
            .save("https://example.com", "#p1", &sample())
            .expect("the fingerprint is stored");

        let loaded = storage
            .retrieve("https://other.org", "#p1")
            .expect("the query runs");
        assert_eq!(loaded, None);
    }

    #[test]
    fn identifiers_are_stored_without_hashing() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        storage
            .save("https://example.com", "#p1", &sample())
            .expect("the fingerprint is stored");

        let (url, identifier): (String, String) = storage
            .with_connection(|connection| {
                connection.query_row("SELECT url, identifier FROM storage", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
            })
            .expect("the row is readable");

        assert_eq!(url, "example.com");
        assert_eq!(identifier, "#p1");
    }

    #[test]
    fn a_corrupt_row_is_an_error_rather_than_a_panic() {
        let storage = SqliteStorage::in_memory().expect("the in-memory database opens");
        storage
            .with_connection(|connection| {
                connection.execute(
                    "INSERT INTO storage (url, identifier, element_data) VALUES (?1, ?2, ?3)",
                    rusqlite::params!["example.com", "#p1", "{not json"],
                )
            })
            .expect("the row is written");

        assert!(storage.retrieve("https://example.com", "#p1").is_err());
    }

    #[test]
    fn base_domain_reduces_a_host_to_its_registrable_part() {
        assert_eq!(
            SqliteStorage::base_domain("https://www.example.com/a/b?c=d"),
            "example.com"
        );
        assert_eq!(
            SqliteStorage::base_domain("https://shop.example.co.uk/"),
            "example.co.uk"
        );
        assert_eq!(
            SqliteStorage::base_domain("example.com/path"),
            "example.com"
        );
        assert_eq!(
            SqliteStorage::base_domain("HTTPS://WWW.EXAMPLE.COM"),
            "example.com"
        );
        assert_eq!(
            SqliteStorage::base_domain("stackoverflow.com"),
            "stackoverflow.com"
        );
        assert_eq!(
            SqliteStorage::base_domain("http://localhost:8000"),
            "localhost"
        );
        assert_eq!(
            SqliteStorage::base_domain("http://127.0.0.1:8000/x"),
            "127.0.0.1"
        );
    }

    #[test]
    fn a_missing_scheme_is_filled_in_like_python_does() {
        // Without `fix_protocol` the parser would read `example.com` as the scheme here.
        assert_eq!(
            SqliteStorage::base_domain("example.com:8080/products"),
            "example.com"
        );
        assert_eq!(
            SqliteStorage::base_domain("//cdn.example.com/x"),
            "example.com"
        );
        assert_eq!(SqliteStorage::base_domain("www.example.com"), "example.com");
    }

    #[test]
    fn base_domain_falls_back_to_default() {
        assert_eq!(SqliteStorage::base_domain(""), DEFAULT_DOMAIN);
        assert_eq!(SqliteStorage::base_domain("   "), DEFAULT_DOMAIN);
        assert_eq!(SqliteStorage::base_domain("/just/a/path"), DEFAULT_DOMAIN);
    }

    #[test]
    fn a_malformed_url_never_panics() {
        for candidate in [
            "://",
            "http://",
            "not a url",
            "%%%",
            "http://[::1",
            "mailto:someone@example.com",
            "https://",
            "..",
            "https://....",
        ] {
            let _ = SqliteStorage::base_domain(candidate);
        }
    }

    #[test]
    fn storage_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SqliteStorage>();
        assert_send_sync::<Arc<SqliteStorage>>();
    }
}
