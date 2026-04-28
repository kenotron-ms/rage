# Windows Detours Sandbox Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Add a Windows sandbox backend to rage using Microsoft Detours inline patching,
giving rage the same file-access observation capability on Windows that `DYLD_INSERT_LIBRARIES`
provides on macOS and eBPF provides on Linux.

**Architecture:** Two components: a `cdylib` DLL (`sandbox-windows-detours`) that hooks
`CreateFileW`, `NtCreateFile`, and related Win32/NT APIs via `detour-rs` (or its maintained
fork `retour`) and writes binary `AccessEvent` records to a named pipe; and a parent-side
runner in `sandbox/src/windows.rs` that creates the named pipe, spawns the child in
suspended state, injects the DLL via `VirtualAllocEx` + `CreateRemoteThread(LoadLibraryW)`,
resumes the process, and reads the pathset. The shared wire protocol (`pipe_proto.rs`)
compiles and tests on all platforms.

**Tech Stack:** Rust 2021, `retour`/`detour-rs` (inline patching), `windows-sys` (Win32
types), `byteorder` (binary encoding), Win32 named pipes (IPC), `tokio::task::spawn_blocking`
(async wrapper for blocking Win32 calls).

**Platform note:** All Windows-specific code is gated with `#[cfg(target_os = "windows")]`
or `#[cfg(windows)]` throughout. The existing macOS and Linux backends are unchanged.
Integration tests require a Windows environment. `pipe_proto` encode/decode tests run on
all platforms including macOS CI.

---

## Pre-flight: verify dependency versions

Before Task 2 run:
```bash
cargo search retour      # maintained fork of detour; check latest version
cargo search detour-rs   # if retour doesn't exist, check this
cargo search windows-sys # confirm 0.59 is current
cargo search byteorder   # confirm 1.x is current
```

Use the highest stable version found. The design spec lists `detour-rs = "0.13"`,
`windows-sys = "0.59"`, `byteorder = "1"` — verify these are accurate on crates.io
before writing `Cargo.toml`. If the crate is named `retour` rather than `detour-rs`,
use `retour` and adjust all `use` statements accordingly. **`retour` is the actively
maintained fork as of 2025 and is the correct choice if available.**

---

## Codebase conventions (observed from existing code)

- `run_sandboxed` signature across all platform modules:
  `pub async fn run_sandboxed(cmd: &str, cwd: &Path, env: &[(String, String)]) -> Result<RunResult>`
  The `unsupported.rs` stub currently has wrong types (`&str` for cwd, `&[(&str, &str)]` for
  env) — fix this in Task 8 while touching `lib.rs`.
- `AccessEvent { Read { path: String, pid: u32 }, Write { path: String, pid: u32 } }`
- `PathSet::from_events(&[AccessEvent]) -> PathSet` — dedupes and sorts
- `RunResult { exit_code: i32, path_set: PathSet }` — the public return type
- Global state in dylibs: `OnceLock<Mutex<Option<T>>>` pattern (see `sandbox-macos-dylib/src/client.rs`)
- Test style: inline assertions, `tempfile::tempdir()`, `assert!(matches!(...))`, no fixtures
- `#[cfg(target_os = "windows")]` (not `#[cfg(windows)]`) for test gates to stay consistent
  with how macOS tests are gated in this codebase
- Commit style: `feat(scope): description`

---

## Wire protocol reference

```
[op: u8][pid: u32 LE][path_len: u16 LE][path_utf16: path_len × 2 bytes]
```

- `op = 0x01` → `AccessEvent::Read`
- `op = 0x02` → `AccessEvent::Write`
- `path_len` = number of UTF-16 code units (not bytes)
- Minimum record: 7 bytes (header only, zero-length path)
- Named pipe: `\\.\pipe\rage_sandbox_{parent_pid}_{nonce}` where nonce is a random u64
- Child gets the pipe name via env var `RAGE_PIPE_NAME`

---

## Files overview

**New crate:**
```
crates/sandbox-windows-detours/
├── Cargo.toml
└── src/
    ├── lib.rs       — DllMain entry, delegates to hooks::setup_hooks
    ├── hooks.rs     — detour-rs static hooks + setup_hooks()
    └── ipc.rs       — PipeClient: connect + write_event
```

**New modules in `sandbox` crate:**
```
crates/sandbox/src/pipe_proto.rs  — encode_event / decode_event (cross-platform)
crates/sandbox/src/windows.rs    — parent-side: create_pipe, inject_and_spawn, run_sandboxed
```

**Modified:**
```
crates/sandbox/src/lib.rs      — add Windows dispatch + fix server/unsupported gates
crates/sandbox/Cargo.toml      — add byteorder (all), windows-sys (Windows-only)
Cargo.toml (root)              — add sandbox-windows-detours workspace member
```

---

## Task 1: `pipe_proto.rs` — cross-platform binary wire protocol

**Files:**
- Modify: `crates/sandbox/Cargo.toml`
- Create: `crates/sandbox/src/pipe_proto.rs`
- Modify: `crates/sandbox/src/lib.rs` (add module declaration only)

### Step 1: Add `byteorder` to `sandbox/Cargo.toml`

Open `crates/sandbox/Cargo.toml`. Add `byteorder = "1"` to `[dependencies]` (ungated —
`pipe_proto` compiles everywhere):

```toml
[package]
name = "sandbox"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
anyhow = "1"
byteorder = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tempfile = "3"

# Linux eBPF sandbox (Linux-only)
[target.'cfg(target_os = "linux")'.dependencies]
sandbox-linux-ebpf = { path = "../sandbox-linux-ebpf" }

[build-dependencies]

[dev-dependencies]
tempfile = "3"
```

### Step 2: Write the failing test

Create `crates/sandbox/src/pipe_proto.rs` with tests only (no implementation yet):

```rust
//! Binary wire protocol for the Windows named-pipe sandbox IPC.
//!
//! Record layout (little-endian):
//! ```text
//! [op: u8][pid: u32 LE][path_len: u16 LE][path_utf16: path_len × 2 bytes]
//! ```
//! op: 0x01 = Read, 0x02 = Write
//! path_len: number of UTF-16 code units (not bytes)
//!
//! This module is intentionally cross-platform so that wire-format tests run
//! on macOS CI without a Windows environment.

use crate::event::AccessEvent;

pub const OP_READ: u8 = 0x01;
pub const OP_WRITE: u8 = 0x02;
pub const HEADER_LEN: usize = 7; // 1 (op) + 4 (pid) + 2 (path_len)

pub fn encode_event(_event: &AccessEvent, _buf: &mut Vec<u8>) {
    unimplemented!()
}

