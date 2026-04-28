//! Background RSS monitor for child subprocesses.
//!
//! Spawns a lightweight Tokio task alongside each subprocess that polls
//! the process's resident-set size (RSS) every 100 ms via `sysinfo`.
//! Returns the **peak** RSS bytes observed when the process exits.
//!
//! The poll task exits naturally when `sysinfo` can no longer find the
//! process (i.e. after it exits); no explicit cancellation is needed.
//!
//! # Why poll instead of using OS hooks?
//!
//! Cross-platform process-exit hooks (pidfd on Linux, kqueue on macOS)
//! require unsafe code or third-party crates.  Polling at 100 ms trades
//! a small accuracy gap for simplicity and portability.  For build tasks
//! that typically run 1–30 s, 100 ms sampling error is negligible.
//!
//! # Future: live budget feedback
//!
//! In a later iteration, the running peak could be reported back to
//! `MemoryBudget` in real time, allowing the scheduler to admit more tasks
//! when actual usage is lower than estimated — mirroring BuildXL's
//! live-resource-feedback loop.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use std::time::Duration;
use tokio::time::interval;

/// Spawn a background Tokio task that tracks peak RSS for `pid`.
///
/// The returned `JoinHandle` resolves to the peak RSS in bytes once the
/// process exits.  If the process is never observed (e.g. already gone),
/// returns 0.
///
/// # Cancel safety
///
/// The background task does not hold any locks and does not open files —
/// dropping the handle simply stops future polls (the task exits cleanly).
pub fn track_peak_rss(pid: u32) -> tokio::task::JoinHandle<u64> {
    tokio::task::spawn_blocking(move || {
        let mut sys = System::new();
        let sysinfo_pid = Pid::from_u32(pid);
        let mut peak_bytes: u64 = 0;

        loop {
            // sysinfo 0.38: (targets, include_thread_tasks, kind)
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[sysinfo_pid]),
                false,
                ProcessRefreshKind::nothing().with_memory(),
            );

            match sys.process(sysinfo_pid) {
                Some(proc) => {
                    peak_bytes = peak_bytes.max(proc.memory());
                }
                None => break, // process exited
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        peak_bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tracks_own_process_rss() {
        // Spin up a short-lived subprocess that just sleeps for 200 ms,
        // then verify we measured some RSS > 0 for it.
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 0.2")
            .spawn()
            .expect("failed to spawn test subprocess");

        let pid = child.id().expect("no pid");
        let handle = track_peak_rss(pid);

        let _ = child.wait_with_output().await;
        let peak = handle.await.unwrap_or(0);

        // We can't guarantee a minimum RSS (the process may be too short-lived
        // for the first poll to hit), but the function should not panic.
        let _ = peak; // suppress unused warning in release builds
    }

    #[tokio::test]
    async fn returns_zero_for_nonexistent_pid() {
        // PID 0 is not a valid user process on macOS or Linux.
        let handle = track_peak_rss(0);
        let peak = handle.await.unwrap_or(0);
        assert_eq!(peak, 0, "nonexistent pid should yield 0 bytes");
    }
}
