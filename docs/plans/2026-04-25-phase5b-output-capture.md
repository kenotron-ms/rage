# Phase 5b — Output Capture and Replay

**Status:** Planned  
**Branch:** `feat/phase5b-output-capture`  
**Modified crates:** `cache`, `scheduler`

---

## Problem

On cache hit, `runner.rs` prints `✓ (cached)` and returns. The original
stdout/stderr of the task is lost. CI pipelines expect build output even on
cache hits (e.g., TypeScript type errors must be visible even if the build
was cached).

---

## Design

### New storage: `sf-{SF}.output.json`

Alongside each `sf-{SF}.entry` file, write a companion output file:
```json
{
  "stdout": "compiling src/index.ts...\n",
  "stderr": "",
  "exit_code": 0
}
```

### On cache miss

Instead of `Command::new("sh")...status()`, use
`Command::new("sh")...stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()`.

Then read from stdout/stderr pipes concurrently, writing each chunk to:
1. `std::io::stdout()` / `std::io::stderr()` for live display
2. An in-memory buffer for storage

After the command exits, write the buffer to `sf-{SF}.output.json`.

### On cache hit

Read `sf-{SF}.output.json` and replay:
```
print!(stdout)
eprint!(stderr)
[rage] pkg#script ✓ (cached, two-phase)
```

### New type in cache crate

```rust
// crates/cache/src/output_store.rs
pub struct TaskOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn write_output(cache_dir: &Path, sf: &str, output: &TaskOutput)
pub fn read_output(cache_dir: &Path, sf: &str) -> Option<TaskOutput>
```

---

## Implementation

### Step 1 — `OutputStore` in `crates/cache/src/output_store.rs`

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn write_output(cache_dir: &Path, sf: &str, output: &TaskOutput) {
    let path = output_path(cache_dir, sf);
    if let Ok(json) = serde_json::to_string_pretty(output) {
        let _ = std::fs::write(path, json);
    }
}

pub fn read_output(cache_dir: &Path, sf: &str) -> Option<TaskOutput> {
    let raw = std::fs::read_to_string(output_path(cache_dir, sf)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn output_path(dir: &Path, sf: &str) -> PathBuf {
    dir.join(format!("sf-{sf}.output.json"))
}
```

### Step 2 — Capture helper in runner.rs

```rust
/// Spawn a command with piped stdout+stderr; tee to terminal while capturing.
async fn spawn_capture(
    cmd_builder: tokio::process::Command,
) -> std::io::Result<(i32, String, String)> {
    let mut child = cmd_builder
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    
    // Read stdout and stderr concurrently
    let (_, _) = tokio::join!(
        async {
            let mut buf = [0u8; 4096];
            loop {
                match stdout_pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tokio::io::stdout().write_all(&buf[..n]).await;
                        stdout_buf.extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        },
        async {
            let mut buf = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = tokio::io::stderr().write_all(&buf[..n]).await;
                        stderr_buf.extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        }
    );
    
    let status = child.wait().await?;
    let code = status.code().unwrap_or(-1);
    
    Ok((
        code,
        String::from_utf8_lossy(&stdout_buf).into_owned(),
        String::from_utf8_lossy(&stderr_buf).into_owned(),
    ))
}
```

### Step 3 — Update cache hit path

```rust
if let Some((sf, _entry)) = cache.lookup(&inputs) {
    // Replay captured output before printing the cache-hit line.
    if let Some(output) = cache::output_store::read_output(cache.dir(), &sf) {
        print!("{}", output.stdout);
        eprint!("{}", output.stderr);
    }
    eprintln!("[rage] {}#{} ✓ (cached, two-phase)", task.package_name, task.script_name);
    return Ok(());
}
```

### Step 4 — Update cache miss path

Replace `.status()` with the new `spawn_capture` helper.
After recording the cache entry, also write `sf-{SF}.output.json`.

---

## Tests

1. Unit: `write_output` / `read_output` roundtrip
2. Integration: first run records output to file; second run (hit) replays it
3. Integration: failed task does NOT write output (only success)

---

## Acceptance criteria

- First run (miss): output appears live and is stored
- Second run (hit): identical output is replayed before `✓ (cached, two-phase)`
- Integration test comparing output between miss and hit runs