pub fn decode_event(_buf: &[u8]) -> Option<(AccessEvent, usize)> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AccessEvent;

    #[test]
    fn roundtrip_read_event() {
        let event = AccessEvent::Read {
            path: "C:\\Users\\test\\file.txt".to_string(),
            pid: 1234,
        };
        let mut buf = Vec::new();
        encode_event(&event, &mut buf);
        let (decoded, consumed) = decode_event(&buf).expect("should decode");
        assert_eq!(consumed, buf.len(), "consumed bytes must equal buffer length");
        assert!(
            matches!(&decoded, AccessEvent::Read { path, pid }
                if path == "C:\\Users\\test\\file.txt" && *pid == 1234),
            "got: {:?}",
            decoded
        );
    }

    #[test]
    fn roundtrip_write_event() {
        let event = AccessEvent::Write {
            path: "/tmp/output.txt".to_string(),
            pid: 5678,
        };
        let mut buf = Vec::new();
        encode_event(&event, &mut buf);
        let (decoded, consumed) = decode_event(&buf).expect("should decode");
        assert_eq!(consumed, buf.len());
        assert!(
            matches!(&decoded, AccessEvent::Write { path, pid }
                if path == "/tmp/output.txt" && *pid == 5678),
            "got: {:?}",
            decoded
        );
    }

    #[test]
    fn roundtrip_unicode_path() {
        // Verify non-ASCII paths survive the UTF-16 round-trip.
        let event = AccessEvent::Read {
            path: "C:\\Ür\\ñäme\\文件.txt".to_string(),
            pid: 42,
        };
        let mut buf = Vec::new();
        encode_event(&event, &mut buf);
        let (decoded, _) = decode_event(&buf).expect("should decode unicode");
        assert!(
            matches!(&decoded, AccessEvent::Read { path, .. }
                if path == "C:\\Ür\\ñäme\\文件.txt"),
            "got: {:?}",
            decoded
        );
    }

    #[test]
    fn empty_buffer_returns_none() {
        assert!(decode_event(&[]).is_none());
    }

    #[test]
    fn partial_header_returns_none() {
        // 4 bytes — not enough for even the 7-byte header.
        assert!(decode_event(&[0x01, 0x00, 0x00, 0x00]).is_none());
    }

    #[test]
    fn partial_path_returns_none() {
        // Header claims path_len = 5 UTF-16 words (10 bytes), but only 3 bytes follow.
        let mut buf = vec![OP_READ];
        buf.extend_from_slice(&1234u32.to_le_bytes()); // pid
        buf.extend_from_slice(&5u16.to_le_bytes());    // path_len = 5 words
        buf.extend_from_slice(&[0xAB, 0xCD, 0xEF]);   // only 3 of 10 path bytes
        assert!(decode_event(&buf).is_none());
    }

    #[test]
    fn unknown_op_returns_none() {
        // op = 0xFF is not a valid variant.
        let mut buf = vec![0xFFu8];
        buf.extend_from_slice(&1u32.to_le_bytes());  // pid
        buf.extend_from_slice(&0u16.to_le_bytes());  // path_len = 0
        assert!(decode_event(&buf).is_none());
    }

    #[test]
    fn decode_multiple_sequential_events() {
        let e1 = AccessEvent::Read { path: "/a".into(), pid: 1 };
        let e2 = AccessEvent::Write { path: "/b".into(), pid: 2 };
        let mut buf = Vec::new();
        encode_event(&e1, &mut buf);
        encode_event(&e2, &mut buf);

        let (d1, c1) = decode_event(&buf).unwrap();
        let (d2, c2) = decode_event(&buf[c1..]).unwrap();
        assert_eq!(c1 + c2, buf.len());
        assert!(matches!(d1, AccessEvent::Read { path, .. } if path == "/a"));
        assert!(matches!(d2, AccessEvent::Write { path, .. } if path == "/b"));
    }
}
```

### Step 3: Run the test to verify it fails

```bash
cargo test -p sandbox -- pipe_proto
```

Expected: tests fail with `called unimplemented!()` or similar.

### Step 4: Write the implementation

Replace the stub functions in `crates/sandbox/src/pipe_proto.rs`:

```rust
use byteorder::{ByteOrder, LittleEndian, WriteBytesExt};

/// Encode a single [`AccessEvent`] into `buf` using the binary wire format.
///
/// Appends to `buf` — does not clear it first.
pub fn encode_event(event: &AccessEvent, buf: &mut Vec<u8>) {
    let (op, path, pid) = match event {
        AccessEvent::Read { path, pid } => (OP_READ, path.as_str(), *pid),
        AccessEvent::Write { path, pid } => (OP_WRITE, path.as_str(), *pid),
    };

    let utf16: Vec<u16> = path.encode_utf16().collect();
    let path_len = utf16.len() as u16;

    buf.write_u8(op).expect("Vec write is infallible");
    buf.write_u32::<LittleEndian>(pid).expect("Vec write is infallible");
    buf.write_u16::<LittleEndian>(path_len).expect("Vec write is infallible");
    for word in &utf16 {
        buf.write_u16::<LittleEndian>(*word).expect("Vec write is infallible");
    }
}

/// Try to decode one [`AccessEvent`] from the start of `buf`.
///
/// Returns `Some((event, bytes_consumed))` when a complete record is present,
/// or `None` when `buf` is too short (partial record).
pub fn decode_event(buf: &[u8]) -> Option<(AccessEvent, usize)> {
    if buf.len() < HEADER_LEN {
        return None;
    }

    let op = buf[0];
    let pid = LittleEndian::read_u32(&buf[1..5]);
    let path_words = LittleEndian::read_u16(&buf[5..7]) as usize;
    let path_bytes = path_words * 2;
    let total = HEADER_LEN + path_bytes;

    if buf.len() < total {
        return None;
    }

    let utf16_data = &buf[HEADER_LEN..total];
    let utf16: Vec<u16> = utf16_data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    let path = String::from_utf16_lossy(&utf16).into_owned();

    let event = match op {
        OP_READ => AccessEvent::Read { path, pid },
        OP_WRITE => AccessEvent::Write { path, pid },
        _ => return None,
    };

    Some((event, total))
}
```

### Step 5: Add the module declaration to `lib.rs`

Open `crates/sandbox/src/lib.rs`. Add ONE line after the existing `pub mod event;` line:

```rust
pub mod event;
pub mod pipe_proto;   // ← add this line (no cfg gate — cross-platform)
pub mod mock;
```

### Step 6: Run the tests to verify they pass

```bash
cargo test -p sandbox -- pipe_proto
```

Expected output: 8 tests, all PASS.

Also verify macOS still compiles cleanly:
```bash
cargo build -p sandbox
```

Expected: zero errors, zero new warnings.

### Step 7: Commit

```bash
git add crates/sandbox/Cargo.toml crates/sandbox/src/pipe_proto.rs crates/sandbox/src/lib.rs
git commit -m "feat(sandbox): binary pipe protocol encode/decode"
```

---

## Task 2: `sandbox-windows-detours` crate scaffold + `ipc.rs`

**Files:**
- Create: `crates/sandbox-windows-detours/Cargo.toml`
- Create: `crates/sandbox-windows-detours/src/lib.rs` (placeholder)
- Create: `crates/sandbox-windows-detours/src/ipc.rs`
- Modify: `Cargo.toml` (root workspace)

### Step 1: Write the failing test

First create the directory structure:
```bash
mkdir -p crates/sandbox-windows-detours/src
```

Create `crates/sandbox-windows-detours/src/ipc.rs` with tests only (no implementation):

```rust
//! Named-pipe client for the Windows sandbox DLL.
//!
//! Opens the pipe created by the parent (`sandbox/src/windows.rs`) and writes
//! binary [`AccessEvent`] records encoded by [`sandbox::pipe_proto`].
//!
//! The entire module is Windows-only — it will not compile or link on other
//! platforms.

#![cfg(windows)]

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "windows")]
    fn pipe_client_connect_to_missing_pipe_returns_error() {
        use super::PipeClient;
        let result = PipeClient::connect("\\\\.\\pipe\\rage_sandbox_nonexistent_xyzzy");
        assert!(result.is_err(), "connecting to a non-existent pipe should fail");
    }
}
```

Create a placeholder `crates/sandbox-windows-detours/src/lib.rs`:

```rust
//! Windows sandbox DLL — hooks file-system APIs and reports accesses to the
//! rage parent process via a named pipe.
//!
//! Injected by the parent via `VirtualAllocEx` + `CreateRemoteThread(LoadLibraryW)`.

