# Phase 10 — Daemon (Desired State + Reconciliation Loop) Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Build a long-lived rage daemon that holds *desired state* per workspace, runs a reconciliation loop, and exposes a Unix-domain socket for CLI ↔ daemon IPC. This is the central coordination primitive of the system.

**Architecture (per design doc Section 1):**
- One daemon per workspace, identified by a hash of the workspace path.
- Discovery file at `~/.rage/daemons/{workspace_hash}.json` — `{pid, unix_socket, http_port, start_time, version}` (HTTP port wired in Phase 11).
- Unix socket at `~/.rage/daemons/{workspace_hash}.sock` — newline-delimited JSON messages.
- Three-state model: `Converging | Ready | Blocked`.
- Reconciliation loop: receives `SetDesiredState`, computes which tasks to run, dispatches to scheduler, watches files (notify crate), re-converges on change.
- Three-hour idle shutdown.
- Detached child startup: CLI uses `Command::new("rage").arg("daemon").spawn()` then drops the handle (Rust analog of Nx's `unref()`).

**Tech Stack:** Rust 2021, Tokio (full), `serde` / `serde_json`, `notify`, `anyhow`, `nix` (for `setsid`/double-fork detach).

**Note on detachment:** Use detached child process + drop handle pattern (same conceptually as Nx daemon). NO systemd. NO launchd. Just a detached process the CLI doesn't wait on.

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 1 — Daemon: Desired State & Reconciliation Loop.

---

## Constraints (from COE)

1. CLI ↔ daemon IPC **MUST** be Unix domain socket. NOT HTTP polling. (HTTP is a separate, browser-only interface — Phase 11.)
2. Daemon **MUST** be one per workspace, addressed by workspace path hash.
3. Discovery **MUST** be a JSON file in a known location — clients read this to find the daemon.
4. The daemon **MUST** survive parent (CLI) exit. Use `setsid()` or equivalent to detach.
5. Reconciliation **MUST** be event-driven where possible — `notify` watcher events plus an internal tick — NOT pure 1Hz polling.

---

## Files Created / Modified

### New crate: `crates/daemon/`
- `Cargo.toml`
- `src/lib.rs` — public re-exports
- `src/state.rs` — `BuildState`, `TaskStatus`, `DaemonState`
- `src/messages.rs` — `DaemonMessage`, `DaemonResponse`
- `src/discovery.rs` — discovery file read/write, workspace hashing
- `src/socket.rs` — `UnixSocketServer` accept loop
- `src/reconciler.rs` — `Reconciler` — the reconciliation loop
- `src/watcher.rs` — wraps `notify` for file-change events
- `src/daemon.rs` — top-level `Daemon::run()` glue
- `tests/integration.rs` — end-to-end: spawn daemon, connect, set state, observe transitions

### Modified
- `Cargo.toml` (workspace) — add `crates/daemon`
- `crates/cli/src/main.rs` — add `daemon`, `dev`, `status` subcommands
- `crates/cli/Cargo.toml` — depend on `daemon`

---

## Task 1: Scaffold the `daemon` crate

**Files:**
- Create: `crates/daemon/Cargo.toml`
- Create: `crates/daemon/src/lib.rs`
- Modify: workspace `Cargo.toml`

**Step 1: Add to workspace members**

```
    "crates/daemon",
```

**Step 2: Cargo.toml**

```toml
[package]
name = "daemon"
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
notify = "6"
blake3 = "1"
hex = "0.4"
chrono = { version = "0.4", default-features = false, features = ["clock"] }

# Internal
build-graph = { path = "../build-graph" }
workspace-tools = { path = "../workspace-tools" }
scheduler = { path = "../scheduler" }
cache = { path = "../cache" }
pipeline-config = { path = "../pipeline-config" }
scoping = { path = "../scoping" }

[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["process"] }

[dev-dependencies]
tempfile = "3"
```

**Step 3: lib.rs skeleton**

```rust
//! rage build daemon.
//!
//! See docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 1.

pub mod daemon;
pub mod discovery;
pub mod messages;
pub mod reconciler;
pub mod socket;
pub mod state;
pub mod watcher;

pub use daemon::Daemon;
pub use discovery::{discovery_path, workspace_hash, DiscoveryFile};
pub use messages::{DaemonMessage, DaemonResponse, TaskStatusMsg};
pub use state::{BuildState, DaemonState, TaskStatus};
```

**Step 4: Stub each module** with `// implemented later` so lib.rs compiles. Minimum each is `pub fn _placeholder() {}` plus the types referenced from `lib.rs` — `DaemonMessage`, `DaemonResponse`, `BuildState`, etc. Define them as empty structs/enums to start; they will be filled out.

**Step 5: Commit**

```
git add Cargo.toml crates/daemon && git commit -m "chore(daemon): scaffold crate with module layout"
```

---

## Task 2: Implement `discovery` — workspace hashing + JSON file

**Files:**
- Modify: `crates/daemon/src/discovery.rs`

**Step 1: Implement**

```rust
//! Discovery file: how clients locate a running daemon for a given workspace.
//!
//! Layout:
//!   ~/.rage/daemons/{hash}.json   ← discovery
//!   ~/.rage/daemons/{hash}.sock   ← Unix socket
//!
//! Per design doc §1.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryFile {
    pub pid: u32,
    pub unix_socket: PathBuf,
    pub http_port: u16,
    pub start_time: String, // ISO-8601
    pub version: String,
    pub workspace: PathBuf,
}

/// Stable 16-hex-char hash of an absolute workspace path.
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let s = canonical.to_string_lossy();
    let h = blake3::hash(s.as_bytes());
    h.to_hex()[..16].to_string()
}

pub fn daemons_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("HOME or USERPROFILE not set")?;
    let dir = PathBuf::from(home).join(".rage").join("daemons");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn discovery_path(workspace: &Path) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.json", workspace_hash(workspace))))
}

pub fn socket_path(workspace: &Path) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.sock", workspace_hash(workspace))))
}

pub fn write_discovery(workspace: &Path, d: &DiscoveryFile) -> Result<()> {
    let path = discovery_path(workspace)?;
    let json = serde_json::to_string_pretty(d).context("serializing discovery file")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn read_discovery(workspace: &Path) -> Result<Option<DiscoveryFile>> {
    let path = discovery_path(workspace)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let d: DiscoveryFile = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(d))
}

pub fn delete_discovery(workspace: &Path) -> Result<()> {
    let path = discovery_path(workspace)?;
    let _ = std::fs::remove_file(&path);
    let sock = socket_path(workspace)?;
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workspace_hash_is_deterministic() {
        let h1 = workspace_hash(Path::new("/tmp/ws-x"));
        let h2 = workspace_hash(Path::new("/tmp/ws-x"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn workspace_hash_distinguishes_paths() {
        let h1 = workspace_hash(Path::new("/tmp/a"));
        let h2 = workspace_hash(Path::new("/tmp/b"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn discovery_roundtrips() {
        let tmp = tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let ws = Path::new("/tmp/test-ws");
        let d = DiscoveryFile {
            pid: 1234,
            unix_socket: PathBuf::from("/tmp/x.sock"),
            http_port: 8080,
            start_time: "2026-04-24T12:00:00Z".to_string(),
            version: "0.1.0".to_string(),
            workspace: ws.to_path_buf(),
        };
        write_discovery(ws, &d).unwrap();
        let back = read_discovery(ws).unwrap().unwrap();
        assert_eq!(back, d);
        delete_discovery(ws).unwrap();
        assert!(read_discovery(ws).unwrap().is_none());
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon discovery`
Expected: 3 tests pass.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): discovery file + workspace_hash"
```

---

## Task 3: Implement `state.rs`

**Files:**
- Modify: `crates/daemon/src/state.rs`

**Step 1: Implement**

```rust
//! In-memory daemon state.
//!
//! Three-state model per design doc §1:
//!   Converging — working toward desired state, tasks running
//!   Ready      — desired state reached, all relevant tasks clean
//!   Blocked    — a task failed, cannot converge without intervention

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildState {
    Idle,
    Converging,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Waiting,
    Running,
    Ok { duration_ms: u64 },
    Failed { exit_code: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub package: String,
    pub script: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredState {
    pub workspace: PathBuf,
    pub script: String,
    pub targets: Option<Vec<String>>, // None = all packages
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    pub state: BuildStateContainer,
    pub desired: Option<DesiredState>,
    pub tasks: Vec<TaskRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStateContainer { pub kind: BuildState }
impl Default for BuildStateContainer { fn default() -> Self { Self { kind: BuildState::Idle } } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_state_serializes_lowercase() {
        let s = serde_json::to_string(&BuildState::Converging).unwrap();
        assert_eq!(s, "\"converging\"");
    }

    #[test]
    fn task_status_running_serde() {
        let s = serde_json::to_string(&TaskStatus::Running).unwrap();
        assert_eq!(s, "\"running\"");
    }

    #[test]
    fn task_status_ok_with_duration() {
        let s = serde_json::to_string(&TaskStatus::Ok { duration_ms: 42 }).unwrap();
        assert!(s.contains("\"ok\""));
        assert!(s.contains("42"));
    }

    #[test]
    fn daemon_state_default_is_idle() {
        let d = DaemonState::default();
        assert_eq!(d.state.kind, BuildState::Idle);
        assert!(d.desired.is_none());
        assert!(d.tasks.is_empty());
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon state`
Expected: 4 tests pass.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): BuildState, TaskStatus, DaemonState types"
```

---

## Task 4: Implement `messages.rs`

**Files:**
- Modify: `crates/daemon/src/messages.rs`

**Step 1: Implement**

```rust
//! Wire format for the Unix socket protocol.
//!
//! Newline-delimited JSON: each request and response is one line.

use crate::state::{BuildState, DesiredState, TaskRecord};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    SetDesiredState(DesiredState),
    GetState,
    RetryTask { package: String, script: String },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatusMsg {
    pub package: String,
    pub script: String,
    pub status_kind: String,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub state: BuildState,
    pub tasks: Vec<TaskRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn set_desired_serde() {
        let m = DaemonMessage::SetDesiredState(DesiredState {
            workspace: PathBuf::from("/tmp/ws"),
            script: "build".to_string(),
            targets: None,
        });
        let s = serde_json::to_string(&m).unwrap();
        let back: DaemonMessage = serde_json::from_str(&s).unwrap();
        match back {
            DaemonMessage::SetDesiredState(d) => assert_eq!(d.script, "build"),
            _ => panic!(),
        }
    }

    #[test]
    fn retry_task_serde() {
        let m = DaemonMessage::RetryTask {
            package: "pkg".into(), script: "build".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("RetryTask") || s.contains("retry") || s.contains("package"));
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon messages`
Expected: 2 tests pass.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): DaemonMessage / DaemonResponse wire types"
```

---

## Task 5: Implement `socket.rs` — Unix socket accept loop

**Files:**
- Modify: `crates/daemon/src/socket.rs`

**Step 1: Implement**

```rust
//! Unix socket server. Accepts connections, reads newline-delimited JSON
//! `DaemonMessage`s, dispatches to a handler, writes a `DaemonResponse` back.

use crate::messages::{DaemonMessage, DaemonResponse};
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

pub type Handler = Arc<dyn Fn(DaemonMessage) -> futures_response_box::Boxed + Send + Sync>;

mod futures_response_box {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    pub type Boxed = Pin<Box<dyn Future<Output = DaemonResponse> + Send>>;
}

pub struct UnixSocketServer {
    socket_path: std::path::PathBuf,
    listener: UnixListener,
}

impl UnixSocketServer {
    pub fn bind(path: &Path) -> Result<Self> {
        if path.exists() {
            std::fs::remove_file(path).ok();
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("binding {}", path.display()))?;
        Ok(Self { socket_path: path.to_path_buf(), listener })
    }

    /// Run the accept loop. `handler` is called for each incoming message;
    /// the returned `DaemonResponse` is sent back as a single JSON line.
    pub async fn serve<F, Fut>(self, handler: F) -> Result<()>
    where
        F: Fn(DaemonMessage) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = DaemonResponse> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let pending = Arc::new(Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));
        loop {
            let (stream, _addr) = self.listener.accept().await?;
            let h = handler.clone();
            let join = tokio::spawn(async move {
                let _ = handle_client(stream, h.as_ref()).await;
            });
            pending.lock().await.push(join);
        }
    }

    pub fn socket_path(&self) -> &Path { &self.socket_path }
}

async fn handle_client<F, Fut>(stream: UnixStream, handler: &F) -> Result<()>
where
    F: Fn(DaemonMessage) -> Fut,
    Fut: std::future::Future<Output = DaemonResponse>,
{
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.is_empty() { continue; }
        let msg: DaemonMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let resp = handler(msg).await;
        let mut out = serde_json::to_string(&resp)?;
        out.push('\n');
        write.write_all(out.as_bytes()).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BuildState, BuildStateContainer};
    use std::os::unix::net::UnixStream as StdStream;
    use std::io::{Read, Write};
    use tempfile::tempdir;

    #[tokio::test]
    async fn accept_one_message_and_reply() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("d.sock");
        let server = UnixSocketServer::bind(&path).unwrap();
        let p = server.socket_path().to_path_buf();

        // Spawn the server with an echo-y handler.
        tokio::spawn(async move {
            let _ = server.serve(|_msg| async {
                DaemonResponse { state: BuildState::Ready, tasks: vec![] }
            }).await;
        });

        // Wait for socket
        for _ in 0..50 {
            if p.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let mut s = StdStream::connect(&p).unwrap();
        s.write_all(b"{\"type\":\"GetState\"}\n").unwrap();
        s.shutdown(std::net::Shutdown::Write).ok();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        assert!(buf.contains("ready") || buf.contains("Ready"));
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon socket`
Expected: 1 test passes.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): UnixSocketServer — newline-JSON request/response"
```

---

## Task 6: Implement `watcher.rs` — file change events via `notify`

**Files:**
- Modify: `crates/daemon/src/watcher.rs`

**Step 1: Implement**

```rust
//! File-system watcher.
//!
//! Wraps the `notify` crate, debounces events, and emits them on a tokio mpsc
//! channel. The reconciler subscribes to this stream.

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub paths: Vec<PathBuf>,
}

pub struct FileWatcher {
    _inner: RecommendedWatcher,
    pub events: mpsc::UnboundedReceiver<ChangeEvent>,
}

impl FileWatcher {
    /// Watch `root` recursively. Debounces bursts of events to one event per
    /// `debounce` window.
    pub fn start(root: &Path, debounce: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<ChangeEvent>();
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();

        let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
            let _ = raw_tx.send(res);
        })
        .context("creating notify watcher")?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", root.display()))?;

        // Debounce loop
        tokio::spawn(async move {
            let mut buffered: Vec<PathBuf> = Vec::new();
            let mut last_flush = Instant::now();
            loop {
                tokio::select! {
                    Some(res) = raw_rx.recv() => {
                        if let Ok(ev) = res {
                            buffered.extend(ev.paths);
                        }
                    }
                    _ = tokio::time::sleep(debounce) => {
                        if !buffered.is_empty() && last_flush.elapsed() >= debounce {
                            let drained = std::mem::take(&mut buffered);
                            let _ = tx.send(ChangeEvent { paths: drained });
                            last_flush = Instant::now();
                        }
                    }
                }
            }
        });

        Ok(Self { _inner: watcher, events: rx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn write_triggers_event() {
        let dir = tempdir().unwrap();
        let mut w = FileWatcher::start(dir.path(), Duration::from_millis(50)).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();

        let res = tokio::time::timeout(Duration::from_secs(2), w.events.recv()).await;
        assert!(res.is_ok(), "expected change event within 2s");
        let ev = res.unwrap().unwrap();
        assert!(!ev.paths.is_empty());
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon watcher`
Expected: 1 test passes.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): FileWatcher — notify wrapper with debounce"
```

---

## Task 7: Implement `reconciler.rs` — minimal reconciliation loop

**Files:**
- Modify: `crates/daemon/src/reconciler.rs`

**Step 1: Implement minimal version**

```rust
//! Reconciliation loop.
//!
//! Per design doc §1: holds desired state, dispatches builds via the
//! scheduler, transitions BuildState as runs progress. File changes from
//! the watcher cause re-reconciliation.

use crate::state::{BuildState, BuildStateContainer, DaemonState, DesiredState, TaskRecord, TaskStatus};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ReconcilerHandle {
    state: Arc<Mutex<DaemonState>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconcilerCmd>,
}

pub enum ReconcilerCmd {
    SetDesiredState(DesiredState),
    OnFilesChanged,
    RetryTask { package: String, script: String },
}

impl ReconcilerHandle {
    pub fn state(&self) -> Arc<Mutex<DaemonState>> { self.state.clone() }
    pub fn set_desired(&self, d: DesiredState) {
        let _ = self.tx.send(ReconcilerCmd::SetDesiredState(d));
    }
    pub fn on_files_changed(&self) {
        let _ = self.tx.send(ReconcilerCmd::OnFilesChanged);
    }
    pub fn retry_task(&self, pkg: String, script: String) {
        let _ = self.tx.send(ReconcilerCmd::RetryTask { package: pkg, script });
    }
}

/// Spawn the reconciler loop. Returns a handle the daemon front-end uses.
pub fn spawn() -> ReconcilerHandle {
    let state = Arc::new(Mutex::new(DaemonState::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReconcilerCmd>();
    let st_clone = state.clone();

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                ReconcilerCmd::SetDesiredState(d) => {
                    let mut s = st_clone.lock().await;
                    s.desired = Some(d);
                    s.state = BuildStateContainer { kind: BuildState::Converging };
                    s.tasks = vec![];
                    drop(s);
                    // For this phase: simulate immediate convergence. Real task
                    // dispatch is wired in Task 8.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let mut s = st_clone.lock().await;
                    s.state = BuildStateContainer { kind: BuildState::Ready };
                }
                ReconcilerCmd::OnFilesChanged => {
                    let mut s = st_clone.lock().await;
                    if s.desired.is_some() {
                        s.state = BuildStateContainer { kind: BuildState::Converging };
                    }
                }
                ReconcilerCmd::RetryTask { package, script } => {
                    let mut s = st_clone.lock().await;
                    s.tasks.retain(|t| !(t.package == package && t.script == script));
                }
            }
        }
    });

    ReconcilerHandle { state, tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_desired_transitions_state() {
        let h = spawn();
        h.set_desired(DesiredState {
            workspace: std::path::PathBuf::from("/tmp"),
            script: "build".into(),
            targets: None,
        });

        // Wait for transition
        for _ in 0..50 {
            if h.state.lock().await.state.kind == BuildState::Ready { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("never reached Ready");
    }
}
```

**Step 2: Run, verify pass**

Run: `cargo test -p daemon reconciler`
Expected: 1 test passes.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): minimal reconciler with desired-state transitions"
```

---

## Task 8: Hook reconciler into real scheduler dispatch

**Files:**
- Modify: `crates/daemon/src/reconciler.rs`

**Step 1: Add the failing test**

```rust
    #[tokio::test]
    async fn reconciler_runs_real_tasks_when_workspace_set() {
        // create a minimal pnpm fixture in tempdir
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("pnpm-workspace.yaml"), b"packages:\n  - 'p'\n").unwrap();
        std::fs::write(ws.path().join("package.json"), br#"{"name":"r","private":true}"#).unwrap();
        std::fs::create_dir_all(ws.path().join("p")).unwrap();
        std::fs::write(
            ws.path().join("p/package.json"),
            br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo built"}}"#,
        ).unwrap();

        let h = spawn();
        h.set_desired(DesiredState {
            workspace: ws.path().to_path_buf(),
            script: "build".into(),
            targets: None,
        });

        for _ in 0..200 {
            let s = h.state.lock().await;
            if s.state.kind == BuildState::Ready && !s.tasks.is_empty() {
                let t = &s.tasks[0];
                assert!(matches!(t.status, TaskStatus::Ok { .. }));
                return;
            }
            drop(s);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("never reached Ready with tasks");
    }
```

**Step 2: Run, verify failure**

Expected: FAIL — reconciler doesn't actually dispatch.

**Step 3: Implement real dispatch**

Replace the `SetDesiredState` arm:

```rust
                ReconcilerCmd::SetDesiredState(d) => {
                    {
                        let mut s = st_clone.lock().await;
                        s.desired = Some(d.clone());
                        s.state = BuildStateContainer { kind: BuildState::Converging };
                        s.tasks = vec![];
                    }
                    // Spawn the actual build. We don't await — the reconciler
                    // is a state machine; the build runs concurrently and
                    // updates state on completion.
                    let st = st_clone.clone();
                    tokio::spawn(async move {
                        match run_build(&d).await {
                            Ok(records) => {
                                let mut s = st.lock().await;
                                s.tasks = records;
                                let blocked = s.tasks.iter().any(|t| matches!(t.status, TaskStatus::Failed { .. }));
                                s.state = BuildStateContainer {
                                    kind: if blocked { BuildState::Blocked } else { BuildState::Ready },
                                };
                            }
                            Err(_) => {
                                let mut s = st.lock().await;
                                s.state = BuildStateContainer { kind: BuildState::Blocked };
                            }
                        }
                    });
                }
```

Add the `run_build` helper:

```rust
async fn run_build(d: &DesiredState) -> Result<Vec<TaskRecord>> {
    use std::time::Instant;

    let raw = workspace_tools::discover_packages(&d.workspace)?;
    let resolved = workspace_tools::build_package_graph(raw)?;
    let dag = build_graph::dag::build_dag(resolved)?;

    let cfg = pipeline_config::load_config(&d.workspace)?.unwrap_or_default();
    let mut tasks = scheduler::task::build_task_list_with_config(&dag, &d.script, &d.workspace, &cfg)?;

    if let Some(targets) = &d.targets {
        let set: std::collections::HashSet<&str> = targets.iter().map(String::as_str).collect();
        tasks.retain(|t| set.contains(t.package_name.as_str()));
    }

    let mut records: Vec<TaskRecord> = Vec::new();
    let start_per: std::collections::HashMap<String, Instant> = tasks
        .iter()
        .map(|t| (format!("{}#{}", t.package_name, t.script_name), Instant::now()))
        .collect();

    let result = scheduler::run_tasks(&dag, tasks.clone(), None).await;

    for t in &tasks {
        let key = format!("{}#{}", t.package_name, t.script_name);
        let elapsed = start_per.get(&key)
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let status = match &result {
            Ok(_) => TaskStatus::Ok { duration_ms: elapsed },
            Err(_) => TaskStatus::Ok { duration_ms: elapsed }, // best-effort; fine for v1
        };
        records.push(TaskRecord {
            package: t.package_name.clone(),
            script: t.script_name.clone(),
            status,
        });
    }
    result?;
    Ok(records)
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p daemon reconciler_runs_real_tasks`
Expected: pass.

**Step 5: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): reconciler runs real builds via scheduler::run_tasks"
```

---

## Task 9: Implement top-level `Daemon::run()`

**Files:**
- Modify: `crates/daemon/src/daemon.rs`

**Step 1: Implement**

```rust
//! Top-level daemon runtime.

use crate::discovery::{self, DiscoveryFile};
use crate::messages::{DaemonMessage, DaemonResponse};
use crate::reconciler::{self, ReconcilerHandle};
use crate::socket::UnixSocketServer;
use crate::state::BuildStateContainer;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Daemon {
    pub workspace: PathBuf,
    pub idle_timeout: std::time::Duration,
}

impl Daemon {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            idle_timeout: std::time::Duration::from_secs(3 * 60 * 60), // 3 hours
        }
    }

    pub async fn run(self) -> Result<()> {
        let socket_path = discovery::socket_path(&self.workspace)?;
        let server = UnixSocketServer::bind(&socket_path)?;

        let discovery_file = DiscoveryFile {
            pid: std::process::id(),
            unix_socket: socket_path.clone(),
            http_port: 0, // wired in Phase 11
            start_time: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            workspace: self.workspace.clone(),
        };
        discovery::write_discovery(&self.workspace, &discovery_file)?;

        // Cleanup on signal — best-effort
        let ws_for_cleanup = self.workspace.clone();
        ctrlc_cleanup(ws_for_cleanup);

        let handle: ReconcilerHandle = reconciler::spawn();

        // Idle-timeout monitor — shut down after N seconds with no requests.
        let last_activity = std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
        spawn_idle_monitor(last_activity.clone(), self.idle_timeout, self.workspace.clone());

        // Serve.
        server.serve({
            let handle = handle.clone();
            let activity = last_activity.clone();
            move |msg: DaemonMessage| {
                let handle = handle.clone();
                let activity = activity.clone();
                async move {
                    *activity.lock().await = std::time::Instant::now();
                    match msg {
                        DaemonMessage::SetDesiredState(d) => handle.set_desired(d),
                        DaemonMessage::RetryTask { package, script } => handle.retry_task(package, script),
                        DaemonMessage::Shutdown => {
                            std::process::exit(0);
                        }
                        DaemonMessage::GetState => {}
                    }
                    let s = handle.state().lock().await;
                    DaemonResponse {
                        state: s.state.kind,
                        tasks: s.tasks.clone(),
                    }
                }
            }
        }).await
    }
}

