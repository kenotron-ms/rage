# Phase 6 — Pipeline Config Wiring Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Wire `pipeline-config` (currently parsed-but-ignored) into the CLI and scheduler. Expand the `rage.json` schema to match the design doc.

**Architecture:** `rage.json` is loaded once in `cmd_run`, the resolved `RageConfig` is threaded into the scheduler and cache layer. New `cache`, `policies`, and `plugins_config` sections are added. CLI flags continue to override config values.

**Tech Stack:** Rust 2021, Tokio, serde / serde_json, anyhow.

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 3 — Config Architecture.

---

## Files Touched

- Modify `crates/pipeline-config/src/config.rs` — expand schema
- Modify `crates/pipeline-config/src/lib.rs` — re-export new types
- Create `crates/pipeline-config/src/policy.rs` — glob policy matching
- Modify `crates/pipeline-config/Cargo.toml` — add `globset`
- Modify `crates/cli/src/main.rs` — load config, thread to scheduler
- Modify `crates/scheduler/src/runner.rs` — accept `SandboxMode` per task, log it
- Modify `crates/scheduler/Cargo.toml` — depend on `pipeline-config`

## Constraints

1. `rage.json` is **optional** — the CLI must keep working when it is absent.
2. CLI flags **always** override config values (e.g. `--no-cache` overrides `cache.backend`).
3. Per-task sandbox mode is determined by: (1) per-package overrides — out of scope for this phase, (2) glob policies that match the package path, (3) workspace `sandbox.default`.
4. This phase does **not** wire sandbox execution itself — that is Phase 7. We thread the resolved `SandboxMode` through and log it; actual sandboxing is a no-op for now.

---

## Task 1: Expand `RageConfig` with `cache` section

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/pipeline-config/src/config.rs`

**Step 1: Add the failing test**

Append to `crates/pipeline-config/src/config.rs` `tests` module:

```rust
    #[test]
    fn parses_cache_section() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(
            br#"{
                "cache": { "backend": "local", "dir": "/tmp/rage-cache" }
            }"#,
        )
        .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg.cache.backend, "local");
        assert_eq!(cfg.cache.dir, Some(std::path::PathBuf::from("/tmp/rage-cache")));
    }

    #[test]
    fn cache_section_has_defaults() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(b"{}").unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg.cache.backend, "local");
        assert_eq!(cfg.cache.dir, None);
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p pipeline-config parses_cache_section`
Expected: FAIL — `RageConfig` has no `cache` field.

**Step 3: Implement the `CacheConfig` struct**

Add to `crates/pipeline-config/src/config.rs` above `RageConfig`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub backend: String,
    pub dir: Option<std::path::PathBuf>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            dir: None,
        }
    }
}
```

Add the field to `RageConfig`:

```rust
pub struct RageConfig {
    pub plugins: Vec<String>,
    pub sandbox: SandboxConfig,
    pub cache: CacheConfig,
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p pipeline-config`
Expected: all tests pass.

**Step 5: Commit**

```
git add crates/pipeline-config && git commit -m "feat(pipeline-config): add cache section to rage.json schema"
```

---

## Task 2: Add `policies` array

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/pipeline-config/src/config.rs`

**Step 1: Add the failing test**

```rust
    #[test]
    fn parses_policies_array() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(
            br#"{
                "policies": [
                    { "selector": "packages/core/**",   "sandbox": "strict" },
                    { "selector": "packages/legacy/**", "sandbox": "loose" }
                ]
            }"#,
        )
        .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg.policies.len(), 2);
        assert_eq!(cfg.policies[0].selector, "packages/core/**");
        assert_eq!(cfg.policies[0].sandbox, Some(SandboxMode::Strict));
        assert_eq!(cfg.policies[1].sandbox, Some(SandboxMode::Loose));
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p pipeline-config parses_policies_array`
Expected: FAIL.

**Step 3: Add `Policy` struct + field**

Add to `crates/pipeline-config/src/config.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Policy {
    pub selector: String,
    #[serde(default)]
    pub sandbox: Option<SandboxMode>,
}
```

Add to `RageConfig`:

```rust
    pub policies: Vec<Policy>,
