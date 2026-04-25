//! Content-addressed cache for rage build tasks.
//!
//! Two cache implementations live here:
//!   * `LocalCache` — single-phase blake3, kept for tests / fallback.
//!   * `TwoPhaseCache` — production: WF → pathset → SF (per design doc §5).

pub mod entry;
pub mod fingerprint; // legacy single-phase, kept for back-compat
pub mod local;
pub mod pathset_store;
pub mod provider;
pub mod strong_fp;
pub mod tool_hash;
pub mod two_phase;
pub mod weak_fp;
pub mod why_miss;

pub use entry::CacheEntry;
pub use fingerprint::fingerprint_task;
pub use local::LocalCache;
pub use pathset_store::PathsetStore;
pub use provider::CacheProvider;
pub use strong_fp::compute_strong_fingerprint;
pub use two_phase::TwoPhaseCache;
pub use weak_fp::{compute_weak_fingerprint, WeakFpInputs};