#![cfg(windows)]

mod hooks;
mod ipc;
```

### Step 2: Create `crates/sandbox-windows-detours/Cargo.toml`

> **First, run `cargo search retour` and `cargo search detour-rs` to confirm the
> correct crate name and latest stable version.** Substitute the confirmed name
> and version below. The maintained fork as of 2025 is most likely `retour`.

```toml
[package]
name = "sandbox-windows-detours"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

# Override workspace-wide `unsafe_code = "forbid"` — this crate requires unsafe
# for raw Win32 FFI and detour inline patching.
[lints.rust]
unsafe_code = "allow"

[lints.clippy]
all = { level = "warn", priority = -1 }

[lib]
name = "rage_sandbox"
crate-type = ["cdylib"]

[target.'cfg(windows)'.dependencies]
# Maintained fork of the `detour` crate — verify name/version on crates.io first.
retour = { version = "0.3", features = ["static-detour"] }
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_Pipes",
    "Win32_System_SystemServices",
    "Win32_System_Threading",
] }
sandbox = { path = "../sandbox" }
```

**Note on `sandbox` as a dependency:** The DLL depends on `sandbox` only to reuse
`AccessEvent` and `pipe_proto::encode_event`. Tokio is pulled in transitively but
is never used in the DLL (no runtime is started). This is acceptable for injection
purposes — tokio's runtime is only started if `tokio::runtime::Runtime::new()` is
called, which this crate never does.

### Step 3: Add to workspace `Cargo.toml` (root)

Open `/Users/ken/workspace/ms/rage/Cargo.toml`. Add `"crates/sandbox-windows-detours"`
to the `members` array:

```toml
[workspace]
resolver = "2"
members = [
    "crates/workspace-tools",
    "crates/build-graph",
    "crates/pipeline-config",
    "crates/scheduler",
    "crates/artifact-store",
    "crates/cache",
    "crates/scoping",
    "crates/daemon",
    "crates/cli",
    "crates/sandbox",
    "crates/sandbox-macos-dylib",
    "crates/sandbox-windows-detours",
    "crates/plugin",
    "crates/plugin-typescript",
    "crates/sandbox-linux-ebpf",
    "crates/hub",
    "crates/spoke-client",
]
```

### Step 4: Write the `PipeClient` implementation in `ipc.rs`

Replace the stub in `crates/sandbox-windows-detours/src/ipc.rs` with the full
implementation:

```rust
//! Named-pipe client for the Windows sandbox DLL.

#![cfg(windows)]

use sandbox::event::AccessEvent;
use sandbox::pipe_proto;
use std::io;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GENERIC_WRITE, OPEN_EXISTING,
};

/// Client end of the named pipe connecting the DLL to the rage parent process.
///
/// Constructed once during `DLL_PROCESS_ATTACH` and stored in a
/// `OnceLock<Mutex<PipeClient>>`. All errors in [`write_event`] are silently
/// swallowed so that hooked syscalls are never impacted by IPC failures.
pub struct PipeClient {
    handle: HANDLE,
}

// SAFETY: `HANDLE` is a raw pointer-sized integer.  `PipeClient` is only
// accessed through a `Mutex` in practice.
unsafe impl Send for PipeClient {}

impl PipeClient {
    /// Open the named pipe created by the rage parent.
    ///
    /// `pipe_name` must be a valid Win32 named-pipe path, e.g.
    /// `\\.\pipe\rage_sandbox_1234_5678901234`.
    pub fn connect(pipe_name: &str) -> io::Result<Self> {
        let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                0, // no template file handle
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        Ok(PipeClient { handle })
    }

    /// Encode `event` and write it to the pipe.
    ///
    /// Errors are silently ignored — a failed write must never affect the
    /// behaviour of the process being sandboxed.
    pub fn write_event(&mut self, event: &AccessEvent) {
        let mut buf = Vec::new();
        pipe_proto::encode_event(event, &mut buf);

        let mut bytes_written: u32 = 0;
        unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                buf.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            );
        }
        // Intentionally ignore the return value — the sandbox is best-effort.
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "windows")]
    fn pipe_client_connect_to_missing_pipe_returns_error() {
        let result = PipeClient::connect("\\\\.\\pipe\\rage_sandbox_nonexistent_xyzzy");
        assert!(result.is_err(), "connecting to a non-existent pipe should fail");
    }
}
```

### Step 5: Create a stub `hooks.rs` so the crate compiles

Create `crates/sandbox-windows-detours/src/hooks.rs`:

```rust
//! File-system API hooks using inline patching (detour-rs / retour).

#![cfg(windows)]

use std::io;

/// Attach all file-system hooks and connect to the named pipe.
///
/// Called from `DllMain` on `DLL_PROCESS_ATTACH`. Implementation is completed
/// in Task 3.
pub fn setup_hooks(_pipe_name: &str) -> io::Result<()> {
    // Stub — completed in Task 3.
    Ok(())
}
```

### Step 6: Run to verify it compiles (macOS)

On macOS you can only check that the non-Windows code path compiles. The Windows
crate itself won't build without a Windows target, which is expected:

```bash
cargo check --workspace --exclude sandbox-windows-detours
```

Expected: zero errors, no regressions in existing crates.

```bash
# Verify pipe_proto tests still pass everywhere
cargo test -p sandbox -- pipe_proto
```

Expected: all 8 pipe_proto tests PASS.

### Step 7: Commit

```bash
git add crates/sandbox-windows-detours/ Cargo.toml
git commit -m "feat(sandbox-windows-detours): Cargo.toml + named pipe client"
```

---

## Task 3: `hooks.rs` — detour-rs file-system hooks

**Files:**
- Modify: `crates/sandbox-windows-detours/src/hooks.rs`

This task implements `CreateFileW` and `NtCreateFile` hooks. All other hooks
(`DeleteFileW`, `MoveFileExW`, `NtOpenFile`, etc.) follow the exact same pattern
once these two work.

### Step 1: Write the failing test

Add a test to `hooks.rs` before implementing anything. Open
`crates/sandbox-windows-detours/src/hooks.rs` and append:

```rust
#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "windows")]
    fn setup_hooks_with_bad_pipe_name_returns_error() {
        // When no pipe exists, setup_hooks must propagate the connection error.
        let result = super::setup_hooks("\\\\.\\pipe\\rage_test_no_such_pipe_xyz");
        assert!(result.is_err(), "setup_hooks should fail when the pipe does not exist");
    }
}
```

Run to verify it fails:
```bash
# On Windows only:
cargo test -p sandbox-windows-detours -- hooks::tests::setup_hooks_with_bad_pipe_name_returns_error
```

Expected: test FAILS (stub `setup_hooks` returns `Ok(())` unconditionally).

### Step 2: Write the implementation

Replace the entire contents of `crates/sandbox-windows-detours/src/hooks.rs`:

```rust
//! Inline-patching hooks for Win32 and NT file-system APIs.
//!
//! Uses `retour` (or `detour-rs`) `static_detour!` to intercept calls to
//! `kernel32.CreateFileW` and `ntdll.NtCreateFile`.  Each hook:
//!   1. Extracts the accessed path.
//!   2. Classifies the access (Read vs Write) from the `DesiredAccess` flags.
//!   3. Sends an [`AccessEvent`] to the parent via [`crate::ipc::IPC_CLIENT`].
//!   4. Calls the original function through the trampoline.

#![cfg(windows)]

