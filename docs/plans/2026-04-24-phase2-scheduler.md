# Phase 2: Scheduler + `rage run` Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.
> **Prerequisite:** Phase 1b complete. `cargo test --workspace` passes (38 tests). `rage graph fixtures/js-pnpm` emits valid DOT.

**Goal:** Add a `scheduler` crate and a `rage run <task>` CLI command so that `rage run build fixtures/js-pnpm` executes the `build` script from each `package.json` in wave-parallel topological order — dependencies run before dependents, independent packages run concurrently.

**Architecture:**
- `scheduler` crate: `Task` struct, `build_task_list()`, `compute_task_levels()`, `run_tasks()` async
- `cli` update: new `Run` subcommand wiring everything together
- No cache, no sandbox, no daemon in this phase

**Tech Stack:** Rust 2021, `tokio` (full features) for async parallel process execution, `thiserror`/`anyhow` for errors.

**End state of Phase 2:**
- `cargo test --workspace` passes (38 + new tests)
- `rage run build fixtures/js-pnpm` executes `build` scripts for all 4 packages in topological order
- Independent packages in the same wave run concurrently (tokio JoinSet)
- Any failing task aborts the run with a non-zero exit code

**What's deferred:** cache (Phase 3), sandbox (Phase 4), daemon (Phase 5), scoping/affected (Phase 6).

---

## Context For The Implementer

You are implementing Phase 2 of the `rage` build system. The repo lives in a git worktree at a path like `/Users/ken/workspace/ms/rage/.worktrees/feat-phase-2-scheduler`. Use **the worktree path** for all file operations, never the main repo path.

The existing crate structure after Phase 1b:
```
crates/
├── workspace-tools/   - Package discovery, PM detection, dependency graph
├── build-graph/       - WorkspaceDag, topological sort, DOT output
├── pipeline-config/   - rage.json loader (skeleton)
└── cli/               - rage binary with `graph` subcommand
fixtures/
├── js-pnpm/           - 4 packages (core, utils, ui, app)
├── js-yarn/           - 3 packages (core, lib, app)
└── js-npm/            - 3 packages (shared, server, client)
```

Key public APIs available:
```rust
// workspace-tools
workspace_tools::detect_package_manager(root: &Path) -> Option<PackageManager>
workspace_tools::discover_packages(root: &Path) -> anyhow::Result<Vec<Package>>
workspace_tools::build_package_graph(packages: Vec<Package>) -> anyhow::Result<Vec<Package>>
workspace_tools::Package { name, version, path, dependencies }

// build-graph
build_graph::dag::build_dag(packages: Vec<Package>) -> Result<WorkspaceDag, DagError>
build_graph::topo::topological_sort(dag: &WorkspaceDag) -> Result<Vec<String>, DagError>
build_graph::dag::WorkspaceDag { graph, nodes, packages }
```

**Rules for this plan:**
- Follow each task's steps literally and in order.
- Do **not** add functionality beyond what this plan specifies.
- Commit after each task with the exact message given.
- TDD: write failing tests first, verify RED, then implement to GREEN.
- Run `cargo clippy --workspace --all-targets -- -D warnings` after each task and fix any warnings.
- If a test fails unexpectedly, STOP and report. Do not change the test to match broken behavior.
- All test paths use `env!("CARGO_MANIFEST_DIR")` for fixture resolution.
- Work in the **worktree** path, not the main repo path.

---

## Task 1: Update Fixtures With Build Scripts

**Why:** The fixture `package.json` files have no `scripts` field. `rage run build` needs a `build` script to execute.

**Files to modify (in worktree):**
- `fixtures/js-pnpm/packages/core/package.json`
- `fixtures/js-pnpm/packages/utils/package.json`
- `fixtures/js-pnpm/packages/ui/package.json`
- `fixtures/js-pnpm/packages/app/package.json`
- `fixtures/js-yarn/packages/core/package.json`
- `fixtures/js-yarn/packages/lib/package.json`
- `fixtures/js-yarn/packages/app/package.json`
- `fixtures/js-npm/packages/shared/package.json`
- `fixtures/js-npm/packages/server/package.json`
- `fixtures/js-npm/packages/client/package.json`

**Step 1:** Add `"scripts": { "build": "echo 'building <name>'" }` to each fixture `package.json`. Replace `<name>` with the actual package name from the `name` field (e.g. `@fixture/core`).

