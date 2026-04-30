//! Integration tests for the `rage` binary.

use std::path::PathBuf;
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates/
        .parent()
        .unwrap() // workspace root
        .join("fixtures")
}

fn rage() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rage"))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), dst_path).unwrap();
        }
    }
}

// ── pnpm smoke tests ───────────────────────────────────────────────────────

#[test]
fn graph_pnpm_stdout_is_dot() {
    let output = rage()
        .arg("graph")
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .expect("failed to run rage");

    assert!(output.status.success(), "exit code: {}", output.status);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.starts_with("digraph workspace {"),
        "expected DOT output, got:\n{stdout}"
    );
}

#[test]
fn graph_pnpm_stderr_reports_count() {
    let output = rage()
        .arg("graph")
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .expect("failed to run rage");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        "Found 4 packages (pnpm workspace)",
        "stderr was: {stderr:?}"
    );
}

// ── yarn smoke tests ───────────────────────────────────────────────────────

#[test]
fn graph_yarn_stderr_reports_count() {
    let output = rage()
        .arg("graph")
        .arg(fixtures_dir().join("js-yarn"))
        .output()
        .expect("failed to run rage");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        "Found 3 packages (yarn workspace)",
        "stderr was: {stderr:?}"
    );
}

// ── npm smoke tests ────────────────────────────────────────────────────────

#[test]
fn graph_npm_stderr_reports_count() {
    let output = rage()
        .arg("graph")
        .arg(fixtures_dir().join("js-npm"))
        .output()
        .expect("failed to run rage");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        "Found 3 packages (npm workspace)",
        "stderr was: {stderr:?}"
    );
}

// ── DOT content checks ─────────────────────────────────────────────────────

#[test]
fn graph_pnpm_dot_contains_expected_nodes() {
    let output = rage()
        .arg("graph")
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .expect("failed to run rage");

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"@fixture/app\""), "dot:\n{stdout}");
    assert!(stdout.contains("\"@fixture/core\""), "dot:\n{stdout}");
}

// ── --workspace flag tests ────────────────────────────────────────────────────

