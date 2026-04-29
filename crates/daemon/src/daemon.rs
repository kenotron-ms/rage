use crate::discovery::{self, DiscoveryFile};
use crate::messages::{DaemonMessage, DaemonResponse};
use crate::reconciler::{self, ReconcilerHandle};
use crate::transport::DaemonServer;
use crate::watcher::FileWatcher;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;

pub struct Daemon {
    pub workspace: PathBuf,
    pub idle_timeout: std::time::Duration,
}

impl Daemon {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            idle_timeout: std::time::Duration::from_secs(3 * 60 * 60),
        }
    }

    pub async fn run(self) -> Result<()> {
        let (server, endpoint) = DaemonServer::bind(&self.workspace)?;
        let (http_listener, http_port) = crate::http::bind_dynamic()
            .await
            .context("binding HTTP listener")?;
        let discovery_file = DiscoveryFile {
            pid: std::process::id(),
            endpoint: endpoint.clone(),
            http_port,
            start_time: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            workspace: self.workspace.clone(),
        };
        discovery::write_discovery(&self.workspace, &discovery_file)?;
        let ws_for_cleanup = self.workspace.clone();
        ctrlc_cleanup(ws_for_cleanup);
        let handle: ReconcilerHandle = reconciler::spawn();
        let (snap_tx, _) = tokio::sync::broadcast::channel::<crate::http::StateSnapshot>(64);
        let snap_tx_clone = snap_tx.clone();
        let handle_clone = handle.clone();
        let mut sub = handle.subscribe();
        tokio::spawn(async move {
            // Initial broadcast
            {
                let arc = handle_clone.state();
                let s = arc.lock().await;
                let _ = snap_tx_clone.send(crate::http::StateSnapshot {
                    state: s.state.kind,
                    tasks: s.tasks.clone(),
                });
            }
            while sub.recv().await.is_ok() {
                let arc = handle_clone.state();
                let s = arc.lock().await;
                let _ = snap_tx_clone.send(crate::http::StateSnapshot {
                    state: s.state.kind,
                    tasks: s.tasks.clone(),
                });
            }
        });

        let app = crate::http::AppState {
            reconciler: handle.clone(),
            broadcast_tx: snap_tx,
        };
        tokio::spawn(async move {
            let _ = crate::http::serve(http_listener, app).await;
        });

        let last_activity = std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
        spawn_idle_monitor(
            last_activity.clone(),
            self.idle_timeout,
            self.workspace.clone(),
        );
        // Wire file-change events → reconciler so builds re-converge on edits.
        if let Ok(mut watcher) = FileWatcher::start(&self.workspace, Duration::from_millis(300)) {
            let watcher_handle = handle.clone();
            tokio::spawn(async move {
                while let Some(_ev) = watcher.events.recv().await {
                    watcher_handle.on_files_changed();
                }
            });
        }
        crate::socket::serve(server, {
            let handle = handle.clone();
            let activity = last_activity.clone();
            move |msg: DaemonMessage| {
                let handle = handle.clone();
                let activity = activity.clone();
                async move {
                    *activity.lock().await = std::time::Instant::now();
                    match msg {
                        DaemonMessage::SetDesiredState(d) => handle.set_desired(d),
                        DaemonMessage::RetryTask { package, script } => {
                            handle.retry_task(package, script)
                        }
                        DaemonMessage::Shutdown => {
                            std::process::exit(0);
                        }
                        DaemonMessage::GetState => {}
                    }
                    let arc = handle.state();
                    let s = arc.lock().await;
                    DaemonResponse {
                        state: s.state.kind,
                        tasks: s.tasks.clone(),
                    }
                }
            }
        })
        .await
    }
}

fn ctrlc_cleanup(workspace: PathBuf) {
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        let _ = discovery::delete_discovery(&workspace);
        std::process::exit(0);
    });
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn daemon_new_sets_workspace_and_default_idle_timeout() {
        let ws = PathBuf::from("/tmp/test-workspace");
        let d = Daemon::new(ws.clone());
        assert_eq!(d.workspace, ws, "workspace must be set");
        assert_eq!(
            d.idle_timeout,
            Duration::from_secs(3 * 60 * 60),
            "idle_timeout must default to 3 hours"
        );
    }

    #[test]
    fn daemon_fields_are_pub() {
        let ws = PathBuf::from("/tmp/another-workspace");
        let d = Daemon::new(ws.clone());
        // Access pub fields directly
        let _ = d.workspace;
        let _ = d.idle_timeout;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn daemon_writes_http_port_to_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let ws = tempfile::tempdir().unwrap();
        let mut d = Daemon::new(ws.path().to_path_buf());
        d.idle_timeout = std::time::Duration::from_secs(2);
        let task = tokio::spawn(async move {
            let _ = d.run().await;
        });
        let mut disc = None;
        for _ in 0..50 {
            if let Ok(Some(d)) = crate::discovery::read_discovery(ws.path()) {
                disc = Some(d);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let disc = disc.expect("discovery file written");
        assert!(
            disc.http_port > 0,
            "http_port must be a real port, got {}",
            disc.http_port
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let url = format!("http://127.0.0.1:{}/api/state", disc.http_port);
        let out = std::process::Command::new("curl")
            .args(["-s", &url])
            .output()
            .unwrap();
        let body = String::from_utf8_lossy(&out.stdout);
        assert!(body.contains("state"), "body: {body}");
        task.abort();
    }
}