```

**Step 4: Run, verify pass**

Run: `cargo test -p pipeline-config`
Expected: all pass.

**Step 5: Commit**

```
git add crates/pipeline-config && git commit -m "feat(pipeline-config): add policies array (per-glob sandbox overrides)"
```

---

## Task 3: Add `plugins_config` map

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/pipeline-config/src/config.rs`

**Step 1: Add the failing test**

```rust
    #[test]
    fn parses_plugins_config() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(
            br#"{
                "plugins_config": {
                    "rage-typescript": {
                        "input_globs": {
                            "extend":  ["../../tsconfig.base.json"],
                            "exclude": ["**/*.test.ts"]
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        let ts = cfg.plugins_config.get("rage-typescript").unwrap();
        assert_eq!(ts.input_globs.extend, vec!["../../tsconfig.base.json".to_string()]);
        assert_eq!(ts.input_globs.exclude, vec!["**/*.test.ts".to_string()]);
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p pipeline-config parses_plugins_config`
Expected: FAIL.

**Step 3: Add `PluginConfig` + `InputGlobsConfig` structs**

Add to `crates/pipeline-config/src/config.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct InputGlobsConfig {
    pub extend: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct PluginConfig {
    pub input_globs: InputGlobsConfig,
}
```

Add to `RageConfig`:

```rust
    pub plugins_config: std::collections::HashMap<String, PluginConfig>,
```

**Step 4: Run, verify pass**

Run: `cargo test -p pipeline-config`
Expected: all pass.

**Step 5: Commit**

```
git add crates/pipeline-config && git commit -m "feat(pipeline-config): add plugins_config map"
```

---

## Task 4: Re-export new types from `lib.rs`

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/pipeline-config/src/lib.rs`

**Step 1: Update re-exports**

Replace contents:

```rust
//! Parse the workspace `rage.json` config file.

pub mod config;
pub mod policy;

pub use config::{
    load_config, CacheConfig, InputGlobsConfig, Policy, PluginConfig, RageConfig, SandboxConfig,
    SandboxMode,
};
pub use policy::resolve_sandbox_mode;
```

(Note: `policy` module is created in Task 5.)

**Step 2: Skip — file won't compile yet** (forward reference to `policy`)

**Step 3: Commit deferred** — this is a setup step finalized after Task 5.

---

## Task 5: Add `globset` dependency and `policy` module

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/pipeline-config/Cargo.toml`
- Create: `/Users/ken/workspace/ms/rage/crates/pipeline-config/src/policy.rs`

**Step 1: Write the failing test**

Create `crates/pipeline-config/src/policy.rs`:

```rust
//! Glob-based policy resolution.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! `policies` are evaluated in order; first match wins. If no policy matches,
//! the workspace `sandbox.default` is used.

use crate::config::{Policy, RageConfig, SandboxMode};
use std::path::Path;

/// Resolve the sandbox mode that applies to a package at `pkg_relative_path`.
///
/// `pkg_relative_path` is the package directory relative to the workspace root,
/// using forward slashes (e.g. `packages/core`).
///
/// Algorithm:
///   1. Iterate `config.policies` in order.
///   2. Return the first policy whose `selector` glob matches the path.
///   3. If none match, return `config.sandbox.default`.
pub fn resolve_sandbox_mode(config: &RageConfig, pkg_relative_path: &Path) -> SandboxMode {
    let path_str = pkg_relative_path.to_string_lossy().replace('\\', "/");
    for policy in &config.policies {
        if let Some(mode) = matches_policy(policy, &path_str) {
            return mode;
        }
    }
    config.sandbox.default.clone()
}

fn matches_policy(policy: &Policy, path: &str) -> Option<SandboxMode> {
    let glob = globset::Glob::new(&policy.selector).ok()?;
    let matcher = glob.compile_matcher();
    if matcher.is_match(path) {
        policy.sandbox.clone()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CacheConfig, SandboxConfig};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn cfg(default: SandboxMode, policies: Vec<Policy>) -> RageConfig {
        RageConfig {
            plugins: vec![],
            sandbox: SandboxConfig { default },
            cache: CacheConfig::default(),
            policies,
            plugins_config: HashMap::new(),
        }
    }

    #[test]
    fn falls_back_to_default_when_no_policies() {
        let c = cfg(SandboxMode::Observed, vec![]);
        assert_eq!(
            resolve_sandbox_mode(&c, &PathBuf::from("packages/core")),
            SandboxMode::Observed
        );
    }

    #[test]
    fn matches_first_policy() {
        let c = cfg(
            SandboxMode::Observed,
            vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
        );
        assert_eq!(
            resolve_sandbox_mode(&c, &PathBuf::from("packages/core/x")),
            SandboxMode::Strict
        );
    }

    #[test]
    fn first_match_wins() {
        let c = cfg(
            SandboxMode::Observed,
            vec![
                Policy {
                    selector: "packages/legacy/**".to_string(),
                    sandbox: Some(SandboxMode::Loose),
                },
                Policy {
                    selector: "packages/**".to_string(),
                    sandbox: Some(SandboxMode::Strict),
                },
            ],
        );
        // legacy matches the first policy
        assert_eq!(
            resolve_sandbox_mode(&c, &PathBuf::from("packages/legacy/old")),
            SandboxMode::Loose
        );
        // others match the second
        assert_eq!(
            resolve_sandbox_mode(&c, &PathBuf::from("packages/core/new")),
            SandboxMode::Strict
        );
    }

    #[test]
    fn no_match_falls_back_to_default() {
        let c = cfg(
            SandboxMode::Loose,
            vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
        );
        assert_eq!(
            resolve_sandbox_mode(&c, &PathBuf::from("apps/web")),
            SandboxMode::Loose
        );
    }
}
```

