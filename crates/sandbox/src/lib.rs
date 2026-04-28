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

/// Wire protocol shared by all platforms that use a named pipe or Unix socket
/// to receive [`event::AccessEvent`]s from the sandboxed child process.
///
/// Round-trip tests (`pipe_proto::tests`) run on macOS CI.
pub mod pipe_proto;

pub mod mock;

/// Unix-domain socket server (macOS and Linux parent-side infrastructure).
///
/// Gated behind `#[cfg(unix)]` because `tokio::net::UnixListener` does not
/// exist on Windows.
#[cfg(unix)]
pub mod server;

/// Platform implementations — exactly one `run_sandboxed` is active.
///
/// macOS   → DYLD_INSERT_LIBRARIES sandbox
/// Linux   → eBPF tracepoint sandbox
/// Windows → Detours DLL injection sandbox
/// Other   → unsupported stub (returns an error)
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::run_sandboxed;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::run_sandboxed;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub use windows::run_sandboxed;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub mod unsupported;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use unsupported::run_sandboxed;

pub use event::{PathSet, RunResult};
