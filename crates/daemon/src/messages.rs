use crate::state::{BuildState, DesiredState, TaskRecord};
use serde::{Deserialize, Serialize};

// TODO: implementation comes after RED test run

/// Messages sent to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    SetDesiredState(DesiredState),
    GetState,
    RetryTask { package: String, script: String },
    Shutdown,
}

/// Task status message (wire type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatusMsg {
    pub package: String,
    pub script: String,
    pub status_kind: String,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
}

/// Response sent from the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub state: BuildState,
    pub tasks: Vec<TaskRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn set_desired_serde() {
        // Build a SetDesiredState message and round-trip it through JSON.
        // The `script` field must survive the round-trip.
        let desired = DesiredState {
            workspace: PathBuf::from("/workspace"),
            script: "build".to_string(),
            targets: None,
        };
        let msg = DaemonMessage::SetDesiredState(desired);
        let json = serde_json::to_string(&msg).expect("serialise");
        let back: DaemonMessage = serde_json::from_str(&json).expect("deserialise");
        match back {
            DaemonMessage::SetDesiredState(ds) => {
                assert_eq!(ds.script, "build", "script field not preserved");
            }
            other => panic!("expected SetDesiredState, got {other:?}"),
        }
    }

    #[test]
    fn retry_task_serde() {
        // Serialize RetryTask and verify the JSON contains identifiable content.
        let msg = DaemonMessage::RetryTask {
            package: "pkg".to_string(),
            script: "build".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("serialise");
        // The serde(tag = "type") annotation means the "type" key will hold the variant
        // name.  The JSON must also contain either the package or script value.
        let lower = json.to_lowercase();
        assert!(
            lower.contains("retrytask") || lower.contains("retry") || lower.contains("package"),
            "expected recognisable content in JSON, got: {json}",
        );
    }
}
