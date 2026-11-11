//! Stat-validated sharded BLAKE3 digest cache.
//!
//! Avoids re-reading full file contents on every candidate pathset lookup or
//! weak-fingerprint glob evaluation by caching `(FileStamp, blake3::Hash)` in
//! a 64-shard, 128-byte-aligned in-memory table. Unchanged files are validated
//! via a metadata stat call instead of reading and hashing their bytes, while
//! race-free open-handle `fstat` verification ensures concurrent modifications
//! or symlink retargetings never associate a digest with the wrong stamp.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
#[cfg(unix)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SHARD_COUNT: usize = 64;

/// Racily-clean recency window (2 seconds, matching Git's index `racy-git`
/// guard and covering 1-second coarse filesystems as well as sub-millisecond
/// clock ticks). Any file whose `mtime` or `ctime` is within this window of
/// `SystemTime::now()` (or in the future) is never served from the metadata
/// cache and is always re-hashed from disk.
#[cfg(unix)]
const RACY_CLEAN_WINDOW_NS: i128 = Duration::from_secs(2).as_nanos() as i128;

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
    pub fn from_symlink_metadata(path: &Path) -> std::io::Result<Option<Self>> {
        let meta = fs::symlink_metadata(path)?;
        Ok(Self::from_metadata(&meta))
    }

    /// Extract a [`FileStamp`] for `path` following symlinks (`fs::metadata`).
    pub fn from_target_metadata(path: &Path) -> std::io::Result<Option<Self>> {
        let meta = fs::metadata(path)?;
        Ok(Self::from_metadata(&meta))
    }

    /// Construct a [`FileStamp`] from an already-fetched [`Metadata`].
    ///
    /// Returns `None` on platforms (such as stable Rust on Windows) where
    /// `std::fs::Metadata` does not expose inode/file-index or a genuine
    /// metadata-change timestamp (`ctime_ns`), since `Metadata::created()` is
    /// the creation time and an in-place same-size rewrite that preserves
    /// `mtime` would otherwise alias to the same stamp.
    pub fn from_metadata(meta: &Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mtime_ns = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
            let ctime_ns = i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec());
            Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
                size: meta.len(),
                mtime_ns,
                ctime_ns,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            None
        }
    }

    /// Returns `true` when this stamp is old enough (strictly older than the
    /// racy-clean guard window relative to `SystemTime::now()`), so a
    /// subsequent same-size rewrite cannot share the same `(mtime_ns, ctime_ns)`
    /// within the same clock tick or coarse 1-second filesystem tick.
    pub fn is_racily_clean_safe(&self) -> bool {
        #[cfg(unix)]
        {
            let Ok(now_dur) = SystemTime::now().duration_since(UNIX_EPOCH) else {
                return false;
            };
            let now_ns = now_dur.as_nanos() as i128;
            let newest_ts = self.mtime_ns.max(self.ctime_ns);
            newest_ts > 0 && now_ns >= newest_ts && (now_ns - newest_ts) >= RACY_CLEAN_WINDOW_NS
        }
        #[cfg(not(unix))]
        {
            false
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
    /// On a cache miss, opens `path` once, reads its contents from that open
    /// file handle, and verifies via `file.metadata()` (`fstat`) before and
    /// after reading — plus a post-read `fs::metadata(path)` check — that the
    /// file identity and timestamps did not change or retarget during the read.
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

        let maybe_stamp = FileStamp::from_metadata(&meta);
        let shard = &self.shards[Self::shard_index(path)];

        if let Some(stamp) = maybe_stamp {
            if stamp.is_racily_clean_safe() {
                if let Ok(guard) = shard.map.read() {
                    if let Some((cached_stamp, cached_hash)) = guard.get(path) {
                        if *cached_stamp == stamp {
                            return Some(*cached_hash);
                        }
                    }
                }
            }
        }

        let (digest, verified_stamp) = read_and_hash_verified(path, maybe_stamp)?;

        if let Some(stamp) = verified_stamp {
            if let Ok(mut guard) = shard.map.write() {
                guard.insert(path.to_path_buf(), (stamp, digest));
            }
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

/// Open `path` once, read and hash its bytes from the open handle, and return
/// `(digest, verified_stamp)`. `verified_stamp` is `Some(stamp)` only when:
/// 1. `pre_open_stamp` (if present) equals the open handle's pre-read `fstat` stamp,
/// 2. the open handle's post-read `fstat` stamp equals its pre-read `fstat` stamp, and
/// 3. a post-read `fs::metadata(path)` stamp also matches (preventing symlink
///    retargeting between metadata lookup and open from poisoning the cache).
pub(crate) fn read_and_hash_verified(
    path: &Path,
    pre_open_stamp: Option<FileStamp>,
) -> Option<(blake3::Hash, Option<FileStamp>)> {
    let mut file = fs::File::open(path).ok()?;
    let meta_before = file.metadata().ok()?;
    if !meta_before.is_file() {
        return None;
    }
    let stamp_before = FileStamp::from_metadata(&meta_before);
    let mut bytes = Vec::with_capacity(meta_before.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    let digest = blake3::hash(&bytes);

    let verified_stamp = stamp_before.and_then(|sb| {
        if let Some(pre) = pre_open_stamp {
            if pre != sb {
                return None;
            }
        }
        let meta_after = file.metadata().ok()?;
        let sa = FileStamp::from_metadata(&meta_after)?;
        if sb != sa {
            return None;
        }
        let path_after = fs::metadata(path).ok()?;
        let sp = FileStamp::from_metadata(&path_after)?;
        if sb == sp {
            Some(sb)
        } else {
            None
        }
    });

    Some((digest, verified_stamp))
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

        #[cfg(unix)]
        let stamp1 = {
            let (stamp1, cached1) = cache.peek(&file).expect("entry cached");
            assert_eq!(cached1, h1);
            stamp1
        };
        #[cfg(not(unix))]
        assert!(
            cache.peek(&file).is_none(),
            "non-Unix platforms without ctime/inode stamps must not cache metadata entries"
        );

        // Second lookup with unchanged file returns the same hash.
        let h1_again = cache.get_or_hash(&file).expect("hash v1 again");
        assert_eq!(h1, h1_again);
        #[cfg(unix)]
        assert_eq!(cache.peek(&file).unwrap().0, stamp1);

        // Modify file with different size/content so FileStamp changes immediately.
        fs::write(&file, b"export const x = 222;\n").unwrap();
        let h2 = cache.get_or_hash(&file).expect("hash v2");
        assert_ne!(h1, h2);
        assert_eq!(h2, blake3::hash(b"export const x = 222;\n"));

        #[cfg(unix)]
        {
            let (stamp2, cached2) = cache.peek(&file).expect("updated entry");
            assert_ne!(stamp1, stamp2);
            assert_eq!(cached2, h2);
        }
        #[cfg(not(unix))]
        assert!(cache.peek(&file).is_none());
    }

    #[test]
    fn non_existent_or_directory_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = DigestCache::new();
        assert!(cache.get_or_hash(dir.path()).is_none());
        assert!(cache.get_or_hash(&dir.path().join("missing.ts")).is_none());
    }

    #[test]
    fn read_and_hash_verified_rejects_mismatched_pre_open_stamp() {
        let dir = TempDir::new().unwrap();
        let file_a = dir.path().join("target_a.ts");
        let file_b = dir.path().join("target_b.ts");
        fs::write(&file_a, b"content-a").unwrap();
        fs::write(&file_b, b"content-b-different").unwrap();

        let stamp_a = FileStamp::from_target_metadata(&file_a).unwrap();
        let (digest_b, verified) = read_and_hash_verified(&file_b, stamp_a).unwrap();
        assert_eq!(digest_b, blake3::hash(b"content-b-different"));
        assert!(
            verified.is_none(),
            "mismatched pre-open stamp (e.g. symlink retargeted from A to B) must not produce a cacheable stamp"
        );
    }

    #[test]
    fn invalidates_on_same_size_in_place_rewrite_with_restored_mtime() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("same_size.ts");
        fs::write(&file, b"aaaa").unwrap();
        let original_mtime = fs::metadata(&file).unwrap().modified().unwrap();

        let cache = DigestCache::new();
        let h1 = cache.get_or_hash(&file).expect("hash v1");
        assert_eq!(h1, blake3::hash(b"aaaa"));

        // Immediate same-size rewrite with NO sleep (within the same clock tick)
        // and restored mtime must still re-read and return the updated digest.
        fs::write(&file, b"bbbb").unwrap();
        let f = fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(original_mtime))
            .unwrap();
        drop(f);

        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            original_mtime
        );

        let h2 = cache.get_or_hash(&file).expect("hash v2");
        assert_eq!(h2, blake3::hash(b"bbbb"));
        assert_ne!(
            h1, h2,
            "same-size rewrite with restored mtime must not return stale digest"
        );
    }
}
