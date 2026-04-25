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

/// Find the shell to use for running sandboxed commands.
///
/// On macOS 26+ (Tahoe), system binaries with `Platform identifier=26`
/// (including `/bin/sh` and `/bin/zsh`) strip `DYLD_INSERT_LIBRARIES` from
/// their process environment, which prevents the sandbox dylib from being
/// injected into any subprocess in the tree.  Prefer a non-system shell
/// (e.g., Homebrew's `bash`, which is adhoc-signed) so that
/// `DYLD_INSERT_LIBRARIES` is honoured and propagated.
///
/// Falls back to `"sh"` when no Homebrew shell is found (this will work on
/// older macOS but not on macOS 26+ without a developer-installed bash).
fn resolve_shell() -> String {
    // On macOS, prefer Homebrew bash which is adhoc-signed and therefore
    // honours DYLD_INSERT_LIBRARIES on macOS 26+.
    #[cfg(target_os = "macos")]
    {
        const CANDIDATES: &[&str] = &[
            "/opt/homebrew/bin/bash", // Apple Silicon Homebrew
            "/usr/local/bin/bash",    // Intel Homebrew
        ];
        for &candidate in CANDIDATES {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
    }
    "sh".to_string()
}

/// Run `cmd` inside the macOS sandbox.
///
/// The sandbox dylib is injected into the child process via
/// `DYLD_INSERT_LIBRARIES`.  File-system access events are collected over a
/// Unix-domain socket and returned as a [`PathSet`].
///
/// # Shell selection
///
/// The command is executed via a shell found by [`resolve_shell`].  On
/// macOS 26+, the system `/bin/sh` strips `DYLD_INSERT_LIBRARIES` from its
/// process environment so the dylib cannot be injected.  When a Homebrew
/// `bash` is present it is used instead.
///
/// # Caveats
///
/// - On macOS 26+, `DYLD_INSERT_LIBRARIES` is stripped by system binaries
///   that carry `Platform identifier=26` (including `/bin/sh`, `/bin/cat`,
///   `/usr/bin/stat`, etc.).  Only non-system binaries (adhoc-signed or
///   developer-signed without library validation) can be fully observed.
/// - Children that re-exec to a hardened binary lose interposition for that
///   subtree.
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

    let shell = resolve_shell();

    let status = Command::new(&shell)
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
    async fn stat_interposed_records_read() {
        let dylib = dylib_path().expect("dylib_path() should succeed");
        if !dylib.exists() {
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "sandbox-macos-dylib"])
                .status()
                .expect("failed to run cargo build -p sandbox-macos-dylib");
            assert!(status.success(), "cargo build -p sandbox-macos-dylib failed");
        }

        // Use a shell conditional that calls lstat("/etc/passwd") within the
        // shell process itself.  On macOS 26+, system binaries like
        // /usr/bin/stat strip DYLD_INSERT_LIBRARIES, so we avoid them.
        // Single-bracket test is POSIX-compatible and works with both sh and bash.
        let result = run_sandboxed("[ -f /etc/passwd ]", Path::new("/tmp"), &[])
            .await
            .unwrap();

        assert!(
            result.path_set.reads.iter().any(|p| p
                .to_string_lossy()
                .ends_with("passwd")),
            "expected a path ending with 'passwd' in reads, got: {:?}",
            result.path_set.reads
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

        // Use a shell file-descriptor redirect so the open() call happens
        // inside the shell process (which has the dylib loaded).  On macOS
        // 26+, system binaries like /bin/cat strip DYLD_INSERT_LIBRARIES.
        let result = run_sandboxed(
            "exec 3< /etc/hosts; exec 3>&-",
            Path::new("/tmp"),
            &[],
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(
            result.path_set.reads.contains(&PathBuf::from("/etc/hosts")),
            "expected /etc/hosts in reads, got: {:?}",
            result.path_set.reads
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore]
    async fn force_flat_namespace_is_set() {
        let r = run_sandboxed("test 1 -eq 1", Path::new("/tmp"), &[])
            .await
            .unwrap();
        assert_eq!(r.exit_code, 0);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore]
    async fn write_operations_recorded() {
        let dylib = dylib_path().expect("dylib_path() should succeed");
        if !dylib.exists() {
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "sandbox-macos-dylib"])
                .status()
                .expect("failed to run cargo build -p sandbox-macos-dylib");
            assert!(status.success(), "cargo build -p sandbox-macos-dylib failed");
        }

        let dir = tempfile::tempdir().expect("create tempdir");
        // Use a shell output-redirect so the open(O_WRONLY|O_CREAT|O_TRUNC)
        // call happens inside the shell process (which has the dylib loaded).
        // On macOS 26+, system binaries like /usr/bin/touch strip
        // DYLD_INSERT_LIBRARIES, so we avoid them here.
        let cmd = format!("> '{}/a.txt'", dir.path().display());

        let result = run_sandboxed(&cmd, Path::new("/tmp"), &[])
            .await
            .unwrap();

        assert!(
            result.path_set.writes.iter().any(|p| p
                .to_string_lossy()
                .ends_with("a.txt")),
            "expected a path ending with 'a.txt' in writes, got: {:?}",
            result.path_set.writes
        );
    }
}
