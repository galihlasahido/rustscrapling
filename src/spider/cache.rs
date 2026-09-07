//! The development-mode response cache, a port of `scrapling/spiders/cache.py`.

use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::response::{Cookie, HeaderMap, Response};

/// The on-disk shape of a cached response; the same keys the Python implementation writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedResponse {
    url: String,
    /// The body, base64-encoded so the file stays valid JSON.
    content: String,
    status: u16,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    encoding: String,
    #[serde(default)]
    cookies: Vec<Cookie>,
    #[serde(default)]
    headers: HeaderMap,
    #[serde(default)]
    request_headers: HeaderMap,
    #[serde(default)]
    method: String,
}

/// Records responses to disk in development mode and replays them on later runs.
#[derive(Debug, Clone)]
pub struct ResponseCache {
    dir: PathBuf,
}

impl ResponseCache {
    /// A cache stored under `dir` (Scrapling uses `.scrapling_cache/<spider name>`).
    pub fn new(dir: impl Into<PathBuf>) -> ResponseCache {
        ResponseCache { dir: dir.into() }
    }

    /// The directory this cache writes into.
    pub fn dir(&self) -> &std::path::Path {
        self.dir.as_path()
    }

    fn path_for(&self, fingerprint: &[u8; 20]) -> PathBuf {
        self.dir.join(format!("{}.json", hex::encode(fingerprint)))
    }

    /// The cached response for a fingerprint, when there is a readable one.
    pub async fn get(&self, fingerprint: &[u8; 20]) -> Option<Response> {
        let path = self.path_for(fingerprint);
        let bytes = tokio::fs::read(&path).await.ok()?;

        match Self::decode(&bytes) {
            Ok(response) => Some(response),
            Err(error) => {
                tracing::warn!(
                    fingerprint = %hex::encode(fingerprint),
                    %error,
                    "failed to read a cached response"
                );
                None
            }
        }
    }

    fn decode(bytes: &[u8]) -> Result<Response> {
        let cached: CachedResponse = serde_json::from_slice(bytes)?;
        let body = BASE64
            .decode(cached.content.as_bytes())
            .map_err(|error| Error::Other(format!("bad base64 in a cached response: {error}")))?;

        let mut response = Response::new(cached.url, body, cached.status)?
            .with_cookies(cached.cookies)
            .with_headers(cached.headers)
            .with_request_headers(cached.request_headers);
        if !cached.reason.is_empty() {
            response = response.with_reason(cached.reason);
        }
        // An absent or empty label would otherwise replace the UTF-8 default with nothing and
        // re-parse the document for no reason.
        if !cached.encoding.is_empty() {
            response = response.with_encoding(cached.encoding);
        }
        if !cached.method.is_empty() {
            response = response.with_method(cached.method);
        }
        Ok(response)
    }

    /// Store a response, written atomically as `<hex fingerprint>.json`.
    pub async fn put(
        &self,
        fingerprint: &[u8; 20],
        response: &Response,
        method: &str,
    ) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;

        let cached = CachedResponse {
            url: response.url.clone(),
            content: BASE64.encode(&response.body),
            status: response.status,
            reason: response.reason.clone(),
            encoding: response.encoding.clone(),
            cookies: response.cookies.clone(),
            headers: response.headers.clone(),
            request_headers: response.request_headers.clone(),
            method: method.to_string(),
        };
        let serialized = serde_json::to_vec(&cached)?;

        let final_path = self.path_for(fingerprint);
        let temp_path = final_path.with_extension("tmp");
        if let Err(error) = tokio::fs::write(&temp_path, &serialized).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(Error::Io(error));
        }
        if let Err(error) = tokio::fs::rename(&temp_path, &final_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(Error::Io(error));
        }
        Ok(())
    }

    /// Delete every cached response.
    pub async fn clear(&self) -> Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
                tokio::fs::remove_file(&path).await?;
            }
        }
        tracing::info!(dir = %self.dir.display(), "cleared the response cache");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_a_response() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = ResponseCache::new(dir.path());
        let fingerprint = [7u8; 20];

        assert!(cache.get(&fingerprint).await.is_none());

        let response = Response::new(
            "http://example.com/a",
            b"<html><body><h1>hi</h1></body></html>".to_vec(),
            200,
        )
        .expect("a response")
        .with_reason("OK")
        .with_encoding("utf-8");

        cache
            .put(&fingerprint, &response, "GET")
            .await
            .expect("put");

        let replayed = cache.get(&fingerprint).await.expect("a cached response");
        assert_eq!(replayed.url, "http://example.com/a");
        assert_eq!(replayed.status, 200);
        assert_eq!(replayed.body, response.body);
        assert_eq!(replayed.method, "GET");

        cache.clear().await.expect("clear");
        assert!(cache.get(&fingerprint).await.is_none());
    }

    /// The cache is the crate's only `base64` user. It stays on the version `reqwest`/`hyper-util`
    /// already pull in so the dependency graph carries a single copy of the encoder; the API used
    /// here (`general_purpose::STANDARD` plus the `Engine` trait) is the same across those
    /// versions, so the pin costs nothing.
    #[test]
    fn base64_stays_on_the_version_the_http_stack_already_pulls_in() {
        let manifest = include_str!("../../Cargo.toml");
        let declared = manifest
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("base64 ="))
            .expect("`base64` is declared in Cargo.toml");
        assert_eq!(
            declared, "base64 = \"0.22\"",
            "base64 must stay on the version reqwest/hyper-util use, got `{declared}`"
        );

        // And the API this file relies on still round-trips on it.
        let encoded = BASE64.encode(b"<html>caf\xc3\xa9</html>");
        assert_eq!(
            BASE64.decode(&encoded).expect("decodes"),
            b"<html>caf\xc3\xa9</html>"
        );
    }

    #[tokio::test]
    async fn unreadable_files_are_a_miss_not_a_panic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = ResponseCache::new(dir.path());
        let fingerprint = [1u8; 20];
        tokio::fs::create_dir_all(dir.path()).await.expect("mkdir");
        tokio::fs::write(cache.path_for(&fingerprint), b"{ not json")
            .await
            .expect("write");
        assert!(cache.get(&fingerprint).await.is_none());
    }
}
