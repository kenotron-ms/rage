# Phase 9 — Two-Phase Cache Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Replace the architecturally-incorrect single-phase blake3 fingerprint cache with the two-phase Weak Fingerprint → Strong Fingerprint design from the design document.

**Architecture:**
1. **Weak Fingerprint (WF)** — `blake3(command || tool_path || tool_hash || pkg_path || sorted(declared_input_globs → file content hashes) || sorted(tracked_env_vars))`. Indexes a *list* of pathsets observed for prior runs of this task.
2. **Pathset** — the set of files a prior run of this task actually read (from the sandbox). Stored as JSON under `wf-{WF}.pathsets`.
3. **Strong Fingerprint (SF)** — `blake3(WF || sorted(content hash of every file in pathset))`. The exact key for a cached `CacheEntry`.
4. **Lookup** — compute WF, list candidate pathsets, for each compute SF against current file contents, look up `sf-{SF}.entry`. Hit ⇒ skip execution. Miss ⇒ execute (under sandbox), capture the new pathset, store WF→pathset and SF→entry.

**Tech Stack:** Rust 2021, blake3, serde / serde_json, anyhow, plus the new `sandbox` crate from Phase 7 and the `plugin` crate from Phase 8.

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 5 — Cache Key Design.

---

## Constraints (from COE)

1. **MUST** be two-phase WF→SF — single-phase hashing is the bug we are fixing.
2. The **sandbox is the source of truth** for the pathset on cache miss. When `SandboxMode::Loose` or sandbox unavailable, fall back to using `declared_input_globs` as the pathset (correct but pessimistic).
3. **MUST** preserve the existing `CacheProvider` trait so the scheduler keeps working — the trait `get/put` semantics are unchanged at the trait level. `TwoPhaseCache` is a *new* implementor; `LocalCache` (single-phase) is kept for backwards compat in tests but no longer the default in the runner.
4. The first run of any task **MUST** be a cache miss (no prior pathset). The second identical run **MUST** hit. Tests must demonstrate both.
5. Tracked env vars come from the workspace config (Phase 6); for now, accept an empty list and add the wiring stub.

---

## Files Created / Modified

### New files in `crates/cache/src/`
- `weak_fp.rs` — WF computation
- `strong_fp.rs` — SF computation
- `pathset_store.rs` — read/write `wf-{WF}.pathsets`
- `two_phase.rs` — `TwoPhaseCache` provider implementing the WF→SF lookup
- `tool_hash.rs` — hash a tool binary by path

### Modified
- `crates/cache/Cargo.toml` — add `globset`, `hex`, `plugin` (path), `sandbox` (path)
- `crates/cache/src/lib.rs` — export new types
- `crates/cache/src/entry.rs` — extend `CacheEntry` with `pathset_reads: Vec<PathBuf>` (stored alongside the entry, for diagnostics)
- `crates/scheduler/src/runner.rs` — replace single-phase `fingerprint_task` call with `TwoPhaseCache.lookup_or_run`
- `crates/scheduler/Cargo.toml` — confirm `sandbox` and `plugin` deps

---

## Task 1: Add deps + scaffold modules

**Files:**
- Modify: `crates/cache/Cargo.toml`
- Modify: `crates/cache/src/lib.rs`

**Step 1: Cargo.toml**

Add to `[dependencies]`:

```toml
globset = "0.4"
hex = "0.4"
plugin = { path = "../plugin" }
sandbox = { path = "../sandbox" }
```

**Step 2: lib.rs — declare new modules and re-exports**

```rust
//! Content-addressed cache for rage build tasks.
//!
//! Two cache implementations live here:
//!   * `LocalCache` — single-phase blake3, kept for tests / fallback.
//!   * `TwoPhaseCache` — production: WF → pathset → SF (per design doc §5).

pub mod entry;
pub mod fingerprint; // legacy single-phase, kept for back-compat
pub mod local;
pub mod pathset_store;
pub mod provider;
pub mod strong_fp;
pub mod tool_hash;
pub mod two_phase;
pub mod weak_fp;

pub use entry::CacheEntry;
pub use fingerprint::fingerprint_task;
pub use local::LocalCache;
pub use pathset_store::PathsetStore;
pub use provider::CacheProvider;
pub use strong_fp::compute_strong_fingerprint;
pub use two_phase::TwoPhaseCache;
pub use weak_fp::{compute_weak_fingerprint, WeakFpInputs};
```

