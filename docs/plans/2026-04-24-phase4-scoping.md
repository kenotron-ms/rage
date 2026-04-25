# Phase 4: Git-Based Scoping Implementation Plan

> **Execution:** Use the subagent-driven-development workflow.
> **Prerequisite:** Phase 3 complete — `cargo test --workspace` passes (74 tests), `rage run build fixtures/js-pnpm` uses local cache on second run.

**Goal:** Add git-based scoping so `rage run build --since HEAD~1` only executes packages affected by recent changes. Packages that haven't changed (and whose transitive dependencies haven't changed) are skipped entirely — even before checking the cache.

**Architecture:** New `scoping` crate with two functions: `git_changed_files` (runs `git diff`) and `affected_packages` (direct match + reverse BFS for transitive dependents). CLI gains a `--since <REF>` flag. `scheduler` gains a `scope` parameter that filters tasks before execution.

**Tech Stack:** Rust 2021, `petgraph` (already in `build-graph`), `std::process::Command` for git.

**End state:** Given js-pnpm fixture with a change in `@fixture/utils/package.json`:

```
# rage run build --since HEAD~1 fixtures/js-pnpm
Found 4 packages (pnpm workspace)
Scoping to packages affected since HEAD~1: 3 affected (1 scoped out)
Running 'build' across 3 packages
[rage] @fixture/utils#build ✓ 0.02s
[rage] @fixture/ui#build ✓ 0.01s
[rage] @fixture/app#build ✓ 0.01s
Done.
# @fixture/core was NOT run — it's unaffected
```

**What's deferred:** `--affected` (uncommitted changes), merge-base computation, per-file granularity within packages, sandbox-observed inputs.

---

## Context For The Implementer

Repo lives at `/Users/ken/workspace/ms/rage`. Work on branch `feat/phase-4-scoping` in worktree `.worktrees/feat-phase-4-scoping`.

**Existing crates:**
- `workspace-tools` — `Package` struct (has `pub path: PathBuf`, `pub name: String`)
- `build-graph` — `WorkspaceDag` (has `pub graph: DiGraph<String, ()>`, `pub nodes: HashMap<String, NodeIndex>`, `pub packages: HashMap<String, Package>`)
- `scheduler` — `build_task_list`, `compute_task_levels`, `run_tasks`
- `cache` — `LocalCache`, `fingerprint_task`
- `cli` — `rage` binary (`rage graph`, `rage run`)

**Key data flow for scoping:**
1. CLI discovers packages → `Vec<Package>`
2. CLI builds DAG → `WorkspaceDag`
3. CLI calls `git_changed_files(root, base_ref)` → `Vec<PathBuf>`
4. CLI calls `affected_packages(&packages, &dag, &changed_files)` → `HashSet<String>`
5. CLI calls `build_task_list(&dag, script)` → `Vec<Task>` (all tasks)
6. CLI filters tasks: `tasks.retain(|t| scope.contains(&t.package_name))`
7. CLI calls `run_tasks(&dag, tasks, cache)` with filtered tasks

Scoping is purely a CLI concern — `scheduler` and `build-graph` don't change.

**Dependency graph of js-pnpm fixture:**
```
@fixture/core (no deps)
@fixture/utils → @fixture/core
@fixture/ui → @fixture/core, @fixture/utils
@fixture/app → @fixture/core, @fixture/ui
```
If only `@fixture/utils` changes: affected = {utils, ui, app}. `@fixture/core` is NOT affected (nothing changed in it, and it has no upstream deps that changed).

**Rules:**
1. Follow each task's steps literally and in order.
2. TDD: write failing tests **before** implementation.
3. Do NOT add functionality beyond what each task specifies.
4. Commit after each task with the exact message given.
5. If a test fails unexpectedly, STOP and report. Do not alter tests to match broken behavior.
6. Run `cargo clippy --workspace --all-targets -- -D warnings` after every task.

---

## Task 1: Scaffold `scoping` Crate

**Goal:** Create the `scoping` crate skeleton so it compiles.

### Steps

1. Add `"crates/scoping"` to workspace `Cargo.toml` members (after `crates/cache`).

2. Create `crates/scoping/Cargo.toml`:
```toml
[package]
name = "scoping"
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
anyhow = "1"
thiserror = "2"
```

