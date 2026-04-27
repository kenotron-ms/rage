//! Caching layer for package postinstall scripts.
//!
//! v1 strategy: snapshot package directory before + after running script,
//! compute delta of new/modified files, store in CAS keyed by
//! `blake3(tarball_integrity + ":" + platform + ":" + node_version)`.
//! Restore on cache hit by writing the delta files back.
//! Deletions are out of scope for v1.


/// Whether a filesystem entry is a regular file or a symlink.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileKind {
    Regular,
    Symlink(std::path::PathBuf),
}

/// One entry in a postinstall manifest — describes a single file or symlink
/// relative to the package directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the package directory (e.g. `bin/esbuild`).
    pub rel_path: std::path::PathBuf,
    /// Blake3 hash of the file's contents. Zeroed (`[0u8; 32]`) for symlinks.
    pub content_hash: [u8; 32],
    /// Unix permission bits (`st_mode & 0o777`). Zero for symlinks.
    pub mode: u32,
    /// Whether this entry is a regular file or a symlink (with its target).
    pub kind: FileKind,
}

/// A postinstall manifest is a list of changed or new entries in the package directory.
pub type PostinstallManifest = Vec<ManifestEntry>;

/// Walk `dir` recursively, capturing every file and symlink into the CAS.
///
/// Returns a `PostinstallManifest` describing all entries found.
/// If `dir` does not exist, returns `Ok(Vec::new())`.
/// Directories and unreadable entries are silently skipped.
pub fn capture_dir(
    dir: &std::path::Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<PostinstallManifest> {
    use std::os::unix::fs::PermissionsExt;

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut manifest = PostinstallManifest::new();

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let file_type = entry.file_type();
        if file_type.is_dir() {
            continue;
        }
        let abs = entry.path();
        let rel = match abs.strip_prefix(dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            let target = match std::fs::read_link(abs) {
                Ok(t) => t,
                Err(_) => continue,
            };
            manifest.push(ManifestEntry {
                rel_path: rel,
                content_hash: [0u8; 32],
                mode: 0,
                kind: FileKind::Symlink(target),
            });
        } else if file_type.is_file() {
            let bytes = match std::fs::read(abs) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let content_hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
            if store.put_bytes_keyed(content_hash, &bytes).is_err() {
                continue;
            }
            let mode = match abs.metadata() {
                Ok(m) => m.permissions().mode() & 0o777,
                Err(_) => 0o644,
            };
            manifest.push(ManifestEntry {
                rel_path: rel,
                content_hash,
                mode,
                kind: FileKind::Regular,
            });
        }
    }
    Ok(manifest)
}

/// Return the subset of `after` whose entries are new or changed relative to `before`.
/// Deletions (paths only in `before`) are not returned.
pub fn diff_manifests(
    before: &[ManifestEntry],
    after: &[ManifestEntry],
) -> PostinstallManifest {
    let before_map: std::collections::HashMap<&std::path::PathBuf, (&[u8; 32], u32, &FileKind)> =
        before.iter().map(|e| (&e.rel_path, (&e.content_hash, e.mode, &e.kind))).collect();
    after
        .iter()
        .filter(|e| match before_map.get(&e.rel_path) {
            Some((hash, mode, kind)) => {
                *hash != &e.content_hash || *mode != e.mode || **kind != e.kind
            }
            None => true,
        })
        .cloned()
        .collect()
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
        assert_ne!(
            k1, k2,
            "different integrity strings should produce different CAS keys"
        );
    }
}





/// Run `task.script` via `sh -c` in `task.cwd`. Returns `Ok(true)` when the
/// script exits 0, `Ok(false)` for any other exit. Stdout/stderr are inherited.
pub fn run_postinstall(task: &plugin::PostinstallTask) -> std::io::Result<bool> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&task.script)
        .current_dir(&task.cwd)
        .status()?;
    Ok(status.success())
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use std::path::Path;

    fn task_with_script(cwd: &Path, script: &str) -> plugin::PostinstallTask {
        plugin::PostinstallTask {
            package_name: "p".to_string(),
            version: "1.0.0".to_string(),
            tarball_integrity: "sha512-x".to_string(),
            script: script.to_string(),
            cwd: cwd.to_path_buf(),
        }
    }

    #[test]
    fn run_succeeds_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let task = task_with_script(dir.path(), "true");
        let result = run_postinstall(&task).unwrap();
        assert!(result, "script 'true' should return Ok(true)");
    }

    #[test]
    fn run_failure_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let task = task_with_script(dir.path(), "exit 1");
        let result = run_postinstall(&task).unwrap();
        assert!(!result, "script 'exit 1' should return Ok(false)");
    }

    #[test]
    fn run_executes_in_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let task = task_with_script(dir.path(), "touch ran.txt");
        run_postinstall(&task).unwrap();
        assert!(
            dir.path().join("ran.txt").exists(),
            "ran.txt should exist after 'touch ran.txt' script"
        );
    }
}



