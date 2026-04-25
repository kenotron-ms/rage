//! HTTP/WebSocket server for the rage build daemon.
//!
//! Endpoints (design §2):
//!   GET /         — serves the static dashboard (index.html)
//!   GET /api/state — returns current build state as JSON
//!   GET /ws       — WebSocket upgrade; streams [`StateSnapshot`] on every state change

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

/// Shared application state for axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub reconciler: ReconcilerHandle,
    pub broadcast_tx: broadcast::Sender<StateSnapshot>,
}

/// A point-in-time snapshot of the daemon build state, sent over WS and REST.
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub state: BuildState,
    pub tasks: Vec<TaskRecord>,
}

/// Bind to a random available port on 127.0.0.1 and return the listener + its port.
pub async fn bind_dynamic() -> anyhow::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Build the axum [`Router`] wired with all routes.
pub fn router(app: AppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/api/state", get(serve_state))
        .route("/ws", get(ws_upgrade))
        .with_state(Arc::new(app))
}

/// GET / — return the static dashboard HTML.
async fn serve_index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

/// GET /api/state — return current [`StateSnapshot`] as JSON.
async fn serve_state(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let arc = app.reconciler.state();
    let s = arc.lock().await;
    let snap = StateSnapshot {
        state: s.state.kind,
        tasks: s.tasks.clone(),
    };
    Json(snap)
}

/// GET /ws — upgrade the HTTP connection to a WebSocket.
async fn ws_upgrade(
    State(app): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, app))
}

/// Drive a single WebSocket session: send initial snapshot, subscribe to broadcasts,
/// and handle incoming RetryTask commands.
async fn ws_session(socket: WebSocket, app: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();

    // Send the current state as the initial message.
    {
        let arc = app.reconciler.state();
        let s = arc.lock().await;
        let snap = StateSnapshot {
            state: s.state.kind,
            tasks: s.tasks.clone(),
        };
        let text = serde_json::to_string(&snap).unwrap_or_default();
        if sink.send(Message::Text(text)).await.is_err() {
            return;
        }
    }

    // Subscribe to broadcast updates.
    let mut rx = app.broadcast_tx.subscribe();

    // Spawn the read half to handle incoming client messages.
    let app_read = app.clone();
    let read_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(t))) = stream.next().await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                if v["type"] == "RetryTask" {
                    if let (Some(serde_json::Value::String(p)), Some(serde_json::Value::String(s))) =
                        (v.get("package"), v.get("script"))
                    {
                        app_read.reconciler.retry_task(p.clone(), s.clone());
                    }
                }
            }
        }
    });

    // Forward broadcast snapshots to the client.
    while let Ok(snap) = rx.recv().await {
        let text = serde_json::to_string(&snap).unwrap_or_default();
        if sink.send(Message::Text(text)).await.is_err() {
            break;
        }
    }

    read_task.abort();
}

/// Start the axum server on the given listener.
pub async fn serve(listener: TcpListener, app: AppState) -> anyhow::Result<()> {
    axum::serve(
        listener,
        router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconciler;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn bind_dynamic_returns_open_port() {
        let (_listener, port) = bind_dynamic().await.unwrap();
        assert!(port > 0, "expected port > 0, got {port}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serve_index_returns_html() {
        let (listener, port) = bind_dynamic().await.unwrap();
        let reconciler = reconciler::spawn();
        let (broadcast_tx, _rx) = broadcast::channel::<StateSnapshot>(64);
        let app = AppState {
            reconciler,
            broadcast_tx,
        };

        tokio::spawn(async move {
            serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let url = format!("http://127.0.0.1:{port}/");
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("curl")
                .args(["-s", &url])
                .output()
                .expect("curl must be available")
        })
        .await
        .unwrap();
        let body = String::from_utf8_lossy(&output.stdout);
        assert!(
            body.contains("<title>rage</title>"),
            "expected '<title>rage</title>' in body, got: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn api_state_returns_json() {
        let (listener, port) = bind_dynamic().await.unwrap();
        let reconciler = reconciler::spawn();
        let (broadcast_tx, _rx) = broadcast::channel::<StateSnapshot>(64);
        let app = AppState {
            reconciler,
            broadcast_tx,
        };

        tokio::spawn(async move {
            serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let url = format!("http://127.0.0.1:{port}/api/state");
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("curl")
                .args(["-s", &url])
                .output()
                .expect("curl must be available")
        })
        .await
        .unwrap();
        let body = String::from_utf8_lossy(&output.stdout);
        assert!(
            body.contains("\"state\""),
            "expected '\"state\"' in body, got: {body}"
        );
        assert!(
            body.contains("\"tasks\""),
            "expected '\"tasks\"' in body, got: {body}"
        );
    }
}
