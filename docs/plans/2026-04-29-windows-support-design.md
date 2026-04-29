# Windows Support Design

## Goal

Make `rage` fully functional on Windows by closing the four remaining gaps in the Rust workspace: platform IPC transport, shell dispatch, Node version manager paths, and an end-to-end DLL injection integration test plus CI hardening.

## Background

The Windows sandbox DLL injection layer is already complete and working. The remaining cross-platform gaps prevent `rage` from compiling and running correctly on Windows:

- `daemon/src/socket.rs` and `cli/src/main.rs` use `tokio::net::Unix*` types unconditionally — these will not compile on Windows.
- `scheduler/src/runner.rs` shells out via `Command::new("sh").arg("-c")` in four places, but Windows has no `sh`.
- `scheduler/src/node_path.rs` only checks Unix-shaped paths for fnm, nvm, and Volta, and uses `HOME` (which is not always set on Windows).
- The existing Windows sandbox test points at a deliberately nonexistent DLL, so the full inject → hook → pipe → event pipeline is never exercised. The `sandbox-smoke-windows` CI step also pipes output through `Select-String` without an exit-code assertion, so the job will pass even if `rage.exe` crashes.

The sandbox DLL itself, the named-pipe protocol, and the injection code are out of scope — they are already solid. The `.worktrees/fix-distributed-test-path` work is also out of scope.

## Approach

Treat the four areas as independent work items. They share no state and no dependencies on each other, so they can be implemented in parallel:

1. **Platform IPC Transport** — introduce a `DaemonStream` / `DaemonServer` abstraction and a unified `~/.rage/daemon.lock` discovery file. Use `tokio::net::windows::named_pipe` on Windows and `tokio::net::Unix*` on Unix. No new `windows-sys` calls.
2. **Shell Dispatch** — add a `scheduler::shell::command()` helper that returns a `tokio::process::Command` configured with `sh -c` on Unix and `cmd /c` on Windows. Replace the four hardcoded `runner.rs` call sites.
3. **Node Version Manager Paths** — add a `#[cfg(windows)]` block to `node_path.rs` covering fnm, nvm-windows, and Volta; fall back to `USERPROFILE` when `HOME` is unset.
4. **Task 7 Integration Test + CI Hardening** — add a real end-to-end injection test in `crates/sandbox/tests/windows_integration.rs`, wire it into CI with `RAGE_SANDBOX_DLL_PATH`, and add an exit-code assertion to the existing PowerShell smoke step.

## Architecture

```
                        ┌──────────────────────────┐
                        │      crates/cli          │
                        │  cmd_dev / cmd_status    │
                        └────────────┬─────────────┘
                                     │ daemon_connect()
                                     ▼
              ┌────────────────────────────────────────────┐
              │       crates/daemon/src/transport.rs       │
              │                                            │
              │   DaemonStream  ─┬─ UnixStream      (unix) │
              │                  └─ NamedPipe{S,C}  (win)  │
              │                                            │
              │   DaemonServer  ─┬─ UnixListener    (unix) │
              │                  └─ NamedPipeServer (win)  │
              │                                            │
              │   Lockfile: ~/.rage/daemon.lock            │
              └─────────────────────┬──────────────────────┘
                                    │ uses
                                    ▼
                        ┌──────────────────────────┐
                        │   crates/daemon/src/     │
                        │       socket.rs          │
                        └──────────────────────────┘

   ┌─────────────────────────────┐         ┌────────────────────────────┐
   │  crates/scheduler/runner.rs │ ──uses──▶│  crates/scheduler/shell.rs │
   │  (Loose mode + hooks)       │          │  shell::command(cmd)       │
   └─────────────────────────────┘          │   sh -c   |  cmd /c        │
                                             └────────────────────────────┘

   ┌──────────────────────────────────┐
   │ crates/scheduler/node_path.rs    │  + #[cfg(windows)] paths for
   │                                  │    fnm, nvm-windows, Volta
   │                                  │  + USERPROFILE fallback
   └──────────────────────────────────┘

   ┌─────────────────────────────────────────────┐
   │ crates/sandbox/tests/windows_integration.rs │  end-to-end:
   │                                             │  inject → hook → pipe → events
   └─────────────────────────────────────────────┘
```

## Components

### 1. Platform IPC Transport (`crates/daemon/src/transport.rs`, new)

Public surface:

