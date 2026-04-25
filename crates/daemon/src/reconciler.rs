use crate::state::{BuildState, BuildStateContainer, DaemonState, DesiredState, TaskRecord, TaskStatus};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ReconcilerHandle {
    state: Arc<Mutex<DaemonState>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconcilerCmd>,
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
    pub fn set_desired(&self, d: DesiredState) {
        let _ = self.tx.send(ReconcilerCmd::SetDesiredState(d));
    }
    pub fn on_files_changed(&self) {
        let _ = self.tx.send(ReconcilerCmd::OnFilesChanged);
    }
    pub fn retry_task(&self, pkg: String, script: String) {
        let _ = self.tx.send(ReconcilerCmd::RetryTask { package: pkg, script });
    }
}

pub fn spawn() -> ReconcilerHandle {
    let state = Arc::new(Mutex::new(DaemonState::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReconcilerCmd>();
    let st_clone = state.clone();
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                ReconcilerCmd::SetDesiredState(d) => {
                    let mut s = st_clone.lock().await;
                    s.desired = Some(d);
                    s.state = BuildStateContainer { kind: BuildState::Converging };
                    s.tasks = vec![];
                    drop(s);
                    // For this phase: simulate immediate convergence.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let mut s = st_clone.lock().await;
                    s.state = BuildStateContainer { kind: BuildState::Ready };
                }
                ReconcilerCmd::OnFilesChanged => {
                    let mut s = st_clone.lock().await;
                    if s.desired.is_some() {
                        s.state = BuildStateContainer { kind: BuildState::Converging };
                    }
                }
                ReconcilerCmd::RetryTask { package, script } => {
                    let mut s = st_clone.lock().await;
                    s.tasks.retain(|t| !(t.package == package && t.script == script));
                }
            }
        }
    });
    ReconcilerHandle { state, tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BuildState, DesiredState};
    use std::path::PathBuf;

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
            if state.state.kind == BuildState::Ready {
                reached_ready = true;
                break;
            }
        }
        assert!(reached_ready, "State never reached BuildState::Ready");
    }
}
