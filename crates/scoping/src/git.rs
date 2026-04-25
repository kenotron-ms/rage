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

/// Return absolute paths of files with uncommitted changes (staged, unstaged, or untracked).
///
/// Combines three git queries:
/// - `git diff --name-only` — unstaged modifications
/// - `git diff --name-only --cached` — staged modifications
/// - `git ls-files --others --exclude-standard` — untracked files
///
/// Results are deduplicated and sorted for deterministic order. Returns an empty `Vec`
/// if the working tree is clean.
pub fn git_dirty_files(root: &Path) -> Result<Vec<PathBuf>> {
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

    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for path in unstaged.into_iter().chain(staged).chain(untracked) {
        if seen.insert(path.clone()) {
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
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
        assert!(
            names.contains(&"a.ts".to_string()),
            "a.ts should be changed"
        );
        assert!(
            !names.contains(&"b.ts".to_string()),
            "b.ts should not be changed"
        );
    }

    #[test]
    fn returns_only_files_in_diff_range() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("a.ts"), b"const a = 1;").unwrap();
        git_commit_all(root, "initial");

        fs::write(root.join("unchanged.ts"), b"unchanged").unwrap();
        git_commit_all(root, "second identical");

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

        fs::write(
            root.join("packages").join("core").join("index.ts"),
            b"export const x = 1;",
        )
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

#[cfg(test)]
mod dirty_tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

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
    fn detects_unstaged_modification() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("file.txt"), b"initial").unwrap();
        git_commit_all(root, "initial");

        // Modify without staging
        fs::write(root.join("file.txt"), b"modified").unwrap();

        let dirty = git_dirty_files(root).unwrap();
        let names: Vec<_> = dirty
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"file.txt".to_string()),
            "file.txt should appear in dirty list; got: {names:?}"
        );
    }

    #[test]
    fn detects_staged_modification() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("file.txt"), b"initial").unwrap();
        git_commit_all(root, "initial");

        // Modify and stage
        fs::write(root.join("file.txt"), b"modified").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(root)
            .output()
            .unwrap();

        let dirty = git_dirty_files(root).unwrap();
        let names: Vec<_> = dirty
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"file.txt".to_string()),
            "file.txt should appear in dirty list; got: {names:?}"
        );
    }

    #[test]
    fn detects_untracked_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("existing.txt"), b"existing").unwrap();
        git_commit_all(root, "initial");

        // Create new untracked file
        fs::write(root.join("new.txt"), b"new file").unwrap();

        let dirty = git_dirty_files(root).unwrap();
        let names: Vec<_> = dirty
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"new.txt".to_string()),
            "new.txt should appear in dirty list; got: {names:?}"
        );
    }

    #[test]
    fn clean_tree_returns_empty() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("file.txt"), b"content").unwrap();
        git_commit_all(root, "initial");

        let dirty = git_dirty_files(root).unwrap();
        assert!(
            dirty.is_empty(),
            "clean tree should return empty Vec; got: {dirty:?}"
        );
    }

    #[test]
    fn no_duplicates_when_staged_and_unstaged() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("file.txt"), b"initial").unwrap();
        git_commit_all(root, "initial");

        // Stage one change
        fs::write(root.join("file.txt"), b"staged change").unwrap();
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(root)
            .output()
            .unwrap();

        // Then make an additional unstaged change
        fs::write(root.join("file.txt"), b"also unstaged").unwrap();

        let dirty = git_dirty_files(root).unwrap();
        let count = dirty
            .iter()
            .filter(|p| p.file_name().unwrap() == "file.txt")
            .count();
        assert_eq!(
            count, 1,
            "file.txt should appear exactly once; got {count} occurrences in {dirty:?}"
        );
    }

    #[test]
    fn paths_are_absolute() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        git_init(root);

        fs::write(root.join("a.txt"), b"a").unwrap();
        git_commit_all(root, "initial");

        // Create an untracked file to ensure we have results
        fs::write(root.join("b.txt"), b"b").unwrap();

        let dirty = git_dirty_files(root).unwrap();
        assert!(!dirty.is_empty(), "should have at least one dirty file");
        for path in &dirty {
            assert!(
                path.is_absolute(),
                "all paths should be absolute, got: {path:?}"
            );
        }
    }
}