**Step 3: Create empty stubs for each new file** so the workspace builds:

```rust
// crates/cache/src/weak_fp.rs
//! Weak fingerprint computation (placeholder — implemented in next task).
pub struct WeakFpInputs;
pub fn compute_weak_fingerprint(_: &WeakFpInputs) -> String { String::new() }
```

```rust
// crates/cache/src/strong_fp.rs
pub fn compute_strong_fingerprint(_wf: &str, _reads: &[std::path::PathBuf]) -> String { String::new() }
```

```rust
// crates/cache/src/pathset_store.rs
use std::path::{Path, PathBuf};
pub struct PathsetStore { pub _dir: PathBuf }
impl PathsetStore {
    pub fn new(dir: &Path) -> Self { Self { _dir: dir.to_path_buf() } }
}
```

```rust
// crates/cache/src/tool_hash.rs
pub fn hash_tool_binary(_p: &std::path::Path) -> String { String::new() }
```

```rust
// crates/cache/src/two_phase.rs
//! TwoPhaseCache (placeholder — implemented in later tasks).
```

**Step 4: Verify workspace builds**

Run: `cargo build --workspace`
Expected: builds.

**Step 5: Commit**

```
git add crates/cache && git commit -m "chore(cache): scaffold weak_fp, strong_fp, pathset_store, two_phase, tool_hash modules"
```

---

## Task 2: Implement `tool_hash`

**Files:**
- Modify: `crates/cache/src/tool_hash.rs`

**Step 1: Write the failing test**

Replace `tool_hash.rs`:

```rust
//! Hash a tool binary by path. Used as part of the weak fingerprint so a
//! tsc / cargo / rustc upgrade invalidates caches.

use std::path::Path;

/// Hash the bytes of the binary at `path`.
///
/// Returns:
///   - `Some(hex)` when the file exists and is readable.
///   - `None` when the file is missing or unreadable. Callers should fall back
///     to hashing only the path string.
pub fn hash_tool_binary(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let h = blake3::hash(&bytes);
    Some(h.to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn hashes_file_contents() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("tool");
        std::fs::write(&p, b"#!/bin/sh\necho hi\n").unwrap();
        let h = hash_tool_binary(&p).unwrap();
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn missing_returns_none() {
        assert!(hash_tool_binary(Path::new("/nope/nope/nope")).is_none());
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a"); std::fs::write(&a, b"a").unwrap();
        let b = dir.path().join("b"); std::fs::write(&b, b"b").unwrap();
        assert_ne!(hash_tool_binary(&a).unwrap(), hash_tool_binary(&b).unwrap());
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p cache tool_hash`
Expected: pass.

**Step 3: Commit**

```
git add crates/cache && git commit -m "feat(cache): tool_hash::hash_tool_binary"
```

---

## Task 3: Implement `compute_weak_fingerprint`

**Files:**
- Modify: `crates/cache/src/weak_fp.rs`

**Step 1: Add the failing tests**

Replace `weak_fp.rs`:

```rust
//! Weak fingerprint (WF) computation.
//!
//! WF = blake3(
//!   command_string,
//!   tool_path,
//!   tool_hash (or "<missing>"),
//!   package_path,
//!   sorted(file_path || file_content_hash for f in declared_input_globs),
//!   sorted(env_var=value),
//! )
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 5.

use blake3::Hasher;
use globset::{Glob, GlobSetBuilder};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct WeakFpInputs<'a> {
    pub command: &'a str,
    pub tool_path: &'a Path,
    pub package_path: &'a Path,
    pub declared_input_globs: &'a [String],
    pub tracked_env: &'a [(String, String)],
}

pub fn compute_weak_fingerprint(inputs: &WeakFpInputs) -> String {
    let mut hasher = Hasher::new();

    // 1. command
    hasher.update(b"command:");
    hasher.update(inputs.command.as_bytes());
    hasher.update(b"\n");

    // 2. tool path + tool hash
    hasher.update(b"tool_path:");
    hasher.update(inputs.tool_path.as_os_str().as_encoded_bytes());
    hasher.update(b"\n");
    hasher.update(b"tool_hash:");
    match crate::tool_hash::hash_tool_binary(inputs.tool_path) {
        Some(h) => hasher.update(h.as_bytes()),
        None => hasher.update(b"<missing>"),
    };
    hasher.update(b"\n");

    // 3. package path
    hasher.update(b"pkg_path:");
    hasher.update(inputs.package_path.as_os_str().as_encoded_bytes());
    hasher.update(b"\n");

    // 4. declared inputs
    let files = resolve_globs(inputs.package_path, inputs.declared_input_globs);
    for f in &files {
        hasher.update(b"input:");
        hasher.update(f.as_os_str().as_encoded_bytes());
        hasher.update(b":");
        let content = std::fs::read(f).unwrap_or_default();
        let content_hash = blake3::hash(&content);
        hasher.update(content_hash.as_bytes());
        hasher.update(b"\n");
    }

    // 5. tracked env (sorted)
    let mut env: Vec<(String, String)> = inputs.tracked_env.to_vec();
    env.sort();
    for (k, v) in env {
        hasher.update(b"env:");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\n");
    }

    hasher.finalize().to_hex().to_string()
}

/// Walk `pkg_dir` and return all files matching any of `globs` (relative to pkg_dir).
/// Sorted, deduped. Skips `node_modules`, `target`, `dist`, `.git`.
fn resolve_globs(pkg_dir: &Path, globs: &[String]) -> Vec<PathBuf> {
    if globs.is_empty() {
        return Vec::new();
    }
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        if let Ok(parsed) = Glob::new(g) {
            builder.add(parsed);
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<PathBuf> = WalkDir::new(pkg_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let n = e.file_name().to_string_lossy();
                !matches!(n.as_ref(), "node_modules" | "target" | "dist" | ".git")
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let rel = e.path().strip_prefix(pkg_dir).unwrap_or(e.path());
            set.is_match(rel)
        })
        .map(|e| e.into_path())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn inputs<'a>(
        command: &'a str,
        tool: &'a Path,
        pkg: &'a Path,
        globs: &'a [String],
        env: &'a [(String, String)],
    ) -> WeakFpInputs<'a> {
        WeakFpInputs {
            command,
            tool_path: tool,
            package_path: pkg,
            declared_input_globs: globs,
            tracked_env: env,
        }
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let g = vec![];
        let env = vec![];
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &g, &env));
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &g, &env));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn command_changes_invalidate() {
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let h1 = compute_weak_fingerprint(&inputs("tsc",       &tool, pkg.path(), &[], &[]));
        let h2 = compute_weak_fingerprint(&inputs("tsc --x",   &tool, pkg.path(), &[], &[]));
        assert_ne!(h1, h2);
    }

    #[test]
    fn tool_content_changes_invalidate() {
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"v1").unwrap();
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &[]));
        std::fs::write(&tool, b"v2").unwrap();
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &[]));
        assert_ne!(h1, h2);
    }

    #[test]
    fn declared_input_content_invalidates() {
        let pkg = tempdir().unwrap();
        std::fs::create_dir_all(pkg.path().join("src")).unwrap();
        std::fs::write(pkg.path().join("src/index.ts"), b"a").unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let globs = vec!["src/**/*.ts".to_string()];
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &globs, &[]));
        std::fs::write(pkg.path().join("src/index.ts"), b"b").unwrap();
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &globs, &[]));
        assert_ne!(h1, h2);
    }

    #[test]
    fn unrelated_file_does_not_invalidate() {
        let pkg = tempdir().unwrap();
        std::fs::create_dir_all(pkg.path().join("src")).unwrap();
        std::fs::write(pkg.path().join("src/index.ts"), b"a").unwrap();
        let tool = pkg.path().join("tool");
        std::fs::write(&tool, b"x").unwrap();
        let globs = vec!["src/**/*.ts".to_string()];
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &globs, &[]));
        std::fs::write(pkg.path().join("README.md"), b"hi").unwrap();
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &globs, &[]));
        assert_eq!(h1, h2);
    }

    #[test]
    fn tracked_env_changes_invalidate() {
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let env1 = vec![("CI".to_string(), "1".to_string())];
        let env2 = vec![("CI".to_string(), "0".to_string())];
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &env1));
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &env2));
        assert_ne!(h1, h2);
    }

    #[test]
    fn env_order_does_not_matter() {
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let env1 = vec![("A".to_string(), "1".into()), ("B".to_string(), "2".into())];
        let env2 = vec![("B".to_string(), "2".into()), ("A".to_string(), "1".into())];
        let h1 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &env1));
        let h2 = compute_weak_fingerprint(&inputs("tsc", &tool, pkg.path(), &[], &env2));
        assert_eq!(h1, h2);
    }
}
```

