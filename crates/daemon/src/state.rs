use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// State of the overall build process.
///
/// Three-state model (§1 of design doc):
/// - `Converging`: working toward desired state, tasks are running.
/// - `Ready`: desired state reached, all relevant tasks clean.
/// - `Blocked`: a task failed, cannot converge without intervention.
/// - `Idle`: no desired state has been set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildState {
    Idle,
    Converging,
    Ready,
    Blocked,
}

/// Current status of a single build task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Waiting,
    Running,
    Ok { duration_ms: u64 },
    Failed { exit_code: i32 },
}

/// A record of a single task and its current status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub package: String,
    pub script: String,
    pub status: TaskStatus,
}

/// The desired state requested by the client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredState {
    pub workspace: PathBuf,
    pub script: String,
    /// `None` means all packages; `Some(vec)` is an explicit target list.
    pub targets: Option<Vec<String>>,
}

/// Top-level daemon state snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    pub state: BuildStateContainer,
    pub desired: Option<DesiredState>,
    pub tasks: Vec<TaskRecord>,
}

/// Wrapper that carries the current `BuildState` kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStateContainer {
    pub kind: BuildState,
}

impl Default for BuildStateContainer {
    fn default() -> Self {
        Self {
            kind: BuildState::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_state_serializes_lowercase() {
        let json = serde_json::to_string(&BuildState::Converging).unwrap();
        assert_eq!(json, "\"converging\"");
    }

    #[test]
    fn task_status_running_serde() {
        let json = serde_json::to_string(&TaskStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn task_status_ok_with_duration() {
        let json = serde_json::to_string(&TaskStatus::Ok { duration_ms: 42 }).unwrap();
        assert!(json.contains("ok"), "expected 'ok' in JSON, got: {json}");
        assert!(json.contains("42"), "expected '42' in JSON, got: {json}");
    }

    #[test]
    fn daemon_state_default_is_idle() {
        let ds = DaemonState::default();
        assert_eq!(ds.state.kind, BuildState::Idle);
        assert!(ds.desired.is_none());
        assert!(ds.tasks.is_empty());
    }
}
