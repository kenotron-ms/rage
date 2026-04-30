# Windows Support — Phase 2: Platform IPC Transport Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Replace the Unix-socket-only daemon IPC with a cross-platform transport layer so `rage dev`, `rage status`, and the daemon compile and run on Windows.

**Architecture:** Introduce a new `crates/daemon/src/transport.rs` module that exposes a `DaemonStream` wrapper (Unix socket on Unix, named pipe handle on Windows), a `DaemonServer` that binds and accepts connections, and a `daemon_connect()` client helper with typed `DaemonError`. Extend `DiscoveryFile` with a platform-neutral `endpoint: String` field (replacing `unix_socket: PathBuf`), and migrate `daemon.rs`, `socket.rs`, and `cli/src/main.rs` to use the new transport. Per-workspace daemon keying via `daemons_dir()/{workspace_hash}.{json,sock}` is preserved on Unix; Windows uses pipe names of the form `\\.\pipe\rage-{workspace_hash}-{nonce}`.

**Tech Stack:** Rust 2021, tokio (with `full` features — already includes `tokio::net::windows::named_pipe`), `pin-project-lite` (new dep), `thiserror` (new dep), `anyhow`, `serde`, `serde_json`, `blake3`.

---

## Background — read this first

Before touching any code, skim these files so you know the lay of the land. **You will get this wrong if you don't read them first.**

- `crates/daemon/src/discovery.rs` — the `DiscoveryFile` struct and the `daemons_dir()`, `discovery_path()`, `socket_path()`, `read_discovery()`, `write_discovery()`, `delete_discovery()` functions.
- `crates/daemon/src/socket.rs` — the current `UnixSocketServer` and its `bind`/`serve`/`handle_client` flow.
- `crates/daemon/src/daemon.rs` — top-of-file imports and the first ~40 lines of `run()` are where the IPC server is created.
- `crates/cli/src/main.rs` — the `cmd_dev` and `cmd_status` functions (search for `fn cmd_dev` and `fn cmd_status`).
- `docs/plans/2026-04-29-windows-support-design.md` — the overall design document this plan implements.

You do **not** need to touch the sandbox crate, scheduler crate, or anything related to the watcher / reconciler / HTTP server. Those keep working unchanged.

## Conventions used throughout this plan

- All `cargo` commands are run from the repo root (`/Users/ken/workspace/ms/rage`).
- "Run" means: execute the command and read the output. If it fails, fix the immediate problem before moving to the next step.
- "Expected: PASS" / "Expected: FAIL" is your acceptance criterion for that step. Do not move on if the expectation isn't met.
- Code snippets in this plan are **complete and ready to paste**, unless explicitly marked otherwise.
- Use `tail -30` / `tail -50` after `cargo test` so the output stays readable.

---

## Task 1: Add dependencies and create empty transport module

**Files:**
- Modify: `crates/daemon/Cargo.toml`
- Create: `crates/daemon/src/transport.rs`
- Modify: `crates/daemon/src/lib.rs`

**Step 1: Add `pin-project-lite` and `thiserror` to daemon Cargo.toml**

Open `crates/daemon/Cargo.toml`. Find the `[dependencies]` block. Add these two lines just before the empty line that precedes `axum`:

```toml
pin-project-lite = "0.2"
thiserror = "1"
```

Do not touch any other dependency.

