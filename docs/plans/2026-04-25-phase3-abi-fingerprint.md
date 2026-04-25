# Phase 3 — Wire abi_fingerprint into TwoPhaseCache

**Status:** ✅ Complete — merged to `main` at `92437ac`; entry.abi_fingerprint fix at `feat/phase3-complete-abi-fingerprint`  
**Branch:** `feat/phase3-abi-fingerprint`  
**Crates touched:** `cache`, `scheduler`

---

## Problem

`TypeScriptPlugin::abi_fingerprint` hashes `.d.ts` output files but
`two_phase.rs` never calls it. The early-cutoff mechanism exists in the plugin
but is never used.

---

## Design

The ABI fingerprint enables an early-cutoff for downstream tasks: if `core`
changes its implementation but not its `.d.ts` exports, `utils` (which depends
on `core`) doesn't need to rebuild.

**Mechanism:**
1. After task `core#build` completes, compute ABI fingerprint from output files
   and store in `{cache_dir}/pkg-abi/{package_name}` (a small text file)
2. Before computing WF for `utils#build`, read `core`'s stored ABI fingerprint
   and include it in `utils`'s WeakFpInputs
3. If `core`'s ABI fingerprint is unchanged → `utils`'s WF matches the stored
   WF → cache hit for `utils` without re-running

Waves run sequentially, so by the time `utils`'s WF is computed, `core`'s ABI
fingerprint file is already written.

---

## Implementation

### Step 1 — `abi_fingerprint: Option<String>` on CacheEntry

File: `crates/cache/src/entry.rs`

```rust
#[serde(default)]
pub abi_fingerprint: Option<String>,
```

### Step 2 — `dep_abi_fingerprints` in WeakFpInputs + WF computation

File: `crates/cache/src/weak_fp.rs`

Add to `WeakFpInputs`:
```rust
/// ABI fingerprints of immediate upstream dependencies.
/// Each entry is (package_name, abi_hex). Included in WF so that changing
/// a dependency's public API invalidates downstream tasks' WF hashes.
pub dep_abi_fingerprints: &'a [(String, String)],
```

In `compute_weak_fingerprint`, include these after the existing inputs:
```rust
for (dep, afp) in inputs.dep_abi_fingerprints {
    hasher.update(b"dep_abi:");
    hasher.update(dep.as_bytes());
    hasher.update(b"=");
    hasher.update(afp.as_bytes());
    hasher.update(b"\n");
}
```

### Step 3 — `dep_package_names` on Task + pkg-abi store in TwoPhaseCache

Add to `Task`:
```rust
/// Immediate dependency package names (from the DAG). Used to look up their
/// ABI fingerprints when computing this task's weak fingerprint.
pub dep_package_names: Vec<String>,
```

Populate in `build_task_list` from `dag.packages[pkg_name].dependencies`.

Add to `TwoPhaseCache`:
```rust
/// Read a stored ABI fingerprint for `pkg_name`. Returns None if not stored.
pub fn get_pkg_abi_fp(&self, pkg_name: &str) -> Option<String> {
    let path = self.dir.join("pkg-abi").join(pkg_name_to_filename(pkg_name));
    std::fs::read_to_string(&path).ok().map(|s| s.trim().to_string())
}

/// Persist the ABI fingerprint for `pkg_name`.
pub fn set_pkg_abi_fp(&self, pkg_name: &str, abi_fp: &str) {
    let dir = self.dir.join("pkg-abi");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(pkg_name_to_filename(pkg_name));
    let _ = std::fs::write(&path, abi_fp);
}

fn pkg_name_to_filename(name: &str) -> String {
    // Replace `/` and `@` with safe chars for filenames.
    name.replace('/', "__").replace('@', "_at_")
}
```

### Step 4 — Wire in runner.rs

In `run_single_task_two_phase`:

Before computing WF, look up dep ABI fingerprints:
```rust
// Gather dep ABI fingerprints for early-cutoff
let dep_abi_fps: Vec<(String, String)> = task
    .dep_package_names
    .iter()
    .filter_map(|dep| {
        cache.get_pkg_abi_fp(dep).map(|fp| (dep.clone(), fp))
    })
    .collect();

let inputs = WeakFpInputs {
    command: &task.command,
    tool_path: &tool_path,
    package_path: &task.cwd,
    declared_input_globs: &task.declared_input_globs,
    tracked_env: &[],
    dep_abi_fingerprints: &dep_abi_fps,
};
```

After success, compute and store ABI fingerprint:
```rust
// Compute and persist ABI fingerprint for downstream tasks
if let Some(plugin) = find_plugin_for_task(&task) {
    let output_files = collect_output_files(&task.cwd);
    if let Some(abi_fp) = plugin.abi_fingerprint(&output_files) {
        cache.set_pkg_abi_fp(&task.package_name, &abi_fp);
        // Also update the SF entry with the ABI fingerprint
    }
}
```

### Step 5 — Add `output_globs` to Task

The runner needs to know what files were produced to compute ABI fingerprint.
Add `output_globs: Vec<String>` to Task, populated similarly to `declared_input_globs`.

TypeScript plugin's `infer_tasks` already has `output_globs`:
```rust
TaskDef {
    name: "build".to_string(),
    output_globs: vec!["dist/**", "lib/**", "**/*.d.ts"],
    ...
}
```

Use these to find `.d.ts` files after the build.

---

## Tests (TDD)

### Test 1: ABI fingerprint stored in CacheEntry

```rust
#[test]
fn abi_fingerprint_round_trips_in_entry() {
    let e = CacheEntry {
        abi_fingerprint: Some("abc123".to_string()),
        ..
    };
    let json = serde_json::to_string(&e).unwrap();
    let back: CacheEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.abi_fingerprint, Some("abc123".to_string()));
}
```

### Test 2: WF includes dep_abi_fingerprints

```rust
#[test]
fn wf_changes_when_dep_abi_changes() {
    let tool = PathBuf::from("/usr/bin/env");
    let dir = tempdir().unwrap();
    let inputs_v1 = WeakFpInputs {
        command: "tsc",
        tool_path: &tool,
        package_path: dir.path(),
        declared_input_globs: &[],
        tracked_env: &[],
        dep_abi_fingerprints: &[("core".to_string(), "abi-v1".to_string())],
    };
    let inputs_v2 = WeakFpInputs {
        dep_abi_fingerprints: &[("core".to_string(), "abi-v2".to_string())],
        ..inputs_v1
    };
    let wf1 = compute_weak_fingerprint(&inputs_v1);
    let wf2 = compute_weak_fingerprint(&inputs_v2);
    assert_ne!(wf1, wf2, "changing dep ABI must change WF");
}
```

### Test 3: Early-cutoff integration test

In scheduler tests: build two-package DAG where B depends on A. Run both. Change A's impl without changing A's output types. Run again. Assert B hits cache (WF hit).

---

## Files to modify

| File | Change |
|------|--------|
| `crates/cache/src/entry.rs` | Add `abi_fingerprint: Option<String>` |
| `crates/cache/src/weak_fp.rs` | Add `dep_abi_fingerprints` to WeakFpInputs; include in hash |
| `crates/cache/src/two_phase.rs` | Add `get_pkg_abi_fp`/`set_pkg_abi_fp` |
| `crates/scheduler/src/task.rs` | Add `dep_package_names`, `output_globs` to Task; populate in build_task_list |
| `crates/scheduler/src/runner.rs` | Wire dep_abi_fps into WeakFpInputs; compute+store ABI fp after success |