/// Serialize `delta` as JSON and store it in `store` under the postinstall `key`.
///
/// Returns `Ok(false)` without writing anything when `delta` is empty — this prevents
/// spurious cache hits for postinstall scripts that produce no output files.
/// Returns `Ok(true)` after successfully writing the manifest JSON to CAS.
/// Individual file bytes must already be in CAS from a prior `capture_dir` call.
pub fn store_manifest(
    key: &[u8; 32],
    delta: &PostinstallManifest,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    if delta.is_empty() {
        return Ok(false);
    }
    let json = serde_json::to_vec(delta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    store.put_bytes_keyed(*key, &json)?;
    Ok(true)
}

/// Look up `key` in CAS. If absent, return `Ok(false)`. Otherwise deserialize the
/// `PostinstallManifest` and restore all entries under `target_dir`, using hardlinks
/// for regular files (with cross-device copy fallback) and symlinks for symlinks.
/// Unix permission bits are restored on regular files.
pub fn restore_manifest(
    key: &[u8; 32],
    target_dir: &std::path::Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let bytes = match store.get_bytes_by_raw_key(key)? {
        Some(b) => b,
        None => return Ok(false),
    };
    let manifest: PostinstallManifest = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    for entry in &manifest {
        let dest = target_dir.join(&entry.rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match &entry.kind {
            FileKind::Regular => {
                let cas_path = store.cas_file_path(&entry.content_hash);
                let _ = std::fs::remove_file(&dest);
                match std::fs::hard_link(&cas_path, &dest) {
                    Ok(()) => {}
                    Err(_) => {
                        std::fs::copy(&cas_path, &dest)?;
                    }
                }
                std::fs::set_permissions(
                    &dest,
                    std::fs::Permissions::from_mode(entry.mode),
                )?;
            }
            FileKind::Symlink(target) => {
                let _ = std::fs::remove_file(&dest);
                std::os::unix::fs::symlink(target, &dest)?;
            }
        }
    }
    Ok(true)
}











#[cfg(test)]
mod manifest_type_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn regular_entry_serde_roundtrip() {
        let entry = ManifestEntry {
            rel_path: PathBuf::from("bin/foo.node"),
            content_hash: [1u8; 32],
            mode: 0o755,
            kind: FileKind::Regular,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let decoded: ManifestEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.rel_path, entry.rel_path);
        assert_eq!(decoded.content_hash, entry.content_hash);
        assert_eq!(decoded.mode, entry.mode);
        assert_eq!(decoded.kind, FileKind::Regular);
    }

    #[test]
    fn symlink_entry_serde_roundtrip() {
        let target = PathBuf::from("../../real/path");
        let entry = ManifestEntry {
            rel_path: PathBuf::from("link"),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(target.clone()),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let decoded: ManifestEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.content_hash, [0u8; 32]);
        match decoded.kind {
            FileKind::Symlink(t) => assert_eq!(t, target),
            FileKind::Regular => panic!("expected Symlink variant"),
        }
    }

    #[test]
    fn empty_manifest_serializes_as_empty_array() {
        let manifest: PostinstallManifest = vec![];
        let json = serde_json::to_string(&manifest).expect("serialize");
        let decoded: PostinstallManifest = serde_json::from_str(&json).expect("deserialize");
        assert!(decoded.is_empty(), "expected empty manifest after roundtrip");
    }
}

#[cfg(test)]
mod capture_dir_tests {
    use super::*;
    use std::path::PathBuf;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn nonexistent_dir_returns_empty() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let nonexistent = store_dir.path().join("does_not_exist");
        let manifest = capture_dir(&nonexistent, &store).unwrap();
        assert!(manifest.is_empty(), "expected empty manifest for nonexistent dir, got {manifest:?}");
    }

    #[test]
    fn regular_file_produces_correct_hash_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let content = b"hello world";
        let file_path = pkg_dir.path().join("hello.txt");
        std::fs::write(&file_path, content).unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1, "expected 1 entry, got {}", manifest.len());

        let entry = &manifest[0];
        assert_eq!(entry.rel_path, PathBuf::from("hello.txt"));
        assert_eq!(entry.kind, FileKind::Regular);
        assert_eq!(entry.mode, 0o644);
        let expected_hash: [u8; 32] = *blake3::hash(content).as_bytes();
        assert_eq!(entry.content_hash, expected_hash, "content_hash mismatch");
        assert!(
            store.cas_file_path(&entry.content_hash).is_file(),
            "CAS file should exist after capture"
        );
    }

    #[test]
    fn executable_mode_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let file_path = pkg_dir.path().join("runner");
        std::fs::write(&file_path, b"#!/bin/sh\necho hi").unwrap();
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1, "expected 1 entry");
        assert_eq!(manifest[0].mode, 0o755, "mode should be 0o755");
    }

    #[test]
    fn symlink_entry_has_zero_hash_and_correct_target() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        std::fs::write(pkg_dir.path().join("real.txt"), b"data").unwrap();
        std::os::unix::fs::symlink("real.txt", pkg_dir.path().join("link.txt")).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();

        let link_entry = manifest
            .iter()
            .find(|e| e.rel_path == std::path::Path::new("link.txt"))
            .expect("link.txt entry not found in manifest");

        assert_eq!(link_entry.content_hash, [0u8; 32], "symlink should have zeroed hash");
        assert_eq!(link_entry.mode, 0, "symlink mode should be 0");
        match &link_entry.kind {
            FileKind::Symlink(target) => {
                assert_eq!(target, &PathBuf::from("real.txt"), "symlink target mismatch");
            }
            FileKind::Regular => panic!("expected Symlink variant, got Regular"),
        }
    }

    #[test]
    fn nested_file_rel_path_is_relative() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        std::fs::create_dir_all(pkg_dir.path().join("bin")).unwrap();
        std::fs::write(pkg_dir.path().join("bin/esbuild"), b"binary").unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1, "expected 1 entry, got {}", manifest.len());
        assert_eq!(
            manifest[0].rel_path,
            PathBuf::from("bin/esbuild"),
            "rel_path should be bin/esbuild"
        );
    }
}

