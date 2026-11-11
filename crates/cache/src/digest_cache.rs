//! Stat-validated sharded BLAKE3 digest cache.
//!
//! Avoids re-reading full file contents on every candidate pathset lookup or
//! weak-fingerprint glob evaluation by caching `(FileStamp, blake3::Hash)` in
//! a 64-shard, 128-byte-aligned in-memory table. Unchanged files are validated
//! via a single metadata stat call instead of reading and hashing their bytes.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

const SHARD_COUNT: usize = 64;

/// Cheap metadata identity stamp for a file on disk.
///
/// Captures device, inode, byte length, modification time, and status-change
/// time in nanoseconds so any content mutation, truncation, or atomic rename
/// produces a distinct stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
}

impl FileStamp {
    /// Extract a [`FileStamp`] for `path` using `fs::symlink_metadata`.
    pub fn from_symlink_metadata(path: &Path) -> std::io::Result<Self> {
        let meta = fs::symlink_metadata(path)?;
        Ok(Self::from_metadata(&meta))
    }

    /// Extract a [`FileStamp`] for `path` following symlinks (`fs::metadata`).
    pub fn from_target_metadata(path: &Path) -> std::io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self::from_metadata(&meta))
    }

    /// Construct a [`FileStamp`] from an already-fetched [`Metadata`].
    pub fn from_metadata(meta: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mtime_ns = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
            let ctime_ns = i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec());
            Self {
                dev: meta.dev(),
                ino: meta.ino(),
                size: meta.len(),
                mtime_ns,
                ctime_ns,
            }
        }
        #[cfg(not(unix))]
        {
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(0);
            let ctime_ns = meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(mtime_ns);
            Self {
                dev: 0,
                ino: 0,
                size: meta.len(),
                mtime_ns,
                ctime_ns,
            }
        }
    }
}

/// A single cache-line-isolated shard (128 bytes covers two 64-byte x86_64
/// prefetch lines and one 128-byte Apple Silicon cache line).
#[repr(align(128))]
struct AlignedShard {
    map: RwLock<HashMap<PathBuf, (FileStamp, blake3::Hash)>>,
}

impl AlignedShard {
    fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }
}

/// 64-shard, 128-byte-aligned `(FileStamp, blake3::Hash)` cache keyed by [`PathBuf`].
pub struct DigestCache {
    shards: [AlignedShard; SHARD_COUNT],
}

impl Default for DigestCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DigestCache {
    /// Create an empty 64-shard [`DigestCache`].
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| AlignedShard::new()),
        }
    }

    fn shard_index(path: &Path) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        (hasher.finish() as usize) & (SHARD_COUNT - 1)
    }

    /// Return the BLAKE3 hash of `path`, validating any cached digest against
    /// its current [`FileStamp`] via `symlink_metadata` (or target metadata if
    /// `path` is a symlink to a regular file).
    ///
    /// Returns `None` if `path` does not exist or is not a regular file.
    pub fn get_or_hash(&self, path: &Path) -> Option<blake3::Hash> {
        let sym_meta = fs::symlink_metadata(path).ok()?;
        let meta = if sym_meta.file_type().is_symlink() {
            let target_meta = fs::metadata(path).ok()?;
            if !target_meta.is_file() {
                return None;
            }
            target_meta
        } else if sym_meta.is_file() {
            sym_meta
        } else {
            return None;
        };

        let stamp = FileStamp::from_metadata(&meta);
        let shard = &self.shards[Self::shard_index(path)];

        if let Ok(guard) = shard.map.read() {
            if let Some((cached_stamp, cached_hash)) = guard.get(path) {
                if *cached_stamp == stamp {
                    return Some(*cached_hash);
                }
            }
        }

        let bytes = fs::read(path).ok()?;
        let digest = blake3::hash(&bytes);

        if let Ok(mut guard) = shard.map.write() {
            guard.insert(path.to_path_buf(), (stamp, digest));
        }

        Some(digest)
    }

    /// Return the cached `(FileStamp, blake3::Hash)` entry for `path` without
    /// touching the filesystem. Useful for diagnostics and testing.
    pub fn peek(&self, path: &Path) -> Option<(FileStamp, blake3::Hash)> {
        let shard = &self.shards[Self::shard_index(path)];
        shard.map.read().ok()?.get(path).copied()
    }

    /// Clear all shards.
    pub fn clear(&self) {
        for shard in &self.shards {
            if let Ok(mut guard) = shard.map.write() {
                guard.clear();
            }
        }
    }
}

/// Process-wide [`DigestCache`] shared across fingerprint calculations.
pub fn global_digest_cache() -> &'static DigestCache {
    static CACHE: OnceLock<DigestCache> = OnceLock::new();
    CACHE.get_or_init(DigestCache::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn alignment_is_128_bytes() {
        assert_eq!(std::mem::align_of::<AlignedShard>(), 128);
    }

    #[test]
    fn caches_and_invalidates_on_file_modification() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("input.ts");
        fs::write(&file, b"export const x = 1;\n").unwrap();

        let cache = DigestCache::new();
        assert!(cache.peek(&file).is_none());

        let h1 = cache.get_or_hash(&file).expect("hash v1");
        assert_eq!(h1, blake3::hash(b"export const x = 1;\n"));

        let (stamp1, cached1) = cache.peek(&file).expect("entry cached");
        assert_eq!(cached1, h1);

        // Second lookup with unchanged file hits the stamp and returns the same hash.
        let h1_again = cache.get_or_hash(&file).expect("hash v1 again");
        assert_eq!(h1, h1_again);
        assert_eq!(cache.peek(&file).unwrap().0, stamp1);

        // Modify file with different size/content so FileStamp changes immediately.
        fs::write(&file, b"export const x = 222;\n").unwrap();
        let h2 = cache.get_or_hash(&file).expect("hash v2");
        assert_ne!(h1, h2);
        assert_eq!(h2, blake3::hash(b"export const x = 222;\n"));

        let (stamp2, cached2) = cache.peek(&file).expect("updated entry");
        assert_ne!(stamp1, stamp2);
        assert_eq!(cached2, h2);
    }

    #[test]
    fn non_existent_or_directory_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = DigestCache::new();
        assert!(cache.get_or_hash(dir.path()).is_none());
        assert!(cache.get_or_hash(&dir.path().join("missing.ts")).is_none());
    }
}
