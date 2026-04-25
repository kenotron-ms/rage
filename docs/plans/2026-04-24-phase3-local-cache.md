# Phase 3: Local Cache Implementation Plan

> **Execution:** Use the subagent-driven-development workflow.
> **Prerequisite:** Phase 2 complete — `cargo test --workspace` passes (55 tests), `rage run build fixtures/js-pnpm` executes all 4 packages.

**Goal:** Add a local content-addressed cache so that `rage run` skips tasks whose inputs haven't changed. Second run should print `✓ (cached)` for all packages and be nearly instant.

**Architecture:** New `cache` crate provides `fingerprint_task` (blake3 content hash), `CacheEntry` (JSON on disk), `CacheProvider` trait, and `LocalCache` impl (`~/.rage/cache/`). `scheduler` gains a cache integration layer. CLI adds `--no-cache` flag.

**Tech Stack:** `blake3` for content hashing, `walkdir` for source file enumeration, `serde`/`serde_json` for JSON, existing `tokio` + `anyhow`.

**End state:** `rage run build fixtures/js-pnpm` run twice:
```
# First run (cold)
[rage] @fixture/core#build starting
building @fixture/core
[rage] @fixture/core#build ✓ 0.03s
[rage] @fixture/utils#build starting
...
[rage] @fixture/app#build ✓ 0.01s
Done.

# Second run (warm cache)
[rage] @fixture/core#build ✓ (cached)
[rage] @fixture/utils#build ✓ (cached)
[rage] @fixture/ui#build ✓ (cached)
[rage] @fixture/app#build ✓ (cached)
Done.
```

**What's deferred:** remote cache (Azure Blob, S3), sandbox-observed inputs, stdout/stderr capture and replay.

---

## Context For The Implementer

You are adding a local cache to the `rage` build system. The repo lives at `/Users/ken/workspace/ms/rage`. Work on branch `feat/phase-3-local-cache` in worktree `.worktrees/feat-phase-3-local-cache`.

**Existing crates:**
- `workspace-tools` — package discovery, dependency resolution
- `build-graph` — DAG, topo sort, DOT output
- `pipeline-config` — `rage.json` loader (skeleton)
- `scheduler` — `Task`, `build_task_list`, `compute_task_levels`, `run_tasks`
- `cli` — `rage` binary (`rage graph`, `rage run`)

**Key files to understand before starting:**
- `crates/scheduler/src/runner.rs` — `run_tasks` and `run_single_task`
- `crates/scheduler/src/task.rs` — `Task` struct
- `crates/cli/src/main.rs` — CLI wiring

**Rules:**
1. Follow each task's steps literally and in order.
2. TDD: write failing tests **before** implementation for every function.
3. Do NOT add functionality beyond what each task specifies.
4. Commit after each task with the exact message given.
5. If a test fails unexpectedly, STOP and report. Do not alter tests to match broken behavior.
6. Run `cargo clippy --workspace --all-targets -- -D warnings` after every task; fix all warnings before committing.

---

## Task 1: Scaffold `cache` Crate

**Goal:** Create the `cache` crate skeleton so it compiles. No logic yet.

### Steps

1. Add `"crates/cache"` to workspace `Cargo.toml` members list (after `crates/scheduler`).

2. Create `crates/cache/Cargo.toml`:
```toml
[package]
name = "cache"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
blake3 = "1"
walkdir = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
```

3. Create `crates/cache/src/lib.rs`:
```rust
//! Content-addressed local cache for rage build tasks.

pub mod entry;
pub mod fingerprint;
pub mod local;
pub mod provider;

pub use entry::CacheEntry;
pub use fingerprint::fingerprint_task;
pub use local::LocalCache;
pub use provider::CacheProvider;
```

4. Create four placeholder files:
   - `crates/cache/src/entry.rs` — `// placeholder`
   - `crates/cache/src/fingerprint.rs` — `// placeholder`
   - `crates/cache/src/local.rs` — `// placeholder`
   - `crates/cache/src/provider.rs` — `// placeholder`