**Step 2: Verify the dependency change compiles**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: PASS (no errors, may show warnings about unused new deps — that's fine).

**Step 3: Create the empty transport module file**

Create `crates/daemon/src/transport.rs` with exactly this content:

```rust
//! Cross-platform IPC transport for the rage daemon.
//!
//! On Unix, this wraps `tokio::net::UnixStream` / `UnixListener`.
//! On Windows, this wraps `tokio::net::windows::named_pipe::NamedPipe{Server,Client}`.
//!
//! See docs/plans/2026-04-29-windows-support-design.md.

use crate::discovery::{self, DiscoveryFile};
use anyhow::Result;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("no daemon running for this workspace")]
    NotRunning,
    #[error("daemon discovery file was stale (removed); start daemon with `rage dev`")]
    Stale,
    #[error("transport error: {0}")]
    Transport(#[from] std::io::Error),
}
```

Note: `discovery` and `DiscoveryFile` imports are added now even though they're not yet used — they will be used in Task 3. If the linter complains about the unused imports, add `#[allow(unused_imports)]` above the `use` lines for now and remove that allow in Task 3.

**Step 4: Wire the module into `lib.rs`**

Open `crates/daemon/src/lib.rs`. In the `pub mod ...;` block, add this line (alphabetical order — between `state;` and `watcher;`... actually, between `socket;` and `state;`):

```rust
pub mod transport;
```

Final ordering of the module list should be:
```
pub mod daemon;
pub mod discovery;
pub mod http;
pub mod messages;
pub mod reconciler;
pub mod socket;
pub mod state;
pub mod transport;
pub mod watcher;
```

**Step 5: Verify everything still compiles**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: PASS.

**Step 6: Commit**

```
git add crates/daemon/Cargo.toml crates/daemon/src/transport.rs crates/daemon/src/lib.rs
git commit -m "feat(daemon): add transport module skeleton and pin-project-lite/thiserror deps"
```

---

## Task 2: Write failing tests for the Unix transport

**Files:**
- Modify: `crates/daemon/src/transport.rs`

These tests describe the behavior we want. They will not compile yet — that's the point.

**Step 1: Append the test module to `transport.rs`**

Open `crates/daemon/src/transport.rs`. Append this entire block at the end of the file:

```rust
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Helper: point HOME at a temp directory so `daemons_dir()` is isolated.
    fn isolate_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        env::set_var("HOME", tmp.path());
        tmp
    }

    #[tokio::test]
    #[serial]
    async fn daemon_server_bind_creates_socket() {
        let _home = isolate_home();
        let workspace = _home.path().join("ws-bind");
        std::fs::create_dir_all(&workspace).unwrap();

        let (_server, endpoint) = DaemonServer::bind(&workspace).expect("bind ok");
        assert!(
            endpoint.ends_with(".sock"),
            "endpoint should end in .sock on Unix, got {endpoint}"
        );
        assert!(
            std::path::Path::new(&endpoint).exists(),
            "socket file should exist on disk"
        );
    }

    #[tokio::test]
    #[serial]
    async fn daemon_connect_returns_not_running_when_no_discovery() {
        let _home = isolate_home();
        let workspace = _home.path().join("ws-no-disc");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = daemon_connect(&workspace).await.unwrap_err();
        assert!(
            matches!(err, DaemonError::NotRunning),
            "expected NotRunning, got {err:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn daemon_connect_returns_stale_when_socket_gone() {
        let _home = isolate_home();
        let workspace = _home.path().join("ws-stale");
        std::fs::create_dir_all(&workspace).unwrap();

        // Write a discovery file pointing at an endpoint that does not exist.
        let nonexistent = _home.path().join("ghost.sock");
        let d = DiscoveryFile {
            pid: 99999,
            endpoint: nonexistent.to_string_lossy().into_owned(),
            http_port: 0,
            start_time: "2026-01-01T00:00:00Z".to_string(),
            version: "0.0.0".to_string(),
            workspace: workspace.clone(),
        };
        discovery::write_discovery(&workspace, &d).unwrap();

        let err = daemon_connect(&workspace).await.unwrap_err();
        assert!(
            matches!(err, DaemonError::Stale),
            "expected Stale, got {err:?}"
        );

        // Stale file should have been deleted as a side effect.
        let still_there = discovery::read_discovery(&workspace).unwrap();
        assert!(still_there.is_none(), "stale discovery file should be removed");
    }

    #[tokio::test]
    #[serial]
    async fn daemon_stream_read_write_roundtrip() {
        let _home = isolate_home();
        let workspace = _home.path().join("ws-rw");
        std::fs::create_dir_all(&workspace).unwrap();

        let (mut server, _endpoint) = DaemonServer::bind(&workspace).expect("bind");

        // Write the discovery file so daemon_connect() can find the endpoint.
        let d = DiscoveryFile {
            pid: std::process::id(),
            endpoint: _endpoint.clone(),
            http_port: 0,
            start_time: "2026-01-01T00:00:00Z".to_string(),
            version: "0.0.0".to_string(),
            workspace: workspace.clone(),
        };
        discovery::write_discovery(&workspace, &d).unwrap();

        // Spawn a client that connects and writes "ping\n", reads a reply.
        let workspace_for_client = workspace.clone();
        let client_task = tokio::spawn(async move {
            let mut stream = daemon_connect(&workspace_for_client).await.expect("connect");
            stream.write_all(b"ping\n").await.expect("write");
            stream.shutdown().await.ok();
            let mut buf = String::new();
            stream.read_to_string(&mut buf).await.expect("read");
            buf
        });

        // Server-side: accept, read "ping\n", write "pong\n".
        let mut sstream = server.accept().await.expect("accept");
        let mut got = [0u8; 5];
        sstream.read_exact(&mut got).await.expect("server read");
        assert_eq!(&got, b"ping\n");
        sstream.write_all(b"pong\n").await.expect("server write");
        sstream.shutdown().await.ok();
        drop(sstream);

        let client_buf = client_task.await.expect("client task");
        assert_eq!(client_buf, "pong\n");
    }
}
```

**Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p daemon transport -- --nocapture 2>&1 | tail -40`

Expected: FAIL with errors like `cannot find type \`DaemonServer\` in this scope`, `cannot find function \`daemon_connect\``, and `no field \`endpoint\` on type \`DiscoveryFile\``. **Do not proceed if the failure is not a compile error about these specific missing symbols** — that means you've drifted from the plan.

**Do not commit yet** — Task 3 makes these compile and pass.

---

## Task 3: Implement Unix `DaemonStream`, `DaemonServer`, and `daemon_connect()`

**Files:**
- Modify: `crates/daemon/src/transport.rs`

**Step 1: Add the Unix `DaemonStream` newtype and impls**

Open `crates/daemon/src/transport.rs`. Insert this block **after** the `DaemonError` enum and **before** the `#[cfg(test)]` block:

```rust
// ---------- Unix implementation ----------

#[cfg(unix)]
use pin_project_lite::pin_project;
#[cfg(unix)]
use std::pin::Pin;
#[cfg(unix)]
use std::task::{Context, Poll};
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(unix)]
pin_project! {
    pub struct DaemonStream {
        #[pin]
        inner: tokio::net::UnixStream,
    }
}

#[cfg(unix)]
impl AsyncRead for DaemonStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

#[cfg(unix)]
impl AsyncWrite for DaemonStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_shutdown(cx)
    }
}
```

**Step 2: Add the Unix `DaemonServer`**

Append, still inside the `#[cfg(unix)]` Unix block:

```rust
#[cfg(unix)]
pub struct DaemonServer {
    socket_path: std::path::PathBuf,
    listener: tokio::net::UnixListener,
}

#[cfg(unix)]
impl DaemonServer {
    /// Bind a server for the given workspace. Returns the server and the
    /// endpoint string (the socket path) to record in the DiscoveryFile.
    pub fn bind(workspace: &Path) -> Result<(Self, String)> {
        let path = discovery::daemons_dir()?
            .join(format!("{}.sock", discovery::workspace_hash(workspace)));
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| anyhow::anyhow!("binding {}: {e}", path.display()))?;
        let endpoint = path.to_string_lossy().into_owned();
        Ok((
            Self {
                socket_path: path,
                listener,
            },
            endpoint,
        ))
    }

    /// Accept the next incoming connection.
    pub async fn accept(&mut self) -> Result<DaemonStream> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(DaemonStream { inner: stream })
    }
}

#[cfg(unix)]
impl Drop for DaemonServer {
    fn drop(&mut self) {
        // Best-effort cleanup of the socket file.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
```

**Step 3: Add the Unix `daemon_connect()`**

Append, still inside the `#[cfg(unix)]` Unix block:

```rust
#[cfg(unix)]
pub async fn daemon_connect(workspace: &Path) -> std::result::Result<DaemonStream, DaemonError> {
    let disc = discovery::read_discovery(workspace)
        .map_err(|e| DaemonError::Transport(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let Some(disc) = disc else {
        return Err(DaemonError::NotRunning);
    };
    let path = std::path::PathBuf::from(&disc.endpoint);
    match tokio::net::UnixStream::connect(&path).await {
        Ok(stream) => Ok(DaemonStream { inner: stream }),
        Err(_e) => {
            // Discovery file exists but socket is unreachable: stale.
            // Best-effort: delete the stale discovery file.
            let _ = discovery::delete_discovery(workspace);
            Err(DaemonError::Stale)
        }
    }
}
```

**Step 4: Note about `DiscoveryFile` field**

The tests construct `DiscoveryFile { endpoint: ..., ... }`. The current struct has `unix_socket: PathBuf` instead — that gets renamed in **Task 4**. Tasks 2/3 will not actually pass yet because the struct field doesn't exist. That's fine. Move to Task 4 next; we'll come back and re-run.

**Step 5: Verify compile state**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: FAIL — error should be specifically about `endpoint` field not existing on `DiscoveryFile`. If you see any other error (e.g. a typo in the transport code, missing import), fix that first.

**Do not commit yet.**

---

## Task 4: Rename `DiscoveryFile.unix_socket` to `endpoint` and update `delete_discovery`

**Files:**
- Modify: `crates/daemon/src/discovery.rs`
- Modify: `crates/daemon/src/lib.rs` (re-export check only)

**Step 1: Rename the field**

Open `crates/daemon/src/discovery.rs`. In the struct definition, change:

```rust
    pub unix_socket: PathBuf,
```

to:

```rust
    pub endpoint: String,
```

**Step 2: Delete the `socket_path()` function**

Still in `crates/daemon/src/discovery.rs`, delete this entire function (it's about 3 lines):

```rust
pub fn socket_path(workspace: &Path) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.sock", workspace_hash(workspace))))
}
```

**Step 3: Update `delete_discovery()` to only remove the JSON file**

Replace the entire `delete_discovery` function with:

```rust
/// Deletes the `.json` discovery file for this workspace. Best-effort.
/// The transport-level endpoint (Unix socket file or Windows named pipe)
/// is owned and cleaned up by `DaemonServer` itself.
pub fn delete_discovery(workspace: &Path) -> Result<()> {
    let json_path = discovery_path(workspace)?;
    match std::fs::remove_file(&json_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("deleting discovery file {}", json_path.display()))
        }
    }
    Ok(())
}
```

**Step 4: Update the existing `discovery_roundtrips` test**

In the same file, find the test at the bottom that builds `DiscoveryFile { ... unix_socket: tmp.path().join("daemon.sock"), ... }`. Change that line to:

```rust
            endpoint: tmp.path().join("daemon.sock").to_string_lossy().into_owned(),
```

**Step 5: Verify the discovery module compiles and its tests pass**

Run: `cargo test -p daemon discovery -- --nocapture 2>&1 | tail -30`

Expected: PASS (all 3 discovery tests).

**Step 6: Verify the transport module also compiles and its tests pass**

Run: `cargo test -p daemon transport -- --nocapture 2>&1 | tail -40`

Expected: FAIL — but now the failure is from `daemon.rs` and `socket.rs` (which still reference the old `unix_socket` field and `socket_path` function). Your transport tests themselves should compile; they just can't run because the daemon crate as a whole doesn't build yet.

If `cargo check -p daemon` still has errors only in `daemon.rs`, you're on track. Move to Task 5.

**Do not commit yet — the daemon crate doesn't build.**

---

## Task 5: Migrate `daemon.rs` to use the new transport

**Files:**
- Modify: `crates/daemon/src/daemon.rs`

**Step 1: Update imports at the top of `daemon.rs`**

Open `crates/daemon/src/daemon.rs`. Replace the first 4 lines:

```rust
use crate::discovery::{self, DiscoveryFile};
use crate::messages::{DaemonMessage, DaemonResponse};
use crate::reconciler::{self, ReconcilerHandle};
use crate::socket::UnixSocketServer;
```

with:

```rust
use crate::discovery::{self, DiscoveryFile};
use crate::messages::{DaemonMessage, DaemonResponse};
use crate::reconciler::{self, ReconcilerHandle};
use crate::transport::DaemonServer;
```

**Step 2: Replace the bind block in `Daemon::run()`**

Find these two lines near the top of `run()`:

```rust
        let socket_path = discovery::socket_path(&self.workspace)?;
        let server = UnixSocketServer::bind(&socket_path)?;
```

Replace with:

```rust
        let (server, endpoint) = DaemonServer::bind(&self.workspace)?;
```

**Step 3: Update the `DiscoveryFile` literal**

A few lines below, find this in the `DiscoveryFile { ... }` literal:

```rust
            unix_socket: socket_path.clone(),
```

Replace with:

```rust
            endpoint: endpoint.clone(),
```

**Step 4: Verify the daemon crate compiles**

Run: `cargo check -p daemon 2>&1 | tail -30`

Expected: a small number of errors remain — specifically in `socket.rs` where `serve()` is now called on a `DaemonServer` (not a `UnixSocketServer`). Type errors should mention `serve` / `UnixSocketServer` / `DaemonServer`. If you see anything else, you've drifted — re-read this task.

**Do not commit yet — `socket.rs` still needs migration in Task 6.**

---

## Task 6: Migrate `socket.rs` to use `DaemonServer` and `DaemonStream`

**Files:**
- Modify: `crates/daemon/src/socket.rs`

This task replaces the `UnixSocketServer` glue with a small platform-agnostic `serve()` helper that takes a `DaemonServer` and dispatches to a handler. We keep the file (so the existing test stays) but it no longer owns its own listener type.

**Step 1: Replace the entire contents of `crates/daemon/src/socket.rs`**

Overwrite the file with exactly this content:

```rust
use crate::messages::{DaemonMessage, DaemonResponse};
use crate::transport::{DaemonServer, DaemonStream};
use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// Boxed async future type alias used by the `Handler` type.
pub mod futures_response_box {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    pub type Boxed = Pin<Box<dyn Future<Output = DaemonResponse> + Send>>;
}

/// Type alias for a handler that can be shared across tasks.
pub type Handler = Arc<dyn Fn(DaemonMessage) -> futures_response_box::Boxed + Send + Sync>;

/// Drive a `DaemonServer`'s accept loop, dispatching newline-delimited JSON
/// `DaemonMessage` requests to `handler` and writing JSON `DaemonResponse`s back.
pub async fn serve<F, Fut>(mut server: DaemonServer, handler: F) -> Result<()>
where
    F: Fn(DaemonMessage) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = DaemonResponse> + Send + 'static,
{
    let handler = Arc::new(handler);
    let pending = Arc::new(Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));
    loop {
        let stream = server.accept().await?;
        let h = handler.clone();
        let join = tokio::spawn(async move {
            let _ = handle_client(stream, h.as_ref()).await;
        });
        pending.lock().await.push(join);
    }
}

/// Handle a single client connection: read newline-delimited JSON messages,
/// dispatch to `handler`, and write newline-delimited JSON responses.
async fn handle_client<F, Fut>(stream: DaemonStream, handler: &F) -> Result<()>
where
    F: Fn(DaemonMessage) -> Fut,
    Fut: std::future::Future<Output = DaemonResponse>,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }
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
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::messages::{DaemonMessage, DaemonResponse};
    use crate::state::BuildState;
    use crate::transport::DaemonServer;
    use serial_test::serial;
    use std::env;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    #[serial]
    async fn accept_one_message_and_reply() {
        // Isolate HOME so daemons_dir() is per-test.
        let tmp = TempDir::new().unwrap();
        env::set_var("HOME", tmp.path());

        let workspace = tmp.path().join("ws-socket-test");
        std::fs::create_dir_all(&workspace).unwrap();

        let (server, endpoint) = DaemonServer::bind(&workspace).expect("bind");
        let socket_path = std::path::PathBuf::from(&endpoint);

        // Spawn server with handler returning Ready state
        tokio::spawn(async move {
            serve(server, |_msg: DaemonMessage| async {
                DaemonResponse {
                    state: BuildState::Ready,
                    tasks: vec![],
                }
            })
            .await
            .ok();
        });

        // Wait for socket file to appear (poll up to 50 × 20 ms = 1 s)
        let mut appeared = false;
        for _ in 0..50 {
            if socket_path.exists() {
                appeared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(appeared, "socket file did not appear within timeout");

        // Connect via std UnixStream, write request, shutdown write, read response
        let path_clone = socket_path.clone();
        let response = tokio::task::spawn_blocking(move || {
            let mut stream = StdUnixStream::connect(&path_clone).expect("connect");
            stream
                .write_all(b"{\"type\":\"GetState\"}\n")
                .expect("write");
            stream
                .shutdown(std::net::Shutdown::Write)
                .expect("shutdown write");
            let mut buf = String::new();
            stream.read_to_string(&mut buf).expect("read");
            buf
        })
        .await
        .unwrap();

        let lower = response.to_lowercase();
        assert!(
            lower.contains("ready"),
            "expected 'ready' in response, got: {response}"
        );
    }
}
```

**Step 2: Update `daemon.rs` to call the new `serve()` function**

Open `crates/daemon/src/daemon.rs`. Find the `server.serve({ ... }).await` call near the end of `run()`. Change `server.serve(` to `crate::socket::serve(server, `. The closing `).await` stays.

The full call should now look like:

```rust
        crate::socket::serve(server, {
            let handle = handle.clone();
            let activity = last_activity.clone();
            move |msg: DaemonMessage| {
                // ... existing body unchanged ...
            }
        })
        .await
```

**Step 3: Verify the daemon crate compiles**

Run: `cargo check -p daemon 2>&1 | tail -30`

Expected: PASS.

**Step 4: Run all daemon tests**

Run: `cargo test -p daemon 2>&1 | tail -40`

Expected: PASS for all daemon tests including the 4 new transport tests, the existing 3 discovery tests, and the socket `accept_one_message_and_reply` test.

If a discovery or transport test is flaky because of `HOME` pollution, double-check that it has `#[serial_test::serial]` (or `#[serial]` with the import).

**Do not commit yet — Phase 2 is half done. The CLI still uses `UnixStream` directly.**

---

## Task 7: Add Windows `DaemonStream`

**Files:**
- Modify: `crates/daemon/src/transport.rs`

We're adding the Windows-only block. You're on macOS, so we cannot run Windows tests; we use `cargo check` cfg-gating to verify the Windows code at least parses. The compile-time test below also documents the trait bounds we need.

**Step 1: Append the Windows `DaemonStream` to `transport.rs`**

Open `crates/daemon/src/transport.rs`. Append this block at the end of the file (after the `#[cfg(test)]` module):

```rust
// ---------- Windows implementation ----------

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use pin_project_lite::pin_project;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};

    pin_project! {
        #[project = DaemonStreamProj]
        pub enum DaemonStream {
            Server { #[pin] inner: NamedPipeServer },
            Client { #[pin] inner: NamedPipeClient },
        }
    }

    impl AsyncRead for DaemonStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_read(cx, buf),
                DaemonStreamProj::Client { inner } => inner.poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for DaemonStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_write(cx, buf),
                DaemonStreamProj::Client { inner } => inner.poll_write(cx, buf),
            }
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_flush(cx),
                DaemonStreamProj::Client { inner } => inner.poll_flush(cx),
            }
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_shutdown(cx),
                DaemonStreamProj::Client { inner } => inner.poll_shutdown(cx),
            }
        }
    }
}

#[cfg(windows)]
pub use windows_impl::DaemonStream;
```

**Step 2: Add a Windows-only compile-time trait check**

Append at the end of `transport.rs`:

```rust
#[cfg(windows)]
#[cfg(test)]
mod windows_compile_tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncWrite};

    fn _assert_send_unpin<T: Send + Unpin>() {}
    fn _assert_async_io<T: AsyncRead + AsyncWrite>() {}

    #[allow(dead_code)]
    fn assert_daemon_stream_traits() {
        _assert_send_unpin::<DaemonStream>();
        _assert_async_io::<DaemonStream>();
    }
}
```

**Step 3: Verify on macOS that the Unix path still compiles**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: PASS. The Windows block is `#[cfg(windows)]` so it does not get compiled here.

**Do not commit yet.**

---

## Task 8: Implement Windows `DaemonServer`

**Files:**
- Modify: `crates/daemon/src/transport.rs`

**Step 1: Append the Windows `DaemonServer` to the `windows_impl` module**

Open `crates/daemon/src/transport.rs`. Find the `#[cfg(windows)] mod windows_impl { ... }` block and append, **inside that module**, just before the closing `}` of the module:

```rust
    use crate::discovery;
    use anyhow::{Context, Result};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::net::windows::named_pipe::ServerOptions;

    fn make_pipe_name(workspace: &Path) -> String {
        let hash = discovery::workspace_hash(workspace);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let nonce = (nanos as u64) ^ ((std::process::id() as u64).wrapping_mul(0x517CC1B727220A95));
        format!("\\\\.\\pipe\\rage-{hash}-{:08x}", (nonce & 0xFFFF_FFFF) as u32)
    }

    pub struct DaemonServer {
        pipe_name: String,
        // The next, not-yet-connected pipe instance. accept() takes this and
        // creates a fresh one for the subsequent caller, so new clients never
        // see ERROR_PIPE_BUSY.
        next: Option<NamedPipeServer>,
    }

    impl DaemonServer {
        pub fn bind(workspace: &Path) -> Result<(Self, String)> {
            let pipe_name = make_pipe_name(workspace);
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .access_inbound(true)
                .access_outbound(true)
                .create(&pipe_name)
                .with_context(|| format!("creating named pipe {pipe_name}"))?;
            Ok((
                Self {
                    pipe_name: pipe_name.clone(),
                    next: Some(server),
                },
                pipe_name,
            ))
        }

        pub async fn accept(&mut self) -> Result<DaemonStream> {
            // Take the pre-created instance and wait for a client to connect.
            let mut current = self
                .next
                .take()
                .context("DaemonServer::accept called without a pre-created pipe instance")?;
            current.connect().await.context("waiting for pipe client")?;

            // Pre-create the NEXT instance BEFORE handing the connected one back.
            let next = ServerOptions::new()
                .access_inbound(true)
                .access_outbound(true)
                .create(&self.pipe_name)
                .with_context(|| format!("creating next pipe instance {}", self.pipe_name))?;
            self.next = Some(next);

            Ok(DaemonStream::Server { inner: current })
        }
    }
```

**Step 2: Re-export `DaemonServer` from the windows_impl module**

At the bottom of `transport.rs`, find the line:

```rust
#[cfg(windows)]
pub use windows_impl::DaemonStream;
```

Change it to:

```rust
#[cfg(windows)]
pub use windows_impl::{DaemonServer, DaemonStream};
```

**Step 3: Verify macOS compile is unaffected**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: PASS.

**Step 4: Smoke-check the Windows code with `cargo check --target` if available**

Run: `rustup target list --installed | grep windows || true`

If `x86_64-pc-windows-gnu` or `x86_64-pc-windows-msvc` is listed, run:

```
cargo check -p daemon --target x86_64-pc-windows-msvc 2>&1 | tail -40
```

If neither target is installed, **skip this step** — it'll be checked by CI on the Windows runner. Note the limitation in your eventual commit message.

**Do not commit yet.**

---

## Task 9: Implement Windows `daemon_connect()`

**Files:**
- Modify: `crates/daemon/src/transport.rs`

**Step 1: Append `daemon_connect()` to the `windows_impl` module**

Open `crates/daemon/src/transport.rs`. Inside the `#[cfg(windows)] mod windows_impl { ... }` block, before its closing `}`, append:

```rust
    use tokio::net::windows::named_pipe::ClientOptions;

    /// ERROR_FILE_NOT_FOUND on Windows = pipe does not exist yet.
    /// This means the daemon is starting up but the pipe is not bound,
    /// or no daemon is running at all. We treat it as `NotRunning`.
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    pub async fn daemon_connect(
        workspace: &Path,
    ) -> std::result::Result<DaemonStream, DaemonError> {
        let disc = discovery::read_discovery(workspace).map_err(|e| {
            DaemonError::Transport(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        let Some(disc) = disc else {
            return Err(DaemonError::NotRunning);
        };
        match ClientOptions::new().open(&disc.endpoint) {
            Ok(client) => Ok(DaemonStream::Client { inner: client }),
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                // Pipe doesn't exist yet — the daemon may be starting.
                // Don't delete the discovery file; the caller (ensure_daemon)
                // will retry.
                Err(DaemonError::NotRunning)
            }
            Err(_) => {
                // Discovery file exists, but pipe connect failed for some other
                // reason: the server is gone. Best-effort delete and report stale.
                let _ = discovery::delete_discovery(workspace);
                Err(DaemonError::Stale)
            }
        }
    }
```

**Step 2: Re-export `daemon_connect` from the `windows_impl` module**

At the bottom of `transport.rs`, update the Windows re-export line to include `daemon_connect`:

```rust
#[cfg(windows)]
pub use windows_impl::{daemon_connect, DaemonServer, DaemonStream};
```

**Step 3: Verify macOS compile is unaffected**

Run: `cargo check -p daemon 2>&1 | tail -20`

Expected: PASS.

**Step 4: Run all daemon tests on macOS**

Run: `cargo test -p daemon 2>&1 | tail -40`

Expected: PASS — Windows code is `#[cfg(windows)]` and doesn't run here, but Unix tests must still pass.

**Do not commit yet — CLI still uses `UnixStream` directly.**

---

## Task 10: Migrate `cli/src/main.rs` to use `daemon_connect` and `ensure_daemon`

**Files:**
- Modify: `crates/cli/src/main.rs`

**Step 1: Add imports near the top of `main.rs`**

Open `crates/cli/src/main.rs`. Find the existing `use daemon::...` imports near the top of the file. Add this line just below them:

```rust
use daemon::transport::{daemon_connect, DaemonError, DaemonStream};
```

If there is no existing `use daemon::...` line, place this just below the other top-level `use` statements.

**Step 2: Add the `ensure_daemon` helper**

Find the end of `cmd_dev`. Just **before** the `async fn cmd_dev` line, insert this new function:

```rust
/// Connect to the workspace's daemon, spawning it if necessary.
async fn ensure_daemon(root: &Path) -> Result<DaemonStream> {
    match daemon_connect(root).await {
        Ok(stream) => Ok(stream),
        Err(DaemonError::NotRunning) | Err(DaemonError::Stale) => {
            spawn_detached_daemon(root)?;
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if let Ok(stream) = daemon_connect(root).await {
                    return Ok(stream);
                }
            }
            anyhow::bail!("daemon failed to start within 5 seconds")
        }
        Err(DaemonError::Transport(e)) => {
            Err(anyhow::anyhow!("daemon transport error: {e}"))
        }
    }
}
```

**Step 3: Rewrite `cmd_dev`**

Replace the entire body of `cmd_dev` with:

```rust
async fn cmd_dev(root: &Path, script: &str, target: Option<Vec<String>>) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = ensure_daemon(root).await?;
    let (read, mut write) = tokio::io::split(stream);
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
```

**Step 4: Rewrite `cmd_status`**

Replace the entire body of `cmd_status` with:

```rust
async fn cmd_status(root: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = match daemon_connect(root).await {
        Ok(s) => s,
        Err(DaemonError::NotRunning) => {
            eprintln!("no daemon running for {}", root.display());
            return Ok(());
        }
        Err(DaemonError::Stale) => {
            eprintln!("no daemon running for {} (stale discovery file removed)", root.display());
            return Ok(());
        }
        Err(DaemonError::Transport(e)) => {
            return Err(anyhow::anyhow!("daemon transport error: {e}"));
        }
    };
    let (read, mut write) = tokio::io::split(stream);
    write.write_all(b"{\"type\":\"GetState\"}\n").await?;
    write.shutdown().await.ok();
    let mut lines = BufReader::new(read).lines();
    if let Ok(Some(resp)) = lines.next_line().await {
        println!("{resp}");
    }
    Ok(())
}
```

**Step 5: Verify the CLI crate compiles**

Run: `cargo check -p cli 2>&1 | tail -30`

Expected: PASS. If you see errors about `OwnedReadHalf` / `OwnedWriteHalf` / `ReadHalf<UnixStream>` mismatches, that means an old `tokio::net::UnixStream` import is still around — search the file for `UnixStream` and remove it. The new `tokio::io::split(stream)` returns `tokio::io::ReadHalf<DaemonStream>` and `tokio::io::WriteHalf<DaemonStream>`, which is what we want.

**Step 6: Run CLI tests**

Run: `cargo test -p cli 2>&1 | tail -30`

Expected: PASS.

**Do not commit yet — final verification in Tasks 11–12.**

---

## Task 11: Run the daemon and CLI test suites end-to-end

**Files:** none (verification only)

**Step 1: Run daemon tests**

Run: `cargo test -p daemon 2>&1 | tail -30`

Expected: PASS — at minimum:
- `discovery::tests::workspace_hash_is_deterministic`
- `discovery::tests::workspace_hash_distinguishes_paths`
- `discovery::tests::discovery_roundtrips`
- `socket::tests::accept_one_message_and_reply`
- `transport::tests::daemon_server_bind_creates_socket`
- `transport::tests::daemon_connect_returns_not_running_when_no_discovery`
- `transport::tests::daemon_connect_returns_stale_when_socket_gone`
- `transport::tests::daemon_stream_read_write_roundtrip`

If a test is flaky due to env-var pollution, add `#[serial_test::serial]` to it. If a test is missing, re-read Tasks 2–6.

**Step 2: Run CLI tests**

Run: `cargo test -p cli 2>&1 | tail -30`

Expected: PASS.

**Step 3: Run formatter and clippy on the changed crates**

Run: `cargo fmt -p daemon -p cli`
Run: `cargo clippy -p daemon -p cli -- -D warnings 2>&1 | tail -40`

Expected: clippy clean. If clippy flags `dead_code` for the Windows module on Unix, add `#[allow(dead_code)]` — the Windows code is unused on macOS by design.

**Do not commit yet.**

---

## Task 12: Run the full workspace test suite and resolve any cfg-guarded compile issues

**Files:** none (verification only)

**Step 1: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -50`

Expected: PASS for everything that passed before this work plus the new transport tests. Anything failing here is a regression — fix it.

Common regression sources:
- A consumer of `DiscoveryFile` that still references `unix_socket` (search the workspace: `rg unix_socket`).
- A consumer of `discovery::socket_path` that we missed (search: `rg socket_path crates/`).

If `rg unix_socket crates/` or `rg 'discovery::socket_path' crates/` returns any matches outside of comments, fix them now.

**Step 2: Cross-platform `cargo check` sanity**

Run: `cargo check --workspace 2>&1 | tail -30`

Expected: PASS.

If you have a Windows toolchain installed (rare on a macOS dev box), also run:

```
cargo check --workspace --target x86_64-pc-windows-msvc 2>&1 | tail -40
```

If that's not available, skip — CI will validate on a real Windows runner.

**Step 3: Verify no orphaned references**

Run these searches; each should produce zero hits in source files (matches in `docs/` are fine):

```
rg 'unix_socket' crates/ 2>&1 | grep -v 'docs/' | tail -20
rg 'discovery::socket_path' crates/ 2>&1 | tail -20
rg 'UnixSocketServer' crates/ 2>&1 | tail -20
```

If any of these has hits in `crates/`, you missed a migration. Go fix.

**Do not commit yet.**

---

## Task 13: Commit

**Files:** all changes from the previous tasks.

**Step 1: Inspect the staged diff one more time**

Run: `git status`

Expected files modified or added:
- `crates/daemon/Cargo.toml`
- `crates/daemon/src/lib.rs`
- `crates/daemon/src/transport.rs` (new)
- `crates/daemon/src/discovery.rs`
- `crates/daemon/src/socket.rs`
- `crates/daemon/src/daemon.rs`
- `crates/cli/src/main.rs`
- `Cargo.lock` (auto-updated by `cargo`)

**Step 2: Stage and commit**

```
git add crates/daemon/Cargo.toml \
        crates/daemon/src/lib.rs \
        crates/daemon/src/transport.rs \
        crates/daemon/src/discovery.rs \
        crates/daemon/src/socket.rs \
        crates/daemon/src/daemon.rs \
        crates/cli/src/main.rs \
        Cargo.lock
git commit -m "feat(daemon): cross-platform IPC transport (named pipes on Windows, unix sockets on Unix)

- Add crates/daemon/src/transport.rs with DaemonStream, DaemonServer, daemon_connect, DaemonError.
- Rename DiscoveryFile.unix_socket: PathBuf to DiscoveryFile.endpoint: String.
- Drop discovery::socket_path; DaemonServer::bind owns its endpoint.
- Migrate socket.rs to a thin serve()/handle_client() over DaemonStream.
- Replace tokio::net::UnixStream usage in cli/main.rs with daemon_connect + tokio::io::split.
- Add ensure_daemon() helper in CLI for spawn-and-wait flow.

Phase 2 of docs/plans/2026-04-29-windows-support-design.md."
```

**Step 3: Confirm clean tree**

Run: `git status`

Expected: `nothing to commit, working tree clean`.

---

## Summary of acceptance criteria

By the time Task 13 lands:

- [ ] `cargo test --workspace` passes on macOS.
- [ ] `cargo clippy -p daemon -p cli -- -D warnings` is clean.
- [ ] `rg unix_socket crates/` has zero non-comment hits.
- [ ] `rg 'discovery::socket_path' crates/` has zero hits.
- [ ] `rg UnixSocketServer crates/` has zero hits.
- [ ] `crates/daemon/src/transport.rs` exists and exports `DaemonStream`, `DaemonServer`, `daemon_connect`, `DaemonError` on both `cfg(unix)` and `cfg(windows)`.
- [ ] `DiscoveryFile.endpoint: String` is the canonical platform-neutral field.
- [ ] CLI uses `daemon_connect` + `ensure_daemon` instead of `UnixStream::connect`.

Phase 3 (shell dispatch — already landed per recent commits in main) and Phase 4 (Windows DLL injection integration test + CI hardening) are tracked separately in the design document.