- `DaemonStream` — a newtype wrapping `UnixStream` on Unix, or a `pin_project` enum over `NamedPipeServer` / `NamedPipeClient` on Windows. Implements `AsyncRead + AsyncWrite + Send + Unpin` on both platforms so existing message-handling code is untouched.
- `DaemonServer` — wraps `UnixListener` on Unix and a named-pipe server loop on Windows. Methods:
  - `bind() -> (DaemonServer, endpoint_string)` — chooses an endpoint, opens it, writes the endpoint string into `~/.rage/daemon.lock`.
  - `accept() -> DaemonStream` — accepts the next inbound client.
- `daemon_connect() -> Result<DaemonStream, DaemonError>` — reads `~/.rage/daemon.lock`, connects to the endpoint, returns a typed `DaemonNotRunning` error when no daemon is reachable.
- `DaemonLock` — RAII handle that deletes `~/.rage/daemon.lock` on `Drop`.

Windows implementation uses `tokio::net::windows::named_pipe` (`ServerOptions` / `ClientOptions`) which ships with tokio 1.x — **no new `windows-sys` calls**. The pipe name format is `\\.\pipe\rage-daemon-{nonce}` where the nonce is 8 hex chars derived from `SystemTime` XOR'd with PID — the same nonce pattern already used in `sandbox::windows::create_pipe`.

Unified discovery on both platforms via `~/.rage/daemon.lock`:

- On daemon start, `DaemonServer::bind()` writes the endpoint string (socket path on Unix, pipe name on Windows) to the lockfile.
- On daemon stop, `DaemonLock::drop` removes the lockfile.
- `daemon_connect()` reads the lockfile to find the endpoint.
- Stale-lockfile detection: if the lockfile exists but `connect` returns `NotFound` / `ConnectionRefused` (Unix) or `ERROR_FILE_NOT_FOUND` (Windows), delete the stale lockfile and return `DaemonError::DaemonNotRunning` (distinct from generic transport errors).

Modifications to existing files:

- `crates/daemon/src/socket.rs` — replace `UnixListener` with `DaemonServer`. All message-handling logic is unchanged.
- `crates/cli/src/main.rs` — `cmd_dev()` and `cmd_status()` use `daemon_connect()` instead of `UnixStream::connect()`.

### 2. Shell Dispatch (`crates/scheduler/src/shell.rs`, new)

Single public function:

```rust
/// Returns a Command that executes `cmd` through the platform shell.
/// Unix:    sh -c <cmd>
/// Windows: cmd /c <cmd>
pub fn command(cmd: &str) -> tokio::process::Command
```

- `#[cfg(unix)]` returns `Command::new("sh").arg("-c").arg(cmd)`.
- `#[cfg(windows)]` returns `Command::new("cmd").arg("/c").arg(cmd)`.

Call sites replaced in `runner.rs`:

| Location                       | Before                              | After                  |
|--------------------------------|-------------------------------------|------------------------|
| Loose-mode task execution      | `Command::new("sh").arg("-c")`      | `shell::command(cmd)`  |
| Pre-install hooks              | `Command::new("sh").arg("-c")`      | `shell::command(cmd)`  |
| Post-install hooks             | `Command::new("sh").arg("-c")`      | `shell::command(cmd)`  |
| `which_first` fallback         | `Command::new("sh").arg("-c")`      | `shell::command(cmd)`  |

Boundary: `run_sandboxed` is **not** touched. On Windows, `inject_and_spawn` already wraps commands in `cmd /c` when calling `CreateProcessW`. The `shell::command()` helper is for Loose-mode and hook paths only.

### 3. Node Version Manager Paths (`crates/scheduler/src/node_path.rs`, modified)

Add a `#[cfg(windows)]` block alongside the existing Unix paths:

| Manager       | Windows Path                                                                |
|---------------|-----------------------------------------------------------------------------|
| fnm           | `%LOCALAPPDATA%\fnm\node-versions\{ver}\installation\node.exe`              |
| nvm-windows   | `%APPDATA%\nvm\{ver}\node.exe`                                              |
| Volta         | `%LOCALAPPDATA%\Volta\tools\image\node\{ver}\node.exe`                      |

Also add `USERPROFILE` as a fallback for `HOME` in the environment lookup — Windows does not always set `HOME`, but always sets `USERPROFILE`.

### 4. Task 7 Integration Test (`crates/sandbox/tests/windows_integration.rs`, new)

```rust
#[tokio::test]
#[cfg(target_os = "windows")]
#[ignore]  // requires DLL built; run explicitly in CI
async fn dll_injection_produces_file_access_events() {
    // Set DLL path to compiled output.
    std::env::set_var("RAGE_SANDBOX_DLL_PATH", "target/debug/rage_sandbox.dll");

    // Run a command that reads from the filesystem.
    let events = sandbox::run_sandboxed("cmd /c dir C:\\Windows", &[])
        .await
        .expect("run_sandboxed failed");

    // Verify at least one read event for a path under C:\Windows\.
    assert!(
        events.iter().any(|e| matches!(e, AccessEvent::Read(p) if p.starts_with("C:\\Windows\\"))),
        "expected at least one AccessEvent::Read under C:\\Windows\\"
    );
}
```