5. Verify: `cargo build -p cache` succeeds (`Finished` in output, zero errors).

6. Verify: `cargo test --workspace 2>&1 | tail -5` shows all prior tests still pass (≥ 55 tests, 0 failed).

7. Commit:
```
feat(cache): scaffold crate
```

---

## Task 2: Implement `fingerprint_task`

**Goal:** Hash a task's command + source file contents using blake3 to produce a stable cache key.

### Steps

1. Write the failing tests in `crates/cache/src/fingerprint.rs` **first** (TDD — use `todo!()` for the function body):

```rust
//! Content fingerprinting for tasks.

use anyhow::Result;
use std::path::Path;

/// Compute a blake3 fingerprint for a task.
///
/// Hashes:
///   1. The command string (bytes)
///   2. All source files in `pkg_dir` (*.ts *.js *.tsx *.jsx *.mts *.cts *.rs *.go *.py *.json),
///      excluding `node_modules/`, `target/`, `dist/`, `.git/` directories,
///      sorted by path for determinism.
///
/// Returns the hex-encoded 32-byte blake3 hash.
pub fn fingerprint_task(command: &str, pkg_dir: &Path) -> Result<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn same_command_same_empty_dir_same_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("echo hello", dir.path()).unwrap();
        let h2 = fingerprint_task("echo hello", dir.path()).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_command_different_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("echo hello", dir.path()).unwrap();
        let h2 = fingerprint_task("echo world", dir.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn adding_source_file_changes_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        fs::write(dir.path().join("index.ts"), b"export const x = 1;").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_ne!(h1, h2, "adding a source file should change the fingerprint");
    }

    #[test]
    fn node_modules_excluded() {
        let dir = tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("dep.ts"), b"// should be ignored").unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        // Modifying node_modules should NOT change the hash
        fs::write(nm.join("dep.ts"), b"// changed").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h1, h2, "node_modules changes should not affect fingerprint");
    }

    #[test]
    fn non_source_file_excluded() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        fs::write(dir.path().join("README.md"), b"# readme").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h1, h2, ".md files should not affect fingerprint");
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let dir = tempdir().unwrap();
        let h = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h.len(), 64, "blake3 hex output is 64 chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonexistent_dir_still_hashes_command() {
        // A package with no directory on disk (e.g. in-memory test package)
        // should still return a fingerprint based on the command alone.
        let h = fingerprint_task("echo test", Path::new("/tmp/nonexistent-rage-test-xyz")).unwrap();
        assert_eq!(h.len(), 64);
    }
}
```

2. Run `cargo test -p cache 2>&1 | tail -15` — confirm all 7 tests fail with `not yet implemented`.

3. Implement `fingerprint_task`:

```rust
use anyhow::Result;
use blake3::Hasher;
use std::path::Path;
use walkdir::WalkDir;

pub fn fingerprint_task(command: &str, pkg_dir: &Path) -> Result<String> {
    let mut hasher = Hasher::new();

    // 1. Hash the command string
    hasher.update(command.as_bytes());

    // 2. Collect and sort source files (deterministic order)
    let mut files: Vec<std::path::PathBuf> = WalkDir::new(pkg_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prune excluded directories — don't descend into them
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    "node_modules" | "target" | "dist" | ".git"
                )
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let ext = e
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "rs" | "go" | "py" | "json"
            )
        })
        .map(|e| e.into_path())
        .collect();

    files.sort();

    // 3. Hash each source file's contents
    for file in files {
        let contents = std::fs::read(&file)
            .unwrap_or_default(); // missing file → treat as empty (tolerant)
        hasher.update(&contents);
    }

    Ok(hasher.finalize().to_hex().to_string())
}
```

Note: use `unwrap_or_default()` (not `?`) for individual file reads inside the loop so that files deleted between directory scan and read don't abort the whole fingerprint.

4. Add `tempfile = "3"` to `[dev-dependencies]` in `crates/cache/Cargo.toml`.

5. Run `cargo test -p cache 2>&1 | tail -15` — confirm all 7 tests pass.

6. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

7. Commit:
```
feat(cache): fingerprint_task — blake3 hash of command + source files
```

---

## Task 3: Implement `CacheEntry` and `CacheProvider` trait

**Goal:** Define the data model and the trait that all cache backends implement.

### Steps

1. Write tests in `crates/cache/src/entry.rs` first:

```rust
//! Cache entry data model.

use serde::{Deserialize, Serialize};

/// A stored result for a task execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheEntry {
    /// The fingerprint (blake3 hex) that produced this entry.
    pub fingerprint: String,
    /// The command that was executed.
    pub command: String,
    /// Exit code of the task (0 = success).
    pub exit_code: i32,
    /// Wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Unix timestamp (seconds) when the entry was stored.
    pub cached_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let entry = CacheEntry {
            fingerprint: "abc123".to_string(),
            command: "echo hello".to_string(),
            exit_code: 0,
            elapsed_ms: 42,
            cached_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: CacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn fields_serialize_with_snake_case_names() {
        let entry = CacheEntry {
            fingerprint: "fp".to_string(),
            command: "cmd".to_string(),
            exit_code: 1,
            elapsed_ms: 100,
            cached_at: 0,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"fingerprint\""));
        assert!(json.contains("\"exit_code\""));
        assert!(json.contains("\"elapsed_ms\""));
        assert!(json.contains("\"cached_at\""));
    }
}
```

2. Implement `entry.rs` (the struct and derives are already in the test file — just move them to the non-test section and remove the `todo!()`).

3. Write `crates/cache/src/provider.rs`:

```rust
//! CacheProvider trait — implemented by LocalCache and any future remote backends.

use crate::entry::CacheEntry;
use anyhow::Result;

/// Abstraction over cache storage backends.
///
/// All implementations must be `Send + Sync` so they can be shared across
/// async task threads via `Arc<dyn CacheProvider>`.
pub trait CacheProvider: Send + Sync {
    /// Look up a cache entry by fingerprint key.
    ///
    /// Returns `None` on a miss (key not found or data corrupt).
    fn get(&self, key: &str) -> Option<CacheEntry>;

    /// Store a cache entry under the given fingerprint key.
    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()>;
}
```

