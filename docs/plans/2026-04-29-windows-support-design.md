# Windows Support Design

## Goal

Make `rage` fully functional on Windows by closing the four remaining gaps in the Rust workspace: platform IPC transport, shell dispatch, Node version manager paths, and an end-to-end DLL injection integration test plus CI hardening.

## Background

The Windows sandbox DLL injection layer is already complete and working. The remaining cross-platform gaps prevent `rage` from compiling and running correctly on Windows:

- `daemon/src/socket.rs` and `cli/src/main.rs` use `tokio::net::Unix*` types unconditionally — these will not compile on Windows.
- `scheduler/src/runner.rs` shells out via `Command::new("sh").arg("-c")` in multiple places, but Windows has no `sh` (unless Git Bash is installed).
- `scheduler/src/node_path.rs` only checks Unix-shaped paths for fnm, nvm, and Volta, and uses `HOME` (which is not always set on Windows).
- The existing Windows sandbox test points at a deliberately nonexistent DLL, so the full inject → hook → pipe → event pipeline is never exercised. The `sandbox-smoke-windows` CI step also pipes output through `Select-String` without an exit-code assertion, so the job will pass even if `rage.exe` crashes.

The sandbox DLL itself, the named-pipe protocol, and the injection code are out of scope — they are already solid. The `.worktrees/fix-distributed-test-path` work is also out of scope.

## Approach

Four work areas, with the dependency ordering described in **Work Ordering** below — they are not all independent:

1. **Platform IPC Transport** — introduce `DaemonStream` / `DaemonServer` and **extend the existing `daemon/src/discovery.rs`** so it carries a platform-neutral `endpoint: String` (Unix socket path on Unix, named pipe name on Windows). Per-workspace daemon keying (today: `daemons_dir()/{hash}.sock`) is preserved. Use `tokio::net::windows::named_pipe` on Windows and `tokio::net::Unix*` on Unix. No new `windows-sys` calls.
2. **Shell Dispatch** — add a `scheduler::shell` module exposing `command()` (async) and `std_command()` (sync) helpers that return a `Command` configured with `sh -c` on Unix and either `sh -c` (when `sh.exe` is found on PATH) or `cmd /c` on Windows. Replace **all seven** hardcoded call sites — six in `runner.rs`/`rss_monitor.rs` and one in `postinstall_cache.rs` (which uses `std::process::Command`).
3. **Node Version Manager Paths** — add a `#[cfg(windows)]` block to `node_path.rs` covering fnm, nvm-windows, and Volta. Always check the canonical env var (`FNM_DIR`, `NVM_HOME`, `VOLTA_HOME`) before falling back to a conventional path. Use `USERPROFILE` as a fallback for `HOME`.
4. **Task 7 Integration Test + CI Hardening** — add a real end-to-end injection test in `crates/sandbox/tests/windows_integration.rs`, wire it into CI with `RAGE_SANDBOX_DLL_PATH` (resolved to an absolute path at compile time), pre-build the DLL crate explicitly, and add an exit-code assertion to the existing PowerShell smoke step.

## Work Ordering

The four areas are NOT fully independent. The actual dependency graph is:

1. **Daemon transport is one coordinated change.** The IPC work touches `discovery.rs`, `socket.rs`, `daemon.rs`, and `cli/main.rs` together — these are not separable files. They land as a single PR (or stacked PRs that merge together).
2. **Shell dispatch must land before (or in the same PR as) any new Windows integration test** — otherwise Windows CI won't even compile while the test is being added.
3. **The `sandbox-windows-detours` DLL build must precede the Task 7 integration test.** CI must run `cargo build -p sandbox-windows-detours` as an explicit step before `cargo test --test windows_integration`.
4. **Node path changes and the Task 7 test are genuinely independent** of everything else and can land in any order.

A reasonable merge order: shell dispatch → node paths → daemon transport → Task 7 test + CI hardening.

## Architecture

