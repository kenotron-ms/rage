//! Caching layer for package postinstall scripts.
//!
//! v1 strategy: snapshot package directory before + after running script,
//! compute delta of new/modified files, store in CAS keyed by
//! `blake3(tarball_integrity + ":" + platform + ":" + node_version)`.
//! Restore on cache hit by writing the delta files back.
//! Deletions are out of scope for v1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        assert_eq!(
            delta.len(),
            2,
            "expected 2 entries in delta, got {}",
            delta.len()
        );
        assert_eq!(
            delta.get(&PathBuf::from("b")).map(|v| v.as_slice()),
            Some(b"Z".as_slice())
        );
        assert_eq!(
            delta.get(&PathBuf::from("c")).map(|v| v.as_slice()),
            Some(b"W".as_slice())
        );
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
        assert!(
            delta.is_empty(),
            "expected empty delta (deletions not tracked in v1), got {delta:?}"
        );
    }
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

/// Encode `bytes` using standard Base64 (RFC 4648 §4).
/// Uses no external crate — only the standard alphabet is needed here.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        match chunk {
            [a, b, c] => {
                out.push(ALPHA[(a >> 2) as usize] as char);
                out.push(ALPHA[((a & 0x3) << 4 | b >> 4) as usize] as char);
                out.push(ALPHA[((b & 0xf) << 2 | c >> 6) as usize] as char);
                out.push(ALPHA[(c & 0x3f) as usize] as char);
            }
            [a, b] => {
                out.push(ALPHA[(a >> 2) as usize] as char);
                out.push(ALPHA[((a & 0x3) << 4 | b >> 4) as usize] as char);
                out.push(ALPHA[((b & 0xf) << 2) as usize] as char);
                out.push('=');
            }
            [a] => {
                out.push(ALPHA[(a >> 2) as usize] as char);
                out.push(ALPHA[((a & 0x3) << 4) as usize] as char);
                out.push('=');
                out.push('=');
            }
            _ => {}
        }
    }
    out
}

/// Decode a standard Base64 string.
/// Returns `None` on bad input (unknown character or length not a multiple of 4).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(4) {
        return None;
    }

    fn val(c: char) -> Option<u8> {
        match c {
            'A'..='Z' => Some(c as u8 - b'A'),
            'a'..='z' => Some(c as u8 - b'a' + 26),
            '0'..='9' => Some(c as u8 - b'0' + 52),
            '+' => Some(62),
            '/' => Some(63),
            _ => None,
        }
    }

    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let chars: Vec<char> = s.chars().collect();
    for chunk in chars.chunks(4) {
        let c0 = val(chunk[0])?;
        let c1 = val(chunk[1])?;
        if chunk[2] == '=' && chunk[3] == '=' {
            // 1-byte group
            out.push((c0 << 2) | (c1 >> 4));
        } else if chunk[3] == '=' {
            // 2-byte group
            let c2 = val(chunk[2])?;
            out.push((c0 << 2) | (c1 >> 4));
            out.push(((c1 & 0xf) << 4) | (c2 >> 2));
        } else {
            // 3-byte group (full)
            let c2 = val(chunk[2])?;
            let c3 = val(chunk[3])?;
            out.push((c0 << 2) | (c1 >> 4));
            out.push(((c1 & 0xf) << 4) | (c2 >> 2));
            out.push(((c2 & 0x3) << 6) | c3);
        }
    }
    Some(out)
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

/// Look up `key` in CAS. If absent, return `Ok(false)`. Otherwise deserialize
/// JSON delta and write each entry under `target_dir`, creating parent dirs.
pub fn restore_postinstall_outputs(
    key: &[u8; 32],
    target_dir: &Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    let bytes = match store.get_bytes_by_raw_key(key)? {
        Some(b) => b,
        None => return Ok(false),
    };
    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    for (rel_str, b64) in map {
        let bytes = base64_decode(&b64)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad base64"))?;
        let dest = target_dir.join(&rel_str);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &bytes)?;
    }
    Ok(true)
}