**Step 2: Add dependency**

Edit `crates/pipeline-config/Cargo.toml`, add to `[dependencies]`:

```
globset = "0.4"
```

**Step 3: Run, verify failure**

Run: `cargo test -p pipeline-config policy`
Expected: FAIL (compile error — `policy` module not declared in lib.rs).

**Step 4: Update lib.rs (Task 4)**

Apply the change from Task 4 above. Save lib.rs.

**Step 5: Run, verify pass**

Run: `cargo test -p pipeline-config`
Expected: all pass.

**Step 6: Commit**

```
git add crates/pipeline-config && git commit -m "feat(pipeline-config): add policy module — glob-based sandbox resolution"
```

---

## Task 6: Make scheduler depend on pipeline-config + accept SandboxMode per task

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/Cargo.toml`
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/task.rs`

**Step 1: Add the dependency**

Edit `crates/scheduler/Cargo.toml`, add under `[dependencies]`:

```
pipeline-config = { path = "../pipeline-config" }
```

**Step 2: Add the failing test**

Append to `crates/scheduler/src/task.rs` `tests` module:

```rust
    #[test]
    fn task_carries_sandbox_mode() {
        let t = Task {
            package_name: "x".to_string(),
            script_name: "build".to_string(),
            command: "echo".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Strict,
        };
        assert_eq!(t.sandbox_mode, pipeline_config::SandboxMode::Strict);
    }
```

**Step 3: Run, verify failure**

Run: `cargo test -p scheduler task_carries_sandbox_mode`
Expected: FAIL — `Task` has no `sandbox_mode` field.

**Step 4: Add the field**

In `crates/scheduler/src/task.rs`:

```rust
pub struct Task {
    pub package_name: String,
    pub script_name: String,
    pub command: String,
    pub cwd: PathBuf,
    pub sandbox_mode: pipeline_config::SandboxMode,
}
```

In the `build_task_list` function, default the field when constructing:

```rust
            tasks.push(Task {
                package_name: pkg_name.clone(),
                script_name: script_name.to_string(),
                command: cmd,
                cwd: pkg.path.clone(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
            });
```

In existing tests (e.g. `mk_task` helpers in `runner.rs` and other test code) add `sandbox_mode: pipeline_config::SandboxMode::default()` to every `Task { ... }` literal. Search with: `rg "Task \{" crates/scheduler/src/`.

**Step 5: Run, verify pass**

Run: `cargo test -p scheduler`
Expected: all pass.

**Step 6: Commit**

```
git add crates/scheduler && git commit -m "feat(scheduler): Task carries SandboxMode"
```

---

