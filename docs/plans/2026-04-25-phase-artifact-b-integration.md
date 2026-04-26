# Observation-Driven Artifact Cache — Phase B: Runner Integration

> **For execution:** Use `/execute-plan` mode or the subagent-driven-development recipe.

**Goal:** Wire the Phase A primitives into the runner so that (1) every successful sandboxed task captures its read packages into the CAS in the background, (2) the root install task verifies its on-disk effects on a cache hit, and (3) when effects are missing but the CAS has the package set, restoration runs instead of the install command.

**Architecture:** Three integration points in `crates/scheduler/src/runner.rs`. After a successful `run_single_task_two_phase`, a `tokio::task::spawn_blocking` extracts packages from the pathset reads (via `plugin_typescript::pathset_extractor`), captures each via `artifact_store::capture_package`, and atomically updates `{cache_dir}/artifact-packages/{install_fp}.json`. In `run_root_task_two_phase`, before declaring a marker hit, the plugin's `verify_install_effects` is called; on `false`, `try_restore_from_cas` checks the manifest, verifies all hashes are present, and hardlinks every package back. Partial restoration is forbidden — if any hash is missing the function returns `Ok(false)` and the install command runs.

**Tech Stack:** Rust 2021, Tokio (existing), serde_json. CLI threads `Arc<LocalArtifactStore>` from `cmd_run` down through `run_tasks_two_phase`.

**Prerequisite:** Phase A complete and merged (`docs/plans/2026-04-25-phase-artifact-a-foundation.md`).

---

## COE constraints baked into this plan

1. **All-or-nothing restoration.** `try_restore_from_cas` returns `Ok(false)` if *any* required hash is missing — partial restore would silently corrupt the workspace.
2. **Capture is fire-and-forget.** Always `tokio::task::spawn_blocking` with no `.await` — the build never blocks on cache I/O.
3. **Workspace packages excluded.** Already enforced in Phase A's `extract_pnpm_packages` (no `.pnpm/` ⇒ no extraction).
4. **Scoped packages preserved.** Already enforced in Phase A's `restore_package`.
5. **Cross-device hardlinks fall back to copy.** Already enforced in Phase A's `LocalArtifactStore::link`.
6. **Manifest writes are atomic.** Tempfile + rename. Concurrent task captures must merge, not clobber.
7. **A failed restoration must not leave the workspace half-populated.** If `try_restore_from_cas` errors mid-flight, the install command runs and overwrites whatever was created.

---

## Storage layout (additive — new path, no changes to existing files)

```
{cache_dir}/
├── root-{fp}.done                              # existing marker, untouched
├── artifact-packages/
│   └── {install_fingerprint}.json              # WorkspacePackageManifest, this plan
└── ...
{cache_dir}/../artifacts/                        # i.e. sibling to {cache_dir}
└── content/{hex[0..2]}/{hex[2..]}/data         # the LocalArtifactStore content tree
```

The store sits **outside** the per-workspace cache dir on purpose: it can be shared across workspaces on the same machine because content addressing is global.

---

## File map

**Modified files:**
- `crates/scheduler/Cargo.toml` — add `artifact-store` and `plugin-typescript` deps
- `crates/scheduler/src/runner.rs` — three new integration points
- `crates/cli/Cargo.toml` — add `artifact-store`
- `crates/cli/src/main.rs` — construct `Arc<LocalArtifactStore>`, pass through
- `crates/cli/tests/integration.rs` — new e2e test (file may need to be created if it does not exist)

**New helper files (kept inside scheduler, not new crates):**
- `crates/scheduler/src/artifact_capture.rs` — capture-from-pathset helper
- `crates/scheduler/src/artifact_restore.rs` — `try_restore_from_cas`

---

## Task 1: Wire `artifact-store` into scheduler crate; add failing test for `verify_install_effects` call

**Files:**
- Modify: `crates/scheduler/Cargo.toml`
- Modify: `crates/scheduler/src/runner.rs` (test only)

**Step 1: Add dependencies.**

Edit `crates/scheduler/Cargo.toml`. Update `[dependencies]`:
```toml
[dependencies]
workspace-tools = { path = "../workspace-tools" }
build-graph = { path = "../build-graph" }
cache = { path = "../cache" }
pipeline-config = { path = "../pipeline-config" }
plugin = { path = "../plugin" }
plugin-typescript = { path = "../plugin-typescript" }
artifact-store = { path = "../artifact-store" }
sandbox = { path = "../sandbox" }
tokio = { version = "1", features = ["full"] }
thiserror = "2"
anyhow = "1"
serde_json = "1"
blake3 = "1"
walkdir = "2"
```

Run: `cargo check -p scheduler`
Expected: clean compile.

**Step 2: Write the failing test.**

Append to the bottom of `crates/scheduler/src/runner.rs` (or to its existing `#[cfg(test)] mod tests` if present — search for `mod tests` in the file). Add this test module if none exists:

```rust
#[cfg(test)]
mod artifact_integration_tests {
    use super::*;
    use plugin::EcosystemPlugin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    /// Plugin double that records `verify_install_effects` calls.
    #[derive(Default)]
    struct CountingPlugin {
        calls: StdArc<AtomicUsize>,
        result: bool,
    }
    impl EcosystemPlugin for CountingPlugin {
        fn id(&self) -> &'static str { "test-counting" }
        fn detection_globs(&self) -> Vec<&'static str> { vec![] }
        fn infer_tasks(&self, _: &std::path::Path) -> Vec<plugin::TaskDef> { vec![] }
        fn toolchain_allowlist(&self) -> Vec<plugin::AllowlistEntry> { vec![] }
        fn declared_input_globs(&self, _: &str, _: &plugin::PluginConfig) -> Vec<String> { vec![] }
        fn abi_fingerprint(&self, _: &[plugin::OutputFile]) -> Option<String> { None }
        fn verify_install_effects(&self, _: &std::path::Path) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result
        }
    }

    #[tokio::test]
    async fn root_task_with_marker_calls_verify_install_effects() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(cache::TwoPhaseCache::new(dir.path().to_path_buf()));

        // Build a root task and pre-create its marker so the runner sees a cache hit.
        let task = crate::task::Task {
            package_name: "workspace".into(),
            script_name: "install".into(),
            command: "echo should-not-run".into(),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            input_paths: vec![],
            env_hash_inputs: vec![],
            is_root: true,
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            declared_input_globs: vec![],
            output_globs: vec![],
            dep_package_names: vec![],
        };
        let fp = root_task_fingerprint(&task);
        std::fs::write(dir.path().join(format!("root-{fp}.done")), b"").unwrap();

        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(artifact_store::LocalArtifactStore::new(store_dir.path()));

        let calls = StdArc::new(AtomicUsize::new(0));
        let plugin = CountingPlugin { calls: calls.clone(), result: true };

        // Will fail to compile until run_root_task_two_phase grows plugin + store params.
        run_root_task_two_phase(task, cache, &plugin, store).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
```

**Step 3: Run — expect FAIL (compile error: too few arguments to `run_root_task_two_phase`).**

Run: `cargo test -p scheduler artifact_integration_tests::root_task_with_marker_calls_verify_install_effects`
Expected: compile error about `run_root_task_two_phase` argument count.

**Step 4: Commit (RED of TDD).**

Run: `git add crates/scheduler && git commit -m "wip(scheduler): failing test for verify_install_effects wiring"`

---

## Task 2: Thread `plugin` + `store` into `run_root_task_two_phase`; verify the call

**Files:**
- Modify: `crates/scheduler/src/runner.rs`

**Step 1: Update `run_root_task_two_phase` signature and body.**

Replace the existing function (currently around lines 631–686) with:
```rust
async fn run_root_task_two_phase(
    task: Task,
    cache: Arc<cache::TwoPhaseCache>,
    plugin: &dyn plugin::EcosystemPlugin,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> Result<(), RunError> {
    let fp = root_task_fingerprint(&task);
    let marker = cache.dir().join(format!("root-{fp}.done"));

    if marker.exists() {
        // Verify the install task's on-disk effects are still present.
        if plugin.verify_install_effects(&task.workspace_root) {
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
        // Effects gone — try CAS restoration before falling through to re-run.
        let manifest_path = cache
            .dir()
            .join("artifact-packages")
            .join(format!("{fp}.json"));
        match crate::artifact_restore::try_restore_from_cas(
            &manifest_path,
            &task.workspace_root,
            artifact_store.as_ref(),
        ) {
            Ok(true) => {
                eprintln!(
                    "[rage] {}#{} \u{2713} (restored from artifact cache)",
                    task.package_name, task.script_name
                );
                return Ok(());
            }
            Ok(false) => {
                // CAS miss or partial — fall through and re-run install.
                eprintln!(
                    "[rage] {}#{} marker present but effects missing — re-running",
                    task.package_name, task.script_name
                );
                // Best-effort: clear the marker so a partial run leaves the cache consistent.
                let _ = std::fs::remove_file(&marker);
            }
            Err(e) => {
                eprintln!(
                    "[rage] {}#{} CAS restore failed ({e}) — re-running",
                    task.package_name, task.script_name
                );
                let _ = std::fs::remove_file(&marker);
            }
        }
    }

    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
    let start = Instant::now();
    let system_path = std::env::var("PATH").unwrap_or_default();
    let new_path = crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .env("PATH", &new_path)
        .status()
        .await
        .map_err(|e| RunError::Spawn {
            package: task.package_name.clone(),
            script: task.script_name.clone(),
            source: e,
        })?;
    let elapsed = start.elapsed();

    if status.success() {
        let _ = std::fs::write(&marker, b"");
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name,
            task.script_name,
            elapsed.as_secs_f64()
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

**Step 2: Add stub modules so the references compile.**

Create `crates/scheduler/src/artifact_restore.rs`:
```rust
//! CAS-backed restoration of a workspace's installed packages.

use artifact_store::{ArtifactError, ArtifactStore, LocalArtifactStore, WorkspacePackageManifest};
use std::path::Path;

/// Attempt to restore `node_modules/` from the per-workspace package manifest.
///
/// Returns:
///   * `Ok(true)`  — every package in the manifest was hardlinked back into place.
///   * `Ok(false)` — the manifest is missing, or the CAS does not contain every
///                   required hash (partial restore is forbidden — never run).
///   * `Err(_)`    — unexpected I/O failure while reading the manifest.
pub fn try_restore_from_cas(
    manifest_path: &Path,
    workspace_root: &Path,
    store: &LocalArtifactStore,
) -> Result<bool, ArtifactError> {
    let _ = (manifest_path, workspace_root, store);
    Ok(false) // implemented in Task 4
}
```

Create `crates/scheduler/src/artifact_capture.rs`:
```rust
//! After-build hook: extract packages from a sandbox pathset and stuff them
//! into the per-package CAS, updating the workspace manifest atomically.

#![allow(dead_code)]

