# Phase 2 — Wire declared_input_globs into TwoPhaseCache

**Status:** Planned  
**Branch:** `feat/phase2-declared-input-globs`  
**Crates touched:** `scheduler`, `cache` (tests only)

---

## Problem

`WeakFpInputs.declared_input_globs` is already a field on the struct, but it
is ALWAYS passed as `&[]` in `run_single_task_two_phase`:

```rust
let inputs = WeakFpInputs {
    command: &task.command,
    tool_path: &tool_path,
    package_path: &task.cwd,
    declared_input_globs: &[],   // ← stub, never wired
    tracked_env: &[],
};
```

On Linux (no sandbox), the pathset recorded by `StoredPathset` is always empty
(the unsupported stub returns `Ok(RunResult { path_set: PathSet::default() })`).
With empty declared globs AND empty pathset, the WF hash doesn't include any
source file content, so changing a `.ts` file silently gives a cache hit.

---

## Fix

1. Add `declared_input_globs: Vec<String>` to `Task` in `task.rs`.
2. In `build_task_list`, for each package task:
   - Detect which plugins apply to the package dir (check if any detection glob
     matches a file in the package root)
   - Collect `plugin.declared_input_globs(script_name, &PluginConfig::default())`
     from each matching plugin
   - Deduplicate and store in `task.declared_input_globs`
3. In `build_task_list_with_config`, respect per-plugin config from `rage.json`
   (`plugins_config.<plugin_id>`) when computing globs.
4. In `run_single_task_two_phase`, replace `declared_input_globs: &[]` with
   `declared_input_globs: &task.declared_input_globs`.

---

## Implementation tasks (TDD order)

### Task 1 — Add `declared_input_globs` to Task struct

File: `crates/scheduler/src/task.rs`

```rust
/// Globs (relative to package root) that declare this task's input files.
/// Used as the weak-fingerprint input set for TwoPhaseCache.
/// Populated by the ecosystem plugin(s) that apply to this package.
/// Empty for root tasks (they use input_paths instead).
pub declared_input_globs: Vec<String>,
```

### Task 2 — Helper: detect plugins that apply to a package

In `build_task_list`, after getting each package path, check if the package
has any of the plugin's `detection_globs()` as existing files:

```rust
fn plugins_for_package(
    pkg_path: &Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
) -> Vec<&dyn plugin::EcosystemPlugin> { ... }
```

Actually, simpler: just iterate plugins and check with glob matching.

For the TypeScript plugin, `detection_globs()` returns `["tsconfig.json",
"tsconfig.*.json"]`. If `pkg_path/tsconfig.json` exists, the TS plugin applies.

Simple check: for each detection glob `g`, if `pkg_path.join(g)` exists as a
file, the plugin matches. (Glob expansion not needed for the simple case.)

### Task 3 — Wire globs into build_task_list

For package tasks:
```rust
let globs: Vec<String> = plugins
    .iter()
    .filter(|p| {
        p.detection_globs().iter().any(|g| {
            // Simple case: direct file check (no glob expansion needed for tsconfig.json)
            pkg.path.join(g).exists()
            // Full glob matching for patterns like "tsconfig.*.json"
            || globset::Glob::new(g).ok()
                .and_then(|g| g.compile_matcher().ok()... // trickier
        })
    })
    .flat_map(|p| p.declared_input_globs(script_name, &plugin::PluginConfig::default()))
    .collect::<std::collections::HashSet<_>>()
    .into_iter()
    .collect();

tasks.push(Task {
    ...
    declared_input_globs: globs,
    ...
});
```

Actually for the detection check, use a simple approach: for each plugin, check
if ANY detection glob matches any file in the package root using glob expansion.

```rust
fn package_matches_plugin(pkg_path: &Path, plugin: &dyn plugin::EcosystemPlugin) -> bool {
    use globset::{Glob, GlobSetBuilder};
    let mut builder = GlobSetBuilder::new();
    for g in plugin.detection_globs() {
        if let Ok(glob) = Glob::new(g) {
            builder.add(glob);
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(_) => return false,
    };
    // Only check direct children of pkg_path (not recursive)
    std::fs::read_dir(pkg_path)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let name = e.file_name();
            set.is_match(std::path::Path::new(&name))
        })
}
```