```
                        +---------------------------+
                        |       crates/cli          |
                        |  cmd_dev / cmd_status     |
                        +-------------+-------------+
                                      | daemon_connect()
                                      v
              +--------------------------------------------+
              |       crates/daemon/src/transport.rs       |
              |                                            |
              |   DaemonStream  -+- UnixStream      (unix) |
              |                  +- NamedPipe{S,C}  (win)  |
              |                                            |
              |   DaemonServer  -+- UnixListener    (unix) |
              |                  +- NamedPipeServer (win)  |
              |                                            |
              |   DaemonError   -- NotRunning |            |
              |                    Stale      |            |
              |                    Transport(io::Error)    |
              +--------------------+-----------------------+
                                   | uses
                                   v
              +--------------------------------------------+
              |     crates/daemon/src/discovery.rs         |
              |     (existing, EXTENDED)                   |
              |                                            |
              |   DaemonDiscovery {                        |
              |       endpoint: String,  // was unix_socket|
              |       pid: u32, ...                        |
              |   }                                        |
              |   File: daemons_dir()/{workspace_hash}.json|
              |   (per-workspace keying preserved)         |
              +--------------------+-----------------------+
                                   | uses
                                   v
                        +-------------------------+
                        |   crates/daemon/src/    |
                        |       socket.rs         |
                        +-------------------------+

   +-----------------------------+         +-----------------------------+
   |  crates/scheduler/runner.rs | --uses->|  crates/scheduler/shell.rs  |
   |  (Loose mode + hooks)       |         |  shell::command(cmd)        |
   |  + rss_monitor.rs           |         |  shell::std_command(cmd)    |
   |  + postinstall_cache.rs     |         |   sh -c (preferred on win)  |
   +-----------------------------+         |   cmd /c (fallback on win)  |
                                           +-----------------------------+

   +----------------------------------+
   | crates/scheduler/node_path.rs    |  + #[cfg(windows)] paths for
   |                                  |    fnm, nvm-windows, Volta
   |                                  |  + env vars first, paths second
   |                                  |  + USERPROFILE fallback
   +----------------------------------+

   +---------------------------------------------+
   | crates/sandbox/tests/windows_integration.rs |  end-to-end:
   |                                             |  inject -> hook -> pipe -> events
   +---------------------------------------------+
```

## Components

### 1. Platform IPC Transport (`crates/daemon/src/transport.rs`, new + `discovery.rs` extension)

#### 1a. Discovery (extends existing `daemon/src/discovery.rs`)

The existing codebase already keys daemons per-workspace at `daemons_dir()/{workspace_hash}.json` so multiple `rage dev` instances in different worktrees coexist. That mechanism is preserved. The change is field-only:

```rust
pub struct DaemonDiscovery {
    pub endpoint: String,   // RENAMED from `unix_socket: PathBuf`
    pub pid: u32,
    // ... other existing fields unchanged
}
```

