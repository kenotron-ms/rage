//! Spoke client — connects to hub, subscribes to work, executes tasks.
//!
//! Usage:
//! ```ignore
//! let client = SpokeClient::new("http://hub:9650".into(), "token".into(), "/workspace".into());
//! run_as_spoke(client).await?;
//! ```

// Include generated gRPC client code.
mod proto {
    tonic::include_proto!("rage.coordinator.v1");
}

use proto::coordinator_client::CoordinatorClient;
use proto::{CompletionReport, WorkerInfo};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tonic::transport::Channel;

/// A spoke worker that connects to the hub and executes tasks.
pub struct SpokeClient {
    hub_address: String,
    token: String,
    workspace_root: PathBuf,
    worker_id: String,
}

impl SpokeClient {
    pub fn new(hub_address: String, token: String, workspace_root: PathBuf) -> Self {
        let worker_id = format!(
            "{}-{}",
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string()),
            std::process::id()
        );
        Self {
            hub_address,
            token,
            workspace_root,
            worker_id,
        }
    }

    /// Connect to the hub and run until the build completes.
    /// Reconnects with exponential backoff on disconnect.
    pub async fn run(&self) -> anyhow::Result<()> {
        let mut backoff = Duration::from_millis(500);

        loop {
            match self.connect_and_work().await {
                Ok(()) => {
                    eprintln!("[rage-spoke] build complete");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!(
                        "[rage-spoke] disconnected: {e} — reconnecting in {:?}",
                        backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, Duration::from_secs(30));
                }
            }
        }
    }

    async fn connect_and_work(&self) -> anyhow::Result<()> {
        let channel = Channel::from_shared(self.hub_address.clone())?
            .tcp_keepalive(Some(Duration::from_secs(20)))
            .connect()
            .await?;

        let mut client = CoordinatorClient::new(channel);

        eprintln!(
            "[rage-spoke] connected to {} as {}",
            self.hub_address, self.worker_id
        );

        let stream = client
            .subscribe(WorkerInfo {
                worker_id: self.worker_id.clone(),
                token: self.token.clone(),
                parallelism: 1,
                platform: std::env::consts::OS.to_string(),
            })
            .await?
            .into_inner();

        use tokio_stream::StreamExt;
        let mut stream = stream;
        while let Some(item) = stream.next().await {
            let item = item?;
            let task_id = item.task_id.clone();
            let pkg_name = item.package_name.clone();
            let script_name = item.script_name.clone();

            eprintln!(
                "[rage-spoke] running {}#{} (task: {})",
                pkg_name, script_name, task_id
            );

            let result = self.execute(&item).await;
            let start = Instant::now();

            let report = match result {
                Ok(()) => CompletionReport {
                    task_id: task_id.clone(),
                    worker_id: self.worker_id.clone(),
                    success: true,
                    exit_code: 0,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    output_sf_hash: String::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
                Err(e) => {
                    eprintln!("[rage-spoke] task {} failed: {}", task_id, e);
                    CompletionReport {
                        task_id: task_id.clone(),
                        worker_id: self.worker_id.clone(),
                        success: false,
                        exit_code: 1,
                        stdout_tail: String::new(),
                        stderr_tail: e.to_string(),
                        output_sf_hash: String::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                    }
                }
            };

            let _ = client.complete(report).await;
        }

        Ok(())
    }

    async fn execute(&self, item: &proto::WorkItem) -> anyhow::Result<()> {
        let pkg_dir = self.workspace_root.join(&item.package_path);
        let status = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&item.command)
            .current_dir(&pkg_dir)
            .status()
            .await?;

        if status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "command exited with code {:?}",
                status.code()
            ))
        }
    }
}

/// Run this process as a spoke worker until the build completes.
pub async fn run_as_spoke(
    hub_address: String,
    token: String,
    workspace_root: PathBuf,
) -> anyhow::Result<()> {
    let client = SpokeClient::new(hub_address, token, workspace_root);
    client.run().await
}