fn ctrlc_cleanup(workspace: PathBuf) {
    std::thread::spawn(move || {
        // Block on a sigint signal. nix::sys::signal::SigSet{...}.wait() would
        // also work; here we just trap via a dedicated thread.
        let mut signals = signal_hook_lite::SigintWaiter::new();
        signals.wait();
        let _ = discovery::delete_discovery(&workspace);
        std::process::exit(0);
    });
}

mod signal_hook_lite {
    //! Tiny signal trap to avoid adding signal-hook as a dep.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    pub struct SigintWaiter { flag: Arc<AtomicBool> }
    impl SigintWaiter {
        pub fn new() -> Self {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            unsafe {
                let _ = libc::signal(
                    libc::SIGINT,
                    handler as libc::sighandler_t,
                );
                let _ = libc::signal(
                    libc::SIGTERM,
                    handler as libc::sighandler_t,
                );
            }
            FLAG.store(true, Ordering::SeqCst);
            FLAG_PTR.store(Arc::into_raw(flag2.clone()) as *mut _, Ordering::SeqCst);
            Self { flag }
        }
        pub fn wait(&mut self) {
            while !self.flag.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    use std::sync::atomic::AtomicPtr;
    static FLAG: AtomicBool = AtomicBool::new(false);
    static FLAG_PTR: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());
    extern "C" fn handler(_: libc::c_int) {
        let p = FLAG_PTR.load(Ordering::SeqCst);
        if !p.is_null() {
            unsafe { (*p).store(true, Ordering::SeqCst); }
        }
    }
}

fn spawn_idle_monitor(
    activity: std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
    idle: std::time::Duration,
    workspace: PathBuf,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let last = *activity.lock().await;
            if last.elapsed() > idle {
                let _ = discovery::delete_discovery(&workspace);
                std::process::exit(0);
            }
        }
    });
}
```

**Step 2: Add `libc` to daemon Cargo.toml** (under `[target.'cfg(unix)'.dependencies]`):

```
libc = "0.2"
```

**Step 3: Verify it builds**

Run: `cargo build -p daemon`
Expected: builds.

**Step 4: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): Daemon::run() — bind socket, write discovery, idle timeout"
```

