use crate::state::{
    BuildState, BuildStateContainer, DaemonState, DesiredState, TaskRecord, TaskStatus,
};
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, watch, Mutex};

/// Cooperative cancellation token attached to each reconciliation revision.
///
/// When a newer `SetDesiredState` or relevant `OnFilesChanged` arrives, the
/// active token is cancelled (`cancel()`) so the superseded build can abort
/// early and never overwrite [`DaemonState`].
#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Create a fresh, uncancelled token.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Signal cancellation to all clones of this token.
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(true);
        }
    }

    /// Returns `true` if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Wait until cancellation is requested.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut rx = self.rx.clone();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

#[derive(Clone)]
pub struct ReconcilerHandle {
    pub state: Arc<Mutex<DaemonState>>,
    tx: tokio::sync::mpsc::UnboundedSender<ReconcilerCmd>,
    state_changes: broadcast::Sender<()>,
    observed_paths: Arc<Mutex<HashSet<PathBuf>>>,
}

pub enum ReconcilerCmd {
    SetDesiredState(DesiredState),
    OnFilesChanged,
    OnFilesChangedPaths(Vec<PathBuf>),
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
    /// Notify the reconciler that specific `paths` changed on disk.
    ///
    /// If an observed pathset filter has been registered via
    /// [`ReconcilerHandle::set_observed_paths`] (non-empty), changes that do
    /// not intersect the observed pathset are ignored without cancelling or
    /// re-running the current revision.
    pub fn on_files_changed_paths(&self, paths: Vec<PathBuf>) {
        let _ = self.tx.send(ReconcilerCmd::OnFilesChangedPaths(paths));
    }
    /// Register the set of watched input paths for pathset-filtered file change
    /// notifications. An empty set disables filtering (all changes trigger).
    pub async fn set_observed_paths(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut guard = self.observed_paths.lock().await;
        *guard = paths.into_iter().collect();
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
    let observed_paths: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReconcilerCmd>();
    let (state_tx, _) = broadcast::channel::<()>(64);
    let st_clone = state.clone();
    let obs_clone = observed_paths.clone();
    let bcast = state_tx.clone();
    tokio::spawn(async move {
        let mut active_token: Option<CancellationToken> = None;

        while let Some(cmd) = rx.recv().await {
            match cmd {
                ReconcilerCmd::SetDesiredState(d) => {
                    if let Some(prev) = active_token.take() {
                        prev.cancel();
                    }
                    let token = CancellationToken::new();
                    active_token = Some(token.clone());

                    let rev = {
                        let mut s = st_clone.lock().await;
                        s.revision = s.revision.saturating_add(1);
                        s.desired = Some(d.clone());
                        s.state = BuildStateContainer {
                            kind: BuildState::Converging,
                        };
                        s.tasks = vec![];
                        s.revision
                    };
                    let _ = bcast.send(());
                    spawn_revision_run(st_clone.clone(), bcast.clone(), d, rev, token);
                }
                ReconcilerCmd::OnFilesChanged => {
                    dispatch_files_changed(&st_clone, &bcast, &mut active_token).await;
                }
                ReconcilerCmd::OnFilesChangedPaths(paths) => {
                    let relevant = {
                        let obs = obs_clone.lock().await;
                        obs.is_empty() || paths.iter().any(|p| obs.contains(p))
                    };
                    if relevant {
                        dispatch_files_changed(&st_clone, &bcast, &mut active_token).await;
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
        observed_paths,
    }
}

async fn dispatch_files_changed(
    st_clone: &Arc<Mutex<DaemonState>>,
    bcast: &broadcast::Sender<()>,
    active_token: &mut Option<CancellationToken>,
) {
    let desired_and_rev = {
        let mut s = st_clone.lock().await;
        if let Some(d) = s.desired.clone() {
            s.revision = s.revision.saturating_add(1);
            s.state = BuildStateContainer {
                kind: BuildState::Converging,
            };
            s.tasks = vec![];
            Some((d, s.revision))
        } else {
            None
        }
    };
    if let Some((d, rev)) = desired_and_rev {
        if let Some(prev) = active_token.take() {
            prev.cancel();
        }
        let token = CancellationToken::new();
        *active_token = Some(token.clone());

        let _ = bcast.send(());
        spawn_revision_run(st_clone.clone(), bcast.clone(), d, rev, token);
    }
}

fn spawn_revision_run(
    st: Arc<Mutex<DaemonState>>,
    b2: broadcast::Sender<()>,
    d: DesiredState,
    rev: u64,
    token: CancellationToken,
) {
    tokio::spawn(async move {
        let build_res = tokio::select! {
            _ = token.cancelled() => return,
            res = run_build(&d) => res,
        };
        if token.is_cancelled() {
            return;
        }
        match build_res {
            Ok(records) => {
                let mut s = st.lock().await;
                if s.revision != rev || token.is_cancelled() {
                    return;
                }
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
                if s.revision != rev || token.is_cancelled() {
                    return;
                }
                s.state = BuildStateContainer {
                    kind: BuildState::Blocked,
                };
                let _ = b2.send(());
            }
        }
    });
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
    let mut tasks = scheduler::task::build_task_list_with_config(
        &dag,
        &d.script,
        &d.workspace,
        &plugins,
        &cfg,
    )?;
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
    let result = scheduler::run_tasks(&dag, tasks.clone(), None, None, None).await;
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
        assert!(
            res.is_ok(),
            "expected at least one state-change notification"
        );
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

    #[tokio::test]
    async fn newer_desired_state_supersedes_slow_in_flight_revision() {
        // Workspace 1 has a slow build (`sleep 2`) that fails (`exit 1`).
        let slow_ws = tempfile::tempdir().unwrap();
        std::fs::write(
            slow_ws.path().join("pnpm-workspace.yaml"),
            b"packages:\n  - 'p'\n",
        )
        .unwrap();
        std::fs::write(
            slow_ws.path().join("package.json"),
            br#"{"name":"slow","private":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(slow_ws.path().join("p")).unwrap();
        std::fs::write(
            slow_ws.path().join("p/package.json"),
            br#"{"name":"@slow/p","version":"1.0.0","scripts":{"build":"sleep 1 && exit 1"}}"#,
        )
        .unwrap();

        // Workspace 2 has a fast build (`echo ok`) that succeeds.
        let fast_ws = tempfile::tempdir().unwrap();
        std::fs::write(
            fast_ws.path().join("pnpm-workspace.yaml"),
            b"packages:\n  - 'p'\n",
        )
        .unwrap();
        std::fs::write(
            fast_ws.path().join("package.json"),
            br#"{"name":"fast","private":true}"#,
        )
        .unwrap();
        std::fs::create_dir_all(fast_ws.path().join("p")).unwrap();
        std::fs::write(
            fast_ws.path().join("p/package.json"),
            br#"{"name":"@fast/p","version":"1.0.0","scripts":{"build":"echo fast"}}"#,
        )
        .unwrap();

        let h = spawn();
        // Start revision 1 (slow failing build)
        h.set_desired(DesiredState {
            workspace: slow_ws.path().to_path_buf(),
            script: "build".into(),
            targets: None,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Immediately supersede with revision 2 (fast succeeding build)
        h.set_desired(DesiredState {
            workspace: fast_ws.path().to_path_buf(),
            script: "build".into(),
            targets: None,
        });

        // Wait for revision 2 to reach Ready
        for _ in 0..100 {
            let s = h.state.lock().await;
            if s.revision == 2 && s.state.kind == BuildState::Ready {
                break;
            }
            drop(s);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }

        // Wait past slow_ws's 1s sleep to verify revision 1 cannot overwrite revision 2
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let final_state = h.state.lock().await;
        assert_eq!(final_state.revision, 2);
        assert_eq!(
            final_state.state.kind,
            BuildState::Ready,
            "superseded revision 1 must not overwrite revision 2 state"
        );
        assert_eq!(final_state.tasks.len(), 1);
        assert_eq!(final_state.tasks[0].package, "@fast/p");
    }

    #[tokio::test]
    async fn pathset_filtered_on_files_changed_ignores_unrelated_paths() {
        let h = spawn();
        let watched = PathBuf::from("/tmp/ws/packages/a/src/index.ts");
        let unrelated = PathBuf::from("/tmp/ws/README.md");
        h.set_observed_paths([watched.clone()]).await;

        h.set_desired(DesiredState {
            workspace: PathBuf::from("/tmp/nonexistent-ws-for-filter-test"),
            script: "build".into(),
            targets: None,
        });

        for _ in 0..50 {
            if h.state.lock().await.state.kind == BuildState::Blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(h.state.lock().await.revision, 1);

        // Unrelated file change must NOT bump revision.
        h.on_files_changed_paths(vec![unrelated]);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(
            h.state.lock().await.revision,
            1,
            "unrelated path must not trigger a new reconciliation revision"
        );

        // Observed file change MUST bump revision to 2.
        h.on_files_changed_paths(vec![watched]);
        for _ in 0..50 {
            if h.state.lock().await.revision == 2 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("observed path change did not increment revision to 2");
    }
}
