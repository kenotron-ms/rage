//! Linux eBPF sandbox implementation for rage.
//!
//! This crate provides a `run_sandboxed` function that intercepts file-system
//! syscalls from a spawned task process (and all its children) using eBPF
//! tracepoints, and returns the resulting `RunResult` with a `PathSet`.
//!
//! # Availability
//!
//! The eBPF implementation is Linux-only. On non-Linux platforms the function
//! returns an error immediately, so callers can fall back gracefully.

use anyhow::Result;
use std::path::Path;

/// Platform-independent run result.
#[derive(Debug)]
pub struct EbpfRunResult {
    pub exit_code: i32,
    pub reads: Vec<std::path::PathBuf>,
    pub writes: Vec<std::path::PathBuf>,
}

// ─── Platform implementations ─────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux;

// ─── Public API ─────────────────────────────────────────────────────────────

/// Run `cmd` inside an eBPF sandbox, collecting all file-system accesses.
///
/// On Linux: loads the eBPF program, attaches tracepoints, spawns the
/// process, collects events, and returns the observed PathSet.
///
/// On non-Linux: returns `Err` — use the macOS DYLD sandbox on macOS.
pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<EbpfRunResult> {
    #[cfg(target_os = "linux")]
    {
        linux::run_sandboxed_linux(cmd, cwd, env).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cmd, cwd, env);
        anyhow::bail!("eBPF sandbox is only supported on Linux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn non_linux_returns_error() {
        let result = run_sandboxed("echo hello", std::path::Path::new("/tmp"), &[]).await;
        assert!(result.is_err(), "expected error on non-Linux");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Linux"), "error should mention Linux: {err}");
    }

    // ── Linux eBPF integration tests ───────────────────────────────────────────
    // These tests only compile on Linux and require CAP_SYS_ADMIN (privileged).
    // Run them via: docker compose -f docker/docker-compose.sandbox.yml run sandbox-test
    // On the host they are compiled out by #[cfg(target_os = "linux")] and skipped.

    /// Verify the eBPF sandbox can run a simple command and observe file reads.
    ///
    /// `cat /etc/hostname` is a reliable way to produce an openat() syscall for
    /// a real file path.  The PathSet `reads` must be non-empty after the run.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires Linux kernel + eBPF privileges; run via docker/docker-compose.sandbox.yml"]
    async fn linux_sandbox_produces_nonempty_pathset_for_cat() {
        let result = run_sandboxed("cat /etc/hostname", std::path::Path::new("/tmp"), &[])
            .await
            .expect("sandboxed run must succeed on Linux with CAP_SYS_ADMIN");

        assert_eq!(result.exit_code, 0, "cat /etc/hostname must exit 0");
        assert!(
            !result.reads.is_empty(),
            "eBPF sandbox must record at least one file read for `cat /etc/hostname`;              got an empty PathSet — check that the eBPF program loaded and the ring buffer drained"
        );

        // The hostname file itself must appear in the read set.
        let has_hostname = result
            .reads
            .iter()
            .any(|p| p.to_string_lossy().contains("hostname"));
        assert!(
            has_hostname,
            "reads must include /etc/hostname; got: {:?}",
            result.reads
        );
    }

    /// Verify that write-flag opens (O_CREAT | O_WRONLY) are classified as writes.
    ///
    /// Creates a temp file via `touch`; the openat() with O_CREAT must appear
    /// in the PathSet `writes` set, not reads.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires Linux kernel + eBPF privileges; run via docker/docker-compose.sandbox.yml"]
    async fn linux_sandbox_classifies_write_flag_opens_as_writes() {
        use tempfile::tempdir;
        let dir = tempdir().expect("tempdir must succeed");
        let target = dir.path().join("sandbox-write-test.txt");

        let cmd = format!("touch '{}'", target.display());
        let result = run_sandboxed(&cmd, dir.path(), &[])
            .await
            .expect("sandboxed touch must succeed");

        assert_eq!(result.exit_code, 0, "touch must exit 0");
        assert!(
            !result.writes.is_empty(),
            "eBPF sandbox must classify O_CREAT opens as writes; writes = {:?}",
            result.writes
        );
    }
}
