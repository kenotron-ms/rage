use std::path::PathBuf;

use crate::event::{PathSet, RunResult};

/// A test double for sandboxed execution.
///
/// Use [`MockSandbox::ok`] to configure the outcome, then call [`MockSandbox::run`]
/// to obtain a [`RunResult`].
#[derive(Debug, Clone)]
pub struct MockSandbox {
    pub exit_code: i32,
    pub path_set: PathSet,
}

impl MockSandbox {
    /// Create a `MockSandbox` that exits successfully (code 0) and reports
    /// the given read/write paths.
    pub fn ok(reads: Vec<PathBuf>, writes: Vec<PathBuf>) -> Self {
        MockSandbox {
            exit_code: 0,
            path_set: PathSet { reads, writes },
        }
    }

    /// Return the configured [`RunResult`].
    pub fn run(&self) -> RunResult {
        RunResult {
            exit_code: self.exit_code,
            path_set: self.path_set.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_configured_pathset() {
        let reads = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let writes = vec![PathBuf::from("/c")];
        let mock = MockSandbox::ok(reads.clone(), writes.clone());
        let result = mock.run();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.path_set.reads, reads);
        assert_eq!(result.path_set.writes, writes);
    }
}
