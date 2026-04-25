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
        .arg(fixtures_dir().join("js-npm"))   // npm = 3 pkgs via flag
        .arg(fixtures_dir().join("js-pnpm"))  // pnpm = 4 pkgs via positional (wins)
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
    assert!(stderr.contains("Found 4 packages"), "should report package count");
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
