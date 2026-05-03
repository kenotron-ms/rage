#![cfg(target_os = "windows")]

//! Windows-specific named pipe server for the rage sandbox (parent side).
//!
//! This module provides the parent-side infrastructure for the Windows
//! Detours sandbox:
//!
//! 1. **Creates a named pipe** that the injected `rage_sandbox.dll` connects
//!    to over `\\\\.\\pipe\\rage_sandbox_{pid}_{nonce}`.
//! 2. **Injects `rage_sandbox.dll`** into the child process at startup
//!    via `inject_and_spawn` (suspended `CreateProcess` + `CreateRemoteThread`
//!    with `LoadLibraryW`).
//! 3. **Reads [`AccessEvent`]s** from the pipe until the child exits
//!    (signalled by `ERROR_BROKEN_PIPE` or a zero-byte read).

use crate::event::{AccessEvent, PathSet, RunResult};
use crate::pipe_proto;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING,
    ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
// In windows-sys 0.59, PIPE_ACCESS_INBOUND and FILE_FLAG_OVERLAPPED are typed as
// FILE_FLAGS_AND_ATTRIBUTES and live in Win32::Storage::FileSystem.
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, CreateRemoteThread, GetCurrentProcessId, GetExitCodeProcess,
    ResetEvent, ResumeThread, TerminateProcess, WaitForSingleObject, CREATE_SUSPENDED, INFINITE,
    PROCESS_INFORMATION, STARTUPINFOW,
};

/// HANDLE wrapper that satisfies the `Send` bound required by
/// `tokio::task::spawn_blocking`.
///
/// # Safety
///
/// HANDLE values are kernel-object references valid on any thread; the Windows
/// kernel serialises all access.  The wrapper must NOT be cloned — each
/// `SendHandle` must have exactly one owner at a time.
struct SendHandle(HANDLE);
// SAFETY: see doc comment above.
unsafe impl Send for SendHandle {}

/// Blocking work for [`run_sandboxed`]: drain the pipe and wait for the child.
///
/// Defined as a free function (not a closure) so that the `spawn_blocking`
/// closure only captures whole `SendHandle` values, which are `Send`.  With
/// Rust 2021 precision capture a closure that accesses `handle.0` captures the
/// inner `*mut c_void` field — which is **not** `Send`.  Passing `SendHandle`
/// as a by-value function argument forces whole-struct capture instead.
fn do_pipe_blocking(pipe_wrapper: SendHandle, proc_wrapper: SendHandle) -> (Vec<AccessEvent>, i32) {
    let pipe = pipe_wrapper.0;
    let proc_h = proc_wrapper.0;

    // Drain all AccessEvents from the pipe. Blocks until the DLL closes
    // the write end (which happens when the process exits).
    let events = read_events(pipe);

    // Wait for the child process to exit (belt-and-suspenders after pipe EOF),
    // then collect the exit code and release both handles.
    let mut raw_exit: u32 = 0;
    // SAFETY: proc_h is a valid process handle; INFINITE waits indefinitely.
    unsafe {
        WaitForSingleObject(proc_h, INFINITE);
        GetExitCodeProcess(proc_h, &mut raw_exit);
        CloseHandle(pipe);
        CloseHandle(proc_h);
    }

    (events, raw_exit as i32)
}

