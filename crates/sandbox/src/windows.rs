#![cfg(target_os = "windows")]

//! Windows-specific named pipe server for the rage sandbox (parent side).
//!
//! This module provides the parent-side infrastructure for the Windows
//! Detours sandbox:
//!
//! 1. **Creates a named pipe** that the injected `rage_sandbox.dll` connects
//!    to over `\\.\pipe\rage_sandbox_{pid}_{nonce}`.
//! 2. **Injects `rage_sandbox.dll`** into the child process at startup
//!    (future: `run_sandboxed` will handle this via `CreateProcess` +
//!    `CreateRemoteThread`).
//! 3. **Reads [`AccessEvent`]s** from the pipe until the child exits
//!    (signalled by `ERROR_BROKEN_PIPE` or a zero-byte read).

#[allow(unused_imports)]
use crate::event::{AccessEvent, PathSet, RunResult};
use crate::pipe_proto;
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
#[allow(unused_imports)]
use windows_sys::Win32::Storage::FileSystem::{ReadFile, FILE_FLAG_OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_INBOUND, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

/// Creates a new inbound, synchronous, byte-mode named pipe instance.
///
/// The pipe name has the form `\\.\pipe\rage_sandbox_{pid}_{nonce}`.
///
/// The nonce is derived from the sub-second component of the current time
/// XOR-mixed with the process ID — no external `rand` dependency required.
///
/// # Errors
///
/// Returns `Err(io::Error::last_os_error())` when `CreateNamedPipeW` fails
/// (e.g. insufficient permissions, too many pipe instances).
pub fn create_pipe() -> std::io::Result<(HANDLE, String)> {
    // SAFETY: GetCurrentProcessId is always safe to call.
    let pid = unsafe { GetCurrentProcessId() };

    // Cheap nonce via XOR to spread the namespace — rand is not a dependency.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        ^ (pid as u64 * 0x517C_C1B7_2722_0A95);

    let name = format!("\\\\.\\pipe\\rage_sandbox_{}_{}", pid, nonce);

    // Encode the name as a null-terminated UTF-16 string for the Win32 API.
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0u16)).collect();

    // SAFETY: All arguments are valid Win32 values; the returned handle is
    // checked immediately below.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,                // nMaxInstances: exactly one client at a time
            0,                // nOutBufferSize: no outbound data
            65536,            // nInBufferSize: 64 KiB inbound
            0,                // nDefaultTimeOut: use system default (50 ms)
            std::ptr::null(), // lpSecurityAttributes: inherit from process
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }

    Ok((handle, name))
}