- **Unix:** `endpoint` is the Unix socket path (functionally identical to today's `unix_socket.to_string_lossy()`).
- **Windows:** `endpoint` is the named-pipe name, `\\.\pipe\rage-daemon-{workspace_hash}-{nonce}`, where `nonce` is 8 hex chars derived from `SystemTime` XOR'd with PID (matching the existing pattern in `sandbox::windows::create_pipe`).

`DaemonServer::bind()` writes to the existing per-workspace `DaemonDiscovery` file. **There is no new global `~/.rage/daemon.lock` file** — that would break per-workspace coexistence.

#### 1b. Public surface (`transport.rs`)

- `DaemonStream` — newtype wrapping `UnixStream` on Unix; on Windows, an enum over `NamedPipeServer` / `NamedPipeClient`. Implements `AsyncRead + AsyncWrite + Send + Unpin` on both platforms so existing message-handling code is untouched.
- `DaemonServer` — wraps `UnixListener` on Unix and a named-pipe server loop on Windows.
  - `bind() -> (DaemonServer, endpoint_string)` — chooses an endpoint, opens the server, writes the endpoint into the workspace's `DaemonDiscovery` file.
  - `accept() -> io::Result<DaemonStream>` — accepts the next inbound client. **On Windows, this method must pre-create the next `NamedPipeServer` instance before returning the connected one**, otherwise new clients arriving during the accept gap receive `ERROR_PIPE_BUSY`. The standard pattern is: hold a "next" `NamedPipeServer` already in `connect()`-pending state; on connection, swap it out, immediately create a fresh `NamedPipeServer` with the same name to become the new "next", and return the connected one wrapped in `DaemonStream`.
- `daemon_connect() -> Result<DaemonStream, DaemonError>` — reads `DaemonDiscovery` for the current workspace, connects to `endpoint`, returns a typed error.
- `ensure_daemon() -> Result<DaemonStream, DaemonError>` — used by `cmd_dev`. Spawns the daemon binary if no live daemon is reachable, then polls `DaemonDiscovery::load()` up to N times with a small sleep between attempts; once loaded, retries `daemon_connect()`. If the timeout is exceeded, surfaces `DaemonError::NotRunning`. This consolidates the spawn-then-poll loop that today lives inline in `cmd_dev` (which currently checks `socket_path.exists()` then retries).

`DaemonError`:

```rust
pub enum DaemonError {
    /// No DaemonDiscovery file exists, or connect failed before any prior
    /// successful connection / liveness confirmation. Caller should surface
    /// "no daemon running" or trigger ensure_daemon().
    NotRunning,

    /// DaemonDiscovery file exists, but connect failed AFTER the daemon
    /// was previously confirmed running (or no live process matches `pid`).
    /// Stale-detection cleanup has been performed (discovery file deleted).
    Stale,

    /// Connection succeeded but I/O failed mid-session.
    Transport(io::Error),
}
```

The distinction between `NotRunning` and `Stale` is critical on Windows: when a client opens a named pipe during the brief window before the daemon has created the server, `ClientOptions::open()` returns `ERROR_FILE_NOT_FOUND` — this is **not** a stale-endpoint case. Stale detection therefore only triggers when:

- the discovery file exists, AND
- connect fails, AND
- either (a) a previous connection in this process succeeded, or (b) the recorded `pid` is checked and is no longer alive.

Otherwise the connect failure is reported as `NotRunning` and `ensure_daemon()` retries / spawns.

#### 1c. Tokio named-pipe API specifics

Three implementation details that the design must call out, because they will silently produce bugs otherwise:

1. **No `into_split()` on named pipes.** `NamedPipeServer` and `NamedPipeClient` do **not** implement `into_split()`. Use `tokio::io::split(stream)`, which returns generic `ReadHalf<T>` / `WriteHalf<T>` — these are different types from `OwnedReadHalf` / `OwnedWriteHalf` that `UnixStream::into_split()` returns. Existing call sites in `socket.rs` that destructure with `into_split()` must be migrated to `tokio::io::split()` and to the `ReadHalf<DaemonStream>` / `WriteHalf<DaemonStream>` types. (Plan note: introduce a small helper or type aliases in `transport.rs` so the migration is one search-and-replace.)
2. **Accept loop must pre-create the next server instance.** Documented under `accept()` above. The `DaemonServer::new()` constructor creates the **first** `NamedPipeServer`; `accept()` then atomically swaps it out and creates the next one. Failing to do so causes a race where any client connecting between `accept()` returning and the next `NamedPipeServer::new(...)` call gets `ERROR_PIPE_BUSY`.
3. **Client connect during daemon startup returns `ERROR_FILE_NOT_FOUND`, not stale.** As described in the `DaemonError` section.

#### 1d. Named-pipe security

By default, `tokio::net::windows::named_pipe::ServerOptions` creates a pipe with a permissive DACL — **any local user can connect**. On a multi-user host (terminal server, shared CI runner, dev box with multiple interactive sessions), one user's `rage` daemon would be reachable by every other user's session.

**Decision for v1:** accept the default permissive DACL and document the risk. Rationale: rage is a developer tool typically used on single-user workstations; restricting to the current user's SID requires the `windows-sys` `SECURITY_DESCRIPTOR` machinery, which we explicitly avoided in the IPC layer. A future hardening pass can use `ServerOptions::security_attributes()` with an SDDL string like `D:(A;;GA;;;S-1-5-...)` resolved from the current process token (`OpenProcessToken` + `GetTokenInformation(TokenUser)`).

**Required action:** add a `// SECURITY:` comment at the `ServerOptions` construction site referencing the threat model and pointing at the future hardening note. Track as a follow-up issue.

#### 1e. Lockfile / discovery-file atomicity and crash cleanup

- **Atomic create.** Write the discovery file using `OpenOptions::new().create_new(true).write(true).open(...)`. If `create_new` fails with `AlreadyExists`, treat it as "another daemon is starting for this workspace — wait briefly and retry, then fall through to stale detection." This prevents two concurrent daemon starts from corrupting the file mid-write.
- **`Drop`-based cleanup is unreliable.** SIGKILL, hard power-off, OOM kill, and Windows `TerminateProcess` all bypass `Drop`. **Stale detection is the primary cleanup path**, not an edge case. The design must assume the discovery file is routinely left behind by crashed daemons.
- **Ordering invariant for liveness signaling.**
  - On startup: create the named pipe / Unix socket **before** writing the discovery file.
  - On shutdown: delete the discovery file **before** closing the pipe / socket handle.
  - This guarantees that "discovery file exists" is a sound (if not always tight) liveness signal: any client that successfully reads the file and connects will reach a live server, modulo the `ERROR_FILE_NOT_FOUND` startup window handled above.

#### 1f. Modifications to existing files

- `crates/daemon/src/discovery.rs` — rename `unix_socket: PathBuf` to `endpoint: String`; update all readers/writers; bump on-disk schema version (or accept that a one-shot incompatibility on upgrade is fine since these files are ephemeral).
- `crates/daemon/src/socket.rs` — replace `UnixListener` with `DaemonServer`; migrate `into_split()` call sites to `tokio::io::split()`. All message-handling logic is unchanged.
- `crates/daemon/src/daemon.rs` — call `DaemonServer::bind()` and persist returned endpoint via the existing `DaemonDiscovery` write path.
- `crates/cli/src/main.rs` — `cmd_dev()` calls `ensure_daemon()`; `cmd_status()` calls `daemon_connect()` directly. Both surface `DaemonError::NotRunning` / `Stale` as user-facing messages instead of raw `io::Error`.

### 2. Shell Dispatch (`crates/scheduler/src/shell.rs`, new)

Two public functions:

```rust
/// Async variant — for callers that use `tokio::process::Command`.
pub fn command(cmd: &str) -> tokio::process::Command;

/// Sync variant — for callers that use `std::process::Command`
/// (currently `postinstall_cache.rs`).
pub fn std_command(cmd: &str) -> std::process::Command;
```

#### 2a. Behavior

- **Unix:** `Command::new("sh").arg("-c").arg(cmd)`.
- **Windows:** prefer `sh.exe` if found on `PATH` (covers Git for Windows, MSYS2, Cygwin, and WSL-based dev setups — the dominant Windows dev environments). Fall back to `cmd /c` only if `sh.exe` is absent. Cache the `which("sh")` lookup in a `OnceLock<Option<PathBuf>>` so each invocation isn't a fresh PATH scan.

#### 2b. Quoting caveat (`cmd /c` fallback only)

`tokio::process::Command::arg()` and `std::process::Command::arg()` apply Windows' standard `CommandLineToArgvW` quoting rules. **`cmd.exe` then re-parses with its own rules** — caret (`^`), doubled quotes (`""`), and the metacharacters `&`, `|`, `<`, `>`, `(`, `)` all have additional meaning. For commands containing those characters, the round-trip through `Command::arg()` → `cmd.exe` is broken (the typical failure is split commands run as separate cmd statements or quotes being stripped).

For the `cmd /c` branch the implementation must therefore use `std::os::windows::process::CommandExt::raw_arg()` (and the tokio equivalent) to bypass `CommandLineToArgvW` quoting and pass the raw command string to `cmd.exe`:

```rust
#[cfg(windows)]
{
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    c.raw_arg("/c").raw_arg(cmd);
    c
}
```

(Use the appropriate tokio re-export for the async variant.)

#### 2c. Documented limitations

Even with `raw_arg`, **complex POSIX scripts will not work under the `cmd /c` fallback.** Specifically, npm/task scripts authored with POSIX semantics — `&&` chaining (works in modern cmd but fragile), `$VAR` expansion, single-quoted strings, glob expansion, `2>&1` redirection ordering — do not translate. This is a known, accepted limitation for users on bare Windows without Git Bash / MSYS2 / WSL. The error path is "the user installs Git for Windows and the `sh -c` branch takes over." Document this in the user-facing Windows setup notes, not just the design.

#### 2d. Call sites replaced

All seven sites must be migrated:

| File | Location | Note |
|---|---|---|
| `runner.rs` | Line 236 | Pre-install hook |
| `runner.rs` | Line 314 | Post-install hook |
| `runner.rs` | Line 910 | Loose mode task execution |
| `runner.rs` | Line 960 | Sandbox-failure fallback (Loose mode) |
| `runner.rs` | Line 1197 | Post-install hook (second site) |
| `rss_monitor.rs` | Line 87 | RSS sampling command |
| `postinstall_cache.rs` | Line 197 | Uses `std::process::Command`; calls `shell::std_command()` |

#### 2e. Boundary

`run_sandboxed` is **not** touched. On Windows, `inject_and_spawn` already wraps commands appropriately when calling `CreateProcessW`. The `shell` helpers are for Loose-mode, hooks, RSS sampling, and post-install caching only.

### 3. Node Version Manager Paths (`crates/scheduler/src/node_path.rs`, modified)

`find_version_manager_bin` returns a **directory** (callers append the binary name). The Windows entries reflect that.

| Manager | Env var (check first) | Windows path |
|---|---|---|
| fnm | `FNM_DIR` | `%LOCALAPPDATA%\fnm\node-versions\v{ver}\installation\` |
| nvm-windows | `NVM_HOME` | `%APPDATA%\nvm\v{ver}\` |
| Volta | `VOLTA_HOME` | `%LOCALAPPDATA%\Volta\bin\` (shim dir, not per-version image) |

Notes:

- Always check the canonical env var (`FNM_DIR` / `NVM_HOME` / `VOLTA_HOME`) before falling back to the conventional `%LOCALAPPDATA%` / `%APPDATA%` path.
- **fnm on Windows has no `bin/` subdirectory** (unlike Unix); `node.exe` lives directly in `installation\`.
- **nvm-windows version directories are prefixed `v`** (`v20.10.0\`, not `20.10.0\`).
- **Volta on Windows exposes `node.exe` through shims in `%LOCALAPPDATA%\Volta\bin\`**, not per-version image directories. The shim dir is also what gets prepended to `PATH` by Volta itself, so resolving to it is sufficient and correct.
- Use `USERPROFILE` as a fallback for `HOME` in any environment lookup that touches the user home directory.

#### Volta asymmetry note

Volta is currently listed only for Windows. **Add Volta to the Unix list as well** at `~/.volta/bin/` (the shim directory; same model as Windows). This keeps the two platforms symmetric and matches the way Volta actually exposes shims on Linux/macOS. A Volta `tools/image/node/{ver}/bin/` per-version directory exists but is not what's on `PATH`, so the shim dir is the right answer on both platforms.

### 4. Task 7 Integration Test (`crates/sandbox/tests/windows_integration.rs`, new)

```rust
#![cfg(target_os = "windows")]

use serial_test::serial;
use sandbox::AccessEvent;

#[tokio::test]
#[ignore]  // requires DLL built; CI runs with --include-ignored
#[serial]  // mutates RAGE_SANDBOX_DLL_PATH (process-wide env)
async fn dll_injection_produces_file_access_events() {
    // Resolve absolute DLL path at compile time. cargo test -p sandbox
    // runs with CWD = crates/sandbox/, so a workspace-relative path
    // ("target/debug/rage_sandbox.dll") would not resolve.
    let dll_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/rage_sandbox.dll")
        .canonicalize()
        .expect(
            "DLL not found - run `cargo build -p sandbox-windows-detours` first",
        );
    std::env::set_var("RAGE_SANDBOX_DLL_PATH", dll_path);

    // Use a command that goes through CreateFileW + ReadFile, which IS hooked.
    // `cmd /c dir` uses FindFirstFileW/FindNextFileW which may not be hooked.
    // `cmd /c type <file>` calls CreateFileW -> ReadFile reliably.
    let events = sandbox::run_sandboxed(
        "cmd /c type C:\\Windows\\System32\\drivers\\etc\\hosts",
        &[],
    )
    .await
    .expect("run_sandboxed failed");

    // AccessEvent is a struct variant, not a tuple variant.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AccessEvent::Read { path, .. } if path.starts_with("C:\\Windows\\")
        )),
        "expected at least one AccessEvent::Read under C:\\Windows\\, got {:?}",
        events,
    );
}
```

Key fixes captured in the snippet:

- **File-level `#![cfg(target_os = "windows")]`** so the file is skipped wholesale on Unix (the alternative — per-test `#[cfg]` — still requires the file to compile elsewhere, which forces import-level cfg gymnastics).
- **`AccessEvent::Read { path, .. }`** struct-variant pattern (the actual definition; the prior `AccessEvent::Read(p)` tuple-variant form was wrong).
- **Absolute DLL path** computed via `env!("CARGO_MANIFEST_DIR")` so the test works regardless of `cargo test` CWD.
- **`cmd /c type ...`** instead of `cmd /c dir ...` so the command path goes through the hooked `CreateFileW` / `ReadFile` APIs.
- **`#[serial]`** because the test mutates the process-wide env var `RAGE_SANDBOX_DLL_PATH`. Any test in `node_path.rs` that mutates `HOME` or `USERPROFILE` must likewise be `#[serial]`, consistent with the existing `discovery::tests::discovery_roundtrips` pattern.