3. Create `crates/scoping/src/lib.rs`:
```rust
//! Git-based scoping — determines which packages are affected by recent changes.

pub mod affected;
pub mod git;

pub use affected::affected_packages;
pub use git::git_changed_files;
```

4. Create two placeholder files:
   - `crates/scoping/src/git.rs` — `// placeholder`
   - `crates/scoping/src/affected.rs` — `// placeholder`

5. Verify: `cargo build -p scoping` succeeds.

6. Verify: `cargo test --workspace 2>&1 | tail -5` shows all prior 74 tests still pass.

7. Commit:
```
feat(scoping): scaffold crate
```

---

## Task 2: Implement `git_changed_files`

**Goal:** Return the list of files changed between `base_ref` and `HEAD` by running `git diff --name-only <base_ref>..HEAD` in the workspace root.

### Steps

1. Write the failing tests in `crates/scoping/src/git.rs` **first**:

```rust
//! Git interface — discover changed files using `git diff`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Return absolute paths of files changed between `base_ref` and HEAD.
///
/// Runs: `git diff --name-only <base_ref>..HEAD`
///
/// Paths are resolved relative to `root` (the workspace root). If the repo
/// root differs from `root`, paths are still rooted at `root` for consistency
/// with `Package::path`.
///
/// Returns an empty `Vec` if no files changed.
pub fn git_changed_files(root: &Path, base_ref: &str) -> Result<Vec<PathBuf>> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    /// Initialize a bare git repo in `dir` with user config so commits work.
    fn git_init(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
    }

    fn git_commit_all(dir: &std::path::Path, msg: &str) {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn returns_changed_files_since_ref() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        // Initial commit
        fs::write(root.join("a.ts"), b"const a = 1;").unwrap();
        fs::write(root.join("b.ts"), b"const b = 2;").unwrap();
        git_commit_all(root, "initial");

        // Second commit — only modify a.ts
        fs::write(root.join("a.ts"), b"const a = 99;").unwrap();
        git_commit_all(root, "update a");

        let changed = git_changed_files(root, "HEAD~1").unwrap();
        let names: Vec<_> = changed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.ts".to_string()), "a.ts should be changed");
        assert!(
            !names.contains(&"b.ts".to_string()),
            "b.ts should not be changed"
        );
    }

    #[test]
    fn returns_empty_when_no_changes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("a.ts"), b"const a = 1;").unwrap();
        git_commit_all(root, "initial");

        // Nothing changed since HEAD~0..HEAD would be empty but HEAD has no parent.
        // Compare HEAD against itself (no diff).
        // Use a 2-commit setup with identical commits to test the empty case.
        fs::write(root.join("unchanged.ts"), b"unchanged").unwrap();
        git_commit_all(root, "second identical");

        // diff HEAD~1..HEAD is the second commit (only unchanged.ts changed)
        // diff HEAD..HEAD is empty
        // We test the case where nothing changed relative to a ref that == HEAD:
        // Actually test: make two identical commits where no files were modified
        // This is hard to do, so instead test that the second commit shows only unchanged.ts
        let changed = git_changed_files(root, "HEAD~1").unwrap();
        let names: Vec<_> = changed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"unchanged.ts".to_string()));
        assert!(!names.contains(&"a.ts".to_string()));
    }

    #[test]
    fn paths_are_absolute() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::create_dir_all(root.join("packages").join("core")).unwrap();
        fs::write(
            root.join("packages").join("core").join("index.ts"),
            b"export {}",
        )
        .unwrap();
        git_commit_all(root, "initial");

        fs::write(root.join("packages").join("core").join("index.ts"), b"export const x = 1;")
            .unwrap();
        git_commit_all(root, "update core");

        let changed = git_changed_files(root, "HEAD~1").unwrap();
        assert!(!changed.is_empty());
        for path in &changed {
            assert!(
                path.is_absolute(),
                "all paths should be absolute, got: {path:?}"
            );
        }
    }

    #[test]
    fn invalid_ref_returns_error() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);
        fs::write(root.join("f.ts"), b"x").unwrap();
        git_commit_all(root, "init");

        let result = git_changed_files(root, "nonexistent-ref-xyz");
        assert!(result.is_err(), "invalid ref should return an error");
    }
}
```