Write exactly:

`fixtures/js-pnpm/packages/core/package.json`:
```json
{
  "name": "@fixture/core",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @fixture/core'"
  }
}
```

`fixtures/js-pnpm/packages/utils/package.json`:
```json
{
  "name": "@fixture/utils",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @fixture/utils'"
  },
  "dependencies": {
    "@fixture/core": "workspace:*"
  }
}
```

`fixtures/js-pnpm/packages/ui/package.json`:
```json
{
  "name": "@fixture/ui",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @fixture/ui'"
  },
  "dependencies": {
    "@fixture/core": "workspace:*",
    "@fixture/utils": "workspace:*"
  }
}
```

`fixtures/js-pnpm/packages/app/package.json`:
```json
{
  "name": "@fixture/app",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @fixture/app'"
  },
  "dependencies": {
    "@fixture/ui": "workspace:*",
    "@fixture/core": "workspace:*"
  }
}
```

`fixtures/js-yarn/packages/core/package.json`:
```json
{
  "name": "@yarn-fixture/core",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @yarn-fixture/core'"
  }
}
```

`fixtures/js-yarn/packages/lib/package.json`:
```json
{
  "name": "@yarn-fixture/lib",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @yarn-fixture/lib'"
  },
  "dependencies": {
    "@yarn-fixture/core": "1.0.0"
  }
}
```

`fixtures/js-yarn/packages/app/package.json`:
```json
{
  "name": "@yarn-fixture/app",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @yarn-fixture/app'"
  },
  "dependencies": {
    "@yarn-fixture/lib": "1.0.0",
    "@yarn-fixture/core": "1.0.0"
  }
}
```

`fixtures/js-npm/packages/shared/package.json`:
```json
{
  "name": "@npm-fixture/shared",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @npm-fixture/shared'"
  }
}
```

`fixtures/js-npm/packages/server/package.json`:
```json
{
  "name": "@npm-fixture/server",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @npm-fixture/server'"
  },
  "dependencies": {
    "@npm-fixture/shared": "workspace:*"
  }
}
```

`fixtures/js-npm/packages/client/package.json`:
```json
{
  "name": "@npm-fixture/client",
  "version": "1.0.0",
  "scripts": {
    "build": "echo 'building @npm-fixture/client'"
  },
  "dependencies": {
    "@npm-fixture/shared": "workspace:*"
  }
}
```

**Step 2:** Verify all JSON is valid:
```bash
for f in fixtures/js-pnpm/packages/*/package.json \
          fixtures/js-yarn/packages/*/package.json \
          fixtures/js-npm/packages/*/package.json; do
  python3 -c "import json,sys; json.load(open('$f'))" && echo "OK: $f"
done
```

**Step 3:** Verify `cargo test --workspace` still passes (38 tests, no regressions).

**Step 4:** Commit:
```
git add fixtures/
git commit -m "test(fixtures): add build scripts to all fixture packages"
```

**Expected outcome:** `cargo test --workspace` → 38 passed, 0 failed.

---

## Task 2: Scaffold `scheduler` Crate

**Files to create:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/scheduler/Cargo.toml`
- Create: `crates/scheduler/src/lib.rs`
- Create: `crates/scheduler/src/task.rs`
- Create: `crates/scheduler/src/runner.rs`

**Step 1: Add to workspace members**

Edit the workspace `Cargo.toml`. Change the `members` array from:
```toml
members = [
    "crates/workspace-tools",
    "crates/build-graph",
    "crates/pipeline-config",
    "crates/cli",
]
```
to:
```toml
members = [
    "crates/workspace-tools",
    "crates/build-graph",
    "crates/pipeline-config",
    "crates/scheduler",
    "crates/cli",
]
```

**Step 2: Create `crates/scheduler/Cargo.toml`**

Write exactly:
```toml
[package]
name = "scheduler"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
workspace-tools = { path = "../workspace-tools" }
build-graph = { path = "../build-graph" }
tokio = { version = "1", features = ["full"] }
thiserror = "2"
anyhow = "1"
serde_json = "1"
```

**Step 3: Create `crates/scheduler/src/lib.rs`**

Write exactly:
```rust
//! Task scheduler — builds task lists from workspace packages and runs
//! them in wave-parallel topological order using Tokio.

pub mod runner;
pub mod task;

