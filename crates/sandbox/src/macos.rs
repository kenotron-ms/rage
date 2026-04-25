//! macOS-specific sandbox runner.
//!
//! The dylib (`librage_sandbox.dylib`) is produced by the `sandbox-macos-dylib`
//! workspace crate, which is a *sibling* crate — NOT a direct Cargo dependency
//! of this crate.  We must not link against it.  Instead, `build.rs` computes
//! the expected artifact path and bakes it into the binary as
//! `RAGE_SANDBOX_DYLIB_DEFAULT`.  Callers may override this at runtime by
//! setting `RAGE_SANDBOX_DYLIB`.

use crate::event::{PathSet, RunResult};
use crate::server::EventServer;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Return the path to `librage_sandbox.dylib`.
///
/// Resolution order:
/// 1. `RAGE_SANDBOX_DYLIB` environment variable (runtime override).
/// 2. Path baked in at compile time by `build.rs` (`RAGE_SANDBOX_DYLIB_DEFAULT`).
pub fn dylib_path() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("RAGE_SANDBOX_DYLIB") {
        return Ok(PathBuf::from(val));
    }
    Ok(PathBuf::from(env!("RAGE_SANDBOX_DYLIB_DEFAULT")))
}

/// Run `cmd` inside the macOS sandbox.
///
/// The sandbox dylib is injected into the child process via
/// `DYLD_INSERT_LIBRARIES`.  File-system access events are collected over a
/// Unix-domain socket and returned as a [`PathSet`].
pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<RunResult> {
    let dylib = dylib_path()?;
    if !dylib.exists() {
        anyhow::bail!(
            "sandbox dylib not found at `{}`: \
             run `cargo build -p sandbox-macos-dylib` first",
            dylib.display()
        );
    }

    // Create a temp directory to hold the Unix-domain socket.
    let tmp = tempfile::tempdir().context("create tempdir for socket")?;
    let server = EventServer::start(tmp.path()).context("start EventServer")?;
    let socket_path = server.socket_path.clone();

    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("DYLD_INSERT_LIBRARIES", &dylib)
        .env("DYLD_FORCE_FLAT_NAMESPACE", "1")
        .env("RAGE_SANDBOX_SOCKET", &socket_path)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .status()
        .await
        .context("spawn child process")?;

    let exit_code = status.code().unwrap_or(-1);

    // Give the dylib a moment to flush any pending events before we drain.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = server.drain().await;
    let path_set = PathSet::from_events(&events);

    Ok(RunResult { exit_code, path_set })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_path_returns_a_path() {
        let path = dylib_path().expect("dylib_path() should succeed");
        assert!(
            path.to_string_lossy().contains("librage_sandbox.dylib"),
            "expected path to contain librage_sandbox.dylib, got: {}",
            path.display()
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore]
    async fn open_interposed_records_read() {
        let dylib = dylib_path().expect("dylib_path() should succeed");
        if !dylib.exists() {
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "sandbox-macos-dylib"])
                .status()
                .expect("failed to run cargo build -p sandbox-macos-dylib");
            assert!(status.success(), "cargo build -p sandbox-macos-dylib failed");
        }

        let result = run_sandboxed("cat /etc/hosts > /dev/null", Path::new("/tmp"), &[])
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(
            result.path_set.reads.contains(&PathBuf::from("/etc/hosts")),
            "expected /etc/hosts in reads, got: {:?}",
            result.path_set.reads
        );
    }
}
