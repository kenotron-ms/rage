# Phase 5a — rage why-miss

**Status:** Planned  
**Branch:** `feat/phase5a-why-miss`  
**New crates:** none  
**Modified crates:** `cache`, `scheduler`, `cli`

---

## Problem

When a task is a cache miss, the developer has no visibility into WHY the cache
missed. Was it a source file change? A tool version change? An env var change?
Without this information, debugging cache behavior is impossible.

---

## Design

### Data collected per task run

Every time `run_single_task_two_phase` is called (even on a cache hit), record
a snapshot of the WF inputs to disk:

```
~/.rage/cache/why/{pkg_slug}-{script}.snapshot.json
```

Each snapshot is a small JSON object:
```json
{
  "timestamp": 1700000000,
  "pkg": "@lage-run/core",
  "script": "build",
  "command": "tsc",
  "tool_path": "/ws/node_modules/.bin/tsc",
  "tool_hash": "abc123...",
  "inputs": [
    { "path": "src/index.ts", "hash": "def456..." },
    { "path": "tsconfig.json", "hash": "ghi789..." }
  ],
  "env": [["CI", "1"]],
  "dep_abi_fps": [["@lage-run/types", "jkl012..."]]
}
```

We keep at most the **last 2** snapshots per task (old + new). The file is
rewritten on every run. If only one snapshot exists, `why-miss` reports
"first run — no baseline to compare".

### CLI command

```
rage why-miss <package> <script> [--workspace PATH]
```

Reads `~/.rage/cache/why/{pkg_slug}-{script}.snapshot.json`, diffs the two
most recent entries, and prints to stderr:

```
[rage why-miss] @lage-run/core#build — comparing run 2 vs run 1

  CHANGED INPUT FILES
    src/index.ts
      was: abc123...
      now: def456...

  UNCHANGED (no diff)
    tsconfig.json

  TOOL HASH: unchanged (tsc @ /ws/node_modules/.bin/tsc)
  COMMAND:   unchanged (tsc)
  ENV:       unchanged
```

---

## Implementation

### Step 1 — `WhyMissSnapshot` in `crates/cache/src/why_miss.rs`

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEntry {
    pub path: PathBuf,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyMissSnapshot {
    pub timestamp: u64,
    pub pkg: String,
    pub script: String,
    pub command: String,
    pub tool_path: String,
    pub tool_hash: String,
    pub inputs: Vec<InputEntry>,
    pub env: Vec<(String, String)>,
    pub dep_abi_fps: Vec<(String, String)>,
}

/// Read the stored pair of snapshots for a task. Returns (old, new).
/// Returns None if no snapshots exist.
pub fn read_snapshots(
    cache_dir: &Path,
    pkg: &str,
    script: &str,
) -> Option<(WhyMissSnapshot, WhyMissSnapshot)> {
    let path = snapshot_path(cache_dir, pkg, script);
    let raw = std::fs::read_to_string(&path).ok()?;
    let snaps: Vec<WhyMissSnapshot> = serde_json::from_str(&raw).ok()?;
    if snaps.len() < 2 {
        return None;
    }
    let old = snaps[snaps.len() - 2].clone();
    let new = snaps[snaps.len() - 1].clone();
    Some((old, new))
}

/// Append a snapshot, keeping only the last 2.
pub fn record_snapshot(cache_dir: &Path, snap: WhyMissSnapshot) {
    let _ = std::fs::create_dir_all(cache_dir.join("why"));
    let path = snapshot_path(cache_dir, &snap.pkg, &snap.script);
    
    let mut existing: Vec<WhyMissSnapshot> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    existing.push(snap);
    
    // Keep last 2
    if existing.len() > 2 {
        existing.drain(..existing.len() - 2);
    }
    
    if let Ok(json) = serde_json::to_string_pretty(&existing) {
        let _ = std::fs::write(&path, json);
    }
}

fn snapshot_path(dir: &Path, pkg: &str, script: &str) -> PathBuf {
    let slug = pkg.replace('/', "__").replace('@', "_at_");
    dir.join("why").join(format!("{slug}-{script}.snapshot.json"))
}
```

### Step 2 — Record snapshot in `runner.rs`

In `run_single_task_two_phase`, after computing `inputs` and `dep_abi_fps`:

```rust
// Record snapshot for rage why-miss
if let Ok(cache_dir) = std::env::var("RAGE_CACHE_DIR") {
    let snap = cache::why_miss::WhyMissSnapshot {
        timestamp: ...,
        pkg: task.package_name.clone(),
        script: task.script_name.clone(),
        command: task.command.clone(),
        tool_path: tool_path.to_string_lossy().into_owned(),
        tool_hash: ...,
        inputs: resolved_inputs.iter().map(|(p, h)| InputEntry { path: p.clone(), hash: h.clone() }).collect(),
        env: vec![],
        dep_abi_fps: dep_abi_fps.clone(),
    };
    cache::why_miss::record_snapshot(&PathBuf::from(cache_dir), snap);
}
```

But to avoid the RAGE_CACHE_DIR env var, we need to pass the cache_dir through.
Best approach: Add `cache_dir: Option<&Path>` parameter to `run_tasks_two_phase`,
or pass the `TwoPhaseCache` ref which has `dir()`.

Since `run_single_task_two_phase` already has `cache: Arc<TwoPhaseCache>`, we
can use `cache.dir()` to get the cache directory.

### Step 3 — Add `cmd_why_miss` to CLI

In `crates/cli/src/main.rs`:

```rust
#[command = "WhyMiss"]
WhyMiss {
    /// Package name (e.g. `@lage-run/core`).
    package: String,
    /// Script name (e.g. `build`).
    script: String,
    #[arg(long)]
    workspace: Option<PathBuf>,
    workspace_pos: Option<PathBuf>,
}
```

Implementation:
```rust
fn cmd_why_miss(root: &Path, pkg: &str, script: &str) {
    let cache_dir = resolve_cache_dir(root);
    match cache::why_miss::read_snapshots(&cache_dir, pkg, script) {
        None => eprintln!("[rage why-miss] no snapshots found for {pkg}#{script}"),
        Some((old, new)) => print_diff(&old, &new),
    }
}

fn print_diff(old: &WhyMissSnapshot, new: &WhyMissSnapshot) {
    eprintln!("[rage why-miss] {}#{} — comparing run 2 vs run 1", new.pkg, new.script);
    eprintln!();
    
    // Command diff
    if old.command != new.command {
        eprintln!("  COMMAND CHANGED");
        eprintln!("    was: {}", old.command);
        eprintln!("    now: {}", new.command);
    }
    
    // Tool hash diff
    if old.tool_hash != new.tool_hash {
        eprintln!("  TOOL BINARY CHANGED ({})", new.tool_path);
        eprintln!("    was: {}", old.tool_hash);
        eprintln!("    now: {}", new.tool_hash);
    }
    
    // Input file diffs
    ...
    
    // Dep ABI diffs
    ...
}
```

---

## Tests

1. Unit test: `record_snapshot` keeps at most 2 entries
2. Unit test: `read_snapshots` returns (old, new) correctly
3. Integration test: run a task twice with a source change; `rage why-miss` output contains the changed file

---

## Acceptance criteria

- Run `rage run build` twice with source file change between runs
- `rage why-miss <pkg> build` names the changed file
- Integration test: trigger a miss, assert `why-miss` output contains the mutated file path
