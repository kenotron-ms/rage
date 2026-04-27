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

/// Encode `bytes` using standard Base64 (RFC 4648 §4).
/// Uses no external crate — only the standard alphabet is needed here.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
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
    if s.len() % 4 != 0 {
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

        assert!(store.contains_raw_key(&key), "key should exist in CAS after store");
    }
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