#[cfg(test)]
mod diff_manifests_tests {
    use super::*;
    use std::path::PathBuf;

    fn reg(path: &str, hash: [u8; 32], mode: u32) -> ManifestEntry {
        ManifestEntry {
            rel_path: PathBuf::from(path),
            content_hash: hash,
            mode,
            kind: FileKind::Regular,
        }
    }

    fn lnk(path: &str, target: &str) -> ManifestEntry {
        ManifestEntry {
            rel_path: PathBuf::from(path),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(PathBuf::from(target)),
        }
    }

    #[test]
    fn both_empty_yields_empty() {
        let delta = diff_manifests(&[], &[]);
        assert!(delta.is_empty(), "expected empty delta for two empty manifests");
    }

    #[test]
    fn new_file_in_after_is_included() {
        let after = vec![reg("a.txt", [1u8; 32], 0o644)];
        let delta = diff_manifests(&[], &after);
        assert_eq!(delta.len(), 1, "expected 1 entry in delta");
        assert_eq!(delta[0].rel_path, PathBuf::from("a.txt"));
    }

    #[test]
    fn unchanged_file_excluded() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let after = vec![reg("a.txt", [1u8; 32], 0o644)];
        let delta = diff_manifests(&before, &after);
        assert!(delta.is_empty(), "expected empty delta for unchanged file");
    }

    #[test]
    fn changed_hash_included() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let after = vec![reg("a.txt", [2u8; 32], 0o644)];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "expected 1 entry in delta");
        assert_eq!(delta[0].content_hash, [2u8; 32]);
    }

    #[test]
    fn changed_mode_included() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let after = vec![reg("a.txt", [1u8; 32], 0o755)];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "expected 1 entry in delta");
        assert_eq!(delta[0].mode, 0o755);
    }

    #[test]
    fn deletion_not_tracked() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let delta = diff_manifests(&before, &[]);
        assert!(delta.is_empty(), "expected empty delta (deletions not tracked)");
    }

    #[test]
    fn changed_symlink_target_included() {
        let before = vec![lnk("link", "old-target")];
        let after = vec![lnk("link", "new-target")];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "expected 1 entry in delta");
        match &delta[0].kind {
            FileKind::Symlink(target) => {
                assert_eq!(*target, PathBuf::from("new-target"));
            }
            FileKind::Regular => panic!("expected Symlink variant, got Regular"),
        }
    }

    #[test]
    fn unchanged_symlink_excluded() {
        let before = vec![lnk("link", "target")];
        let after = vec![lnk("link", "target")];
        let delta = diff_manifests(&before, &after);
        assert!(delta.is_empty(), "expected empty delta for unchanged symlink");
    }
}

