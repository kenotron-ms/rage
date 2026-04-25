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
    /// Watch root recursively. Debounces bursts of events to one event per debounce window.
    pub fn start(root: &Path, debounce: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<ChangeEvent>();
        let (raw_tx, mut raw_rx) =
            mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res| {
                let _ = raw_tx.send(res);
            })
            .context("creating notify watcher")?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", root.display()))?;
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
        Ok(Self {
            _inner: watcher,
            events: rx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn write_triggers_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut watcher = FileWatcher::start(dir.path(), Duration::from_millis(50))
            .expect("start watcher");

        // Write a file into the temp directory
        std::fs::write(dir.path().join("test_file.txt"), b"hello").expect("write file");

        let result = timeout(Duration::from_secs(2), watcher.events.recv()).await;
        let event = result
            .expect("timed out waiting for event")
            .expect("channel closed");
        assert!(!event.paths.is_empty(), "expected non-empty paths in ChangeEvent");
    }
}
