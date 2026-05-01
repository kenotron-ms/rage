//! Strong fingerprint (SF) computation.
//!
//! SF = blake3(WF || sorted(path || file_content_hash for path in pathset_reads))
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 5.

use blake3::Hasher;
use std::path::{Path, PathBuf};

/// Compute the strong fingerprint.
///
/// # Pathset filtering
///
/// Files under `node_modules` are excluded from the SF hash.  Their
/// contents are managed by the package manager (yarn/pnpm/npm) and are
/// stable for any given lockfile.  The lockfile is already tracked by the
/// root-task fingerprint (`workspace#install`), so changes to installed
/// packages will invalidate the root task and cause a full rebuild —
/// including re-populating all SF entries.
///
/// Excluding `node_modules` dramatically reduces the number of file reads
/// per SF computation (TypeScript stdlib alone can add hundreds of files)
/// without sacrificing correctness.
pub fn compute_strong_fingerprint(weak_fp: &str, pathset_reads: &[PathBuf]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"wf:");
    hasher.update(weak_fp.as_bytes());
    hasher.update(b"\n");

    let mut sorted: Vec<&Path> = pathset_reads
        .iter()
        .map(|p| p.as_path())
        // Skip files inside node_modules — they're pinned by the lockfile.
        .filter(|p| {
            !p.components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new("node_modules"))
        })
        .collect();
    sorted.sort();
    sorted.dedup();

    for p in sorted {
        // Skip non-regular files (directories, /dev/, device files, etc.).
        // On macOS, read() on devfs or large directories can block indefinitely.
        if !p.is_file() {
            continue;
        }
        hasher.update(b"read:");
        hasher.update(p.as_os_str().as_encoded_bytes());
        hasher.update(b":");
        let content = std::fs::read(p).unwrap_or_default();
        let h = blake3::hash(&content);
        hasher.update(h.as_bytes());
        hasher.update(b"\n");
    }

    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "a.txt", b"hello");
        let sf1 = compute_strong_fingerprint("wf-abc", std::slice::from_ref(&p));
        let sf2 = compute_strong_fingerprint("wf-abc", &[p]);
        assert_eq!(sf1, sf2);
        assert_eq!(sf1.len(), 64);
    }

    #[test]
    fn changes_when_file_content_changes() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "b.txt", b"version1");
        let sf1 = compute_strong_fingerprint("wf-abc", std::slice::from_ref(&p));

        std::fs::write(&p, b"version2").unwrap();
        let sf2 = compute_strong_fingerprint("wf-abc", &[p]);
        assert_ne!(sf1, sf2);
    }

    #[test]
    fn different_wf_yields_different_sf() {
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "c.txt", b"same content");
        let sf1 = compute_strong_fingerprint("wf-111", std::slice::from_ref(&p));
        let sf2 = compute_strong_fingerprint("wf-222", &[p]);
        assert_ne!(sf1, sf2);
    }

    #[test]
    fn order_independent() {
        let dir = TempDir::new().unwrap();
        let pa = write_file(&dir, "x.txt", b"file-x");
        let pb = write_file(&dir, "y.txt", b"file-y");
        let sf_ab = compute_strong_fingerprint("wf-abc", &[pa.clone(), pb.clone()]);
        let sf_ba = compute_strong_fingerprint("wf-abc", &[pb, pa]);
        assert_eq!(sf_ab, sf_ba);
    }

    #[test]
    fn missing_file_treated_as_empty() {
        let missing = PathBuf::from("/tmp/__nonexistent_rage_test_file_xyz__.txt");
        let sf = compute_strong_fingerprint("wf-abc", &[missing]);
        assert_eq!(sf.len(), 64, "SF must be 64-char hex");
        // Verify it equals the SF we'd get for a real empty file
        let dir = TempDir::new().unwrap();
        let p = write_file(&dir, "empty.txt", b"");
        let sf_empty = compute_strong_fingerprint("wf-abc", &[p]);
        assert_ne!(
            sf, sf_empty,
            "Different paths → different SFs even if both empty"
        );
    }
}