use artifact_store::LocalArtifactStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Spawn a fire-and-forget background task that captures every package
/// referenced by `pathset_reads` into the CAS, then merges the new
/// `PackageArtifact`s into `{manifest_path}` (atomic write).
///
/// All errors are swallowed: capture is best-effort and must never break a build.
pub fn schedule_capture(
    pathset_reads: Vec<PathBuf>,
    workspace_root: PathBuf,
    manifest_path: PathBuf,
    install_fingerprint: String,
    store: Arc<LocalArtifactStore>,
) {
    let _ = (pathset_reads, workspace_root, manifest_path, install_fingerprint, store);
    // implemented in Task 6
}

/// Test-visible synchronous variant — does the work inline. Used by integration tests.
pub fn capture_now(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    manifest_path: &Path,
    install_fingerprint: &str,
    store: &LocalArtifactStore,
) -> std::io::Result<()> {
    let _ = (pathset_reads, workspace_root, manifest_path, install_fingerprint, store);
    Ok(())
}
```

**Step 3: Wire the new modules in `crates/scheduler/src/lib.rs`.**

Open `crates/scheduler/src/lib.rs` and add (next to the other `mod` declarations):
```rust
pub mod artifact_capture;
pub mod artifact_restore;
```

**Step 4: Update the call site of `run_root_task_two_phase`.**

`run_single_task_two_phase` (around line 402) currently calls `run_root_task_two_phase(task, cache).await`. The runner doesn't know about the plugin or store yet — we'll thread those in next. For now, change `run_tasks_two_phase` to accept an extra `Arc<LocalArtifactStore>` and a `Box<dyn EcosystemPlugin>`.

Edit `run_tasks_two_phase` signature (currently lines 357–360):
```rust
pub async fn run_tasks_two_phase(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Arc<cache::TwoPhaseCache>,
    plugin: Arc<dyn plugin::EcosystemPlugin>,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> anyhow::Result<()> {
```

Inside the function, `set.spawn(run_single_task_two_phase(task, cache_clone))` becomes:
```rust
let plugin_clone = Arc::clone(&plugin);
let store_clone = Arc::clone(&artifact_store);
set.spawn(run_single_task_two_phase(task, cache_clone, plugin_clone, store_clone));
```

Edit `run_single_task_two_phase` (currently line 402) signature:
```rust
async fn run_single_task_two_phase(
    task: Task,
    cache: Arc<cache::TwoPhaseCache>,
    plugin: Arc<dyn plugin::EcosystemPlugin>,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> Result<(), RunError> {
    if task.is_root {
        return run_root_task_two_phase(task, cache, plugin.as_ref(), artifact_store).await;
    }
    // ... rest of the existing body unchanged for now
```

The unused `plugin` and `artifact_store` parameters in the non-root branch will be wired up in Task 6.

**Step 5: Update CLI call site to keep the workspace compiling.**

Search for the call to `run_tasks_two_phase` in `crates/cli/src/main.rs`:
```bash
grep -n 'run_tasks_two_phase' /Users/ken/workspace/ms/rage/crates/cli/src/main.rs
```

At that call site, build a temporary `Arc<dyn EcosystemPlugin>` and `Arc<LocalArtifactStore>`:
```rust
use std::sync::Arc as StdArc;
let plugin: StdArc<dyn plugin::EcosystemPlugin> = StdArc::new(plugin_typescript::TypeScriptPlugin::new());
// Place the store at <cache_dir>/../artifacts so it can be shared across workspaces on this host.
let store_root = cache_dir.parent().unwrap_or(std::path::Path::new(".")).join("artifacts");
let artifact_store = StdArc::new(artifact_store::LocalArtifactStore::new(&store_root));
scheduler::run_tasks_two_phase(&dag, tasks, cache, plugin, artifact_store).await?;
```

Add to `crates/cli/Cargo.toml` `[dependencies]`:
```toml
artifact-store = { path = "../artifact-store" }
```

**Step 6: Update the test from Task 1 to use the new signature.**

The test as written passes `&plugin` and the `Arc<LocalArtifactStore>` directly to `run_root_task_two_phase`, which already matches the signature you've now established. Run it.

Run: `cargo test -p scheduler root_task_with_marker_calls_verify_install_effects`
Expected: 1 test passes.

**Step 7: Run the workspace.**

Run: `cargo check --workspace`
Expected: clean compile.

**Step 8: Commit.**

Run:
```
git add crates/scheduler crates/cli
git commit -m "feat(scheduler): thread plugin + artifact_store through two-phase runner; verify_install_effects on cache hit"
```

---

## Task 3: When effects missing AND no manifest, fall through to re-run install

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (test)

**Step 1: Write the failing test.**

Append to `artifact_integration_tests`:
```rust
    #[tokio::test]
    async fn missing_effects_no_manifest_runs_install_command() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(cache::TwoPhaseCache::new(dir.path().to_path_buf()));

        // Place a sentinel file the install command will write.
        let sentinel = dir.path().join("ran.txt");
        let task = crate::task::Task {
            package_name: "workspace".into(),
            script_name: "install".into(),
            command: format!("touch {}", sentinel.display()),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            input_paths: vec![],
            env_hash_inputs: vec![],
            is_root: true,
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            declared_input_globs: vec![],
            output_globs: vec![],
            dep_package_names: vec![],
        };
        let fp = root_task_fingerprint(&task);
        // Marker exists, but effects missing AND no artifact-packages/{fp}.json:
        std::fs::write(dir.path().join(format!("root-{fp}.done")), b"").unwrap();

        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(artifact_store::LocalArtifactStore::new(store_dir.path()));

        // Plugin returns false → effects missing
        let plugin = CountingPlugin { calls: StdArc::new(AtomicUsize::new(0)), result: false };

        run_root_task_two_phase(task, cache, &plugin, store).await.unwrap();
        assert!(sentinel.exists(), "install command must have run");
    }
```

**Step 2: Run.**

Run: `cargo test -p scheduler missing_effects_no_manifest_runs_install_command`
Expected: PASS — Task 2's implementation already deletes the marker and re-runs the command when `try_restore_from_cas` returns `Ok(false)`, which is the current Task-2 stub default.

If the test fails (e.g. because of an error path), trace: `try_restore_from_cas` should return `Ok(false)` for a non-existent manifest. The stub returns `Ok(false)` unconditionally so it should pass — fix any issue in `run_root_task_two_phase` flow control.

**Step 3: Commit.**

Run: `git add crates/scheduler && git commit -m "test(scheduler): missing effects + no manifest re-runs install"`

---

## Task 4: Implement `try_restore_from_cas` (all-or-nothing semantics)

**Files:**
- Modify: `crates/scheduler/src/artifact_restore.rs`

**Step 1: Write the three failing tests.**

Append to `crates/scheduler/src/artifact_restore.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use artifact_store::{
        capture_package, ContentHash, LocalArtifactStore, PackageArtifact, PathsetPackageRef,
        WorkspacePackageManifest,
    };
    use std::path::PathBuf;

    fn write_manifest(path: &Path, m: &WorkspacePackageManifest) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, serde_json::to_vec(m).unwrap()).unwrap();
    }

    #[test]
    fn returns_false_when_manifest_missing() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());
        let result = try_restore_from_cas(
            &ws.path().join("does-not-exist.json"),
            ws.path(),
            &store,
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn returns_false_when_cas_missing_some_hashes() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let manifest = WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: "fp".into(),
            packages: vec![PackageArtifact {
                name: "ms".into(),
                version: "2.1.3".into(),
                files: vec![(PathBuf::from("index.js"), ContentHash::of(b"never stored"))],
            }],
        };
        let mp = ws.path().join("manifest.json");
        write_manifest(&mp, &manifest);

        let result = try_restore_from_cas(&mp, ws.path(), &store).unwrap();
        assert!(!result);
        // node_modules must NOT have been touched
        assert!(!ws.path().join("node_modules").exists());
    }

    #[test]
    fn returns_true_and_restores_files_when_all_hashes_present() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        // Capture a real package so the store contains real bytes
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("ms")).unwrap();
        std::fs::write(src.path().join("ms/index.js"), b"INDEX").unwrap();
        std::fs::write(src.path().join("ms/package.json"), br#"{"name":"ms","version":"2.1.3"}"#).unwrap();
        let pkg_ref = PathsetPackageRef {
            name: "ms".into(),
            version: "2.1.3".into(),
            package_root: src.path().join("ms"),
        };
        let artifact = capture_package(&pkg_ref, &store).unwrap();

        let manifest = WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: "fp".into(),
            packages: vec![artifact],
        };
        let mp = ws.path().join("manifest.json");
        write_manifest(&mp, &manifest);

        let result = try_restore_from_cas(&mp, ws.path(), &store).unwrap();
        assert!(result);
        assert_eq!(
            std::fs::read(ws.path().join("node_modules/ms/index.js")).unwrap(),
            b"INDEX"
        );
    }
}
```

**Step 2: Run — expect FAIL.**

Run: `cargo test -p scheduler artifact_restore::tests`
Expected: 1 test passes (the missing-manifest one matches the stub `Ok(false)`); the other two fail.

**Step 3: Implement `try_restore_from_cas`.**

Replace the stub function in `crates/scheduler/src/artifact_restore.rs`:
```rust
pub fn try_restore_from_cas(
    manifest_path: &Path,
    workspace_root: &Path,
    store: &LocalArtifactStore,
) -> Result<bool, ArtifactError> {
    let bytes = match std::fs::read(manifest_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let manifest: WorkspacePackageManifest = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => return Ok(false), // corrupt manifest — same as miss
    };

    // Pre-flight: every required hash must be present BEFORE we touch the
    // workspace. Partial restores are silent corruption — never allowed.
    for pkg in &manifest.packages {
        for (_, hash) in &pkg.files {
            if !store.contains(hash) {
                return Ok(false);
            }
        }
    }

    // All present — restore.
    let nm = workspace_root.join("node_modules");
    for pkg in &manifest.packages {
        artifact_store::restore_package(pkg, &nm, store)?;
    }
    Ok(true)
}
```

**Step 4: Run — expect PASS.**

Run: `cargo test -p scheduler artifact_restore::tests`
Expected: all 3 tests pass.

**Step 5: Commit.**

Run: `git add crates/scheduler && git commit -m "feat(scheduler): try_restore_from_cas — all-or-nothing CAS restoration"`

---

## Task 5: End-to-end: missing effects + valid manifest → restore path runs, install does NOT

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (test)

**Step 1: Write the failing test.**

Append to `artifact_integration_tests`:
```rust
    #[tokio::test]
    async fn missing_effects_with_valid_manifest_restores_without_running_install() {
        use artifact_store::{capture_package, PathsetPackageRef, WorkspacePackageManifest};

        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(cache::TwoPhaseCache::new(dir.path().to_path_buf()));
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(artifact_store::LocalArtifactStore::new(store_dir.path()));

        // Capture a fake "ms" package into the CAS
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("ms")).unwrap();
        std::fs::write(src.path().join("ms/index.js"), b"console.log(1)").unwrap();
        let artifact = capture_package(
            &PathsetPackageRef {
                name: "ms".into(),
                version: "2.1.3".into(),
                package_root: src.path().join("ms"),
            },
            store.as_ref(),
        )
        .unwrap();

        // Build the install task and write a manifest at {cache_dir}/artifact-packages/{fp}.json
        let sentinel = dir.path().join("install-ran.txt");
        let task = crate::task::Task {
            package_name: "workspace".into(),
            script_name: "install".into(),
            // If this command runs, the test fails.
            command: format!("touch {}", sentinel.display()),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            input_paths: vec![],
            env_hash_inputs: vec![],
            is_root: true,
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            declared_input_globs: vec![],
            output_globs: vec![],
            dep_package_names: vec![],
        };
        let fp = root_task_fingerprint(&task);
        std::fs::write(dir.path().join(format!("root-{fp}.done")), b"").unwrap();

        let manifest = WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: fp.clone(),
            packages: vec![artifact],
        };
        let mp = dir.path().join("artifact-packages").join(format!("{fp}.json"));
        std::fs::create_dir_all(mp.parent().unwrap()).unwrap();
        std::fs::write(&mp, serde_json::to_vec(&manifest).unwrap()).unwrap();

        // Plugin reports node_modules missing → triggers restore path
        let plugin = CountingPlugin { calls: StdArc::new(AtomicUsize::new(0)), result: false };

        run_root_task_two_phase(task, cache, &plugin, store).await.unwrap();

        // Install must NOT have run
        assert!(!sentinel.exists(), "install command must NOT have run");
        // node_modules must have been restored
        assert_eq!(
            std::fs::read(dir.path().join("node_modules/ms/index.js")).unwrap(),
            b"console.log(1)"
        );
    }