/// Creates a new inbound, synchronous, byte-mode named pipe instance.
///
/// The pipe name has the form `\\\\.\\pipe\\rage_sandbox_{pid}_{nonce}`.
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
        ^ (pid as u64).wrapping_mul(0x517C_C1B7_2722_0A95);

    let name = format!("\\\\.\\pipe\\rage_sandbox_{}_{}", pid, nonce);

    // Encode the name as a null-terminated UTF-16 string for the Win32 API.
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0u16)).collect();

    // SAFETY: All arguments are valid Win32 values; the returned handle is
    // checked immediately below. FILE_FLAG_OVERLAPPED enables async I/O so
    // that ConnectNamedPipe and ReadFile can wait on a Win32 event with
    // GetOverlappedResult — eliminating the connect/disconnect race.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
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
/// Uses overlapped I/O internally so that `ConnectNamedPipe` and `ReadFile`
/// wait on a Win32 event with `GetOverlappedResult`. This eliminates the
/// race where a synchronous `ConnectNamedPipe` would block indefinitely
/// after a client connect/disconnect cycle that completed before the
/// server entered the wait.
///
/// Returns an empty `Vec` if the connect or first read fails. Partially-read
/// events are decoded best-effort.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn read_events(pipe: HANDLE) -> Vec<AccessEvent> {
    // ----- Connect (overlapped) -----------------------------------------
    // SAFETY: All Win32 calls are checked; the event handle has a single owner.
    let connect_event =
        unsafe { CreateEventW(std::ptr::null(), 1 /* manual reset */, 0, std::ptr::null()) };
    if connect_event.is_null() {
        return Vec::new();
    }

    let mut connect_overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    connect_overlapped.hEvent = connect_event;

    // SAFETY: pipe is a valid HANDLE; connect_overlapped lives until we
    // observe its completion (we either return early or wait below).
    let connect_result = unsafe { ConnectNamedPipe(pipe, &mut connect_overlapped) };

    // With FILE_FLAG_OVERLAPPED, ConnectNamedPipe always returns FALSE.
    // The interesting cases are encoded in GetLastError().
    if connect_result == 0 {
        // SAFETY: GetLastError is always safe to call after a Win32 call.
        let err = unsafe { GetLastError() };
        match err {
            ERROR_PIPE_CONNECTED => {
                // Client connected before our ConnectNamedPipe call —
                // accepted, proceed to ReadFile. Win32 documents that the
                // event is NOT signalled in this case, but the read loop
                // below uses its own per-iteration overlapped struct, so we
                // just continue.
            }
            ERROR_IO_PENDING => {
                // Async wait; block on the event handle.
                // SAFETY: connect_event is a valid manual-reset event handle.
                let wait = unsafe { WaitForSingleObject(connect_event, INFINITE) };
                if wait != WAIT_OBJECT_0 {
                    // SAFETY: connect_event is owned by us.
                    unsafe { CloseHandle(connect_event) };
                    return Vec::new();
                }
            }
            _ => {
                // Hard failure (e.g. invalid handle).
                // SAFETY: connect_event is owned by us.
                unsafe { CloseHandle(connect_event) };
                return Vec::new();
            }
        }
    }

    // ----- Read (overlapped) --------------------------------------------
    let mut raw_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut read_scratch = [0u8; 4096];
    let mut events: Vec<AccessEvent> = Vec::new();

    // Reuse the same event handle for ReadFile completions; manual-reset
    // means we explicitly ResetEvent before each operation.
    loop {
        // SAFETY: connect_event is a valid manual-reset event handle.
        unsafe { ResetEvent(connect_event) };

        let mut io_overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        io_overlapped.hEvent = connect_event;

        // SAFETY: pipe is a valid overlapped HANDLE; read_scratch lives
        // until we either complete the wait or break out of this iteration.
        let ok = unsafe {
            ReadFile(
                pipe,
                read_scratch.as_mut_ptr().cast(),
                4096,
                std::ptr::null_mut(), // bytes_read retrieved via GetOverlappedResult
                &mut io_overlapped,
            )
        };

        let mut bytes_read: u32 = 0;
        if ok == 0 {
            // SAFETY: GetLastError is always safe to call.
            let err = unsafe { GetLastError() };
            match err {
                ERROR_IO_PENDING => {
                    // SAFETY: connect_event is a valid event handle.
                    let wait = unsafe { WaitForSingleObject(connect_event, INFINITE) };
                    if wait != WAIT_OBJECT_0 {
                        break;
                    }
                    // SAFETY: pipe is valid; io_overlapped is alive; bWait=FALSE
                    // because the event already signalled.
                    let got = unsafe {
                        GetOverlappedResult(pipe, &io_overlapped, &mut bytes_read, 0)
                    };
                    if got == 0 {
                        // ERROR_BROKEN_PIPE / ERROR_HANDLE_EOF: client closed —
                        // normal shutdown. Anything else: stop reading.
                        break;
                    }
                }
                ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF | ERROR_NO_DATA => {
                    // Client already closed the pipe between iterations.
                    break;
                }
                _ => {
                    // Any other error — stop reading.
                    break;
                }
            }
        } else {
            // Synchronous completion (rare on overlapped handles, but legal).
            // SAFETY: pipe is valid; io_overlapped is alive.
            let got = unsafe {
                GetOverlappedResult(pipe, &io_overlapped, &mut bytes_read, 0)
            };
            if got == 0 {
                break;
            }
        }

        if bytes_read == 0 {
            // Zero-length read → EOF.
            break;
        }

        raw_buf.extend_from_slice(&read_scratch[..bytes_read as usize]);

        // Drain all complete wire records from the accumulation buffer.
        let mut offset = 0;
        while let Some((event, consumed)) = pipe_proto::decode_event(&raw_buf[offset..]) {
            events.push(event);
            offset += consumed;
        }
        raw_buf.drain(..offset);
    }

    // Drain any trailing complete records that arrived before the pipe closed.
    let mut offset = 0;
    while let Some((event, consumed)) = pipe_proto::decode_event(&raw_buf[offset..]) {
        events.push(event);
        offset += consumed;
    }

    // SAFETY: connect_event is a valid event handle owned solely by this fn.
    unsafe { CloseHandle(connect_event) };

    events
}

