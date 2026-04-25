# Phase 11 — HTTP/WebSocket Status Page Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Embed an HTTP + WebSocket server in the daemon. The status page is the always-on ambient indicator of build state — vanilla JS, no framework, no build step. WebSocket pushes state updates and accepts `RetryTask` commands back from the page.

**Architecture (per design doc Section 2):**
- The daemon binds an additional TCP listener on a dynamic port (`127.0.0.1:0`).
- The chosen port is recorded in the discovery file (Phase 10 already includes the field; we now actually populate it).
- Three endpoints:
  - `GET /` — returns the embedded HTML status page.
  - `GET /api/state` — JSON snapshot of `DaemonState`.
  - `WS /ws` — bidirectional. Server pushes `state` messages on every transition. Client may send `RetryTask`.
- A new CLI subcommand `rage open` reads the discovery file and opens `http://127.0.0.1:{port}` in the user's browser.

**Tech Stack:** `axum` (HTTP + WS), `tower` / `tower-http` (services), `tokio-tungstenite` not needed (axum has its own `WebSocket`).

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 2 — Observability UX: HTTP/WS Status Page.

---

## Constraints (from COE)

1. Status page **MUST** be vanilla JS — NO React, NO Vue, NO Svelte, NO build step. One HTML file.
2. WS **MUST** be bidirectional (page → daemon `RetryTask`).
3. HTTP server **MUST** bind to a dynamic port (port `0`), then write the actual port to the discovery file.
4. The HTML file **MUST** be embedded in the binary via `include_str!` so the daemon is self-contained.
5. No external CDN references — page must work offline.

---

## Files Created / Modified

- Modify: `crates/daemon/Cargo.toml` — add `axum`, `tower`, `tower-http`, `futures-util`
- Modify: `crates/daemon/src/lib.rs` — declare new `http` module
- Create: `crates/daemon/src/http.rs` — HTTP routes + WS handler
- Create: `crates/daemon/static/index.html` — the status page (vanilla)
- Modify: `crates/daemon/src/daemon.rs` — bind HTTP, populate `http_port` in discovery
- Modify: `crates/daemon/src/reconciler.rs` — broadcast state changes to subscribers
- Modify: `crates/cli/src/main.rs` — add `rage open`
- Modify: `crates/cli/Cargo.toml` — add `webbrowser`

---

## Task 1: Add HTTP deps to daemon

**Files:**
- Modify: `crates/daemon/Cargo.toml`

**Step 1: Add dependencies**

```toml
axum = { version = "0.7", features = ["ws"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors"] }
futures-util = "0.3"
tokio-stream = "0.1"
```

**Step 2: Verify it builds**

Run: `cargo build -p daemon`
Expected: builds.

**Step 3: Commit**

```
git add crates/daemon && git commit -m "chore(daemon): add axum + tower + tower-http for HTTP/WS"
```

---

## Task 2: Create the static HTML page

**Files:**
- Create: `crates/daemon/static/index.html`

**Step 1: Write the page**

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>rage</title>
  <style>
    :root {
      --bg: #0d0d10;
      --fg: #e8e8eb;
      --muted: #888;
      --ok: #4caf50;
      --warn: #ffb74d;
      --err: #ef5350;
      --converging: #64b5f6;
    }
    body { font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", monospace;
           background: var(--bg); color: var(--fg); margin: 0; padding: 24px; }
    header { font-size: 18px; font-weight: 600; margin-bottom: 8px; }
    .ws { color: var(--muted); margin-bottom: 16px; }
    .state { font-size: 24px; font-weight: 600; margin: 16px 0; }
    .state.idle       { color: var(--muted); }
    .state.converging { color: var(--converging); }
    .state.ready      { color: var(--ok); }
    .state.blocked    { color: var(--err); }
    .tasks { display: grid; grid-template-columns: 1fr; gap: 4px; }
    .task { padding: 6px 10px; border-radius: 4px; background: #1a1a1f;
            display: flex; justify-content: space-between; align-items: center; }
    .task.waiting { color: var(--muted); }
    .task.running { color: var(--converging); }
    .task.ok      { color: var(--ok); }
    .task.failed  { color: var(--err); }
    .name { font-family: ui-monospace, "SF Mono", monospace; }
    .meta { color: var(--muted); font-size: 12px; }
    button { background: #2c2c33; color: var(--fg); border: 0;
             padding: 4px 10px; border-radius: 3px; cursor: pointer; }
    button:hover { background: #3a3a42; }
  </style>
</head>
<body>
  <header>rage</header>
  <div class="ws" id="ws-status">connecting…</div>
  <div class="state idle" id="state">idle</div>
  <div class="tasks" id="tasks"></div>
<script>
"use strict";
const stateEl  = document.getElementById("state");
const tasksEl  = document.getElementById("tasks");
const wsStatus = document.getElementById("ws-status");

let socket;

function connect() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(proto + "//" + location.host + "/ws");
  socket.onopen    = () => wsStatus.textContent = "connected";
  socket.onclose   = () => { wsStatus.textContent = "disconnected — retrying…"; setTimeout(connect, 1000); };
  socket.onerror   = () => wsStatus.textContent = "error";
  socket.onmessage = ev => {
    try { render(JSON.parse(ev.data)); }
    catch (e) { console.error("bad message", ev.data, e); }
  };
}

