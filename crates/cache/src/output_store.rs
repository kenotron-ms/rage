//! Captured task output storage for cache-hit replay.
//!
//! Whenever a task runs and succeeds, rage captures its stdout+stderr and
//! writes them to `{cache_dir}/sf-{SF}.output.json`.  On a subsequent cache
//! hit, rage replays the stored output before printing `✓ (cached, two-phase)`
//! so that CI pipelines see the same logs on every run.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Types ─────────────────────────────────────────────────────────────────

/// Captured stdout, stderr, and exit code for a task run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    /// Raw stdout content (UTF-8 lossy).
    pub stdout: String,
    /// Raw stderr content (UTF-8 lossy).
    pub stderr: String,
    /// Exit code of the task.
    pub exit_code: i32,
}

// ─── Storage helpers ───────────────────────────────────────────────────────

/// Write `output` to `{cache_dir}/sf-{sf}.output.json`.
/// Failures are silently ignored — output capture is best-effort.
pub fn write_output(cache_dir: &Path, sf: &str, output: &TaskOutput) {
    let path = output_path(cache_dir, sf);
    if let Ok(json) = serde_json::to_string_pretty(output) {
        let _ = std::fs::write(path, json);
    }
}

/// Read the stored output for `sf`.  Returns `None` if not found or invalid.
pub fn read_output(cache_dir: &Path, sf: &str) -> Option<TaskOutput> {
    let raw = std::fs::read_to_string(output_path(cache_dir, sf)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn output_path(dir: &Path, sf: &str) -> PathBuf {
    dir.join(format!("sf-{sf}.output.json"))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_write_read() {
        let dir = tempdir().unwrap();
        let output = TaskOutput {
            stdout: "hello world\n".to_string(),
            stderr: "warning: something\n".to_string(),
            exit_code: 0,
        };
        write_output(dir.path(), "abc123", &output);

        let read_back = read_output(dir.path(), "abc123").unwrap();
        assert_eq!(read_back.stdout, "hello world\n");
        assert_eq!(read_back.stderr, "warning: something\n");
        assert_eq!(read_back.exit_code, 0);
    }

    #[test]
    fn missing_sf_returns_none() {
        let dir = tempdir().unwrap();
        assert!(read_output(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn empty_output_roundtrips() {
        let dir = tempdir().unwrap();
        let output = TaskOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        };
        write_output(dir.path(), "empty123", &output);
        let back = read_output(dir.path(), "empty123").unwrap();
        assert!(back.stdout.is_empty());
        assert!(back.stderr.is_empty());
    }
}
