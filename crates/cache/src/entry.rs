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
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"fingerprint\""));
        assert!(json.contains("\"exit_code\""));
        assert!(json.contains("\"elapsed_ms\""));
        assert!(json.contains("\"cached_at\""));
    }
}