#[test]
fn graph_via_workspace_flag() {
    let output = rage()
        .args(["graph", "--workspace"])
        .arg(fixtures_dir().join("js-yarn"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.trim(), "Found 3 packages (yarn workspace)");
}

#[test]
fn positional_overrides_workspace_flag() {
    let output = rage()
        .args(["graph", "--workspace"])
        .arg(fixtures_dir().join("js-npm")) // npm = 3 pkgs via flag
        .arg(fixtures_dir().join("js-pnpm")) // pnpm = 4 pkgs via positional (wins)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.trim(), "Found 4 packages (pnpm workspace)");
}

// ── rage run tests ────────────────────────────────────────────────────────────

#[test]
fn run_build_pnpm_exits_zero() {
    let output = rage()
        .args(["run", "build"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rage run build should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Found 4 packages"),
        "should report package count"
    );
    assert!(stderr.contains("Done."), "should report completion");
}

#[test]
fn run_build_shows_all_packages() {
    let output = rage()
        .args(["run", "build"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("@fixture/core"), "core should run");
    assert!(stderr.contains("@fixture/utils"), "utils should run");
    assert!(stderr.contains("@fixture/ui"), "ui should run");
    assert!(stderr.contains("@fixture/app"), "app should run");
}

#[test]
fn run_unknown_script_exits_nonzero() {
    let output = rage()
        .args(["run", "nonexistent-script"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "rage run nonexistent-script should exit nonzero"
    );
}

// ── --since flag tests ──────────────────────────────────────────────

#[test]
fn since_flag_is_recognized() {
    // Before --since is implemented, clap rejects unknown flags with
    // "unexpected argument". After it is wired in, that message must
    // not appear regardless of whether git diff itself succeeds or fails.
    let output = rage()
        .args(["run", "build", "--since", "HEAD"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--since should be a recognized flag, got stderr:\n{stderr}"
    );
}

// ── cache integration tests ──────────────────────────────────────────────

#[test]
fn no_cache_flag_accepted() {
    // Verify --no-cache is a recognized flag (doesn't error with "unexpected argument")
    let bin = env!("CARGO_BIN_EXE_rage");
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures");
    let output = std::process::Command::new(bin)
        .args(["run", "build", "--no-cache"])
        .arg(fixtures_dir.join("js-pnpm"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rage run build --no-cache should succeed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Done."));
}

#[test]
fn second_run_uses_cache() {
    use tempfile::tempdir;
    // Use a rage.json-specified cache dir to isolate this test run from
    // ~/.rage/cache and guarantee repeatability. The default code path (no
    // --no-cache) now uses TwoPhaseCache, so the second run prints
    // "(cached, two-phase)" instead of the old "(cached)".

    let workspace = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_rage");

    // Copy the js-pnpm fixture into the isolated workspace so we can add a
    // rage.json that points the cache at our temp dir.
    let pnpm_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("js-pnpm");
    copy_dir_recursive(&pnpm_fixture, workspace.path());

    // Write rage.json to redirect the cache to our temp dir.
    std::fs::write(
        workspace.path().join("rage.json"),
        format!(
            r#"{{ "cache": {{ "backend": "local", "dir": "{}" }} }}"#,
            cache_dir.path().to_string_lossy()
        ),
    )
    .unwrap();

    let run = || {
        std::process::Command::new(bin)
            .args(["run", "build"])
            .arg(workspace.path())
            .output()
            .unwrap()
    };

    // First run — cold cache
    let first = run();
    assert!(first.status.success());

    // Second run — warm TwoPhaseCache
    let second = run();
    assert!(second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("(cached, two-phase)"),
        "second run should show two-phase cached tasks, got:\n{stderr}"
    );
}

// ── --affected flag tests ──────────────────────────────────────────────────

#[test]
fn affected_flag_is_recognized() {
    // Before --affected is implemented, clap rejects unknown flags with
    // "unexpected argument". After it is wired in, that message must
    // not appear regardless of whether git dirty-check itself succeeds or fails.
    let output = rage()
        .args(["run", "build", "--affected"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--affected should be a recognized flag, got stderr:\n{stderr}"
    );
}

#[test]
fn since_and_affected_are_mutually_exclusive() {
    // Using both --since and --affected together should exit nonzero and
    // print a message containing "mutually exclusive".
    let output = rage()
        .args(["run", "build", "--since", "HEAD~1", "--affected"])
        .arg(fixtures_dir().join("js-pnpm"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "rage run --since HEAD --affected should exit nonzero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutually exclusive"),
        "error message should say 'mutually exclusive', got:\n{stderr}"
    );
}

#[test]
fn affected_flag_scopes_to_dirty_packages() {
    use std::fs;
    use std::process::Command as Cmd;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path();

    // 1. Copy the js-pnpm fixture
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("js-pnpm");

    copy_dir_recursive(&fixtures_dir, root);

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

    // 3. Make an *uncommitted* change to @fixture/utils
    let utils_pkg = root.join("packages").join("utils").join("package.json");
    let original = fs::read_to_string(&utils_pkg).unwrap();
    let modified = original.replace("\"version\": \"1.0.0\"", "\"version\": \"1.0.1\"");
    fs::write(&utils_pkg, modified).unwrap();

    // 4. Run rage with --affected (no git add / commit)
    let bin = env!("CARGO_BIN_EXE_rage");
    let output = Cmd::new(bin)
        .args(["run", "build", "--affected", "--no-cache"])
        .arg(root)
        .env("RAGE_CACHE_DIR", root.join(".rage-test-cache"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "rage run --affected should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // utils, ui, and app should run (utils modified; ui+app depend on utils)
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

    // core should NOT run (doesn't depend on utils)
    assert!(
        !stderr.contains("@fixture/core#build"),
        "core should be scoped out\n{stderr}"
    );

    // Scoping message should appear
    assert!(
        stderr.contains("Scoping to packages with uncommitted changes"),
        "scoping message should appear\n{stderr}"
    );
}

// ── TwoPhaseCache integration test ─────────────────────────────────────────

/// Verify the default (no --no-cache) code path creates TwoPhaseCache-specific
/// files (`wf-*.pathsets`) in the rage.json-specified cache directory.
/// This test fails with LocalCache (which writes `{fingerprint}.json` files)
/// and passes only when TwoPhaseCache is the default.
#[test]
fn default_run_uses_two_phase_cache() {
    use std::process::Command;

    let workspace = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();

    // Minimal pnpm workspace with one package
    std::fs::write(
        workspace.path().join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("package.json"),
        br#"{"name":"root","private":true}"#,
    )
    .unwrap();
    let pkg = workspace.path().join("packages/p");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo hi"}}"#,
    )
    .unwrap();

    // rage.json points cache.dir at our temp dir
    std::fs::write(
        workspace.path().join("rage.json"),
        format!(
            r#"{{ "cache": {{ "backend": "local", "dir": "{}" }} }}"#,
            cache_dir.path().to_string_lossy()
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_rage");
    let out = Command::new(bin)
        .args(["run", "build"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rage run build should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // TwoPhaseCache writes `wf-*.pathsets` files (LocalCache writes `{fp}.json`).
    // The presence of a wf-* file proves TwoPhaseCache was used.
    let entries: Vec<_> = std::fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        entries
            .iter()
            .any(|e| e.file_name().to_string_lossy().starts_with("wf-")),
        "expected TwoPhaseCache wf-*.pathsets file in {}; found: {:?}",
        cache_dir.path().display(),
        entries
            .iter()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    );
}

// ── scoping integration tests ──────────────────────────────────────────────

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

    copy_dir_recursive(&fixtures_dir, root);

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
    let modified = original.replace("\"version\": \"1.0.0\"", "\"version\": \"1.0.1\"");
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
fn rage_run_loads_rage_json_cache_dir() {
    use std::process::Command;

    let workspace = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();

    // minimal pnpm workspace with one package
    std::fs::write(
        workspace.path().join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("package.json"),
        br#"{"name":"root","private":true}"#,
    )
    .unwrap();
    let pkg = workspace.path().join("packages/p");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo hi"}}"#,
    )
    .unwrap();

    // rage.json points cache.dir at our tempdir
    std::fs::write(
        workspace.path().join("rage.json"),
        format!(
            r#"{{ "cache": {{ "backend": "local", "dir": "{}" }} }}"#,
            cache_dir.path().to_string_lossy()
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_rage");
    let out = Command::new(bin)
        .args(["run", "build"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Cache should have been written into rage.json's cache.dir, not ~/.rage/cache
    let entries: Vec<_> = std::fs::read_dir(cache_dir.path()).unwrap().collect();
    assert!(
        !entries.is_empty(),
        "expected cache entries in {}",
        cache_dir.path().display()
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

    copy_dir_recursive(&fixtures_dir, root);

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

// ── rage open tests ──────────────────────────────────────────────────────────

#[test]
fn rage_open_errors_when_no_daemon() {
    use std::process::Command;
    let workspace = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_rage");
    let out = Command::new(bin)
        .args(["open"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "open should fail when no daemon");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no daemon"),
        "expected 'no daemon' message, got: {err}"
    );
}

// ── daemon / dev / status integration tests ──────────────────────────────────

/// Verify that `rage dev build <workspace>` spawns a detached daemon, sends
/// SetDesiredState, and returns within 10 seconds; and that `rage status`
/// round-trips GetState against the same daemon; finally the daemon is shut
/// down cleanly via the Unix socket.
#[cfg(unix)]
#[test]
fn rage_dev_starts_daemon_and_returns_quickly() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Instant;
    use tempfile::tempdir;

    // Isolated HOME so daemon discovery files don't pollute ~/.rage/daemons
    let fake_home = tempdir().unwrap();

    // Minimal pnpm workspace fixture
    let workspace = tempdir().unwrap();
    let ws = workspace.path();
    std::fs::write(
        ws.join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("package.json"),
        br#"{"name":"root","private":true}"#,
    )
    .unwrap();
    let pkg = ws.join("packages/a");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@test/a","version":"1.0.0","scripts":{"build":"echo hi"}}"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_rage");

    // ── rage dev build <workspace> ──────────────────────────────────────────
    let start = Instant::now();
    let dev_out = std::process::Command::new(bin)
        .args(["dev", "build"])
        .arg(ws)
        .env("HOME", fake_home.path())
        .output()
        .expect("failed to run rage dev");
    let elapsed = start.elapsed();

    assert!(
        dev_out.status.success(),
        "rage dev should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&dev_out.stderr)
    );
    assert!(
        elapsed.as_secs() < 10,
        "rage dev took {elapsed:?}, expected < 10s"
    );

    // ── rage status <workspace> ─────────────────────────────────────────────
    let status_out = std::process::Command::new(bin)
        .args(["status"])
        .arg(ws)
        .env("HOME", fake_home.path())
        .output()
        .expect("failed to run rage status");

    assert!(
        status_out.status.success(),
        "rage status should exit 0; stderr:\n{}",
        String::from_utf8_lossy(&status_out.stderr)
    );

    // ── Cleanup: send Shutdown over the Unix socket ─────────────────────────
    let daemons_dir = fake_home.path().join(".rage").join("daemons");
    if let Ok(entries) = std::fs::read_dir(&daemons_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "sock") {
                if let Ok(mut stream) = UnixStream::connect(&p) {
                    let _ = stream.write_all(b"{\"type\":\"Shutdown\"}\n");
                    let mut buf = String::new();
                    let _ = stream.read_to_string(&mut buf);
                }
            }
        }
    }
    // Give the daemon a moment to exit
    std::thread::sleep(std::time::Duration::from_millis(300));
}

// ── root install task tests ─────────────────────────────────────────────────

/// Stage a minimal pnpm workspace inside `dir`:
/// - workspace package.json
/// - pnpm-workspace.yaml referencing packages/*
/// - pnpm-lock.yaml so the TypeScript plugin detects pnpm
/// - one package `@fixture/install-test` with a `build` script
fn stage_pnpm_workspace(dir: &std::path::Path) {
    std::fs::write(
        dir.join("package.json"),
        br#"{"name":"root","private":true,"version":"0.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    )
    .unwrap();
    std::fs::write(dir.join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();
    let pkg = dir.join("packages").join("install-test");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@fixture/install-test","version":"1.0.0","scripts":{"build":"echo BUILT-PACKAGE"}}"#,
    )
    .unwrap();
}

/// Write a fake `pnpm` shim to `bin_dir` that records its argv to
/// `bin_dir/pnpm.log` and exits 0. Returns `bin_dir` for PATH prepending.
fn install_pnpm_shim(bin_dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin_dir).unwrap();
    let shim = bin_dir.join("pnpm");
    let log = bin_dir.join("pnpm.log");
    let script = format!(
        "#!/bin/sh\necho FAKE-PNPM \"$@\" >> '{}'\n# Create a stub node_modules so verify_install_effects returns true\nmkdir -p \"${{PWD}}/node_modules/.bin\"\ntouch \"${{PWD}}/node_modules/.bin/.fake-install\"\necho INSTALL-RAN\nexit 0\n",
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

    // Run #1 - install runs.
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

    // Run #2 - install must be cached.
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
    assert_eq!(
        invocations, 1,
        "pnpm should run exactly once across both rage invocations; log:\n{log}"
    );
}

// ── node PATH injection unit test ─────────────────────────────────────────────

#[test]
fn js_task_path_includes_node_modules_bin() {
    use std::fs;
    use tempfile::tempdir;

    // Build a minimal yarn workspace with one package whose `build` script
    // resolves a tool that ONLY exists at workspace_root/node_modules/.bin.
    let work = tempdir().unwrap();
    let root = work.path();
    fs::write(
        root.join("package.json"),
        br#"{"name":"r","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::write(root.join("yarn.lock"), b"# yarn lockfile v1\n").unwrap();

    // Create both workspace root and package node_modules/.bin dirs.
    let ws_bin = root.join("node_modules/.bin");
    fs::create_dir_all(&ws_bin).unwrap();
    let stub = ws_bin.join("my-stub-tool");
    fs::write(&stub, b"#!/bin/sh\necho FOUND\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub, perms).unwrap();
    }

    // One package with a build script that just runs the stub.
    let pkg = root.join("packages/p");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(
        pkg.join("package.json"),
        br#"{"name":"p","version":"0.0.0","scripts":{"build":"my-stub-tool"}}"#,
    )
    .unwrap();

    // Create the package-level node_modules/.bin so it's included in PATH.
    let pkg_bin = pkg.join("node_modules/.bin");
    fs::create_dir_all(&pkg_bin).unwrap();

    // Verify PATH injection via the public API.
    use scheduler::node_path::build_node_path;

    let path = build_node_path(&pkg, root, "/usr/bin:/bin");
    assert!(
        path.contains(&format!("{}/node_modules/.bin", pkg.display())),
        "package node_modules/.bin missing from PATH: {path}"
    );
    assert!(
        path.contains(&format!("{}/node_modules/.bin", root.display())),
        "workspace node_modules/.bin missing from PATH: {path}"
    );

    // Verify that NODE_VERSION flows into the install task's env_hash_inputs.
    fs::write(root.join(".node-version"), "18.20.4\n").unwrap();
    use plugin::EcosystemPlugin;
    use plugin_typescript::TypeScriptPlugin;
    let ts = TypeScriptPlugin::new();
    let rts = ts.infer_root_tasks(root);
    assert_eq!(rts.len(), 1);
    assert_eq!(
        rts[0].env_hash_inputs,
        vec![("NODE_VERSION".to_string(), "18.20.4".to_string())]
    );
}

// ── Observation-driven artifact cache — Plan B e2e integration tests ──────────
// These tests require the real sandbox dylib + a real pnpm workspace at
// /tmp/rage-symlink-poc/pnpm-test. They are marked #[ignore] and must be
// run manually: cargo test --test integration --ignored --nocapture

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn pnpm_workspace_capture_writes_manifest() {
    use std::process::Command;
    let ws = std::path::Path::new("/tmp/rage-symlink-poc/pnpm-test");
    if !ws.join("node_modules").exists() {
        eprintln!("skip: pnpm fixture not present at {ws:?}");
        return;
    }
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/debug/rage");
    assert!(bin.exists(), "build rage first: cargo build -p rage-cli");
    let status = Command::new(&bin)
        .args(["run", "build"])
        .current_dir(ws)
        .status()
        .unwrap();
    assert!(status.success(), "rage run build failed");

    let rage_home = std::env::var("RAGE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(".rage")
        });
    let cache_root = rage_home.join("cache");
    let any = walkdir::WalkDir::new(&cache_root)
        .into_iter()
        .flatten()
        .any(|e| {
            let s = e.path().to_string_lossy().to_string();
            s.contains("/artifact-packages/") && s.ends_with(".json")
        });
    assert!(any, "no artifact-packages/<fp>.json manifest written");
}

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
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/debug/rage");
    assert!(bin.exists(), "build rage first");

    // First run: populates the CAS + writes the manifest
    let s1 = Command::new(&bin)
        .args(["run", "build"])
        .current_dir(ws)
        .status()
        .unwrap();
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

    // If the fixture has any scoped package, ensure its @scope/name layout is preserved.
    let mut found_scoped = false;
    for entry in std::fs::read_dir(&nm).unwrap().flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n.starts_with('@') {
            for sub in std::fs::read_dir(entry.path()).unwrap().flatten() {
                assert!(
                    sub.path().is_dir(),
                    "scoped pkg {sub:?} must be a directory"
                );
                found_scoped = true;
            }
        }
    }
    if !found_scoped {
        eprintln!("note: pnpm fixture has no scoped packages — scoped layout assertion skipped");
    }
}

// ── diamond-dep fixture structure test ────────────────────────────────────────

#[test]
fn distributed_fixture_structure() {
    let base = e2e_fixtures_dir().join("distributed");

    // Root files
    assert!(base.join("package.json").exists(), "missing distributed/package.json");
    assert!(base.join("pnpm-workspace.yaml").exists(), "missing distributed/pnpm-workspace.yaml");
    assert!(base.join("tsconfig.base.json").exists(), "missing distributed/tsconfig.base.json");
    assert!(base.join(".gitignore").exists(), "missing distributed/.gitignore");

    // Five packages
    let pkgs = base.join("packages");
    for pkg in &["pkg-a", "pkg-b", "pkg-c", "pkg-d", "pkg-e"] {
        let p = pkgs.join(pkg);
        assert!(p.exists(), "missing package directory: {pkg}");
        assert!(p.join("package.json").exists(), "missing {pkg}/package.json");
        assert!(p.join("tsconfig.json").exists(), "missing {pkg}/tsconfig.json");
        assert!(p.join("src").join("index.ts").exists(), "missing {pkg}/src/index.ts");
    }

    // Verify scope name in root package.json
    let root_pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(base.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(root_pkg["name"], "@fix-dist/root", "root package name mismatch");

    // Verify dependency graph via package.json files
    let pkg_c: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-c").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_c["dependencies"]["@fix-dist/pkg-a"].as_str().is_some(),
        "pkg-c must depend on pkg-a"
    );

    let pkg_d: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-d").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_d["dependencies"]["@fix-dist/pkg-b"].as_str().is_some(),
        "pkg-d must depend on pkg-b"
    );

    let pkg_e: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-e").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_e["dependencies"]["@fix-dist/pkg-c"].as_str().is_some(),
        "pkg-e must depend on pkg-c"
    );
    assert!(
        pkg_e["dependencies"]["@fix-dist/pkg-d"].as_str().is_some(),
        "pkg-e must depend on pkg-d"
    );
}

fn e2e_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates/
        .parent()
        .unwrap() // workspace root
        .join("tests")
        .join("fixtures")
}

#[test]
fn diamond_dep_fixture_structure() {
    let base = e2e_fixtures_dir().join("diamond-dep");

    // Root files
    assert!(base.join("package.json").exists(), "missing diamond-dep/package.json");
    assert!(base.join("pnpm-workspace.yaml").exists(), "missing diamond-dep/pnpm-workspace.yaml");
    assert!(base.join("tsconfig.base.json").exists(), "missing diamond-dep/tsconfig.base.json");
    assert!(base.join(".gitignore").exists(), "missing diamond-dep/.gitignore");

    // Four packages
    let pkgs = base.join("packages");
    for pkg in &["pkg-shared", "pkg-a", "pkg-b", "pkg-app"] {
        let p = pkgs.join(pkg);
        assert!(p.exists(), "missing package directory: {pkg}");
        assert!(p.join("package.json").exists(), "missing {pkg}/package.json");
        assert!(p.join("tsconfig.json").exists(), "missing {pkg}/tsconfig.json");
        assert!(p.join("src").join("index.ts").exists(), "missing {pkg}/src/index.ts");
    }

    // Verify scope names in package.json files
    let root_pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(base.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(root_pkg["name"], "@fix-dd/root", "root package name mismatch");

    // Verify diamond dependency graph via package.json dependencies
    let pkg_a: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-a").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_a["dependencies"]["@fix-dd/pkg-shared"].as_str().is_some(),
        "pkg-a must depend on pkg-shared"
    );

    let pkg_b: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-b").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_b["dependencies"]["@fix-dd/pkg-shared"].as_str().is_some(),
        "pkg-b must depend on pkg-shared"
    );

    let pkg_app: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkgs.join("pkg-app").join("package.json")).unwrap(),
    )
    .unwrap();
    assert!(
        pkg_app["dependencies"]["@fix-dd/pkg-a"].as_str().is_some(),
        "pkg-app must depend on pkg-a"
    );
    assert!(
        pkg_app["dependencies"]["@fix-dd/pkg-b"].as_str().is_some(),
        "pkg-app must depend on pkg-b"
    );
}

// ── Lockfile audit ────────────────────────────────────────────────────────────

/// Every independent fixture workspace must have its own `pnpm-lock.yaml`.
///
/// These lockfiles are checked in so that CI can install dependencies
/// reproducibly without network access.  If a fixture is missing its lockfile,
/// run `pnpm install` inside that directory and commit the result.
#[test]
fn fixture_lockfiles_all_present() {
    let fixtures = e2e_fixtures_dir();
    let expected = [
        "cache-correctness",
        "diamond-dep",
        "distributed",
        "error-propagation",
        "partial-rebuild",
    ];

    for name in &expected {
        let lockfile = fixtures.join(name).join("pnpm-lock.yaml");
        assert!(
            lockfile.exists(),
            "missing lockfile: {}\n\
             Fix: cd tests/fixtures/{} && pnpm install",
            lockfile.display(),
            name
        );
    }
}

/// There must be no stray `pnpm-lock.yaml` or `node_modules` directly under
/// `tests/fixtures/`.  Each fixture is an independent pnpm workspace; there
/// is no parent workspace at `tests/fixtures/` level.
#[test]
fn no_stray_files_at_fixtures_root() {
    let fixtures = e2e_fixtures_dir();

    let stray_lockfile = fixtures.join("pnpm-lock.yaml");
    assert!(
        !stray_lockfile.exists(),
        "stray file found: {}\n\
         Fix: rm -f {}",
        stray_lockfile.display(),
        stray_lockfile.display()
    );

    let stray_nm = fixtures.join("node_modules");
    assert!(
        !stray_nm.exists(),
        "stray directory found: {}\n\
         Fix: rm -rf {}",
        stray_nm.display(),
        stray_nm.display()
    );
}
