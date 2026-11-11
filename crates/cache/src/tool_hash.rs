//! Hash a tool binary by path. Used as part of the weak fingerprint so a
//! tsc / cargo / rustc upgrade invalidates caches.
//!
//! Resolves symlinks via `std::fs::canonicalize` and memoizes the resulting
//! BLAKE3 hex digest in-process keyed by `(canonical_path, FileStamp)` so
//! repeated lookups across hundreds of packages do not re-read `node` / `tsc`.

use crate::digest_cache::FileStamp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

type ToolHashMap = HashMap<PathBuf, (FileStamp, String)>;

fn tool_hash_cache() -> &'static RwLock<ToolHashMap> {
    static CACHE: OnceLock<RwLock<ToolHashMap>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Hash the bytes of the binary at `path`, following symlinks and memoizing
/// the result in-process keyed by [`FileStamp`].
///
/// Returns:
///   - `Some(hex)` when the file exists and is readable.
///   - `None` when the file is missing or unreadable. Callers should fall back
///     to hashing only the path string.
pub fn hash_tool_binary(path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = std::fs::metadata(&canonical).ok()?;
    if !meta.is_file() {
        return None;
    }
    let stamp = FileStamp::from_metadata(&meta);

    if let Ok(guard) = tool_hash_cache().read() {
        if let Some((cached_stamp, cached_hex)) = guard.get(&canonical) {
            if *cached_stamp == stamp {
                return Some(cached_hex.clone());
            }
        }
    }

    let bytes = std::fs::read(&canonical).ok()?;
    let hex = blake3::hash(&bytes).to_hex().to_string();

    if let Ok(mut guard) = tool_hash_cache().write() {
        guard.insert(canonical, (stamp, hex.clone()));
    }

    Some(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn hashes_file_contents() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("tool");
        std::fs::write(&p, b"#!/bin/sh\necho hi\n").unwrap();
        let h = hash_tool_binary(&p).unwrap();
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn missing_returns_none() {
        assert!(hash_tool_binary(Path::new("/nope/nope/nope")).is_none());
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"a").unwrap();
        let b = dir.path().join("b");
        std::fs::write(&b, b"b").unwrap();
        assert_ne!(hash_tool_binary(&a).unwrap(), hash_tool_binary(&b).unwrap());
    }

    #[test]
    fn memoizes_and_invalidates_when_binary_changes() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("tsc");
        std::fs::write(&p, b"tsc-v1").unwrap();
        let h1 = hash_tool_binary(&p).unwrap();
        let h1_cached = hash_tool_binary(&p).unwrap();
        assert_eq!(h1, h1_cached);

        // Overwrite with different content/size so FileStamp changes.
        std::fs::write(&p, b"tsc-v2-updated").unwrap();
        let h2 = hash_tool_binary(&p).unwrap();
        assert_ne!(h1, h2);
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlink_and_invalidates_on_target_update() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("tsc-real");
        let link = dir.path().join("tsc-link");
        std::fs::write(&target, b"real-v1").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let h1 = hash_tool_binary(&link).unwrap();
        assert_eq!(h1, hash_tool_binary(&target).unwrap());

        std::fs::write(&target, b"real-v2-longer").unwrap();
        let h2 = hash_tool_binary(&link).unwrap();
        assert_ne!(h1, h2);
    }
}
