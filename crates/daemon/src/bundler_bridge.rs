//! JSON-lines `invalidate` bridge for the rage daemon.
//!
//! When codegen or compile tasks (`prepare` / `compile`) complete or update
//! package ABI fingerprints, the daemon formats and optionally sends a
//! newline-delimited JSON [`InvalidationNotice`] over a Unix socket or Windows
//! named pipe so external dev bundlers and persistent workers can invalidate
//! only the affected module subgraph.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Wire operation tag for external bundler invalidation messages.
pub const INVALIDATE_OP: &str = "invalidate";

/// JSON-lines invalidation payload emitted to external dev bundlers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidationNotice {
    /// Always `"invalidate"`.
    pub op: String,
    /// Workspace package name (e.g. `"@acme/web-app"`).
    pub name: String,
    /// Monotonic daemon reconciliation revision / epoch.
    pub epoch: u64,
    /// Source paths whose modification triggered the rebuild.
    pub paths: Vec<PathBuf>,
    /// Generated output paths (e.g. `.d.ts`, GraphQL artifacts, CSS modules).
    pub generated: Vec<PathBuf>,
    /// `true` if the package's semantic ABI fingerprint changed on this epoch.
    pub abi_changed: bool,
    /// Short machine/human reason (e.g. `"prepare-complete"`, `"abi-updated"`).
    pub reason: String,
}

impl InvalidationNotice {
    /// Construct a new [`InvalidationNotice`] with `op` set to `"invalidate"`.
    pub fn new(
        name: impl Into<String>,
        epoch: u64,
        paths: Vec<PathBuf>,
        generated: Vec<PathBuf>,
        abi_changed: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            op: INVALIDATE_OP.to_string(),
            name: name.into(),
            epoch,
            paths,
            generated,
            abi_changed,
            reason: reason.into(),
        }
    }

    /// Format this notice as a single-line JSON string terminated by `\n`
    /// (JSON-lines wire format).
    pub fn to_json_line(&self) -> Result<String> {
        let mut line = serde_json::to_string(self).context("serializing InvalidationNotice")?;
        line.push('\n');
        Ok(line)
    }

    /// Write this notice as a JSON line to any [`AsyncWrite`] stream.
    pub async fn write_to<W>(&self, writer: &mut W) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let line = self.to_json_line()?;
        writer
            .write_all(line.as_bytes())
            .await
            .context("writing InvalidationNotice JSON line")?;
        writer
            .flush()
            .await
            .context("flushing InvalidationNotice stream")?;
        Ok(())
    }

    /// Connect to `endpoint` (Unix domain socket on Unix, named pipe on Windows)
    /// and send this notice as a single JSON line.
    #[cfg(unix)]
    pub async fn send_to_endpoint(&self, endpoint: &Path) -> Result<()> {
        let mut stream = tokio::net::UnixStream::connect(endpoint)
            .await
            .with_context(|| format!("connecting to bundler socket {}", endpoint.display()))?;
        self.write_to(&mut stream).await
    }

    /// Connect to `endpoint` (Windows named pipe) and send this notice as a
    /// single JSON line.
    #[cfg(windows)]
    pub async fn send_to_endpoint(&self, endpoint: &Path) -> Result<()> {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe_name = endpoint.to_string_lossy();
        let mut client = ClientOptions::new()
            .open(pipe_name.as_ref())
            .with_context(|| format!("connecting to bundler pipe {}", endpoint.display()))?;
        self.write_to(&mut client).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_valid_single_line_json() {
        let notice = InvalidationNotice::new(
            "@acme/chat",
            7,
            vec![PathBuf::from("packages/chat/src/index.ts")],
            vec![PathBuf::from("packages/chat/dist/index.d.ts")],
            true,
            "prepare-complete",
        );

        let line = notice.to_json_line().unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.lines().count(), 1);

        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["op"], "invalidate");
        assert_eq!(parsed["name"], "@acme/chat");
        assert_eq!(parsed["epoch"], 7);
        assert_eq!(parsed["abi_changed"], true);
        assert_eq!(parsed["reason"], "prepare-complete");

        let roundtrip: InvalidationNotice = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(roundtrip, notice);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sends_json_line_over_unix_socket() {
        use tokio::io::AsyncBufReadExt;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("bundler.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let notice = InvalidationNotice::new(
            "@acme/core",
            42,
            vec![PathBuf::from("src/a.ts")],
            vec![PathBuf::from("dist/a.d.ts")],
            false,
            "incremental-build",
        );

        let sock_for_client = sock_path.clone();
        let notice_for_client = notice.clone();
        let client_jh = tokio::spawn(async move {
            notice_for_client
                .send_to_endpoint(&sock_for_client)
                .await
                .unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let mut received = String::new();
        reader.read_line(&mut received).await.unwrap();
        client_jh.await.unwrap();

        let decoded: InvalidationNotice = serde_json::from_str(received.trim_end()).unwrap();
        assert_eq!(decoded, notice);
    }
}
