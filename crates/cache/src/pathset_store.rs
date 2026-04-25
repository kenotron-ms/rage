//! Persists per-WF lists of pathsets observed by prior runs.
//!
//! On disk: `{cache_dir}/wf-{WF}.pathsets` is a JSON array of pathsets, where
//! each pathset is `{ "reads": [...], "writes": [...] }`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPathset {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
}

pub struct PathsetStore {
    dir: PathBuf,
}

impl PathsetStore {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
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

    /// Append a pathset under `weak_fp`. Idempotent — if the same pathset is
    /// already stored, no duplicate is added.
    pub fn append(&self, weak_fp: &str, ps: StoredPathset) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating cache dir {}", self.dir.display()))?;
        let mut existing = self.list(weak_fp);
        if existing.iter().any(|e| e == &ps) {
            return Ok(());
        }
        existing.push(ps);
        let json = serde_json::to_string_pretty(&existing).context("serializing pathsets")?;
        let path = self.path_for(weak_fp);
        std::fs::write(&path, json)
            .with_context(|| format!("writing {}", path.display()))?;
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
}