```

**Step 2: Run.**

Run: `cargo test -p scheduler missing_effects_with_valid_manifest_restores_without_running_install`
Expected: PASS — Task 2 + Task 4 already implement the full path.

If FAIL: re-read `run_root_task_two_phase` and `try_restore_from_cas`. The most likely bug is that the marker is deleted before restoration is attempted, or the manifest path is computed from the wrong fingerprint.

**Step 3: Commit.**

Run: `git add crates/scheduler && git commit -m "test(scheduler): valid manifest restores without re-running install"`

---

## Task 6: Capture hook — extract + store after each successful sandboxed task

**Files:**
- Modify: `crates/scheduler/src/artifact_capture.rs`
- Modify: `crates/scheduler/src/runner.rs`

**Step 1: Write the failing test for `capture_now` (sync variant — easier to test).**

Append to `crates/scheduler/src/artifact_capture.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use artifact_store::{LocalArtifactStore, WorkspacePackageManifest};

    #[test]
    fn capture_now_writes_manifest_with_pnpm_packages() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        // Build a fake pnpm virtual store layout so the extractor finds packages
        let pnpm_dir = ws.path().join("node_modules/.pnpm/ms@2.1.3/node_modules/ms");
        std::fs::create_dir_all(&pnpm_dir).unwrap();
        std::fs::write(pnpm_dir.join("index.js"), b"// ms").unwrap();
        std::fs::write(pnpm_dir.join("package.json"), br#"{"name":"ms","version":"2.1.3"}"#).unwrap();

        let pathset_reads = vec![
            pnpm_dir.join("index.js"),
            pnpm_dir.join("package.json"),
        ];
        let manifest_path = ws.path().join("artifact-packages/fp123.json");

        capture_now(
            &pathset_reads,
            ws.path(),
            &manifest_path,
            "fp123",
            &store,
        )
        .unwrap();

        let bytes = std::fs::read(&manifest_path).unwrap();
        let m: WorkspacePackageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m.install_fingerprint, "fp123");
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages[0].name, "ms");
        assert_eq!(m.packages[0].version, "2.1.3");
        // The captured package's files are now in the CAS
        for (_, h) in &m.packages[0].files {
            assert!(store.contains(h));
        }
    }
}
```

Run: `cargo test -p scheduler artifact_capture::tests`
Expected: FAIL (`capture_now` is a stub).

**Step 2: Implement `capture_now` and `schedule_capture`.**

Replace `crates/scheduler/src/artifact_capture.rs` body:
```rust
//! After-build hook: extract packages from a sandbox pathset and stuff them
//! into the per-package CAS, updating the workspace manifest atomically.