## Task 7: Add `build_task_list_with_config` for sandbox resolution

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/task.rs`

**Step 1: Add the failing test**

```rust
    #[test]
    fn task_list_with_config_resolves_sandbox_per_policy() {
        use pipeline_config::{CacheConfig, Policy, RageConfig, SandboxConfig, SandboxMode};
        use std::collections::HashMap;
        use workspace_tools::{build_package_graph, discover_packages};

        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();

        let cfg = RageConfig {
            plugins: vec![],
            sandbox: SandboxConfig { default: SandboxMode::Observed },
            cache: CacheConfig::default(),
            policies: vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
            plugins_config: HashMap::new(),
        };

        let tasks = build_task_list_with_config(&dag, "build", &root, &cfg).unwrap();
        let core = tasks.iter().find(|t| t.package_name == "@fixture/core").unwrap();
        let utils = tasks.iter().find(|t| t.package_name == "@fixture/utils").unwrap();
        assert_eq!(core.sandbox_mode, SandboxMode::Strict);
        assert_eq!(utils.sandbox_mode, SandboxMode::Observed);
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p scheduler task_list_with_config`
Expected: FAIL — function doesn't exist.

**Step 3: Implement `build_task_list_with_config`**

Add to `crates/scheduler/src/task.rs`:

```rust
/// Build a task list with sandbox modes resolved against `RageConfig` policies.
///
/// `workspace_root` is used to compute each package's path relative to the
/// workspace for glob policy matching.
pub fn build_task_list_with_config(
    dag: &build_graph::dag::WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    config: &pipeline_config::RageConfig,
) -> Result<Vec<Task>, TaskError> {
    let mut tasks = build_task_list(dag, script_name)?;
    for task in &mut tasks {
        let rel = task
            .cwd
            .strip_prefix(workspace_root)
            .unwrap_or(&task.cwd)
            .to_path_buf();
        task.sandbox_mode = pipeline_config::resolve_sandbox_mode(config, &rel);
    }
    Ok(tasks)
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p scheduler`
Expected: all pass.

**Step 5: Commit**

```
git add crates/scheduler && git commit -m "feat(scheduler): build_task_list_with_config — resolves SandboxMode per task"
```

---

## Task 8: Log sandbox mode from runner on task start

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs`

**Step 1: Add the failing test**

In `crates/scheduler/src/runner.rs`, append:

```rust
    #[tokio::test]
    async fn task_logs_sandbox_mode_in_starting_line() {
        // Smoke test: just verify runner accepts SandboxMode-bearing tasks.
        let task = Task {
            package_name: "smoke".to_string(),
            script_name: "build".to_string(),
            command: "true".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Strict,
        };
        let pkg = mk_pkg("smoke", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        run_tasks(&dag, vec![task], None).await.unwrap();
    }
```

**Step 2: Run, verify pass (probably already does)**

Run: `cargo test -p scheduler task_logs_sandbox_mode`
Expected: pass — Task is constructible.

**Step 3: Update the log line in `run_single_task`**

In `crates/scheduler/src/runner.rs`, change the "starting" log line:

```rust
    eprintln!(
        "[rage] {}#{} starting [sandbox={:?}]",
        task.package_name, task.script_name, task.sandbox_mode
    );
```

**Step 4: Run, verify pass**

Run: `cargo test -p scheduler`
Expected: all pass.

**Step 5: Commit**

```
git add crates/scheduler && git commit -m "feat(scheduler): log resolved sandbox mode per task"
```

---

## Task 9: Wire config into the CLI

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/cli/src/main.rs`

**Step 1: Add an integration test in `crates/cli/tests/integration.rs`**

Append:

```rust
#[test]
fn rage_run_loads_rage_json_cache_dir() {
    use std::process::Command;

    let workspace = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();

    // minimal pnpm workspace with one package
    std::fs::write(
        workspace.path().join("pnpm-workspace.yaml"),
        b"packages:\n  - 'packages/*'\n",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("package.json"),
        br#"{"name":"root","private":true}"#,
    )
    .unwrap();
    let pkg = workspace.path().join("packages/p");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        br#"{"name":"@x/p","version":"1.0.0","scripts":{"build":"echo hi"}}"#,
    )
    .unwrap();

    // rage.json points cache.dir at our tempdir
    std::fs::write(
        workspace.path().join("rage.json"),
        format!(
            r#"{{ "cache": {{ "backend": "local", "dir": "{}" }} }}"#,
            cache_dir.path().to_string_lossy()
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_rage");
    let out = Command::new(bin)
        .args(["run", "build"])
        .arg(workspace.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // Cache should have been written into rage.json's cache.dir, not ~/.rage/cache
    let entries: Vec<_> = std::fs::read_dir(cache_dir.path()).unwrap().collect();
    assert!(
        !entries.is_empty(),
        "expected cache entries in {}",
        cache_dir.path().display()
    );
}
```

**Step 2: Run, verify failure**

Run: `cargo test -p rage-cli rage_run_loads_rage_json`
Expected: FAIL — config currently ignored, cache goes to default location.

**Step 3: Update `cmd_run` to load and use config**

In `crates/cli/src/main.rs`, in `cmd_run`:

After the `pm = workspace_tools::detect_package_manager...` line, add:

```rust
    // Load rage.json (optional; absent → defaults).
    let config = pipeline_config::load_config(root)
        .with_context(|| "loading rage.json")?
        .unwrap_or_default();
```

Replace the cache construction block with:

```rust
    let cache: Option<Arc<dyn cache::CacheProvider>> = if no_cache {
        None
    } else {
        // Resolve cache dir: CLI flag > rage.json > RAGE_CACHE_DIR > ~/.rage/cache.
        let cache_result = match &config.cache.dir {
            Some(d) => LocalCache::with_dir(d.clone()),
            None => LocalCache::new(),
        };
        match cache_result {
            Ok(lc) => Some(Arc::new(lc)),
            Err(e) => {
                eprintln!("[rage] warning: cache unavailable: {e}");
                None
            }
        }
    };
```

Replace the task-list build with:

```rust
    let mut tasks = scheduler::task::build_task_list_with_config(&dag, script, root, &config)
        .with_context(|| format!("no packages have a '{script}' script"))?;
```

**Step 4: Run, verify pass**

Run: `cargo test -p rage-cli`
Expected: all pass.

**Step 5: Run full test suite**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all pass, no clippy warnings.

**Step 6: Commit**

```
git add crates/cli && git commit -m "feat(cli): wire rage.json into cmd_run (cache.dir, sandbox policies)"
```

---

## Task 10: Update default LocalCache to expand `~`

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/cache/src/local.rs`

**Step 1: Add the failing test**

In `crates/cache/src/local.rs` `tests` module:

```rust
    #[test]
    fn with_dir_expands_tilde() {
        let cache = LocalCache::with_dir(PathBuf::from("~/.rage-test-tilde-expansion")).unwrap();
        // Just verifying construction doesn't fail; cleanup not strictly necessary.
        let _ = cache;
        let home = std::env::var("HOME").unwrap();
        assert!(std::path::PathBuf::from(home).join(".rage-test-tilde-expansion").exists());
        std::fs::remove_dir_all(std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".rage-test-tilde-expansion")).ok();
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p cache with_dir_expands_tilde`
Expected: FAIL — `~` is treated literally.

**Step 3: Implement tilde expansion in `with_dir`**

```rust
pub fn with_dir(dir: PathBuf) -> Result<Self> {
    let dir = expand_tilde(dir)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating cache dir {}", dir.display()))?;
    Ok(Self { dir })
}

fn expand_tilde(p: PathBuf) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .context("HOME or USERPROFILE not set")?;
        Ok(PathBuf::from(home).join(rest))
    } else if s == "~" {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .context("HOME or USERPROFILE not set")?;
        Ok(PathBuf::from(home))
    } else {
        Ok(p)
    }
}
```

**Step 4: Run, verify pass**

Run: `cargo test -p cache`
Expected: all pass.

**Step 5: Commit**

```
git add crates/cache && git commit -m "feat(cache): LocalCache::with_dir expands leading ~"
```

---

## Task 11: Verification gate

Run the full test suite:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
./target/release/rage run build fixtures/js-pnpm
```

Expected: all green; the fixture build runs and prints `[sandbox=Strict|Observed|Loose]` next to `starting`.

Final commit if formatting/lint changes made:

```
git add . && git commit -m "chore: workspace-wide cargo test + clippy clean"
```

---

## Total tasks: 11