The existing `inject_and_spawn_cmd_echo_runs_to_completion` test uses a deliberately nonexistent DLL, so hooks never run and no events are produced. This new test exercises the full inject → hook → pipe → event pipeline for the first time.

### 5. CI Hardening (`.github/workflows/ci.yml`, modified)

Three changes to `sandbox-smoke-windows`:

1. **Build the DLL crate explicitly before running the integration test** (without this, `canonicalize()` in the test fails):

   ```yaml
   - run: cargo build -p sandbox-windows-detours
   ```

2. **Run the new Task 7 integration test** with `--include-ignored`:

   ```yaml
   - run: cargo test -p sandbox --test windows_integration -- --include-ignored
   ```

   `RAGE_SANDBOX_DLL_PATH` is resolved at test runtime from `CARGO_MANIFEST_DIR`, so no env injection is required at the YAML level.

3. **Add an exit-code assertion** after the existing PowerShell smoke invocation, so a `rage.exe` crash fails the job:

   ```powershell
   if ($LASTEXITCODE -ne 0) { exit 1 }
   ```

## Data Flow

**Daemon connection (both platforms, unified through `discovery.rs`):**

```
daemon start (per workspace):
  DaemonServer::bind()
      |-- pick endpoint   (unix: socket path / win: \\.\pipe\rage-daemon-{hash}-{nonce})
      |-- open server     (UnixListener / NamedPipeServer)   <-- BEFORE writing discovery
      `-- write DaemonDiscovery { endpoint, pid, ... }
                          atomically (create_new) at daemons_dir()/{hash}.json