pub use runner::run_tasks;
pub use task::{build_task_list, Task, TaskError};
```

**Step 4: Create placeholder modules**

`crates/scheduler/src/task.rs`:
```rust
// placeholder
```

`crates/scheduler/src/runner.rs`:
```rust
// placeholder
```

**Step 5:** Verify the crate compiles:
```bash
cargo build -p scheduler
```
Expected: `Finished` with no errors.

**Step 6:** Run full test suite:
```bash
cargo test --workspace
```
Expected: 38 passed, 0 failed.

**Step 7:** Commit:
```
git add Cargo.toml crates/scheduler/
git commit -m "feat(scheduler): scaffold crate"
```

---

## Task 3: Implement `Task` Struct and `build_task_list`

**File:** `crates/scheduler/src/task.rs`

### What to implement

```rust
use std::path::PathBuf;
use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use thiserror::Error;

/// A single unit of work: run `script_name` for `package_name`.
#[derive(Debug, Clone)]
pub struct Task {
    /// Package name, e.g. `@fixture/core`
    pub package_name: String,
    /// Script name to run, e.g. `build`
    pub script_name: String,
    /// Shell command from `package.json` scripts[script_name]
    pub command: String,
    /// Working directory (package root)
    pub cwd: PathBuf,
}

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("no packages have a '{0}' script in this workspace")]
    NoMatchingScript(String),
}

/// Build a task list for `script_name` from the workspace DAG.
///
/// - Returns tasks in topological order (dependencies before dependents).
/// - Packages without `scripts.{script_name}` in their `package.json` are silently skipped.
/// - Returns `TaskError::NoMatchingScript` if no package has the script.
pub fn build_task_list(dag: &WorkspaceDag, script_name: &str) -> Result<Vec<Task>, TaskError>
```

### Implementation notes

- Call `topological_sort(dag)` to get package names in dependency order.
- For each package name in topo order, look up the `Package` in `dag.packages`.
- Read `package.path/package.json`, parse with `serde_json`, extract `scripts[script_name]`.
- If `scripts[script_name]` exists → create a `Task`; if absent → skip silently.
- If no tasks were created at the end → return `Err(TaskError::NoMatchingScript(script_name.to_string()))`.
- Use `if let Ok(raw) = std::fs::read_to_string(...)` for reading; if the file can't be read, skip (tolerate in-memory packages with no filesystem backing).

### Tests (write these FIRST, verify they fail with `not yet implemented`, then implement)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;
    use workspace_tools::Package;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()   // crates/
            .parent().unwrap()   // workspace root
            .join("fixtures")
    }

    fn mk(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_matching_script_is_an_error() {
        // In-memory packages with no filesystem backing; path doesn't exist
        let packages = vec![mk("a", &[]), mk("b", &["a"])];
        let dag = build_dag(packages).unwrap();
        let err = build_task_list(&dag, "build").unwrap_err();
        assert!(matches!(err, TaskError::NoMatchingScript(_)));
        assert!(err.to_string().contains("build"));
    }

    #[test]
    fn finds_build_tasks_in_pnpm_fixture() {
        use workspace_tools::{discover_packages, build_package_graph};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        // 4 packages in the fixture, all have build scripts
        assert_eq!(tasks.len(), 4);
        // All tasks reference script_name = "build"
        assert!(tasks.iter().all(|t| t.script_name == "build"));
        // All commands are non-empty
        assert!(tasks.iter().all(|t| !t.command.is_empty()));
        // Dependencies come before dependents:
        // @fixture/core has no deps, must appear before @fixture/utils
        let pos = |name: &str| tasks.iter().position(|t| t.package_name == name).unwrap();
        assert!(pos("@fixture/core") < pos("@fixture/utils"));
        assert!(pos("@fixture/core") < pos("@fixture/ui"));
        assert!(pos("@fixture/utils") < pos("@fixture/ui"));
        assert!(pos("@fixture/ui") < pos("@fixture/app"));
    }

    #[test]
    fn skips_packages_without_the_script() {
        // One real package (has build script), one in-memory (no filesystem)
        use workspace_tools::discover_packages;
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        // build_package_graph resolves deps; but we only need discover for this test
        let dag = build_dag(
            workspace_tools::build_package_graph(raw).unwrap()
        ).unwrap();
        // All 4 pnpm packages have build scripts, so "test" (not defined) should skip all
        let err = build_task_list(&dag, "test").unwrap_err();
        assert!(matches!(err, TaskError::NoMatchingScript(_)));
    }

    #[test]
    fn task_fields_are_populated_correctly() {
        use workspace_tools::{discover_packages, build_package_graph};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        let core = tasks.iter().find(|t| t.package_name == "@fixture/core").unwrap();
        assert_eq!(core.script_name, "build");
        assert!(core.command.contains("@fixture/core"));
        assert!(core.cwd.ends_with("core"));
        assert!(core.cwd.is_absolute());
    }
}
```

