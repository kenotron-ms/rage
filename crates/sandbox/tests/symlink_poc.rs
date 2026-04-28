//! Proof of concept: does rage's DYLD sandbox record symlink paths or resolved
//! paths when Node.js imports a package via pnpm's virtual store?
//!
//! Run with:
//!   cargo test -p sandbox --test symlink_poc -- --nocapture --include-ignored

use std::path::{Path, PathBuf};

fn dylib_path() -> PathBuf {
    // Use the debug build dylib directly.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // Go up to workspace root then into target/debug
    let ws_root = Path::new(manifest_dir)
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap();
    ws_root.join("target/debug/librage_sandbox.dylib")
}

fn node_path() -> String {
    std::process::Command::new("which")
        .arg("node")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "node".to_string())
}

/// Print and analyse all paths containing `term`, breaking them into
/// symlink-path vs resolved-path buckets.
fn analyze_paths(label: &str, term: &str, reads: &[PathBuf], writes: &[PathBuf]) {
    println!("\n=== {label} — paths containing '{term}' ===");

    let ms_reads: Vec<_> = reads
        .iter()
        .filter(|p| p.to_string_lossy().contains(term))
        .collect();

    if ms_reads.is_empty() {
        println!("  (none in reads — sandbox may not have observed node subprocess)");
    } else {
        for p in &ms_reads {
            let s = p.to_string_lossy();
            let has_version = s.contains('@');
            let has_pnpm = s.contains(".pnpm");
            println!("  READ  {s}  [version_in_path={has_version}] [pnpm_store={has_pnpm}]");
        }
    }

    for p in writes.iter().filter(|p| p.to_string_lossy().contains(term)) {
        println!("  WRITE {}", p.display());
    }

    // Categorise
    let version_paths = ms_reads
        .iter()
        .filter(|p| p.to_string_lossy().contains(&format!("{term}@")))
        .count();
    let symlink_paths = ms_reads
        .iter()
        .filter(|p| {
            let s = p.to_string_lossy();
            s.contains(&format!("node_modules/{term}")) && !s.contains(".pnpm")
        })
        .count();
    let resolved_paths = ms_reads
        .iter()
        .filter(|p| p.to_string_lossy().contains(".pnpm/ms@"))
        .count();

    println!();
    println!("  SUMMARY for '{label}':");
    println!(
        "    total reads containing '{term}':             {}",
        ms_reads.len()
    );
    println!("    version-bearing paths ('{term}@<ver>'):      {version_paths}");
    println!("    symlink-only paths  (node_modules/{term}):   {symlink_paths}");
    println!("    pnpm resolved paths (.pnpm/{term}@<ver>/..): {resolved_paths}");

    if resolved_paths > 0 {
        println!("    → VERDICT: RESOLVED PATH seen ✓  version info extractable from path");
    } else if symlink_paths > 0 {
        println!("    → VERDICT: SYMLINK PATH only ✗  version info NOT in path (needs lockfile)");
    } else if ms_reads.is_empty() {
        println!("    → VERDICT: NO PATHS CAPTURED  (DYLD interpose may not have reached node)");
    } else {
        println!("    → VERDICT: AMBIGUOUS — inspect paths above");
    }
}