function render(snap) {
  const s = snap.state || "idle";
  stateEl.className = "state " + s;
  stateEl.textContent = s;
  tasksEl.innerHTML = "";
  for (const t of snap.tasks || []) {
    tasksEl.appendChild(renderTask(t));
  }
}

function renderTask(t) {
  const div = document.createElement("div");
  const status = typeof t.status === "string" ? t.status : Object.keys(t.status)[0];
  div.className = "task " + status;
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = t.package + "#" + t.script;
  const meta = document.createElement("span");
  meta.className = "meta";
  if (typeof t.status === "object" && "ok" in t.status) {
    meta.textContent = (t.status.ok.duration_ms / 1000).toFixed(2) + "s";
  } else if (typeof t.status === "object" && "failed" in t.status) {
    const btn = document.createElement("button");
    btn.textContent = "retry";
    btn.onclick = () => retry(t.package, t.script);
    meta.textContent = "exit " + t.status.failed.exit_code + " ";
    meta.appendChild(btn);
  } else {
    meta.textContent = status;
  }
  div.appendChild(name);
  div.appendChild(meta);
  return div;
}

function retry(pkg, script) {
  if (socket && socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: "RetryTask", package: pkg, script }));
  }
}

connect();
</script>
</body>
</html>
```

**Step 2: Commit**

```
git add crates/daemon/static && git commit -m "feat(daemon): static/index.html — vanilla JS status page"
```

---

## Task 3: Implement `http.rs` — routes

**Files:**
- Create: `crates/daemon/src/http.rs`
- Modify: `crates/daemon/src/lib.rs`

**Step 1: Write the failing test**

Create `crates/daemon/src/http.rs`:

```rust
//! HTTP + WebSocket server embedded in the daemon.
//!
//! Per design doc §2:
//!   GET  /            — static HTML status page
//!   GET  /api/state   — JSON snapshot of DaemonState
//!   WS   /ws          — bidirectional state stream + RetryTask back-channel

use crate::reconciler::ReconcilerHandle;
use crate::state::{BuildState, TaskRecord};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

const INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Clone)]
pub struct AppState {
    pub reconciler: ReconcilerHandle,
    pub broadcast_tx: broadcast::Sender<StateSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub state: BuildState,
    pub tasks: Vec<TaskRecord>,
}

/// Bind a TCP listener on an ephemeral port and return (listener, actual_port).
pub async fn bind_dynamic() -> anyhow::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Build the axum router for the status server.
pub fn router(app: AppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/api/state", get(serve_state))
        .route("/ws", get(ws_upgrade))
        .with_state(Arc::new(app))
}

async fn serve_index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn serve_state(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let s = app.reconciler.state().lock().await;
    Json(StateSnapshot { state: s.state.kind, tasks: s.tasks.clone() })
}

async fn ws_upgrade(State(app): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, app))
}