**Step 1 (RED):** Write all 4 tests with `todo!()` implementation. Run `cargo test -p scheduler`. Verify all 4 fail.

**Step 2 (GREEN):** Implement `build_task_list`. Run `cargo test -p scheduler`. Verify 4 passed, 0 failed.

**Step 3:** Run `cargo clippy --workspace --all-targets -- -D warnings`. Fix any issues.

**Step 4:** Run `cargo test --workspace`. Verify all tests pass.

**Step 5:** Commit:
```
git add crates/scheduler/src/task.rs
git commit -m "feat(scheduler): Task struct and build_task_list"
```

**Expected outcome:** `cargo test -p scheduler` → 4 passed, 0 failed.

---

## Task 4: Implement `compute_task_levels` (Wave Grouping)

**File:** `crates/scheduler/src/runner.rs`

### What to implement

```rust
use crate::task::Task;
use build_graph::dag::WorkspaceDag;
use std::collections::HashMap;

/// Group tasks into parallel execution waves.
///
/// A task is placed in wave N where N = 1 + max(wave of dependencies that also have tasks).
/// Packages without tasks (e.g., scripts not found, deps skipped) don't contribute to wave depth.
/// Within a wave, tasks are sorted by package name for determinism.
///
/// Example: core (wave 0) → utils (wave 1) → ui (wave 2) → app (wave 3)
/// But core and an unrelated sibling are BOTH in wave 0 and run concurrently.
pub fn compute_task_levels(dag: &WorkspaceDag, tasks: &[Task]) -> Vec<Vec<Task>>
```

### Implementation approach

1. Build a `HashMap<String, usize>` mapping `package_name → task_index` for tasks.
2. Get topological order from `build_graph::topo::topological_sort(dag)`.
3. For each package in topo order (deps first), if it has a task:
   - `level = 1 + max(level_of[dep] for dep in package.dependencies if dep has a task)`, or `0` if no deps have tasks.
   - Record `level_of[package_name] = level`.
   - Resize `levels` vec if needed; append task clone to `levels[level]`.
4. Sort each level by `package_name` for determinism.
5. Return `levels`.