### Task 4 — Wire globs in runner.rs

In `run_single_task_two_phase`:
```rust
let inputs = WeakFpInputs {
    command: &task.command,
    tool_path: &tool_path,
    package_path: &task.cwd,
    declared_input_globs: &task.declared_input_globs,   // ← was &[]
    tracked_env: &[],
};
```

### Task 5 — Update all Task construction sites in tests

Add `declared_input_globs: Vec::new()` to all test Task { ... } constructors
(same as how we added `workspace_root` in Phase 1).

### Task 6 — Regression test: WF changes when declared input changes

```rust
#[test]
fn declared_input_change_invalidates_wf() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/index.ts"), b"export const x = 1;").unwrap();

    let tool = PathBuf::from("/usr/bin/env");
    let inputs_v1 = WeakFpInputs {
        command: "tsc",
        tool_path: &tool,
        package_path: dir.path(),
        declared_input_globs: &["src/**/*.ts".to_string()],
        tracked_env: &[],
    };
    let wf_v1 = compute_weak_fingerprint(&inputs_v1);

    // Mutate the declared input
    std::fs::write(dir.path().join("src/index.ts"), b"export const x = 2;").unwrap();

    let wf_v2 = compute_weak_fingerprint(&inputs_v1); // same globs, different file content
    assert_ne!(wf_v1, wf_v2, "WF must change when declared input file changes");
}
```

### Task 7 — Integration test: real task with TS plugin globs

In scheduler tests, build a fake TS package, wire TypeScript plugin, confirm
that after a source change the task re-runs (cache miss) because WF changed.

```rust
#[tokio::test]
async fn source_change_causes_cache_miss_with_input_globs() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    // Create a minimal package with tsconfig.json so TS plugin is detected
    std::fs::write(dir.path().join("tsconfig.json"), b"{}").unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("index.ts");
    std::fs::write(&src_file, b"export const v = 1;").unwrap();

    // Build a task with TypeScript plugin globs
    let ts_plugin = plugin_typescript::TypeScriptPlugin::new();
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = vec![&ts_plugin];
    // (Use build_task_list to auto-detect globs)
    
    let task = Task {
        package_name: "ts-pkg".to_string(),
        script_name: "build".to_string(),
        command: "echo build".to_string(),
        cwd: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        sandbox_mode: pipeline_config::SandboxMode::Loose,
        is_root: false,
        input_paths: Vec::new(),
        declared_input_globs: vec![
            "src/**/*.ts".to_string(),
            "tsconfig*.json".to_string(),
            "package.json".to_string(),
        ],
    };
    
    let cache_dir = tempdir().unwrap();
    let cache = std::sync::Arc::new(
        cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap()
    );
    let pkg = mk_pkg("ts-pkg", &[]);
    let dag = build_dag(vec![pkg]).unwrap();
    
    // First run — miss
    run_tasks_two_phase(&dag, vec![task.clone()], cache.clone()).await.unwrap();
    
    // Mutate source file
    std::fs::write(&src_file, b"export const v = 2;").unwrap();
    
    // Second run — should MISS because WF changed
    run_tasks_two_phase(&dag, vec![task], cache).await.unwrap();
    
    // Verify two wf-*.pathsets files exist (two separate WF entries)
    let wf_files: Vec<_> = std::fs::read_dir(cache_dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("wf-"))
        .collect();
    assert_eq!(wf_files.len(), 2, "source change should produce 2 distinct WF entries");
}
```

---

## Files to modify

| File | Change |
|------|--------|
| `crates/scheduler/src/task.rs` | Add `declared_input_globs: Vec<String>` to Task; wire in build_task_list |
| `crates/scheduler/src/runner.rs` | Use `&task.declared_input_globs` in WeakFpInputs; update test helpers |

---

## Acceptance criteria

1. Modify a `.ts` source file in a fixture package; confirm WF hash changes
2. Regression test: `source_change_causes_cache_miss_with_input_globs` passes
3. All existing tests pass (`cargo test --workspace`)
