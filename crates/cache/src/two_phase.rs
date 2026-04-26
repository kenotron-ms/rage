//! Two-phase cache: WF lookup → SF check.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 5.
//!
//! Algorithm (`lookup`):
//!   1. Compute WF.
//!   2. List candidate pathsets stored under WF.
//!   3. For each pathset: compute SF; look up `sf-{SF}.entry`.
//!   4. Return the first hit; or `None` (miss).
//!
//! Algorithm (`record`):
//!   1. Append the new pathset under WF.
//!   2. Compute SF from the pathset reads.
//!   3. Write `sf-{SF}.entry`.

use crate::entry::CacheEntry;
use crate::pathset_store::{PathsetStore, StoredPathset};
use crate::strong_fp::compute_strong_fingerprint;
use crate::weak_fp::{compute_weak_fingerprint, WeakFpInputs};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct TwoPhaseCache {
    dir: PathBuf,
    pathsets: PathsetStore,
}

impl TwoPhaseCache {
    /// Create (or open) a `TwoPhaseCache` backed by `dir`.
    /// Creates the directory if it does not exist.
    pub fn with_dir(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache dir {}", dir.display()))?;
        let pathsets = PathsetStore::new(&dir);
        Ok(Self { dir, pathsets })
    }

    /// Return the directory this cache is stored in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Return a reference to the underlying `PathsetStore`.
    pub fn pathsets(&self) -> &PathsetStore {
        &self.pathsets
    }

    /// Look up a hit using two-phase fingerprinting.
    ///
    /// Returns `Some((sf, entry))` on a hit, or `None` on a miss.
    pub fn lookup(&self, weak_fp_inputs: &WeakFpInputs) -> Option<(String, CacheEntry)> {
        self.lookup_with_pathset_reads(weak_fp_inputs)
            .map(|(sf, entry, _)| (sf, entry))
    }

    /// Like `lookup`, but also returns the pathset reads from the stored WF entry
    /// so callers can populate the artifact CAS on cache hits without re-reading the file.
    ///
    /// Returns `None` on cache miss. On hit, the `Vec<PathBuf>` contains all read paths
    /// from the matching stored pathset.
    pub fn lookup_with_pathset_reads(
        &self,
        weak_fp_inputs: &WeakFpInputs,
    ) -> Option<(String, CacheEntry, Vec<PathBuf>)> {
        let wf = compute_weak_fingerprint(weak_fp_inputs);
        for ps in self.pathsets.list(&wf) {
            let sf = compute_strong_fingerprint(&wf, &ps.reads);
            if let Some(entry) = self.read_entry(&sf) {
                return Some((sf, entry, ps.reads));
            }
        }
        None
    }

    /// Record a successful run. Stores both the pathset (under WF) and the
    /// entry (under SF). Returns the SF string.
    ///
    /// `entry.fingerprint` is set to the SF and `entry.pathset_reads` is set
    /// to `pathset.reads` before writing the entry.
    pub fn record(
        &self,
        weak_fp_inputs: &WeakFpInputs,
        pathset: StoredPathset,
        entry_template: CacheEntry,
    ) -> Result<String> {
        let wf = compute_weak_fingerprint(weak_fp_inputs);
        self.pathsets.append(&wf, pathset.clone())?;
        let sf = compute_strong_fingerprint(&wf, &pathset.reads);
        let mut entry = entry_template;
        entry.fingerprint = sf.clone();
        entry.pathset_reads = pathset.reads;
        self.write_entry(&sf, &entry)?;
        Ok(sf)
    }

    // ── private helpers ──────────────────────────────────────────────────

    fn entry_path(&self, sf: &str) -> PathBuf {
        self.dir.join(format!("sf-{sf}.entry"))
    }

