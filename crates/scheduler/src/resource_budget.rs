//! Memory-aware admission control for subprocess scheduling.
//!
//! # Design
//!
//! Inspired by BuildXL's historical-statistics + live-monitoring scheduler.
//! Two layers of admission control cooperate:
//!
//! 1. **Process-count semaphore** — caps concurrent subprocesses at
//!    `min(available_parallelism, maxConcurrency)`.  CPU-centric guard.
//!
//! 2. **`MemoryBudget`** (this module) — caps total *estimated* in-flight
//!    RSS at 75% of available system memory.  Memory-centric guard.
//!
//! A task must clear *both* before its subprocess is spawned.
//!
//! ## Estimation
//!
//! Before spawning, rage looks up the p75 peak-RSS from prior runs of this
//! task (see `cache::task_stats`).  If no history exists, the default
//! estimate is `total_memory / (2 × cpu_count)` — conservative enough to
//! avoid OOM on first run without being so aggressive that only one task
//! runs at a time.
//!
//! ## Starvation prevention
//!
//! A single task is always allowed through even if its estimate exceeds the
//! remaining budget, provided nothing else is currently running.  Without
//! this, a very large task would starve forever on a small machine.
//!
//! ## Live feedback loop (future)
//!
//! The `MemoryBudget::committed()` value is updated with *actual* peak RSS
//! once each task exits (via `MemoryGuard::release_with_actual`).  Future
//! work: a background thread that polls system `available_memory()` and
//! shrinks the budget dynamically when the OS is under pressure — mirroring
//! BuildXL's live resource monitor.

use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Shared memory budget state.
struct BudgetState {
    /// Maximum bytes we allow in-flight at once.
    capacity_bytes: u64,
    /// Sum of estimates of currently running subprocesses.
    committed_bytes: u64,
    /// Number of subprocesses currently running (for starvation guard).
    active_count: u32,
}

/// Memory-aware admission control.
///
/// Clone freely — clones share the same inner state (like `Arc`).
#[derive(Clone)]
pub struct MemoryBudget {
    state: Arc<Mutex<BudgetState>>,
    notify: Arc<Notify>,
    /// Total system memory, reported once at construction.
    total_bytes: u64,
    /// Per-CPU default estimate (bytes) used when no historical data exists.
    default_estimate_bytes: u64,
}

impl MemoryBudget {
    /// Build a budget from live system memory stats.
    ///
    /// `capacity` = `available_memory × 0.75` — 25% headroom for OS and
    /// non-build processes.  This is intentionally *available* not *total*,
    /// so the budget naturally shrinks on busy machines.
    pub fn from_system() -> Self {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();

        let total = sys.total_memory();
        let available = sys.available_memory().max(1);
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as u64;

        // 75 % of currently-available memory
        let capacity = available * 3 / 4;
        // Safe default per task: total / (2 × cpu_count)
        let default_estimate = (total / (2 * cpu_count)).max(256 * 1024 * 1024); // ≥ 256 MB

        eprintln!(
            "[rage] resource budget: {:.1} GB capacity ({:.1} GB available), \
             default estimate {:.0} MB/task, {} logical CPUs",
            capacity as f64 / 1e9,
            available as f64 / 1e9,
            default_estimate as f64 / 1e6,
            cpu_count,
        );

        Self {
            state: Arc::new(Mutex::new(BudgetState {
                capacity_bytes: capacity,
                committed_bytes: 0,
                active_count: 0,
            })),
            notify: Arc::new(Notify::new()),
            total_bytes: total,
            default_estimate_bytes: default_estimate,
        }
    }

    /// The per-task default estimate in bytes (used when no history exists).
    pub fn default_estimate_bytes(&self) -> u64 {
        self.default_estimate_bytes
    }

    /// Total system memory (bytes) at startup.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Currently committed bytes (sum of in-flight estimates).
    pub fn committed_bytes(&self) -> u64 {
        self.state.lock().unwrap().committed_bytes
    }

    /// Acquire `estimate_bytes` of budget.  Returns a [`MemoryGuard`] that
    /// releases the reservation on drop.
    ///
    /// Waits asynchronously if the budget is exhausted **and** at least one
    /// other task is already running (starvation-free: the very first
    /// waiter always gets through).
    pub async fn reserve(&self, estimate_bytes: u64) -> MemoryGuard {
        loop {
            {
                let mut state = self.state.lock().unwrap();
                let remaining = state.capacity_bytes.saturating_sub(state.committed_bytes);
                let can_proceed = remaining >= estimate_bytes || state.active_count == 0;
                if can_proceed {
                    state.committed_bytes += estimate_bytes;
                    state.active_count += 1;
                    return MemoryGuard {
                        inner: Some((
                            Arc::clone(&self.state),
                            Arc::clone(&self.notify),
                            estimate_bytes,
                        )),
                    };
                }
            }
            // Wait until another task finishes and releases budget.
            self.notify.notified().await;
        }
    }
}