### Tests (write these FIRST)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::build_task_list;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;
    use workspace_tools::Package;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .join("fixtures")
    }

    #[test]
    fn single_package_is_level_zero() {
        // In-memory task — no filesystem needed for level computation
        let task = Task {
            package_name: "a".to_string(),
            script_name: "build".to_string(),
            command: "echo a".to_string(),
            cwd: PathBuf::from("/tmp/a"),
        };
        let pkg = Package {
            name: "a".to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp/a"),
            dependencies: vec![],
        };
        let dag = build_dag(vec![pkg]).unwrap();
        let levels = compute_task_levels(&dag, &[task]);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[0][0].package_name, "a");
    }

    #[test]
    fn linear_chain_is_separate_levels() {
        // a → b → c (a has no deps, c depends on b which depends on a)
        let tasks: Vec<Task> = ["a", "b", "c"].iter().map(|n| Task {
            package_name: n.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {n}"),
            cwd: PathBuf::from(format!("/tmp/{n}")),
        }).collect();
        let packages = vec![
            Package { name: "a".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/a"), dependencies: vec![] },
            Package { name: "b".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/b"), dependencies: vec!["a".to_string()] },
            Package { name: "c".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/c"), dependencies: vec!["b".to_string()] },
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 3, "linear chain → 3 levels");
        assert_eq!(levels[0][0].package_name, "a");
        assert_eq!(levels[1][0].package_name, "b");
        assert_eq!(levels[2][0].package_name, "c");
    }

    #[test]
    fn diamond_graph_correct_levels() {
        // core (L0) → utils (L1), ui (L1) → app (L2)
        // Wait, ui depends on BOTH core AND utils, so:
        // core: L0, utils: L1 (depends on core), ui: L2 (depends on core + utils), app: L3
        let tasks: Vec<Task> = ["core","utils","ui","app"].iter().map(|n| Task {
            package_name: n.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {n}"),
            cwd: PathBuf::from(format!("/tmp/{n}")),
        }).collect();
        let packages = vec![
            Package { name: "core".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/core"), dependencies: vec![] },
            Package { name: "utils".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/utils"), dependencies: vec!["core".to_string()] },
            Package { name: "ui".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/ui"), dependencies: vec!["core".to_string(), "utils".to_string()] },
            Package { name: "app".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/app"), dependencies: vec!["ui".to_string(), "core".to_string()] },
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0][0].package_name, "core");
        assert_eq!(levels[1][0].package_name, "utils");
        assert_eq!(levels[2][0].package_name, "ui");
        assert_eq!(levels[3][0].package_name, "app");
    }

    #[test]
    fn independent_packages_share_level() {
        // a and b are both independent (no deps) → both in level 0
        let tasks: Vec<Task> = ["a", "b"].iter().map(|n| Task {
            package_name: n.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {n}"),
            cwd: PathBuf::from(format!("/tmp/{n}")),
        }).collect();
        let packages = vec![
            Package { name: "a".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/a"), dependencies: vec![] },
            Package { name: "b".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/b"), dependencies: vec![] },
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 1, "two independent packages → 1 level");
        assert_eq!(levels[0].len(), 2, "both tasks in level 0");
    }

    #[test]
    fn pnpm_fixture_has_four_levels() {
        use workspace_tools::{discover_packages, build_package_graph};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // core(L0) → utils(L1) → ui(L2) → app(L3)
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0].len(), 1); // core
        assert_eq!(levels[0][0].package_name, "@fixture/core");
        assert_eq!(levels[3].len(), 1); // app
        assert_eq!(levels[3][0].package_name, "@fixture/app");
    }
}
```

**Step 1 (RED):** Write all 5 tests. Implement `compute_task_levels` with `todo!()`. Run `cargo test -p scheduler --lib runner`. Verify 5 fail.

**Step 2 (GREEN):** Implement `compute_task_levels`. Run `cargo test -p scheduler --lib runner`. Verify 5 passed.

**Step 3:** Clippy clean. Full test suite.

**Step 4:** Commit:
```
git add crates/scheduler/src/runner.rs
git commit -m "feat(scheduler): compute_task_levels wave grouping"
```

**Expected outcome:** `cargo test -p scheduler` → 9 passed (4 task + 5 runner), 0 failed.

---

## Task 5: Implement Async Task Runner

**File:** `crates/scheduler/src/runner.rs` (add to existing file)

### What to implement

```rust
use std::time::Instant;
use tokio::process::Command;
use tokio::task::JoinSet;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("task {package}#{script} failed with exit code {code}")]
    TaskFailed { package: String, script: String, code: i32 },
    #[error("task {package}#{script} was terminated by signal")]
    Killed { package: String, script: String },
    #[error("failed to spawn task {package}#{script}: {source}")]
    Spawn { package: String, script: String, #[source] source: std::io::Error },
}

/// Execute tasks in wave-parallel order using Tokio.
///
/// For each wave (level) from `compute_task_levels`:
/// 1. Spawn all tasks in the wave concurrently using `JoinSet`.
/// 2. Wait for all tasks in the wave to complete.
/// 3. If ANY task fails, cancel remaining (drain JoinSet) and return `Err`.
/// 4. Print progress to stderr: one line before start, one line after.
///
/// Progress format (stderr):
/// ```
/// [rage] @fixture/core#build starting
/// [rage] @fixture/core#build ✓ 0.12s
/// ```
/// On failure:
/// ```
/// [rage] @fixture/core#build ✗ FAILED (exit 1)
/// ```
///
/// Commands are executed via `sh -c "{command}"` in `task.cwd`.
pub async fn run_tasks(dag: &WorkspaceDag, tasks: Vec<Task>) -> anyhow::Result<()>
```

### Implementation

```rust
pub async fn run_tasks(dag: &WorkspaceDag, tasks: Vec<Task>) -> anyhow::Result<()> {
    use anyhow::Context;
    let levels = compute_task_levels(dag, &tasks);
    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();
        for task in level {
            set.spawn(run_single_task(task));
        }
        // Collect results; return first error
        let mut first_error: Option<RunError> = None;
        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    // Abort remaining tasks in this wave
                    set.abort_all();
                }
                Err(join_err) => {
                    // Task panicked
                    if first_error.is_none() {
                        first_error = Some(RunError::Killed {
                            package: "unknown".to_string(),
                            script: "unknown".to_string(),
                        });
                    }
                    set.abort_all();
                }
            }
        }
        if let Some(e) = first_error {
            return Err(e).context("task execution failed");
        }
    }
    Ok(())
}

