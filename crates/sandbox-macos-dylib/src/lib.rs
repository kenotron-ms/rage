//! macOS interposition dylib loaded by the `sandbox` crate.
//!
//! When injected via `DYLD_INSERT_LIBRARIES`, this library reads the
//! `RAGE_SANDBOX_SOCKET` environment variable, connects to the sandbox
//! supervisor over a Unix-domain socket, and registers Mach-O interpose
//! entries so that file-system calls are reported back to the supervisor.

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types)]

mod client;
mod interpose;
pub use interpose::*;

#[ctor::ctor]
fn rage_sandbox_init() {
    client::init_from_env();
}