cli connect (cmd_dev / cmd_status):
  daemon_connect()
      |-- read DaemonDiscovery for current workspace
      |-- connect to endpoint
      |     |-- ok                 -> return DaemonStream
      |     |-- ERROR_FILE_NOT_FOUND
      |     |     during startup   -> DaemonError::NotRunning (no cleanup)
      |     |-- connect fails AND
      |     |   pid is dead OR
      |     |   prior conn existed -> unlink discovery, DaemonError::Stale
      |     `-- other io error     -> DaemonError::Transport(err)
      `-- ensure_daemon() (cmd_dev only):
            spawn daemon binary, poll DaemonDiscovery::load() up to N times
            with sleep, retry daemon_connect(); on timeout -> NotRunning
```

**Loose-mode / hook task execution (Windows):**

```
runner.rs hook step
   `-- shell::command("npm run build")
        |-- if sh.exe on PATH: sh -c "npm run build"
        `-- else:              cmd /c (via raw_arg) "npm run build"
            `-- tokio::process::Command spawn -> child runs to completion
```

**Sandboxed task execution (Windows, unchanged):**

```
runner.rs sandbox step
   `-- sandbox::run_sandboxed(cmd, env)
        `-- inject_and_spawn (CreateProcessW with `cmd /c <cmd>`)
             `-- DLL hooks -> named pipe -> AccessEvent stream