No tests needed for the trait itself (it's a pure interface).

4. Update `lib.rs` re-exports to use the new types (they should already be there from Task 1).

5. Run `cargo test -p cache 2>&1 | tail -10` — confirm 9 tests pass (7 fingerprint + 2 entry).

6. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

7. Commit:
```
feat(cache): CacheEntry + CacheProvider trait
```

---

## Task 4: Implement `LocalCache`

**Goal:** `LocalCache` reads/writes `{key}.json` files under `~/.rage/cache/` (or a configurable test dir).

### Steps

1. Write the tests in `crates/cache/src/local.rs` first:

```rust
//! Local filesystem cache backend.

use crate::entry::CacheEntry;
use crate::provider::CacheProvider;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Cache backend that stores entries as JSON files on the local filesystem.
///
/// Default location: `~/.rage/cache/`.
/// Each entry is stored as `{fingerprint}.json`.
pub struct LocalCache {
    dir: PathBuf,
}

impl LocalCache {
    /// Create a LocalCache using the default directory (`~/.rage/cache/`).
    /// Creates the directory if it does not exist.
    pub fn new() -> Result<Self> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .context("HOME or USERPROFILE env var not set")?;
        Self::with_dir(PathBuf::from(home).join(".rage").join("cache"))
    }

    /// Create a LocalCache using the given directory.
    /// Creates the directory if it does not exist.
    pub fn with_dir(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache dir {}", dir.display()))?;
        Ok(Self { dir })
    }
}

impl CacheProvider for LocalCache {
    fn get(&self, key: &str) -> Option<CacheEntry> {
        let path = self.dir.join(format!("{key}.json"));
        let raw = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()> {
        let path = self.dir.join(format!("{key}.json"));
        let json = serde_json::to_string_pretty(entry)
            .context("serializing cache entry")?;
        std::fs::write(&path, json)
            .with_context(|| format!("writing cache entry to {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_entry(fp: &str) -> CacheEntry {
        CacheEntry {
            fingerprint: fp.to_string(),
            command: "echo test".to_string(),
            exit_code: 0,
            elapsed_ms: 10,
            cached_at: 0,
        }
    }

    #[test]
    fn miss_returns_none() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        assert!(cache.get("nonexistent-key").is_none());
    }

    #[test]
    fn put_then_get_roundtrips() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        let entry = sample_entry("abc123def456");
        cache.put("abc123def456", &entry).unwrap();
        let retrieved = cache.get("abc123def456").unwrap();
        assert_eq!(retrieved, entry);
    }

    #[test]
    fn creates_dir_if_missing() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("a").join("b").join("c");
        // sub does not exist yet
        let cache = LocalCache::with_dir(sub.clone()).unwrap();
        // dir was created
        assert!(sub.is_dir());
        // and cache works
        let entry = sample_entry("key1");
        cache.put("key1", &entry).unwrap();
        assert!(cache.get("key1").is_some());
    }

    #[test]
    fn corrupt_json_returns_none() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        // Write garbage JSON to a cache key file
        std::fs::write(dir.path().join("badkey.json"), b"not valid json").unwrap();
        assert!(cache.get("badkey").is_none(), "corrupt JSON should return None");
    }

    #[test]
    fn different_keys_stored_independently() {
        let dir = tempdir().unwrap();
        let cache = LocalCache::with_dir(dir.path().to_path_buf()).unwrap();
        let e1 = sample_entry("fp1");
        let e2 = CacheEntry {
            fingerprint: "fp2".to_string(),
            command: "cargo build".to_string(),
            exit_code: 0,
            elapsed_ms: 500,
            cached_at: 100,
        };
        cache.put("fp1", &e1).unwrap();
        cache.put("fp2", &e2).unwrap();
        assert_eq!(cache.get("fp1").unwrap(), e1);
        assert_eq!(cache.get("fp2").unwrap(), e2);
    }
}
```

2. Confirm RED: `cargo test -p cache --lib local 2>&1 | tail -10` shows all 5 tests failing.

3. The implementation is already written inline in the test file above. Remove `todo!()` if present, ensure the struct, impl, and `CacheProvider` impl compile and tests pass.

4. Run `cargo test -p cache 2>&1 | tail -10` — confirm **14 tests pass** (7 fingerprint + 2 entry + 5 local), 0 failed.

5. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

6. Commit:
```
feat(cache): LocalCache — JSON storage in ~/.rage/cache/
```

---

## Task 5: Integrate cache into `scheduler`

**Goal:** `run_tasks` and `run_single_task` check the local cache before executing. On a hit, print `✓ (cached)` and skip execution. On a miss, execute and store the result.

### Steps

**Prerequisite:** Add `cache` as a dependency to `scheduler`:

In `crates/scheduler/Cargo.toml`, add under `[dependencies]`:
```toml
cache = { path = "../cache" }
```

**Changes to `crates/scheduler/src/runner.rs`:**

1. Change `run_tasks` signature to accept an optional cache:
```rust
pub async fn run_tasks(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> anyhow::Result<()>
```

2. Update `run_tasks` body to pass the cache to each task:
```rust
pub async fn run_tasks(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> anyhow::Result<()> {
    let levels = compute_task_levels(dag, &tasks);

    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();

        for task in level {
            let cache_clone = cache.clone();
            set.spawn(run_single_task(task, cache_clone));
        }

        let mut first_error: Option<RunError> = None;

        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    set.abort_all();
                }
                Err(_join_err) => {
                    if first_error.is_none() {
                        first_error = Some(RunError::Killed {
                            package: "unknown".to_string(),
                            script: "unknown".to_string(),
                        });
                    }
                }
            }
        }

        if let Some(e) = first_error {
            return Err(e.into());
        }
    }

    Ok(())
}
```

3. Change `run_single_task` to:
```rust
async fn run_single_task(
    task: Task,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> Result<(), RunError> {
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    // Compute fingerprint if cache is provided
    let fingerprint = cache.as_ref().and_then(|_| {
        cache::fingerprint_task(&task.command, &task.cwd).ok()
    });

    // Check cache — on hit, print and return early
    if let (Some(fp), Some(c)) = (&fingerprint, &cache) {
        if c.get(fp).is_some() {
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
    }

    // Cache miss (or no cache) — execute the task
    eprintln!(
        "[rage] {}#{} starting",
        task.package_name, task.script_name
    );
    let start = Instant::now();

    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .status()
        .await
        .map_err(|e| RunError::Spawn {
            package: task.package_name.clone(),
            script: task.script_name.clone(),
            source: e,
        })?;

    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let elapsed_ms = elapsed.as_millis() as u64;

    if status.success() {
        // Store in cache on success
        if let (Some(fp), Some(c)) = (&fingerprint, &cache) {
            let entry = cache::CacheEntry {
                fingerprint: fp.clone(),
                command: task.command.clone(),
                exit_code: 0,
                elapsed_ms,
                cached_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            let _ = c.put(fp, &entry); // ignore cache write errors
        }
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name, task.script_name, elapsed_secs
        );
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        eprintln!(
            "[rage] {}#{} \u{2717} FAILED (exit {code})",
            task.package_name, task.script_name
        );
        Err(RunError::TaskFailed {
            package: task.package_name,
            script: task.script_name,
            code,
        })
    }
}
```

4. **Update all existing `run_tasks` call sites in `runner.rs` tests** — add `None` as the third argument. Find calls like `run_tasks(&dag, vec![task]).await` and change to `run_tasks(&dag, vec![task], None).await`.

5. Add new tests in `runner.rs` to verify cache integration:

```rust
#[tokio::test]
async fn task_is_cached_on_second_run() {
    use cache::LocalCache;
    use std::sync::Arc;
    use tempfile::tempdir;

    let cache_dir = tempdir().unwrap();
    let local = LocalCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
    let cache: Option<Arc<dyn cache::CacheProvider>> = Some(Arc::new(local));

    let pkg_dir = tempdir().unwrap();
    let task = Task {
        package_name: "cached-pkg".to_string(),
        script_name: "build".to_string(),
        command: "echo cached-test".to_string(),
        cwd: pkg_dir.path().to_path_buf(),
    };
    let pkg = mk_pkg("cached-pkg", &[]);
    let dag = build_dag(vec![pkg]).unwrap();

    // First run — should execute and write to cache
    run_tasks(&dag, vec![task.clone()], cache.clone()).await.unwrap();

    // Verify a cache entry was written (check cache_dir has at least one .json file)
    let json_files: Vec<_> = std::fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    assert!(!json_files.is_empty(), "cache entry should have been written");

    // Second run — should be a cache hit (same fingerprint)
    run_tasks(&dag, vec![task], cache).await.unwrap();
}

#[tokio::test]
async fn no_cache_option_executes_normally() {
    let task = Task {
        package_name: "uncached-pkg".to_string(),
        script_name: "build".to_string(),
        command: "echo no-cache-test".to_string(),
        cwd: PathBuf::from("/tmp"),
    };
    let pkg = mk_pkg("uncached-pkg", &[]);
    let dag = build_dag(vec![pkg]).unwrap();
    // None = no cache — should just execute
    run_tasks(&dag, vec![task], None).await.unwrap();
}
```

6. Run `cargo test -p scheduler 2>&1 | tail -15` — confirm **14 tests pass**, 0 failed (12 prior + 2 new).

7. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

8. Commit:
```
feat(scheduler): integrate cache — check on miss, store on hit
```

---

## Task 6: Update CLI — `--no-cache` flag and wire `LocalCache`

**Goal:** `rage run` uses the local cache by default. `--no-cache` bypasses it.

### Steps

**Changes to `crates/cli/src/main.rs`:**

1. Add `cache = { path = "../cache" }` to `crates/cli/Cargo.toml` under `[dependencies]`.

2. Add the `--no-cache` flag to the `Run` command in `Cli`:
```rust
Run {
    /// Script name to run (e.g. `build`, `test`, `lint`).
    script: String,

    /// Workspace root (defaults to cwd).
    #[arg(long)]
    workspace: Option<PathBuf>,

    /// Positional workspace path (overrides --workspace).
    workspace_pos: Option<PathBuf>,

    /// Disable the local cache — always re-execute tasks.
    #[arg(long)]
    no_cache: bool,
},
```

3. Update `cmd_run` to accept and use `no_cache`:
```rust
async fn cmd_run(root: &Path, script: &str, no_cache: bool) -> Result<()> {
    use cache::LocalCache;
    use std::sync::Arc;

    let pm = workspace_tools::detect_package_manager(root).with_context(|| {
        format!("{} is not a recognized JS workspace", root.display())
    })?;

    let raw = workspace_tools::discover_packages(root)
        .context("discovering workspace packages")?;
    let resolved = workspace_tools::build_package_graph(raw)
        .context("resolving package dependency edges")?;

    eprintln!("Found {} packages ({} workspace)", resolved.len(), pm.as_str());

    let dag = build_graph::dag::build_dag(resolved).context("building package DAG")?;

    let tasks = scheduler::task::build_task_list(&dag, script)
        .with_context(|| format!("no packages have a '{script}' script"))?;

    eprintln!("Running '{}' across {} packages", script, tasks.len());

    let cache: Option<Arc<dyn cache::CacheProvider>> = if no_cache {
        None
    } else {
        match LocalCache::new() {
            Ok(lc) => Some(Arc::new(lc)),
            Err(e) => {
                eprintln!("[rage] warning: cache unavailable: {e}");
                None
            }
        }
    };

    scheduler::run_tasks(&dag, tasks, cache)
        .await
        .with_context(|| format!("'{script}' run failed"))?;

    eprintln!("Done.");
    Ok(())
}
```

4. Update the `match` arm for `Command::Run` to pass `no_cache`:
```rust
Command::Run {
    script,
    workspace,
    workspace_pos,
    no_cache,
} => {
    let root = resolve_workspace(workspace_pos, workspace);
    cmd_run(&root, &script, no_cache).await
}
```

5. Add integration tests to `crates/cli/tests/integration.rs`:

```rust
// ── cache integration tests ─────────────────────────────────────────────────

#[test]
fn no_cache_flag_accepted() {
    // Verify --no-cache is a recognized flag (doesn't error with "unexpected argument")
    let bin = env!("CARGO_BIN_EXE_rage");
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("fixtures");
    let output = std::process::Command::new(bin)
        .args(["run", "build", "--no-cache"])
        .arg(fixtures_dir.join("js-pnpm"))
        .output()
        .unwrap();
    assert!(output.status.success(), "rage run build --no-cache should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Done."));
}

#[test]
fn second_run_uses_cache() {
    use tempfile::tempdir;
    // Use RAGE_CACHE_DIR env var to isolate cache per test run
    // (We use a temp dir so tests don't pollute ~/.rage/cache/ and are repeatable)
    let cache_dir = tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_rage");
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("fixtures");

    let run = |extra_args: &[&str]| {
        std::process::Command::new(bin)
            .args(["run", "build"])
            .arg(fixtures_dir.join("js-pnpm"))
            .args(extra_args)
            .env("RAGE_CACHE_DIR", cache_dir.path())
            .output()
            .unwrap()
    };

    // First run — cold cache
    let first = run(&[]);
    assert!(first.status.success());

    // Second run — warm cache
    let second = run(&[]);
    assert!(second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("(cached)"),
        "second run should show cached tasks, got:\n{stderr}"
    );
}
```

**Important:** The `second_run_uses_cache` test uses a `RAGE_CACHE_DIR` environment variable to override the cache directory. You must update `LocalCache::new()` to respect this env var:

```rust
pub fn new() -> Result<Self> {
    // Allow tests (and power users) to override the cache directory
    if let Ok(dir) = std::env::var("RAGE_CACHE_DIR") {
        return Self::with_dir(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("HOME or USERPROFILE env var not set")?;
    Self::with_dir(PathBuf::from(home).join(".rage").join("cache"))
}
```

Add `use tempfile::tempdir;` at the top of the test file if not already present. Also add `tempfile = "3"` to `[dev-dependencies]` in `crates/cli/Cargo.toml` if not already present.

6. Run `cargo test --workspace 2>&1 | tail -20` — confirm all tests pass (target: ≥ 71 tests, 0 failed).

7. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

8. Commit:
```
feat(cli): --no-cache flag + wire LocalCache with RAGE_CACHE_DIR override
```

---

## Task 7: Final Verification

**Goal:** End-to-end proof that the cache works. Two consecutive runs, second shows `(cached)`.

### Steps

1. Build the release binary:
```bash
cargo build --release 2>&1 | tail -5
```

2. Run the full test suite:
```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: ≥ 71 tests, 0 failed.

3. Cold cache run (uses a temp dir so it's fresh every time the test is run):
```bash
export RAGE_CACHE_DIR=$(mktemp -d)
./target/release/rage run build fixtures/js-pnpm 2>&1
```
Expected output (to stderr):
```
Found 4 packages (pnpm workspace)
Running 'build' across 4 packages
[rage] @fixture/core#build starting
building @fixture/core
[rage] @fixture/core#build ✓ <N>s
[rage] @fixture/utils#build starting
...
[rage] @fixture/app#build ✓ <N>s
Done.
```

4. Warm cache run (same `RAGE_CACHE_DIR`):
```bash
./target/release/rage run build fixtures/js-pnpm 2>&1
```
Expected output:
```
Found 4 packages (pnpm workspace)
Running 'build' across 4 packages
[rage] @fixture/core#build ✓ (cached)
[rage] @fixture/utils#build ✓ (cached)
[rage] @fixture/ui#build ✓ (cached)
[rage] @fixture/app#build ✓ (cached)
Done.
```

5. Verify `--no-cache` bypasses the cache:
```bash
./target/release/rage run build fixtures/js-pnpm --no-cache 2>&1
```
Expected: shows `starting` + timing for all 4 packages (no `(cached)`).

6. Print git log (last 15 commits):
```bash
git log --oneline -15
```

7. If all steps pass, report **STATUS: DONE**. If any step fails, report the exact failure and STOP.

---

## Expected Test Count Growth

| Phase | Tests |
|-------|-------|
| End of Phase 2 | 55 |
| After Task 2 (+7 fingerprint) | 62 |
| After Task 3 (+2 entry) | 64 |
| After Task 4 (+5 local) | 69 |
| After Task 5 (+2 scheduler) | 71 |
| After Task 6 (+2 CLI integration) | 73 |

---

## Failure Triage

**`blake3` compile error:** Check `blake3 = "1"` is in `crates/cache/Cargo.toml`. Run `cargo update` to fetch.

**`walkdir` not found:** Add `walkdir = "2"` to `crates/cache/Cargo.toml`.

**Clippy `clippy::arc_with_non_send_sync`:** Ensure `CacheProvider: Send + Sync` is declared in the trait. `LocalCache` itself must not hold any `!Send` fields (it only has `PathBuf`, which is `Send + Sync`).

**`run_tasks` call sites in tests fail to compile:** All `run_tasks` calls need a third argument. Pass `None::<std::sync::Arc<dyn cache::CacheProvider>>` or just `None` (type inference usually works).

**`second_run_uses_cache` integration test fails:** Check `RAGE_CACHE_DIR` is passed correctly via `.env()`. Verify `LocalCache::new()` reads `RAGE_CACHE_DIR` before falling back to `~/.rage/cache/`.

**Cache entries written to wrong location:** Ensure `LocalCache::with_dir` uses the path verbatim, not relative to cwd.

**Do NOT change test assertions to match broken behavior. STOP and report unexpected failures.**