async fn ws_session(socket: WebSocket, app: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();

    // Send initial snapshot
    {
        let s = app.reconciler.state().lock().await;
        let snap = StateSnapshot { state: s.state.kind, tasks: s.tasks.clone() };
        let _ = sink.send(Message::Text(serde_json::to_string(&snap).unwrap_or_default())).await;
    }

    // Subscribe for further updates
    let mut rx = app.broadcast_tx.subscribe();

    // Spawn the read half — handle RetryTask messages from the page.
    let app_for_read = app.clone();
    let read_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            if let Message::Text(t) = msg {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    if v.get("type").and_then(|x| x.as_str()) == Some("RetryTask") {
                        if let (Some(p), Some(s)) = (
                            v.get("package").and_then(|x| x.as_str()).map(String::from),
                            v.get("script").and_then(|x| x.as_str()).map(String::from),
                        ) {
                            app_for_read.reconciler.retry_task(p, s);
                        }
                    }
                }
            }
        }
    });

    // Forward broadcast updates to the client.
    while let Ok(snap) = rx.recv().await {
        if sink.send(Message::Text(serde_json::to_string(&snap).unwrap_or_default()))
            .await
            .is_err()
        {
            break;
        }
    }
    read_task.abort();
}

pub async fn serve(listener: TcpListener, app: AppState) -> anyhow::Result<()> {
    let router = router(app);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconciler;

    #[tokio::test]
    async fn bind_dynamic_returns_open_port() {
        let (listener, port) = bind_dynamic().await.unwrap();
        assert!(port > 0);
        drop(listener);
    }

    #[tokio::test]
    async fn serve_index_returns_html() {
        let handle = reconciler::spawn();
        let (tx, _rx) = broadcast::channel::<StateSnapshot>(64);
        let app = AppState { reconciler: handle, broadcast_tx: tx };
        let (listener, port) = bind_dynamic().await.unwrap();
        tokio::spawn(serve(listener, app));

        // Give the server a moment to be ready.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let url = format!("http://127.0.0.1:{port}/");
        // Use std::process::Command + curl to keep dependency surface small.
        let out = std::process::Command::new("curl")
            .args(["-s", "-o-", &url])
            .output()
            .unwrap();
        let body = String::from_utf8_lossy(&out.stdout);
        assert!(body.contains("<title>rage</title>"), "body: {body}");
    }

    #[tokio::test]
    async fn api_state_returns_json() {
        let handle = reconciler::spawn();
        let (tx, _rx) = broadcast::channel::<StateSnapshot>(64);
        let app = AppState { reconciler: handle, broadcast_tx: tx };
        let (listener, port) = bind_dynamic().await.unwrap();
        tokio::spawn(serve(listener, app));

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let out = std::process::Command::new("curl")
            .args(["-s", &format!("http://127.0.0.1:{port}/api/state")])
            .output()
            .unwrap();
        let body = String::from_utf8_lossy(&out.stdout);
        assert!(body.contains("\"state\""), "body: {body}");
        assert!(body.contains("\"tasks\""));
    }
}
```

**Step 2: Update `lib.rs` to export the new module**

```rust
pub mod http;
pub use http::{AppState, StateSnapshot, bind_dynamic};
```

**Step 3: Run, verify pass**

Run: `cargo test -p daemon http`
Expected: 3 tests pass.

**Step 4: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): http module — /, /api/state, /ws (axum)"
```

---

## Task 4: Reconciler broadcasts state changes

**Files:**
- Modify: `crates/daemon/src/reconciler.rs`

**Step 1: Add a broadcast channel to `ReconcilerHandle`**

In `reconciler.rs`:

```rust
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct ReconcilerHandle {
    state: Arc<Mutex<DaemonState>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconcilerCmd>,
    state_changes: broadcast::Sender<()>, // notification only; consumers re-read state
}

impl ReconcilerHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.state_changes.subscribe()
    }
    /* existing methods unchanged */
}
```

**Step 2: Create the broadcaster in `spawn()` and emit on every state mutation**

