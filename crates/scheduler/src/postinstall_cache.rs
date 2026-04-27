//! Caching layer for package postinstall scripts.
//!
//! v1 strategy: snapshot package directory before + after running script,
//! compute delta of new/modified files, store in CAS keyed by
//! `blake3(tarball_integrity + ":" + platform + ":" + node_version)`.
//! Restore on cache hit by writing the delta files back.
//! Deletions are out of scope for v1.

use plugin::PostinstallTask;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Stub to keep the `PostinstallTask` import live until it is used by
/// future functions in this module.
#[allow(dead_code)]
const _UNUSED_PIN: fn(&PostinstallTask) = |_| {};

/// Walk `dir` recursively and return a map of relative path → file contents
/// for every regular file found.
///
/// If `dir` does not exist, an empty map is returned rather than an error.
/// Symlinks are skipped. Individual file-read errors are silently ignored so
/// that a single unreadable file does not abort the snapshot.
pub fn snapshot_dir(dir: &Path) -> std::io::Result<HashMap<PathBuf, Vec<u8>>> {
    if !dir.exists() {
        return Ok(HashMap::new());
    }

    let mut map = HashMap::new();

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip symlinks and non-regular-files
        let file_type = entry.file_type();
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }

        let abs = entry.path();

        let rel = match abs.strip_prefix(dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };

        let bytes = match std::fs::read(abs) {
            Ok(b) => b,
            Err(_) => continue,
        };

        map.insert(rel, bytes);
    }

    Ok(map)
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = snapshot_dir(&dir.path().join("nope")).unwrap();
        assert!(result.is_empty(), "expected empty map for missing dir, got {result:?}");
    }

    #[test]
    fn snapshot_captures_all_files_with_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Write a.txt at the top level
        std::fs::write(root.join("a.txt"), b"hello").unwrap();

        // Write nested/b.txt
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested").join("b.txt"), b"world").unwrap();

        let result = snapshot_dir(root).unwrap();

        assert_eq!(result.len(), 2, "expected 2 files, got {}", result.len());

        assert_eq!(
            result.get(&PathBuf::from("a.txt")).map(|b| b.as_slice()),
            Some(b"hello".as_slice()),
            "a.txt contents mismatch"
        );
        assert_eq!(
            result.get(&PathBuf::from("nested/b.txt")).map(|b| b.as_slice()),
            Some(b"world".as_slice()),
            "nested/b.txt contents mismatch"
        );
    }
}