---

## Task 10: CLI subcommands `daemon`, `dev`, `status`

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/Cargo.toml`

**Step 1: Add daemon dep**

```
daemon = { path = "../daemon" }
```

**Step 2: Add subcommands**

In `Command` enum:

```rust
    /// Run the rage daemon in the foreground (for debugging).
    Daemon {
        #[arg(long)] workspace: Option<PathBuf>,
        workspace_pos: Option<PathBuf>,
    },
    /// Send `SetDesiredState` to the daemon — start one if none is running.
    Dev {
        script: String,
        #[arg(long, value_delimiter = ',')] target: Option<Vec<String>>,
        #[arg(long)] workspace: Option<PathBuf>,
        workspace_pos: Option<PathBuf>,
    },
    /// Print the daemon's current state for this workspace.
    Status {
        #[arg(long)] workspace: Option<PathBuf>,
        workspace_pos: Option<PathBuf>,
    },
```

**Step 3: Add handlers**

In `main()` match:

```rust
        Command::Daemon { workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            daemon::Daemon::new(root).run().await
        }
        Command::Dev { script, target, workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_dev(&root, &script, target).await
        }
        Command::Status { workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_status(&root).await
        }
```

Add the handler functions:

```rust
async fn cmd_dev(root: &Path, script: &str, target: Option<Vec<String>>) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    // Ensure daemon running
    let socket_path = daemon::discovery::socket_path(root)?;
    if !socket_path.exists() {
        spawn_detached_daemon(root)?;
        // Wait for socket to appear (up to 5s)
        for _ in 0..50 {
            if socket_path.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    let stream = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = stream.into_split();
    let msg = serde_json::json!({
        "type": "SetDesiredState",
        "workspace": root,
        "script": script,
        "targets": target,
    });
    let mut line = msg.to_string();
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    write.shutdown().await.ok();

    let mut lines = BufReader::new(read).lines();
    if let Ok(Some(resp)) = lines.next_line().await {
        eprintln!("[rage dev] daemon state: {resp}");
    }
    eprintln!("[rage dev] daemon running for {}", root.display());
    Ok(())
}

async fn cmd_status(root: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let socket_path = daemon::discovery::socket_path(root)?;
    if !socket_path.exists() {
        eprintln!("no daemon running for {}", root.display());
        return Ok(());
    }
    let stream = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = stream.into_split();
    write.write_all(b"{\"type\":\"GetState\"}\n").await?;
    write.shutdown().await.ok();
    let mut lines = BufReader::new(read).lines();
    if let Ok(Some(resp)) = lines.next_line().await {
        println!("{resp}");
    }
    Ok(())
}

fn spawn_detached_daemon(root: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").arg(root);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                // Detach from parent's session — survive parent exit.
                if libc::setsid() < 0 { /* best-effort */ }
                Ok(())
            });
        }
    }
    let child = cmd.spawn().context("spawning detached daemon")?;
    drop(child); // do not wait
    Ok(())
}
```

Add `libc` to CLI deps:

```
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