```rust
pub fn spawn() -> ReconcilerHandle {
    let state = Arc::new(Mutex::new(DaemonState::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReconcilerCmd>();
    let (state_tx, _) = broadcast::channel::<()>(64);
    let st_clone = state.clone();
    let bcast = state_tx.clone();

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                ReconcilerCmd::SetDesiredState(d) => {
                    {
                        let mut s = st_clone.lock().await;
                        s.desired = Some(d.clone());
                        s.state = BuildStateContainer { kind: BuildState::Converging };
                        s.tasks = vec![];
                    }
                    let _ = bcast.send(());
                    let st = st_clone.clone();
                    let b2 = bcast.clone();
                    tokio::spawn(async move {
                        match run_build(&d).await {
                            Ok(records) => {
                                let mut s = st.lock().await;
                                s.tasks = records;
                                let blocked = s.tasks.iter().any(|t| matches!(t.status, TaskStatus::Failed { .. }));
                                s.state = BuildStateContainer {
                                    kind: if blocked { BuildState::Blocked } else { BuildState::Ready },
                                };
                                drop(s);
                                let _ = b2.send(());
                            }
                            Err(_) => {
                                let mut s = st.lock().await;
                                s.state = BuildStateContainer { kind: BuildState::Blocked };
                                drop(s);
                                let _ = b2.send(());
                            }
                        }
                    });
                }
                ReconcilerCmd::OnFilesChanged => {
                    let mut s = st_clone.lock().await;
                    if s.desired.is_some() {
                        s.state = BuildStateContainer { kind: BuildState::Converging };
                    }
                    drop(s);
                    let _ = bcast.send(());
                }
                ReconcilerCmd::RetryTask { package, script } => {
                    let mut s = st_clone.lock().await;
                    s.tasks.retain(|t| !(t.package == package && t.script == script));
                    drop(s);
                    let _ = bcast.send(());
                }
            }
        }
    });

    ReconcilerHandle { state, tx, state_changes: state_tx }
}
```

**Step 3: Add the failing test**

In `reconciler.rs` `tests`:

```rust
    #[tokio::test]
    async fn subscribe_receives_notification_on_state_change() {
        let h = spawn();
        let mut rx = h.subscribe();
        h.set_desired(DesiredState {
            workspace: std::path::PathBuf::from("/tmp"),
            script: "build".into(),
            targets: None,
        });
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(res.is_ok(), "expected at least one state-change notification");
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p daemon reconciler`
Expected: pass.

**Step 5: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): reconciler broadcasts state-change notifications"
```

---

## Task 5: Wire HTTP into `Daemon::run()`

**Files:**
- Modify: `crates/daemon/src/daemon.rs`

**Step 1: Update `run()` to bind and serve HTTP, populate `http_port`**

Inside `Daemon::run()`, before constructing `discovery_file`:

```rust
        // Bind HTTP first so we can record the port.
        let (http_listener, http_port) = crate::http::bind_dynamic().await
            .context("binding HTTP listener")?;
```

Update `discovery_file` to use `http_port`.

After `let handle = reconciler::spawn();`:

```rust
        // Bridge reconciler change-notifications into a broadcast<StateSnapshot>.
        let (snap_tx, _) = tokio::sync::broadcast::channel::<crate::http::StateSnapshot>(64);
        let snap_tx_clone = snap_tx.clone();
        let handle_clone = handle.clone();
        let mut sub = handle.subscribe();
        tokio::spawn(async move {
            // Initial broadcast
            {
                let s = handle_clone.state().lock().await;
                let _ = snap_tx_clone.send(crate::http::StateSnapshot {
                    state: s.state.kind, tasks: s.tasks.clone(),
                });
            }
            while sub.recv().await.is_ok() {
                let s = handle_clone.state().lock().await;
                let _ = snap_tx_clone.send(crate::http::StateSnapshot {
                    state: s.state.kind, tasks: s.tasks.clone(),
                });
            }
        });

        // Spawn HTTP server
        let app = crate::http::AppState {
            reconciler: handle.clone(),
            broadcast_tx: snap_tx,
        };
        tokio::spawn(async move {
            let _ = crate::http::serve(http_listener, app).await;
        });
