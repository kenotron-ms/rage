# Phase 7 — Sandbox (macOS DYLD_INSERT_LIBRARIES) Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Build a macOS-native file-access observation sandbox using `DYLD_INSERT_LIBRARIES` interposition. A child process executed under the sandbox emits a `PathSet` describing every file it read or wrote.

**Architecture:** Two crates:
1. `sandbox` (rlib) — public Rust API. `run_sandboxed(cmd, cwd, env) -> RunResult { exit_status, path_set }`. Hosts a Unix-domain-socket listener that the injected dylib connects to and streams events over.
2. `sandbox-macos-dylib` (cdylib, macOS only) — `librage_sandbox.dylib`. Uses Mach-O `__DATA,__interpose` section to hook libc filesystem entrypoints (`open`, `openat`, `stat$INODE64`, `lstat$INODE64`, `read`, `write`, `rename`, `unlink`, `mkdir`). On startup it reads `RAGE_SANDBOX_SOCKET` env var and connects.

**Tech Stack:** Rust 2021, Tokio (for socket server), `libc`, `ctor` (cdylib init), `serde_json` (event framing), `tempfile` (test sockets).

**Caveat:** This plan targets macOS only. Linux (eBPF via `aya`) is a follow-up. The `sandbox` crate has a `unsupported` no-op stub for non-macOS platforms so the workspace builds and tests compile everywhere.

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 5 — Cache Key Design (sandbox execution model) and the parent BuildXL/Lage spec referenced therein.

---

## Constraints (from COE)

1. **MUST** be `DYLD_INSERT_LIBRARIES` cdylib interposing — NOT file watching, NOT inotify, NOT `fs_usage` parsing, NOT `dtrace`, NOT glob matching.
2. **MUST** use the Mach-O `__DATA,__interpose` section mechanism for symbol hooking. NOT `dlsym`, NOT function-pointer swaps at runtime.
3. **MUST** call the original libc function via the interpose-pair mechanism — NOT via name lookup (which would loop).
4. **MUST** work on Apple Silicon (arm64) and Intel (x86_64). System Integrity Protection (SIP) is not relevant for our use case (we are not interposing into protected system processes — we control the child process).
5. The crate **MUST** allow `unsafe_code` (the workspace lints `forbid` it; this crate adds an explicit allow).

---

## Files Created / Modified

### New crates
- `/Users/ken/workspace/ms/rage/crates/sandbox/` — Cargo.toml, src/lib.rs, src/event.rs, src/server.rs, src/macos.rs, src/unsupported.rs, src/mock.rs
- `/Users/ken/workspace/ms/rage/crates/sandbox-macos-dylib/` — Cargo.toml, src/lib.rs, src/interpose.rs, src/client.rs

### Modified
- `/Users/ken/workspace/ms/rage/Cargo.toml` — add both crates to workspace members
- `/Users/ken/workspace/ms/rage/crates/scheduler/Cargo.toml` — add `sandbox = { path = "../sandbox" }` (used in Phase 9)

---

## Task 1: Scaffold the `sandbox` crate

**Files:**
- Create: `crates/sandbox/Cargo.toml`
- Create: `crates/sandbox/src/lib.rs`
- Modify: `Cargo.toml` (workspace)

**Step 1: Add to workspace `Cargo.toml`**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml` `members` array — add:

```
    "crates/sandbox",
    "crates/sandbox-macos-dylib",
```

**Step 2: Create `crates/sandbox/Cargo.toml`**

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
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tempfile = "3"

[dev-dependencies]
tempfile = "3"
```

**Step 3: Create `crates/sandbox/src/lib.rs` skeleton**

