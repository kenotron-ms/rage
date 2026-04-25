# Phase 5: `--affected` Flag — Uncommitted Changes Scoping

> **Execution:** Use the subagent-driven-development workflow.
> **Prerequisite:** Phase 4 complete — `cargo test --workspace` passes (87 tests), `rage run build --since HEAD~1` works.

**Goal:** Add `--affected` flag that scopes `rage run` to packages with uncommitted changes (staged, unstaged, and new untracked files). This is the everyday dev workflow: "run only what I'm currently working on."

**Architecture:** New function `git_dirty_files(root: &Path) -> Result<Vec<PathBuf>>` in `scoping` crate (combines `git diff --name-only HEAD` + `git ls-files --others --exclude-standard`). CLI gets `--affected` flag as a shorthand for `--since HEAD` with working-tree awareness.

**End state:**
```bash
# Edit a source file in one package, then:
rage run build --affected ~/path/to/monorepo

# Output:
Found N packages (pnpm workspace)
Scoping to packages with uncommitted changes: 2 affected (N-2 scoped out)
Running 'build' across 2 packages
...
Done.
```

**What's deferred:** Watch mode, daemon, remote cache.

---

## Context For The Implementer

Repo at `/Users/ken/workspace/ms/rage`. Work on branch `feat/phase-5-affected-flag` in worktree `.worktrees/feat-phase-5-affected-flag`.

**Existing `scoping` crate has:**
- `git_changed_files(root: &Path, base_ref: &str) -> Result<Vec<PathBuf>>` — runs `git diff --name-only <base_ref>..HEAD`
- `affected_packages(packages, dag, changed_files) -> HashSet<String>` — direct + transitive

**What `git_dirty_files` needs to return:**
1. Files modified but not staged: `git diff --name-only`
2. Files staged (index vs HEAD): `git diff --name-only --cached`
3. Untracked files not in `.gitignore`: `git ls-files --others --exclude-standard`
All three combined, deduplicated, relative to `root` → absolute paths.

**CLI change:** Add `--affected` flag to `Command::Run`. When set, calls `git_dirty_files(root)` instead of `git_changed_files(root, base_ref)`. Mutually exclusive with `--since` (error if both given).

**Rules:**
1. TDD: write failing tests before implementation.
2. Commit after each task with the exact message given.
3. Run `cargo clippy --workspace --all-targets -- -D warnings` after every task.
4. Do NOT add functionality beyond what each task specifies.
5. STOP and report if a test fails unexpectedly.

---

## Task 1: Implement `git_dirty_files`

**Goal:** Add `git_dirty_files` to `scoping` crate.

### Steps

1. Write failing tests in `crates/scoping/src/git.rs` **first** — add to the end of the existing file:

```rust
// ── git_dirty_files tests ────────────────────────────────────────────────────

/// Return absolute paths of files with uncommitted changes in the working tree.
///
/// Combines three git commands:
///   1. `git diff --name-only`         — unstaged modifications
///   2. `git diff --name-only --cached` — staged modifications
///   3. `git ls-files --others --exclude-standard` — untracked files
///
/// Results are deduplicated and returned as absolute paths rooted at `root`.
/// Returns an empty Vec if the working tree is clean.
pub fn git_dirty_files(root: &Path) -> Result<Vec<PathBuf>> {
    todo!()
}

#[cfg(test)]  // These are ADDITIONAL tests, not a new mod block — add to existing tests mod
mod dirty_tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git_init(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            Command::new("git").args(args).current_dir(dir).output().unwrap()
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
    }

    fn git_commit_all(dir: &std::path::Path, msg: &str) {
        Command::new("git").args(["add", "-A"]).current_dir(dir).output().unwrap();
        Command::new("git").args(["commit", "-m", msg]).current_dir(dir).output().unwrap();
    }

    #[test]
    fn detects_unstaged_modification() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("a.ts"), b"original").unwrap();
        git_commit_all(dir.path(), "init");

        // Unstaged modification
        fs::write(dir.path().join("a.ts"), b"modified").unwrap();

        let dirty = git_dirty_files(dir.path()).unwrap();
        let names: Vec<_> = dirty.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"a.ts".to_string()), "unstaged a.ts should be dirty");
    }

    #[test]
    fn detects_staged_modification() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("b.ts"), b"original").unwrap();
        git_commit_all(dir.path(), "init");

        // Staged modification
        fs::write(dir.path().join("b.ts"), b"staged").unwrap();
        Command::new("git").args(["add", "b.ts"]).current_dir(dir.path()).output().unwrap();

        let dirty = git_dirty_files(dir.path()).unwrap();
        let names: Vec<_> = dirty.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"b.ts".to_string()), "staged b.ts should be dirty");
    }

    #[test]
    fn detects_untracked_file() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("existing.ts"), b"x").unwrap();
        git_commit_all(dir.path(), "init");

        // Untracked new file
        fs::write(dir.path().join("new.ts"), b"new content").unwrap();

        let dirty = git_dirty_files(dir.path()).unwrap();
        let names: Vec<_> = dirty.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"new.ts".to_string()), "untracked new.ts should appear");
    }

    #[test]
    fn clean_tree_returns_empty() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("clean.ts"), b"clean").unwrap();
        git_commit_all(dir.path(), "init");

        // Nothing modified
        let dirty = git_dirty_files(dir.path()).unwrap();
        assert!(dirty.is_empty(), "clean tree should return empty Vec");
    }

    #[test]
    fn no_duplicates_when_staged_and_unstaged() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("f.ts"), b"v1").unwrap();
        git_commit_all(dir.path(), "init");

        // Stage a change, then make another unstaged change to same file
        fs::write(dir.path().join("f.ts"), b"v2").unwrap();
        Command::new("git").args(["add", "f.ts"]).current_dir(dir.path()).output().unwrap();
        fs::write(dir.path().join("f.ts"), b"v3").unwrap();

        let dirty = git_dirty_files(dir.path()).unwrap();
        let count = dirty.iter().filter(|p| p.file_name().unwrap() == "f.ts").count();
        assert_eq!(count, 1, "f.ts should appear exactly once despite staged+unstaged changes");
    }

    #[test]
    fn paths_are_absolute() {
        let dir = tempdir().unwrap();
        git_init(dir.path());
        fs::write(dir.path().join("abs.ts"), b"x").unwrap();
        git_commit_all(dir.path(), "init");
        fs::write(dir.path().join("abs.ts"), b"y").unwrap();

        let dirty = git_dirty_files(dir.path()).unwrap();
        assert!(!dirty.is_empty());
        for p in &dirty {
            assert!(p.is_absolute(), "path should be absolute: {p:?}");
        }
    }
}
```

**NOTE on test structure:** The existing `git.rs` already has a `mod tests { ... }` block. Add the new `git_dirty_files` function **outside** any existing test module (in the main module body, after the existing `git_changed_files` function). Add the new tests in a **new** `mod dirty_tests { ... }` block (separate from the existing `mod tests { ... }` block). This keeps them isolated.

2. Run `cargo test -p scoping --lib git 2>&1 | tail -10` — confirm the 6 new tests fail with `not yet implemented`.

3. Implement `git_dirty_files`:

```rust
pub fn git_dirty_files(root: &Path) -> Result<Vec<PathBuf>> {
    // Helper to run a git command and collect output lines as absolute paths
    let run_git = |args: &[&str]| -> Result<Vec<PathBuf>> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .context("running git")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("git {:?} failed: {stderr}", args);
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| root.join(l))
            .collect())
    };

    let unstaged = run_git(&["diff", "--name-only"])?;
    let staged = run_git(&["diff", "--name-only", "--cached"])?;
    let untracked = run_git(&["ls-files", "--others", "--exclude-standard"])?;

    // Deduplicate using a HashSet, then collect back to Vec
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for path in unstaged.into_iter().chain(staged).chain(untracked) {
        if seen.insert(path.clone()) {
            result.push(path);
        }
    }
    result.sort(); // deterministic order
    Ok(result)
}
```

4. Update `lib.rs` to re-export `git_dirty_files`:
```rust
pub use git::git_dirty_files;
```

