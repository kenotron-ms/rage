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
}