// ─── pnpm test ──────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn pnpm_symlink_poc() {
    let dylib = dylib_path();
    assert!(
        dylib.exists(),
        "dylib not found at {}: run `cargo build -p sandbox-macos-dylib` first",
        dylib.display()
    );
    // Tell the macos runner to use our debug dylib.
    std::env::set_var("RAGE_SANDBOX_DYLIB", &dylib);

    let cwd = Path::new("/tmp/rage-symlink-poc/pnpm-test");
    assert!(
        cwd.join("node_modules").exists(),
        "pnpm node_modules not found — run `pnpm install` in {cwd:?} first"
    );

    let node = node_path();
    println!("\n[pnpm] node:  {node}");
    println!("[pnpm] cwd:   {}", cwd.display());
    println!("[pnpm] dylib: {}", dylib.display());

    // Show the symlink target
    let readlink = std::process::Command::new("readlink")
        .arg(cwd.join("node_modules/ms"))
        .output()
        .unwrap();
    println!(
        "[pnpm] node_modules/ms -> {}",
        String::from_utf8_lossy(&readlink.stdout).trim()
    );
    let realpath = std::process::Command::new("realpath")
        .arg(cwd.join("node_modules/ms"))
        .output()
        .unwrap();
    println!(
        "[pnpm] realpath(node_modules/ms) = {}",
        String::from_utf8_lossy(&realpath.stdout).trim()
    );

    // Run node through the sandbox — require('ms') forces full module resolution
    let cmd = format!(
        r#"{node} -e "const ms = require('ms'); console.log(ms('2 days')); console.log(require.resolve('ms'));""#
    );
    println!("\n[pnpm] cmd: {cmd}");

    let result = sandbox::run_sandboxed(&cmd, cwd, &[]).await.unwrap();
    println!("[pnpm] exit_code:    {}", result.exit_code);
    println!("[pnpm] total reads:  {}", result.path_set.reads.len());
    println!("[pnpm] total writes: {}", result.path_set.writes.len());

    // Show everything node_modules-related
    println!("\n[pnpm] ALL node_modules reads:");
    for p in result
        .path_set
        .reads
        .iter()
        .filter(|p| p.to_string_lossy().contains("node_modules"))
    {
        println!("  {}", p.display());
    }

    analyze_paths(
        "pnpm",
        "ms",
        &result.path_set.reads,
        &result.path_set.writes,
    );

    assert_eq!(result.exit_code, 0, "node should exit 0");
}

// ─── yarn (classic v1) test ─────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn yarn_symlink_poc() {
    let dylib = dylib_path();
    assert!(dylib.exists(), "dylib not found at {}", dylib.display());
    std::env::set_var("RAGE_SANDBOX_DYLIB", &dylib);

    let cwd = Path::new("/tmp/rage-symlink-poc/yarn-test");
    assert!(
        cwd.join("node_modules").exists(),
        "yarn node_modules not found — run `yarn install` first"
    );

    let node = node_path();
    println!("\n[yarn] node: {node}");

    // yarn classic uses flat hoisting — no symlinks, real directory
    let ls = std::process::Command::new("ls")
        .args(["-la", cwd.join("node_modules/ms").to_str().unwrap()])
        .output()
        .unwrap();
    println!(
        "[yarn] node_modules/ms: {}",
        String::from_utf8_lossy(&ls.stdout).trim()
    );

    let cmd = format!(
        r#"{node} -e "const ms = require('ms'); console.log(ms('2 days')); console.log(require.resolve('ms'));""#
    );
    println!("\n[yarn] cmd: {cmd}");

    let result = sandbox::run_sandboxed(&cmd, cwd, &[]).await.unwrap();
    println!("[yarn] exit_code:   {}", result.exit_code);
    println!("[yarn] total reads: {}", result.path_set.reads.len());

    println!("\n[yarn] ALL node_modules reads:");
    for p in result
        .path_set
        .reads
        .iter()
        .filter(|p| p.to_string_lossy().contains("node_modules"))
    {
        println!("  {}", p.display());
    }

    analyze_paths(
        "yarn",
        "ms",
        &result.path_set.reads,
        &result.path_set.writes,
    );

    assert_eq!(result.exit_code, 0, "node should exit 0");
}

// ─── npm test ────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn npm_symlink_poc() {
    let dylib = dylib_path();
    assert!(dylib.exists(), "dylib not found at {}", dylib.display());
    std::env::set_var("RAGE_SANDBOX_DYLIB", &dylib);

    let cwd = Path::new("/tmp/rage-symlink-poc/npm-test");
    assert!(
        cwd.join("node_modules").exists(),
        "npm node_modules not found — run `npm install` first"
    );

    let node = node_path();
    println!("\n[npm] node: {node}");

    let ls = std::process::Command::new("ls")
        .args(["-la", cwd.join("node_modules/ms").to_str().unwrap()])
        .output()
        .unwrap();
    println!(
        "[npm] node_modules/ms: {}",
        String::from_utf8_lossy(&ls.stdout).trim()
    );

    let cmd = format!(
        r#"{node} -e "const ms = require('ms'); console.log(ms('2 days')); console.log(require.resolve('ms'));""#
    );
    println!("\n[npm] cmd: {cmd}");

    let result = sandbox::run_sandboxed(&cmd, cwd, &[]).await.unwrap();
    println!("[npm] exit_code:   {}", result.exit_code);
    println!("[npm] total reads: {}", result.path_set.reads.len());

    println!("\n[npm] ALL node_modules reads:");
    for p in result
        .path_set
        .reads
        .iter()
        .filter(|p| p.to_string_lossy().contains("node_modules"))
    {
        println!("  {}", p.display());
    }

    analyze_paths("npm", "ms", &result.path_set.reads, &result.path_set.writes);

    assert_eq!(result.exit_code, 0, "node should exit 0");
}