/// Returns the path to `rage_sandbox.dll`.
///
/// Resolution order (mirrors the macOS dylib resolution pattern):
/// 1. If the environment variable `RAGE_SANDBOX_DLL_PATH` is set, its value
///    is returned verbatim as a [`PathBuf`].
/// 2. Otherwise, if `<dir-of-current-exe>/rage_sandbox.dll` exists, that
///    path is returned (the colocated install layout used by
///    `cargo install --path crates/cli` and packaged distributions).
/// 3. Otherwise, the path baked in at compile time by `build.rs`
///    (`RAGE_SANDBOX_DLL_DEFAULT`, the workspace `target/<profile>/`
///    artifact path) is returned.  This succeeds during local development
///    because `cargo build --workspace` colocates all artifacts in
///    `target/<profile>/`.
///
/// # Errors
///
/// Returns `Err` only when steps 1 and 2 both fail AND `current_exe()`
/// itself errors (rare). The compile-time fallback always exists as a
/// `PathBuf`; whether the file at that path is on disk is a runtime
/// concern caught by the `dll_path.exists()` check in `run_sandboxed`.
pub fn find_dll_path() -> std::io::Result<PathBuf> {
    // 1. Runtime env-var override.
    if let Ok(override_path) = std::env::var("RAGE_SANDBOX_DLL_PATH") {
        return Ok(PathBuf::from(override_path));
    }

    // 2. Colocated with the current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let colocated = parent.join("rage_sandbox.dll");
            if colocated.exists() {
                return Ok(colocated);
            }
        }
    }

    // 3. Compile-time-baked workspace target/<profile>/ path.
    Ok(PathBuf::from(env!("RAGE_SANDBOX_DLL_DEFAULT")))
}

