//! Saving and restoring a paused crawl, a port of `scrapling/spiders/checkpoint.py`.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::request::Request;

/// The file a checkpoint is written to inside the crawl directory.
const CHECKPOINT_FILE: &str = "checkpoint.json";

/// The crawl state written to disk so a paused crawl can resume.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointData {
    /// The requests that were still queued.
    #[serde(default)]
    pub requests: Vec<Request>,
    /// The fingerprints already seen, hex-encoded so the file stays readable JSON.
    #[serde(default)]
    pub seen: Vec<String>,
}

impl CheckpointData {
    /// Build a checkpoint from a scheduler snapshot.
    pub fn from_snapshot(requests: Vec<Request>, seen: HashSet<[u8; 20]>) -> CheckpointData {
        let mut seen: Vec<String> = seen.iter().map(hex::encode).collect();
        seen.sort();
        CheckpointData { requests, seen }
    }

    /// The `seen` fingerprints decoded back into their binary form; unreadable entries are
    /// skipped rather than failing the whole restore.
    pub fn seen_fingerprints(&self) -> HashSet<[u8; 20]> {
        let mut out = HashSet::with_capacity(self.seen.len());
        for entry in &self.seen {
            match hex::decode(entry) {
                Ok(bytes) => match <[u8; 20]>::try_from(bytes.as_slice()) {
                    Ok(fingerprint) => {
                        out.insert(fingerprint);
                    }
                    Err(_) => {
                        tracing::warn!(entry = %entry, "skipping a malformed checkpoint fingerprint")
                    }
                },
                Err(_) => {
                    tracing::warn!(entry = %entry, "skipping a malformed checkpoint fingerprint")
                }
            }
        }
        out
    }
}

/// Reads and writes the checkpoint file.
#[derive(Debug, Clone)]
pub struct CheckpointManager {
    crawldir: PathBuf,
    interval: f64,
}

impl CheckpointManager {
    /// A manager writing `checkpoint.json` into `crawldir` every `interval` seconds
    /// (0 disables the periodic save).
    pub fn new(crawldir: impl Into<PathBuf>, interval: f64) -> CheckpointManager {
        CheckpointManager {
            crawldir: crawldir.into(),
            interval: if interval.is_finite() && interval > 0.0 {
                interval
            } else {
                0.0
            },
        }
    }

    /// The file this manager reads and writes.
    pub fn path(&self) -> PathBuf {
        self.crawldir.join(CHECKPOINT_FILE)
    }

    /// Whether a checkpoint file exists.
    pub async fn has_checkpoint(&self) -> bool {
        tokio::fs::metadata(self.path()).await.is_ok()
    }

    /// Write the state atomically: to a temp file, then rename over the real one.
    pub async fn save(&self, data: &CheckpointData) -> Result<()> {
        tokio::fs::create_dir_all(&self.crawldir).await?;

        let serialized = serde_json::to_vec(data)?;
        let final_path = self.path();
        let temp_path = final_path.with_extension("tmp");

        if let Err(error) = tokio::fs::write(&temp_path, &serialized).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(Error::Io(error));
        }
        if let Err(error) = tokio::fs::rename(&temp_path, &final_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(Error::Io(error));
        }

        tracing::info!(
            requests = data.requests.len(),
            seen = data.seen.len(),
            "checkpoint saved"
        );
        Ok(())
    }

    /// Read the state back; `None` when there is none or it cannot be read.
    pub async fn load(&self) -> Option<CheckpointData> {
        let bytes = tokio::fs::read(self.path()).await.ok()?;
        match serde_json::from_slice::<CheckpointData>(&bytes) {
            Ok(data) => {
                tracing::info!(
                    requests = data.requests.len(),
                    seen = data.seen.len(),
                    "checkpoint loaded"
                );
                Some(data)
            }
            Err(error) => {
                tracing::error!(%error, "failed to load the checkpoint, starting fresh");
                None
            }
        }
    }

    /// Delete the checkpoint file after a completed crawl.
    pub async fn cleanup(&self) -> Result<()> {
        match tokio::fs::remove_file(self.path()).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                tracing::warn!(%error, "failed to clean the checkpoint file up");
                Ok(())
            }
        }
    }

    /// The configured save interval in seconds.
    pub fn interval(&self) -> f64 {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saves_loads_and_cleans_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manager = CheckpointManager::new(dir.path(), 300.0);
        assert!(!manager.has_checkpoint().await);
        assert!(manager.load().await.is_none());

        let mut seen = HashSet::new();
        seen.insert([3u8; 20]);
        let data = CheckpointData::from_snapshot(
            vec![Request::new("http://example.com/a").priority(2)],
            seen.clone(),
        );

        manager.save(&data).await.expect("save");
        assert!(manager.has_checkpoint().await);

        let loaded = manager.load().await.expect("a checkpoint");
        assert_eq!(loaded.requests.len(), 1);
        assert_eq!(loaded.requests[0].url, "http://example.com/a");
        assert_eq!(loaded.requests[0].priority, 2);
        assert_eq!(loaded.seen_fingerprints(), seen);

        manager.cleanup().await.expect("cleanup");
        assert!(!manager.has_checkpoint().await);
        manager.cleanup().await.expect("cleanup is idempotent");
    }

    #[test]
    fn a_zero_or_negative_interval_disables_periodic_saves() {
        assert_eq!(CheckpointManager::new(".", 0.0).interval(), 0.0);
        assert_eq!(CheckpointManager::new(".", -5.0).interval(), 0.0);
        assert_eq!(CheckpointManager::new(".", 12.5).interval(), 12.5);
    }

    #[test]
    fn malformed_fingerprints_are_skipped() {
        let data = CheckpointData {
            requests: Vec::new(),
            seen: vec!["zz".to_string(), "0011".to_string(), hex::encode([9u8; 20])],
        };
        assert_eq!(data.seen_fingerprints().len(), 1);
    }
}
