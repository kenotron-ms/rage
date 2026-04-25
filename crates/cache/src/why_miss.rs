//! Snapshot storage for `rage why-miss` diagnostics.
//!
//! Every time a two-phase cache lookup is attempted, rage records a small
//! JSON snapshot of the WF inputs (command, tool hash, input file hashes,
//! env vars, dep ABI fingerprints).  Keeping the **last 2** snapshots per
//! task allows `rage why-miss` to diff them and identify what changed.
//!
//! Storage: `{cache_dir}/why/{slug}-{script}.snapshot.json`
//! where `slug = pkg.replace(\'@\', "_at_").replace(\'/\', "__")`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Snapshot types ───────────────────────────────────────────────────────

/// A single file that was declared as a WF input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InputEntry {
    /// Path relative to package root.
    pub path: PathBuf,
    /// blake3 hex of the file contents at snapshot time.
    pub hash: String,
}

/// A complete snapshot of the WF inputs for one task invocation.
///
/// Stored as the last-2-element JSON array at
/// `{cache_dir}/why/{slug}-{script}.snapshot.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyMissSnapshot {
    /// Unix timestamp (seconds) of the snapshot.
    pub timestamp: u64,
    /// Package name, e.g. `@lage-run/core`.
    pub pkg: String,
    /// Script name, e.g. `build`.
    pub script: String,
    /// Full shell command, e.g. `tsc`.
    pub command: String,
    /// Absolute path to the tool binary (first token of command, resolved).
    pub tool_path: String,
    /// blake3 hex of the tool binary contents.
    pub tool_hash: String,
    /// Declared input files and their hashes (resolved from input globs).
    pub inputs: Vec<InputEntry>,
    /// Tracked env var (key, value) pairs.
    pub env: Vec<(String, String)>,
    /// Dep ABI fingerprints: (package_name, abi_hex).
    pub dep_abi_fps: Vec<(String, String)>,
}

// ─── Storage helpers ──────────────────────────────────────────────────────

/// Read the last two snapshots for `pkg` + `script`.
///
/// Returns `Some((older, newer))` when at least two are stored, else `None`.
pub fn read_snapshots(
    cache_dir: &Path,
    pkg: &str,
    script: &str,
) -> Option<(WhyMissSnapshot, WhyMissSnapshot)> {
    let path = snapshot_path(cache_dir, pkg, script);
    let raw = std::fs::read_to_string(&path).ok()?;
    let snaps: Vec<WhyMissSnapshot> = serde_json::from_str(&raw).ok()?;
    if snaps.len() < 2 {
        return None;
    }
    let n = snaps.len();
    Some((snaps[n - 2].clone(), snaps[n - 1].clone()))
}

/// Append `snap` to the snapshot log for its task, keeping at most 2 entries.
///
/// Failures are silently ignored — snapshot writes must never break a build.
pub fn record_snapshot(cache_dir: &Path, snap: WhyMissSnapshot) {
    let _ = std::fs::create_dir_all(cache_dir.join("why"));
    let path = snapshot_path(cache_dir, &snap.pkg, &snap.script);

    let mut existing: Vec<WhyMissSnapshot> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    existing.push(snap);
    // Keep only the last 2 snapshots.
    if existing.len() > 2 {
        existing.drain(..existing.len() - 2);
    }

    if let Ok(json) = serde_json::to_string_pretty(&existing) {
        let _ = std::fs::write(&path, json);
    }
}

fn snapshot_path(dir: &Path, pkg: &str, script: &str) -> PathBuf {
    let slug = pkg.replace('@', "_at_").replace('/', "__");
    dir.join("why")
        .join(format!("{slug}-{script}.snapshot.json"))
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_snap(pkg: &str, script: &str, cmd: &str) -> WhyMissSnapshot {
        WhyMissSnapshot {
            timestamp: 0,
            pkg: pkg.to_string(),
            script: script.to_string(),
            command: cmd.to_string(),
            tool_path: "/usr/bin/tsc".to_string(),
            tool_hash: "abc".to_string(),
            inputs: vec![],
            env: vec![],
            dep_abi_fps: vec![],
        }
    }

    #[test]
    fn no_snapshots_returns_none() {
        let dir = tempdir().unwrap();
        assert!(read_snapshots(dir.path(), "@x/core", "build").is_none());
    }

    #[test]
    fn one_snapshot_returns_none() {
        let dir = tempdir().unwrap();
        record_snapshot(dir.path(), make_snap("@x/core", "build", "tsc"));
        assert!(read_snapshots(dir.path(), "@x/core", "build").is_none());
    }

    #[test]
    fn two_snapshots_returns_pair() {
        let dir = tempdir().unwrap();
        let s1 = make_snap("@x/core", "build", "tsc v1");
        let s2 = make_snap("@x/core", "build", "tsc v2");
        record_snapshot(dir.path(), s1.clone());
        record_snapshot(dir.path(), s2.clone());
        let (old, new) = read_snapshots(dir.path(), "@x/core", "build").unwrap();
        assert_eq!(old.command, "tsc v1");
        assert_eq!(new.command, "tsc v2");
    }

    #[test]
    fn keeps_at_most_two_snapshots() {
        let dir = tempdir().unwrap();
        for i in 0..5u32 {
            record_snapshot(
                dir.path(),
                make_snap("@x/core", "build", &format!("tsc v{i}")),
            );
        }
        let (old, new) = read_snapshots(dir.path(), "@x/core", "build").unwrap();
        assert_eq!(old.command, "tsc v3");
        assert_eq!(new.command, "tsc v4");
    }

    #[test]
    fn input_entries_roundtrip() {
        let dir = tempdir().unwrap();
        let mut snap = make_snap("@x/pkg", "typecheck", "tsc --noEmit");
        snap.inputs = vec![InputEntry {
            path: PathBuf::from("src/index.ts"),
            hash: "def456".to_string(),
        }];
        record_snapshot(dir.path(), snap);
        record_snapshot(dir.path(), make_snap("@x/pkg", "typecheck", "tsc --noEmit"));
        let (old, _) = read_snapshots(dir.path(), "@x/pkg", "typecheck").unwrap();
        assert_eq!(old.inputs.len(), 1);
        assert_eq!(old.inputs[0].hash, "def456");
    }
}
