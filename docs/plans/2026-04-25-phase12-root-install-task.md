# Phase 12: Root Install Task via the EcosystemPlugin Trait

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Make `pnpm install` / `yarn install` / `npm install` (and, eventually, `uv sync`, `go mod download`, `cargo fetch`) a first-class root task that runs once at the workspace root before any package tasks, cached by lockfile content. The package-manager-specific knowledge lives **inside the ecosystem plugin** — not the scheduler.

**Architecture:** Add a `RootTask` struct and an `infer_root_tasks(workspace_root)` method to the `EcosystemPlugin` trait. The TypeScript plugin implements it for JS by detecting a lockfile (`pnpm-lock.yaml` → `pnpm install`, `yarn.lock` → `yarn install`, `package-lock.json` → `npm install`). `build_task_list` accepts `&[&dyn EcosystemPlugin]`, calls `infer_root_tasks` on every plugin, and prepends each result as a synthetic `workspace#<name>` task with `is_root: true` and `input_paths` set to the lockfile path. `compute_task_levels` partitions root tasks into wave 0 alone; package tasks shift down to wave 1+. Root-task fingerprinting hashes `command + concatenated input_path contents` (blake3) and bypasses the sandbox.

**Tech Stack:** Rust 1.75+, Tokio, blake3, anyhow / thiserror, tempfile (dev), `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.

---

## COE constraints (read before Task 1, do not violate)

1. **The scheduler must NOT hardcode package manager detection.** All PM-specific logic (which lockfile, which command) lives inside the plugin. `build_task_list` simply iterates plugins and asks each one for its root tasks.
2. **Root tasks bypass the sandbox.** `pnpm install` legitimately writes to `node_modules` — running it under `SandboxMode::Strict`/`Observed` would either block writes or generate spurious warnings. `build_task_list_with_config` must force `SandboxMode::Loose` for any task with `is_root: true`.
3. **Root tasks live in wave 0.** `compute_task_levels` partitions `is_root: true` into a dedicated wave 0; all package tasks shift down by one level when at least one root task is present.
4. **Scope filters must NOT drop root tasks.** `--since`/`--affected` filters by `package_name`, but `"workspace"` is not a package — root tasks must be unconditionally retained.
5. **The TypeScript plugin's `infer_root_tasks` must detect the package manager from lockfile *file presence***, not from the `workspace_tools::PackageManager` enum. The plugin layer does not depend on the scheduler's PM detection.
6. **`infer_root_tasks` is the extensibility point.** A future `PythonPlugin` returns `[uv sync]` keyed on `uv.lock`. A future `GoPlugin` returns `[go mod download]` keyed on `go.sum`. A `RustPlugin` may return `[]` (cargo handles deps during build). The trait signature must support all of these without scheduler changes.
7. `unsafe_code = "forbid"` is set workspace-wide. Do not add `unsafe` blocks.
8. **Do not** add a real `pnpm-lock.yaml` to `fixtures/js-pnpm/` — integration tests stage their own tempdir workspaces.

---

## Background reading (do this first, before Task 1)

Open these files in your editor and skim them — every reference in the plan assumes you have read them:

| File | What lives here |
|------|-----------------|
| `crates/plugin/src/lib.rs` | `EcosystemPlugin` trait — you will add `RootTask` + `infer_root_tasks` |
| `crates/plugin/src/types.rs` | `TaskDef`, `OutputFile`, `AllowlistEntry`, `PluginConfig` |
| `crates/plugin-typescript/src/lib.rs` | `TypeScriptPlugin` — you will add `impl` for `infer_root_tasks` here |
| `crates/scheduler/src/task.rs` | `Task` struct, `build_task_list`, `build_task_list_with_config` |
| `crates/scheduler/src/runner.rs` | `compute_task_levels`, `run_single_task`, `run_single_task_two_phase` |
| `crates/scheduler/src/lib.rs` | crate `pub use` re-exports |
| `crates/scheduler/Cargo.toml` | scheduler dependencies (you will add `plugin` and `blake3` here) |
| `crates/cli/src/main.rs` | `cmd_run` calls `build_task_list_with_config` and dispatches to `run_tasks` / `run_tasks_two_phase` |
| `crates/cli/Cargo.toml` | cli dependencies (you will add `plugin-typescript`) |
| `crates/cli/tests/integration.rs` | end-to-end binary tests using `CARGO_BIN_EXE_rage` |
| `crates/cache/src/two_phase.rs` | `TwoPhaseCache::with_dir`, `TwoPhaseCache::dir() -> &Path` |
| `fixtures/js-pnpm/` | four packages, no `pnpm-lock.yaml` |
| `fixtures/js-yarn/` | packages plus `yarn.lock` |

---

## Task 1 — Add `RootTask` + `infer_root_tasks` to the `EcosystemPlugin` trait

**Files:**
- Modify: `crates/plugin/src/lib.rs`

**Step 1.1 — Read the current trait.**
```bash
sed -n '1,60p' crates/plugin/src/lib.rs
```
Confirm you see the six existing methods: `id`, `detection_globs`, `infer_tasks`, `toolchain_allowlist`, `declared_input_globs`, `abi_fingerprint`.

**Step 1.2 — Add the `RootTask` struct and the new trait method.**
Replace the entire content of `crates/plugin/src/lib.rs` with:
```rust
//! Ecosystem plugin contract.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! ecosystem plugins centralize the declaration burden — they tell rage what
//! TypeScript / Rust / Go / Python packages typically read, write, and emit.
//! User-supplied config augments these defaults.

pub mod types;

pub use types::{AllowlistEntry, OutputFile, PluginConfig, TaskDef};

use std::path::{Path, PathBuf};

/// A workspace-level task that runs ONCE at the workspace root before any
/// per-package task. Examples:
///
/// - TypeScript / JavaScript: `pnpm install`, `yarn install`, `npm install`
/// - Python (future):         `uv sync`
/// - Go (future):              `go mod download`
/// - Rust (future):            `cargo fetch` — or no root task at all
///
/// Cache fingerprinting uses `command` plus the contents of every path in
/// `input_paths` (typically the lockfile). Changing the lockfile invalidates
/// the cache; a stable lockfile + same command = cache hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootTask {
    /// Human-readable script name. Becomes the `script_name` of the synthesized
    /// task (e.g. `"install"` → log line `workspace#install`).
    pub name: String,
    /// Shell command to execute at the workspace root.
    pub command: String,
    /// Files whose contents fingerprint this task. Most ecosystems supply a
    /// single lockfile path; an empty vec is allowed (the command alone
    /// becomes the fingerprint, which is rarely what you want).
    pub input_paths: Vec<PathBuf>,
}