#[cfg(test)]
mod store_manifest_tests {
    use super::*;
    use std::path::PathBuf;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn empty_delta_returns_false_and_writes_nothing() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [7u8; 32];

        let result = store_manifest(&key, &vec![], &store).unwrap();
        assert!(!result, "empty delta should return false");
        assert!(!store.contains_raw_key(&key), "nothing should be written for empty delta");
    }

    #[test]
    fn non_empty_delta_returns_true_and_cas_entry_readable() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [42u8; 32];

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("bin/tool"),
            content_hash: [1u8; 32],
            mode: 0o755,
            kind: FileKind::Regular,
        }];

        let result = store_manifest(&key, &delta, &store).unwrap();
        assert!(result, "non-empty delta should return true");
        assert!(store.contains_raw_key(&key), "key should exist in CAS after store");

        let bytes = store.get_bytes_by_raw_key(&key).unwrap().unwrap();
        let manifest: PostinstallManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(manifest.len(), 1, "expected 1 entry in deserialized manifest");
        assert_eq!(manifest[0].rel_path, PathBuf::from("bin/tool"));
        assert_eq!(manifest[0].mode, 0o755);
    }

    #[test]
    fn idempotent_write_same_key_does_not_error() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [99u8; 32];

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("lib/lib.so"),
            content_hash: [2u8; 32],
            mode: 0o644,
            kind: FileKind::Regular,
        }];

        let first = store_manifest(&key, &delta, &store).unwrap();
        assert!(first, "first write should return true");

        let second = store_manifest(&key, &delta, &store);
        assert!(second.is_ok(), "second write with same key should not error, got: {:?}", second);
    }
}

#[cfg(test)]
mod restore_manifest_tests {
    use super::*;
    use std::path::PathBuf;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn returns_false_when_key_not_in_cas() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let result = restore_manifest(&[0u8; 32], target.path(), &store).unwrap();
        assert!(!result, "should return false for missing key");
    }

    #[test]
    fn roundtrip_regular_file_content_and_mode() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let content = b"binary content";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("lib/foo.node"),
            content_hash: hash,
            mode: 0o755,
            kind: FileKind::Regular,
        }];
        store_manifest(&[10u8; 32], &delta, &store).unwrap();

        let result = restore_manifest(&[10u8; 32], target.path(), &store).unwrap();
        assert!(result, "should return true on cache hit");

        let restored_path = target.path().join("lib/foo.node");
        assert!(restored_path.is_file(), "lib/foo.node should be a regular file");
        assert_eq!(
            std::fs::read(&restored_path).unwrap(),
            content,
            "restored file content should match original"
        );
    }

    #[test]
    fn executable_permission_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let content = b"#!/bin/sh\necho hi";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("runner"),
            content_hash: hash,
            mode: 0o755,
            kind: FileKind::Regular,
        }];
        store_manifest(&[20u8; 32], &delta, &store).unwrap();

        restore_manifest(&[20u8; 32], target.path(), &store).unwrap();

        let restored = target.path().join("runner");
        let mode = restored.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "restored file should have mode 0o755");
    }

    #[test]
    fn symlink_restored_correctly() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("link"),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(PathBuf::from("real.txt")),
        }];
        store_manifest(&[30u8; 32], &delta, &store).unwrap();

        let result = restore_manifest(&[30u8; 32], target.path(), &store).unwrap();
        assert!(result, "should return true on cache hit");

        let link = target.path().join("link");
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "link should be a symlink"
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("real.txt"),
            "symlink target should be real.txt"
        );
    }

    #[test]
    fn parent_dirs_created_automatically() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let content = b"deep content";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        let delta = vec![ManifestEntry {
            rel_path: PathBuf::from("a/b/c/deep.txt"),
            content_hash: hash,
            mode: 0o644,
            kind: FileKind::Regular,
        }];
        store_manifest(&[40u8; 32], &delta, &store).unwrap();

        restore_manifest(&[40u8; 32], target.path(), &store).unwrap();

        assert!(
            target.path().join("a/b/c/deep.txt").is_file(),
            "a/b/c/deep.txt should exist after restore"
        );
    }
}
