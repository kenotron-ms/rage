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
/// macOS              → DYLD_INSERT_LIBRARIES sandbox
/// Linux + ebpf feat  → eBPF tracepoint sandbox (requires nightly + bpf-linker)
/// Linux - ebpf feat  → unsupported stub (returns an error; safe for CI / loose mode)
/// Windows            → Detours DLL injection sandbox
/// Other              → unsupported stub (returns an error)
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::run_sandboxed;

// Linux eBPF sandbox: only compiled when the `ebpf` feature is enabled.
// Without the feature the `unsupported` stub is used instead, which means
// CI builds and loose-mode runs work without nightly Rust or bpf-linker.
#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub mod linux;

#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub use linux::run_sandboxed;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub use windows::run_sandboxed;

// Use the unsupported stub on:
//   • Linux without the `ebpf` feature (CI builds, loose-mode)
//   • Any other OS that is not macOS, Linux, or Windows
#[cfg(any(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    all(target_os = "linux", not(feature = "ebpf"))
))]
pub mod unsupported;

#[cfg(any(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    all(target_os = "linux", not(feature = "ebpf"))
))]
pub use unsupported::run_sandboxed;

pub use event::{PathSet, RunResult};