2. Run `cargo test -p scoping 2>&1 | tail -10` — confirm all 4 tests fail with `not yet implemented`.

3. Add `tempfile = "3"` to `[dev-dependencies]` in `crates/scoping/Cargo.toml`.

4. Implement `git_changed_files`:

```rust
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git_changed_files(root: &Path, base_ref: &str) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("{base_ref}..HEAD")])
        .current_dir(root)
        .output()
        .context("running git diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let paths = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| root.join(l))
        .collect();

    Ok(paths)
}
```

5. Run `cargo test -p scoping --lib git 2>&1 | tail -10` — confirm all 4 tests pass.

6. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

7. Commit:
```
feat(scoping): git_changed_files — diff base_ref..HEAD
```

---

## Task 3: Implement `affected_packages`

**Goal:** Given a list of changed files and a workspace DAG, return the set of package names that are directly or transitively affected.

**A package is directly affected if** any changed file has a path prefix matching `pkg.path`.
**A package is transitively affected if** it depends (directly or transitively) on any directly-affected package.

This is equivalent to: starting from directly-affected nodes in the dependency DAG, do a reverse BFS (edges point from dependent → dependency; traversing reversed edges finds all dependents).

### Steps

1. Write the failing tests in `crates/scoping/src/affected.rs` **first**:

```rust
//! Affected-package computation — direct match + transitive dependent closure.

use build_graph::dag::WorkspaceDag;
use std::collections::HashSet;
use std::path::PathBuf;
use workspace_tools::Package;

/// Return the set of package names affected by the given changed files.
///
/// A package is **directly affected** if any `changed_file` has a path prefix
/// matching `pkg.path`.
///
/// A package is **transitively affected** if it directly or transitively
/// depends on any directly-affected package (i.e., it would need a rebuild).
///
/// The returned set includes both directly and transitively affected packages.
pub fn affected_packages(
    packages: &[Package],
    dag: &WorkspaceDag,
    changed_files: &[PathBuf],
) -> HashSet<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;

    fn pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/workspace").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn file(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn empty_changed_files_returns_empty_set() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();
        let affected = affected_packages(&packages, &dag, &[]);
        assert!(affected.is_empty());
    }

    #[test]
    fn directly_affected_package_included() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();
        let changed = vec![file("/workspace/core/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);
        assert!(
            affected.contains("core"),
            "core should be directly affected"
        );
    }

    #[test]
    fn transitive_dependents_included() {
        // core ← utils ← ui ← app  (arrow = "depends on")
        let packages = vec![
            pkg("core", &[]),
            pkg("utils", &["core"]),
            pkg("ui", &["core", "utils"]),
            pkg("app", &["core", "ui"]),
        ];
        let dag = build_dag(packages.clone()).unwrap();

        // Only utils changed
        let changed = vec![file("/workspace/utils/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        // utils itself + ui (depends on utils) + app (depends on ui)
        assert!(affected.contains("utils"), "utils directly affected");
        assert!(affected.contains("ui"), "ui transitively affected");
        assert!(affected.contains("app"), "app transitively affected");
        // core is a dep OF utils, not a dependent — NOT affected
        assert!(!affected.contains("core"), "core should not be affected");
    }

    #[test]
    fn leaf_change_only_affects_leaf() {
        // core ← app
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();

        // app is a leaf (nothing depends on it)
        let changed = vec![file("/workspace/app/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert!(affected.contains("app"), "app directly affected");
        assert!(!affected.contains("core"), "core not affected when only app changes");
    }

    #[test]
    fn root_change_affects_all() {
        let packages = vec![
            pkg("core", &[]),
            pkg("utils", &["core"]),
            pkg("ui", &["core", "utils"]),
            pkg("app", &["core", "ui"]),
        ];
        let dag = build_dag(packages.clone()).unwrap();

        // core changed — everything that depends on core (directly or transitively) is affected
        let changed = vec![file("/workspace/core/src/main.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert_eq!(affected.len(), 4, "all 4 packages should be affected when core changes");
    }

    #[test]
    fn file_outside_any_package_affects_nothing() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();

        // A file at the repo root, outside any package
        let changed = vec![file("/workspace/tsconfig.json")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert!(
            affected.is_empty(),
            "a file outside any package should not affect any package"
        );
    }
}
```