The existing `inject_and_spawn_cmd_echo_runs_to_completion` test uses a deliberately nonexistent DLL, so hooks never run and no events are produced. This new test exercises the full inject → hook → pipe → event pipeline for the first time.

### 5. CI Hardening (`.github/workflows/ci.yml`, modified)

Two fixes to `sandbox-smoke-windows`:

1. Run the new Task 7 integration test explicitly with `RAGE_SANDBOX_DLL_PATH` pointing at the built DLL:

   ```yaml
   - run: cargo test -p sandbox --test windows_integration -- --include-ignored
     env:
       RAGE_SANDBOX_DLL_PATH: target/debug/rage_sandbox.dll
   ```

2. Add an exit-code assertion after the existing PowerShell smoke invocation, so a `rage.exe` crash fails the job:

   ```powershell
   if ($LASTEXITCODE -ne 0) { exit 1 }
   ```

## Data Flow

**Daemon connection (both platforms, unified):**

```
daemon start:
  DaemonServer::bind()
      ├── pick endpoint   (unix: socket path / win: \\.\pipe\rage-daemon-{nonce})
      ├── open server     (UnixListener / NamedPipeServer)
      └── write endpoint string → ~/.rage/daemon.lock

cli connect (cmd_dev / cmd_status):
  daemon_connect()
      ├── read ~/.rage/daemon.lock
      ├── connect to endpoint
      │     ├── ok           → return DaemonStream
      │     └── NotFound /
      │       ConnRefused /
      │       FILE_NOT_FOUND → unlink stale lockfile, return DaemonNotRunning
      └── any other error    → return transport error
```

**Loose-mode / hook task execution (Windows):**

```
runner.rs hook step
   └── shell::command("npm run build")        // cmd /c on Windows
        └── tokio::process::Command spawn
             └── child process inherits stdio, runs to completion
```

**Sandboxed task execution (Windows, unchanged):**

```
runner.rs sandbox step
   └── sandbox::run_sandboxed(cmd, env)
        └── inject_and_spawn (CreateProcessW with `cmd /c <cmd>`)
             └── DLL hooks → named pipe → AccessEvent stream
```

## Error Handling

- `DaemonError::DaemonNotRunning` — distinct typed variant returned from `daemon_connect()` when the lockfile is missing or stale. Callers (`cmd_dev`, `cmd_status`) surface a clear "no daemon running" message instead of a transport error.
- Stale lockfile cleanup is best-effort — if the unlink fails we still propagate `DaemonNotRunning`.
- `shell::command()` returns `tokio::process::Command`; existing spawn error handling in `runner.rs` is preserved verbatim.
- The Task 7 test panics with a descriptive message when the events list contains no read under `C:\Windows\`, making CI failures self-explanatory.
- Node-path lookup remains a best-effort fallback chain: missing `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` simply skips that candidate, matching existing Unix behavior.

## Testing Strategy

| Area | Tests |
|------|-------|
| Transport | Unit: lockfile round-trip (write, read, stale detection). Integration on both platforms: bind → write lockfile → connect → echo → drop → lockfile gone. Stale-lockfile case: pre-write a lockfile with a dead endpoint and assert `DaemonNotRunning`. |
| Shell dispatch | One unit test — `shell::command("echo hello")` spawns and exits 0 — with `#[cfg(unix)]` and `#[cfg(windows)]` variants. |
| Node version paths | Unit tests with a temp `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` to verify each manager's path resolves; `HOME`-unset fallback verified. |
| Task 7 / CI | New `windows_integration.rs` integration test (gated on `target_os = "windows"`, `#[ignore]`, run with `--include-ignored` in CI). Exit-code assertion guards the existing PowerShell smoke step against silent `rage.exe` crashes. |

All four work areas keep their tests local to the crate they touch — no cross-crate test rewiring is required.

## Open Questions

None. All key decisions were made during design:

- IPC transport — `DaemonStream` / `DaemonServer` abstraction with named pipes on Windows and Unix sockets on Unix; no new `windows-sys` calls.
- Shell — `cmd /c` on Windows, `sh -c` on Unix, scoped to Loose-mode and hooks (sandbox path unchanged).
- Discovery — unified `~/.rage/daemon.lock` on both platforms with stale-lockfile detection.