/// Implemented by each ecosystem (TypeScript, Rust, Go, ...).
///
/// A plugin is a value type; the runtime stores `Box<dyn EcosystemPlugin>`.
pub trait EcosystemPlugin: Send + Sync {
    /// Stable plugin id, used to look up `plugins_config.<id>` in `rage.json`.
    fn id(&self) -> &'static str;

    /// Globs (workspace-relative) that signal a package belongs to this plugin.
    /// Example: TypeScript returns `["tsconfig.json", "tsconfig.*.json"]`.
    fn detection_globs(&self) -> Vec<&'static str>;

    /// Given a package root, return the task definitions this plugin offers.
    /// Implementations may inspect manifest files (e.g. `package.json` scripts)
    /// to specialize.
    fn infer_tasks(&self, root: &Path) -> Vec<TaskDef>;

    /// Files the toolchain reads on every invocation but that are not package
    /// inputs (system libs, compiler internals). Used to suppress "undeclared
    /// read" warnings.
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;

    /// Globs feeding the *weak fingerprint* for cache lookup.
    ///
    /// `task_name` is the script being run (`build`, `typecheck`, ...).
    /// `config` carries `extend`/`exclude` overrides from `rage.json`.
    fn declared_input_globs(&self, task_name: &str, config: &PluginConfig) -> Vec<String>;

    /// Optional ABI fingerprint of `outputs`.
    ///
    /// Returns a deterministic hex string identifying the *semantic* shape of
    /// the outputs (e.g. TypeScript hashes `.d.ts` files; Go hashes exported
    /// symbols). Used for downstream early-cutoff: if a package's ABI didn't
    /// change, dependents may skip rebuilds.
    ///
    /// Returns `None` when this plugin doesn't support ABI-level cutoffs.
    fn abi_fingerprint(&self, outputs: &[OutputFile]) -> Option<String>;

    /// Root tasks for this ecosystem — run ONCE at workspace root before any
    /// package tasks. Return an empty vec if this ecosystem has no preparation
    /// step (e.g. Rust/cargo handles dep fetch transparently during build).
    ///
    /// Implementations should detect their preparation step from filesystem
    /// signals (lockfile presence, manifest contents) and NOT from any
    /// scheduler-level enum, so the plugin remains self-contained.
    ///
    /// Default impl returns `vec![]` so existing implementors don't break;
    /// each ecosystem opts in by overriding.
    fn infer_root_tasks(&self, _workspace_root: &Path) -> Vec<RootTask> {
        Vec::new()
    }
}
```

**Step 1.3 — Confirm the trait still compiles standalone.**
```bash
cargo check -p plugin 2>&1 | tail -10
```
Expected: clean compile. (The default impl means existing impls don't yet need to override.)

**Step 1.4 — Commit.**
```bash
git add crates/plugin/src/lib.rs
git commit -m "feat(plugin): add RootTask + EcosystemPlugin::infer_root_tasks (default empty)"
```

---

## Task 2 — Failing test: `TypeScriptPlugin::infer_root_tasks` detects pnpm lockfile

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs` (test module only)

**Step 2.1 — Add the failing test at the bottom of the existing `mod tests` block in `crates/plugin-typescript/src/lib.rs`** (just before the closing `}` of the module):
```rust
    // ── infer_root_tasks tests ──────────────────────────────────────────────

    #[test]
    fn infer_root_tasks_detects_pnpm_lockfile() {
        use plugin::RootTask;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1, "exactly one install task for pnpm");
        let t = &tasks[0];
        assert_eq!(t.name, "install");
        assert_eq!(t.command, "pnpm install");
        assert_eq!(t.input_paths, vec![dir.path().join("pnpm-lock.yaml")]);
        // Confirm the type round-trips
        let _: RootTask = t.clone();
    }
```

**Step 2.2 — Run the test to confirm it fails.**
```bash
cargo test -p plugin-typescript infer_root_tasks_detects_pnpm_lockfile 2>&1 | tail -15
```
Expected: the test compiles (because `infer_root_tasks` has a default impl returning `vec![]`) and FAILS the first assertion: `assertion `left == right` failed: exactly one install task for pnpm — left: 0, right: 1`. Good — that's the failure we want.

**Step 2.3 — Commit failing test.**
```bash
git add crates/plugin-typescript/src/lib.rs
git commit -m "test(plugin-typescript): failing test for pnpm root install detection"
```

---

## Task 3 — Implement: `TypeScriptPlugin::infer_root_tasks` for pnpm + yarn + npm

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 3.1 — Implement the method.**
Add this method INSIDE the `impl EcosystemPlugin for TypeScriptPlugin` block in `crates/plugin-typescript/src/lib.rs`, immediately after `abi_fingerprint`:
```rust
    fn infer_root_tasks(&self, workspace_root: &Path) -> Vec<plugin::RootTask> {
        // Detect the JS package manager from lockfile presence.
        // Priority: pnpm > yarn > npm. Returning at most one root task —
        // the "install" step for the detected manager.
        let pnpm_lock = workspace_root.join("pnpm-lock.yaml");
        if pnpm_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "pnpm install".to_string(),
                input_paths: vec![pnpm_lock],
            }];
        }

        let yarn_lock = workspace_root.join("yarn.lock");
        if yarn_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "yarn install".to_string(),
                input_paths: vec![yarn_lock],
            }];
        }

        let npm_lock = workspace_root.join("package-lock.json");
        if npm_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "npm install".to_string(),
                input_paths: vec![npm_lock],
            }];
        }

        // No lockfile found — no install task. (A future heuristic could
        // fall back to package.json#packageManager; that's out of scope.)
        Vec::new()
    }
```

**Step 3.2 — Add the imports needed by the method body.**
At the top of `crates/plugin-typescript/src/lib.rs`, the existing `use plugin::{...};` line must export `RootTask`. The body uses `plugin::RootTask` directly so no additional `use` is required, but make sure `plugin` is the one path the method references. Verify:
```bash
grep -n "^use plugin" crates/plugin-typescript/src/lib.rs
```
Expected: one line of the form `use plugin::{AllowlistEntry, EcosystemPlugin, OutputFile, PluginConfig, TaskDef};`. Leave it as-is.

**Step 3.3 — Run the pnpm test.**
```bash
cargo test -p plugin-typescript infer_root_tasks_detects_pnpm_lockfile 2>&1 | tail -10
```
Expected: `test result: ok. 1 passed`.

