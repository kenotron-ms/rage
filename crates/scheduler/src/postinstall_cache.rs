//! Caching layer for package postinstall scripts.
//!
//! v1 strategy: snapshot package directory before + after running script,
//! compute delta of new/modified files, store in CAS keyed by
//! `blake3(tarball_integrity + ":" + platform + ":" + node_version)`.
//! Restore on cache hit by writing the delta files back.
//! Deletions are out of scope for v1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Return only the files that are NEW in `after` or whose content DIFFERS
/// from `before`. Files only present in `before` (deletions) are NOT captured.
pub fn compute_delta(
    before: &HashMap<PathBuf, Vec<u8>>,
    after: &HashMap<PathBuf, Vec<u8>>,
) -> HashMap<PathBuf, Vec<u8>> {
    let mut delta = HashMap::new();
    for (path, after_bytes) in after {
        match before.get(path) {
            Some(before_bytes) if before_bytes == after_bytes => continue,
            _ => {
                delta.insert(path.clone(), after_bytes.clone());
            }
        }
    }
    delta
}

#[cfg(test)]
mod delta_tests {
    use super::*;

    fn map(pairs: &[(&str, &[u8])]) -> HashMap<PathBuf, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.to_vec()))
            .collect()
    }

    #[test]
    fn delta_picks_up_new_and_changed() {
        let before = map(&[("a", b"X"), ("b", b"Y")]);
        let after = map(&[("a", b"X"), ("b", b"Z"), ("c", b"W")]);
        let delta = compute_delta(&before, &after);
        assert_eq!(delta.len(), 2, "expected 2 entries in delta, got {}", delta.len());
        assert_eq!(delta.get(&PathBuf::from("b")).map(|v| v.as_slice()), Some(b"Z".as_slice()));
        assert_eq!(delta.get(&PathBuf::from("c")).map(|v| v.as_slice()), Some(b"W".as_slice()));
    }

    #[test]
    fn delta_empty_when_unchanged() {
        let before = map(&[("a", b"X")]);
        let after = map(&[("a", b"X")]);
        let delta = compute_delta(&before, &after);
        assert!(delta.is_empty(), "expected empty delta, got {delta:?}");
    }

    #[test]
    fn delta_ignores_deletions() {
        let before = map(&[("a", b"X"), ("b", b"Y")]);
        let after = map(&[("a", b"X")]);
        let delta = compute_delta(&before, &after);
        assert!(delta.is_empty(), "expected empty delta (deletions not tracked in v1), got {delta:?}");
    }
}

/// Compute the CAS key under which a postinstall task's outputs are stored.
/// Inputs: tarball integrity + platform + node version. Each axis breaks the
/// cache so darwin-arm64+node20 cannot be restored on linux-x86_64+node18.
pub fn postinstall_cas_key(task: &plugin::PostinstallTask) -> [u8; 32] {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let node_version = read_node_version();
    let input = format!("{}:{}:{}", task.tarball_integrity, platform, node_version);
    blake3::hash(input.as_bytes()).into()
}

fn read_node_version() -> String {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use std::path::PathBuf;

    fn task(integrity: &str) -> plugin::PostinstallTask {
        plugin::PostinstallTask {
            package_name: "esbuild".to_string(),
            version: "0.21.5".to_string(),
            tarball_integrity: integrity.to_string(),
            script: "node install.js".to_string(),
            cwd: PathBuf::from("/tmp/p"),
        }
    }

    #[test]
    fn same_inputs_produce_same_key() {
        let t = task("sha512-abc");
        let k1 = postinstall_cas_key(&t);
        let k2 = postinstall_cas_key(&t);
        assert_eq!(k1, k2, "same task should produce same CAS key");
    }

    #[test]
    fn different_integrity_produces_different_key() {
        let k1 = postinstall_cas_key(&task("sha512-abc"));
        let k2 = postinstall_cas_key(&task("sha512-xyz"));
        assert_ne!(k1, k2, "different integrity strings should produce different CAS keys");
    }
}

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
