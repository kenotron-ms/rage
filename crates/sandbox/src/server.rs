// EventServer — Unix domain socket listener that collects AccessEvents.

use crate::event::AccessEvent;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// Listens on a Unix-domain socket and collects [`AccessEvent`]s sent by the
/// injected dylib over JSONL.
pub struct EventServer {
    /// Path of the bound socket file.
    pub socket_path: PathBuf,
    /// Receiver end of the internal event channel.
    pub events_rx: mpsc::UnboundedReceiver<AccessEvent>,
    listener_task: tokio::task::JoinHandle<()>,
}

impl EventServer {
    /// Bind a new socket in `dir` and start the accept loop.
    pub fn start(dir: &Path) -> Result<Self> {
        let socket_path = dir.join(format!("rage-sandbox-{}.sock", std::process::id()));

        // Remove any stale socket file left by a previous run.
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind Unix socket {}", socket_path.display()))?;

        let (tx, events_rx) = mpsc::unbounded_channel::<AccessEvent>();

        let listener_task = tokio::spawn(async move {
            while let Ok((stream, _addr)) = listener.accept().await {
                tokio::spawn(handle_client(stream, tx.clone()));
            }
        });

        Ok(Self {
            socket_path,
            events_rx,
            listener_task,
        })
    }

    /// Stop the server and return all events collected so far.
    pub async fn drain(mut self) -> Vec<AccessEvent> {
        self.listener_task.abort();
        // Wait for the task to finish (ignore the expected Cancelled error).
        let _ = self.listener_task.await;
        // Clean up the socket file.
        let _ = std::fs::remove_file(&self.socket_path);

        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

/// Read JSONL lines from a connected client and forward parsed events to `tx`.
async fn handle_client(stream: UnixStream, tx: mpsc::UnboundedSender<AccessEvent>) {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(event) = serde_json::from_str::<AccessEvent>(&line) {
            let _ = tx.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EventServer;
    use crate::event::AccessEvent;
    use std::io::Write;

    #[tokio::test]
    async fn server_collects_events_from_a_client() {
        let dir = tempfile::tempdir().unwrap();
        let server = EventServer::start(dir.path()).unwrap();

        // Connect via std UnixStream (blocking), write two JSON event lines, then drop.
        let socket_path = server.socket_path.clone();
        let mut client = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();

        let read_event = AccessEvent::Read {
            path: "/etc/hosts".to_string(),
            pid: 1,
        };
        let write_event = AccessEvent::Write {
            path: "/tmp/x".to_string(),
            pid: 1,
        };

        writeln!(client, "{}", serde_json::to_string(&read_event).unwrap()).unwrap();
        writeln!(client, "{}", serde_json::to_string(&write_event).unwrap()).unwrap();
        drop(client);

        // Allow the async tasks time to process the events.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let events = server.drain().await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AccessEvent::Read { .. }));
        assert!(matches!(events[1], AccessEvent::Write { .. }));
    }
}