**Step 3.4 — Commit.**
```bash
git add crates/plugin-typescript/src/lib.rs
git commit -m "feat(plugin-typescript): implement infer_root_tasks for JS package managers"
```

---

## Task 4 — Add yarn / npm / no-lockfile coverage tests

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs` (test module only)

**Step 4.1 — Add three more tests at the bottom of `mod tests`** (just below the pnpm test from Task 2):
```rust
    #[test]
    fn infer_root_tasks_detects_yarn_lockfile() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"# yarn lockfile v1\n").unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "install");
        assert_eq!(tasks[0].command, "yarn install");
        assert_eq!(tasks[0].input_paths, vec![dir.path().join("yarn.lock")]);
    }

    #[test]
    fn infer_root_tasks_detects_npm_package_lock() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package-lock.json"),
            br#"{"lockfileVersion":3,"requires":true,"packages":{}}"#,
        ).unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "install");
        assert_eq!(tasks[0].command, "npm install");
        assert_eq!(tasks[0].input_paths, vec![dir.path().join("package-lock.json")]);
    }

    #[test]
    fn infer_root_tasks_returns_empty_when_no_lockfile() {
        let dir = tempdir().unwrap();
        // Empty workspace — no lockfile of any kind.
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert!(tasks.is_empty(), "no lockfile must yield no root tasks");
    }

    #[test]
    fn infer_root_tasks_prefers_pnpm_over_yarn_over_npm() {
        // If multiple lockfiles are present (rare, but possible during a migration),
        // pnpm wins, then yarn, then npm.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"v: 6\n").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"v1\n").unwrap();
        std::fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "pnpm install");
    }
```

**Step 4.2 — Run the new tests.**
```bash
cargo test -p plugin-typescript infer_root_tasks 2>&1 | tail -15
```
Expected: `test result: ok. 5 passed; 0 failed` (the four new ones plus the pnpm test from Task 2).

**Step 4.3 — Commit.**
```bash
git add crates/plugin-typescript/src/lib.rs
git commit -m "test(plugin-typescript): cover yarn, npm, no-lockfile, and PM precedence"
```

---

## Task 5 — Add `is_root` + `input_paths` fields to `Task` and fix all construction sites

**Files:**
- Modify: `crates/scheduler/src/task.rs`
- Modify: `crates/scheduler/src/runner.rs`

**Step 5.1 — Replace the `Task` struct.**
In `crates/scheduler/src/task.rs`, replace the existing `Task` struct (lines 8–21) with:
```rust
/// A single unit of work: run `script_name` for `package_name`.
#[derive(Debug, Clone)]
pub struct Task {
    /// Package name, e.g. `@fixture/core`. For root tasks this is `"workspace"`.
    pub package_name: String,
    /// Script name to run, e.g. `build`. For root tasks this is the
    /// `name` field of the originating `RootTask` (e.g. `"install"`).
    pub script_name: String,
    /// Shell command to execute. For root tasks this comes verbatim from
    /// `RootTask::command` (e.g. `"pnpm install"`).
    pub command: String,
    /// Working directory. Package root for normal tasks; workspace root for root tasks.
    pub cwd: PathBuf,
    /// Sandbox mode to apply when executing this task.
    pub sandbox_mode: pipeline_config::SandboxMode,
    /// `true` for synthetic workspace-level tasks (e.g. `workspace#install`).
    /// Root tasks always run alone in wave 0 before any package task.
    pub is_root: bool,
    /// Files whose contents are hashed to fingerprint a root task
    /// (e.g. `pnpm-lock.yaml`). Empty for non-root tasks.
    pub input_paths: Vec<PathBuf>,
}
```

**Step 5.2 — Update the existing `tasks.push(Task { ... })` inside `build_task_list`** (~line 61) to include the two new fields:
```rust
            tasks.push(Task {
                package_name: pkg_name.clone(),
                script_name: script_name.to_string(),
                command: cmd,
                cwd: pkg.path.clone(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: false,
                input_paths: Vec::new(),
            });
```

**Step 5.3 — Update inline `Task { ... }` in `task.rs` test module.**
The `task_carries_sandbox_mode` test (~line 171). Add the two fields at the end:
```rust
        let t = Task {
            package_name: "x".to_string(),
            script_name: "build".to_string(),
            command: "echo".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Strict,
            is_root: false,
            input_paths: Vec::new(),
        };
```

**Step 5.4 — Update inline `Task { ... }` in `runner.rs`.**
Find every construction site:
```bash
grep -n "package_name:" crates/scheduler/src/runner.rs
```
You should see seven `Task { ... }` blocks total (one in `mk_task` helper plus six in test bodies). Add `is_root: false,` and `input_paths: Vec::new(),` as the last two fields in every one. Example for `mk_task` (~line 417):
```rust
    fn mk_task(name: &str) -> Task {
        Task {
            package_name: name.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {name}"),
            cwd: PathBuf::from(format!("/tmp/{name}")),
            sandbox_mode: pipeline_config::SandboxMode::default(),
            is_root: false,
            input_paths: Vec::new(),
        }
    }
```
Repeat for: `single_successful_task_runs`, `failing_task_returns_error`, both tasks in `two_independent_tasks_both_run`, `task_is_cached_on_second_run`, `no_cache_option_executes_normally`, `task_logs_sandbox_mode_in_starting_line`, `two_phase_cache_first_run_misses_second_run_hits`.

**Step 5.5 — Verify scheduler compiles + existing tests pass.**
```bash
cargo test -p scheduler --no-fail-fast 2>&1 | tail -20
```
Expected: every existing test still passes (no behavioural change yet — just two new fields with default values).

**Step 5.6 — Commit.**
```bash
git add crates/scheduler/
git commit -m "refactor(scheduler): add is_root/input_paths fields with default values"
```

---

## Task 6 — Failing test: `build_task_list` accepts plugins and prepends a pnpm root install

**Files:**
- Modify: `crates/scheduler/src/task.rs` (test module only)

**Step 6.1 — Add the test to the bottom of the `tests` module in `crates/scheduler/src/task.rs`** (just before the closing `}`):
```rust
    #[test]
    fn pnpm_workspace_includes_root_install_first() {
        use plugin::EcosystemPlugin;
        use plugin_typescript::TypeScriptPlugin;
        use tempfile::tempdir;
        use workspace_tools::{build_package_graph, discover_packages};

        // Stage a pnpm workspace inside a tempdir so we control whether
        // the lockfile exists. Copy the four js-pnpm packages over.
        let work = tempdir().unwrap();
        let src = fixtures_dir().join("js-pnpm");
        // Recursively copy fixture into tempdir so it's mutable.
        copy_dir_recursive(&src, work.path());
        // Stage a lockfile so the TypeScript plugin will detect pnpm.
        std::fs::write(work.path().join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();

        let raw = discover_packages(work.path()).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();

        let ts = TypeScriptPlugin::new();
        let plugins: Vec<&dyn EcosystemPlugin> = vec![&ts];

        let tasks = build_task_list(&dag, "build", work.path(), &plugins).unwrap();

        // First task is the synthesized workspace#install root task.
        assert!(tasks[0].is_root, "first task must be flagged is_root");
        assert_eq!(tasks[0].package_name, "workspace");
        assert_eq!(tasks[0].script_name, "install");
        assert_eq!(tasks[0].command, "pnpm install");
        assert_eq!(tasks[0].cwd, work.path());
        assert_eq!(tasks[0].input_paths, vec![work.path().join("pnpm-lock.yaml")]);

        // Followed by 4 package build tasks, none flagged is_root.
        assert_eq!(tasks.len(), 5, "1 install + 4 package builds");
        assert!(tasks[1..].iter().all(|t| !t.is_root));
    }

    /// Recursively copy a directory tree. Test helper only.
    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap().flatten() {
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir_recursive(&from, &to);
            } else {
                std::fs::copy(&from, &to).unwrap();
            }
        }
    }
