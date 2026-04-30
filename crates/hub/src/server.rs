//! Hub gRPC server — dispatches tasks to spokes via streaming Subscribe/Complete.

use crate::dag::{HubDag, TaskNode};
use crate::proto::coordinator_server::{Coordinator, CoordinatorServer};
use crate::proto::{
    build_event, Ack, BuildDone, BuildEvent, BuildFailed, BuildRequest, CompletionReport,
    PingRequest, PingResponse, WorkItem, WorkerInfo,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tonic::{transport::Server, Request, Response, Status};

type WorkStream = Pin<Box<dyn futures_core::Stream<Item = Result<WorkItem, Status>> + Send>>;
type BuildStream = Pin<Box<dyn futures_core::Stream<Item = Result<BuildEvent, Status>> + Send>>;

/// Shared hub state.
struct HubState {
    dag: HubDag,
    build_id: String,
}

/// The hub gRPC server.
#[derive(Clone)]
pub struct HubServer {
    state: Arc<Mutex<HubState>>,
    token: String,
    notify: Arc<Notify>,
    workspace: PathBuf,
}

impl HubServer {
    pub fn new(
        tasks: Vec<TaskNode>,
        token: String,
        build_id: String,
        workspace: PathBuf,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(HubState {
                dag: HubDag::new(tasks),
                build_id,
            })),
            token,
            notify: Arc::new(Notify::new()),
            workspace,
        }
    }

    pub(crate) fn build_work_item(&self, task: &TaskNode) -> WorkItem {
        WorkItem {
            task_id: task.task_id.clone(),
            package_name: task.package_name.clone(),
            script_name: task.script_name.clone(),
            command: task.command.clone(),
            workspace_root: self.workspace.to_string_lossy().to_string(),
            package_path: task.package_path.clone(),
            input_refs: vec![],
            cache_backend_url: String::new(),
            env: std::collections::HashMap::new(),
        }
    }

    fn wake_all(&self) {
        self.notify.notify_waiters();
    }

    pub fn into_service(self) -> CoordinatorServer<Self> {
        CoordinatorServer::new(self)
    }

    pub async fn serve(self, addr: SocketAddr) -> anyhow::Result<()> {
        eprintln!("[rage-hub] listening on {addr}");
        let state_for_shutdown = Arc::clone(&self.state);
        let state_for_result = Arc::clone(&self.state);
        let notify = Arc::clone(&self.notify);

        // Shutdown signal: fires when the DAG is done (all tasks complete or failed).
        let shutdown = async move {
            loop {
                // Wake every 500ms or when notified by complete().
                tokio::time::timeout(Duration::from_millis(500), notify.notified())
                    .await
                    .ok();
                if state_for_shutdown.lock().await.dag.is_done() {
                    return;
                }
            }
        };

        Server::builder()
            .add_service(self.into_service())
            .serve_with_shutdown(addr, shutdown)
            .await
            .map_err(|e| anyhow::anyhow!("gRPC server error: {e}"))?;

        // Check final DAG state and return success/failure accordingly.
        let s = state_for_result.lock().await;
        if s.dag.has_failure() {
            let (id, err) = s
                .dag
                .first_failure()
                .map(|(i, e)| (i.to_string(), e.to_string()))
                .unwrap_or_else(|| ("unknown".to_string(), "build failed".to_string()));
            anyhow::bail!("build failed: task {id} — {err}");
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl Coordinator for HubServer {
    type SubscribeStream = WorkStream;
    type SubmitBuildStream = BuildStream;

    async fn subscribe(
        &self,
        request: Request<WorkerInfo>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let info = request.into_inner();
        if info.token != self.token {
            return Err(Status::unauthenticated("invalid token"));
        }

        let worker_id = info.worker_id.clone();
        let state = Arc::clone(&self.state);
        let notify = Arc::clone(&self.notify);
        let self_for_stream = self.clone();

        let stream = async_stream::stream! {
            loop {
                // Try to get a task
                let work_item = {
                    let mut s = state.lock().await;
                    if s.dag.is_done() {
                        break;
                    }
                    s.dag.dispatch_next(&worker_id).map(|task| self_for_stream.build_work_item(&task))
                };

                if let Some(item) = work_item {
                    yield Ok(item);
                } else {
                    // Wait for a notification that new tasks are ready
                    tokio::time::timeout(
                        Duration::from_secs(30),
                        notify.notified()
                    ).await.ok();

                    // Check if done
                    if state.lock().await.dag.is_done() {
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as WorkStream))
    }

    async fn complete(&self, request: Request<CompletionReport>) -> Result<Response<Ack>, Status> {
        let report = request.into_inner();

        {
            let mut s = self.state.lock().await;
            if report.success {
                s.dag.mark_complete(&report.task_id);
            } else {
                s.dag.mark_failed(&report.task_id, &report.stderr_tail);
            }
        }

        // Wake all waiting spokes
        self.wake_all();

        Ok(Response::new(Ack {
            accepted: true,
            reason: String::new(),
        }))
    }

    async fn submit_build(
        &self,
        request: Request<BuildRequest>,
    ) -> Result<Response<Self::SubmitBuildStream>, Status> {
        let req = request.into_inner();
        if req.token != self.token {
            return Err(Status::unauthenticated("invalid token"));
        }

        let tasks: Vec<TaskNode> = req
            .tasks
            .iter()
            .map(|t| TaskNode {
                task_id: t.task_id.clone(),
                package_name: t.package_name.clone(),
                script_name: t.script_name.clone(),
                command: t.command.clone(),
                package_path: t.package_path.clone(),
                depends_on: t.depends_on.clone(),
            })
            .collect();

        {
            let mut s = self.state.lock().await;
            s.dag = HubDag::new(tasks);
            s.build_id = req.build_id.clone();
        }

        // Wake all spokes
        self.wake_all();

        // Return a simple status stream that polls until done
        let state = Arc::clone(&self.state);
        let stream = async_stream::stream! {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let (is_done, has_failure, total) = {
                    let s = state.lock().await;
                    let done = s.dag.is_done();
                    let fail = s.dag.has_failure();
                    let total = s.dag.total_tasks();
                    (done, fail, total)
                };

                if is_done {
                    if has_failure {
                        let (id, err) = {
                            let s = state.lock().await;
                            s.dag.first_failure().map(|(i, e)| (i.to_string(), e.to_string()))
                                .unwrap_or(("unknown".to_string(), "build failed".to_string()))
                        };
                        yield Ok(BuildEvent {
                            event: Some(build_event::Event::BuildFailed(BuildFailed {
                                failed_task_id: id,
                                error: err,
                            })),
                        });
                    } else {
                        yield Ok(BuildEvent {
                            event: Some(build_event::Event::BuildDone(BuildDone {
                                tasks_completed: total as u32,
                                total_duration_ms: 0,
                            })),
                        });
                    }
                    break;
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as BuildStream))
    }

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let s = self.state.lock().await;
        let stats = s.dag.stats();
        Ok(Response::new(PingResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            connected_spokes: 0,
            pending_tasks: (stats.pending + stats.ready + stats.dispatched) as u32,
            build_id: s.build_id.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::TaskNode;
    use std::path::PathBuf;

    fn one_task() -> Vec<TaskNode> {
        vec![TaskNode {
            task_id: "pkg-a#build".to_string(),
            package_name: "pkg-a".to_string(),
            script_name: "build".to_string(),
            command: "echo hi".to_string(),
            package_path: "packages/pkg-a".to_string(),
            depends_on: vec![],
        }]
    }

    #[tokio::test]
    async fn work_item_carries_real_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace: PathBuf = tmp.path().to_path_buf();

        let hub = HubServer::new(
            one_task(),
            "tok".to_string(),
            "build-1".to_string(),
            workspace.clone(),
        );

        let task = {
            let mut state = hub.state.lock().await;
            state.dag.dispatch_next("worker-1").unwrap()
        };

        let work_item = hub.build_work_item(&task);

        assert_eq!(
            work_item.workspace_root,
            workspace.to_string_lossy().to_string(),
            "WorkItem.workspace_root must reflect the real workspace path, \
             not a hardcoded \"/workspace\""
        );
    }
}
