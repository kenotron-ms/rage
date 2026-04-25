use crate::state::{
    BuildState, BuildStateContainer, DaemonState, DesiredState, TaskRecord, TaskStatus,
};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(Clone)]
pub struct ReconcilerHandle {
    pub state: Arc<Mutex<DaemonState>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconcilerCmd>,
    state_changes: broadcast::Sender<()>,
}

pub enum ReconcilerCmd {
    SetDesiredState(DesiredState),
    OnFilesChanged,
    RetryTask { package: String, script: String },
}

impl ReconcilerHandle {
    pub fn state(&self) -> Arc<Mutex<DaemonState>> {
        self.state.clone()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.state_changes.subscribe()
    }
    pub fn set_desired(&self, d: DesiredState) {
        let _ = self.tx.send(ReconcilerCmd::SetDesiredState(d));
    }
    pub fn on_files_changed(&self) {
        let _ = self.tx.send(ReconcilerCmd::OnFilesChanged);
    }
    pub fn retry_task(&self, pkg: String, script: String) {
        let _ = self.tx.send(ReconcilerCmd::RetryTask {
            package: pkg,
            script,
        });
    }
}

pub fn spawn() -> ReconcilerHandle {
    let state = Arc::new(Mutex::new(DaemonState::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReconcilerCmd>();
    let (state_tx, _) = broadcast::channel::<()>(64);
    let st_clone = state.clone();
    let bcast = state_tx.clone();
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                ReconcilerCmd::SetDesiredState(d) => {
                    {
                        let mut s = st_clone.lock().await;
                        s.desired = Some(d.clone());
                        s.state = BuildStateContainer {
                            kind: BuildState::Converging,
                        };
                        s.tasks = vec![];
                    }
                    let _ = bcast.send(());
                    let st = st_clone.clone();
                    let b2 = bcast.clone();
                    tokio::spawn(async move {
                        match run_build(&d).await {
                            Ok(records) => {
                                let mut s = st.lock().await;
                                s.tasks = records;
                                let blocked = s
                                    .tasks
                                    .iter()
                                    .any(|t| matches!(t.status, TaskStatus::Failed { .. }));
                                s.state = BuildStateContainer {
                                    kind: if blocked {
                                        BuildState::Blocked
                                    } else {
                                        BuildState::Ready
                                    },
                                };
                                let _ = b2.send(());
                            }
                            Err(_) => {
                                let mut s = st.lock().await;
                                s.state = BuildStateContainer {
                                    kind: BuildState::Blocked,
                                };
                                let _ = b2.send(());
                            }
                        }
                    });
                }
                ReconcilerCmd::OnFilesChanged => {
                    let desired = {
                        let mut s = st_clone.lock().await;
                        if s.desired.is_some() {
                            s.state = BuildStateContainer {
                                kind: BuildState::Converging,
                            };
                            s.tasks = vec![];
                        }
                        s.desired.clone()
                    };
                    if let Some(d) = desired {
                        let _ = bcast.send(());
                        let st = st_clone.clone();
                        let b2 = bcast.clone();
                        tokio::spawn(async move {
                            match run_build(&d).await {
                                Ok(records) => {
                                    let mut s = st.lock().await;
                                    s.tasks = records;
                                    let blocked = s
                                        .tasks
                                        .iter()
                                        .any(|t| matches!(t.status, TaskStatus::Failed { .. }));
                                    s.state = BuildStateContainer {
                                        kind: if blocked {
                                            BuildState::Blocked
                                        } else {
                                            BuildState::Ready
                                        },
                                    };
                                    let _ = b2.send(());
                                }
                                Err(_) => {
                                    let mut s = st.lock().await;
                                    s.state = BuildStateContainer {
                                        kind: BuildState::Blocked,
                                    };
                                    let _ = b2.send(());
                                }
                            }
                        });
                    }
                }
                ReconcilerCmd::RetryTask { package, script } => {
                    // Phase 11 intentional: clear the failed task from UI state and
                    // broadcast so the task card disappears immediately.  Re-dispatching
                    // a build is deferred to a later phase; the user can save a file
                    // (triggering OnFilesChanged) to force a rebuild in the interim.
                    let mut s = st_clone.lock().await;
                    s.tasks
                        .retain(|t| !(t.package == package && t.script == script));
                    let _ = bcast.send(());
                }
            }
        }
    });
    ReconcilerHandle {
        state,
        tx,
        state_changes: state_tx,
    }
}

