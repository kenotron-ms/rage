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
//! task (see `cache::task_stats`).  Tasks with **no history** (first-ever
//! run, or new WF fingerprint) are admitted through a dedicated cold-start
//! semaphore sized at `max(1, cpu_count / 2)`.  This prevents OOM on first
//! builds while still allowing meaningful parallelism.  Tasks with history
//! use their measured p75 peak-RSS as the estimate against the memory budget.
//! After the first build every task has real data, so the memory gate is
//! fully grounded in measured reality from the second build onward.
//!
//! ## Starvation prevention
//!
//! A single task with a **known** estimate is always allowed through even if
//! its estimate exceeds the remaining budget, provided nothing else is
//! currently running.  Without this, a very large task would starve forever
//! on a small machine.
//!
//! ## Live feedback loop (future)
//!
//! The `MemoryBudget::committed()` value is updated with *actual* peak RSS
//! once each task exits (via `MemoryGuard::release_with_actual`).  Future
//! work: a background thread that polls system `available_memory()` and
//! shrinks the budget dynamically when the OS is under pressure — mirroring
//! BuildXL's live resource monitor.

use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

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
    /// Semaphore that gates tasks with no RSS history (cold starts).
    /// Sized at `max(1, cpu_count / 2)` to prevent OOM on first builds.
    cold_sem: Arc<Semaphore>,
}

impl MemoryBudget {
    /// Build a budget from live system memory stats.
    ///
    /// `capacity` = `available_memory × 0.75` — 25% headroom for OS and
    /// non-build processes.  This is intentionally *available* not *total*,
    /// so the budget naturally shrinks on busy machines.
    /// Build a budget from live system memory stats.
    ///
    /// `cold_concurrency`: maximum concurrent tasks with no RSS history.
    /// `None` uses the memory-based default: `min(cpu_count, available_gb / 2)`,
    /// which gives full-core utilisation on memory-rich machines and a safe cap
    /// on constrained ones.  Override with `rage.json` `coldConcurrency`.
    pub fn from_system(cold_concurrency: Option<usize>) -> Self {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();

        let total = sys.total_memory();
        let available = sys.available_memory().max(1);
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        // 75 % of currently-available memory
        let capacity = available * 3 / 4;
        // Cold-start slot count: configurable, defaulting to
        // min(cpu_count, available_gb / 2).  This saturates all cores on
        // memory-rich machines (e.g. 64 GB → 30 available → 15 slots, capped
        // to cpu_count) while protecting constrained ones (16 GB → 10 GB
        // available → 5 slots).
        let cold_permits = cold_concurrency.unwrap_or_else(|| {
            let available_gb = available / (1024 * 1024 * 1024);
            let by_memory = ((available_gb / 2).max(1)) as usize;
            cpu_count.min(by_memory)
        });

        eprintln!(
            "[rage] resource budget: {:.1} GB capacity ({:.1} GB available), \
             {} cold-start slots",
            capacity as f64 / 1e9,
            available as f64 / 1e9,
            cold_permits,
        );

        Self {
            state: Arc::new(Mutex::new(BudgetState {
                capacity_bytes: capacity,
                committed_bytes: 0,
                active_count: 0,
            })),
            notify: Arc::new(Notify::new()),
            total_bytes: total,
            cold_sem: Arc::new(Semaphore::new(cold_permits)),
        }
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
                        cold_permit: None,
                    };
                }
            }
            // Wait until another task finishes and releases budget.
            self.notify.notified().await;
        }
    }

    /// Acquire a cold-start slot for a task with no RSS history.
    ///
    /// Blocks until one of the `max(1, cpu_count/2)` cold-start semaphore
    /// permits is available.  The permit is released when the returned
    /// [`MemoryGuard`] is dropped, bounding concurrent cold-start tasks to
    /// a safe fraction of available cores.
    pub async fn reserve_cold(&self) -> MemoryGuard {
        let permit = Arc::clone(&self.cold_sem)
            .acquire_owned()
            .await
            .expect("cold_sem is never closed");
        MemoryGuard {
            inner: None,
            cold_permit: Some(permit),
        }
    }
}