/// Waits for a client to connect to `pipe`, then reads all [`AccessEvent`]s
/// until the client closes the connection (`ERROR_BROKEN_PIPE` or a
/// zero-byte read).
///
/// Returns an empty `Vec` if the client fails to connect (and the error is
/// not `ERROR_PIPE_CONNECTED`, which means the client connected before this
/// function was called — a normal race the caller need not worry about).
pub fn read_events(pipe: HANDLE) -> Vec<AccessEvent> {
    // SAFETY: pipe is a valid HANDLE and null is a valid lpOverlapped value
    // for synchronous (non-overlapped) named-pipe I/O.
    let connect_result = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) };
    // SAFETY: GetLastError is always safe to call immediately after a Win32 call.
    let last_error = unsafe { GetLastError() };

    // connect_result == 0 means the Win32 call returned FALSE.
    // ERROR_PIPE_CONNECTED is acceptable: the client connected between
    // CreateNamedPipeW and ConnectNamedPipe — data may already be buffered.
    if connect_result == 0 && last_error != ERROR_PIPE_CONNECTED {
        return Vec::new();
    }

    let mut raw_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut read_scratch = [0u8; 4096];
    let mut events: Vec<AccessEvent> = Vec::new();

    loop {
        let mut bytes_read: u32 = 0;

        // SAFETY: pipe is a valid HANDLE; read_scratch is a live mutable
        // buffer; null is valid for lpOverlapped with synchronous I/O.
        let ok = unsafe {
            ReadFile(
                pipe,
                read_scratch.as_mut_ptr().cast(),
                4096,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };

        if ok == 0 || bytes_read == 0 {
            // SAFETY: GetLastError is always safe to call.
            let err = unsafe { GetLastError() };
            // ERROR_BROKEN_PIPE: client closed the pipe (normal shutdown).
            // ERROR_NO_DATA:     no more data (pipe closing in NOWAIT mode).
            // bytes_read == 0:   zero-length read — treat as EOF.
            if err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA || bytes_read == 0 {
                break;
            }
            // Any other error — stop reading.
            break;
        }

        raw_buf.extend_from_slice(&read_scratch[..bytes_read as usize]);

        // Drain all complete wire records from the accumulation buffer.
        let mut offset = 0;
        loop {
            match pipe_proto::decode_event(&raw_buf[offset..]) {
                Some((event, consumed)) => {
                    events.push(event);
                    offset += consumed;
                }
                None => break,
            }
        }
        raw_buf.drain(..offset);
    }

    // Drain any trailing complete records that arrived before the pipe closed.
    let mut offset = 0;
    loop {
        match pipe_proto::decode_event(&raw_buf[offset..]) {
            Some((event, consumed)) => {
                events.push(event);
                offset += consumed;
            }
            None => break,
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GENERIC_WRITE, OPEN_EXISTING,
    };

    /// Verifies that [`create_pipe`] returns a valid pipe handle and a pipe
    /// name that matches the expected `\\.\pipe\rage_sandbox_` prefix.
    #[test]
    fn create_pipe_returns_valid_handle_and_name() {
        let (handle, name) = create_pipe().expect("create_pipe should succeed");

        assert!(
            name.starts_with("\\\\.\\pipe\\"),
            "pipe name should start with \\\\.\\pipe\\ but got: {name}"
        );
        assert!(
            name.contains("rage_sandbox_"),
            "pipe name should contain 'rage_sandbox_' but got: {name}"
        );

        // SAFETY: handle is a valid, open pipe handle returned by create_pipe.
        unsafe { CloseHandle(handle) };
    }

    /// Verifies the full round-trip: a writer thread connects to the server
    /// pipe, encodes one [`AccessEvent`], writes it, then closes; then
    /// [`read_events`] must return exactly that event.
    #[test]
    fn pipe_round_trip_single_event() {
        let (handle, name) = create_pipe().expect("create_pipe should succeed");

        // Encode the pipe name as null-terminated UTF-16 for the Win32 API
        // used inside the thread (thread closures can't borrow &str across
        // thread boundaries without Arc/String).
        let pipe_name_wide: Vec<u16> =
            name.encode_utf16().chain(std::iter::once(0u16)).collect();

        let writer_thread = std::thread::spawn(move || {
            // Connect to the server end as a write-only client.
            // SAFETY: all arguments are valid Win32 values; the handle is
            // checked immediately.
            let client = unsafe {
                CreateFileW(
                    pipe_name_wide.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    0,
                )
            };
            assert_ne!(
                client,
                INVALID_HANDLE_VALUE,
                "client CreateFileW failed: {:?}",
                std::io::Error::last_os_error()
            );

            // Encode one Read event into the binary wire format.
            let event = AccessEvent::Read {
                path: "C:\\test.txt".to_string(),
                pid: 42,
            };
            let mut buf = Vec::new();
            crate::pipe_proto::encode_event(&event, &mut buf);

            // Write the encoded event to the pipe.
            let mut bytes_written: u32 = 0;
            // SAFETY: client is a valid open handle; buf is a live, non-empty
            // slice; null is valid for lpOverlapped with synchronous I/O.
            let ok = unsafe {
                WriteFile(
                    client,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                ok,
                0,
                "WriteFile failed: {:?}",
                std::io::Error::last_os_error()
            );

            // Close the client handle to signal EOF to the server.
            // SAFETY: client is a valid, open handle and this is its sole owner.
            unsafe { CloseHandle(client) };
        });

        // Wait for the writer to finish connecting, writing, and closing before
        // calling read_events — this ensures ConnectNamedPipe sees
        // ERROR_PIPE_CONNECTED (client already connected) and ReadFile drains
        // the buffered data before receiving ERROR_BROKEN_PIPE.
        writer_thread.join().expect("writer thread should not panic");

        // Read all events from the server side.
        let events = read_events(handle);

        // Clean up the server handle.
        // SAFETY: handle is a valid, open pipe handle.
        unsafe { CloseHandle(handle) };

        assert_eq!(events.len(), 1, "expected exactly one event, got: {events:?}");
        match &events[0] {
            AccessEvent::Read { path, pid } => {
                assert_eq!(path, "C:\\test.txt", "path mismatch");
                assert_eq!(*pid, 42, "pid mismatch");
            }
            other => panic!("expected AccessEvent::Read, got: {other:?}"),
        }
    }
}
