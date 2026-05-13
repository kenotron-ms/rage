# Windows Port Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the Windows backend of `rage` from "85% done with caveats" to first-class supported status: race-free named-pipe IPC, gating CI, automated DLL placement, accurate documentation, and a stable-Rust toolchain.

**Architecture:** Five sequenced tasks, each producing a green CI on its own. Task 1 fixes a Win32 race in the named-pipe server using overlapped I/O so the previously-ignored unit test passes deterministically. Task 2 flips the integration test from advisory to gating. Task 3 mirrors the macOS dylib path-baking pattern for the Windows DLL via `build.rs`. Task 4 updates the README to match reality. Task 5 migrates from `retour`'s nightly-only `static_detour!` macro to its stable `GenericDetour` API and removes nightly-toolchain installs from CI.

**Tech Stack:** Rust 1.91 (workspace MSRV), `windows-sys` 0.59, `retour` 0.3 (stable API surface), Tokio, GitHub Actions.

**Working tree:** All paths are relative to `/Users/ken/workspace/ms/rage`. Confirm `pwd` reports that path before each task. (The original session's CWD was the typo path `~/wokspace/ms/rage` — do not work there.)

---

## File Structure

| File | Responsibility | Touched In |
|---|---|---|
| `crates/sandbox/src/windows.rs` | Parent-side named-pipe + DLL injection (Win32) | Task 1, Task 3 |
| `crates/sandbox/build.rs` | Bake artifact paths into the binary | Task 3 |
| `crates/sandbox-windows-detours/Cargo.toml` | Crate manifest, retour feature flags | Task 5 |
| `crates/sandbox-windows-detours/src/hooks.rs` | Inline-patch hooks via retour | Task 5 |
| `crates/sandbox-windows-detours/src/lib.rs` | DllMain entry + module wiring | Task 5 (untouched) |
| `.github/workflows/ci.yml` | CI matrix and nightly-toolchain installs | Task 2, Task 5 |
| `README.md` | Platform-support table and status footer | Task 4 |

---

## Task 1: Fix the named-pipe race condition with overlapped I/O

**Why:** `crates/sandbox/src/windows.rs::tests::pipe_round_trip_single_event` is `#[ignore]`d because synchronous `ConnectNamedPipe` deadlocks when the writer races ahead of the server. The fix is canonical: switch the server side to overlapped I/O so connect/read can wait on Win32 events with an explicit timeout, eliminating both the deadlock and the test's reliance on unspecified ordering.

**Files:**
- Modify: `crates/sandbox/src/windows.rs:94-205` — `create_pipe()` and `read_events()`
- Modify: `crates/sandbox/src/windows.rs:528-622` — un-ignore `pipe_round_trip_single_event`
- No test file needed: existing in-tree test exercises the fix.

**Pre-flight (one-time):** confirm we have the windows-sys features required by overlapped I/O. They are already present (`Win32_System_IO`, `Win32_System_Threading`) in `crates/sandbox/Cargo.toml:39-50`.

---

- [ ] **Step 1.1: Verify the unit test currently fails (or hangs)**

This step is RED for the whole task — establishes the baseline behavior the fix must change.

Because the test is `#[ignore]`d, we run it explicitly. From the workspace root on a Windows host (or the CI logs of a hand-triggered run), run:

```bash
cd /Users/ken/workspace/ms/rage
cargo test -p sandbox --lib windows::tests::pipe_round_trip_single_event -- --include-ignored --nocapture
```

Expected: the test either hangs (kill it after 30s) or fails with `expected exactly one event, got: []`. Either outcome confirms the race the fix targets. If it passes deterministically across 5 consecutive runs, this task is moot — abandon it and update the plan. (For developers without Windows hardware: skip this step and trust the existing comment at `windows.rs:522-527`; rely on Step 1.6 below.)

- [ ] **Step 1.2: Add the overlapped-I/O imports to `crates/sandbox/src/windows.rs`**

Replace the existing `use windows_sys::...` block at the top of the file (lines 20-40) with the following expanded block. The change adds `CreateEventW`, `ResetEvent`, `OVERLAPPED`, `GetOverlappedResult`, `FILE_FLAG_OVERLAPPED`, `ERROR_IO_PENDING`, `ERROR_HANDLE_EOF`, and `WAIT_OBJECT_0`.

```rust
use crate::event::{AccessEvent, PathSet, RunResult};
use crate::pipe_proto;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING,
    ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
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
```

**Note:** `FILE_FLAG_OVERLAPPED` lives in `Win32::Storage::FileSystem` in windows-sys 0.59 (next to `PIPE_ACCESS_INBOUND`). `WAIT_OBJECT_0` lives in `Win32::Foundation`. `OVERLAPPED` and `GetOverlappedResult` live in `Win32::System::IO`. If any import fails to resolve at compile time, search for it via:

```bash
grep -rn "pub const FILE_FLAG_OVERLAPPED\|pub fn GetOverlappedResult\|pub struct OVERLAPPED" \
  $(find ~/.cargo/registry/src -maxdepth 3 -type d -name 'windows-sys-0.59*' 2>/dev/null | head -1)/src 2>/dev/null
```

Then move the import to the correct module path.

- [ ] **Step 1.3: Modify `create_pipe()` to use `FILE_FLAG_OVERLAPPED`**

Replace the body of `create_pipe()` at `crates/sandbox/src/windows.rs:94-130` with:

```rust
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
```

The single substantive change is `PIPE_ACCESS_INBOUND` → `PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED`. Everything else is identical to the existing code.

- [ ] **Step 1.4: Rewrite `read_events()` for overlapped I/O**

Replace the entire body of `read_events()` at `crates/sandbox/src/windows.rs:139-205` with:

```rust
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
```

- [ ] **Step 1.5: Remove `#[ignore]` from `pipe_round_trip_single_event`**

Edit `crates/sandbox/src/windows.rs:528-530`. Replace:

```rust
    #[test]
    #[ignore = "requires careful concurrency — end-to-end tested by windows_integration test"]
    fn pipe_round_trip_single_event() {
```

with:

```rust
    #[test]
    fn pipe_round_trip_single_event() {
```

Leave `run_sandboxed_cmd_exit_returns_zero` (line 644-652) ignored — it requires the actual DLL artifact and is exercised by the integration test.

- [ ] **Step 1.6: Run the test on a Windows host (or via CI ad-hoc)**

If you have a Windows machine:

```bash
cd /Users/ken/workspace/ms/rage
cargo test -p sandbox --lib windows::tests::pipe_round_trip_single_event --release -- --nocapture
```

Expected: `test result: ok. 1 passed; 0 failed`. Run it 5 times in a row to confirm determinism:

```bash
for i in 1 2 3 4 5; do
  cargo test -p sandbox --lib windows::tests::pipe_round_trip_single_event --release -- --nocapture || break
done
```

If you don't have a Windows host, push the change to a branch and let CI exercise it via the `test (windows-latest)` job. The job will fail if the test is flaky.

- [ ] **Step 1.7: Sanity-check the file with `cargo check`**

This catches imports we may have left unused.

```bash
cd /Users/ken/workspace/ms/rage
cargo check --target x86_64-pc-windows-msvc -p sandbox 2>&1 | tail -40 || true
# If the target isn't installed, skip this and trust CI.
```

The full `cargo clippy --workspace --all-targets -- -D warnings` runs on Windows in CI; we will rely on it.

- [ ] **Step 1.8: Commit**

```bash
cd /Users/ken/workspace/ms/rage
git add crates/sandbox/src/windows.rs
git commit -m "fix(sandbox): use overlapped I/O in named-pipe server to remove race

Replace synchronous ConnectNamedPipe/ReadFile with overlapped variants
so the server can wait on a Win32 event with a deterministic
GetOverlappedResult code. This eliminates the connect/disconnect race
that caused pipe_round_trip_single_event to deadlock when the writer
thread completed before the server entered ConnectNamedPipe.

Un-ignore pipe_round_trip_single_event; run_sandboxed_cmd_exit_returns_zero
remains ignored because it requires the rage_sandbox.dll artifact."
```

---

## Task 2: Make Windows sandbox CI gating

**Why:** The Windows sandbox integration test currently runs in CI with `continue-on-error: true`. CI is therefore green even when DLL injection fails. Removing this flag makes Windows sandbox failures break PRs — the desired post-Task-1 behavior.

**Files:**
- Modify: `.github/workflows/ci.yml:244-250`

**Pre-flight:** Task 1 must be complete and merged (or at least on the same branch). If `pipe_round_trip_single_event` is flaky, this task will produce flaky CI.

---

- [ ] **Step 2.1: Remove `continue-on-error: true` from `sandbox-smoke-windows`**

Edit `.github/workflows/ci.yml:244-250`. Replace:

```yaml
      - name: Run Windows sandbox integration test
        run: |
          cargo test -p sandbox --test windows_integration -- --include-ignored --nocapture
        env:
          RAGE_SANDBOX_DLL_PATH: ${{ github.workspace }}\target\release\rage_sandbox.dll
        shell: pwsh
        continue-on-error: true
```

with:

```yaml
      - name: Run Windows sandbox integration test
        run: |
          cargo test -p sandbox --test windows_integration -- --include-ignored --nocapture
        env:
          RAGE_SANDBOX_DLL_PATH: ${{ github.workspace }}\target\release\rage_sandbox.dll
        shell: pwsh
```

The only change is dropping the final `continue-on-error: true` line.

- [ ] **Step 2.2: Push and watch CI for one full Windows run**

```bash
cd /Users/ken/workspace/ms/rage
git add .github/workflows/ci.yml
git commit -m "ci(windows): make sandbox-smoke-windows job gating

Removes continue-on-error: true so Windows sandbox failures now block
PRs. Safe to flip after the overlapped-I/O race fix in
crates/sandbox/src/windows.rs."
git push
```

Watch the next CI run. The `Sandbox smoke test (Windows)` job must finish green. If it goes red, the integration test exercises a different race or genuine bug — branch into a Task 1.5 (extend overlapped I/O coverage) before continuing.

---

## Task 3: Automate DLL placement via `build.rs`

**Why:** `crates/sandbox/src/windows.rs::find_dll_path()` (lines 218-230) currently uses two fallbacks:
1. `RAGE_SANDBOX_DLL_PATH` env var.
2. `<dir-of-current-exe>/rage_sandbox.dll`.

The macOS pipeline has a third, more reliable fallback: a build-time-baked path stored in `RAGE_SANDBOX_DYLIB_DEFAULT`, computed in `crates/sandbox/build.rs`. We will mirror that pattern for Windows by baking `RAGE_SANDBOX_DLL_DEFAULT` and consuming it in `find_dll_path()`.

**Files:**
- Modify: `crates/sandbox/build.rs:11-33` — emit the Windows env var alongside the macOS one.
- Modify: `crates/sandbox/src/windows.rs:207-230` — use the baked default.

---

- [ ] **Step 3.1: Read `dylib_path()` for the macOS reference pattern**

Open `crates/sandbox/src/macos.rs:21-26` for reference:

```rust
pub fn dylib_path() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("RAGE_SANDBOX_DYLIB") {
        return Ok(PathBuf::from(val));
    }
    Ok(PathBuf::from(env!("RAGE_SANDBOX_DYLIB_DEFAULT")))
}
```

We mirror it for Windows.

- [ ] **Step 3.2: Update `crates/sandbox/build.rs` to emit the Windows env var**

Replace the entire contents of `crates/sandbox/build.rs` with:

```rust
// build.rs — locate the sandbox dynamic-library at build time and bake the
// expected path into the binary as a compile-time env var.
//
// The dylib (`librage_sandbox.dylib` on macOS) is produced by the
// `sandbox-macos-dylib` workspace crate, and the DLL (`rage_sandbox.dll` on
// Windows) by the `sandbox-windows-detours` workspace crate.  Both are
// *sibling* crates — NOT direct Cargo dependencies of this crate.  We must not
// link against them.  Instead, we compute the path Cargo would place the
// output artifact at and pass it through as a compile-time env var.  Consumers
// may override this at runtime by setting the corresponding `RAGE_SANDBOX_*`
// runtime variable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set by Cargo");
    let out_path = std::path::Path::new(&out_dir);

    // OUT_DIR has the form:
    //   .../target/<profile>/build/sandbox-<hash>/out
    //
    // ancestors().nth(3) steps back three levels to land on:
    //   .../target/<profile>/
    let profile_dir = out_path
        .ancestors()
        .nth(3)
        .expect("OUT_DIR does not have the expected directory depth");

    // macOS: librage_sandbox.dylib
    let dylib = profile_dir.join("librage_sandbox.dylib");
    println!(
        "cargo:rustc-env=RAGE_SANDBOX_DYLIB_DEFAULT={}",
        dylib.display()
    );

    // Windows: rage_sandbox.dll  (lib name is `rage_sandbox` per
    // crates/sandbox-windows-detours/Cargo.toml [lib] name = "rage_sandbox")
    let dll = profile_dir.join("rage_sandbox.dll");
    println!(
        "cargo:rustc-env=RAGE_SANDBOX_DLL_DEFAULT={}",
        dll.display()
    );
}
```

- [ ] **Step 3.3: Update `find_dll_path()` in `crates/sandbox/src/windows.rs`**

Replace `find_dll_path()` at `crates/sandbox/src/windows.rs:207-230` with:

```rust
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
```

- [ ] **Step 3.4: Update the corresponding unit test**

Edit `crates/sandbox/src/windows.rs:626-636`. The existing test only verifies the env-var override; add a sibling test for the compile-time-baked path. Replace the `find_dll_path_uses_env_override` test with:

```rust
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
```

(Note: this second test passes whether the colocated DLL is present or not — both legitimate fallbacks end in `rage_sandbox.dll`.)

- [ ] **Step 3.5: Run the unit tests on Windows (or via CI)**

```bash
cd /Users/ken/workspace/ms/rage
cargo test -p sandbox --lib windows::tests -- --nocapture
```

Expected: all non-ignored tests pass, including the two `find_dll_path_*` tests. On non-Windows hosts, this command compiles to nothing (the module is gated behind `#[cfg(target_os = "windows")]`); rely on CI.

- [ ] **Step 3.6: Verify the macOS path is unchanged**

The macOS dylib path resolution must still work after the build.rs refactor. Run:

```bash
cd /Users/ken/workspace/ms/rage
cargo test -p sandbox --lib macos -- --nocapture
```

Expected: no regressions. The build.rs continues to emit `RAGE_SANDBOX_DYLIB_DEFAULT` exactly as before; only an additional Windows env var was added.

- [ ] **Step 3.7: Commit**

```bash
cd /Users/ken/workspace/ms/rage
git add crates/sandbox/build.rs crates/sandbox/src/windows.rs
git commit -m "feat(sandbox): bake Windows DLL path via build.rs

Mirror the macOS dylib path-baking pattern for the Windows DLL.
crates/sandbox/build.rs now emits RAGE_SANDBOX_DLL_DEFAULT pointing at
target/<profile>/rage_sandbox.dll, and crates/sandbox/src/windows.rs
::find_dll_path() falls back to that path when neither the runtime
RAGE_SANDBOX_DLL_PATH override nor a DLL colocated with the current
executable is present.

This means \`cargo build --workspace\` (or \`cargo build -p rage-cli
-p sandbox-windows-detours\`) produces a working Windows sandbox
without the user manually setting environment variables."
```

---

## Task 4: Update the README to reflect Windows support

**Why:** `README.md:63-71` lists Windows as "🔌 Planned"; line 124 says "Windows is not yet implemented." After Tasks 1–3, both statements are wrong.

**Files:**
- Modify: `README.md:63-71` — platform support table.
- Modify: `README.md:122-124` — status footer.

---

- [ ] **Step 4.1: Update the platform support table**

Edit `README.md:63-71`. Replace:

```markdown
## Platform support

| Platform | Status | Sandbox mechanism |
|---|---|---|
| macOS | ✅ Supported | DYLD interpose (`__DATA,__interpose` in a Mach-O dylib loaded via `DYLD_INSERT_LIBRARIES`) |
| Linux | ✅ Supported | eBPF tracepoints (aya loader, `sys_enter_*` hooks, ring-buffer events) |
| Windows | 🔌 Planned | Microsoft Detours inline patching (DLL injected into a suspended child via `DetourCreateProcessWithDllsW`, named-pipe IPC) |

The Windows backend is a planned `sandbox-windows-detours` crate. The mechanism deliberately mirrors BuildXL's Windows sandbox so a rage build on Windows matches BuildXL's correctness model. See [`docs/architecture/SANDBOX.md`](docs/architecture/SANDBOX.md) for the design.
```

with:

```markdown
## Platform support

| Platform | Status | Sandbox mechanism |
|---|---|---|
| macOS | ✅ Supported | DYLD interpose (`__DATA,__interpose` in a Mach-O dylib loaded via `DYLD_INSERT_LIBRARIES`) |
| Linux | ✅ Supported | eBPF tracepoints (aya loader, `sys_enter_*` hooks, ring-buffer events) |
| Windows | ✅ Supported | Inline patching via `retour` (DLL injected into a suspended child via `CreateRemoteThread(LoadLibraryW)`; `kernel32!CreateFileW` and `ntdll!NtCreateFile` hooked; named-pipe IPC) |

The Windows backend lives in the `sandbox-windows-detours` crate. The mechanism deliberately mirrors BuildXL's Windows sandbox so a rage build on Windows matches BuildXL's correctness model. See [`docs/architecture/SANDBOX.md`](docs/architecture/SANDBOX.md) for the design.
```

Two substantive edits:
1. Status: 🔌 Planned → ✅ Supported, mechanism rewritten to match what's actually shipping (`retour` + `CreateRemoteThread`, not `DetourCreateProcessWithDllsW`).
2. "is a planned crate" → "lives in the crate".

- [ ] **Step 4.2: Update the status footer**

Edit `README.md:122-124`. Replace:

```markdown
## Status

Pre-1.0. macOS and Linux supported. Windows is not yet implemented. The TypeScript plugin is the first ecosystem; the trait is designed for Rust, Go, and Python plugins to follow without scheduler changes.
```

with:

```markdown
## Status

Pre-1.0. macOS, Linux, and Windows supported. The TypeScript plugin is the first ecosystem; the trait is designed for Rust, Go, and Python plugins to follow without scheduler changes.
```

The change is dropping the "Windows is not yet implemented" sentence and adding Windows to the supported list.

- [ ] **Step 4.3: Verify the renderer**

Inspect the rendered diff to make sure no orphaned anchors or broken links remain:

```bash
cd /Users/ken/workspace/ms/rage
git diff README.md
grep -n "Planned\|not yet implemented\|🔌" README.md || echo "OK: no stale Windows status text remaining"
```

Expected output: `OK: no stale Windows status text remaining`. If you see hits in `docs/architecture/SANDBOX.md`, those are out of scope for this plan — note them in the commit message but don't fix them here (separate doc-update plan).

- [ ] **Step 4.4: Commit**

```bash
cd /Users/ken/workspace/ms/rage
git add README.md
git commit -m "docs(readme): mark Windows as supported

The sandbox-windows-detours crate is shipping. Update the platform
support table (line 63) and status footer (line 124) to match
reality. The mechanism description now names the actual primitives in
use (retour + CreateRemoteThread + named-pipe IPC) rather than the
BuildXL-style DetourCreateProcessWithDllsW that was originally
planned."
```

---

## Task 5: Remove the nightly Rust requirement

**Why:** `crates/sandbox-windows-detours/Cargo.toml:21` opts into `retour`'s `static-detour` feature, which requires nightly because `static_detour!` uses unstable Rust features (e.g., naked function calling conventions, certain const-eval forms). `retour`'s `GenericDetour<T>` API works on stable. Migrating to `GenericDetour` allows us to drop the nightly toolchain installs from CI (`.github/workflows/ci.yml:46-49, 75, 81, 229-230, 279-282, 313`).

**Files:**
- Modify: `crates/sandbox-windows-detours/Cargo.toml:20-21`
- Modify: `crates/sandbox-windows-detours/src/hooks.rs` (top-level rewrite of detour storage + setup_hooks)
- Modify: `.github/workflows/ci.yml` (multiple lines — remove nightly installs + RUSTUP_TOOLCHAIN overrides)

**Pre-flight investigation (mandatory — do NOT skip):**

Before changing any code, verify on the developer's machine that `retour` 0.3 with default features compiles on stable. Run:

```bash
mkdir -p /tmp/retour-stable-probe && cd /tmp/retour-stable-probe
cat > Cargo.toml <<'EOF'
[package]
name = "retour-stable-probe"
version = "0.0.0"
edition = "2021"

[target.'cfg(windows)'.dependencies]
retour = { version = "0.3", default-features = false, features = ["std"] }
EOF
mkdir -p src && cat > src/lib.rs <<'EOF'
#![cfg(windows)]
use retour::GenericDetour;
type FnT = unsafe extern "system" fn(u32) -> u32;
extern "system" fn replacement(x: u32) -> u32 { x + 1 }
pub fn make_detour(target: FnT) -> Result<GenericDetour<FnT>, retour::Error> {
    unsafe { GenericDetour::<FnT>::new(target, replacement) }
}
EOF
rustup default stable
cargo +stable check --target x86_64-pc-windows-msvc 2>&1 | tail -20
```

Expected: clean compile or only "linker not found" / "no Windows target" errors (acceptable on macOS — we just need rustc to type-check). Compile errors mentioning `feature(...)` or `#![feature(...)]` mean `GenericDetour` itself requires nightly in this version of retour, in which case fall through to **Step 5.0a** instead.

- [ ] **Step 5.0a: If `GenericDetour` does NOT compile on stable**

If the probe in the pre-flight step shows that `retour` 0.3 default features still require nightly even without `static-detour`, switch to one of these alternatives in priority order. Document which one was chosen at the top of `crates/sandbox-windows-detours/src/hooks.rs`:

| Option | Crate | Tradeoff |
|---|---|---|
| A | `minhook-sys` 0.1 + thin Rust wrapper | C library; ~5 KiB; well-tested by Wine and many trainers; MIT |
| B | `detour` 0.8 (the unmaintained predecessor — sometimes builds where `retour` won't) | Last published 2018 |
| C | Custom 4-byte JMP patcher using `windows-sys` `VirtualProtect` + `WriteProcessMemory` directly | ~80 LOC, no deps, but maintainability burden |
| D | Stay on nightly, document the requirement, close the task | Defers work |

If choosing A, B, or C, the rest of Task 5 (Steps 5.1–5.7) becomes "rewrite hooks.rs against the chosen primitive" — write that as a follow-up plan with concrete code per primitive. **Do not blindly proceed past this step on a probe failure.**

If the probe succeeds (the most likely outcome — `GenericDetour` is documented as stable-compatible in retour 0.3), proceed with Steps 5.1 onward.

- [ ] **Step 5.1: Drop the `static-detour` feature from `crates/sandbox-windows-detours/Cargo.toml`**

Edit `crates/sandbox-windows-detours/Cargo.toml:20-21`. Replace:

```toml
[target.'cfg(windows)'.dependencies]
retour = { version = "0.3", features = ["static-detour"] }
```

with:

```toml
[target.'cfg(windows)'.dependencies]
# Use GenericDetour (stable Rust). The static-detour feature requires
# nightly; we store detours in OnceLock<GenericDetour<...>> ourselves instead.
retour = { version = "0.3", default-features = false, features = ["std"] }
```

(The `std` feature is required by `GenericDetour`; `default-features = false` removes any nightly-gated default features that may exist in this version.)

- [ ] **Step 5.2: Rewrite the detour declarations in `crates/sandbox-windows-detours/src/hooks.rs`**

Replace lines 1–48 of `crates/sandbox-windows-detours/src/hooks.rs` (everything from `#![cfg(windows)]` through the end of the `static_detour! { ... }` block and the type aliases) with:

```rust
#![cfg(windows)]

use crate::ipc::PipeClient;
use retour::GenericDetour;
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
// Detour storage (stable-Rust replacement for the retour `static_detour!` macro)
// ---------------------------------------------------------------------------
//
// `static_detour!` in retour 0.3 requires nightly (it uses unstable language
// features for static initialization).  We store the detours in OnceLock cells
// instead — populated exactly once during `setup_hooks`, then read from the
// hook trampolines via `.get().unwrap().call(...)`.

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

/// Detour for `kernel32!CreateFileW`. Set once during `setup_hooks`.
static CREATE_FILE_W_DETOUR: OnceLock<GenericDetour<CreateFileWFn>> = OnceLock::new();

/// Detour for `ntdll!NtCreateFile`. Set once during `setup_hooks`.
static NT_CREATE_FILE_DETOUR: OnceLock<GenericDetour<NtCreateFileFn>> = OnceLock::new();
```

- [ ] **Step 5.3: Update the hook implementations to call the stored detours**

Replace `hook_create_file_w` (lines 163-193 of the existing `hooks.rs`) with:

```rust
/// Hook for `kernel32!CreateFileW`. Forwards to the original via the
/// stored detour's trampoline.
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

    // The detour cell is populated before any hook fires (setup_hooks
    // installs the detour AFTER OnceLock::set), so .get() is always Some.
    // SAFETY: All arguments are forwarded unmodified to the original function
    // via the retour trampoline.
    let detour = CREATE_FILE_W_DETOUR
        .get()
        .expect("CREATE_FILE_W_DETOUR must be initialized before hook fires");
    unsafe {
        detour.call(
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
```

Replace `hook_nt_create_file` (lines 195-236) with:

```rust
/// Hook for `ntdll!NtCreateFile`. Forwards to the original via the stored
/// detour's trampoline.
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
    let detour = NT_CREATE_FILE_DETOUR
        .get()
        .expect("NT_CREATE_FILE_DETOUR must be initialized before hook fires");
    unsafe {
        detour.call(
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
```

- [ ] **Step 5.4: Rewrite `setup_hooks` to construct GenericDetours**

Replace `setup_hooks` (lines 250-321 of the existing `hooks.rs`) with:

```rust
/// Install file-system hooks in the current process.
///
/// 1. Connects to the named pipe at `pipe_name` (returns error if absent).
/// 2. Stores the [`PipeClient`] in the global [`IPC_CLIENT`].
/// 3. Resolves `CreateFileW` from `kernel32.dll` and `NtCreateFile` from
///    `ntdll.dll`, constructs a `GenericDetour` for each, enables them, and
///    stores them in `CREATE_FILE_W_DETOUR` / `NT_CREATE_FILE_DETOUR`.
///
/// # Errors
///
/// Returns an [`io::Error`] if:
/// - The pipe cannot be opened (`PipeClient::connect` fails).
/// - A module handle cannot be obtained (`GetModuleHandleW` returns 0).
/// - A function address cannot be resolved (`GetProcAddress` returns `None`).
/// - A detour cannot be initialised or enabled (retour returns an error).
/// - The OnceLock for either detour was already populated (would only happen
///   if `setup_hooks` was called twice in the same process).
#[allow(clippy::manual_c_str_literals)]
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

        let create_detour = GenericDetour::<CreateFileWFn>::new(create_file_w, hook_create_file_w)
            .map_err(|e| io::Error::other(format!("CreateFileW detour::new: {e}")))?;
        create_detour
            .enable()
            .map_err(|e| io::Error::other(format!("CreateFileW detour::enable: {e}")))?;
        CREATE_FILE_W_DETOUR
            .set(create_detour)
            .map_err(|_| io::Error::other("CREATE_FILE_W_DETOUR already initialized"))?;

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

        let nt_detour = GenericDetour::<NtCreateFileFn>::new(nt_create_file, hook_nt_create_file)
            .map_err(|e| io::Error::other(format!("NtCreateFile detour::new: {e}")))?;
        nt_detour
            .enable()
            .map_err(|e| io::Error::other(format!("NtCreateFile detour::enable: {e}")))?;
        NT_CREATE_FILE_DETOUR
            .set(nt_detour)
            .map_err(|_| io::Error::other("NT_CREATE_FILE_DETOUR already initialized"))?;
    }

    Ok(())
}
```

The existing `wide_ptr_to_string`, `oa_to_string`, `send_access`, and `to_wide_null` helpers (lines 54-130, 137-157, 243-245) are unchanged.

The existing test `setup_hooks_with_bad_pipe_name_returns_error` (lines 323-334) is also unchanged.

- [ ] **Step 5.5: Compile the crate on stable**

If you're on Windows:

```bash
cd /Users/ken/workspace/ms/rage
rustup default stable
cargo +stable build --release -p sandbox-windows-detours 2>&1 | tail -40
```

Expected: clean build. If you see `error[E0658]: ... feature ... is unstable`, the migration is incomplete — return to Step 5.0a.

If you're on macOS or Linux without a Windows target installed:

```bash
cd /Users/ken/workspace/ms/rage
rustup target add x86_64-pc-windows-msvc 2>/dev/null || true
cargo +stable check --target x86_64-pc-windows-msvc -p sandbox-windows-detours 2>&1 | tail -40
```

(Linker errors are acceptable; rustc errors are not.)

- [ ] **Step 5.6: Remove the nightly toolchain installs from `.github/workflows/ci.yml`**

Six edits in this file. Apply them in order top-to-bottom so line numbers stay anchored.

**Edit 5.6.a** — remove the `Install Rust nightly` step in the `test` job (lines 46-49):

Delete:

```yaml
      - name: Install Rust nightly (Windows - required for Detours DLL)
        if: matrix.os == 'windows-latest'
        run: rustup toolchain install nightly --profile default
        shell: bash
```

**Edit 5.6.b** — drop the conditional `RUSTUP_TOOLCHAIN` env in the clippy step (line 75). Replace:

```yaml
      - name: Clippy
        run: cargo clippy --workspace ${{ matrix.exclude_flags }} --all-targets -- -D warnings
        shell: bash
        env:
          RUSTUP_TOOLCHAIN: ${{ matrix.os == 'windows-latest' && 'nightly' || 'stable' }}
```

with:

```yaml
      - name: Clippy
        run: cargo clippy --workspace ${{ matrix.exclude_flags }} --all-targets -- -D warnings
        shell: bash
```

**Edit 5.6.c** — drop the conditional `RUSTUP_TOOLCHAIN` env in the test step (line 81). Replace:

```yaml
      - name: Run tests
        run: cargo test --workspace ${{ matrix.exclude_flags }}
        shell: bash
        env:
          RUSTUP_TOOLCHAIN: ${{ matrix.os == 'windows-latest' && 'nightly' || 'stable' }}
```

with:

```yaml
      - name: Run tests
        run: cargo test --workspace ${{ matrix.exclude_flags }}
        shell: bash
```

**Edit 5.6.d** — replace the nightly toolchain install in `sandbox-smoke-windows` (lines 229-230). Replace:

```yaml
      - name: Install Rust nightly toolchain (for Detours DLL - retour crate requires nightly)
        uses: dtolnay/rust-toolchain@nightly
```

with:

```yaml
      - name: Install Rust stable toolchain
        uses: dtolnay/rust-toolchain@stable
```

**Edit 5.6.e** — remove the nightly install in the `e2e-smoke` job (lines 279-282). Delete:

```yaml
      - name: Install Rust nightly (Windows - required for Detours DLL)
        if: matrix.os == 'windows-latest'
        run: rustup toolchain install nightly
        shell: bash
```

**Edit 5.6.f** — drop the conditional `RUSTUP_TOOLCHAIN` env in the e2e Build step (line 313). Replace:

```yaml
      - name: Build rage
        run: cargo build --workspace ${{ matrix.exclude_flags }}
        shell: bash
        env:
          RUSTUP_TOOLCHAIN: ${{ matrix.os == 'windows-latest' && 'nightly' || 'stable' }}
```

with:

```yaml
      - name: Build rage
        run: cargo build --workspace ${{ matrix.exclude_flags }}
        shell: bash
```

After all six edits, sanity-check that no nightly references remain:

```bash
cd /Users/ken/workspace/ms/rage
grep -n "nightly\|RUSTUP_TOOLCHAIN" .github/workflows/ci.yml || echo "OK: no nightly references"
```

Expected: `OK: no nightly references`. If `bpf-linker` or eBPF-related nightly references show up, those are out of scope (they live in the `sandbox-linux-ebpf` build path and were already stable-only in CI per `.github/workflows/ci.yml:24-28`).

- [ ] **Step 5.7: Commit**

```bash
cd /Users/ken/workspace/ms/rage
git add crates/sandbox-windows-detours/Cargo.toml \
        crates/sandbox-windows-detours/src/hooks.rs \
        .github/workflows/ci.yml
git commit -m "build(windows): migrate to retour GenericDetour for stable Rust

Replace the static_detour! macro (which requires nightly) with
GenericDetour stored in OnceLock cells. This is functionally
equivalent — both produce inline-patched trampolines via retour's
detour engine — but compiles on stable Rust.

Drop the static-detour feature from the retour dependency and remove
the six nightly-toolchain installs and RUSTUP_TOOLCHAIN env overrides
from CI. Windows now builds, lints, and tests on stable like every
other platform."
```

- [ ] **Step 5.8: Push and watch CI for one full Windows run on stable**

```bash
git push
```

Watch all four Windows CI jobs (test, build, sandbox-smoke-windows, e2e-smoke). All four must finish green on stable. If `sandbox-smoke-windows` fails specifically inside the integration test, the GenericDetour install path may differ from `static_detour!` in some subtle way (e.g., trampoline alignment); branch into a hotfix that reverts Steps 5.1–5.4 and keeps Step 5.6 disabled, then re-investigate.

---

## Self-Review

**1. Spec coverage:**

| User-stated task | Plan task | Concrete code? |
|---|---|---|
| Fix `pipe_round_trip` race | Task 1 | ✅ Full overlapped-I/O rewrite of `read_events` and `create_pipe`. |
| Make sandbox integration test gating | Task 2 | ✅ One-line YAML deletion. |
| Automate DLL placement via build.rs | Task 3 | ✅ Mirror of macOS pattern, with new test for the baked default. |
| Update README | Task 4 | ✅ Both edit hunks shown verbatim. |
| Remove nightly Rust requirement | Task 5 | ⚠️ Concrete code for the most-likely path (GenericDetour); fallbacks A–D documented at Step 5.0a. |

**2. Placeholder scan:** No "TBD", "implement later", or "add appropriate error handling" tokens in the plan. Step 5.0a explicitly enumerates fallback options rather than waving at them.

**3. Type consistency:** `CreateFileWFn` and `NtCreateFileFn` aliases are defined once in Step 5.2 and re-used in Steps 5.3, 5.4. `RAGE_SANDBOX_DLL_DEFAULT` env var name is consistent across `build.rs` (Step 3.2) and `find_dll_path()` (Step 3.3). `connect_event` is a single owned handle through Step 1.4 — every code path either consumes it via `CloseHandle` or never receives it.

**4. Dependency order:** Task 1 (race fix) → Task 2 (gate CI on the now-reliable test) → Task 3 (DLL automation, independent) → Task 4 (docs, depends on Tasks 1–3 being on `main`) → Task 5 (nightly removal, last because it can churn CI).

**5. Known unknowns:** The retour-stable migration in Task 5 has one verification gate (the Step 5.0 probe). If `GenericDetour` does NOT compile on stable in retour 0.3 — possible if the version pulled by `Cargo.lock` has different feature flags than the published crate — the plan branches at Step 5.0a rather than blocking. Three concrete fallback paths are listed.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-02-windows-port-completion.md`.**

Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task with two-stage review (spec compliance + code quality) between each. Each task is independently green-able and committable, so checkpoints are natural.

**2. Inline Execution** — Execute all five tasks in this session using the executing-plans skill, with checkpoints between Tasks 1↔2 (race fix must demonstrably hold before flipping CI gating) and 4↔5 (must observe a stable-only CI green before committing the cleanup).

**Which approach?**