use crate::ipc::PipeClient;
use retour::static_detour;
use sandbox::event::AccessEvent;
use std::io;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HANDLE, NTSTATUS};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_WRITE_DATA, GENERIC_WRITE,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

// ---------------------------------------------------------------------------
// Global IPC client — set once on DLL_PROCESS_ATTACH, never changed after.
// ---------------------------------------------------------------------------

pub(crate) static IPC_CLIENT: OnceLock<Mutex<PipeClient>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Static detour declarations
// ---------------------------------------------------------------------------

// Win32 layer — kernel32.dll
type CreateFileWFn = unsafe extern "system" fn(
    *const u16, // lpFileName
    u32,        // dwDesiredAccess
    u32,        // dwShareMode
    *const u8,  // lpSecurityAttributes (opaque pointer)
    u32,        // dwCreationDisposition
    u32,        // dwFlagsAndAttributes
    HANDLE,     // hTemplateFile
) -> HANDLE;

static_detour! {
    static HookCreateFileW: unsafe extern "system" fn(
        *const u16, u32, u32, *const u8, u32, u32, HANDLE) -> HANDLE;
}

// NT native layer — ntdll.dll
// NtCreateFile signature (abbreviated — only the fields we use)
type NtCreateFileFn = unsafe extern "system" fn(
    *mut HANDLE,  // FileHandle (out)
    u32,          // DesiredAccess
    *const u8,    // ObjectAttributes (OBJECT_ATTRIBUTES*)
    *mut u8,      // IoStatusBlock
    *const i64,   // AllocationSize
    u32,          // FileAttributes
    u32,          // ShareAccess
    u32,          // CreateDisposition
    u32,          // CreateOptions
    *mut u8,      // EaBuffer
    u32,          // EaLength
) -> NTSTATUS;

static_detour! {
    static HookNtCreateFile: unsafe extern "system" fn(
        *mut HANDLE, u32, *const u8, *mut u8, *const i64, u32, u32, u32, u32, *mut u8, u32
    ) -> NTSTATUS;
}

// ---------------------------------------------------------------------------
// Helper: convert a wide (UTF-16) C-string pointer to a Rust String.
// ---------------------------------------------------------------------------

/// # Safety
/// `ptr` must point to a valid null-terminated UTF-16 sequence.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf16_lossy(slice).into_owned())
}

/// # Safety
/// `oa_ptr` must be a valid pointer to an `OBJECT_ATTRIBUTES` structure whose
/// `ObjectName` field is a valid `UNICODE_STRING` with a non-null `Buffer`.
unsafe fn oa_to_string(oa_ptr: *const u8) -> Option<String> {
    if oa_ptr.is_null() {
        return None;
    }
    // OBJECT_ATTRIBUTES layout (64-bit):
    //   u32 Length           @ offset 0
    //   u32 _pad             @ offset 4  (alignment)
    //   HANDLE RootDirectory @ offset 8
    //   *UNICODE_STRING ObjectName @ offset 16
    //   u32 Attributes       @ offset 24
    //   ...
    // UNICODE_STRING layout:
    //   u16 Length  @ offset 0
    //   u16 MaximumLength @ offset 2
    //   u32 _pad    @ offset 4
    //   *u16 Buffer @ offset 8
    let oa = oa_ptr as *const usize;
    let obj_name_ptr = *oa.add(2) as *const u8; // ObjectName pointer at offset 16
    if obj_name_ptr.is_null() {
        return None;
    }
    let us_len = *(obj_name_ptr as *const u16) as usize; // Length in bytes
    let buf_ptr = *(obj_name_ptr.add(8) as *const *const u16); // Buffer pointer at offset 8
    if buf_ptr.is_null() || us_len == 0 {
        return None;
    }
    let char_count = us_len / 2;
    let slice = std::slice::from_raw_parts(buf_ptr, char_count);
    Some(String::from_utf16_lossy(slice).into_owned())
}

// ---------------------------------------------------------------------------
// Hook implementations
// ---------------------------------------------------------------------------

fn send_access(is_write: bool, path: Option<String>) {
    let path = match path {
        Some(p) => p,
        None => return,
    };
    let pid = unsafe { GetCurrentProcessId() };
    let event = if is_write {
        AccessEvent::Write { path, pid }
    } else {
        AccessEvent::Read { path, pid }
    };
    if let Some(client) = IPC_CLIENT.get() {
        if let Ok(mut guard) = client.lock() {
            guard.write_event(&event);
        }
    }
}