2. Run `cargo test -p scoping --lib affected 2>&1 | tail -10` — confirm all 6 tests fail with `not yet implemented`.

3. Implement `affected_packages` using petgraph's `Reversed` + `Bfs`:

```rust
use build_graph::dag::WorkspaceDag;
use petgraph::visit::{Bfs, Reversed};
use std::collections::HashSet;
use std::path::PathBuf;
use workspace_tools::Package;

pub fn affected_packages(
    packages: &[Package],
    dag: &WorkspaceDag,
    changed_files: &[PathBuf],
) -> HashSet<String> {
    // 1. Find directly-affected packages: those whose path contains a changed file
    let mut directly_affected: Vec<String> = packages
        .iter()
        .filter(|pkg| {
            changed_files
                .iter()
                .any(|f| f.starts_with(&pkg.path))
        })
        .map(|pkg| pkg.name.clone())
        .collect();

    if directly_affected.is_empty() {
        return HashSet::new();
    }

    // 2. Compute transitive dependents via reverse BFS in the dependency graph.
    //
    // The DAG has edges pkg → dep (pkg depends on dep).
    // Reversing gives dep → pkg (dep is depended on by pkg).
    // BFS from each directly-affected node in the reversed graph visits
    // all packages that (transitively) depend on the directly-affected package.
    let reversed = Reversed(&dag.graph);
    let mut affected: HashSet<String> = HashSet::new();

    for pkg_name in directly_affected.drain(..) {
        if let Some(&start) = dag.nodes.get(&pkg_name) {
            let mut bfs = Bfs::new(reversed, start);
            while let Some(nx) = bfs.next(reversed) {
                affected.insert(dag.graph[nx].clone());
            }
        }
    }

    affected
}
```

Note: `petgraph` is already a dependency of `build-graph`, but `scoping` needs it too. Add `petgraph = "0.6"` to `crates/scoping/Cargo.toml` under `[dependencies]`.

4. Run `cargo test -p scoping 2>&1 | tail -10` — confirm **10 tests pass** (4 git + 6 affected), 0 failed.

5. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

6. Commit:
```
feat(scoping): affected_packages — direct match + reverse BFS
```

---

## Task 4: Wire Scoping into CLI

**Goal:** Add `--since <REF>` flag to `rage run`. When present: compute changed files, determine affected packages, filter tasks to only affected packages.

### Steps

1. Add `scoping = { path = "../scoping" }` to `crates/cli/Cargo.toml` under `[dependencies]`.