```

**Step 6.2 — Add `plugin` and `plugin-typescript` as dev-dependencies of `scheduler`.**
Edit `crates/scheduler/Cargo.toml`:
```toml
    [dev-dependencies]
    tempfile = "3"
    plugin = { path = "../plugin" }
    plugin-typescript = { path = "../plugin-typescript" }
```
And add `plugin` as a regular dependency (the trait is part of the public surface of `build_task_list`):
```toml
    [dependencies]
    workspace-tools = { path = "../workspace-tools" }
    build-graph = { path = "../build-graph" }
    cache = { path = "../cache" }
    pipeline-config = { path = "../pipeline-config" }
    plugin = { path = "../plugin" }
    sandbox = { path = "../sandbox" }
    tokio = { version = "1", features = ["full"] }
    thiserror = "2"
    anyhow = "1"
    serde_json = "1"
    blake3 = "1"
```
(`blake3` is also added — Task 11 will need it.)

**Step 6.3 — Run the test to confirm it fails to compile.**
```bash
cargo test -p scheduler pnpm_workspace_includes_root_install_first 2>&1 | tail -15
```
Expected: a compile error of the form `this function takes 2 arguments but 4 arguments were supplied` — the call to `build_task_list(&dag, "build", work.path(), &plugins)` doesn't match today's two-arg signature. Good.

**Step 6.4 — Commit failing test + dependency wiring.**
```bash
git add crates/scheduler/
git commit -m "test(scheduler): failing test for plugin-driven root install task"
```

---

## Task 7 — Implement: change `build_task_list` to accept `&[&dyn EcosystemPlugin]`

**Files:**
- Modify: `crates/scheduler/src/task.rs`

**Step 7.1 — Replace `build_task_list` with the plugin-aware version.**
In `crates/scheduler/src/task.rs`, replace the existing `build_task_list` (lines 29–77) with:
```rust
/// Build a task list for `script_name` from the workspace DAG.
///
/// For each plugin in `plugins`, calls `plugin.infer_root_tasks(workspace_root)`
/// and prepends every returned root task as a synthesized
/// `workspace#<root_task.name>` `Task` (with `is_root: true`). The scheduler
/// itself contains zero package-manager-specific knowledge — that lives in
/// the plugin.
///
/// - Returns tasks in topological order: first all root tasks (in plugin order),
///   then package tasks ordered by the DAG.
/// - Packages without `scripts.{script_name}` are silently skipped.
/// - Returns `TaskError::NoMatchingScript` if **no package** has the script —
///   even if root tasks were synthesized. (A workspace where nobody declares
///   the script is still an error: the user typo'd the script name.)
pub fn build_task_list(
    dag: &WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
) -> Result<Vec<Task>, TaskError> {
    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    // 1. Collect root tasks from every plugin (in plugin order, stable).
    let mut tasks: Vec<Task> = Vec::new();
    for p in plugins {
        for rt in p.infer_root_tasks(workspace_root) {
            tasks.push(Task {
                package_name: "workspace".to_string(),
                script_name: rt.name,
                command: rt.command,
                cwd: workspace_root.to_path_buf(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: true,
                input_paths: rt.input_paths,
            });
        }
    }

    // 2. Walk the package DAG and synthesize per-package tasks for `script_name`.
    let mut package_tasks_added = 0usize;
    for pkg_name in &order {
        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

        let manifest_path = pkg.path.join("package.json");
        let command = if let Ok(raw) = std::fs::read_to_string(&manifest_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                json.get("scripts")
                    .and_then(|s| s.get(script_name))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(cmd) = command {
            tasks.push(Task {
                package_name: pkg_name.clone(),
                script_name: script_name.to_string(),
                command: cmd,
                cwd: pkg.path.clone(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: false,
                input_paths: Vec::new(),
            });
            package_tasks_added += 1;
        }
    }

    if package_tasks_added == 0 {
        return Err(TaskError::NoMatchingScript(script_name.to_string()));
    }

    Ok(tasks)
}
```

**Step 7.2 — Update `build_task_list_with_config` to take `plugins` and force `Loose` for root tasks.**
Replace it with:
```rust
/// Build a task list with sandbox modes resolved against `RageConfig` policies.
///
/// `workspace_root` is used to compute each package's path relative to the
/// workspace for glob policy matching.
///
/// Root tasks (`is_root: true`) ALWAYS receive `SandboxMode::Loose`, regardless
/// of policy. Sandboxing the package manager would block legitimate writes to
/// `node_modules`.
pub fn build_task_list_with_config(
    dag: &build_graph::dag::WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
    config: &pipeline_config::RageConfig,
) -> Result<Vec<Task>, TaskError> {
    let mut tasks = build_task_list(dag, script_name, workspace_root, plugins)?;
    for task in &mut tasks {
        if task.is_root {
            // Root tasks bypass per-package sandbox policy — see COE constraint #2.
            task.sandbox_mode = pipeline_config::SandboxMode::Loose;
            continue;
        }
        let rel = task
            .cwd
            .strip_prefix(workspace_root)
            .unwrap_or(&task.cwd)
            .to_path_buf();
        task.sandbox_mode = pipeline_config::resolve_sandbox_mode(config, &rel);
    }
    Ok(tasks)
}
```

**Step 7.3 — Update the existing tests in `task.rs` for the new signature.**
Find every call site in the test module:
```bash
grep -n "build_task_list" crates/scheduler/src/task.rs
```
Three existing tests use `build_task_list(&dag, "build")` / `build_task_list(&dag, "test")` — update them to pass `&root` (or any path) plus an empty plugin slice:

```rust
    // no_matching_script_is_an_error  (~line 127)
    let dummy_root = PathBuf::from("/tmp");
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let err = build_task_list(&dag, "build", &dummy_root, &plugins).unwrap_err();

    // finds_build_tasks_in_pnpm_fixture  (~line 137)
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let tasks = build_task_list(&dag, "build", &root, &plugins).unwrap();
    // No plugins → no root task → 4 package tasks (unchanged from today).
    assert_eq!(tasks.len(), 4);
    // ... rest of assertions unchanged ...

    // skips_packages_without_the_script  (~line 159)
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let err = build_task_list(&dag, "test", &root, &plugins).unwrap_err();

    // task_fields_are_populated_correctly  (~line 212)
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let tasks = build_task_list(&dag, "build", &root, &plugins).unwrap();
```

And `task_list_with_config_resolves_sandbox_per_policy` (~line 183) — update its call:
```rust
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let tasks = build_task_list_with_config(&dag, "build", &root, &plugins, &cfg).unwrap();
```

**Step 7.4 — Run all `task` tests.**
```bash
cargo test -p scheduler --lib task::tests 2>&1 | tail -15
```
Expected: every test passes, including `pnpm_workspace_includes_root_install_first` from Task 6.

**Step 7.5 — Commit.**
```bash
git add crates/scheduler/src/task.rs
git commit -m "feat(scheduler): build_task_list takes plugins and prepends root tasks"
```

---

## Task 8 — Failing tests: `compute_task_levels` puts root tasks alone in wave 0

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (test module only)

**Step 8.1 — Add the tests at the bottom of the `compute_task_levels` test group** (just before the `// ── run_tasks tests ──` divider at ~line 510):
```rust
    #[test]
    fn root_task_alone_in_wave_zero_pushes_package_to_wave_one() {
        let root_task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![PathBuf::from("/tmp/pnpm-lock.yaml")],
        };
        let pkg_task = mk_task("core");
        let dag = build_dag(vec![mk_pkg("core", &[])]).unwrap();
        let levels = compute_task_levels(&dag, &[root_task, pkg_task]);
        assert_eq!(levels.len(), 2, "expected two waves: [install] then [core]");
        assert_eq!(levels[0].len(), 1);
        assert!(levels[0][0].is_root);
        assert_eq!(levels[0][0].package_name, "workspace");
        assert_eq!(levels[1].len(), 1);
        assert_eq!(levels[1][0].package_name, "core");
        assert!(!levels[1][0].is_root);
    }

    #[test]
    fn root_task_pushes_diamond_down_one_wave() {
        let root_task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![],
        };
        let mut tasks: Vec<Task> = ["core", "utils", "ui", "app"]
            .iter().map(|n| mk_task(n)).collect();
        tasks.insert(0, root_task);
        let packages = vec![
            mk_pkg("core", &[]),
            mk_pkg("utils", &["core"]),
            mk_pkg("ui", &["core", "utils"]),
            mk_pkg("app", &["ui", "core"]),
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // 1 install wave + 4 package waves
        assert_eq!(levels.len(), 5);
        assert!(levels[0][0].is_root);
        assert_eq!(levels[1][0].package_name, "core");
        assert_eq!(levels[2][0].package_name, "utils");
        assert_eq!(levels[3][0].package_name, "ui");
        assert_eq!(levels[4][0].package_name, "app");
    }

    #[test]
    fn no_root_tasks_means_no_extra_wave() {
        // Sanity: if there are no root tasks, behaviour matches the legacy version.
        let tasks: Vec<Task> = ["a", "b"].iter().map(|n| mk_task(n)).collect();
        let packages = vec![mk_pkg("a", &[]), mk_pkg("b", &[])];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 1, "two independent packages, no root → 1 wave");
        assert_eq!(levels[0].len(), 2);
    }
```

**Step 8.2 — Run the new tests; confirm failure.**
```bash
cargo test -p scheduler --lib runner::tests::root_task 2>&1 | tail -15
```
Expected: `root_task_alone_in_wave_zero_pushes_package_to_wave_one` panics with something like `expected two waves: [install] then [core] — left: 1, right: 2`. The current implementation drops the workspace task because `dag.packages.get("workspace")` returns `None`. That's the failure we want.

**Step 8.3 — Commit failing tests.**
```bash
git add crates/scheduler/src/runner.rs
git commit -m "test(scheduler): failing tests for root-task wave-0 partitioning"
```

---

## Task 9 — Implement: `compute_task_levels` partitions root tasks into wave 0

**Files:**
- Modify: `crates/scheduler/src/runner.rs`

**Step 9.1 — Replace `compute_task_levels` (lines 39–84)** with the partitioning version:
```rust
/// Group tasks into parallel execution waves.
///
/// Root tasks (`is_root: true`) are placed alone in wave 0 — they are not in
/// the package DAG and run before any package task. All package tasks shift
/// down by one wave when at least one root task is present.
///
/// Within a wave, tasks are sorted by package name for determinism.
pub fn compute_task_levels(dag: &WorkspaceDag, tasks: &[Task]) -> Vec<Vec<Task>> {
    // Partition: root tasks live in their own wave 0; package tasks go through
    // the normal topological levelling pass.
    let (root_tasks, package_tasks): (Vec<&Task>, Vec<&Task>) =
        tasks.iter().partition(|t| t.is_root);

    let task_map: HashMap<&str, &Task> = package_tasks
        .iter()
        .map(|t| (t.package_name.as_str(), *t))
        .collect();

    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    let mut level_of: HashMap<&str, usize> = HashMap::new();
    let mut package_levels: Vec<Vec<Task>> = Vec::new();

    for pkg_name in &order {
        if !task_map.contains_key(pkg_name.as_str()) {
            continue;
        }

        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

        let level = pkg
            .dependencies
            .iter()
            .filter(|dep| task_map.contains_key(dep.as_str()))
            .filter_map(|dep| level_of.get(dep.as_str()).copied())
            .max()
            .map(|max_dep_level| max_dep_level + 1)
            .unwrap_or(0);

        level_of.insert(pkg_name.as_str(), level);

        if level >= package_levels.len() {
            package_levels.resize_with(level + 1, Vec::new);
        }
        package_levels[level].push((*task_map[pkg_name.as_str()]).clone());
    }

    for level in &mut package_levels {
        level.sort_by(|a, b| a.package_name.cmp(&b.package_name));
    }

    // Prepend the root-task wave when there are any root tasks.
    if root_tasks.is_empty() {
        package_levels
    } else {
        let mut root_wave: Vec<Task> = root_tasks.into_iter().cloned().collect();
        root_wave.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        let mut out = Vec::with_capacity(package_levels.len() + 1);
        out.push(root_wave);
        out.extend(package_levels);
        out
    }
}
```

**Step 9.2 — Run the runner tests.**
```bash
cargo test -p scheduler --lib runner::tests 2>&1 | tail -25
```
Expected: every existing test still passes (none use `is_root: true` so behaviour is unchanged for them) AND the three new wave-zero tests pass.

**Step 9.3 — Commit.**
```bash
git add crates/scheduler/src/runner.rs
git commit -m "feat(scheduler): partition root tasks into wave 0 in compute_task_levels"
```

---

## Task 10 — Failing test: root-task fingerprint depends on lockfile content

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (test module only)

**Step 10.1 — Add the fingerprint tests at the very bottom of the runner `tests` module** (just before its closing `}`):
```rust
    // ── root task fingerprint tests ─────────────────────────────────────────

    #[test]
    fn root_task_fingerprint_changes_with_lockfile_contents() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let lock = dir.path().join("pnpm-lock.yaml");

        std::fs::write(&lock, b"version: 1\n").unwrap();
        let task_a = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![lock.clone()],
        };
        let fp_a = super::root_task_fingerprint(&task_a);

        // Same task, same lockfile bytes → same fingerprint.
        let fp_a_again = super::root_task_fingerprint(&task_a);
        assert_eq!(fp_a, fp_a_again);

        // Mutate the lockfile → fingerprint changes.
        std::fs::write(&lock, b"version: 2\n").unwrap();
        let fp_b = super::root_task_fingerprint(&task_a);
        assert_ne!(fp_a, fp_b, "fingerprint must change with lockfile contents");

        // Different command → different fingerprint.
        let task_c = Task { command: "yarn install".to_string(), ..task_a.clone() };
        let fp_c = super::root_task_fingerprint(&task_c);
        assert_ne!(fp_b, fp_c);
    }

    #[test]
    fn root_task_fingerprint_handles_missing_lockfile() {
        // Missing files are hashed as a deterministic sentinel — the fingerprint
        // is still stable, just different from the "file present" case.
        let task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![PathBuf::from("/this/does/not/exist/pnpm-lock.yaml")],
        };
        let fp1 = super::root_task_fingerprint(&task);
        let fp2 = super::root_task_fingerprint(&task);
        assert_eq!(fp1, fp2, "missing-file fingerprint must still be deterministic");
        assert!(!fp1.is_empty());
    }
```

**Step 10.2 — Run; confirm compile failure.**
```bash
cargo test -p scheduler root_task_fingerprint 2>&1 | tail -10
```
Expected: `cannot find function `root_task_fingerprint` in module `super``. Good.

**Step 10.3 — Commit failing test.**
```bash
git add crates/scheduler/src/runner.rs
git commit -m "test(scheduler): failing test for root_task_fingerprint helper"
```

---

## Task 11 — Implement `root_task_fingerprint` and route root tasks through it in both runners

**Files:**
- Modify: `crates/scheduler/src/runner.rs`

**Step 11.1 — Add the helper just below the `RunError` enum (~line 32).**
```rust
/// Compute a content-addressed fingerprint for a root task.
///
/// Hashes the command plus the contents of every path in `task.input_paths`.
/// Missing files are folded in as `missing:{path}\0` so the fingerprint
/// remains deterministic across runs.
pub(crate) fn root_task_fingerprint(task: &Task) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rage.root-task.v1\0");
    hasher.update(task.command.as_bytes());
    hasher.update(b"\0");
    for path in &task.input_paths {
        match std::fs::read(path) {
            Ok(bytes) => {
                hasher.update(b"present:");
                hasher.update(path.to_string_lossy().as_bytes());
                hasher.update(b"\0");
                hasher.update(&bytes);
            }
            Err(_) => {
                hasher.update(b"missing:");
                hasher.update(path.to_string_lossy().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}
```

**Step 11.2 — Add a root-task short-circuit at the very top of `run_single_task` (~line 138).**
Just inside the function body, before the existing `let fingerprint = ...` line, add:
```rust
    if task.is_root {
        return run_root_task_legacy(task, cache).await;
    }
```

Then add the helper function just below `run_single_task`:
```rust
async fn run_root_task_legacy(
    task: Task,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> Result<(), RunError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let fp = root_task_fingerprint(&task);

    // Cache hit?
    if let Some(c) = &cache {
        if c.get(&fp).is_some() {
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
    }

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

    if status.success() {
        if let Some(c) = &cache {
            let entry = cache::CacheEntry {
                fingerprint: fp.clone(),
                command: task.command.clone(),
                exit_code: 0,
                elapsed_ms: elapsed.as_millis() as u64,
                cached_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                pathset_reads: vec![],
            };
            let _ = c.put(&fp, &entry);
        }
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

**Step 11.3 — Add a root-task short-circuit at the top of `run_single_task_two_phase` (~line 287).**
Just inside the function body, before the existing `use cache::{CacheEntry, ...};` line, add:
```rust
    if task.is_root {
        return run_root_task_two_phase(task, cache).await;
    }
```

Then add the helper just below `run_single_task_two_phase`:
```rust
async fn run_root_task_two_phase(
    task: Task,
    cache: Arc<cache::TwoPhaseCache>,
) -> Result<(), RunError> {
    let fp = root_task_fingerprint(&task);
    let marker = cache.dir().join(format!("root-{fp}.done"));

    if marker.exists() {
        eprintln!(
            "[rage] {}#{} \u{2713} (cached)",
            task.package_name, task.script_name
        );
        return Ok(());
    }

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

    if status.success() {
        // Best-effort marker write — cache failures must not break a build.
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

**Step 11.4 — Run the full scheduler suite.**
```bash
cargo test -p scheduler --lib 2>&1 | tail -25
```
Expected: every test passes — including `root_task_fingerprint_changes_with_lockfile_contents` and `root_task_fingerprint_handles_missing_lockfile`.

**Step 11.5 — Commit.**
```bash
git add crates/scheduler/
git commit -m "feat(scheduler): root_task_fingerprint + bypass-sandbox runner branches"
```

---

## Task 12 — Wire the TypeScript plugin into `cmd_run` and fix the scope filter

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/Cargo.toml`

**Step 12.1 — Add `plugin` and `plugin-typescript` as dependencies of `rage-cli`.**
Edit `crates/cli/Cargo.toml` and add to `[dependencies]`:
```toml
    plugin = { path = "../plugin" }
    plugin-typescript = { path = "../plugin-typescript" }
```
(Alphabetize within the existing list as the file convention requires.)

**Step 12.2 — Update `cmd_run` to instantiate the plugin and pass it through.**
In `crates/cli/src/main.rs`, find the existing `build_task_list_with_config` call (~line 316):
```rust
    let mut tasks = scheduler::task::build_task_list_with_config(&dag, script, root, &config)
        .with_context(|| format!("no packages have a '{script}' script"))?;
```
Replace with:
```rust
    // Active ecosystem plugins. For Phase 12 the TypeScript plugin is the
    // only one, but the type signature already supports a heterogeneous
    // mix (e.g. JS + Python).
    let ts_plugin = plugin_typescript::TypeScriptPlugin::new();
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = vec![&ts_plugin];

    let mut tasks =
        scheduler::task::build_task_list_with_config(&dag, script, root, &plugins, &config)
            .with_context(|| format!("no packages have a '{script}' script"))?;
```

**Step 12.3 — Fix the scope filter so `--since`/`--affected` cannot drop root tasks.**
A few lines below (~line 320), the current filter is:
```rust
    if let Some(ref scope_set) = scope {
        tasks.retain(|t| scope_set.contains(&t.package_name));
```
Replace with:
```rust
    if let Some(ref scope_set) = scope {
        // Root tasks (workspace#install etc.) are never package-scoped — always retain.
        tasks.retain(|t| t.is_root || scope_set.contains(&t.package_name));
```

**Step 12.4 — Confirm the workspace builds.**
```bash
cargo build -p rage-cli 2>&1 | tail -10
```
Expected: clean compile, zero errors.

**Step 12.5 — Smoke run against a tempdir.** This proves the install task is *attempted first* even with no real `pnpm` installed:
```bash
TMP=$(mktemp -d)
cp -R fixtures/js-pnpm/. "$TMP/"
echo "lockfileVersion: 6" > "$TMP/pnpm-lock.yaml"
./target/debug/rage run build "$TMP" --no-cache 2>&1 | head -3
rm -rf "$TMP"
```
Expected: stderr begins with `Found 4 packages (pnpm workspace)` then `[rage] workspace#install starting`. The `pnpm install` will likely fail (no real pnpm installed) — that's fine for the smoke test; we only care that the install is attempted before any package task. **Do not proceed if `workspace#install` is missing from stderr.**

**Step 12.6 — Commit.**
```bash
git add crates/cli/
git commit -m "feat(cli): instantiate TypeScript plugin, route root tasks via build_task_list"
```

---

## Task 13 — Integration test: `workspace#install` runs before package tasks

**Files:**
- Modify: `crates/cli/tests/integration.rs`

**Strategy:** stage a self-contained tempdir workspace, drop a `pnpm` shim onto `PATH` that exits 0 (and records its argv to a log file), then assert stderr ordering.

**Step 13.1 — Append the helpers + test at the bottom of `integration.rs`.**
```rust
// ── root install task tests ─────────────────────────────────────────────

/// Stage a minimal pnpm workspace inside `dir`:
/// - workspace package.json
/// - pnpm-workspace.yaml referencing packages/*
/// - pnpm-lock.yaml so the TypeScript plugin detects pnpm
/// - one package `@fixture/install-test` with a `build` script
fn stage_pnpm_workspace(dir: &std::path::Path) {
    std::fs::write(
        dir.join("package.json"),
        br#"{"name":"root","private":true,"version":"0.0.0"}"#,
    ).unwrap();
    std::fs::write(
        dir.join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    ).unwrap();
    std::fs::write(dir.join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();
    let pkg = dir.join("packages").join("install-test");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@fixture/install-test","version":"1.0.0","scripts":{"build":"echo BUILT-PACKAGE"}}"#,
    ).unwrap();
}

/// Write a fake `pnpm` shim to `bin_dir` that records its argv to
/// `bin_dir/pnpm.log` and exits 0. Returns `bin_dir` for PATH prepending.
fn install_pnpm_shim(bin_dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin_dir).unwrap();
    let shim = bin_dir.join("pnpm");
    let log = bin_dir.join("pnpm.log");
    let script = format!(
        "#!/bin/sh\necho FAKE-PNPM \"$@\" >> '{}'\necho INSTALL-RAN\nexit 0\n",
        log.display()
    );
    std::fs::write(&shim, script).unwrap();
    let mut perm = std::fs::metadata(&shim).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).unwrap();
    bin_dir.to_path_buf()
}

#[test]
fn run_pnpm_install_runs_before_package_builds() {
    let work = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    stage_pnpm_workspace(work.path());
    install_pnpm_shim(bin.path());

    // Prepend shim dir to PATH so our fake `pnpm` is found first.
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin.path().display(), existing_path);

    // --no-cache exercises the legacy single-phase runner branch
    // (the two-phase branch is covered by the next test).
    let output = rage()
        .arg("run")
        .arg("build")
        .arg(work.path())
        .arg("--no-cache")
        .env("PATH", &new_path)
        .output()
        .expect("failed to run rage");

    let stderr = String::from_utf8(output.stderr).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "rage exited non-zero.\nstderr:\n{stderr}\nstdout:\n{stdout}"
    );

    // Ordering: workspace#install must appear before any package task.
    let install_idx = stderr
        .find("workspace#install")
        .unwrap_or_else(|| panic!("workspace#install missing from stderr:\n{stderr}"));
    let package_idx = stderr
        .find("@fixture/install-test#build")
        .unwrap_or_else(|| panic!("package task missing from stderr:\n{stderr}"));
    assert!(
        install_idx < package_idx,
        "workspace#install must precede package task. stderr:\n{stderr}"
    );

    // Shim was actually invoked.
    let log = std::fs::read_to_string(bin.path().join("pnpm.log")).unwrap();
    assert!(log.contains("FAKE-PNPM install"), "shim log: {log}");
}
```

**Step 13.2 — Run the new test.**
```bash
cargo test -p rage-cli --test integration run_pnpm_install_runs_before_package_builds 2>&1 | tail -20
```
Expected: `test result: ok. 1 passed`.

**Step 13.3 — Commit.**
```bash
git add crates/cli/tests/integration.rs
git commit -m "test(cli): integration test — workspace#install runs before package tasks"
```

---

## Task 14 — Integration test: install is cached on second run

**Files:**
- Modify: `crates/cli/tests/integration.rs`

**Step 14.1 — Append this test below the previous one.**
```rust
#[test]
fn run_pnpm_install_is_cached_on_second_run() {
    let work = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    stage_pnpm_workspace(work.path());
    install_pnpm_shim(bin.path());

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin.path().display(), existing_path);

    // HOME override so the default cache dir lives in our tempdir.
    // (cmd_run honours $HOME/.rage/cache when rage.json doesn't pin a dir.)
    let home = cache.path();

    // Run #1 — install runs.
    let out1 = rage()
        .arg("run")
        .arg("build")
        .arg(work.path())
        .env("PATH", &new_path)
        .env("HOME", home)
        .output()
        .expect("rage run #1");
    assert!(
        out1.status.success(),
        "first run failed:\nstderr:\n{}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let stderr1 = String::from_utf8(out1.stderr).unwrap();
    assert!(
        stderr1.contains("workspace#install starting"),
        "first run should execute install, got:\n{stderr1}"
    );

    // Run #2 — install must be cached.
    let out2 = rage()
        .arg("run")
        .arg("build")
        .arg(work.path())
        .env("PATH", &new_path)
        .env("HOME", home)
        .output()
        .expect("rage run #2");
    assert!(
        out2.status.success(),
        "second run failed:\nstderr:\n{}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stderr2 = String::from_utf8(out2.stderr).unwrap();
    assert!(
        stderr2.contains("workspace#install \u{2713} (cached)"),
        "second run should hit install cache, got:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("workspace#install starting"),
        "second run must NOT re-run install, got:\n{stderr2}"
    );

    // Sanity check: shim log shows exactly one install invocation.
    let log = std::fs::read_to_string(bin.path().join("pnpm.log")).unwrap();
    let invocations = log.matches("FAKE-PNPM install").count();
    assert_eq!(invocations, 1, "pnpm should run exactly once across both rage invocations; log:\n{log}");
}
```

**Step 14.2 — Run the test.**
```bash
cargo test -p rage-cli --test integration run_pnpm_install_is_cached_on_second_run 2>&1 | tail -25
```
Expected: `test result: ok. 1 passed`.

> **Debug hint:** if the second run *re-runs* install, confirm `run_root_task_two_phase` is checking `marker.exists()` against `cache.dir()` and that `cache.dir()` resolves to `$HOME/.rage/cache` for both invocations.

**Step 14.3 — Commit.**
```bash
git add crates/cli/tests/integration.rs
git commit -m "test(cli): integration test — workspace#install is cached on rerun"
```

---

## Task 15 — Full workspace verification + lage smoke

**Step 15.1 — Workspace tests.**
```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -30
```
Expected: every crate's `test result: ok. N passed; 0 failed`.

**Step 15.2 — Clippy (deny warnings).**
```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```
Expected: `Finished` with zero warnings. **If clippy yells about `unused_variables` on `plugins` somewhere, you forgot to thread it through.**

**Step 15.3 — Format check.**
```bash
cargo fmt --all -- --check 2>&1 | tail -5
```
Expected: empty output. If it complains, run `cargo fmt --all` and re-run tests + clippy.

**Step 15.4 — End-to-end smoke against the cloned lage repo.**
```bash
cargo build --release -p rage-cli
./target/release/rage run build ~/workspace/lage 2>&1 | head -5
```
Expected: stderr begins with `Found N packages (pnpm workspace)` then `[rage] workspace#install starting` (real `pnpm install` runs against lage — that's fine). Run it a *second* time:
```bash
./target/release/rage run build ~/workspace/lage 2>&1 | head -5
```
Expected: `[rage] workspace#install ✓ (cached)`. **If the second run does not show `(cached)`, do not proceed — debug `cache.dir()` resolution before claiming success.**

**Step 15.5 — Final commit (catch any stragglers).**
```bash
git status
git log --oneline -15
git add -A
git diff --cached --stat
git commit -m "chore(phase12): final verification pass" || echo "nothing to commit"
```

**Step 15.6 — One-line mental summary:**
> Phase 12 ships: `EcosystemPlugin::infer_root_tasks` + `RootTask` extend the trait; the TypeScript plugin detects pnpm/yarn/npm from lockfile presence; `build_task_list` accepts `&[&dyn EcosystemPlugin]` and prepends every plugin's root tasks; `compute_task_levels` partitions root tasks alone into wave 0; both runners hash lockfile content for the install fingerprint and bypass the sandbox; CLI wires the TypeScript plugin and excludes root tasks from `--since`/`--affected` filtering. A future `PythonPlugin` adds `uv sync` by implementing exactly one trait method, no scheduler change.

Done. Hand off to the orchestrator.

---

## Anti-patterns to avoid (do NOT do these)

- ❌ **Do not** put `match pm { Pnpm => "pnpm install", ... }` anywhere in `crates/scheduler/`. That logic lives in `plugin-typescript`. The scheduler must remain ecosystem-agnostic.
- ❌ **Do not** make `infer_root_tasks` non-default on the trait. Existing plugins (none yet but the design must accommodate them) should not be forced to implement it the moment they upgrade.
- ❌ **Do not** synthesize `workspace#install` as a `package.json` script — root tasks are synthesized in memory and live only in the `Task` list.
- ❌ **Do not** invoke the package manager in parallel with package tasks. Wave 0 must complete fully before wave 1 starts.
- ❌ **Do not** add a real `pnpm-lock.yaml` to `fixtures/js-pnpm/` — the integration tests stage their own workspaces.
- ❌ **Do not** swallow install failures. A failing install is a build failure; the task must propagate `RunError::TaskFailed`.
- ❌ **Do not** sandbox the install command. `pnpm install` legitimately writes to `node_modules`; sandboxing breaks it. `build_task_list_with_config` must force `SandboxMode::Loose` for `is_root: true` tasks.
- ❌ **Do not** use the package directory walker (`fingerprint_task`) for root tasks — it doesn't have workspace context and would hash the whole monorepo. The `root_task_fingerprint` helper is the only correct path.
- ❌ **Do not** filter root tasks out via `--since`/`--affected` scope. The fix in Task 12.3 (`t.is_root || scope_set.contains(...)`) is mandatory.
- ❌ **Do not** skip Step 12.5 / 15.4 — the lage smoke test is the single highest-signal end-to-end verification.
