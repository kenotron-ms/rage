//! Persists per-WF lists of pathsets observed by prior runs.
//!
//! On disk: `{cache_dir}/wf-{WF}.pathsets` is a JSON array of pathsets, where
//! each pathset is `{ "reads": [...], "writes": [...] }`.

use crate::strong_fp::is_node_modules_path;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use plugin::AllowlistEntry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPathset {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
}

impl StoredPathset {
    /// Pre-filter (`allowlist` removed outside `node_modules`, while preserving
    /// all `node_modules` paths in `reads` so warm-hit artifact CAS capture can
    /// discover packages like `typescript` even when matched by a toolchain
    /// allowlist) and lexicographically sort + deduplicate `reads` and `writes`
    /// before persisting to `pathset_store`.
    pub fn normalized(mut self, allowlist: &[AllowlistEntry]) -> Self {
        let allow_set = build_allowlist_globset(allowlist);
        if let Some(set) = &allow_set {
            self.reads
                .retain(|p| is_node_modules_path(p) || !set.is_match(p));
        }
        self.reads.sort();
        self.reads.dedup();

        self.writes.sort();
        self.writes.dedup();
        self
    }

    /// Return the read slice for strong-fingerprint evaluation (`compute_strong_fingerprint`
    /// skips `node_modules` inline in a single linear pass when `self.reads` is
    /// already sorted).
    pub fn reads_for_sf(&self) -> &[PathBuf] {
        &self.reads
    }
}

fn build_allowlist_globset(allowlist: &[AllowlistEntry]) -> Option<GlobSet> {
    if allowlist.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    for entry in allowlist {
        if let Ok(glob) = Glob::new(&entry.path_pattern) {
            builder.add(glob);
        }
    }
    builder.build().ok()
}

pub struct PathsetStore {
    dir: PathBuf,
}

impl PathsetStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// All pathsets recorded under `weak_fp`. Empty if none.
    pub fn list(&self, weak_fp: &str) -> Vec<StoredPathset> {
        let path = self.path_for(weak_fp);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Append a pathset under `weak_fp`, pre-filtering `node_modules` and
    /// sorting/deduplicating paths. Idempotent — if the normalized pathset is
    /// already stored, no duplicate is added.
    pub fn append(&self, weak_fp: &str, ps: StoredPathset) -> Result<()> {
        self.append_with_allowlist(weak_fp, ps, &[])
    }

    /// Append a pathset under `weak_fp`, pre-filtering `node_modules` and
    /// `allowlist` patterns, and sorting/deduplicating paths before writing.
    pub fn append_with_allowlist(
        &self,
        weak_fp: &str,
        ps: StoredPathset,
        allowlist: &[AllowlistEntry],
    ) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating cache dir {}", self.dir.display()))?;
        let ps = ps.normalized(allowlist);
        let mut existing = self.list(weak_fp);
        if existing.iter().any(|e| e == &ps) {
            return Ok(());
        }
        existing.push(ps);
        let json = serde_json::to_string_pretty(&existing).context("serializing pathsets")?;
        let path = self.path_for(weak_fp);
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    fn path_for(&self, weak_fp: &str) -> PathBuf {
        self.dir.join(format!("wf-{weak_fp}.pathsets"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, PathsetStore) {
        let tmp = TempDir::new().unwrap();
        let store = PathsetStore::new(tmp.path());
        (tmp, store)
    }

    #[test]
    fn empty_when_unknown_wf() {
        let (_tmp, store) = make_store();
        assert!(store.list("nonexistent").is_empty());
    }

    #[test]
    fn append_then_list() {
        let (_tmp, store) = make_store();
        let ps = StoredPathset {
            reads: vec![PathBuf::from("a.txt")],
            writes: vec![PathBuf::from("b.txt")],
        };
        store.append("abc123", ps.clone()).unwrap();
        let result = store.list("abc123");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ps);
    }

    #[test]
    fn duplicate_append_is_idempotent() {
        let (_tmp, store) = make_store();
        let ps = StoredPathset {
            reads: vec![PathBuf::from("x.rs")],
            writes: vec![],
        };
        store.append("fp1", ps.clone()).unwrap();
        store.append("fp1", ps.clone()).unwrap();
        store.append("fp1", ps.clone()).unwrap();
        assert_eq!(store.list("fp1").len(), 1);
    }

    #[test]
    fn distinct_pathsets_accumulate() {
        let (_tmp, store) = make_store();
        let ps1 = StoredPathset {
            reads: vec![PathBuf::from("a.rs")],
            writes: vec![],
        };
        let ps2 = StoredPathset {
            reads: vec![PathBuf::from("b.rs")],
            writes: vec![PathBuf::from("out.o")],
        };
        store.append("fp2", ps1.clone()).unwrap();
        store.append("fp2", ps2.clone()).unwrap();
        let result = store.list("fp2");
        assert_eq!(result.len(), 2);
        assert!(result.contains(&ps1));
        assert!(result.contains(&ps2));
    }

    #[test]
    fn separate_wfs_isolated() {
        let (_tmp, store) = make_store();
        let ps_a = StoredPathset {
            reads: vec![PathBuf::from("a.rs")],
            writes: vec![],
        };
        let ps_b = StoredPathset {
            reads: vec![PathBuf::from("b.rs")],
            writes: vec![],
        };
        store.append("wf_alpha", ps_a.clone()).unwrap();
        store.append("wf_beta", ps_b.clone()).unwrap();

        let alpha = store.list("wf_alpha");
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0], ps_a);

        let beta = store.list("wf_beta");
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0], ps_b);
    }

    #[test]
    fn append_pre_filters_allowlist_preserves_node_modules_and_sorts() {
        let (_tmp, store) = make_store();
        let raw = StoredPathset {
            reads: vec![
                PathBuf::from("src/z.ts"),
                PathBuf::from("node_modules/typescript/lib/typescript.js"),
                PathBuf::from("src/a.ts"),
                PathBuf::from("src/z.ts"),
                PathBuf::from("/usr/local/share/toolchain/std.d.ts"),
            ],
            writes: vec![PathBuf::from("dist/b.js"), PathBuf::from("dist/a.js")],
        };
        let allowlist = vec![
            plugin::AllowlistEntry {
                path_pattern: "**/toolchain/**".to_string(),
                reason: "toolchain stdlib".to_string(),
            },
            plugin::AllowlistEntry {
                path_pattern: "**/node_modules/typescript/**".to_string(),
                reason: "typescript compiler".to_string(),
            },
        ];

        store
            .append_with_allowlist("wf_norm", raw, &allowlist)
            .unwrap();
        let stored = store.list("wf_norm");
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].reads,
            vec![
                PathBuf::from("node_modules/typescript/lib/typescript.js"),
                PathBuf::from("src/a.ts"),
                PathBuf::from("src/z.ts"),
            ]
        );
        assert_eq!(
            stored[0].writes,
            vec![PathBuf::from("dist/a.js"), PathBuf::from("dist/b.js")]
        );
    }
}
