//! Cross-platform IPC transport for the rage daemon.
//!
//! On Unix, this wraps `tokio::net::UnixStream` / `UnixListener`.
//! On Windows, this wraps `tokio::net::windows::named_pipe::NamedPipe{Server,Client}`.
//!
//! See docs/plans/2026-04-29-windows-support-design.md.

#[allow(unused_imports)]
use crate::discovery::{self, DiscoveryFile};
#[cfg(unix)]
use anyhow::Result;
#[cfg(unix)]
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
    #[derive(Debug)]
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

#[cfg(unix)]
pub async fn daemon_connect(workspace: &Path) -> std::result::Result<DaemonStream, DaemonError> {
    let disc = discovery::read_discovery(workspace)
        .map_err(|e| DaemonError::Transport(std::io::Error::other(e)))?;
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
        assert!(
            still_there.is_none(),
            "stale discovery file should be removed"
        );
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
            let mut stream = daemon_connect(&workspace_for_client)
                .await
                .expect("connect");
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
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_flush(cx),
                DaemonStreamProj::Client { inner } => inner.poll_flush(cx),
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            match self.project() {
                DaemonStreamProj::Server { inner } => inner.poll_shutdown(cx),
                DaemonStreamProj::Client { inner } => inner.poll_shutdown(cx),
            }
        }
    }

    use crate::discovery;
    use anyhow::{Context as _, Result};
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
        format!(
            "\\\\.\\pipe\\rage-{hash}-{:08x}",
            (nonce & 0xFFFF_FFFF) as u32
        )
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
            let current = self
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

    use tokio::net::windows::named_pipe::ClientOptions;

    /// ERROR_FILE_NOT_FOUND on Windows = pipe does not exist yet.
    /// This means the daemon is starting up but the pipe is not bound,
    /// or no daemon is running at all. We treat it as `NotRunning`.
    const ERROR_FILE_NOT_FOUND: i32 = 2;

    pub async fn daemon_connect(
        workspace: &Path,
    ) -> std::result::Result<DaemonStream, DaemonError> {
        let disc = discovery::read_discovery(workspace)
            .map_err(|e| DaemonError::Transport(std::io::Error::other(e.to_string())))?;
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
}

#[cfg(windows)]
pub use windows_impl::{daemon_connect, DaemonServer, DaemonStream};

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

    #[allow(dead_code)]
    fn assert_daemon_server_bind_signature() {
        // Verify DaemonServer::bind has the correct signature.
        let _bind_fn: fn(&std::path::Path) -> anyhow::Result<(DaemonServer, String)> =
            DaemonServer::bind;
    }

    #[allow(dead_code)]
    fn assert_daemon_connect_signature() {
        // daemon_connect is exported via pub use windows_impl::daemon_connect.
        // Signature verified by integration: fn(&Path) -> impl Future<Output = Result<...>>
    }
}