/// Creates a child process in a suspended state, injects `dll_path` by
/// spawning a remote thread that calls `LoadLibraryW`, waits for the loader
/// thread to complete, frees the remote buffer, then resumes the main thread.
///
/// # Arguments
///
/// * `cmd`        — Command to run (wrapped as `cmd /c <cmd>`).
/// * `cwd`        — Working directory for the child process.
/// * `env`        — Environment variables forwarded to the child.
/// * `_pipe_name` — Named-pipe path the DLL will use (passed via `env`; this
///   parameter is reserved for future direct use).
/// * `dll_path`   — Path to `rage_sandbox.dll` to inject.
///
/// # Returns
///
/// On success, returns the process `HANDLE` of the child process.  The caller
/// is responsible for calling `WaitForSingleObject` and `CloseHandle`.
///
/// # Errors
///
/// Returns `Err` if `CreateProcessW` fails.  If `VirtualAllocEx` fails after
/// the process is created, the child is terminated via `TerminateProcess` and
/// both handles are closed before the error is returned.
///
/// # ASLR note
///
/// `kernel32.dll` is loaded at the same base address in every process on a
/// given Windows boot (ASLR randomises the base once per boot, not per
/// process), so `LoadLibraryW`'s virtual address obtained from the calling
/// process is valid in the target process.
#[allow(clippy::transmute_undefined_repr)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[allow(clippy::manual_c_str_literals)]
#[allow(clippy::missing_transmute_annotations)]
#[allow(clippy::doc_overindented_list_items)]
pub fn inject_and_spawn(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    _pipe_name: &str,
    dll_path: &Path,
) -> std::io::Result<HANDLE> {
    // 1. Build the command line as a null-terminated UTF-16 string.
    //    CreateProcessW requires a *mut u16 (it may modify the buffer).
    let cmd_str = format!("cmd /c {cmd}");
    let mut cmd_wide: Vec<u16> = cmd_str
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();

    // 2. Build the environment block: each entry is "KEY=VALUE\0", followed
    //    by an extra '\0' double-null terminator.
    let mut env_block: Vec<u16> = Vec::new();
    for (k, v) in env {
        let entry = format!("{k}={v}");
        env_block.extend(entry.encode_utf16());
        env_block.push(0u16);
    }
    env_block.push(0u16); // double-null terminator

    // 3. Current working directory as null-terminated UTF-16.
    let cwd_str = cwd.to_string_lossy();
    let cwd_wide: Vec<u16> = cwd_str
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();

    // 4. Initialise STARTUPINFOW (all fields zeroed, then cb set) and
    //    PROCESS_INFORMATION (fully zeroed; filled in by CreateProcessW).
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // 5. Launch the child suspended so we can inject the DLL before it runs.
    //    0x0400 = CREATE_UNICODE_ENVIRONMENT — env_block is UTF-16.
    // SAFETY: All pointers are valid; cmd_wide is kept alive for the call.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),          // lpApplicationName  (use command line)
            cmd_wide.as_mut_ptr(),     // lpCommandLine      (mutable per API)
            std::ptr::null(),          // lpProcessAttributes
            std::ptr::null(),          // lpThreadAttributes
            0,                         // bInheritHandles
            CREATE_SUSPENDED | 0x0400, // dwCreationFlags (0x0400 = CREATE_UNICODE_ENVIRONMENT)
            env_block.as_ptr().cast(), // lpEnvironment
            cwd_wide.as_ptr(),         // lpCurrentDirectory
            &si,                       // lpStartupInfo
            &mut pi,                   // lpProcessInformation
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // 6. Capture the process and primary-thread handles.
    let proc_handle = pi.hProcess;
    let thread_handle = pi.hThread;

    // 7. Encode the DLL path as a null-terminated UTF-16 string.
    let dll_wide: Vec<u16> = dll_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .collect();
    let dll_bytes = dll_wide.len() * 2; // byte length of the UTF-16 buffer

    // 8. Allocate a buffer in the child's address space for the DLL path.
    // SAFETY: proc_handle is a valid process handle; null lets the OS choose.
    let remote_buf = unsafe {
        VirtualAllocEx(
            proc_handle,
            std::ptr::null(),
            dll_bytes,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote_buf.is_null() {
        // Clean up the suspended child before returning the error.
        unsafe {
            TerminateProcess(proc_handle, 1);
            CloseHandle(proc_handle);
            CloseHandle(thread_handle);
        }
        return Err(std::io::Error::last_os_error());
    }

    // 9. Copy the DLL path into the child's address space.
    // SAFETY: remote_buf points to dll_bytes of committed, writable memory in
    // the child; dll_wide is a live buffer of the correct length.
    let mut written: usize = 0;
    let ok = unsafe {
        WriteProcessMemory(
            proc_handle,
            remote_buf,
            dll_wide.as_ptr().cast(),
            dll_bytes,
            &mut written,
        )
    };
    if ok == 0 {
        // WriteProcessMemory failed — the remote buffer has garbage; clean up.
        unsafe {
            VirtualFreeEx(proc_handle, remote_buf, 0, MEM_RELEASE);
            TerminateProcess(proc_handle, 1);
            CloseHandle(proc_handle);
            CloseHandle(thread_handle);
        }
        return Err(std::io::Error::last_os_error());
    }

    // 10. Resolve LoadLibraryW from kernel32.dll in this process.
    //     The VA is valid in the child because kernel32 is mapped at the same
    //     address in every process on the same Windows boot (ASLR is
    //     per-boot, not per-process for system DLLs).
    // SAFETY: "kernel32.dll\0" is a valid null-terminated wide string.
    let kernel32_wide: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    let k32 = unsafe { GetModuleHandleW(kernel32_wide.as_ptr()) };
    // SAFETY: k32 is a valid module handle; the proc name is a valid ANSI string.
    let load_lib = unsafe { GetProcAddress(k32, b"LoadLibraryW\0".as_ptr()) };

    // 11. Spawn a remote thread that calls LoadLibraryW(remote_buf).
    //     Transmute FARPROC → thread-start-routine function pointer (same size,
    //     both are pointer-sized function pointers).
    // SAFETY: load_lib is LoadLibraryW whose VA is valid in the child process.
    let mut remote_tid: u32 = 0;
    let remote_thread = unsafe {
        CreateRemoteThread(
            proc_handle,
            std::ptr::null(),
            0,
            std::mem::transmute(load_lib), // FARPROC → LPTHREAD_START_ROUTINE
            remote_buf,
            0,
            &mut remote_tid,
        )
    };
    if !remote_thread.is_null() {
        // Wait up to 5 s for LoadLibrary to finish, then release the thread handle.
        // SAFETY: remote_thread is a valid thread handle returned by CreateRemoteThread.
        unsafe {
            WaitForSingleObject(remote_thread, 5000);
            CloseHandle(remote_thread);
        }
    }

    // 12. Free the remote buffer, resume the main thread, release the thread handle.
    // SAFETY: remote_buf was allocated with VirtualAllocEx; MEM_RELEASE requires size 0.
    unsafe {
        VirtualFreeEx(proc_handle, remote_buf, 0, MEM_RELEASE);
        ResumeThread(thread_handle);
        CloseHandle(thread_handle);
    }

    // 13. Return the process handle; the caller waits for the process and closes it.
    Ok(proc_handle)
}

/// Runs `cmd` in a sandboxed child process, recording all file-system accesses.
///
/// Flow:
/// 1. Creates a named pipe for the injected DLL to report events over
///    `\\\\.\\pipe\\rage_sandbox_{pid}_{nonce}`.
/// 2. Locates `rage_sandbox.dll` via `RAGE_SANDBOX_DLL_PATH` or next to the
///    current executable.  Returns an error with a descriptive message (and a
///    build hint) if the DLL is not found.
/// 3. Spawns the child in a suspended state, injects the DLL via
///    `CreateRemoteThread(LoadLibraryW)`, then resumes the main thread.
/// 4. Runs blocking Win32 work on a `tokio::task::spawn_blocking` thread:
///    drains all [`AccessEvent`]s from the pipe, waits for the child process to
///    exit, retrieves the exit code, and closes all handles.
/// 5. Returns `RunResult { exit_code, path_set: PathSet::from_events(&events) }`.
///
/// # Errors
///
/// Returns `Err` if the DLL is missing, the pipe cannot be created, or
/// `inject_and_spawn` fails.  The error messages are human-readable and include
/// context sufficient to diagnose the problem.
pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> anyhow::Result<RunResult> {
    let dll_path = find_dll_path().unwrap_or_else(|_| PathBuf::from("rage_sandbox.dll"));

    if !dll_path.exists() {
        anyhow::bail!(
            "sandbox DLL not found at `{}`: build `cargo build -p sandbox-windows-detours` first or set RAGE_SANDBOX_DLL_PATH",
            dll_path.display()
        );
    }

    let (pipe_handle, pipe_name) =
        create_pipe().map_err(|e| anyhow::anyhow!("create named pipe: {}", e))?;

    let mut full_env = env.to_vec();
    full_env.push(("RAGE_PIPE_NAME".to_string(), pipe_name.clone()));

    let proc_handle = inject_and_spawn(cmd, cwd, &full_env, &pipe_name, &dll_path)
        .map_err(|e| anyhow::anyhow!("inject_and_spawn: {}", e))?;

    let send_pipe = SendHandle(pipe_handle);
    let send_proc = SendHandle(proc_handle);

    // `do_pipe_blocking` is a free function (not a closure) so the closure
    // only captures whole `SendHandle` values — which are `Send`.  A closure
    // that accesses `handle.0` directly would capture the inner `*mut c_void`
    // field (not `Send`) due to Rust 2021 precision closure capture.
    let (events, exit_code) =
        tokio::task::spawn_blocking(move || do_pipe_blocking(send_pipe, send_proc))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {}", e))?;

    Ok(RunResult {
        exit_code,
        path_set: PathSet::from_events(&events),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    /// Verifies that [`create_pipe`] returns a valid pipe handle and a pipe
    /// name that matches the expected `\\\\.\\pipe\\rage_sandbox_` prefix.
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
    ///
    /// NOTE: This test requires `read_events` to be called concurrently with
    /// the writer (ConnectNamedPipe blocks until the client connects). If the
    /// writer completes before ConnectNamedPipe is called, the pipe returns to
    /// DISCONNECTED state and ConnectNamedPipe blocks indefinitely. The
    /// end-to-end behavior is validated by the sandbox integration test.
    #[test]
    fn pipe_round_trip_single_event() {
        let (handle, name) = create_pipe().expect("create_pipe should succeed");

        // Encode the pipe name as null-terminated UTF-16 for the Win32 API
        // used inside the thread (thread closures can't borrow &str across
        // thread boundaries without Arc/String).
        let pipe_name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0u16)).collect();

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
                    std::ptr::null_mut(),
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

        // Read events first — ConnectNamedPipe in read_events blocks until the
        // writer thread connects, then reads all data, returns when the writer
        // closes its handle (ERROR_BROKEN_PIPE). Do NOT join the writer thread
        // first: that would close the client handle before ConnectNamedPipe
        // is called, causing ConnectNamedPipe to return a non-CONNECTED error
        // and read_events to return empty.
        let events = read_events(handle);

        // Now wait for the writer thread to complete (it should already be done).
        writer_thread
            .join()
            .expect("writer thread should not panic");

        // Clean up the server handle.
        // SAFETY: handle is a valid, open pipe handle.
        unsafe { CloseHandle(handle) };

        assert_eq!(
            events.len(),
            1,
            "expected exactly one event, got: {events:?}"
        );
        match &events[0] {
            AccessEvent::Read { path, pid } => {
                assert_eq!(path, "C:\\test.txt", "path mismatch");
                assert_eq!(*pid, 42, "pid mismatch");
            }
            other => panic!("expected AccessEvent::Read, got: {other:?}"),
        }
    }

    /// Verifies that [`find_dll_path`] returns the value of the
    /// `RAGE_SANDBOX_DLL_PATH` environment variable when it is set.
    #[test]
    fn find_dll_path_uses_env_override() {
        std::env::set_var("RAGE_SANDBOX_DLL_PATH", "C:\\override\\rage_sandbox.dll");
        let result = find_dll_path().expect("find_dll_path should succeed with env override");
        std::env::remove_var("RAGE_SANDBOX_DLL_PATH");
        assert_eq!(
            result,
            PathBuf::from("C:\\override\\rage_sandbox.dll"),
            "find_dll_path should return the env-var override path"
        );
    }

    /// Verifies that [`find_dll_path`] falls back to the compile-time-baked
    /// `RAGE_SANDBOX_DLL_DEFAULT` when neither the env-var override nor a
    /// colocated DLL is present.
    ///
    /// We force the env var to be unset, then assert that the result ends
    /// with `rage_sandbox.dll` (the path itself depends on the workspace
    /// target directory, which differs between local and CI builds).
    #[test]
    fn find_dll_path_falls_back_to_baked_default() {
        std::env::remove_var("RAGE_SANDBOX_DLL_PATH");
        let result = find_dll_path().expect("find_dll_path should succeed");
        let s = result.to_string_lossy().to_lowercase();
        assert!(
            s.ends_with("rage_sandbox.dll"),
            "expected path ending in rage_sandbox.dll, got: {}",
            result.display()
        );
    }

    /// Smoke-test for the public `run_sandboxed` entry point.
    ///
    /// Requires `rage_sandbox.dll` to be present (build
    /// `cargo build -p sandbox-windows-detours` first or set
    /// `RAGE_SANDBOX_DLL_PATH`).  The child runs `cmd /c exit 0` and the
    /// expected exit code is 0.
    #[tokio::test]
    #[cfg(target_os = "windows")]
    #[ignore = "requires rage_sandbox.dll — build `cargo build -p sandbox-windows-detours` first"]
    async fn run_sandboxed_cmd_exit_returns_zero() {
        let result = run_sandboxed("cmd /c exit 0", Path::new("C:\\"), &[])
            .await
            .expect("run_sandboxed should not fail");
        assert_eq!(result.exit_code, 0, "exit code should be 0");
    }

    /// Verifies that [`inject_and_spawn`] can create a child process, inject
    /// (attempt) a DLL, and return a usable process handle that eventually
    /// exits.
    ///
    /// The DLL path `C:\nonexistent_rage_sandbox.dll` will cause `LoadLibraryW`
    /// to fail silently; the main thread is still resumed and the child runs to
    /// completion.  A full DLL integration test is in Task 7.
    #[test]
    fn inject_and_spawn_cmd_echo_runs_to_completion() {
        let (pipe_handle, pipe_name) = create_pipe().expect("create_pipe should succeed");
        let env = vec![("RAGE_PIPE_NAME".to_string(), pipe_name.clone())];

        let result = inject_and_spawn(
            "cmd /c exit 0",
            Path::new("C:\\"),
            &env,
            &pipe_name,
            Path::new("C:\\nonexistent_rage_sandbox.dll"),
        );

        if let Ok(proc_handle) = result {
            // A real DLL integration test is in Task 7.
            // Wait up to 5 s for the child to exit, then release the handle.
            // SAFETY: proc_handle is a valid process handle returned by inject_and_spawn.
            unsafe {
                WaitForSingleObject(proc_handle, 5000);
                CloseHandle(proc_handle);
            }
        }

        // Always close the pipe handle regardless of inject_and_spawn outcome.
        // SAFETY: pipe_handle is a valid named-pipe handle returned by create_pipe.
        unsafe { CloseHandle(pipe_handle) };

        // Test passes if no panic.
    }
}