fn hook_create_file_w(
    lp_file_name: *const u16,
    dw_desired_access: u32,
    dw_share_mode: u32,
    lp_security_attributes: *const u8,
    dw_creation_disposition: u32,
    dw_flags_and_attributes: u32,
    h_template_file: HANDLE,
) -> HANDLE {
    let path = unsafe { wide_ptr_to_string(lp_file_name) };
    let is_write =
        (dw_desired_access & GENERIC_WRITE) != 0 || (dw_desired_access & FILE_WRITE_DATA) != 0;
    send_access(is_write, path);

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

fn hook_nt_create_file(
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
    // NT FILE_WRITE_DATA = 0x0002, FILE_APPEND_DATA = 0x0004
    const NT_FILE_WRITE_DATA: u32 = 0x0002;
    const NT_FILE_APPEND_DATA: u32 = 0x0004;

    let path = unsafe { oa_to_string(object_attributes) };
    let is_write =
        (desired_access & NT_FILE_WRITE_DATA) != 0 || (desired_access & NT_FILE_APPEND_DATA) != 0;
    send_access(is_write, path);

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
// Public setup function
// ---------------------------------------------------------------------------

/// Helper: produce a null-terminated wide string from a Rust `&str`.
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Connect to the named pipe and install all hooks.
///
/// Called from `DllMain` during `DLL_PROCESS_ATTACH`.  Errors here mean the
/// sandbox is non-functional for this process, but the process itself must
/// still be allowed to run (DllMain must return TRUE regardless).
pub fn setup_hooks(pipe_name: &str) -> io::Result<()> {
    // 1. Connect to the parent's named pipe.
    let client = PipeClient::connect(pipe_name)?;
    // Ignore the error if another thread already initialised (shouldn't happen,
    // but DllMain is called once per process so this is just defensive).
    let _ = IPC_CLIENT.set(Mutex::new(client));

    unsafe {
        // ── kernel32.CreateFileW ────────────────────────────────────────────
        let k32 = GetModuleHandleW(to_wide_null("kernel32.dll").as_ptr());
        if k32 == 0 {
            return Err(io::Error::last_os_error());
        }
        let create_file_w_ptr =
            GetProcAddress(k32, b"CreateFileW\0".as_ptr())
                .ok_or_else(io::Error::last_os_error)?;
        let create_file_w: CreateFileWFn = std::mem::transmute(create_file_w_ptr);
        HookCreateFileW
            .initialize(create_file_w, hook_create_file_w)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .enable()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        // ── ntdll.NtCreateFile ──────────────────────────────────────────────
        let ntdll = GetModuleHandleW(to_wide_null("ntdll.dll").as_ptr());
        if ntdll == 0 {
            return Err(io::Error::last_os_error());
        }
        let nt_create_file_ptr =
            GetProcAddress(ntdll, b"NtCreateFile\0".as_ptr())
                .ok_or_else(io::Error::last_os_error)?;
        let nt_create_file: NtCreateFileFn = std::mem::transmute(nt_create_file_ptr);
        HookNtCreateFile
            .initialize(nt_create_file, hook_nt_create_file)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .enable()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (Windows-only — hooks require live Win32 DLLs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(target_os = "windows")]
    fn setup_hooks_with_bad_pipe_name_returns_error() {
        let result = super::setup_hooks("\\\\.\\pipe\\rage_test_no_such_pipe_xyz");
        assert!(result.is_err(), "setup_hooks should fail when the pipe does not exist");
    }
}
```

> **API note:** The `retour` crate's `static_detour!` API may differ slightly from what
> is shown. Verify:
> - `HookFoo.initialize(fn_ptr, hook_fn)` returns `Result<&'static StaticDetour<_>, _>`
> - `.enable()` arms the hook
> - `HookFoo.call(...)` invokes the original via the trampoline
>
> If `GetProcAddress` returns `Option<unsafe extern "system" fn()>` rather than a raw
> pointer in the version of `windows-sys` you are using, adjust the transmute target
> accordingly.
>
> The `oa_to_string` function assumes 64-bit layout for `OBJECT_ATTRIBUTES` and
> `UNICODE_STRING`. If targeting 32-bit Windows, offsets will differ. This plan
> targets 64-bit only.

### Step 3: Run the test to verify it passes (Windows only)

```bash
# On Windows:
cargo test -p sandbox-windows-detours -- hooks::tests
```

Expected: `setup_hooks_with_bad_pipe_name_returns_error` PASSES.

### Step 4: Verify macOS is unaffected

```bash
cargo check --workspace --exclude sandbox-windows-detours
cargo test -p sandbox -- pipe_proto
```

Expected: zero errors, 8 pipe_proto tests PASS.

### Step 5: Commit

```bash
git add crates/sandbox-windows-detours/src/hooks.rs
git commit -m "feat(sandbox-windows-detours): CreateFileW and NtCreateFile hooks"
```

---

## Task 4: `lib.rs` — DllMain entry point

**Files:**
- Modify: `crates/sandbox-windows-detours/src/lib.rs`

### Step 1: Write the failing test

Add a test to the placeholder `lib.rs` before completing the implementation:

Open `crates/sandbox-windows-detours/src/lib.rs` and append:

```rust
#[cfg(test)]
mod tests {
    /// Verify that calling DllMain with no RAGE_PIPE_NAME env var does not panic.
    /// This is a compile-and-execute smoke test.
    #[test]
    #[cfg(target_os = "windows")]
    fn dll_main_attach_without_env_var_does_not_panic() {
        use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
        // Remove the env var if it happens to be set in this test environment.
        std::env::remove_var("RAGE_PIPE_NAME");
        // Should return TRUE (1) without panicking.
        let result = unsafe { super::DllMain(0, DLL_PROCESS_ATTACH, std::ptr::null_mut()) };
        assert_eq!(result, 1, "DllMain must return TRUE");
    }
}
```

Run to verify it fails:
```bash
# On Windows:
cargo test -p sandbox-windows-detours -- tests::dll_main_attach_without_env_var_does_not_panic
```

Expected: compile error — `DllMain` not yet defined.

### Step 2: Write the implementation

Replace the full contents of `crates/sandbox-windows-detours/src/lib.rs`:

```rust
//! Windows sandbox DLL — hooks file-system APIs and reports accesses to the
//! rage parent process via a named pipe.
//!
//! ## Injection
//! The rage parent injects this DLL into the child process by calling
//! `CreateRemoteThread(child, LoadLibraryW, path_to_dll)` after suspending
//! the process at startup (see `sandbox/src/windows.rs`).
//!
//! ## Hooks
//! On `DLL_PROCESS_ATTACH`, [`DllMain`] reads `RAGE_PIPE_NAME` from the
//! child's environment (set by the parent before spawning), connects to the
//! named pipe, and installs inline hooks via `retour` / `detour-rs`.
//!
//! ## Safety
//! `unsafe_code = "allow"` is set in `Cargo.toml` for this crate.  All unsafe
//! blocks are confined to `hooks.rs` and `ipc.rs` where Win32 FFI is
//! unavoidable.

#![cfg(windows)]

mod hooks;
mod ipc;

use windows_sys::Win32::Foundation::{BOOL, HINSTANCE};
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// Windows DLL entry point.
///
/// Called by the loader on attach and detach.  On `DLL_PROCESS_ATTACH`:
/// reads `RAGE_PIPE_NAME`, connects to the named pipe, and installs hooks.
/// Errors are silenced — the DLL must never prevent the child from running.
///
/// # Safety
/// This function is called by the Windows loader with loader-lock held.
/// We must not load additional DLLs, call `LoadLibrary`, or perform complex
/// synchronisation.  Connecting a named pipe (`CreateFile`) and installing
/// inline patches (memory writes to existing mapped pages) are safe under
/// loader lock in practice.
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
                // Silently ignore setup errors — the child must still run.
                let _ = hooks::setup_hooks(&pipe_name);
            }
            1 // TRUE — DLL load succeeded
        }
        DLL_PROCESS_DETACH => {
            // Hooks are automatically removed when the DLL is unloaded because
            // `retour` / `detour-rs` restores the original bytes on drop.
            1
        }
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
        let result = unsafe { super::DllMain(0, DLL_PROCESS_ATTACH, std::ptr::null_mut()) };
        assert_eq!(result, 1, "DllMain must return TRUE");
    }
}
```

### Step 3: Run to verify the test passes (Windows only)

```bash
# On Windows:
cargo test -p sandbox-windows-detours -- tests::dll_main_attach_without_env_var_does_not_panic
```

Expected: PASS.

### Step 4: Verify macOS unaffected

```bash
cargo check --workspace --exclude sandbox-windows-detours
```

Expected: zero errors.

### Step 5: Commit

```bash
git add crates/sandbox-windows-detours/src/lib.rs
git commit -m "feat(sandbox-windows-detours): DllMain hooks on DLL_PROCESS_ATTACH"
```

---

## Task 5: `sandbox/src/windows.rs` — named pipe server

**Files:**
- Modify: `crates/sandbox/Cargo.toml` (add `windows-sys` conditional dep)
- Create: `crates/sandbox/src/windows.rs`

### Step 1: Add `windows-sys` to `sandbox/Cargo.toml`

Open `crates/sandbox/Cargo.toml`. Add a Windows-conditional dependency block after the
Linux one:

```toml
# Windows Detours sandbox
[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
    "Win32_System_IO",
    "Win32_System_LibraryLoader",
    "Win32_System_Memory",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```

### Step 2: Write the failing test

Create `crates/sandbox/src/windows.rs` with tests only:

```rust
//! Windows-specific sandbox runner.
//!
//! Parent side of the Windows Detours sandbox.  Creates a named pipe, injects
//! `rage_sandbox.dll` into the child process, and reads [`AccessEvent`] records
//! from the pipe until the child exits.

#![cfg(target_os = "windows")]

use crate::event::{AccessEvent, PathSet, RunResult};
use crate::pipe_proto;
use anyhow::Result;
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[cfg(target_os = "windows")]
    fn create_pipe_returns_valid_handle_and_name() {
        let (handle, name) = create_pipe().expect("create_pipe should succeed");
        assert!(name.contains("rage_sandbox_"), "pipe name should contain rage_sandbox_");
        assert!(name.starts_with("\\\\.\\pipe\\"), "pipe name should start with \\\\.\\ ");
        // Clean up
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn pipe_round_trip_single_event() {
        use std::thread;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GENERIC_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::Storage::FileSystem::WriteFile;

        let (server_handle, pipe_name) = create_pipe().expect("create_pipe");

        // Writer thread: connect as client and write one Read event.
        let pipe_name_clone = pipe_name.clone();
        let writer = thread::spawn(move || {
            let wide: Vec<u16> = pipe_name_clone.encode_utf16().chain(std::iter::once(0)).collect();
            let h = unsafe {
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
            assert_ne!(h, windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE);

            let event = AccessEvent::Read { path: "C:\\test.txt".into(), pid: 42 };
            let mut buf = Vec::new();
            crate::pipe_proto::encode_event(&event, &mut buf);
            let mut written = 0u32;
            unsafe { WriteFile(h, buf.as_ptr(), buf.len() as u32, &mut written, std::ptr::null_mut()) };
            unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
        });

        writer.join().expect("writer thread should not panic");

        let events = read_events(server_handle);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], AccessEvent::Read { path, pid }
            if path == "C:\\test.txt" && *pid == 42),
            "got: {:?}", events[0]);
    }
}
```

Run to verify it fails:
```bash
# On Windows:
cargo test -p sandbox -- windows::tests
```

Expected: compile error — `create_pipe` and `read_events` not yet defined.

### Step 3: Write the implementation

Add the implementation before the `#[cfg(test)]` block in `windows.rs`:

```rust
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, FILE_FLAG_OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_INBOUND, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

/// Create the server end of the named pipe that the DLL child will write to.
///
/// Returns `(handle, pipe_name)`.  The pipe name is formatted as
/// `\\.\pipe\rage_sandbox_{parent_pid}_{nonce}` where `nonce` is a random u64.
///
/// The caller is responsible for closing the handle.
fn create_pipe() -> std::io::Result<(HANDLE, String)> {
    let pid = unsafe { GetCurrentProcessId() };
    // Use a simple monotonic nonce derived from the current time in nanos.
    // rand is not a dependency here; SystemTime provides sufficient uniqueness.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        ^ (pid as u64 * 0x517CC1B727220A95); // cheap spread

    let name = format!("\\\\.\\pipe\\rage_sandbox_{}_{}", pid, nonce);
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND,                      // server reads
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,      // max instances — exactly one child
            0,      // out-buffer size (not used for INBOUND)
            65536,  // in-buffer size
            0,      // default timeout
            std::ptr::null(), // default security
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }

    Ok((handle, name))
}

/// Block until the child connects to `pipe`, then drain all [`AccessEvent`]s
/// until the pipe is broken (child exited or closed its end).
fn read_events(pipe: HANDLE) -> Vec<AccessEvent> {
    // Wait for the client (child DLL) to connect.
    let connect_result = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) };
    let last_error = unsafe {
        windows_sys::Win32::Foundation::GetLastError()
    };
    // `ERROR_PIPE_CONNECTED` means the client connected before we called
    // ConnectNamedPipe — that's fine.
    if connect_result == 0 && last_error != ERROR_PIPE_CONNECTED {
        return Vec::new();
    }

    let mut events = Vec::new();
    let mut raw_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut read_scratch = [0u8; 4096];

    loop {
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                pipe,
                read_scratch.as_mut_ptr(),
                read_scratch.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };

        if ok == 0 || bytes_read == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA || bytes_read == 0 {
                break; // Pipe closed — child exited.
            }
            break; // Any other error — stop reading.
        }

        raw_buf.extend_from_slice(&read_scratch[..bytes_read as usize]);

        // Drain all complete records from raw_buf.
        let mut offset = 0;
        while let Some((event, consumed)) = pipe_proto::decode_event(&raw_buf[offset..]) {
            events.push(event);
            offset += consumed;
        }
        raw_buf.drain(..offset);
    }

    // Drain any remaining complete records after the pipe closed.
    let mut offset = 0;
    while let Some((event, consumed)) = pipe_proto::decode_event(&raw_buf[offset..]) {
        events.push(event);
        offset += consumed;
    }

    events
}
```

### Step 4: Run to verify tests pass (Windows only)

```bash
cargo test -p sandbox -- windows::tests
```

Expected: both `create_pipe_*` and `pipe_round_trip_single_event` PASS.

### Step 5: Verify macOS unaffected

```bash
cargo build -p sandbox
cargo test -p sandbox -- pipe_proto
```

Expected: zero errors, 8 pipe_proto tests PASS.

### Step 6: Commit

```bash
git add crates/sandbox/Cargo.toml crates/sandbox/src/windows.rs
git commit -m "feat(sandbox): Windows named pipe server"
```

---

## Task 6: `sandbox/src/windows.rs` — process injection

**Files:**
- Modify: `crates/sandbox/src/windows.rs` (add injection code)

All additions go in the same file, before the `#[cfg(test)]` block.

### Step 1: Write the failing test

Append the following test to the `#[cfg(test)] mod tests` block in `windows.rs`:

```rust
    #[test]
    #[cfg(target_os = "windows")]
    fn find_dll_path_uses_env_override() {
        std::env::set_var("RAGE_SANDBOX_DLL_PATH", "C:\\override\\rage_sandbox.dll");
        let path = find_dll_path().expect("find_dll_path");
        assert_eq!(path, PathBuf::from("C:\\override\\rage_sandbox.dll"));
        std::env::remove_var("RAGE_SANDBOX_DLL_PATH");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn inject_and_spawn_cmd_echo_runs_to_completion() {
        // Requires rage_sandbox.dll to NOT exist (we pass a dummy path that
        // we expect LoadLibraryW to fail on — the test only verifies that the
        // process itself starts and we get a handle back without a panic).
        // A real integration test with a built DLL is done in Task 7.
        use std::path::Path;
        // Use a dummy DLL path that won't exist — the remote thread will fail
        // to load it, but the main thread will still run cmd /c exit 0.
        let (pipe_handle, pipe_name) = create_pipe().expect("pipe");
        let env: Vec<(String, String)> = vec![
            ("RAGE_PIPE_NAME".into(), pipe_name.clone()),
        ];
        let result = inject_and_spawn(
            "cmd /c exit 0",
            Path::new("C:\\"),
            &env,
            &pipe_name,
            Path::new("C:\\nonexistent_rage_sandbox.dll"),
        );
        // We expect Ok(handle) — the process should start even if DLL load fails.
        // (LoadLibrary failure in the remote thread is non-fatal to the process.)
        if let Ok(proc_handle) = result {
            unsafe {
                windows_sys::Win32::System::Threading::WaitForSingleObject(
                    proc_handle,
                    5000, // 5 s timeout
                );
                windows_sys::Win32::Foundation::CloseHandle(proc_handle);
            }
        }
        unsafe { windows_sys::Win32::Foundation::CloseHandle(pipe_handle) };
        // Test passes if we reached this point without a panic.
    }
```

Run to verify it fails:
```bash
# On Windows:
cargo test -p sandbox -- windows::tests::find_dll_path_uses_env_override
```

Expected: compile error — `find_dll_path` not yet defined.

### Step 2: Write the implementation

Add to `crates/sandbox/src/windows.rs`, after `read_events` and before the test block:

```rust
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, WriteProcessMemory, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetExitCodeProcess, ResumeThread, WaitForSingleObject,
    CREATE_SUSPENDED, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
};

/// Locate `rage_sandbox.dll` for injection.
///
/// Resolution order:
/// 1. `RAGE_SANDBOX_DLL_PATH` environment variable.
/// 2. Directory containing the current executable + `rage_sandbox.dll`.
pub fn find_dll_path() -> std::io::Result<PathBuf> {
    if let Ok(val) = std::env::var("RAGE_SANDBOX_DLL_PATH") {
        return Ok(PathBuf::from(val));
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no exe parent dir"))?;
    Ok(dir.join("rage_sandbox.dll"))
}

/// Spawn `cmd` in a suspended process, inject `dll_path` via
/// `CreateRemoteThread(LoadLibraryW)`, then resume the main thread.
///
/// Returns the process handle (caller must `CloseHandle` when done).
///
/// `env` must include `("RAGE_PIPE_NAME", pipe_name)` — set by the caller.
pub fn inject_and_spawn(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    _pipe_name: &str,
    dll_path: &Path,
) -> std::io::Result<HANDLE> {
    // Build the command line wide string.
    let cmd_wide: Vec<u16> = format!("cmd /c {}", cmd)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // Build the environment block (null-separated, double-null terminated).
    let env_block: Vec<u16> = {
        let mut v: Vec<u16> = Vec::new();
        for (k, val) in env {
            for c in format!("{}={}", k, val).encode_utf16() {
                v.push(c);
            }
            v.push(0);
        }
        v.push(0); // double-null terminator
        v
    };

    let cwd_wide: Vec<u16> = cwd
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // 1. Create child process in suspended state with Unicode environment.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),          // application name (use command line)
            cmd_wide.as_ptr() as *mut u16,
            std::ptr::null(),          // process security
            std::ptr::null(),          // thread security
            0,                         // don't inherit handles
            CREATE_SUSPENDED | 0x0400, // 0x0400 = CREATE_UNICODE_ENVIRONMENT
            env_block.as_ptr() as *const _,
            cwd_wide.as_ptr(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    let proc_handle = pi.hProcess;
    let thread_handle = pi.hThread;

    // 2. Allocate memory in child for the DLL path string.
    let dll_wide: Vec<u16> = dll_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let dll_bytes = dll_wide.len() * 2;

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
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(proc_handle, 1);
            CloseHandle(proc_handle);
            CloseHandle(thread_handle);
        }
        return Err(std::io::Error::last_os_error());
    }

    // 3. Write the DLL path into child memory.
    let mut written = 0usize;
    unsafe {
        WriteProcessMemory(
            proc_handle,
            remote_buf,
            dll_wide.as_ptr() as *const _,
            dll_bytes,
            &mut written,
        )
    };

    // 4. Get LoadLibraryW address from kernel32 (same VA in all processes on
    //    the same OS due to ASLR-invariance of kernel32 across processes).
    let kernel32_wide: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    let k32 = unsafe { GetModuleHandleW(kernel32_wide.as_ptr()) };
    let load_lib = unsafe { GetProcAddress(k32, b"LoadLibraryW\0".as_ptr()) };

    // 5. Spawn remote thread that calls LoadLibraryW(dll_path_in_child).
    let mut remote_tid = 0u32;
    let remote_thread = unsafe {
        CreateRemoteThread(
            proc_handle,
            std::ptr::null(),
            0,
            Some(std::mem::transmute(load_lib)),
            remote_buf,
            0,
            &mut remote_tid,
        )
    };

    // 6. Wait for LoadLibrary to finish (DLL init completes).
    if remote_thread != 0 {
        unsafe {
            WaitForSingleObject(remote_thread, 5000); // 5 s max
            CloseHandle(remote_thread);
        }
    }

    // 7. Free the remote buffer and resume the main thread.
    unsafe {
        VirtualFreeEx(proc_handle, remote_buf, 0, MEM_RELEASE);
        ResumeThread(thread_handle);
        CloseHandle(thread_handle);
    }

    Ok(proc_handle)
}
```

### Step 3: Run the tests to verify they pass (Windows only)

```bash
cargo test -p sandbox -- windows::tests
```

Expected: `find_dll_path_uses_env_override` PASSES,
`inject_and_spawn_cmd_echo_runs_to_completion` PASSES.

### Step 4: Verify macOS unaffected

```bash
cargo build -p sandbox
```

Expected: zero errors.

### Step 5: Commit

```bash
git add crates/sandbox/src/windows.rs
git commit -m "feat(sandbox): DLL injection via suspended process + remote thread"
```

---

## Task 7: `run_sandboxed` for Windows

**Files:**
- Modify: `crates/sandbox/src/windows.rs` (add public async function)

### Step 1: Write the failing test

Append to the `#[cfg(test)] mod tests` block in `windows.rs`:

```rust
    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn run_sandboxed_cmd_exit_returns_zero() {
        // Requires `rage_sandbox.dll` to be built alongside this test.
        // If the DLL is not present, the sandbox records no paths but the
        // process still runs.  This test only checks exit code.
        let result = run_sandboxed("cmd /c exit 0", Path::new("C:\\"), &[])
            .await
            .expect("run_sandboxed should not fail");
        assert_eq!(result.exit_code, 0, "exit code should be 0");
    }
```

Run to verify it fails:
```bash
# On Windows:
cargo test -p sandbox -- windows::tests::run_sandboxed_cmd_exit_returns_zero
```

Expected: compile error — `run_sandboxed` not yet defined.

### Step 2: Write the implementation

Add the following to `crates/sandbox/src/windows.rs` BEFORE the `#[cfg(test)]` block:

```rust
/// Run `cmd` inside the Windows Detours sandbox.
///
/// 1. Creates a named pipe that the injected DLL will write [`AccessEvent`]s to.
/// 2. Locates `rage_sandbox.dll` (via `RAGE_SANDBOX_DLL_PATH` or binary dir).
/// 3. Spawns the child in suspended state, injects the DLL, resumes the process.
/// 4. Waits for the child to exit, then drains the pipe.
/// 5. Returns a [`RunResult`] with the exit code and observed [`PathSet`].
///
/// If the DLL is not found, the child is still run but no paths are recorded
/// (the process is not sandboxed).
pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<RunResult> {
    let dll_path = find_dll_path()
        .unwrap_or_else(|_| PathBuf::from("rage_sandbox.dll"));

    if !dll_path.exists() {
        anyhow::bail!(
            "sandbox DLL not found at `{}`: \
             build `cargo build -p sandbox-windows-detours` first or set \
             RAGE_SANDBOX_DLL_PATH",
            dll_path.display()
        );
    }

    let (pipe_handle, pipe_name) = create_pipe()
        .map_err(|e| anyhow::anyhow!("create named pipe: {}", e))?;

    // Extend the caller's env with the pipe name.
    let mut full_env: Vec<(String, String)> = env.to_vec();
    full_env.push(("RAGE_PIPE_NAME".to_string(), pipe_name.clone()));

    let proc_handle = inject_and_spawn(cmd, cwd, &full_env, &pipe_name, &dll_path)
        .map_err(|e| anyhow::anyhow!("inject_and_spawn: {}", e))?;

    // Run the blocking wait + pipe drain on a thread-pool thread so we don't
    // block the tokio executor.
    let (events, exit_code) = tokio::task::spawn_blocking(move || {
        // Drain the pipe (blocks until the client disconnects).
        let events = read_events(pipe_handle);

        // Wait for the process to exit (should already be done by now, but
        // guard against edge cases where the pipe closed before the process).
        unsafe { WaitForSingleObject(proc_handle, INFINITE) };

        let mut raw_exit: u32 = 0;
        unsafe { GetExitCodeProcess(proc_handle, &mut raw_exit) };
        let exit_code = raw_exit as i32;

        unsafe {
            CloseHandle(pipe_handle);
            CloseHandle(proc_handle);
        }

        (events, exit_code)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking join: {}", e))?;

    Ok(RunResult {
        exit_code,
        path_set: PathSet::from_events(&events),
    })
}
```

### Step 3: Run the test to verify it passes (Windows only)

```bash
# On Windows (requires rage_sandbox.dll to exist in binary dir):
cargo build -p sandbox-windows-detours
cargo test -p sandbox -- windows::tests::run_sandboxed_cmd_exit_returns_zero
```

Expected: PASS with `exit_code == 0`.

### Step 4: Verify macOS unaffected

```bash
cargo build -p sandbox
cargo test -p sandbox -- pipe_proto
```

Expected: zero errors, 8 pipe_proto tests PASS.

### Step 5: Commit

```bash
git add crates/sandbox/src/windows.rs
git commit -m "feat(sandbox): run_sandboxed for Windows via Detours DLL injection"
```

---

## Task 8: Wire into `sandbox/src/lib.rs`

**Files:**
- Modify: `crates/sandbox/src/lib.rs`
- Modify: `crates/sandbox/src/unsupported.rs` (fix signature to match other platforms)

### Step 1: Write the failing test

This is a compilation test — the plan adds conditional module exports. Verify current
state first:

```bash
cargo build -p sandbox  # on macOS: should already pass
```

### Step 2: Fix `unsupported.rs` signature

The `unsupported.rs` stub currently has wrong parameter types (`_cwd: &str` and
`&[(&str, &str)]`) that don't match the macOS/Linux implementations. Fix it.

Open `crates/sandbox/src/unsupported.rs` and replace the entire file:

```rust
use anyhow::bail;
use std::path::Path;

use crate::event::RunResult;

/// Platform stub — returns an error on unsupported operating systems.
pub async fn run_sandboxed(
    _cmd: &str,
    _cwd: &Path,
    _env: &[(String, String)],
) -> anyhow::Result<RunResult> {
    bail!("rage sandbox is not supported on this platform")
}
```

### Step 3: Update `lib.rs`

Open `crates/sandbox/src/lib.rs` and replace the entire file:

```rust
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

/// Binary wire protocol for the Windows named-pipe IPC.
///
/// Compiled on all platforms so that round-trip tests run on macOS CI.
pub mod pipe_proto;

pub mod mock;

/// Unix-domain socket event server (macOS sandbox).
///
/// Only available on Unix — this module uses `tokio::net::UnixListener` which
/// is not available on Windows.
#[cfg(unix)]
pub mod server;

/// Platform implementations — exactly one `run_sandboxed` is active.
///
/// macOS   → DYLD_INSERT_LIBRARIES sandbox
/// Linux   → eBPF tracepoint sandbox
/// Windows → Microsoft Detours DLL injection
/// Other   → unsupported stub (returns error)
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
```

> **Why `#[cfg(unix)]` for `server`?**
> `server.rs` uses `tokio::net::UnixListener` which is only available on Unix.
> Leaving it ungated would break `cargo check -p sandbox` on Windows. The gate is
> `unix` (covers macOS + Linux) rather than `target_os = "macos"` because Linux
> may use `server.rs` in future.

### Step 4: Run to verify (macOS)

```bash
cargo build -p sandbox
cargo test -p sandbox -- pipe_proto
cargo test -p sandbox
```

Expected: zero errors, all existing tests PASS, 8 pipe_proto tests PASS.

Also run a broader workspace check to catch any regressions:

```bash
cargo check --workspace --exclude sandbox-windows-detours
```

Expected: zero errors, zero new warnings.

### Step 5: Commit

```bash
git add crates/sandbox/src/lib.rs crates/sandbox/src/unsupported.rs
git commit -m "feat(sandbox): wire Windows Detours backend into dispatch"
```

---

## Task 9: Cross-platform verification

**Files:**
- No new files — this task is verification + documentation of the build command.

### Step 1: macOS full test suite

```bash
cargo test --workspace --exclude sandbox-windows-detours
```

Expected: all tests PASS. Pay attention to any new failures in `sandbox` tests that
were previously passing.

### Step 2: Verify `pipe_proto` tests run on macOS

```bash
cargo test -p sandbox -- pipe_proto --nocapture
```

Expected output (8 tests):
```
test event::tests::pipe_proto::roundtrip_read_event ... ok
test event::tests::pipe_proto::roundtrip_write_event ... ok
test event::tests::pipe_proto::roundtrip_unicode_path ... ok
test event::tests::pipe_proto::empty_buffer_returns_none ... ok
test event::tests::pipe_proto::partial_header_returns_none ... ok
test event::tests::pipe_proto::partial_path_returns_none ... ok
test event::tests::pipe_proto::unknown_op_returns_none ... ok
test event::tests::pipe_proto::decode_multiple_sequential_events ... ok
```

### Step 3: Verify workspace is consistent

```bash
cargo check --workspace --exclude sandbox-windows-detours
```

Expected: zero errors.

### Step 4: On Windows — full DLL integration smoke test

```bash
# Build the DLL
cargo build -p sandbox-windows-detours

# Build sandbox tests
cargo test -p sandbox -- windows --nocapture

# Expected (requires DLL in build output dir):
# test windows::tests::create_pipe_returns_valid_handle_and_name ... ok
# test windows::tests::pipe_round_trip_single_event ... ok
# test windows::tests::find_dll_path_uses_env_override ... ok
# test windows::tests::inject_and_spawn_cmd_echo_runs_to_completion ... ok
# test windows::tests::run_sandboxed_cmd_exit_returns_zero ... ok
```

### Step 5: Commit

```bash
git add .
git commit -m "feat: add sandbox-windows-detours to workspace, cross-platform pipe_proto tests pass"
```

---

## Appendix: Windows-specific build notes

### DLL output location

After `cargo build -p sandbox-windows-detours`, the DLL is produced as:
```
target/debug/rage_sandbox.dll          (debug)
target/release/rage_sandbox.dll        (release)
```

The parent (`run_sandboxed` in `windows.rs`) resolves the DLL relative to the rage
binary via `std::env::current_exe()?.parent()? / "rage_sandbox.dll"`. When running
integration tests, set `RAGE_SANDBOX_DLL_PATH` to the absolute path:

```powershell
$env:RAGE_SANDBOX_DLL_PATH = ".\target\debug\rage_sandbox.dll"
cargo test -p sandbox -- windows
```

### Target triples

- `x86_64-pc-windows-msvc` — MSVC toolchain (recommended, matches production)
- `x86_64-pc-windows-gnu` — MinGW toolchain (alternative, but `detour-rs`/`retour`
  may have issues with GNU exceptions — prefer MSVC)

### `retour` vs `detour-rs` — API quick reference

The `retour` crate (maintained fork) exposes:
```rust
use retour::static_detour;

static_detour! {
    static MyHook: unsafe extern "system" fn(ArgTypes) -> RetType;
}

// Initialize + enable (unsafe block required):
MyHook.initialize(original_fn_ptr, hook_fn_rust)?  // Returns &'static StaticDetour<T>
      .enable()?;

// In hook body, call original via trampoline:
MyHook.call(arg1, arg2, ...)
```

If the crate is named `detour` (not `retour`), the API is identical but the import
is `use detour::static_detour`.

### Loader-lock caution

`DllMain` on `DLL_PROCESS_ATTACH` is called with the loader lock held. Opening a
named pipe (`CreateFile`) and patching memory (`retour`/`detour-rs`) are generally
safe here in practice, but are technically loader-lock-restricted. If stability
issues arise on specific Windows builds, consider spawning an init thread from
`DllMain` (call `CreateThread`, do the work there, return immediately from
`DllMain`). This would require making `IPC_CLIENT` and hook setup thread-safe
across a deferred init, which adds complexity — start with the direct approach
and add the thread only if needed.