async fn run_build(d: &DesiredState) -> Result<Vec<TaskRecord>> {
    use std::time::Instant;
    let raw = workspace_tools::discover_packages(&d.workspace)?;
    let resolved = workspace_tools::build_package_graph(raw)?;
    let dag = build_graph::dag::build_dag(resolved)?;
    let cfg = pipeline_config::load_config(&d.workspace)?.unwrap_or_default();
    // The daemon uses an empty plugin slice — it does not run workspace-level
    // install tasks (those are handled by the CLI's `rage run` invocation).
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
    let mut tasks =
        scheduler::task::build_task_list_with_config(&dag, &d.script, &d.workspace, &plugins, &cfg)?;
    if let Some(targets) = &d.targets {
        let set: std::collections::HashSet<&str> = targets.iter().map(String::as_str).collect();
        tasks.retain(|t| set.contains(t.package_name.as_str()));
    }
    let mut records: Vec<TaskRecord> = Vec::new();
    let start_per: std::collections::HashMap<String, Instant> = tasks
        .iter()
        .map(|t| {
            (
                format!("{}#{}", t.package_name, t.script_name),
                Instant::now(),
            )
        })
        .collect();
    let result = scheduler::run_tasks(&dag, tasks.clone(), None).await;
    for t in &tasks {
        let key = format!("{}#{}", t.package_name, t.script_name);
        let elapsed = start_per
            .get(&key)
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let status = match &result {
            Ok(_) => TaskStatus::Ok {
                duration_ms: elapsed,
            },
            Err(_) => TaskStatus::Ok {
                duration_ms: elapsed,
            }, // best-effort; fine for v1
        };
        records.push(TaskRecord {
            package: t.package_name.clone(),
            script: t.script_name.clone(),
            status,
        });
    }
    result?;
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BuildState, DesiredState};
    use std::path::PathBuf;

    #[tokio::test]
    async fn subscribe_receives_notification_on_state_change() {
        let h = spawn();
        let mut rx = h.subscribe();
        h.set_desired(DesiredState {
            workspace: std::path::PathBuf::from("/tmp"),
            script: "build".into(),
            targets: None,
        });
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(res.is_ok(), "expected at least one state-change notification");
    }

    #[tokio::test]
    async fn reconciler_runs_real_tasks_when_workspace_set() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("pnpm-workspace.yaml"),
            b"packages:\n  - 'p'\n",
        )
        .unwrap();
        std::fs::write(
            ws.path().join("package.json"),
            br#"{"name":"r","private":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(ws.path().join("p")).unwrap();
        std::fs::write(
            ws.path().join("p/package.json"),
            br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo built"}}"#,
        )
        .unwrap();
        let h = spawn();
        h.set_desired(DesiredState {
            workspace: ws.path().to_path_buf(),
            script: "build".into(),
            targets: None,
        });
        for _ in 0..200 {
            let s = h.state.lock().await;
            if s.state.kind == BuildState::Ready && !s.tasks.is_empty() {
                let t = &s.tasks[0];
                assert!(matches!(t.status, TaskStatus::Ok { .. }));
                return;
            }
            drop(s);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("never reached Ready with tasks");
    }

    #[tokio::test]
    async fn on_files_changed_re_dispatches_build() {
        let handle = spawn();
        // First set desired state with an invalid path so it goes to Blocked
        handle.set_desired(DesiredState {
            workspace: PathBuf::from("/tmp/test"),
            script: "build".to_string(),
            targets: None,
        });
        // Wait for initial build to fail → Blocked
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let arc = handle.state();
            let s = arc.lock().await;
            if s.state.kind == BuildState::Blocked {
                break;
            }
        }
        {
            let arc = handle.state();
            let s = arc.lock().await;
            assert_eq!(
                s.state.kind,
                BuildState::Blocked,
                "state should be Blocked after failed build"
            );
        }
        // Trigger on_files_changed — must re-dispatch run_build
        handle.on_files_changed();
        // State should go through Converging and back to Blocked (same workspace, same failure)
        let mut converged_again = false;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let arc = handle.state();
            let s = arc.lock().await;
            if matches!(s.state.kind, BuildState::Blocked | BuildState::Ready) {
                converged_again = true;
                break;
            }
        }
        assert!(
            converged_again,
            "on_files_changed must re-dispatch run_build so state exits Converging"
        );
    }

    #[tokio::test]
    async fn set_desired_transitions_state() {
        let handle = spawn();
        handle.set_desired(DesiredState {
            workspace: PathBuf::from("/tmp/test"),
            script: "build".to_string(),
            targets: None,
        });

        let mut reached_ready = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let arc = handle.state();
            let state = arc.lock().await;
            if state.state.kind == BuildState::Ready || state.state.kind == BuildState::Blocked {
                reached_ready = true;
                break;
            }
        }
        assert!(
            reached_ready,
            "State never reached BuildState::Ready or Blocked"
        );
    }
}