// ─── workspace symlink test ──────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore]
async fn workspace_symlink_poc() {
    // Simulate what pnpm workspaces do:
    //   node_modules/@scope/my-lib  ->  ../../packages/my-lib  (workspace symlink)
    //
    // Does rage see the node_modules path or the packages/ source path?

    let dylib = dylib_path();
    assert!(dylib.exists(), "dylib not found at {}", dylib.display());
    std::env::set_var("RAGE_SANDBOX_DYLIB", &dylib);

    // Build a tiny workspace layout in /tmp
    let base = Path::new("/tmp/rage-symlink-poc/workspace-test");
    std::fs::create_dir_all(base.join("packages/my-lib")).unwrap();
    std::fs::create_dir_all(base.join("app/node_modules/@scope")).unwrap();

    std::fs::write(
        base.join("packages/my-lib/package.json"),
        r#"{"name":"@scope/my-lib","version":"1.0.0","main":"index.js"}"#,
    )
    .unwrap();
    std::fs::write(
        base.join("packages/my-lib/index.js"),
        r#"module.exports = { hello: () => "hello from my-lib" };"#,
    )
    .unwrap();

    // app/node_modules/@scope/my-lib -> ../../../packages/my-lib
    // Relative path from the symlink's parent dir (@scope/) to workspace-test/packages/my-lib:
    //   @scope/ -> .. -> node_modules/ -> .. -> app/ -> .. -> workspace-test/ -> packages/my-lib
    let link = base.join("app/node_modules/@scope/my-lib");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink("../../../packages/my-lib", &link).unwrap();

    std::fs::write(
        base.join("app/test.js"),
        r#"const lib = require('@scope/my-lib'); console.log(lib.hello());"#,
    )
    .unwrap();

    let node = node_path();
    let cwd = base.join("app");
    let cmd = format!("{node} test.js");

    println!("\n[workspace] cwd: {}", cwd.display());
    println!(
        "[workspace] symlink: {} -> ../../../../packages/my-lib",
        link.display()
    );
    let rp = std::process::Command::new("realpath")
        .arg(&link)
        .output()
        .unwrap();
    println!(
        "[workspace] realpath of link: {}",
        String::from_utf8_lossy(&rp.stdout).trim()
    );

    let result = sandbox::run_sandboxed(&cmd, &cwd, &[]).await.unwrap();
    println!("[workspace] exit_code:   {}", result.exit_code);
    println!("[workspace] total reads: {}", result.path_set.reads.len());

    println!("\n[workspace] ALL reads mentioning 'my-lib' OR 'packages':");
    for p in result.path_set.reads.iter().filter(|p| {
        let s = p.to_string_lossy();
        s.contains("my-lib") || (s.contains("packages") && s.contains("rage-symlink"))
    }) {
        println!("  {}", p.display());
    }

    println!("\n[workspace] ALL node_modules reads:");
    for p in result
        .path_set
        .reads
        .iter()
        .filter(|p| p.to_string_lossy().contains("node_modules"))
    {
        println!("  {}", p.display());
    }

    let nm_path_seen = result
        .path_set
        .reads
        .iter()
        .any(|p| p.to_string_lossy().contains("node_modules/@scope/my-lib"));
    let resolved_seen = result
        .path_set
        .reads
        .iter()
        .any(|p| p.to_string_lossy().contains("packages/my-lib"));

    println!("\n  WORKSPACE SUMMARY:");
    println!("    node_modules/@scope/my-lib seen (symlink path): {nm_path_seen}");
    println!("    packages/my-lib seen (resolved/source path):    {resolved_seen}");
    if resolved_seen && !nm_path_seen {
        println!(
            "    → VERDICT: sandbox follows symlinks fully (sees source, not node_modules path)"
        );
    } else if nm_path_seen && !resolved_seen {
        println!("    → VERDICT: sandbox records symlink path only (does NOT follow to source)");
    } else if nm_path_seen && resolved_seen {
        println!("    → VERDICT: sandbox records BOTH paths (symlink + resolved)");
    } else {
        println!("    → VERDICT: neither path seen — check if node ran under sandbox");
    }

    assert_eq!(result.exit_code, 0, "node should exit 0");
}