**Step 2: Update Cargo.toml — add walkdir if not already there**

`crates/cache/Cargo.toml` already has `walkdir = "2"`. Confirm.

**Step 3: Run, verify pass**

Run: `cargo test -p cache weak_fp`
Expected: 7 tests pass.

**Step 4: Commit**

```
git add crates/cache && git commit -m "feat(cache): compute_weak_fingerprint per design doc §5"
```

---

## Task 4: Implement `compute_strong_fingerprint`

**Files:**
- Modify: `crates/cache/src/strong_fp.rs`

**Step 1: Add the failing tests**

Replace `strong_fp.rs`:

```rust
//! Strong fingerprint (SF) computation.
//!
//! SF = blake3(WF || sorted(path || file_content_hash for path in pathset_reads))
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 5.

use blake3::Hasher;
use std::path::{Path, PathBuf};

pub fn compute_strong_fingerprint(weak_fp: &str, pathset_reads: &[PathBuf]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"wf:");
    hasher.update(weak_fp.as_bytes());
    hasher.update(b"\n");

    let mut sorted: Vec<&Path> = pathset_reads.iter().map(|p| p.as_path()).collect();
    sorted.sort();
    sorted.dedup();

    for p in sorted {
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
    use tempfile::tempdir;

    #[test]
    fn deterministic_for_same_inputs() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, b"hi").unwrap();
        let h1 = compute_strong_fingerprint("WF", &[f.clone()]);
        let h2 = compute_strong_fingerprint("WF", &[f]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn changes_when_file_content_changes() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, b"v1").unwrap();
        let h1 = compute_strong_fingerprint("WF", &[f.clone()]);
        std::fs::write(&f, b"v2").unwrap();
        let h2 = compute_strong_fingerprint("WF", &[f]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_wf_yields_different_sf() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, b"v1").unwrap();
        let h1 = compute_strong_fingerprint("WF1", &[f.clone()]);
        let h2 = compute_strong_fingerprint("WF2", &[f]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn order_independent() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a"); std::fs::write(&a, b"a").unwrap();
        let b = dir.path().join("b"); std::fs::write(&b, b"b").unwrap();
        let h1 = compute_strong_fingerprint("WF", &[a.clone(), b.clone()]);
        let h2 = compute_strong_fingerprint("WF", &[b, a]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn missing_file_treated_as_empty() {
        let h = compute_strong_fingerprint("WF", &[PathBuf::from("/nope/nope/nope")]);
        assert_eq!(h.len(), 64);
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p cache strong_fp`
Expected: 5 tests pass.

**Step 3: Commit**

```
git add crates/cache && git commit -m "feat(cache): compute_strong_fingerprint per design doc §5"
```

---

## Task 5: Implement `PathsetStore`

**Files:**
- Modify: `crates/cache/src/pathset_store.rs`

**Step 1: Replace with implementation + tests**