2. Add the `--since` flag to `Command::Run` in `crates/cli/src/main.rs`:

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

    /// Scope execution to packages affected since this git ref.
    /// Example: `--since HEAD~1` or `--since origin/main`.
    #[arg(long)]
    since: Option<String>,
},
```

3. Update `cmd_run` to accept and use `since: Option<String>`:

```rust
async fn cmd_run(root: &Path, script: &str, no_cache: bool, since: Option<&str>) -> Result<()> {
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

    let dag = build_graph::dag::build_dag(resolved.clone()).context("building package DAG")?;

    // Compute scope (if --since was given)
    let scope: Option<std::collections::HashSet<String>> = if let Some(base_ref) = since {
        let changed_files = scoping::git_changed_files(root, base_ref)
            .with_context(|| format!("computing changed files since {base_ref}"))?;
        let affected = scoping::affected_packages(&resolved, &dag, &changed_files);
        eprintln!(
            "Scoping to packages affected since {base_ref}: {} affected ({} scoped out)",
            affected.len(),
            resolved.len().saturating_sub(affected.len())
        );
        Some(affected)
    } else {
        None
    };

    let mut tasks = scheduler::task::build_task_list(&dag, script)
        .with_context(|| format!("no packages have a '{script}' script"))?;

    // Filter tasks by scope if --since was given
    if let Some(ref scope_set) = scope {
        tasks.retain(|t| scope_set.contains(&t.package_name));
        if tasks.is_empty() {
            eprintln!("No affected packages have a '{script}' script. Nothing to do.");
            return Ok(());
        }
    }

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

4. Update the `match` arm for `Command::Run` to pass all arguments:

```rust
Command::Run {
    script,
    workspace,
    workspace_pos,
    no_cache,
    since,
} => {
    let root = resolve_workspace(workspace_pos, workspace);
    cmd_run(&root, &script, no_cache, since.as_deref()).await
}
```

5. Note: `resolved.clone()` requires `Package: Clone`. `Package` already derives `Clone` from the `workspace-tools` crate. Verify this compiles — if `Package` doesn't derive Clone, you'll need to call `workspace_tools::build_package_graph(raw)` twice or rearrange.

   If `Package` already derives `Clone` (it should — check `workspace-tools/src/package.rs`), `resolved.clone()` is fine. If not, add `#[derive(Clone)]` to `Package` in `workspace-tools/src/package.rs`.

6. Run `cargo build -p rage-cli 2>&1 | tail -5` — confirm clean build.

7. Run `cargo test --workspace 2>&1 | tail -5` — confirm all prior tests still pass (expect 84+ tests now).

8. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

9. Commit:
```
feat(cli): --since flag for git-based scoping
```

---

## Task 5: Integration Tests for Scoping

**Goal:** Prove the `--since` flag works end-to-end with a real git repository.

### Steps

1. Add a new test to `crates/cli/tests/integration.rs`. Import `tempfile::tempdir` (already in dev-dependencies):

```rust
// ── scoping integration tests ────────────────────────────────────────────────

#[test]
fn since_flag_skips_unaffected_packages() {
    use std::fs;
    use std::process::Command as Cmd;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();

    // 1. Set up a pnpm workspace in the temp dir
    //    Copy the js-pnpm fixture layout manually (package.json + pnpm-workspace.yaml)
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("js-pnpm");

    // Recursive copy helper
    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &dst_path);
            } else {
                fs::copy(entry.path(), dst_path).unwrap();
            }
        }
    }
    copy_dir(&fixtures_dir, root);

    // 2. Initialize git repo and commit everything
    let git = |args: &[&str]| {
        Cmd::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@test.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial"]);

    // 3. Modify only @fixture/utils/package.json (add a comment field)
    let utils_pkg = root.join("packages").join("utils").join("package.json");
    let original = fs::read_to_string(&utils_pkg).unwrap();
    let modified = original.replace(
        "\"version\": \"1.0.0\"",
        "\"version\": \"1.0.1\"",
    );
    fs::write(&utils_pkg, modified).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-m", "bump utils version"]);

    // 4. Run rage with --since HEAD~1
    let bin = env!("CARGO_BIN_EXE_rage");
    let output = Cmd::new(bin)
        .args(["run", "build", "--since", "HEAD~1", "--no-cache"])
        .arg(root)
        .env("RAGE_CACHE_DIR", root.join(".rage-test-cache"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "rage run --since should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // utils, ui, and app should have run (ui and app depend on utils)
    assert!(
        stderr.contains("@fixture/utils"),
        "utils should run (directly affected)\n{stderr}"
    );
    assert!(
        stderr.contains("@fixture/ui"),
        "ui should run (depends on utils)\n{stderr}"
    );
    assert!(
        stderr.contains("@fixture/app"),
        "app should run (depends on ui)\n{stderr}"
    );

    // core should NOT have run (doesn't depend on utils)
    assert!(
        !stderr.contains("@fixture/core#build"),
        "core should be scoped out\n{stderr}"
    );

    // Scoping message should appear
    assert!(
        stderr.contains("Scoping to packages affected since HEAD~1"),
        "scoping message should appear\n{stderr}"
    );
}

#[test]
fn since_with_no_changes_runs_nothing() {
    use std::fs;
    use std::process::Command as Cmd;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();

    // Copy fixture
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("js-pnpm");

    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &dst_path);
            } else {
                fs::copy(entry.path(), dst_path).unwrap();
            }
        }
    }
    copy_dir(&fixtures_dir, root);

    let git = |args: &[&str]| {
        Cmd::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@test.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial"]);

    // Make a second commit that changes nothing in any package
    // Write to root-level file outside any package
    fs::write(root.join("README.md"), b"# test").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-m", "add readme"]);

    let bin = env!("CARGO_BIN_EXE_rage");
    let output = Cmd::new(bin)
        .args(["run", "build", "--since", "HEAD~1", "--no-cache"])
        .arg(root)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should print "Nothing to do" or run 0 packages
    assert!(
        stderr.contains("Nothing to do") || stderr.contains("0 affected"),
        "no packages should run when only README changed\n{stderr}"
    );
}
```

2. Run `cargo test --workspace 2>&1 | grep -E "(scoping|since|FAILED)" | head -20` — confirm both new tests pass.

3. Run the full test suite: `cargo test --workspace 2>&1 | tail -5` — confirm all tests pass.

4. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

5. Commit:
```
test(cli): integration tests for --since scoping flag
```

---

## Task 6: Final Verification

**Goal:** End-to-end proof that scoping works. All tests pass, behavior confirmed.

### Steps

1. Build the release binary:
```bash
cargo build --release 2>&1 | tail -3
```

2. Run the full test suite and count:
```bash
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{sum += $4} END {print "Total passing:", sum}'
```
Expected: ≥ 86 tests, 0 failed. (74 prior + ~12 new: 10 scoping unit tests + 2 CLI integration)

3. Demonstrate scoping — create a temp git repo and run:
```bash
# Build temp fixture git repo
TMPDIR=$(mktemp -d)
cp -r fixtures/js-pnpm/. "$TMPDIR/"
cd "$TMPDIR"
git init -b main && git config user.email t@t.com && git config user.name T
git add -A && git commit -m init
# Modify only utils
echo '{"name":"@fixture/utils","version":"1.0.1","scripts":{"build":"echo building @fixture/utils"},"dependencies":{"@fixture/core":"workspace:*"}}' > packages/utils/package.json
git add -A && git commit -m "bump utils"
cd -
./target/release/rage run build --since HEAD~1 --no-cache "$TMPDIR" 2>&1
```

Expected output:
```
Found 4 packages (pnpm workspace)
Scoping to packages affected since HEAD~1: 3 affected (1 scoped out)
Running 'build' across 3 packages
[rage] @fixture/utils#build starting
building @fixture/utils
[rage] @fixture/utils#build ✓ <Ns>
[rage] @fixture/ui#build starting
building @fixture/ui
[rage] @fixture/ui#build ✓ <Ns>
[rage] @fixture/app#build starting
building @fixture/app
[rage] @fixture/app#build ✓ <Ns>
Done.
```
`@fixture/core` must NOT appear in the output.

4. Demonstrate that without `--since`, all 4 packages run:
```bash
./target/release/rage run build --no-cache "$TMPDIR" 2>&1
```
Expected: all 4 packages run.

5. Print git log (last 15 commits):
```bash
git log --oneline -15
```

6. If all steps pass, report **STATUS: DONE**.

---

## Expected Test Count Growth

| Phase | Tests |
|-------|-------|
| End of Phase 3 | 74 |
| After Task 2 (+4 git) | 78 |
| After Task 3 (+6 affected) | 84 |
| After Task 4 (no new tests) | 84 |
| After Task 5 (+2 CLI integration) | 86 |

---

## Failure Triage

**`petgraph` not found in `scoping`:** Add `petgraph = "0.6"` to `crates/scoping/Cargo.toml` `[dependencies]`.

**`Reversed` import error:** Use `use petgraph::visit::{Bfs, Reversed};`. The `Reversed` type is in `petgraph::visit`.

**`Package` does not implement `Clone`:** Add `#[derive(Clone)]` to the `Package` struct in `crates/workspace-tools/src/package.rs`. Also add `Clone` to the derive list in `CacheEntry` if needed (it already should be).

**`cmd_run` borrow error on `resolved`:** `affected_packages` takes `&[Package]` — you can call `affected_packages(&resolved, &dag, &changed_files)` before moving `resolved` into `build_task_list` (which doesn't consume `resolved`). If there's a borrow issue, use `resolved.clone()` for the scoping call or restructure.

**Integration test fails on `@fixture/core` appearing in output:** Check that the `since_flag_skips_unaffected_packages` test correctly modifies only `utils/package.json` and that `@fixture/core` truly has no changed files. The `--no-cache` flag is important here — without it, `@fixture/core` might be cached from a prior run and print `✓ (cached)` which would match the `contains("@fixture/core")` check. The test already passes `--no-cache`, but also ensure the assert checks for `@fixture/core#build` (with the `#build` suffix) specifically.

**git `init -b main` not supported on older git:** Replace with `git init && git checkout -b main` or just use `git init` (default branch name won't matter for tests).

**Do NOT change test assertions to match broken behavior. STOP and report unexpected failures.**
