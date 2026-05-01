#![cfg(windows)]

use sandbox::event::AccessEvent;
use sandbox::pipe_proto;
use std::io;
use windows_sys::Win32::Foundation::GENERIC_WRITE;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// Named-pipe client used by the injected DLL to forward [`AccessEvent`]s to
/// the parent process over a Windows named pipe.
pub struct PipeClient {
    handle: HANDLE,
}

// SAFETY: HANDLE is a raw pointer-sized integer (`isize`).  PipeClient is
// wrapped in a Mutex<Option<PipeClient>> in practice — called only from the
// DLL_PROCESS_ATTACH thread or a single dedicated hook thread.
unsafe impl Send for PipeClient {}

impl PipeClient {
    /// Open a write-only connection to the named pipe at `pipe_name`.
    ///
    /// # Errors
    ///
    /// Returns `Err(io::Error::last_os_error())` when the pipe does not exist
    /// or cannot be opened (e.g. the server is not yet listening).
    pub fn connect(pipe_name: &str) -> io::Result<Self> {
        // Encode the path as a null-terminated UTF-16 string for the Win32 API.
        let wide: Vec<u16> = pipe_name
            .encode_utf16()
            .chain(std::iter::once(0u16))
            .collect();

        // SAFETY: all arguments are well-formed; the returned handle is checked
        // immediately below.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                0,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { handle })
    }

    /// Encode `event` into the binary wire format and write it to the pipe.
    ///
    /// Errors are intentionally **ignored**: the sandbox layer is best-effort
    /// and must never interfere with the hooked process's normal operation.
    pub fn write_event(&mut self, event: &AccessEvent) {
        let mut buf = Vec::new();
        pipe_proto::encode_event(event, &mut buf);

        // SAFETY: `handle` is valid (opened in `connect` and not yet closed).
        // `buf` is a valid, non-empty slice for the lifetime of this call.
        unsafe {
            let mut bytes_written: u32 = 0;
            WriteFile(
                self.handle,
                buf.as_ptr().cast(),
                buf.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            );
            // Return value intentionally ignored — see doc comment above.
        }
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        // SAFETY: `handle` is valid and this is the sole owner; CloseHandle
        // is safe to call exactly once, which Drop guarantees.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connecting to a guaranteed-absent pipe must return an `Err` on Windows.
    #[cfg(target_os = "windows")]
    #[test]
    fn pipe_client_connect_to_missing_pipe_returns_error() {
        let result = PipeClient::connect(r"\\.\pipe\rage_sandbox_nonexistent_xyzzy");
        assert!(result.is_err());
    }
}