```

**Step 2: Add the integration test**

Append to `crates/daemon/src/daemon.rs` `tests` (or `tests/integration.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_writes_http_port_to_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let ws = tempfile::tempdir().unwrap();

        // Spawn daemon with a short idle timeout so it cleans up.
        let mut d = Daemon::new(ws.path().to_path_buf());
        d.idle_timeout = std::time::Duration::from_secs(2);
        let task = tokio::spawn(async move {
            let _ = d.run().await;
        });

        // Wait for discovery file
        let mut disc = None;
        for _ in 0..50 {
            if let Ok(Some(d)) = crate::discovery::read_discovery(ws.path()) {
                disc = Some(d);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let disc = disc.expect("discovery file written");
        assert!(disc.http_port > 0, "http_port must be a real port, got {}", disc.http_port);

        // Hit /api/state
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let url = format!("http://127.0.0.1:{}/api/state", disc.http_port);
        let out = std::process::Command::new("curl")
            .args(["-s", &url]).output().unwrap();
        let body = String::from_utf8_lossy(&out.stdout);
        assert!(body.contains("state"), "body: {body}");

        task.abort();
    }
}
```

**Step 3: Run, verify pass**

Run: `cargo test -p daemon daemon_writes_http_port`
Expected: pass.

**Step 4: Commit**

```
git add crates/daemon && git commit -m "feat(daemon): bind HTTP server, populate http_port in discovery"
```

---

## Task 6: WS bidirectional retry round-trip test

**Files:**
- Create: `crates/daemon/tests/ws_retry.rs`
- Modify: `crates/daemon/Cargo.toml` (dev-deps: `tokio-tungstenite`)

**Step 1: Add dev-dep**

```toml
[dev-dependencies]
tokio-tungstenite = "0.21"
```

**Step 2: Write the failing test**

Create `crates/daemon/tests/ws_retry.rs`:

```rust
use daemon::Daemon;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use futures_util::{SinkExt, StreamExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_pushes_initial_state_and_accepts_retry() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());

    let ws_root = tempfile::tempdir().unwrap();
    let mut d = Daemon::new(ws_root.path().to_path_buf());
    d.idle_timeout = std::time::Duration::from_secs(3);
    let task = tokio::spawn(async move { let _ = d.run().await; });

    // Wait for discovery
    let mut disc = None;
    for _ in 0..50 {
        if let Ok(Some(x)) = daemon::discovery::read_discovery(ws_root.path()) {
            disc = Some(x); break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let port = disc.unwrap().http_port;

    // Connect WS
    let url = format!("ws://127.0.0.1:{port}/ws");
    let (mut socket, _) = connect_async(url).await.unwrap();

    // Initial snapshot must arrive
    let first = socket.next().await.unwrap().unwrap();
    let txt = match first {
        Message::Text(t) => t,
        _ => panic!("expected Text, got {first:?}"),
    };
    assert!(txt.contains("\"state\""));

    // Send a RetryTask
    socket
        .send(Message::Text("{\"type\":\"RetryTask\",\"package\":\"x\",\"script\":\"build\"}".to_string()))
        .await
        .unwrap();

    // Should receive a fresh broadcast on the change
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next()).await;
    assert!(next.is_ok());

    task.abort();
}
```

**Step 3: Run, verify pass**

Run: `cargo test -p daemon --test ws_retry`
Expected: pass.

**Step 4: Commit**

```
git add crates/daemon && git commit -m "test(daemon): WS bidirectional retry round-trip"
```

---

## Task 7: `rage open` subcommand

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/Cargo.toml`

**Step 1: Add `webbrowser` dep**

```
webbrowser = "1"
```

**Step 2: Add subcommand**

In `Command`:

```rust
    /// Open the rage status page in the default browser.
    Open {
        #[arg(long)] workspace: Option<PathBuf>,
        workspace_pos: Option<PathBuf>,
    },
```

In match:

```rust
        Command::Open { workspace, workspace_pos } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_open(&root)
        }
```

Add handler:

```rust
fn cmd_open(root: &Path) -> Result<()> {
    let disc = daemon::discovery::read_discovery(root)?;
    let Some(d) = disc else {
        anyhow::bail!("no daemon running for {} — run `rage dev` first", root.display());
    };
    let url = format!("http://127.0.0.1:{}/", d.http_port);
    eprintln!("opening {url}");
    webbrowser::open(&url).context("opening browser")?;
    Ok(())
}
```

**Step 3: Run, verify build**

Run: `cargo build -p rage-cli`
Expected: builds.

**Step 4: Add a smoke integration test**

In `crates/cli/tests/integration.rs`:

```rust
#[test]
fn rage_open_errors_when_no_daemon() {
    use std::process::Command;
    let workspace = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_rage");
    let out = Command::new(bin)
        .args(["open"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "open should fail when no daemon");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no daemon"), "expected 'no daemon' message, got: {err}");
}
```

Run: `cargo test -p rage-cli rage_open_errors`
Expected: pass.

**Step 5: Commit**

```
git add crates/cli && git commit -m "feat(cli): rage open — launches status page in browser"
```

---

## Task 8: Verification gate

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release

# manual smoke
./target/release/rage dev build fixtures/js-pnpm
./target/release/rage open fixtures/js-pnpm   # should pop a browser to the status page
./target/release/rage status fixtures/js-pnpm
```

Expected: page renders, WebSocket connects, state updates appear in real time, retry button works.

---

## Total tasks: 8
