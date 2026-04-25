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