5. Run `cargo test -p scoping 2>&1 | tail -10` — confirm **all tests pass** (10 prior + 6 new = 16 scoping tests), 0 failed.

6. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

7. Commit:
```
feat(scoping): git_dirty_files — staged, unstaged, and untracked changes
```

---

## Task 2: Add `--affected` Flag to CLI

**Goal:** `rage run <script> --affected` runs only packages with uncommitted changes (direct + transitive).

### Steps

1. Add `--affected` flag to `Command::Run` in `crates/cli/src/main.rs`:

```rust
Run {
    script: String,
    #[arg(long)]
    workspace: Option<PathBuf>,
    workspace_pos: Option<PathBuf>,
    #[arg(long)]
    no_cache: bool,
    /// Scope to packages affected since this git ref (e.g. HEAD~1, origin/main).
    #[arg(long)]
    since: Option<String>,
    /// Scope to packages with uncommitted changes (staged, unstaged, untracked).
    /// Cannot be combined with --since.
    #[arg(long)]
    affected: bool,
},
```

2. Update the `cmd_run` signature to accept `affected: bool`:

```rust
async fn cmd_run(
    root: &Path,
    script: &str,
    no_cache: bool,
    since: Option<&str>,
    affected: bool,
) -> Result<()> {
```

3. Add a guard at the top of `cmd_run` to reject `--since` + `--affected` together:
```rust
if since.is_some() && affected {
    anyhow::bail!("--since and --affected are mutually exclusive");
}
```

4. Change the scoping block to handle all three modes (no scope / `--since` / `--affected`):

```rust
let scope: Option<std::collections::HashSet<String>> = if let Some(base_ref) = since {
    let changed_files = scoping::git_changed_files(root, base_ref)
        .with_context(|| format!("computing changed files since {base_ref}"))?;
    let aff = scoping::affected_packages(&resolved, &dag, &changed_files);
    eprintln!(
        "Scoping to packages affected since {base_ref}: {} affected ({} scoped out)",
        aff.len(),
        resolved.len().saturating_sub(aff.len())
    );
    Some(aff)
} else if affected {
    let dirty_files = scoping::git_dirty_files(root)
        .context("computing uncommitted changed files")?;
    let aff = scoping::affected_packages(&resolved, &dag, &dirty_files);
    eprintln!(
        "Scoping to packages with uncommitted changes: {} affected ({} scoped out)",
        aff.len(),
        resolved.len().saturating_sub(aff.len())
    );
    Some(aff)
} else {
    None
};
```

5. Update the `Command::Run` match arm to pass `affected`:
```rust
Command::Run {
    script,
    workspace,
    workspace_pos,
    no_cache,
    since,
    affected,
} => {
    let root = resolve_workspace(workspace_pos, workspace);
    cmd_run(&root, &script, no_cache, since.as_deref(), affected).await
}
```

6. Run `cargo build -p rage-cli 2>&1 | tail -3` — confirm clean build.

7. Run `cargo test --workspace 2>&1 | grep "^test result"` — confirm all tests still pass.

8. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

9. Commit:
```
feat(cli): --affected flag for uncommitted-changes scoping
```

---

## Task 3: Integration Tests for `--affected`

**Goal:** Prove `--affected` works end-to-end with a real git repo and real uncommitted changes.

### Steps

1. Add two new tests to `crates/cli/tests/integration.rs`:

```rust
// ── --affected flag tests ────────────────────────────────────────────────────

#[test]
fn affected_flag_scopes_to_dirty_packages() {
    use std::fs;
    use std::process::Command as Cmd;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();

    // Copy the js-pnpm fixture
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("fixtures").join("js-pnpm");

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir_recursive(&entry.path(), &dst_path);
            } else {
                fs::copy(&entry.path(), &dst_path).unwrap();
            }
        }
    }
    copy_dir_recursive(&fixtures_dir, root);

    // Init git and commit everything
    let git = |args: &[&str]| {
        Cmd::new("git").args(args).current_dir(root).output().unwrap()
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.email", "test@test.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial"]);

    // Modify only utils package — unstaged change
    let utils_pkg = root.join("packages").join("utils").join("package.json");
    let original = fs::read_to_string(&utils_pkg).unwrap();
    let modified = original.replace("\"version\": \"1.0.0\"", "\"version\": \"1.0.1\"");
    fs::write(&utils_pkg, modified).unwrap();
    // Do NOT commit — this is an uncommitted change

    let bin = env!("CARGO_BIN_EXE_rage");
    let output = Cmd::new(bin)
        .args(["run", "build", "--affected", "--no-cache"])
        .arg(root)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "rage run --affected should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // utils, ui, app should run; core should not
    assert!(stderr.contains("@fixture/utils"), "utils should run\n{stderr}");
    assert!(stderr.contains("@fixture/ui"), "ui should run (depends on utils)\n{stderr}");
    assert!(stderr.contains("@fixture/app"), "app should run (depends on ui)\n{stderr}");
    assert!(
        !stderr.contains("@fixture/core#build"),
        "core should be scoped out\n{stderr}"
    );

    // Scoping message should appear
    assert!(
        stderr.contains("uncommitted changes"),
        "scoping message should mention uncommitted changes\n{stderr}"
    );
}

#[test]
fn since_and_affected_are_mutually_exclusive() {
    let bin = env!("CARGO_BIN_EXE_rage");
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("fixtures").join("js-pnpm");

    let output = std::process::Command::new(bin)
        .args(["run", "build", "--since", "HEAD~1", "--affected"])
        .arg(&fixtures_dir)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "--since and --affected together should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutually exclusive"),
        "error message should mention 'mutually exclusive'\n{stderr}"
    );
}
```

2. Run `cargo test --workspace 2>&1 | grep -E "(affected|FAILED)"` — confirm both new tests pass.

3. Run full suite: `cargo test --workspace 2>&1 | grep "^test result" | awk '{sum += $4} END {print "Total:", sum}'`
   Expected: ≥ 95 tests (87 prior + ~6 git_dirty_files unit + 2 CLI integration).

4. Run `cargo clippy --workspace --all-targets -- -D warnings` — fix any warnings.

5. Commit:
```
test(cli): integration tests for --affected flag
```

---

## Task 4: Final Verification

### Steps

1. Build release binary: `cargo build --release 2>&1 | tail -3`

2. Run full test suite: `cargo test --workspace 2>&1 | grep "^test result" | awk '{sum += $4; fail += $6} END {print "Passing:", sum, "| Failing:", fail}'`
   Expected: ≥ 95 passing, 0 failing.

3. Demonstrate `--affected` with a real working-tree change:
```bash
# Create temp fixture git repo
TMPDIR=$(mktemp -d)
cp -r fixtures/js-pnpm/. "$TMPDIR/"
cd "$TMPDIR"
git init -b main && git config user.email t@t.com && git config user.name T
git add -A && git commit -m init
# Make an unstaged modification to utils
echo '{"name":"@fixture/utils","version":"1.0.1","scripts":{"build":"echo building @fixture/utils"},"dependencies":{"@fixture/core":"workspace:*"}}' > packages/utils/package.json
cd -
./target/release/rage run build --affected --no-cache "$TMPDIR" 2>&1
```
Expected:
```
Found 4 packages (pnpm workspace)
Scoping to packages with uncommitted changes: 3 affected (1 scoped out)
Running 'build' across 3 packages
...
Done.
```
`@fixture/core` must NOT appear. `utils`, `ui`, `app` must appear.

4. Demonstrate mutual exclusion error:
```bash
./target/release/rage run build --since HEAD~1 --affected fixtures/js-pnpm 2>&1
```
Expected: non-zero exit, message about "mutually exclusive".

5. Print git log: `git log --oneline -10`

6. Report **STATUS: DONE** if all steps pass.

---

## Expected Test Count Growth

| Phase | Tests |
|-------|-------|
| End of Phase 4 | 87 |
| After Task 1 (+6 git_dirty_files tests) | 93 |
| After Task 2 (no new tests) | 93 |
| After Task 3 (+2 integration tests) | 95 |
