//! DLL injection entry point for the RAGE sandbox Windows detours.
//!
//! ## Injection mechanism
//!
//! The parent process injects this DLL into the child by:
//!
//! 1. Suspending the child immediately after `CreateProcess` with
//!    `CREATE_SUSPENDED`.
//! 2. Calling `CreateRemoteThread(child, LoadLibraryW, dll_path)` to load
//!    the DLL into the child's address space.
//! 3. Resuming the child — Windows then calls `DllMain` with
//!    `DLL_PROCESS_ATTACH` on the injecting thread before any child code runs.
//!
//! ## Hooks
//!
//! `hooks::setup_hooks` installs inline patches (via `retour`) over
//! `kernel32!CreateFileW` and `ntdll!NtCreateFile`.  Every file-system access
//! is forwarded to the parent over a named pipe ([`ipc`] module).
//!
//! ## Safety
//!
//! `unsafe_code = "allow"` is set in `Cargo.toml` for this crate only.
//! Unsafe code is intentionally confined to `hooks.rs` and `ipc.rs`; this
//! file uses only safe-Rust constructs apart from the required `DllMain`
//! signature.

#![cfg(windows)]

mod hooks;
mod ipc;

use windows_sys::Win32::Foundation::{BOOL, HINSTANCE};
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// Windows DLL entry point.
///
/// Called by the Windows loader whenever the DLL is loaded into or unloaded
/// from a process.
///
/// # Safety
///
/// `DllMain` is called with the loader lock held.  We must not:
///
/// - Load additional DLLs (would deadlock the loader).
/// - Perform complex synchronisation that might re-enter the loader.
///
/// What we *do* here is safe in practice:
///
/// - Connecting a named pipe (`CreateFile`) is a single Win32 call that does
///   not load further DLLs.
/// - Patching memory via `retour` modifies only the target function's
///   prologue bytes; it does not invoke the loader.
///
/// Errors from `hooks::setup_hooks` are silently dropped (`let _ = …`).
/// The DLL must **never** prevent the child process from running.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    _hmodule: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            if let Ok(pipe_name) = std::env::var("RAGE_PIPE_NAME") {
                let _ = hooks::setup_hooks(&pipe_name);
            }
            1
        }
        DLL_PROCESS_DETACH => 1,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "windows")]
    fn dll_main_attach_without_env_var_does_not_panic() {
        use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
        std::env::remove_var("RAGE_PIPE_NAME");
        let result = unsafe {
            super::DllMain(
                std::ptr::null_mut(),
                DLL_PROCESS_ATTACH,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 1, "DllMain must return TRUE");
    }
}
