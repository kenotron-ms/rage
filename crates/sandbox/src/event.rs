use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// An individual file-system access event emitted by the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum AccessEvent {
    Read { path: String, pid: u32 },
    Write { path: String, pid: u32 },
}

/// Sorted, deduplicated sets of paths that were read and written during a
/// sandboxed run.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathSet {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
}

impl PathSet {
    /// Build a `PathSet` from a slice of [`AccessEvent`]s.
    ///
    /// Paths are deduplicated and sorted (reads and writes into separate buckets).
    pub fn from_events(events: &[AccessEvent]) -> Self {
        let mut reads: BTreeSet<PathBuf> = BTreeSet::new();
        let mut writes: BTreeSet<PathBuf> = BTreeSet::new();

        for event in events {
            match event {
                AccessEvent::Read { path, .. } => {
                    reads.insert(PathBuf::from(path));
                }
                AccessEvent::Write { path, .. } => {
                    writes.insert(PathBuf::from(path));
                }
            }
        }

        PathSet {
            reads: reads.into_iter().collect(),
            writes: writes.into_iter().collect(),
        }
    }
}

/// The outcome of a sandboxed command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResult {
    pub exit_code: i32,
    pub path_set: PathSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathset_dedupes_and_sorts() {
        let events = vec![
            AccessEvent::Read {
                path: "/b".into(),
                pid: 1,
            },
            AccessEvent::Read {
                path: "/a".into(),
                pid: 1,
            },
            AccessEvent::Read {
                path: "/a".into(),
                pid: 2,
            },
        ];
        let ps = PathSet::from_events(&events);
        assert_eq!(ps.reads, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(ps.writes.is_empty());
    }

    #[test]
    fn read_then_write_separates_buckets() {
        let events = vec![
            AccessEvent::Read {
                path: "/r".into(),
                pid: 1,
            },
            AccessEvent::Write {
                path: "/w".into(),
                pid: 1,
            },
        ];
        let ps = PathSet::from_events(&events);
        assert_eq!(ps.reads, vec![PathBuf::from("/r")]);
        assert_eq!(ps.writes, vec![PathBuf::from("/w")]);
    }
}
