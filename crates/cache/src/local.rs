//! Local filesystem cache backend.

use crate::entry::CacheEntry;
use crate::provider::CacheProvider;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Cache backend that stores entries as JSON files on the local filesystem.
///
/// Default location: `~/.rage/cache/`.
/// Each entry is stored as `{fingerprint}.json`.
pub struct LocalCache {
    dir: PathBuf,
}

impl LocalCache {
    /// Create a LocalCache using the default directory (`~/.rage/cache/`).
    /// Creates the directory if it does not exist.
    pub fn new() -> Result<Self> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .context("HOME or USERPROFILE env var not set")?;
        Self::with_dir(PathBuf::from(home).join(".rage").join("cache"))
    }

    /// Create a LocalCache using the given directory.
    /// Creates the directory if it does not exist.
    pub fn with_dir(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache dir {}", dir.display()))?;
        Ok(Self { dir })
    }
}

impl CacheProvider for LocalCache {
    fn get(&self, key: &str) -> Option<CacheEntry> {
        let path = self.dir.join(format!("{key}.json"));
        let raw = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()> {
        let path = self.dir.join(format!("{key}.json"));
        let json = serde_json::to_string_pretty(entry)
            .context("serializing cache entry")?;
        std::fs::write(&path, json)
            .with_context(|| format!("writing cache entry to {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_entry(fp: &str) -> CacheEntry {
        CacheEntry {
            fingerprint: fp.to_string(),
            command: "echo test".to_string(),
            exit_code: 0,
            elapsed_ms: 10,
            cached_at: 0,
        }
    }

    #[test]
    fn miss_returns_none() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        assert!(cache.get("nonexistent-key").is_none());
    }

    #[test]
    fn put_then_get_roundtrips() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        let entry = sample_entry("abc123def456");
        cache.put("abc123def456", &entry).unwrap();
        let retrieved = cache.get("abc123def456").unwrap();
        assert_eq!(retrieved, entry);
    }

    #[test]
    fn creates_dir_if_missing() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("a").join("b").join("c");
        // sub does not exist yet
        let cache = LocalCache::with_dir(sub.clone()).unwrap();
        // dir was created
        assert!(sub.is_dir());
        // and cache works
        let entry = sample_entry("key1");
        cache.put("key1", &entry).unwrap();
        assert!(cache.get("key1").is_some());
    }

    #[test]
    fn corrupt_json_returns_none() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        // Write garbage JSON to a cache key file
        std::fs::write(dir.path().join("badkey.json"), b"not valid json").unwrap();
        assert!(cache.get("badkey").is_none(), "corrupt JSON should return None");
    }

    #[test]
    fn different_keys_stored_independently() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        let e1 = sample_entry("fp1");
        let e2 = CacheEntry {
            fingerprint: "fp2".to_string(),
            command: "cargo build".to_string(),
            exit_code: 0,
            elapsed_ms: 500,
            cached_at: 100,
        };
        cache.put("fp1", &e1).unwrap();
        cache.put("fp2", &e2).unwrap();
        assert_eq!(cache.get("fp1").unwrap(), e1);
        assert_eq!(cache.get("fp2").unwrap(), e2);
    }
}