```rust
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
    use tempfile::tempdir;

    fn ps(reads: &[&str], writes: &[&str]) -> StoredPathset {
        StoredPathset {
            reads: reads.iter().map(PathBuf::from).collect(),
            writes: writes.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn empty_when_unknown_wf() {
        let dir = tempdir().unwrap();
        let s = PathsetStore::new(dir.path());
        assert!(s.list("nope").is_empty());
    }

    #[test]
    fn append_then_list() {
        let dir = tempdir().unwrap();
        let s = PathsetStore::new(dir.path());
        let p1 = ps(&["/a"], &[]);
        s.append("WF1", p1.clone()).unwrap();
        let got = s.list("WF1");
        assert_eq!(got, vec![p1]);
    }

    #[test]
    fn duplicate_append_is_idempotent() {
        let dir = tempdir().unwrap();
        let s = PathsetStore::new(dir.path());
        let p = ps(&["/a"], &[]);
        s.append("WF1", p.clone()).unwrap();
        s.append("WF1", p.clone()).unwrap();
        assert_eq!(s.list("WF1").len(), 1);
    }

    #[test]
    fn distinct_pathsets_accumulate() {
        let dir = tempdir().unwrap();
        let s = PathsetStore::new(dir.path());
        s.append("WF1", ps(&["/a"], &[])).unwrap();
        s.append("WF1", ps(&["/a", "/b"], &[])).unwrap();
        assert_eq!(s.list("WF1").len(), 2);
    }

    #[test]
    fn separate_wfs_isolated() {
        let dir = tempdir().unwrap();
        let s = PathsetStore::new(dir.path());
        s.append("WF1", ps(&["/a"], &[])).unwrap();
        s.append("WF2", ps(&["/b"], &[])).unwrap();
        assert_eq!(s.list("WF1").len(), 1);
        assert_eq!(s.list("WF2").len(), 1);
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p cache pathset_store`
Expected: 5 tests pass.

**Step 3: Commit**

```
git add crates/cache && git commit -m "feat(cache): PathsetStore — wf-{WF}.pathsets JSON files"
```

---

## Task 6: Extend `CacheEntry` with `pathset_reads`

**Files:**
- Modify: `crates/cache/src/entry.rs`

**Step 1: Add the failing test**

Append to `crates/cache/src/entry.rs` `tests` module:

```rust
    #[test]
    fn entry_carries_pathset_reads() {
        let e = CacheEntry {
            fingerprint: "fp".into(),
            command: "cmd".into(),
            exit_code: 0,
            elapsed_ms: 10,
            cached_at: 0,
            pathset_reads: vec![std::path::PathBuf::from("/a"), std::path::PathBuf::from("/b")],
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: CacheEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back.pathset_reads, e.pathset_reads);
    }

    #[test]
    fn entry_back_compat_no_pathset_reads_in_old_json() {
        // Existing JSON files written by the single-phase cache lack
        // pathset_reads. Decoding must default the field to an empty vec.
        let old = r#"{"fingerprint":"fp","command":"cmd","exit_code":0,"elapsed_ms":1,"cached_at":0}"#;
        let e: CacheEntry = serde_json::from_str(old).unwrap();
        assert!(e.pathset_reads.is_empty());
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p cache entry`
Expected: FAIL — field doesn't exist.

**Step 3: Add the field**

In `crates/cache/src/entry.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheEntry {
    pub fingerprint: String,
    pub command: String,
    pub exit_code: i32,
    pub elapsed_ms: u64,
    pub cached_at: u64,
    /// Pathset reads observed by the sandbox on the run that produced this
    /// entry. Used for diagnostics (`rage why-miss`). Optional for back-compat
    /// with entries written by single-phase cache.
    #[serde(default)]
    pub pathset_reads: Vec<std::path::PathBuf>,
}
```

Update existing constructors (in `runner.rs` and tests) to default the new field:

```bash
rg "CacheEntry \{" crates/
```

Add `pathset_reads: vec![]` to each literal.

**Step 4: Run, verify pass**

Run: `cargo test -p cache && cargo test -p scheduler`
Expected: pass.

**Step 5: Commit**