```

## Error Handling

- `DaemonError::NotRunning` — no discovery file, or connect failed during a startup window. `cmd_status` prints "no daemon running"; `cmd_dev` triggers `ensure_daemon()`.
- `DaemonError::Stale` — discovery file existed but the daemon is gone (dead pid or post-success connect failure). The discovery file has been unlinked; caller surfaces a "stale daemon entry, please retry" message.
- `DaemonError::Transport(io::Error)` — mid-session I/O failure; propagated unchanged.
- Stale-cleanup unlink is best-effort — if the unlink fails we still propagate `Stale`.
- `shell::command()` / `shell::std_command()` return a configured `Command`; existing spawn error handling in callers is preserved verbatim.
- The Task 7 test panics with a descriptive message including the captured event list when no read under `C:\Windows\` is observed, making CI failures self-explanatory.
- Node-path lookup remains a best-effort fallback chain: missing env vars or non-existent paths simply skip that candidate, matching existing Unix behavior.

## Testing Strategy

| Area | Tests |
|------|-------|
| Transport | Unit: `DaemonDiscovery` round-trip with new `endpoint` field; atomic-create-new contention; stale detection (pre-write a discovery file with a dead pid, assert `Stale`); startup-window case (write discovery before pipe is created, assert `NotRunning` not `Stale`). Integration on both platforms: `bind` → write discovery → `connect` → echo → drop → discovery gone. Windows-specific: client connecting during accept-gap does not get `ERROR_PIPE_BUSY`. |
| Shell dispatch | Unit: `shell::command("echo hello")` and `shell::std_command("echo hello")` spawn and exit 0, with `#[cfg(unix)]` and `#[cfg(windows)]` variants. Windows: a test verifying that when `sh.exe` is on PATH the `sh` branch is taken; when it isn't, `cmd /c` (via `raw_arg`) is taken. Quoting test for a command containing `&` to confirm `raw_arg` preserves it. |
| Node version paths | Unit tests with temp `LOCALAPPDATA` / `APPDATA` / `USERPROFILE` and overridden `FNM_DIR` / `NVM_HOME` / `VOLTA_HOME` to verify each manager resolves to the correct directory; `HOME`-unset → `USERPROFILE` fallback; env var takes precedence over conventional path. All env-mutating tests `#[serial]`. |
| Task 7 / CI | New `windows_integration.rs` integration test (file-level `#![cfg(target_os = "windows")]`, `#[ignore]`, `#[serial]`, run with `--include-ignored` in CI). CI step `cargo build -p sandbox-windows-detours` runs first. Exit-code assertion guards the existing PowerShell smoke step against silent `rage.exe` crashes. |

All four work areas keep their tests local to the crate they touch — no cross-crate test rewiring is required.

## Open Questions

None. Key decisions made during design:

- IPC transport — `DaemonStream` / `DaemonServer` abstraction layered over the existing per-workspace `DaemonDiscovery` (extended with a `endpoint: String` field); no new global lockfile, no new `windows-sys` calls.
- Named-pipe security — accept default DACL for v1, document the multi-user reachability risk, track restricted-DACL hardening as a follow-up.
- Shell — prefer `sh -c` even on Windows when `sh.exe` is on PATH; fall back to `cmd /c` with `raw_arg` quoting. Complex POSIX scripts on bare Windows are an accepted, documented limitation.
- Node paths — env vars first, conventional paths second; Volta listed for both platforms as the shim directory.
- Discovery is preserved per-workspace — multiple `rage dev` instances in different worktrees continue to coexist on Windows just as they do on Unix.