async fn run_single_task(task: Task) -> Result<(), RunError> {
    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
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

    let elapsed = start.elapsed().as_secs_f64();

    if status.success() {
        eprintln!("[rage] {}#{} ✓ {:.2}s", task.package_name, task.script_name, elapsed);
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        eprintln!("[rage] {}#{} ✗ FAILED (exit {code})", task.package_name, task.script_name);
        Err(RunError::TaskFailed {
            package: task.package_name,
            script: task.script_name,
            code,
        })
    }
}
```

### Tests (write these FIRST — add to `runner.rs` tests module)

Add these tests to the existing `#[cfg(test)]` block in `runner.rs`:

```rust
    #[tokio::test]
    async fn single_successful_task_runs() {
        use std::path::PathBuf;
        let task = Task {
            package_name: "test-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo hello".to_string(),
            cwd: PathBuf::from("/tmp"),
        };
        let pkg = Package {
            name: "test-pkg".to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp/test-pkg"),
            dependencies: vec![],
        };
        let dag = build_dag(vec![pkg]).unwrap();
        run_tasks(&dag, vec![task]).await.unwrap();
    }

    #[tokio::test]
    async fn failing_task_returns_error() {
        use std::path::PathBuf;
        let task = Task {
            package_name: "failing-pkg".to_string(),
            script_name: "build".to_string(),
            command: "exit 1".to_string(),
            cwd: PathBuf::from("/tmp"),
        };
        let pkg = Package {
            name: "failing-pkg".to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp/failing-pkg"),
            dependencies: vec![],
        };
        let dag = build_dag(vec![pkg]).unwrap();
        let err = run_tasks(&dag, vec![task]).await.unwrap_err();
        assert!(err.to_string().contains("failing-pkg"));
    }

    #[tokio::test]
    async fn two_independent_tasks_both_run() {
        use std::path::PathBuf;
        use tempfile::tempdir;
        // Both tasks write a marker file; verify both files exist after run
        let dir = tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        let cmd_a = format!("touch '{}'", file_a.display());
        let cmd_b = format!("touch '{}'", file_b.display());

        let tasks = vec![
            Task { package_name: "a".to_string(), script_name: "build".to_string(), command: cmd_a, cwd: PathBuf::from("/tmp") },
            Task { package_name: "b".to_string(), script_name: "build".to_string(), command: cmd_b, cwd: PathBuf::from("/tmp") },
        ];
        let packages = vec![
            Package { name: "a".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/a"), dependencies: vec![] },
            Package { name: "b".to_string(), version: "1.0.0".to_string(), path: PathBuf::from("/tmp/b"), dependencies: vec![] },
        ];
        let dag = build_dag(packages).unwrap();
        run_tasks(&dag, tasks).await.unwrap();
        assert!(file_a.exists(), "task a should have run");
        assert!(file_b.exists(), "task b should have run");
    }
```

Note: `tempfile` is already a dev-dependency in `workspace-tools` but needs adding to `scheduler`'s dev-deps:

Add to `crates/scheduler/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = "3"
```

**Step 1 (RED):** Add the `RunError` enum, stub `run_tasks` with `todo!()`, stub `run_single_task` with `todo!()`. Add 3 tokio tests. Run `cargo test -p scheduler`. Verify the 3 new tests fail.

**Step 2 (GREEN):** Implement `run_tasks` and `run_single_task`. Run `cargo test -p scheduler`. Verify all 12 tests pass.

**Step 3:** `cargo clippy --workspace --all-targets -- -D warnings`. Clean.

**Step 4:** Commit:
```
git add crates/scheduler/
git commit -m "feat(scheduler): async wave-parallel task runner"
```

**Expected outcome:** `cargo test -p scheduler` → 12 passed, 0 failed.

---

## Task 6: Add `rage run` to CLI

**File:** `crates/cli/src/main.rs`