/// Serialize `delta` (path → bytes) as a JSON `BTreeMap<String, String>` where
/// values are Base64-encoded, then store it in `store` under the given `key`.
pub fn store_postinstall_outputs(
    key: &[u8; 32],
    delta: &HashMap<PathBuf, Vec<u8>>,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<()> {
    use std::collections::BTreeMap;

    let map: BTreeMap<String, String> = delta
        .iter()
        .map(|(path, bytes)| {
            let path_str = path.to_string_lossy().into_owned();
            let encoded = base64_encode(bytes);
            (path_str, encoded)
        })
        .collect();

    let json = serde_json::to_vec(&map)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    store.put_bytes_keyed(*key, &json)
}

#[cfg(test)]
mod store_tests {
    use super::*;

    #[test]
    fn base64_roundtrip_handles_binary() {
        let cases: &[&[u8]] = &[b"", b"a", b"ab", b"abc", b"abcd", b"\x00\xff\x10"];
        for case in cases {
            let encoded = base64_encode(case);
            let decoded = base64_decode(&encoded).expect("decode should succeed");
            assert_eq!(&decoded, case, "roundtrip failed for {:?}", case);
        }
    }

    #[test]
    fn store_writes_to_cas_under_key() {
        let dir = tempfile::tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(dir.path());

        let mut delta = HashMap::new();
        delta.insert(PathBuf::from("install.js.lock"), b"binary\xff".to_vec());

        let key = [7u8; 32];
        store_postinstall_outputs(&key, &delta, &store).unwrap();

        assert!(
            store.contains_raw_key(&key),
            "key should exist in CAS after store"
        );
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = snapshot_dir(&dir.path().join("nope")).unwrap();
        assert!(
            result.is_empty(),
            "expected empty map for missing dir, got {result:?}"
        );
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
            result
                .get(&PathBuf::from("nested/b.txt"))
                .map(|b| b.as_slice()),
            Some(b"world".as_slice()),
            "nested/b.txt contents mismatch"
        );
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;

    #[test]
    fn restore_returns_false_when_key_missing() {
        let dir = tempfile::tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(dir.path());
        let target = tempfile::tempdir().unwrap();

        let key = [9u8; 32];
        let result = restore_postinstall_outputs(&key, target.path(), &store).unwrap();
        assert!(!result, "should return false for missing key");
    }

    #[test]
    fn store_then_restore_recreates_files() {
        let store_dir = tempfile::tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(store_dir.path());
        let target = tempfile::tempdir().unwrap();

        let mut delta = HashMap::new();
        delta.insert(PathBuf::from("bin/foo.node"), b"\x7fELF...".to_vec());
        delta.insert(PathBuf::from("install.flag"), b"ok".to_vec());

        let key = [42u8; 32];
        store_postinstall_outputs(&key, &delta, &store).unwrap();

        let result = restore_postinstall_outputs(&key, target.path(), &store).unwrap();
        assert!(result, "should return true when key exists");

        assert_eq!(
            std::fs::read(target.path().join("bin/foo.node")).unwrap(),
            b"\x7fELF...",
            "bin/foo.node content mismatch"
        );
        assert_eq!(
            std::fs::read(target.path().join("install.flag")).unwrap(),
            b"ok",
            "install.flag content mismatch"
        );
    }
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    #[test]
    fn full_postinstall_roundtrip_through_cas() {
        // 1. Create tempdir simulating node_modules/fake-pkg/ and write package.json
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(pkg.path().join("package.json"), b"{}").unwrap();

        // 2. Build PostinstallTask
        let task = plugin::PostinstallTask {
            package_name: "fake-pkg".to_string(),
            version: "1.0.0".to_string(),
            tarball_integrity: "sha512-fake".to_string(),
            script: "echo built > built.flag && mkdir -p bin && printf 'bin' > bin/native"
                .to_string(),
            cwd: pkg.path().to_path_buf(),
        };

        // 3. Snapshot before; assert before.len() == 1 (only package.json)
        let before = snapshot_dir(pkg.path()).unwrap();
        assert_eq!(
            before.len(),
            1,
            "expected 1 file before script, got {}",
            before.len()
        );

        // 4. Run postinstall and assert it returns true
        let ran = run_postinstall(&task).unwrap();
        assert!(
            ran,
            "run_postinstall should return true for a successful script"
        );

        // 5. Snapshot after; compute delta; assert delta contains built.flag AND bin/native
        let after = snapshot_dir(pkg.path()).unwrap();
        let delta = compute_delta(&before, &after);
        assert!(
            delta.contains_key(&PathBuf::from("built.flag")),
            "delta should contain built.flag"
        );
        assert!(
            delta.contains_key(&PathBuf::from("bin/native")),
            "delta should contain bin/native"
        );

        // 6. Create fresh CAS tempdir; instantiate store; compute key; store outputs
        let cas_dir = tempfile::tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(cas_dir.path());
        let key = postinstall_cas_key(&task);
        store_postinstall_outputs(&key, &delta, &store).unwrap();

        // 7. Delete the new files to simulate node_modules clean
        std::fs::remove_file(pkg.path().join("built.flag")).unwrap();
        std::fs::remove_dir_all(pkg.path().join("bin")).unwrap();

        // 8. Restore from CAS and assert it returns true
        let restored = restore_postinstall_outputs(&key, &task.cwd, &store).unwrap();
        assert!(
            restored,
            "restore_postinstall_outputs should return true for a stored key"
        );

        // 9. Assert content of restored files
        assert_eq!(
            std::fs::read(pkg.path().join("built.flag")).unwrap(),
            b"built\n",
            "built.flag content mismatch"
        );
        assert_eq!(
            std::fs::read(pkg.path().join("bin/native")).unwrap(),
            b"bin",
            "bin/native content mismatch"
        );
    }
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
            .find(|e| e.rel_path == PathBuf::from("link.txt"))
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