/// RAII guard returned by both [`MemoryBudget::reserve`] and
/// [`MemoryBudget::reserve_cold`].
///
/// Releases the memory reservation (or cold-start semaphore permit) on drop.
/// Call [`MemoryGuard::release_with_actual`] to record the actual peak RSS
/// before dropping; the caller is responsible for persisting it via
/// `cache::task_stats::save`.
pub struct MemoryGuard {
    /// Set by `reserve()`.  `None` after the guard has been released.
    inner: Option<(Arc<Mutex<BudgetState>>, Arc<Notify>, u64)>,
    /// Set by `reserve_cold()`.  Released automatically on drop.
    cold_permit: Option<OwnedSemaphorePermit>,
}

fn release_budget(state: &Mutex<BudgetState>, notify: &Notify, bytes: u64) {
    let mut s = state.lock().unwrap();
    s.committed_bytes = s.committed_bytes.saturating_sub(bytes);
    s.active_count = s.active_count.saturating_sub(1);
    drop(s);
    notify.notify_waiters();
}

impl MemoryGuard {
    /// Release the reservation.
    ///
    /// `actual_peak_bytes` is the measured peak RSS of the subprocess.  It
    /// is accepted here so the call site reads clearly, but the value is
    /// **not** used internally — the caller is responsible for persisting it
    /// via `cache::task_stats::save` before calling this method.  The full
    /// reserved amount is always freed from `committed_bytes`; what was
    /// reserved was the *estimate*, not the actual usage.
    ///
    /// For cold-start guards (`reserve_cold`), the semaphore permit is
    /// released on drop regardless.
    pub fn release_with_actual(mut self, _actual_peak_bytes: u64) {
        if let Some((state, notify, reserved)) = self.inner.take() {
            release_budget(&state, &notify, reserved);
        }
        // cold_permit released on drop of self.
    }
}

impl Drop for MemoryGuard {
    fn drop(&mut self) {
        if let Some((state, notify, reserved)) = self.inner.take() {
            release_budget(&state, &notify, reserved);
        }
        // Explicitly drop the cold-start permit so the intent is clear.
        // The OwnedSemaphorePermit releases its slot back to the semaphore here.
        drop(self.cold_permit.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_budget(capacity_mb: u64, total_mb: u64) -> MemoryBudget {
        let capacity = capacity_mb * 1_048_576;
        let total = total_mb * 1_048_576;
        MemoryBudget {
            state: Arc::new(Mutex::new(BudgetState {
                capacity_bytes: capacity,
                committed_bytes: 0,
                active_count: 0,
            })),
            notify: Arc::new(Notify::new()),
            total_bytes: total,
            cold_sem: Arc::new(Semaphore::new(2)), // 2 cold-start slots for tests
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
    async fn release_with_actual_frees_full_reserved_amount() {
        // actual_peak_bytes is accepted but unused internally; the full
        // reserved amount is always freed.  Caller records actual via task_stats.
        let b = make_budget(1024, 2048);
        let guard = b.reserve(512 * 1_048_576).await;
        assert_eq!(b.committed_bytes(), 512 * 1_048_576);
        guard.release_with_actual(256 * 1_048_576); // actual ignored; 512 MB freed
        assert_eq!(b.committed_bytes(), 0);
    }

    #[tokio::test]
    async fn reserve_cold_limits_concurrency_to_semaphore_permits() {
        // make_budget gives 2 cold-start permits.
        let b = make_budget(1024, 2048);
        let g1 = b.reserve_cold().await;
        let g2 = b.reserve_cold().await;
        // Cold guards do not affect the memory budget.
        assert_eq!(b.committed_bytes(), 0);
        // A third cold acquire should block (no permits left).
        let b_clone = b.clone();
        let timeout_result =
            tokio::time::timeout(std::time::Duration::from_millis(20), b_clone.reserve_cold())
                .await;
        assert!(
            timeout_result.is_err(),
            "third cold acquire should have blocked"
        );
        // Dropping a guard releases a permit; the next acquire succeeds.
        drop(g1);
        let g3 = tokio::time::timeout(std::time::Duration::from_millis(100), b.reserve_cold())
            .await
            .expect("cold acquire should succeed after g1 dropped");
        drop(g2);
        drop(g3);
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