### What to add

Add a `Run` subcommand that:
1. Takes `script_name: String` as positional argument (required)
2. Takes `--workspace <path>` and optional positional workspace path (same pattern as `graph`)
3. Calls: discover → build_dag → build_task_list → run_tasks

Update `crates/cli/Cargo.toml` to add `scheduler` as a dependency.

### Step 1: Update `crates/cli/Cargo.toml`

Add the scheduler dependency:
```toml
scheduler = { path = "../scheduler" }
```

And add `tokio` runtime to cli:
```toml
tokio = { version = "1", features = ["full"] }
```

### Step 2: Update `crates/cli/src/main.rs`

Full updated file:
```rust
//! `rage` — the rage build system CLI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "rage", version, about = "rage build system")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the workspace package dependency graph as DOT.
    Graph {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Run a script across all workspace packages in dependency order.
    Run {
        /// Script name to run (e.g. `build`, `test`, `lint`).
        script: String,

        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Graph { workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_graph(&root)
        }
        Command::Run { script, workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_run(&root, &script).await
        }
    }
}

fn resolve_workspace(pos: Option<PathBuf>, named: Option<PathBuf>) -> PathBuf {
    pos.or(named)
        .map(|p| p.canonicalize().unwrap_or(p))
        .unwrap_or_else(|| std::env::current_dir().unwrap())
}

fn cmd_graph(root: &Path) -> Result<()> {
    let pm = workspace_tools::detect_package_manager(root)
        .with_context(|| format!(
            "{} is not a recognized JS workspace", root.display()
        ))?;

    let raw = workspace_tools::discover_packages(root)
        .context("discovering workspace packages")?;
    let resolved = workspace_tools::build_package_graph(raw)
        .context("resolving package dependency edges")?;

    eprintln!("Found {} packages ({} workspace)", resolved.len(), pm.as_str());

    let dag = build_graph::dag::build_dag(resolved)
        .context("building package DAG")?;
    let dot = build_graph::dot::to_dot(&dag);
    print!("{dot}");
    Ok(())
}

async fn cmd_run(root: &Path, script: &str) -> Result<()> {
    let pm = workspace_tools::detect_package_manager(root)
        .with_context(|| format!(
            "{} is not a recognized JS workspace", root.display()
        ))?;

    let raw = workspace_tools::discover_packages(root)
        .context("discovering workspace packages")?;
    let resolved = workspace_tools::build_package_graph(raw)
        .context("resolving package dependency edges")?;

    eprintln!("Found {} packages ({} workspace)", resolved.len(), pm.as_str());

    let dag = build_graph::dag::build_dag(resolved)
        .context("building package DAG")?;

    let tasks = scheduler::task::build_task_list(&dag, script)
        .with_context(|| format!("no packages have a '{script}' script"))?;

    eprintln!("Running '{}' across {} packages", script, tasks.len());

    scheduler::run_tasks(&dag, tasks).await
        .with_context(|| format!("'{script}' run failed"))?;

    eprintln!("Done.");
    Ok(())
}
```

### Step 3: Update integration tests

Add these tests to `crates/cli/tests/integration.rs` (append to the existing file):

```rust
// ── rage run tests ─────────────────────────────────────────────────────────

#[test]
fn run_build_pnpm_exits_zero() {
    let root = fixtures_dir().join("js-pnpm");
    let output = Command::new(env!("CARGO_BIN_EXE_rage"))
        .args(["run", "build", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rage run build should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Found 4 packages"), "should report package count");
    assert!(stderr.contains("Done."), "should report completion");
}

#[test]
fn run_build_shows_all_packages() {
    let root = fixtures_dir().join("js-pnpm");
    let output = Command::new(env!("CARGO_BIN_EXE_rage"))
        .args(["run", "build", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // All 4 packages should appear in progress output
    assert!(stderr.contains("@fixture/core"), "core should run");
    assert!(stderr.contains("@fixture/utils"), "utils should run");
    assert!(stderr.contains("@fixture/ui"), "ui should run");
    assert!(stderr.contains("@fixture/app"), "app should run");
}

#[test]
fn run_unknown_script_exits_nonzero() {
    let root = fixtures_dir().join("js-pnpm");
    let output = Command::new(env!("CARGO_BIN_EXE_rage"))
        .args(["run", "nonexistent-script", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "rage run nonexistent should exit nonzero"
    );
}
```

