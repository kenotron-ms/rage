use crate::messages::{DaemonMessage, DaemonResponse};
use crate::transport::{DaemonServer, DaemonStream};
use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
    loop {
        let stream = server.accept().await?;
        let h = handler.clone();
        tokio::spawn(async move {
            let _ = handle_client(stream, h.as_ref()).await;
        });
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
