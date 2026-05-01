#![cfg(windows)]

use crate::ipc::PipeClient;
use retour::static_detour;
use sandbox::event::AccessEvent;
use std::io;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::GENERIC_WRITE;
use windows_sys::Win32::Foundation::{HANDLE, NTSTATUS};
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_DATA;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

/// Global IPC client — set once on `DLL_PROCESS_ATTACH`, never changed.
pub(crate) static IPC_CLIENT: OnceLock<Mutex<PipeClient>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Static detour declarations
// ---------------------------------------------------------------------------

static_detour! {
    static HookCreateFileW: unsafe extern "system" fn(
        *const u16, u32, u32, *const u8, u32, u32, HANDLE
    ) -> HANDLE;

    static HookNtCreateFile: unsafe extern "system" fn(
        *mut HANDLE, u32, *const u8, *mut u8, *const i64, u32, u32, u32, u32, *mut u8, u32
    ) -> NTSTATUS;
}

/// Type alias for transmuting the raw `CreateFileW` function pointer.
type CreateFileWFn =
    unsafe extern "system" fn(*const u16, u32, u32, *const u8, u32, u32, HANDLE) -> HANDLE;

/// Type alias for transmuting the raw `NtCreateFile` function pointer.
type NtCreateFileFn = unsafe extern "system" fn(
    *mut HANDLE,
    u32,
    *const u8,
    *mut u8,
    *const i64,
    u32,
    u32,
    u32,
    u32,
    *mut u8,
    u32,
) -> NTSTATUS;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Convert a null-terminated UTF-16 pointer to a Rust [`String`].
///
/// Returns `None` when `ptr` is null.
///
/// # Safety
///
/// The caller must ensure `ptr` is either null or points to a valid
/// null-terminated UTF-16 string that remains valid for the duration of
/// this call.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf16_lossy(slice).to_string())
}

/// Extract the object name from an `OBJECT_ATTRIBUTES` pointer (64-bit layout).
///
/// **64-bit `OBJECT_ATTRIBUTES` layout** (Windows SDK):
/// ```text
/// offset  0 : Length          (u32)
/// offset  4 : (padding)
/// offset  8 : RootDirectory   (HANDLE, 8 bytes)
/// offset 16 : *UNICODE_STRING ObjectName  (8-byte pointer)
/// offset 24 : Attributes      (u32)
/// ...
/// ```
///
/// **64-bit `UNICODE_STRING` layout**:
/// ```text
/// offset 0 : Length          (u16) — byte count, not character count
/// offset 2 : MaximumLength   (u16)
/// offset 4 : (padding, u32)
/// offset 8 : Buffer          (*const u16, 8-byte pointer)
/// ```
///
/// Returns `None` for null pointers or zero-length object names.
///
/// # Safety
///
/// The caller must ensure `oa_ptr` is either null or points to a valid
/// 64-bit `OBJECT_ATTRIBUTES` structure.  32-bit offsets differ and are
/// **not** handled here.
unsafe fn oa_to_string(oa_ptr: *const u8) -> Option<String> {
    if oa_ptr.is_null() {
        return None;
    }

    // Cast to *const usize so each .add(n) steps by 8 bytes (64-bit pointer size).
    let oa = oa_ptr as *const usize;

    // ObjectName pointer is at byte offset 16 → usize index 2.
    let object_name_ptr = *oa.add(2) as *const u8;
    if object_name_ptr.is_null() {
        return None;
    }

    // UNICODE_STRING: Length (u16) at offset 0, Buffer (*const u16) at offset 8.
    let length_bytes = *(object_name_ptr as *const u16); // byte count
    if length_bytes == 0 {
        return None;
    }

    let buffer_ptr = *(object_name_ptr.add(8) as *const *const u16);
    if buffer_ptr.is_null() {
        return None;
    }

    let char_count = (length_bytes / 2) as usize;
    let slice = std::slice::from_raw_parts(buffer_ptr, char_count);
    Some(String::from_utf16_lossy(slice).to_string())
}

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

/// Send an access event to the global IPC client (best-effort; errors ignored).
fn send_access(is_write: bool, path: Option<String>) {
    let path = match path {
        Some(p) => p,
        None => return,
    };

    // SAFETY: `GetCurrentProcessId` is always safe to call.
    let pid = unsafe { GetCurrentProcessId() };

    let event = if is_write {
        AccessEvent::Write { path, pid }
    } else {
        AccessEvent::Read { path, pid }
    };

    if let Some(mutex) = IPC_CLIENT.get() {
        if let Ok(mut guard) = mutex.lock() {
            guard.write_event(&event);
        }
    }
}

// ---------------------------------------------------------------------------
// Hook implementations
// ---------------------------------------------------------------------------