use artifact_store::{
    capture_package, ArtifactStore, LocalArtifactStore, PackageArtifact, PathsetPackageRef,
    WorkspacePackageManifest,
};
use plugin_typescript::pathset_extractor::{extract_pnpm_packages, PathsetPackageRef as TsPkgRef};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn schedule_capture(
    pathset_reads: Vec<PathBuf>,
    workspace_root: PathBuf,
    manifest_path: PathBuf,
    install_fingerprint: String,
    store: Arc<LocalArtifactStore>,
) {
    tokio::task::spawn_blocking(move || {
        // Best-effort: any error is silently swallowed.
        let _ = capture_now(
            &pathset_reads,
            &workspace_root,
            &manifest_path,
            &install_fingerprint,
            store.as_ref(),
        );
    });
}

pub fn capture_now(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    manifest_path: &Path,
    install_fingerprint: &str,
    store: &LocalArtifactStore,
) -> std::io::Result<()> {
    // 1) Discover packages from pnpm-style pathset reads.
    //    (Flat layout extraction is folded in here in a follow-up; pnpm covers
    //    the dominant case for rage's design partners.)
    let ts_refs: Vec<TsPkgRef> = extract_pnpm_packages(pathset_reads, workspace_root);
    if ts_refs.is_empty() {
        return Ok(());
    }

    // 2) Capture each package; map TsPkgRef → artifact_store::PathsetPackageRef.
    let mut artifacts: Vec<PackageArtifact> = Vec::with_capacity(ts_refs.len());
    for r in ts_refs {
        let pkg_ref = PathsetPackageRef {
            name: r.name,
            version: r.version,
            package_root: r.package_root,
        };
        match capture_package(&pkg_ref, store) {
            Ok(a) => artifacts.push(a),
            Err(_) => continue, // tolerate per-package capture errors
        }
    }
    if artifacts.is_empty() {
        return Ok(());
    }

    // 3) Merge into existing manifest (if any) — dedup by (name, version).
    let mut manifest = match std::fs::read(manifest_path) {
        Ok(b) => serde_json::from_slice::<WorkspacePackageManifest>(&b).unwrap_or_else(|_| {
            WorkspacePackageManifest {
                captured_at: 0,
                install_fingerprint: install_fingerprint.to_string(),
                packages: Vec::new(),
            }
        }),
        Err(_) => WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: install_fingerprint.to_string(),
            packages: Vec::new(),
        },
    };
    // Drop any prior entries that match a newly-captured (name, version)
    let new_keys: std::collections::HashSet<(String, String)> = artifacts
        .iter()
        .map(|a| (a.name.clone(), a.version.clone()))
        .collect();
    manifest
        .packages
        .retain(|p| !new_keys.contains(&(p.name.clone(), p.version.clone())));
    manifest.packages.extend(artifacts);
    manifest.packages.sort_by(|a, b| {
        a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version))
    });
    manifest.captured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    manifest.install_fingerprint = install_fingerprint.to_string();

    // 4) Atomic write: tempfile → rename.
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = manifest_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&manifest).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, manifest_path)?;
    Ok(())
}
```

**Step 3: Run the unit test — expect PASS.**

Run: `cargo test -p scheduler artifact_capture::tests::capture_now_writes_manifest_with_pnpm_packages`
Expected: 1 test passes.

**Step 4: Hook `schedule_capture` into `run_single_task_two_phase`.**

In `crates/scheduler/src/runner.rs`, locate the success branch of `run_single_task_two_phase` (around lines 569–610, just after `if exit_code == 0 {`). Just before the trailing `eprintln!("[rage] {}#{} \u{2713} ...", ...)`, add:

```rust
        // ── Observation-driven CAS capture (fire-and-forget) ─────────────
        // The install task's fingerprint is what the manifest is keyed against.
        // Compute it from any root task in `task.dep_package_names`-adjacent state…
        // simpler: pass the install_fp in via the existing root marker convention.
        // We read the most recently-written `root-*.done` and use its fp. This is a
        // best-effort heuristic; the manifest is a recovery aid, not a correctness gate.
        if !pathset.reads.is_empty() {
            let install_fp = find_latest_install_fingerprint(cache.dir());
            if let Some(fp) = install_fp {
                let manifest_path = cache
                    .dir()
                    .join("artifact-packages")
                    .join(format!("{fp}.json"));
                crate::artifact_capture::schedule_capture(
                    pathset.reads.clone(),
                    task.workspace_root.clone(),
                    manifest_path,
                    fp,
                    Arc::clone(&artifact_store),
                );
            }
        }
```

Add the helper at the bottom of `runner.rs`:
```rust
/// Find the most recent `root-{fp}.done` marker in `cache_dir` and return its `fp`.
/// Used by the capture hook to key the manifest against the install fingerprint
/// without plumbing the root task's fp explicitly through every per-task call.
fn find_latest_install_fingerprint(cache_dir: &Path) -> Option<String> {
    use std::time::SystemTime;
    let entries = std::fs::read_dir(cache_dir).ok()?;
    let mut best: Option<(SystemTime, String)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("root-") else { continue };
        let Some(fp) = rest.strip_suffix(".done") else { continue };
        let mtime = entry.metadata().and_then(|m| m.modified()).ok()?;
        match &best {
            None => best = Some((mtime, fp.to_string())),
            Some((old, _)) if mtime > *old => best = Some((mtime, fp.to_string())),
            _ => {}
        }
    }
    best.map(|(_, fp)| fp)
}
```

Add `use std::path::Path;` to the file's imports if not already present (it is, around line 7 — `use std::path::PathBuf;`; ensure `Path` is also in scope).

**Step 5: Workspace check.**

Run: `cargo check --workspace`
Expected: clean.

Run: `cargo test -p scheduler`
Expected: all scheduler tests pass.

**Step 6: Commit.**

Run: `git add crates/scheduler && git commit -m "feat(scheduler): capture packages from pathset after sandboxed builds"`

---

## Task 7: CLI plumbing — construct `LocalArtifactStore` and pass it through

**Files:**
- Modify: `crates/cli/Cargo.toml`
- Modify: `crates/cli/src/main.rs`

**Step 1: Confirm Task 2's stub already added the dep and call site. Verify.**

Run: `grep -n 'artifact-store' /Users/ken/workspace/ms/rage/crates/cli/Cargo.toml`
Expected: a line containing `artifact-store = { path = "../artifact-store" }`. If not, add it under `[dependencies]`.

Run: `grep -n 'LocalArtifactStore\|run_tasks_two_phase' /Users/ken/workspace/ms/rage/crates/cli/src/main.rs`
Expected: the construction added in Task 2 Step 5.

**Step 2: Tighten the store path.**

Locate the construction in `cli/src/main.rs`:
```rust
let store_root = cache_dir.parent().unwrap_or(std::path::Path::new(".")).join("artifacts");
```

If `cache_dir` resolves to something like `~/.rage/cache/<workspace>`, the store ends up at `~/.rage/cache/artifacts`. Refactor so the store sits parallel to `cache/`, not inside it:
```rust
// Cache dir convention: <RAGE_HOME>/cache/<workspace-id>
// Place the artifact store at <RAGE_HOME>/artifacts/ so it's shared across workspaces.
let store_root = cache_dir
    .parent()                         // <RAGE_HOME>/cache
    .and_then(|p| p.parent())         // <RAGE_HOME>
    .map(|p| p.join("artifacts"))
    .unwrap_or_else(|| cache_dir.join("artifacts"));