    fn read_entry(&self, sf: &str) -> Option<CacheEntry> {
        let raw = std::fs::read_to_string(self.entry_path(sf)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn write_entry(&self, sf: &str, entry: &CacheEntry) -> Result<()> {
        let path = self.entry_path(sf);
        let json = serde_json::to_string_pretty(entry).context("serializing entry")?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    // ── ABI fingerprint persistence ──────────────────────────────────────────

    /// Convert a package name to a safe filename component.
    ///
    /// Replaces `/` and `@` with safe chars: `@lage-run/core` → `_at_lage-run__core`.
    fn pkg_name_to_filename(name: &str) -> String {
        name.replace('@', "_at_").replace('/', "__")
    }

    /// Read the stored ABI fingerprint for `pkg_name`.
    ///
    /// Returns `None` if no fingerprint has been stored yet (e.g. first run or
    /// the package's plugin doesn't support ABI fingerprinting).
    pub fn get_pkg_abi_fp(&self, pkg_name: &str) -> Option<String> {
        let path = self
            .dir
            .join("pkg-abi")
            .join(Self::pkg_name_to_filename(pkg_name));
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Persist the ABI fingerprint for `pkg_name`.
    ///
    /// Errors are silently ignored — ABI fingerprint storage is best-effort;
    /// a write failure must never break a build.
    pub fn set_pkg_abi_fp(&self, pkg_name: &str, abi_fp: &str) {
        let dir = self.dir.join("pkg-abi");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(Self::pkg_name_to_filename(pkg_name));
        let _ = std::fs::write(&path, abi_fp);
    }
}

#[cfg(test)]
mod tests {
    use crate::entry::CacheEntry;
    use crate::pathset_store::StoredPathset;
    use crate::weak_fp::WeakFpInputs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    use super::TwoPhaseCache;

    fn pkg_with_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let p = dir.join("src").join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn entry_template(cmd: &str) -> CacheEntry {
        CacheEntry {
            fingerprint: String::new(),
            command: cmd.to_string(),
            exit_code: 0,
            elapsed_ms: 1,
            cached_at: 0,
            pathset_reads: vec![],
            abi_fingerprint: None,
        }
    }

    #[test]
    fn first_lookup_misses() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        assert!(cache.lookup(&inputs).is_none());
    }

    #[test]
    fn record_then_lookup_hits() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        let ps = StoredPathset {
            reads: vec![f.clone()],
            writes: vec![],
        };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        let hit = cache.lookup(&inputs).unwrap();
        assert!(hit.0.len() == 64);
        assert_eq!(hit.1.exit_code, 0);
    }

    #[test]
    fn pathset_file_change_invalidates_sf() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        let ps = StoredPathset {
            reads: vec![f.clone()],
            writes: vec![],
        };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        // change pathset file content
        std::fs::write(&f, b"b").unwrap();
        assert!(
            cache.lookup(&inputs).is_none(),
            "SF must change when pathset content changes"
        );
    }

    #[test]
    fn declared_input_change_invalidates_wf() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let _f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let globs = vec!["src/**/*.ts".to_string()];
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &globs,
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        let ps = StoredPathset {
            reads: vec![],
            writes: vec![],
        };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        // changing declared input → WF changes → no pathsets → miss
        std::fs::write(pkg.path().join("src/index.ts"), b"b").unwrap();
        assert!(cache.lookup(&inputs).is_none());
    }

    #[test]
    fn lookup_with_pathset_reads_returns_reads_on_hit() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let f = pkg_with_file(pkg.path(), "index.ts", b"hello");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        let ps = StoredPathset {
            reads: vec![f.clone()],
            writes: vec![],
        };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        let result = cache.lookup_with_pathset_reads(&inputs);
        assert!(result.is_some(), "expected a cache hit");
        let (sf, entry, reads) = result.unwrap();
        assert_eq!(sf.len(), 64, "sf should be a 64-char hex string");
        assert_eq!(entry.exit_code, 0);
        assert_eq!(reads, vec![f], "reads must match the stored pathset reads");
    }

    #[test]
    fn lookup_with_pathset_reads_returns_none_on_miss() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        assert!(cache.lookup_with_pathset_reads(&inputs).is_none());
    }

    #[test]
    fn distinct_commands_isolated() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs_a = WeakFpInputs {
            command: "cmd-a",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };
        let inputs_b = WeakFpInputs {
            command: "cmd-b",
            ..inputs_a
        };
        cache
            .record(&inputs_a, StoredPathset::default(), entry_template("a"))
            .unwrap();
        assert!(cache.lookup(&inputs_a).is_some());
        assert!(cache.lookup(&inputs_b).is_none());
    }
}