/// Hook for `kernel32!CreateFileW`.
extern "system" fn hook_create_file_w(
    lp_file_name: *const u16,
    dw_desired_access: u32,
    dw_share_mode: u32,
    lp_security_attributes: *const u8,
    dw_creation_disposition: u32,
    dw_flags_and_attributes: u32,
    h_template_file: HANDLE,
) -> HANDLE {
    // SAFETY: `lp_file_name` is a null-terminated UTF-16 string per the Win32
    // `CreateFileW` contract.
    let path = unsafe { wide_ptr_to_string(lp_file_name) };
    let is_write =
        (dw_desired_access & GENERIC_WRITE) != 0 || (dw_desired_access & FILE_WRITE_DATA) != 0;
    send_access(is_write, path);

    // SAFETY: All arguments are forwarded unmodified to the original function
    // via the retour trampoline.
    unsafe {
        HookCreateFileW.call(
            lp_file_name,
            dw_desired_access,
            dw_share_mode,
            lp_security_attributes,
            dw_creation_disposition,
            dw_flags_and_attributes,
            h_template_file,
        )
    }
}

/// Hook for `ntdll!NtCreateFile`.
#[allow(clippy::too_many_arguments)]
extern "system" fn hook_nt_create_file(
    file_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *const u8,
    io_status_block: *mut u8,
    allocation_size: *const i64,
    file_attributes: u32,
    share_access: u32,
    create_disposition: u32,
    create_options: u32,
    ea_buffer: *mut u8,
    ea_length: u32,
) -> NTSTATUS {
    const NT_FILE_WRITE_DATA: u32 = 0x0002;
    const NT_FILE_APPEND_DATA: u32 = 0x0004;

    // SAFETY: `object_attributes` is a valid OBJECT_ATTRIBUTES pointer per
    // the NtCreateFile contract (or null, which oa_to_string handles).
    let path = unsafe { oa_to_string(object_attributes) };
    let is_write = (desired_access & (NT_FILE_WRITE_DATA | NT_FILE_APPEND_DATA)) != 0;
    send_access(is_write, path);

    // SAFETY: All arguments are forwarded unmodified to the original function
    // via the retour trampoline.
    unsafe {
        HookNtCreateFile.call(
            file_handle,
            desired_access,
            object_attributes,
            io_status_block,
            allocation_size,
            file_attributes,
            share_access,
            create_disposition,
            create_options,
            ea_buffer,
            ea_length,
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode a `&str` as a null-terminated UTF-16 `Vec<u16>`.
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0u16)).collect()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Install file-system hooks in the current process.
///
/// 1. Connects to the named pipe at `pipe_name` (returns error if absent).
/// 2. Stores the [`PipeClient`] in the global [`IPC_CLIENT`].
/// 3. Resolves `CreateFileW` from `kernel32.dll` and `NtCreateFile` from
///    `ntdll.dll`, then installs inline patches via retour.
///
/// # Errors
///
/// Returns an [`io::Error`] if:
/// - The pipe cannot be opened (`PipeClient::connect` fails).
/// - A module handle cannot be obtained (`GetModuleHandleW` returns 0).
/// - A function address cannot be resolved (`GetProcAddress` returns `None`).
/// - A detour cannot be initialized or enabled (retour returns an error).
pub fn setup_hooks(pipe_name: &str) -> io::Result<()> {
    // 1. Connect to the named pipe — propagate any error immediately.
    let client = PipeClient::connect(pipe_name)?;

    // 2. Store the client in the global OnceLock.
    let _ = IPC_CLIENT.set(Mutex::new(client));

    // SAFETY: All Win32 API calls are guarded by explicit null/error checks.
    // Transmutes are between function-pointer types of identical size and
    // calling convention (unsafe extern "system" fn).
    unsafe {
        // ----------------------------------------------------------------
        // kernel32!CreateFileW
        // ----------------------------------------------------------------
        let kernel32_wide = to_wide_null("kernel32.dll");
        let kernel32 = GetModuleHandleW(kernel32_wide.as_ptr());
        if kernel32.is_null() {
            return Err(io::Error::last_os_error());
        }

        let create_file_w_addr = GetProcAddress(kernel32, b"CreateFileW\0".as_ptr())
            .ok_or_else(io::Error::last_os_error)?;
        let create_file_w: CreateFileWFn = std::mem::transmute(create_file_w_addr);

        HookCreateFileW
            .initialize(create_file_w, |a, b, c, d, e, f, g| {
                hook_create_file_w(a, b, c, d, e, f, g)
            })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .enable()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        // ----------------------------------------------------------------
        // ntdll!NtCreateFile
        // ----------------------------------------------------------------
        let ntdll_wide = to_wide_null("ntdll.dll");
        let ntdll = GetModuleHandleW(ntdll_wide.as_ptr());
        if ntdll.is_null() {
            return Err(io::Error::last_os_error());
        }

        let nt_create_file_addr = GetProcAddress(ntdll, b"NtCreateFile\0".as_ptr())
            .ok_or_else(io::Error::last_os_error)?;
        let nt_create_file: NtCreateFileFn = std::mem::transmute(nt_create_file_addr);

        HookNtCreateFile
            .initialize(nt_create_file, |a, b, c, d, e, f, g, h, i, j, k| {
                hook_nt_create_file(a, b, c, d, e, f, g, h, i, j, k)
            })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .enable()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "windows")]
    fn setup_hooks_with_bad_pipe_name_returns_error() {
        let result = super::setup_hooks(r"\\.\pipe\rage_test_no_such_pipe_xyz");
        assert!(
            result.is_err(),
            "setup_hooks should fail when the pipe does not exist"
        );
    }
}