std::fs::create_dir_all(&store_root).ok();
let artifact_store = StdArc::new(artifact_store::LocalArtifactStore::new(&store_root));
```

**Step 3: Build.**

Run: `cargo build -p rage-cli`
Expected: clean compile.

**Step 4: Smoke run on a synthetic workspace.**

```
mkdir -p /tmp/rage-cli-smoke && cd /tmp/rage-cli-smoke
echo '{}' > package.json
/Users/ken/workspace/ms/rage/target/debug/rage --help > /dev/null
```
Expected: exit 0.

**Step 5: Commit.**

Run:
```
cd /Users/ken/workspace/ms/rage
git add crates/cli
git commit -m "feat(cli): construct shared LocalArtifactStore at <RAGE_HOME>/artifacts"
```

---

## Task 8: Integration test — pnpm fixture writes a manifest

**Files:**
- Modify (or create): `crates/cli/tests/integration.rs`

**Step 1: Check whether the file exists.**

Run: `ls /Users/ken/workspace/ms/rage/crates/cli/tests/ 2>/dev/null`

If `integration.rs` doesn't exist, create it with the standard preamble. Otherwise, append the new test to it.

**Step 2: Write the failing test.**

Append (or create with):
```rust
//! End-to-end integration tests for observation-driven artifact cache.

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore] // requires real sandbox dylib + pnpm — run manually
async fn pnpm_workspace_capture_writes_manifest() {
    use std::process::Command;

    let ws = std::path::Path::new("/tmp/rage-symlink-poc/pnpm-test");
    if !ws.join("node_modules").exists() {
        eprintln!("skip: pnpm fixture not present at {ws:?}");
        return;
    }

    // Locate built rage binary
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("target/debug/rage");
    assert!(bin.exists(), "build rage first: cargo build -p rage-cli");

    // Run the build with sandbox + capture enabled
    let status = Command::new(&bin)
        .args(["run", "build"])
        .current_dir(ws)
        .status()
        .unwrap();
    assert!(status.success(), "rage run build failed");

    // Find the manifest under <RAGE_HOME>/cache/<workspace-id>/artifact-packages/<fp>.json
    // Resolve <RAGE_HOME>:
    let rage_home = std::env::var("RAGE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs_like_home().join(".rage"));
    let cache_root = rage_home.join("cache");
    let manifests: Vec<_> = walkdir::WalkDir::new(&cache_root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("root-"))
        .filter(|e| e.path().to_string_lossy().contains("/artifact-packages/"))
        .collect();
    // Manifests sit in artifact-packages/<fp>.json — find at least one.
    let any = walkdir::WalkDir::new(cache_root)
        .into_iter()
        .flatten()
        .any(|e| {
            let s = e.path().to_string_lossy().to_string();
            s.contains("/artifact-packages/") && s.ends_with(".json")
        });
    assert!(any, "no artifact-packages/<fp>.json manifest written; entries: {manifests:?}");
}

fn dirs_like_home() -> std::path::PathBuf {
    std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_default()
}
```

If `walkdir` is not yet a dev-dep, add to `crates/cli/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = "3"
walkdir = "2"
tokio = { version = "1", features = ["full"] }
```

**Step 3: Build the dylib + binary.**

```
cargo build -p sandbox-macos-dylib
cargo build -p rage-cli
```

**Step 4: Run the ignored test.**

Run: `cargo test -p rage-cli --test integration pnpm_workspace_capture_writes_manifest -- --ignored --nocapture`
Expected: skip if fixture missing; otherwise PASS with a manifest discovered.

**Step 5: Commit.**

Run: `git add crates/cli && git commit -m "test(cli): integration — pnpm capture writes manifest"`

---

## Task 9: Integration test — the "money test": delete `node_modules`, second run restores from CAS

**Files:**
- Modify: `crates/cli/tests/integration.rs`

**Step 1: Write the failing test.**

Append to `crates/cli/tests/integration.rs`:
```rust
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn pnpm_second_run_restores_from_cas_after_node_modules_wipe() {
    use std::process::Command;

    let ws = std::path::Path::new("/tmp/rage-symlink-poc/pnpm-test");
    if !ws.join("node_modules").exists() {
        eprintln!("skip: pnpm fixture not present at {ws:?}");
        return;
    }

    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .join("target/debug/rage");
    assert!(bin.exists(), "build rage first");

    // First run: populates the CAS + writes the manifest
    let s1 = Command::new(&bin).args(["run", "build"]).current_dir(ws).status().unwrap();
    assert!(s1.success(), "first run failed");

    // Wipe node_modules to simulate a fresh CI machine.
    let nm = ws.join("node_modules");
    let _ = std::fs::remove_dir_all(&nm);
    assert!(!nm.exists());

    // Second run: must restore via CAS, not re-run pnpm install.
    let out = Command::new(&bin)
        .args(["run", "build"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    println!("--- second-run stderr ---\n{stderr}");
    assert!(out.status.success(), "second run failed");

    assert!(
        stderr.contains("(restored from artifact cache)"),
        "expected restoration log line, got: {stderr}"
    );
    assert!(
        !stderr.contains("workspace#install starting"),
        "install should not have been re-executed"
    );

    // node_modules should exist with at least one restored package
    assert!(nm.exists(), "node_modules must be restored");
    assert!(
        std::fs::read_dir(&nm).unwrap().next().is_some(),
        "node_modules must be non-empty"
    );
}
```

**Step 2: Run.**

Run: `cargo test -p rage-cli --test integration pnpm_second_run_restores_from_cas_after_node_modules_wipe -- --ignored --nocapture`
Expected: PASS, with `(restored from artifact cache)` in stderr.

If FAIL: this is the most likely place to hit a real bug. Likely culprits:
- `find_latest_install_fingerprint` resolves to the wrong fp (so the manifest is keyed differently than the install task expects). Consider passing the install fp explicitly via a dedicated channel instead.
- `verify_install_effects` returns true even when only `.bin/` exists. Tighten its check.
- Capture happens after the test process exits → no manifest. The fire-and-forget `tokio::task::spawn_blocking` may need a synchronous wait at end of process, or the runner must `JoinSet::join_all` capture tasks before returning.

**For the runtime-shutdown race:** the simplest fix — add an `Arc<JoinSet>` for capture tasks to `run_tasks_two_phase` and `await` it after the final wave. Implement only if the test reveals the race.

**Step 3: Commit.**

Run: `git add crates/cli && git commit -m "test(cli): integration — second run restores from CAS"`

---

## Task 10: Confirm scoped-package restoration in the e2e flow

**Files:**
- Modify: `crates/cli/tests/integration.rs` (additional assertion or new test)

**Step 1: Add an assertion to the second-run test.**

Inside `pnpm_second_run_restores_from_cas_after_node_modules_wipe`, after the assertion that `node_modules` is non-empty, append:
```rust
    // If the fixture has any scoped package, ensure its @scope/name layout is preserved.
    let mut found_scoped = false;
    for entry in std::fs::read_dir(&nm).unwrap().flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n.starts_with('@') {
            // Inside @scope, every entry must be a directory (the package).
            for sub in std::fs::read_dir(entry.path()).unwrap().flatten() {
                assert!(sub.path().is_dir(), "scoped pkg {sub:?} must be a directory");
                found_scoped = true;
            }
        }
    }
    if !found_scoped {
        eprintln!("note: pnpm fixture has no scoped packages — scoped layout assertion skipped");
    }
```

**Step 2: Re-run.**

Run: `cargo test -p rage-cli --test integration pnpm_second_run_restores_from_cas_after_node_modules_wipe -- --ignored --nocapture`
Expected: PASS.

If it fails because scoped packages restored as `node_modules/types/node/...` instead of `node_modules/@types/node/...`, the bug is in `restore_package` (Phase A); fix it there and re-run.

**Step 3: Commit.**

Run: `git add crates/cli && git commit -m "test(cli): assert scoped @scope/name layout preserved on restore"`

---

## Task 11: Quality gate + lage smoke test

**Files:**
- None — verification only.

**Step 1: Workspace test.**

Run: `cargo test --workspace`
Expected: all tests pass. Investigate and fix any regression.

**Step 2: Clippy.**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Fix any.

**Step 3: Smoke test against `~/workspace/lage`.**

```
cd ~/workspace/lage
~/workspace/ms/rage/target/debug/rage run build 2>&1 | tee /tmp/rage-lage-1.log
ls -la node_modules | head -5
rm -rf node_modules
~/workspace/ms/rage/target/debug/rage run build 2>&1 | tee /tmp/rage-lage-2.log
```

Expected:
- First log: `workspace#install starting` then a build.
- Second log: `workspace#install ✓ (restored from artifact cache)` (or similar) and the build runs from cache.
- `node_modules/` is restored.

If the first run does not show the capture taking effect (i.e. nothing in `~/.rage/cache/<id>/artifact-packages/`), the runtime-shutdown race in Task 9 has reappeared — fix it now.

**Step 4: No commit needed — verification only.**

---

## Task 12: Final integration commit

**Files:**
- None — final commit folds in any tweaks made during Task 11 verification.

**Step 1: Stage anything modified during verification.**

```
cd /Users/ken/workspace/ms/rage
git status
```

If anything was modified during Task 11, run `git add -A`.

**Step 2: Final commit.**

```
git commit -m "feat(scheduler,cli): observation-driven package CAS — capture from pathset + restore on cache hit

Phase B of the artifact-cache rollout. After every successful sandboxed build,
read packages are captured into the per-package CAS in the background. When
workspace#install hits its cache marker but node_modules is missing (fresh CI),
the runner restores from the manifest via hardlinks instead of re-running
pnpm install. Partial restores are forbidden — if any required content is
missing the install command runs as a fallback.

Closes the work started in docs/plans/2026-04-25-phase-artifact-a-foundation.md
+ docs/plans/2026-04-25-phase-artifact-b-integration.md."
```

If there are no pending changes, this commit is a no-op — the per-task commits already cover all functionality.

---

## Definition of Done

- [ ] `verify_install_effects` is consulted on every root-task cache hit
- [ ] `try_restore_from_cas` is all-or-nothing — never produces a partial workspace
- [ ] `schedule_capture` runs in `tokio::task::spawn_blocking` and never blocks the build
- [ ] `{cache_dir}/artifact-packages/{fp}.json` is written atomically and merges across multiple captures
- [ ] CLI constructs `Arc<LocalArtifactStore>` at `<RAGE_HOME>/artifacts` and threads it through `run_tasks_two_phase`
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] Manual smoke test: lage → first run captures, delete `node_modules`, second run restores via CAS (no `pnpm install` executed)
- [ ] Scoped packages (`@types/node`) restore to correct `@scope/name` directories
- [ ] Cross-device hardlink failures fall back to copy silently (Phase A guarantee, exercised here)

**Phase B complete.** Next: distributed task execution (deferred).
