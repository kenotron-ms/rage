//! Cache entry data model.

use serde::{Deserialize, Serialize};

/// A stored result for a task execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheEntry {
    /// The fingerprint (blake3 hex) that produced this entry.
    pub fingerprint: String,
    /// The command that was executed.
    pub command: String,
    /// Exit code of the task (0 = success).
    pub exit_code: i32,
    /// Wall-clock time in milliseconds.
    pub elapsed_ms: u64,
    /// Unix timestamp (seconds) when the entry was stored.
    pub cached_at: u64,
    /// Pathset reads observed by the sandbox on the run that produced this
    /// entry. Used for diagnostics (`rage why-miss`). Optional for back-compat
    /// with entries written by single-phase cache.
    #[serde(default)]
    pub pathset_reads: Vec<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let entry = CacheEntry {
            fingerprint: "abc123".to_string(),
            command: "echo hello".to_string(),
            exit_code: 0,
            elapsed_ms: 42,
            cached_at: 1_700_000_000,
            pathset_reads: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: CacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn fields_serialize_with_snake_case_names() {
        let entry = CacheEntry {
            fingerprint: "fp".to_string(),
            command: "cmd".to_string(),
            exit_code: 1,
            elapsed_ms: 100,
            cached_at: 0,
            pathset_reads: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"fingerprint\""));
        assert!(json.contains("\"exit_code\""));
        assert!(json.contains("\"elapsed_ms\""));
        assert!(json.contains("\"cached_at\""));
    }

    #[test]
    fn entry_carries_pathset_reads() {
        let e = CacheEntry {
            fingerprint: "fp".into(),
            command: "cmd".into(),
            exit_code: 0,
            elapsed_ms: 10,
            cached_at: 0,
            pathset_reads: vec![
                std::path::PathBuf::from("/a"),
                std::path::PathBuf::from("/b"),
            ],
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: CacheEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back.pathset_reads, e.pathset_reads);
    }

    #[test]
    fn entry_back_compat_no_pathset_reads_in_old_json() {
        // Existing JSON files written by the single-phase cache lack
        // pathset_reads. Decoding must default the field to an empty vec.
        let old =
            r#"{"fingerprint":"fp","command":"cmd","exit_code":0,"elapsed_ms":1,"cached_at":0}"#;
        let e: CacheEntry = serde_json::from_str(old).unwrap();
        assert!(e.pathset_reads.is_empty());
    }
}