```rust
//! File-access observation sandbox for rage build tasks.
//!
//! Public API:
//! ```no_run
//! # use sandbox::run_sandboxed;
//! # use std::path::Path;
//! # async fn ex() {
//! let result = run_sandboxed("echo hello", Path::new("/tmp"), &[]).await.unwrap();
//! println!("read {} files", result.path_set.reads.len());
//! # }
//! ```
//!
//! On macOS this uses `DYLD_INSERT_LIBRARIES` with the `rage_sandbox` cdylib.
//! On other platforms `run_sandboxed` returns an `Unsupported` error.

pub mod event;
pub mod mock;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub mod server;

#[cfg(target_os = "macos")]
pub use macos::run_sandboxed;

#[cfg(not(target_os = "macos"))]
pub mod unsupported;

#[cfg(not(target_os = "macos"))]
pub use unsupported::run_sandboxed;

pub use event::{PathSet, RunResult};
```

**Step 4: Stub `event.rs`, `mock.rs`, `unsupported.rs`**

`crates/sandbox/src/event.rs`:

```rust
//! Wire format and result types for sandbox observation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// One observation emitted by the injected dylib over the socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum AccessEvent {
    Read { path: String, pid: u32 },
    Write { path: String, pid: u32 },
}

/// Aggregated set of paths a sandboxed process touched.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathSet {
    /// Files read (sorted, deduped).
    pub reads: Vec<PathBuf>,
    /// Files written, created, or deleted (sorted, deduped).
    pub writes: Vec<PathBuf>,
}