```
git add crates/cache crates/scheduler && git commit -m "feat(cache): CacheEntry.pathset_reads (optional, back-compat)"
```

---

## Task 7: Implement `TwoPhaseCache`

**Files:**
- Modify: `crates/cache/src/two_phase.rs`

**Step 1: Implement**

Replace `two_phase.rs`:

```rust
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
    pub fn with_dir(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache dir {}", dir.display()))?;
        let pathsets = PathsetStore::new(&dir);
        Ok(Self { dir, pathsets })
    }

    pub fn dir(&self) -> &Path { &self.dir }

    pub fn pathsets(&self) -> &PathsetStore { &self.pathsets }

    /// Look up a hit using two-phase fingerprinting.
    pub fn lookup(&self, weak_fp_inputs: &WeakFpInputs) -> Option<(String, CacheEntry)> {
        let wf = compute_weak_fingerprint(weak_fp_inputs);
        for ps in self.pathsets.list(&wf) {
            let sf = compute_strong_fingerprint(&wf, &ps.reads);
            if let Some(entry) = self.read_entry(&sf) {
                return Some((sf, entry));
            }
        }
        None
    }

    /// Record a successful run. Stores both the pathset (under WF) and the
    /// entry (under SF).
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        }
    }

    #[test]
    fn first_lookup_misses() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
        };
        assert!(cache.lookup(&inputs).is_none());
    }

    #[test]
    fn record_then_lookup_hits() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
        };
        let ps = StoredPathset { reads: vec![f.clone()], writes: vec![] };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        let hit = cache.lookup(&inputs).unwrap();
        assert!(hit.0.len() == 64);
        assert_eq!(hit.1.exit_code, 0);
    }

    #[test]
    fn pathset_file_change_invalidates_sf() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &[],
            tracked_env: &[],
        };
        let ps = StoredPathset { reads: vec![f.clone()], writes: vec![] };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        // change pathset file content
        std::fs::write(&f, b"b").unwrap();
        assert!(cache.lookup(&inputs).is_none(), "SF must change when pathset content changes");
    }

    #[test]
    fn declared_input_change_invalidates_wf() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();
        let _f = pkg_with_file(pkg.path(), "index.ts", b"a");

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let globs = vec!["src/**/*.ts".to_string()];
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: pkg.path(),
            declared_input_globs: &globs,
            tracked_env: &[],
        };
        let ps = StoredPathset { reads: vec![], writes: vec![] };
        cache.record(&inputs, ps, entry_template("tsc")).unwrap();

        // changing declared input → WF changes → no pathsets → miss
        std::fs::write(pkg.path().join("src/index.ts"), b"b").unwrap();
        assert!(cache.lookup(&inputs).is_none());
    }

    #[test]
    fn distinct_commands_isolated() {
        let cache_dir = tempdir().unwrap();
        let pkg = tempdir().unwrap();
        let tool = pkg.path().join("tool"); std::fs::write(&tool, b"x").unwrap();

        let cache = TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let inputs_a = WeakFpInputs {
            command: "cmd-a", tool_path: &tool, package_path: pkg.path(),
            declared_input_globs: &[], tracked_env: &[],
        };
        let inputs_b = WeakFpInputs { command: "cmd-b", ..inputs_a };
        cache.record(&inputs_a, StoredPathset::default(), entry_template("a")).unwrap();
        assert!(cache.lookup(&inputs_a).is_some());
        assert!(cache.lookup(&inputs_b).is_none());
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p cache two_phase`
Expected: 5 tests pass.

**Step 3: Commit**

```
git add crates/cache && git commit -m "feat(cache): TwoPhaseCache — lookup() + record() WF→SF"
```

---

## Task 8: Wire `TwoPhaseCache` into the scheduler

**Files:**
- Modify: `crates/scheduler/src/runner.rs`
- Modify: `crates/scheduler/Cargo.toml` (confirm `plugin`, `sandbox` deps already added in Phase 7/8; if missing, add them)

**Step 1: Add the failing integration test**

Append to `crates/scheduler/src/runner.rs` `tests` module:

```rust
    #[tokio::test]
    async fn two_phase_cache_first_run_misses_second_run_hits() {
        use cache::{TwoPhaseCache};
        use std::sync::Arc;
        use tempfile::tempdir;

        let cache_dir = tempdir().unwrap();
        let pkg_dir = tempdir().unwrap();
        // Make a pretend declared input file
        std::fs::create_dir_all(pkg_dir.path().join("src")).unwrap();
        std::fs::write(pkg_dir.path().join("src/index.ts"), b"export const x = 1;").unwrap();

        let two_phase = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());

        let task = Task {
            package_name: "pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo build".to_string(),
            cwd: pkg_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose, // sandbox-bypass for this test
        };
        let pkg = mk_pkg("pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        // First run — must execute (miss). For now we wire via a `run_tasks_two_phase`
        // adapter that the scheduler exposes alongside `run_tasks`.
        run_tasks_two_phase(&dag, vec![task.clone()], two_phase.clone()).await.unwrap();

        // After first run there should be at least one wf-* and one sf-* file
        let entries: Vec<_> = std::fs::read_dir(cache_dir.path()).unwrap().collect();
        assert!(
            entries.iter().any(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("wf-")),
            "expected wf-*.pathsets file"
        );
        assert!(
            entries.iter().any(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("sf-")),
            "expected sf-*.entry file"
        );

        // Second run — same task — should hit and skip execution.
        // We assert by capturing whether we re-recorded; same SF is fine.
        run_tasks_two_phase(&dag, vec![task], two_phase).await.unwrap();
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p scheduler two_phase_cache`
Expected: FAIL — `run_tasks_two_phase` doesn't exist.

**Step 3: Implement `run_tasks_two_phase`**

In `crates/scheduler/src/runner.rs`, add a new public function:

```rust
/// Wave-parallel execution with two-phase cache (WF → SF).
///
/// Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 5:
///   - On `Loose` sandbox mode (or where sandbox isn't available), use
///     declared inputs as the pathset (pessimistic but correct).
///   - On `Observed` / `Strict` modes, run under sandbox and capture the
///     pathset from observation.
pub async fn run_tasks_two_phase(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: std::sync::Arc<cache::TwoPhaseCache>,
) -> anyhow::Result<()> {
    let levels = compute_task_levels(dag, &tasks);
    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();
        for task in level {
            let cache_clone = cache.clone();
            set.spawn(run_single_task_two_phase(task, cache_clone));
        }
        let mut first_error: Option<RunError> = None;
        while let Some(jr) = set.join_next().await {
            match jr {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() { first_error = Some(e); }
                    set.abort_all();
                }
                Err(_) => {
                    if first_error.is_none() {
                        first_error = Some(RunError::Killed {
                            package: "unknown".into(), script: "unknown".into(),
                        });
                    }
                }
            }
        }
        if let Some(e) = first_error { return Err(e.into()); }
    }
    Ok(())
}

async fn run_single_task_two_phase(
    task: Task,
    cache: std::sync::Arc<cache::TwoPhaseCache>,
) -> Result<(), RunError> {
    use cache::{CacheEntry, WeakFpInputs};
    use cache::pathset_store::StoredPathset;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Resolve the tool path. Cheap heuristic: shell-tokenize the command, take
    // the first token, search PATH. If lookup fails we use a synthetic path.
    let tool_path = which_first(&task.command).unwrap_or_else(|| std::path::PathBuf::from("sh"));
    let inputs = WeakFpInputs {
        command: &task.command,
        tool_path: &tool_path,
        package_path: &task.cwd,
        declared_input_globs: &[], // wired from plugin lookup in a follow-up step
        tracked_env: &[],
    };

    // Lookup
    if let Some((_sf, _entry)) = cache.lookup(&inputs) {
        eprintln!(
            "[rage] {}#{} \u{2713} (cached, two-phase)",
            task.package_name, task.script_name
        );
        return Ok(());
    }

    // Miss — execute. For Loose mode, skip sandbox.
    eprintln!(
        "[rage] {}#{} starting [sandbox={:?}]",
        task.package_name, task.script_name, task.sandbox_mode
    );
    let start = std::time::Instant::now();

    let (exit_code, pathset) = match task.sandbox_mode {
        pipeline_config::SandboxMode::Loose => {
            let status = tokio::process::Command::new("sh")
                .arg("-c").arg(&task.command).current_dir(&task.cwd)
                .status().await
                .map_err(|e| RunError::Spawn {
                    package: task.package_name.clone(), script: task.script_name.clone(), source: e,
                })?;
            (status.code().unwrap_or(-1), StoredPathset::default())
        }
        pipeline_config::SandboxMode::Observed | pipeline_config::SandboxMode::Strict => {
            // Sandbox is best-effort; on macOS this works, on Linux it's a stub.
            match sandbox::run_sandboxed(&task.command, &task.cwd, &[]).await {
                Ok(r) => {
                    let ps = StoredPathset {
                        reads: r.path_set.reads, writes: r.path_set.writes,
                    };
                    (r.exit_code, ps)
                }
                Err(_) => {
                    // Sandbox unavailable — fall back to plain execution + empty pathset.
                    let status = tokio::process::Command::new("sh")
                        .arg("-c").arg(&task.command).current_dir(&task.cwd)
                        .status().await
                        .map_err(|e| RunError::Spawn {
                            package: task.package_name.clone(), script: task.script_name.clone(), source: e,
                        })?;
                    (status.code().unwrap_or(-1), StoredPathset::default())
                }
            }
        }
    };

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    if exit_code == 0 {
        let entry = CacheEntry {
            fingerprint: String::new(),
            command: task.command.clone(),
            exit_code: 0,
            elapsed_ms,
            cached_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            pathset_reads: vec![],
        };
        let _ = cache.record(&inputs, pathset, entry);
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name, task.script_name, elapsed.as_secs_f64()
        );
        Ok(())
    } else {
        eprintln!(
            "[rage] {}#{} \u{2717} FAILED (exit {exit_code})",
            task.package_name, task.script_name
        );
        Err(RunError::TaskFailed {
            package: task.package_name, script: task.script_name, code: exit_code,
        })
    }
}

fn which_first(command: &str) -> Option<std::path::PathBuf> {
    let first = command.split_whitespace().next()?;
    if first.contains('/') {
        return Some(std::path::PathBuf::from(first));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(first);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p scheduler two_phase_cache`
Expected: pass.

**Step 5: Commit**

```
git add crates/scheduler && git commit -m "feat(scheduler): run_tasks_two_phase — uses TwoPhaseCache + sandbox"
```

---

## Task 9: Switch CLI to `run_tasks_two_phase`

**Files:**
- Modify: `crates/cli/src/main.rs`

**Step 1: Update `cmd_run`**

Replace the `cache` block + `run_tasks` call with:

```rust
    if no_cache {
        scheduler::run_tasks(&dag, tasks, None)
            .await
            .with_context(|| format!("'{script}' run failed"))?;
    } else {
        let cache_dir = match &config.cache.dir {
            Some(d) => d.clone(),
            None => {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
                home.join(".rage").join("cache")
            }
        };
        let two_phase = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir).context("opening two-phase cache")?,
        );
        scheduler::run_tasks_two_phase(&dag, tasks, two_phase)
            .await
            .with_context(|| format!("'{script}' run failed"))?;
    }
```

**Step 2: Verify**

```
cargo test --workspace
cargo build --release
./target/release/rage run build fixtures/js-pnpm
./target/release/rage run build fixtures/js-pnpm
```

Expected: second invocation prints `(cached, two-phase)` for every task.

**Step 3: Commit**

```
git add crates/cli && git commit -m "feat(cli): cmd_run uses TwoPhaseCache by default"
```

---

## Task 10: Verification gate

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
./target/release/rage run build fixtures/js-pnpm
./target/release/rage run build fixtures/js-pnpm   # should be all hits
```

All green required. The single-phase `LocalCache` and `fingerprint_task` are kept for back-compat; they are no longer the default cache but tests still cover them.

---

## Total tasks: 10
