//! Sandbox execution crate for rage.
//!
//! Runs a command in an OS-level sandbox and returns the set of file-system
//! paths that were accessed during execution.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use sandbox::run_sandboxed;
//!
//! let result = run_sandboxed("echo hello", std::path::Path::new("."), &[]).await?;
//! println!("exit_code={}", result.exit_code);
//! # Ok(())
//! # }
//! ```

pub mod event;
pub mod mock;

#[cfg(target_os = "macos")]
pub mod macos;

pub mod server;

/// Platform implementations — exactly one is active.
///
/// macOS   → DYLD_INSERT_LIBRARIES sandbox
/// Linux   → eBPF tracepoint sandbox
/// Other   → unsupported stub (returns error)
#[cfg(target_os = "macos")]
pub use macos::run_sandboxed;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::run_sandboxed;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub mod unsupported;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub use unsupported::run_sandboxed;

pub use event::{PathSet, RunResult};
