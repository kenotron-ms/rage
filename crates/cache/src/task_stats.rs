//! Per-task historical resource statistics, keyed by weak fingerprint.
//!
//! After each subprocess run, rage records the peak RSS observed during
//! execution.  On subsequent runs, rage reads the stored estimate and uses
//! it to make memory-aware admission decisions before spawning the
//! subprocess.  This mirrors BuildXL's "historical pip performance data"
//! that feeds its dynamic resource scheduler.
//!
//! Storage:  `{cache_dir}/stats/{wf_prefix8}/{wf_full}.json`
//! Format:   `{ "samples_bytes": [u64, ...], "run_count": u32 }`
//!
//! Only the last `MAX_SAMPLES` RSS observations are kept.  The estimate
//! returned by `TaskStats::estimate_bytes` is the **p75** value — high
//! enough to avoid systematic under-allocation, low enough to avoid
//! chronic starvation.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Maximum number of RSS samples retained per task.
const MAX_SAMPLES: usize = 16;

/// Historical peak-RSS observations for one task (identified by its WF hash).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskStats {
    /// Observed peak-RSS values in bytes, newest last.
    /// Capped at `MAX_SAMPLES`.
    pub samples_bytes: Vec<u64>,
    /// Total completed runs recorded.
    pub run_count: u32,
}

impl TaskStats {
    /// Add a new observation and evict the oldest if at capacity.
    pub fn record(&mut self, peak_rss_bytes: u64) {
        if self.samples_bytes.len() >= MAX_SAMPLES {
            self.samples_bytes.remove(0);
        }
        self.samples_bytes.push(peak_rss_bytes);
        self.run_count += 1;
    }

    /// P75 estimate — used as the pre-spawn memory reservation.
    ///
    /// Returns `None` when no samples exist (caller should use a default).
    pub fn estimate_bytes(&self) -> Option<u64> {
        if self.samples_bytes.is_empty() {
            return None;
        }
        let mut sorted = self.samples_bytes.clone();
        sorted.sort_unstable();
        // p75 index: floor(0.75 * (n-1))
        let idx = ((sorted.len() - 1) as f64 * 0.75) as usize;
        Some(sorted[idx])
    }
}

// ── storage helpers ────────────────────────────────────────────────────────

fn stats_path(cache_dir: &Path, wf: &str) -> std::path::PathBuf {
    // Shard by first 8 chars to avoid huge flat directories.
    let prefix = &wf[..wf.len().min(8)];
    cache_dir
        .join("stats")
        .join(prefix)
        .join(format!("{wf}.json"))
}

/// Load stats for a given WF hash. Returns `Default::default()` on any error
/// (missing file, corrupt JSON) — callers treat missing stats as "unknown".
pub fn load(cache_dir: &Path, wf: &str) -> TaskStats {
    let path = stats_path(cache_dir, wf);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return TaskStats::default(),
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Persist updated stats for a WF hash.  Errors are logged but never fatal —
/// a missing stats file just means the next run uses the default estimate.
pub fn save(cache_dir: &Path, wf: &str, stats: &TaskStats) -> Result<()> {
    let path = stats_path(cache_dir, wf);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating stats dir {}", parent.display()))?;
    }
    let json = serde_json::to_string(stats).context("serializing TaskStats")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn estimate_is_none_when_empty() {
        assert!(TaskStats::default().estimate_bytes().is_none());
    }

    #[test]
    fn estimate_p75_single_sample() {
        let mut s = TaskStats::default();
        s.record(100_000_000);
        assert_eq!(s.estimate_bytes(), Some(100_000_000));
    }

    #[test]
    fn estimate_p75_four_samples() {
        let mut s = TaskStats::default();
        // sorted: [100, 200, 300, 400] MB  →  p75 = idx 2 = 300 MB
        for v in [200, 100, 400, 300] {
            s.record(v * 1_048_576);
        }
        assert_eq!(s.estimate_bytes(), Some(300 * 1_048_576));
    }

    #[test]
    fn evicts_oldest_beyond_max() {
        let mut s = TaskStats::default();
        for i in 0..(MAX_SAMPLES + 5) as u64 {
            s.record(i * 1_000_000);
        }
        assert_eq!(s.samples_bytes.len(), MAX_SAMPLES);
        // run_count tracks all observations
        assert_eq!(s.run_count, (MAX_SAMPLES + 5) as u32);
    }

    #[test]
    fn roundtrip_load_save() {
        let tmp = TempDir::new().unwrap();
        let wf = "deadbeefcafe1234abcd";
        let mut stats = TaskStats::default();
        stats.record(512 * 1_048_576);
        save(tmp.path(), wf, &stats).unwrap();
        let loaded = load(tmp.path(), wf);
        assert_eq!(loaded.samples_bytes, stats.samples_bytes);
        assert_eq!(loaded.run_count, 1);
    }

    #[test]
    fn load_returns_default_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let s = load(tmp.path(), "nonexistent_wf_hash");
        assert!(s.samples_bytes.is_empty());
    }
}