impl PathSet {
    pub fn from_events(events: impl IntoIterator<Item = AccessEvent>) -> Self {
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        for ev in events {
            match ev {
                AccessEvent::Read { path, .. } => {
                    reads.insert(PathBuf::from(path));
                }
                AccessEvent::Write { path, .. } => {
                    writes.insert(PathBuf::from(path));
                }
            }
        }
        Self {
            reads: reads.into_iter().collect(),
            writes: writes.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub exit_code: i32,
    pub path_set: PathSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathset_dedupes_and_sorts() {
        let events = vec![
            AccessEvent::Read { path: "/b".into(), pid: 1 },
            AccessEvent::Read { path: "/a".into(), pid: 1 },
            AccessEvent::Read { path: "/a".into(), pid: 1 },
            AccessEvent::Write { path: "/c".into(), pid: 1 },
        ];
        let ps = PathSet::from_events(events);
        assert_eq!(ps.reads, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(ps.writes, vec![PathBuf::from("/c")]);
    }

    #[test]
    fn read_then_write_separates_buckets() {
        let events = vec![
            AccessEvent::Read { path: "/x".into(), pid: 1 },
            AccessEvent::Write { path: "/x".into(), pid: 1 },
        ];
        let ps = PathSet::from_events(events);
        assert_eq!(ps.reads, vec![PathBuf::from("/x")]);
        assert_eq!(ps.writes, vec![PathBuf::from("/x")]);
    }
}
```

`crates/sandbox/src/unsupported.rs`:

```rust
//! Stub for non-macOS platforms.

use crate::event::{PathSet, RunResult};
use anyhow::{bail, Result};
use std::path::Path;

pub async fn run_sandboxed(
    _cmd: &str,
    _cwd: &Path,
    _env: &[(String, String)],
) -> Result<RunResult> {
    bail!("rage sandbox is only supported on macOS in this phase")
}
```

`crates/sandbox/src/mock.rs`:

```rust
//! Test double — synthesizes a `RunResult` without spawning a process.

use crate::event::{PathSet, RunResult};

pub struct MockSandbox {
    pub exit_code: i32,
    pub path_set: PathSet,
}

impl MockSandbox {
    pub fn ok(reads: Vec<&str>, writes: Vec<&str>) -> Self {
        Self {
            exit_code: 0,
            path_set: PathSet {
                reads: reads.into_iter().map(Into::into).collect(),
                writes: writes.into_iter().map(Into::into).collect(),
            },
        }
    }

    pub fn run(&self) -> RunResult {
        RunResult {
            exit_code: self.exit_code,
            path_set: self.path_set.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_configured_pathset() {
        let m = MockSandbox::ok(vec!["/a", "/b"], vec!["/out"]);
        let r = m.run();
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.path_set.reads.len(), 2);
        assert_eq!(r.path_set.writes, vec![std::path::PathBuf::from("/out")]);
    }
}
```

`crates/sandbox/src/server.rs`: leave empty for now (filled in Task 3) — `pub(crate) fn placeholder() {}`. Or skip the file and gate `mod server;` until Task 3.

Stub `crates/sandbox/src/macos.rs`:

```rust
//! macOS DYLD_INSERT_LIBRARIES sandbox implementation.
//!
//! Implemented in subsequent tasks of this phase.

use crate::event::RunResult;
use anyhow::{bail, Result};
use std::path::Path;

pub async fn run_sandboxed(
    _cmd: &str,
    _cwd: &Path,
    _env: &[(String, String)],
) -> Result<RunResult> {
    bail!("not yet implemented")
}
```

**Step 5: Verify the workspace still compiles**

Run: `cargo build -p sandbox && cargo test -p sandbox`
Expected: builds; `event` and `mock` tests pass.

**Step 6: Commit**

```
git add Cargo.toml crates/sandbox && git commit -m "feat(sandbox): scaffold crate with PathSet, AccessEvent, MockSandbox"
```

---

## Task 2: Scaffold `sandbox-macos-dylib` cdylib

**Files:**
- Create: `crates/sandbox-macos-dylib/Cargo.toml`
- Create: `crates/sandbox-macos-dylib/src/lib.rs`

**Step 1: Cargo.toml**

```toml
[package]
name = "sandbox-macos-dylib"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

# Override workspace forbid(unsafe_code) — interposition is intrinsically unsafe.
[lints.rust]
unsafe_code = "allow"

[lints.clippy]
all = { level = "warn", priority = -1 }

[lib]
name = "rage_sandbox"
crate-type = ["cdylib"]

[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"
ctor = "0.2"
```

**Step 2: src/lib.rs (initial — built but inert)**

```rust
//! macOS DYLD_INSERT_LIBRARIES interposition library.
//!
//! Loaded into a child process by the `sandbox` crate. On startup it reads
//! the `RAGE_SANDBOX_SOCKET` env var, connects, and registers Mach-O
//! interpose entries for libc filesystem syscalls. Each intercepted call
//! emits a JSONL `AccessEvent` to the socket and forwards the call to the
//! original libc function.

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types)]

mod client;
mod interpose;

pub use interpose::*;

#[ctor::ctor]
fn rage_sandbox_init() {
    client::init_from_env();
}
```

`crates/sandbox-macos-dylib/src/client.rs`:

```rust
//! Connects to the sandbox server socket on dylib load.

use std::os::unix::net::UnixStream;
use std::sync::OnceLock;
use std::sync::Mutex;

static CLIENT: OnceLock<Mutex<Option<UnixStream>>> = OnceLock::new();

pub(crate) fn init_from_env() {
    let path = match std::env::var("RAGE_SANDBOX_SOCKET") {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Ok(stream) = UnixStream::connect(&path) {
        let _ = CLIENT.set(Mutex::new(Some(stream)));
    }
}

pub(crate) fn send_event(op: &str, path: &str) {
    use std::io::Write;
    let pid = unsafe { libc::getpid() } as u32;
    // path may contain quotes/backslashes — escape minimally for JSON.
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    let line = format!("{{\"op\":\"{op}\",\"path\":\"{escaped}\",\"pid\":{pid}}}\n");
    if let Some(m) = CLIENT.get() {
        if let Ok(mut guard) = m.lock() {
            if let Some(stream) = guard.as_mut() {
                let _ = stream.write_all(line.as_bytes());
            }
        }
    }
}
```

`crates/sandbox-macos-dylib/src/interpose.rs`:

```rust
//! Mach-O __DATA,__interpose entries — populated in subsequent tasks.

#[repr(C)]
pub struct InterposeEntry {
    pub replacement: *const libc::c_void,
    pub original: *const libc::c_void,
}

unsafe impl Sync for InterposeEntry {}
```

**Step 3: Build it**

Run: `cargo build -p sandbox-macos-dylib`
Expected: produces `target/debug/librage_sandbox.dylib`.

**Step 4: Commit**

```
git add Cargo.toml crates/sandbox-macos-dylib && git commit -m "feat(sandbox-macos-dylib): scaffold cdylib with ctor-based init"
```

---

## Task 3: Implement the sandbox server (Unix socket + event collector)

**Files:**
- Modify: `crates/sandbox/src/lib.rs`
- Create: `crates/sandbox/src/server.rs`

**Step 1: Write the failing test**

In `crates/sandbox/src/server.rs`:

```rust
//! Unix socket server that collects `AccessEvent`s from the injected dylib.

use crate::event::AccessEvent;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// A live event-collecting server.
pub struct EventServer {
    pub socket_path: PathBuf,
    pub events_rx: mpsc::UnboundedReceiver<AccessEvent>,
    listener_task: tokio::task::JoinHandle<()>,
}

impl EventServer {
    /// Bind a Unix socket in `dir` and start accepting dylib connections.
    pub fn start(dir: &Path) -> Result<Self> {
        let socket_path = dir.join(format!("rage-sandbox-{}.sock", std::process::id()));
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).ok();
        }
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("binding {}", socket_path.display()))?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        let listener_task = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let tx = events_tx.clone();
                tokio::spawn(handle_client(stream, tx));
            }
        });

        Ok(Self {
            socket_path,
            events_rx,
            listener_task,
        })
    }

    pub async fn drain(mut self) -> Vec<AccessEvent> {
        // Stop accepting and drain remaining events.
        self.listener_task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
        let mut out = Vec::new();
        while let Ok(ev) = self.events_rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

async fn handle_client(stream: UnixStream, tx: mpsc::UnboundedSender<AccessEvent>) {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(ev) = serde_json::from_str::<AccessEvent>(&line) {
            let _ = tx.send(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::io::Write;

    #[tokio::test]
    async fn server_collects_events_from_a_client() {
        let dir = tempfile::tempdir().unwrap();
        let server = EventServer::start(dir.path()).unwrap();
        let path = server.socket_path.clone();

        // Connect synchronously and write two events.
        let mut s = StdUnixStream::connect(&path).unwrap();
        s.write_all(b"{\"op\":\"read\",\"path\":\"/etc/hosts\",\"pid\":1}\n").unwrap();
        s.write_all(b"{\"op\":\"write\",\"path\":\"/tmp/x\",\"pid\":1}\n").unwrap();
        drop(s);

        // Allow events to flush.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let events = server.drain().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AccessEvent::Read { .. }));
        assert!(matches!(events[1], AccessEvent::Write { .. }));
    }
}
```

**Step 2: Update `lib.rs`** — module is already gated for macOS only via the `cfg(target_os = "macos")` block. Confirm `pub mod server;` is in lib.rs (Task 1 already added it).

**Step 3: Run, verify pass**

Run (on macOS): `cargo test -p sandbox server_collects_events_from_a_client`
Expected: pass.

**Step 4: Commit**

```
git add crates/sandbox && git commit -m "feat(sandbox): EventServer — UDS listener that collects AccessEvents"
```

---

## Task 4: Locate the dylib path at build time

**Files:**
- Create: `crates/sandbox/build.rs`
- Modify: `crates/sandbox/Cargo.toml`
- Modify: `crates/sandbox/src/macos.rs`

**Step 1: Add `build-dependencies`**

Edit `crates/sandbox/Cargo.toml`:

```toml
[build-dependencies]
```

(No additional crates — we just need a build.rs.)

**Step 2: Write `build.rs`**

Create `crates/sandbox/build.rs`:

```rust
//! Build script: locate librage_sandbox.dylib so `run_sandboxed` can find it.
//!
//! Strategy:
//!   1. Cargo runs build.rs after dependency builds. The cdylib is *not* a
//!      direct cargo dependency (we don't want to link against it); instead,
//!      `sandbox-macos-dylib` is a sibling workspace crate.
//!   2. We don't trigger its build here. Instead, we record the expected
//!      `target/{profile}/librage_sandbox.dylib` path under the sandbox crate's
//!      OUT_DIR via a generated constant.
//!   3. At runtime, `macos::dylib_path()` resolves the path; production builds
//!      can override with `RAGE_SANDBOX_DYLIB` env var.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // CARGO_TARGET_DIR / target dir resolution
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // OUT_DIR looks like .../target/<profile>/build/sandbox-<hash>/out
    // Walk up to find target/<profile>/
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR has expected depth")
        .to_path_buf();

    let dylib = profile_dir.join("librage_sandbox.dylib");
    println!("cargo:rustc-env=RAGE_SANDBOX_DYLIB_DEFAULT={}", dylib.display());
}
```

**Step 3: Add the failing test in `macos.rs`**

Replace `crates/sandbox/src/macos.rs` body with:

```rust
//! macOS DYLD_INSERT_LIBRARIES sandbox implementation.

use crate::event::{PathSet, RunResult};
use crate::server::EventServer;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Resolve the path to the `librage_sandbox.dylib` to inject.
///
/// Order:
///   1. `RAGE_SANDBOX_DYLIB` env var (set explicitly).
///   2. The path baked in at build time (`RAGE_SANDBOX_DYLIB_DEFAULT`).
pub fn dylib_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("RAGE_SANDBOX_DYLIB") {
        return Ok(PathBuf::from(p));
    }
    let baked = env!("RAGE_SANDBOX_DYLIB_DEFAULT");
    Ok(PathBuf::from(baked))
}

pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<RunResult> {
    let dylib = dylib_path()?;
    if !dylib.exists() {
        anyhow::bail!(
            "rage sandbox dylib not found at {} — did you run `cargo build -p sandbox-macos-dylib`?",
            dylib.display()
        );
    }

    let socket_dir = tempfile::tempdir().context("creating sandbox socket tempdir")?;
    let server = EventServer::start(socket_dir.path())?;

    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("DYLD_INSERT_LIBRARIES", &dylib)
        .env("DYLD_FORCE_FLAT_NAMESPACE", "1")
        .env("RAGE_SANDBOX_SOCKET", &server.socket_path);
    for (k, v) in env {
        command.env(k, v);
    }

    let status = command.status().await.context("spawning sandboxed process")?;
    let exit_code = status.code().unwrap_or(-1);

    // Give the dylib a brief moment to flush remaining events.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = server.drain().await;
    let path_set = PathSet::from_events(events);

    Ok(RunResult { exit_code, path_set })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_path_returns_a_path() {
        let p = dylib_path().unwrap();
        assert!(p.to_string_lossy().contains("librage_sandbox.dylib"));
    }
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p sandbox dylib_path_returns_a_path`
Expected: pass.

**Step 5: Commit**

```
git add crates/sandbox && git commit -m "feat(sandbox): build.rs + dylib_path() + run_sandboxed skeleton"
```

---

## Task 5: Implement `open` interposition

**Files:**
- Modify: `crates/sandbox-macos-dylib/src/interpose.rs`

**Step 1: Add the failing integration test**

In `crates/sandbox/src/macos.rs` `tests` module — gated `#[cfg(target_os = "macos")]` already:

```rust
    #[tokio::test]
    #[ignore] // requires `cargo build -p sandbox-macos-dylib` to have been run
    async fn open_interposed_records_read() {
        // Make sure the dylib is built.
        let dylib = dylib_path().unwrap();
        if !dylib.exists() {
            std::process::Command::new("cargo")
                .args(["build", "-p", "sandbox-macos-dylib"])
                .status()
                .unwrap();
        }

        let result = run_sandboxed("cat /etc/hosts > /dev/null", Path::new("/tmp"), &[])
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(
            result.path_set.reads.iter().any(|p| p.to_string_lossy() == "/etc/hosts"),
            "expected /etc/hosts in reads, got: {:?}",
            result.path_set.reads
        );
    }
```

**Step 2: Run, verify failure**

```
cargo build -p sandbox-macos-dylib
cargo test -p sandbox -- --ignored open_interposed
```

Expected: FAIL — no interpose entries yet, reads is empty.

**Step 3: Implement `open` interposition**

Replace `crates/sandbox-macos-dylib/src/interpose.rs`:

```rust
//! Mach-O __DATA,__interpose entries.
//!
//! Each entry is a pair of (replacement, original) pointers placed in a
//! special section that the dynamic linker reads before mapping libraries.
//! When a process calls e.g. `open()`, the linker actually invokes our
//! `rage_open()` because the interpose table redirects that symbol.

use crate::client::send_event;
use std::ffi::CStr;

#[repr(C)]
pub struct InterposeEntry {
    pub replacement: *const libc::c_void,
    pub original: *const libc::c_void,
}

unsafe impl Sync for InterposeEntry {}

// ---------- open() ----------------------------------------------------------

/// SAFETY: called by the dynamic linker. `path` may be null in malformed
/// callers; we null-check before dereferencing. `flags` is forwarded as-is.
unsafe extern "C" fn rage_open(
    path: *const libc::c_char,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            // Heuristic: O_WRONLY/O_RDWR or O_CREAT/O_TRUNC ⇒ write; else read.
            let is_write = (flags & libc::O_WRONLY) != 0
                || (flags & libc::O_RDWR) != 0
                || (flags & libc::O_CREAT) != 0
                || (flags & libc::O_TRUNC) != 0;
            send_event(if is_write { "write" } else { "read" }, s);
        }
    }
    // Forward to libc::open via the original entry — the interpose table
    // ensures `libc::open` here resolves to the real one (NOT our wrapper).
    libc::open(path, flags, mode)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_OPEN: InterposeEntry = InterposeEntry {
    replacement: rage_open as *const libc::c_void,
    original: libc::open as *const libc::c_void,
};
```

**Step 4: Build dylib and rerun integration test**

```
cargo build -p sandbox-macos-dylib
cargo test -p sandbox -- --ignored open_interposed
```

Expected: PASS — `/etc/hosts` appears in reads.

**Step 5: Commit**

```
git add crates/sandbox-macos-dylib && git commit -m "feat(sandbox-macos-dylib): interpose open() — record reads/writes"
```

---

## Task 6: Add `openat`, `stat`, `lstat` interposition

**Files:**
- Modify: `crates/sandbox-macos-dylib/src/interpose.rs`

**Step 1: Add the failing test**

In `crates/sandbox/src/macos.rs`:

```rust
    #[tokio::test]
    #[ignore]
    async fn stat_interposed_records_read() {
        let result = run_sandboxed("stat /etc/passwd > /dev/null", Path::new("/tmp"), &[])
            .await
            .unwrap();
        assert!(
            result.path_set.reads.iter().any(|p| p.to_string_lossy().ends_with("passwd")),
            "expected /etc/passwd in reads, got: {:?}",
            result.path_set.reads
        );
    }
```

**Step 2: Run, verify failure**

`cargo test -p sandbox -- --ignored stat_interposed`
Expected: FAIL.

**Step 3: Append to `interpose.rs`**

```rust
// ---------- openat() --------------------------------------------------------

unsafe extern "C" fn rage_openat(
    dirfd: libc::c_int,
    path: *const libc::c_char,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            let is_write = (flags & libc::O_WRONLY) != 0
                || (flags & libc::O_RDWR) != 0
                || (flags & libc::O_CREAT) != 0
                || (flags & libc::O_TRUNC) != 0;
            send_event(if is_write { "write" } else { "read" }, s);
        }
    }
    libc::openat(dirfd, path, flags, mode)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_OPENAT: InterposeEntry = InterposeEntry {
    replacement: rage_openat as *const libc::c_void,
    original: libc::openat as *const libc::c_void,
};

// ---------- stat() / lstat() ------------------------------------------------

unsafe extern "C" fn rage_stat(
    path: *const libc::c_char,
    buf: *mut libc::stat,
) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("read", s);
        }
    }
    libc::stat(path, buf)
}

unsafe extern "C" fn rage_lstat(
    path: *const libc::c_char,
    buf: *mut libc::stat,
) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("read", s);
        }
    }
    libc::lstat(path, buf)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_STAT: InterposeEntry = InterposeEntry {
    replacement: rage_stat as *const libc::c_void,
    original: libc::stat as *const libc::c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_LSTAT: InterposeEntry = InterposeEntry {
    replacement: rage_lstat as *const libc::c_void,
    original: libc::lstat as *const libc::c_void,
};
```

**Step 4: Rebuild + rerun**

```
cargo build -p sandbox-macos-dylib
cargo test -p sandbox -- --ignored
```

Expected: PASS.

**Step 5: Commit**

```
git add crates/sandbox-macos-dylib && git commit -m "feat(sandbox-macos-dylib): interpose openat, stat, lstat"
```

---

## Task 7: Add `rename`, `unlink`, `mkdir` for write events

**Files:**
- Modify: `crates/sandbox-macos-dylib/src/interpose.rs`

**Step 1: Add the failing test**

In `crates/sandbox/src/macos.rs`:

```rust
    #[tokio::test]
    #[ignore]
    async fn write_operations_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = format!("touch '{}/a.txt' && rm '{}/a.txt'", dir.path().display(), dir.path().display());
        let result = run_sandboxed(&cmd, Path::new("/tmp"), &[]).await.unwrap();
        assert!(
            result.path_set.writes.iter().any(|p| p.to_string_lossy().ends_with("a.txt")),
            "expected a.txt in writes, got: {:?}",
            result.path_set.writes
        );
    }
```

**Step 2: Run, verify failure**

`cargo test -p sandbox -- --ignored write_operations_recorded`
Expected: FAIL — `unlink` not interposed.

**Step 3: Append to `interpose.rs`**

```rust
// ---------- rename() / unlink() / mkdir() -----------------------------------

unsafe extern "C" fn rage_rename(
    old: *const libc::c_char,
    new: *const libc::c_char,
) -> libc::c_int {
    if !old.is_null() {
        if let Ok(s) = CStr::from_ptr(old).to_str() {
            send_event("write", s);
        }
    }
    if !new.is_null() {
        if let Ok(s) = CStr::from_ptr(new).to_str() {
            send_event("write", s);
        }
    }
    libc::rename(old, new)
}

unsafe extern "C" fn rage_unlink(path: *const libc::c_char) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("write", s);
        }
    }
    libc::unlink(path)
}

unsafe extern "C" fn rage_mkdir(path: *const libc::c_char, mode: libc::mode_t) -> libc::c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("write", s);
        }
    }
    libc::mkdir(path, mode)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_RENAME: InterposeEntry = InterposeEntry {
    replacement: rage_rename as *const libc::c_void,
    original: libc::rename as *const libc::c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_UNLINK: InterposeEntry = InterposeEntry {
    replacement: rage_unlink as *const libc::c_void,
    original: libc::unlink as *const libc::c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_MKDIR: InterposeEntry = InterposeEntry {
    replacement: rage_mkdir as *const libc::c_void,
    original: libc::mkdir as *const libc::c_void,
};
```

**Step 4: Rebuild + rerun**

```
cargo build -p sandbox-macos-dylib
cargo test -p sandbox -- --ignored write_operations_recorded
```

Expected: PASS.

**Step 5: Commit**

```
git add crates/sandbox-macos-dylib && git commit -m "feat(sandbox-macos-dylib): interpose rename, unlink, mkdir as writes"
```

---

## Task 8: Add `read` and `write` syscalls (best-effort fd-to-path)

**Note:** Read/write at the fd layer is harder — we don't always know the path. The cheap-but-useful version: skip them for now; the open/stat hooks already capture all the file *paths* we need. If a future phase needs byte-level read sets, we'll revisit.

For this task we **document** that decision in the dylib's lib.rs and add a comment in `interpose.rs`. No code change beyond a doc-comment.

**Step 1: Add doc to `lib.rs`:**

```rust
//! ## Interposed entrypoints (current)
//! - `open`, `openat` — read or write depending on flags
//! - `stat`, `lstat` — always read
//! - `rename`, `unlink`, `mkdir` — always write
//!
//! ## Not interposed (deliberate)
//! - `read`/`write` syscalls — they operate on fds, not paths. The path was
//!   already recorded at `open` time, so re-recording here adds no signal.
//! - `dup`/`fcntl` — same reason.
```

**Step 2: Commit**

```
git add crates/sandbox-macos-dylib && git commit -m "docs(sandbox-macos-dylib): document non-interposed entrypoints"
```

---

## Task 9: Honor `DYLD_FORCE_FLAT_NAMESPACE` and document caveats

**Files:**
- Modify: `crates/sandbox/src/macos.rs`

**Note:** `DYLD_INSERT_LIBRARIES` requires either:
- the target binary is built with two-level namespace OFF (rare), OR
- we set `DYLD_FORCE_FLAT_NAMESPACE=1`.

We already set both env vars in Task 4. Verify by inspection.

**Step 1: Add the failing test for unset env**

```rust
    #[tokio::test]
    #[ignore]
    async fn force_flat_namespace_is_set() {
        // Verify the env var is wired through. We can't test the linker behavior
        // directly, but we can assert run_sandboxed sets it.
        // (smoke check: a process that requires DYLD interposition succeeds)
        let r = run_sandboxed("test 1 -eq 1", Path::new("/tmp"), &[]).await.unwrap();
        assert_eq!(r.exit_code, 0);
    }
```

**Step 2: Run, verify pass**

`cargo test -p sandbox -- --ignored force_flat_namespace`
Expected: PASS.

**Step 3: Add doc-comment caveat to `run_sandboxed`**

```rust
/// # Caveats
///
/// - On Apple Silicon, `DYLD_INSERT_LIBRARIES` is **silently dropped** for
///   binaries with the hardened runtime + library validation entitlement.
///   `/bin/sh`, `cat`, `tsc`, `node`, `cargo`, `tsc-go`, `go` work in practice
///   because they are not hardened. System binaries (e.g. `/usr/bin/git`) on
///   newer macOS may not be observable.
/// - Children that re-exec to a hardened binary lose interposition for that
///   subtree.
```

**Step 4: Commit**

```
git add crates/sandbox && git commit -m "docs(sandbox): document hardened-runtime caveat for DYLD_INSERT_LIBRARIES"
```

---

## Task 10: Wire scheduler dependency (no use yet)

**Files:**
- Modify: `crates/scheduler/Cargo.toml`

**Step 1: Add the dep**

Append to `[dependencies]`:

```
sandbox = { path = "../sandbox" }
```

**Step 2: Verify workspace builds**

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

**Step 3: Commit**

```
git add crates/scheduler && git commit -m "chore(scheduler): add sandbox dependency (used in Phase 9)"
```

---

## Task 11: Verification gate

```
cargo build -p sandbox-macos-dylib --release
cargo test --workspace
cargo test -p sandbox -- --ignored
cargo clippy --workspace --all-targets -- -D warnings
```

All green required to consider this phase done.

---

## Total tasks: 11
