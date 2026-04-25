//! macOS interposition dylib loaded by the `sandbox` crate.
//!
//! When injected via `DYLD_INSERT_LIBRARIES`, this library reads the
//! `RAGE_SANDBOX_SOCKET` environment variable, connects to the sandbox
//! supervisor over a Unix-domain socket, and registers Mach-O interpose
//! entries so that file-system calls are reported back to the supervisor.
//!
//! ## Interposed entrypoints (current)
//! - `open`, `openat` — read or write depending on flags
//! - `stat`, `lstat` — always read
//! - `rename`, `unlink`, `mkdir` — always write
//!
//! ## Not interposed (deliberate)
//! - `read`/`write` syscalls — they operate on fds, not paths. The path was
//!   already recorded at `open` time, so re-recording here adds no signal.
//! - `dup`/`fcntl` — same reason.

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types)]

mod client;
mod interpose;
pub use interpose::*;

#[ctor::ctor]
fn rage_sandbox_init() {
    client::init_from_env();
}