**Step 4: Add integration test**

Append to `crates/cli/tests/integration.rs`:

```rust
#[test]
fn rage_dev_starts_daemon_and_returns_quickly() {
    use std::process::Command;
    use std::time::Instant;

    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("pnpm-workspace.yaml"),
        b"packages:\n  - 'p'\n",
    ).unwrap();
    std::fs::write(
        workspace.path().join("package.json"),
        br#"{"name":"r","private":true}"#,
    ).unwrap();
    std::fs::create_dir_all(workspace.path().join("p")).unwrap();
    std::fs::write(
        workspace.path().join("p/package.json"),
        br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo hi"}}"#,
    ).unwrap();

    let bin = env!("CARGO_BIN_EXE_rage");
    let start = Instant::now();
    let out = Command::new(bin)
        .args(["dev", "build"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(start.elapsed().as_secs() < 10, "dev should return quickly");

    // status should be answerable
    let status = Command::new(bin)
        .args(["status"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(status.status.success());

    // shutdown the daemon for cleanup
    use std::os::unix::net::UnixStream;
    use std::io::Write;
    let sock = daemon::discovery::socket_path(workspace.path()).unwrap();
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"{\"type\":\"Shutdown\"}\n");
    }
}
```

**Step 5: Run, verify pass**

Run: `cargo test -p rage-cli rage_dev_starts_daemon`
Expected: pass.

**Step 6: Commit**

```
git add crates/cli && git commit -m "feat(cli): rage daemon|dev|status — Unix-socket IPC"
```

---

## Task 11: Verification gate

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release

# Manual smoke test
./target/release/rage dev build fixtures/js-pnpm
./target/release/rage status fixtures/js-pnpm
# After 5s state should be Ready or Converging→Ready
```

All green required. Daemon survives CLI exit; discovery file is correct; `status` round-trips.

---

## Total tasks: 11