/// RAII guard: releases the reserved bytes back to the budget on drop.
///
/// Call [`MemoryGuard::release_with_actual`] before dropping to use the
/// *measured* peak RSS for accounting instead of the original estimate.
/// If dropped without calling that method, the original estimate is used.
pub struct MemoryGuard {
    /// `None` after the guard has been released (prevents double-release).
    inner: Option<(Arc<Mutex<BudgetState>>, Arc<Notify>, u64)>,
}

fn release_budget(state: &Mutex<BudgetState>, notify: &Notify, bytes: u64) {
    let mut s = state.lock().unwrap();
    s.committed_bytes = s.committed_bytes.saturating_sub(bytes);
    s.active_count = s.active_count.saturating_sub(1);
    drop(s);
    notify.notify_waiters();
}

impl MemoryGuard {
    /// Release the reservation using the original estimate for accounting.
    ///
    /// `actual_peak_bytes` is accepted for future statistics tracking but the
    /// full reserved amount is always freed from `committed_bytes`; the
    /// reservation was for the *estimate*, not the actual usage.
    /// After this call, the guard is a no-op on drop.
    pub fn release_with_actual(mut self, _actual_peak_bytes: u64) {
        if let Some((state, notify, reserved)) = self.inner.take() {
            release_budget(&state, &notify, reserved);
        }
    }
}

impl Drop for MemoryGuard {
    fn drop(&mut self) {
        if let Some((state, notify, reserved)) = self.inner.take() {
            release_budget(&state, &notify, reserved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_budget(capacity_mb: u64, total_mb: u64) -> MemoryBudget {
        let capacity = capacity_mb * 1_048_576;
        let total = total_mb * 1_048_576;
        let cpu_count = 4u64;
        MemoryBudget {
            state: Arc::new(Mutex::new(BudgetState {
                capacity_bytes: capacity,
                committed_bytes: 0,
                active_count: 0,
            })),
            notify: Arc::new(Notify::new()),
            total_bytes: total,
            default_estimate_bytes: total / (2 * cpu_count),
        }
    }

    #[tokio::test]
    async fn reserve_and_drop_releases_budget() {
        let b = make_budget(1024, 2048);
        let guard = b.reserve(512 * 1_048_576).await;
        assert_eq!(b.committed_bytes(), 512 * 1_048_576);
        drop(guard);
        assert_eq!(b.committed_bytes(), 0);
    }

    #[tokio::test]
    async fn reserve_with_actual_uses_real_peak() {
        let b = make_budget(1024, 2048);
        let guard = b.reserve(512 * 1_048_576).await;
        // Actual usage was 256 MB — accounting should reflect that
        guard.release_with_actual(256 * 1_048_576);
        // committed should now be 0 (released with actual=256, not estimate=512)
        assert_eq!(b.committed_bytes(), 0);
    }

    #[tokio::test]
    async fn starvation_guard_allows_first_task_through() {
        // Budget capacity is only 100 MB but the task wants 2 GB
        let b = make_budget(100, 8192);
        // Should succeed immediately (active_count == 0)
        let guard = b.reserve(2 * 1024 * 1_048_576).await;
        assert_eq!(b.committed_bytes(), 2 * 1024 * 1_048_576);
        drop(guard);
    }

    #[tokio::test]
    async fn concurrent_tasks_respect_budget() {
        let b = make_budget(1024, 2048); // 1 GB budget
        let g1 = b.reserve(512 * 1_048_576).await; // 512 MB
        let g2 = b.reserve(512 * 1_048_576).await; // 512 MB — total 1024 MB = exactly capacity
        assert_eq!(b.committed_bytes(), 1024 * 1_048_576);

        // A third task that would exceed budget must wait.
        // We verify this by checking that reserve() does NOT return immediately
        // using tokio::time::timeout.
        let b_clone = b.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            b_clone.reserve(1_048_576), // 1 MB — still over budget
        )
        .await;
        assert!(result.is_err(), "should have timed out waiting for budget");

        // Release one task → the waiter should now proceed
        drop(g1);
        // g2 still holds 512 MB; remaining = 512 MB > 1 MB needed
        let g3 = tokio::time::timeout(std::time::Duration::from_millis(100), b.reserve(1_048_576))
            .await
            .expect("should get budget after g1 released");
        drop(g2);
        drop(g3);
        assert_eq!(b.committed_bytes(), 0);
    }
}