### Steps

**Step 1:** Update `crates/cli/Cargo.toml` with `scheduler` and `tokio` deps.

**Step 2:** Write the updated `main.rs` as specified. Build:
```bash
cargo build -p rage-cli
```
Expected: Compiles cleanly.

**Step 3:** Write the 3 new integration tests. Run:
```bash
cargo test -p rage-cli
```
Expected: All 10 tests pass (7 existing + 3 new).

**Step 4:** Manual smoke test:
```bash
./target/debug/rage run build fixtures/js-pnpm
```
Expected output (to stderr):
```
Found 4 packages (pnpm workspace)
Running 'build' across 4 packages
[rage] @fixture/core#build starting
building @fixture/core
[rage] @fixture/core#build ✓ 0.01s
[rage] @fixture/utils#build starting
building @fixture/utils
[rage] @fixture/utils#build ✓ 0.01s
[rage] @fixture/ui#build starting
building @fixture/ui
[rage] @fixture/ui#build ✓ 0.01s
[rage] @fixture/app#build starting
building @fixture/app
[rage] @fixture/app#build ✓ 0.01s
Done.
```

**Step 5:** `cargo clippy --workspace --all-targets -- -D warnings`. Clean.

**Step 6:** Commit:
```
git add crates/cli/
git commit -m "feat(cli): add rage run subcommand"
```

**Expected outcome:** `cargo test --workspace` → all tests pass. `rage run build fixtures/js-pnpm` exits 0.

---

## Task 7: Final Verification

Run all verification commands from the worktree root:

**Step 1:** Full build:
```bash
cargo build --workspace
```
Expected: `Finished` with no errors.

**Step 2:** Full test suite:
```bash
cargo test --workspace
```
Expected: All tests pass, 0 failed.
Document the exact test count (should be ≥51: 38 existing + 4 task + 5 runner levels + 3 runner run + 3 CLI integration = ~53).

**Step 3:** Clippy:
```bash
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: Zero warnings.

**Step 4:** Release build + smoke test:
```bash
cargo build --release
./target/release/rage run build fixtures/js-pnpm
```
Expected: exit 0, all 4 packages built, `Done.` printed.

**Step 5:** Yarn fixture smoke test:
```bash
./target/release/rage run build fixtures/js-yarn
```
Expected: exit 0, 3 packages built.

**Step 6:** Git log (last 10):
```bash
git log --oneline -10
```
Expected to see:
- `feat(cli): add rage run subcommand`
- `feat(scheduler): async wave-parallel task runner`
- `feat(scheduler): compute_task_levels wave grouping`
- `feat(scheduler): Task struct and build_task_list`
- `feat(scheduler): scaffold crate`
- `test(fixtures): add build scripts to all fixture packages`

**Step 7:** Commit verification (no unstaged changes):
```bash
git status
```
Expected: `working tree clean`.

**STATUS: DONE** when all 7 steps pass.

---

## Failure Triage

**`tokio::main` conflicts with `#[test]`:** Clippy or rustc may warn about using `#[tokio::main]` and `#[tokio::test]` in the same crate. If this happens in `scheduler` tests, the tests are in a `#[cfg(test)]` block — that's fine. If cli tests fail because of tokio runtime setup, ensure the integration tests use `Command::new(...)` (spawning a subprocess) and don't need a tokio runtime themselves.

**`sh -c "exit 1"` on macOS:** The command `exit 1` inside `sh -c` should exit with code 1. If it doesn't work as expected in tests, use `sh -c "false"` instead — `false` is guaranteed to exit 1.

**`JoinSet::abort_all` + `drain`:** After calling `set.abort_all()`, the `while let Some(...) = set.join_next().await` loop must drain all remaining entries (they'll return `JoinError` for aborted tasks). The implementation above handles this correctly — the loop continues but `first_error` is already set.

**`compute_task_levels` with skipped packages:** If a package has no matching task, it should be absent from the level computation. Its dependencies still contribute if they have tasks. Test `skips_packages_without_the_script` in Task 3 verifies the `NoMatchingScript` error path.

**Clippy `clippy::wildcard_imports`:** If clippy complains about `use super::*` in tests, add `#[allow(clippy::wildcard_imports)]` to the test module or list imports explicitly.

**If anything unexpected happens:** STOP. Do not invent fixes. Report the exact error and await instructions.
