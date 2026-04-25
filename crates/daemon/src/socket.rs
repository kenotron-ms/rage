use crate::messages::{DaemonMessage, DaemonResponse};
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

/// Boxed async future type alias used by the `Handler` type.
pub mod futures_response_box {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    pub type Boxed = Pin<Box<dyn Future<Output = DaemonResponse> + Send>>;
}

/// Type alias for a handler that can be shared across tasks.
pub type Handler =
    Arc<dyn Fn(DaemonMessage) -> futures_response_box::Boxed + Send + Sync>;

/// Unix socket server that accepts connections and dispatches newline-delimited
/// JSON `DaemonMessage` requests to a handler, writing `DaemonResponse` back.
pub struct UnixSocketServer {
    socket_path: std::path::PathBuf,
    listener: UnixListener,
}

impl UnixSocketServer {
    /// Bind to the given path, removing any stale socket file first.
    pub fn bind(path: &Path) -> Result<Self> {
        if path.exists() {
            std::fs::remove_file(path).ok();
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("binding {}", path.display()))?;
        Ok(Self {
            socket_path: path.to_path_buf(),
            listener,
        })
    }

    /// Run the accept loop.  For each incoming connection a task is spawned
    /// that calls `handle_client`.
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
            let join =
                tokio::spawn(async move { let _ = handle_client(stream, h.as_ref()).await; });
            pending.lock().await.push(join);
        }
    }

    /// Return the socket path this server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Handle a single client connection: read newline-delimited JSON messages,
/// dispatch to `handler`, and write newline-delimited JSON responses.
async fn handle_client<F, Fut>(stream: UnixStream, handler: &F) -> Result<()>
where
    F: Fn(DaemonMessage) -> Fut,
    Fut: std::future::Future<Output = DaemonResponse>,
{
    let (read, mut write) = stream.into_split();
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
mod tests {
    use crate::messages::{DaemonMessage, DaemonResponse};
    use crate::state::BuildState;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::time::Duration;

    #[tokio::test]
    async fn accept_one_message_and_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let server = super::UnixSocketServer::bind(&socket_path).expect("bind");

        // Spawn server with handler returning Ready state
        tokio::spawn(async move {
            server
                .serve(|_msg: DaemonMessage| async {
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
