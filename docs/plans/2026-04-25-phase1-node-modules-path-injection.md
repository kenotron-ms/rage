# Phase 1 — node_modules/.bin PATH Injection

**Status:** Planned  
**Branch:** `feat/phase1-node-modules-path`  
**Crates touched:** `scheduler`, `plugin-typescript`

---

## Problem

`crates/scheduler/src/runner.rs` spawns every task via `sh -c` with no PATH
modification. Local `node_modules/.bin` binaries (`tsc`, `jest`, `eslint`)
are not found, resulting in exit 127 errors.

Five spawn sites in `runner.rs`:

| Function | ~Line | Notes |
|---|---|---|
| `run_tasks` / single-phase | 210 | direct `Command::new("sh")` |
| `run_root_task_legacy` | 282 | root tasks in legacy path |
| `run_single_task_two_phase` / Loose | 441 | sandbox=Loose |
| `run_single_task_two_phase` / fallback | 466 | sandbox unavailable |
| `run_root_task_two_phase` | 537 | root tasks, two-phase |

Also: `sandbox::run_sandboxed(cmd, cwd, &[])` at ~456 — empty env slice means PATH is not injected into sandboxed runs either.

And: `which_first()` at ~336 only searches system PATH for tool binary hashing.

---

## Implementation tasks (TDD order)

### Task 1 — Add `workspace_root` to `Task` struct

File: `crates/scheduler/src/task.rs`

Add field:
```rust
/// Workspace root — used to prepend `{workspace_root}/node_modules/.bin` to PATH.
pub workspace_root: PathBuf,
```

Update `build_task_list` to propagate `workspace_root` to every Task:
- Root tasks: `cwd = workspace_root.to_path_buf()`, `workspace_root = workspace_root.to_path_buf()`
- Package tasks: `cwd = pkg.path.clone()`, `workspace_root = workspace_root.to_path_buf()`

Update test helper `mk_task` in `runner.rs` to add `workspace_root: PathBuf::from("/tmp")`.

Update all other Task construction sites in tests (inline Task { ... } blocks).

### Task 2 — `node_bin_path` helper + unit tests (write tests first)

Add to `crates/scheduler/src/runner.rs`:

```rust
/// Build a PATH value that prepends node_modules/.bin dirs for the task.
///
/// Prepends in order:
///   1. `{cwd}/node_modules/.bin`   (package-local)
///   2. `{workspace_root}/node_modules/.bin`  (workspace-level, skipped if == pkg)
///   3. existing PATH
fn node_bin_path(cwd: &Path, workspace_root: &Path) -> std::ffi::OsString {
    let pkg_bin = cwd.join("node_modules/.bin");
    let ws_bin  = workspace_root.join("node_modules/.bin");
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut segments: Vec<std::ffi::OsString> = vec![pkg_bin.into_os_string()];
    if ws_bin != cwd.join("node_modules/.bin") {
        segments.push(ws_bin.into_os_string());
    }
    segments.push(existing);
    std::env::join_paths(segments).unwrap_or_default()
}
```

Tests:
```rust
#[test]
fn node_bin_path_deduplicates_when_cwd_is_workspace_root() {
    let dir = PathBuf::from("/ws/packages/foo");
    let result = node_bin_path(&dir, &dir);
    let s = result.to_string_lossy();
    let count = s.matches("node_modules/.bin").count();
    assert_eq!(count, 1, "same dir should only appear once");
}

#[test]
fn node_bin_path_prepends_both_when_different() {
    let cwd = PathBuf::from("/ws/packages/foo");
    let ws  = PathBuf::from("/ws");
    let result = node_bin_path(&cwd, &ws);
    let s = result.to_string_lossy().into_owned();
    let pkg_pos = s.find("/ws/packages/foo/node_modules/.bin").unwrap();
    let ws_pos  = s.find("/ws/node_modules/.bin").unwrap();
    assert!(pkg_pos < ws_pos, "package-local bin must come before workspace bin");
}
```

### Task 3 — Apply PATH injection at all 5 Command spawn sites

Pattern:
```rust
let new_path = node_bin_path(&task.cwd, &task.workspace_root);
Command::new("sh")
    .arg("-c").arg(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path)
    .status().await
```

For the sandbox call, build env override:
```rust
let new_path = node_bin_path(&task.cwd, &task.workspace_root);
let env_pairs = vec![
    ("PATH".to_string(), new_path.to_string_lossy().into_owned()),
];
match sandbox::run_sandboxed(&task.command, &task.cwd, &env_pairs).await {
```

### Task 4 — Update `which_first` to search local bins first

New signature:
```rust
fn which_first(command: &str, cwd: &Path, workspace_root: &Path) -> Option<PathBuf>
```

New search order:
1. `{cwd}/node_modules/.bin/{token}` (package-local)
2. `{workspace_root}/node_modules/.bin/{token}` (workspace)
3. System PATH entries

Update call site:
```rust
let tool_path = which_first(&task.command, &task.cwd, &task.workspace_root)
    .unwrap_or_else(|| PathBuf::from("sh"));
```

Test:
```rust
#[test]
fn which_first_prefers_local_node_modules_bin() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("node_modules/.bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tsc = bin_dir.join("tsc");
    std::fs::write(&tsc, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt;
      std::fs::set_permissions(&tsc, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    let result = which_first("tsc --noEmit", dir.path(), dir.path());
    assert_eq!(result.as_deref(), Some(tsc.as_path()));
}
```

### Task 5 — TypeScript plugin allowlist

Add to `toolchain_allowlist()` in `crates/plugin-typescript/src/lib.rs`:
```rust
AllowlistEntry {
    path_pattern: "**/node_modules/.bin/**".to_string(),
    reason: "locally-installed tool binaries (tsc, eslint, jest, etc.)".to_string(),
},
```

### Task 6 — Integration test: PATH reaches subprocess

```rust
#[tokio::test]
async fn node_modules_bin_is_on_path_during_task() {
    let dir = tempdir().unwrap();
    let bin_dir = dir.path().join("node_modules/.bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin_path = bin_dir.join("fake-tsc");
    let sentinel = dir.path().join("fake-tsc-ran.txt");
    std::fs::write(&bin_path,
        format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()).as_bytes()
    ).unwrap();
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt;
      std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap(); }

    let task = Task {
        package_name: "test-pkg".to_string(),
        script_name: "build".to_string(),
        command: "fake-tsc".to_string(),
        cwd: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        sandbox_mode: pipeline_config::SandboxMode::Loose,
        is_root: false,
        input_paths: Vec::new(),
    };
    let pkg = mk_pkg("test-pkg", &[]);
    let dag = build_dag(vec![pkg]).unwrap();
    let cache_dir = tempdir().unwrap();
    let cache = std::sync::Arc::new(
        cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap()
    );
    run_tasks_two_phase(&dag, vec![task], cache).await.unwrap();
    assert!(sentinel.exists(), "fake-tsc must be found via node_modules/.bin");
}
```

---

## Files modified

| File | Change |
|------|--------|
| `crates/scheduler/src/task.rs` | Add `workspace_root: PathBuf` to Task; update build_task_list |
| `crates/scheduler/src/runner.rs` | node_bin_path helper; 5 spawn-site updates; which_first signature |
| `crates/plugin-typescript/src/lib.rs` | Add .bin/** allowlist entry |
| `crates/cli/src/main.rs` | Pass workspace_root when constructing tasks (if not already done) |

---

## Acceptance criteria

- `fake-tsc` integration test passes
- `which_first` returns local bin when available
- All existing tests pass
- `rage run build ~/workspace/lage` shows build lines rather than `tsc: command not found`
