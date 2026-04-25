# Phase 8 — Ecosystem Plugin Trait + TypeScript Plugin Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Define the `EcosystemPlugin` trait and ship a working `rage-typescript` plugin that declares input globs, a toolchain allowlist, and a `.d.ts`-based ABI fingerprint.

**Architecture:** Two new crates:
1. `plugin` (rlib) — defines the trait + supporting types. Pure trait module; no I/O.
2. `plugin-typescript` (rlib) — first concrete plugin. Implements the trait for TypeScript packages.

Plugins are statically linked into the rage binary in this phase (no dynamic loading). Future phases may add a registry indexed by name.

**Tech Stack:** Rust 2021, blake3 (for ABI fingerprint), serde (for `PluginConfig` round-trips, optional).

**Design reference:** `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 3 — Ecosystem Plugin Contract; Section 5 — Plugin ABI Fingerprint.

---

## Constraints (from COE)

1. The trait **MUST** be the exact shape defined in the design doc Section 3 — `detection_globs`, `infer_tasks`, `toolchain_allowlist`, `declared_input_globs`, `abi_fingerprint`.
2. `declared_input_globs` **MUST** consume a `PluginConfig` (extend/exclude pattern from `rage.json`) — the user does not declare; the plugin does, augmented by config.
3. `abi_fingerprint` **MUST** return `Option<String>` — a plugin without ABI knowledge returns `None`. Correctness then falls to the strong fingerprint alone (per design doc).
4. The TypeScript plugin's ABI fingerprint **MUST** hash all `.d.ts` files in the outputs in deterministic order.
5. No dynamic plugin loading. No plugin registry. Static linking only.

---

## Files Created / Modified

### New crates
- `/Users/ken/workspace/ms/rage/crates/plugin/` — Cargo.toml, src/lib.rs, src/types.rs
- `/Users/ken/workspace/ms/rage/crates/plugin-typescript/` — Cargo.toml, src/lib.rs

### Modified
- `/Users/ken/workspace/ms/rage/Cargo.toml` — workspace members

---

## Task 1: Scaffold the `plugin` crate

**Files:**
- Create: `crates/plugin/Cargo.toml`
- Create: `crates/plugin/src/lib.rs`
- Create: `crates/plugin/src/types.rs`
- Modify: `Cargo.toml` (workspace)

**Step 1: Add to workspace members**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml` `members`:

```
    "crates/plugin",
    "crates/plugin-typescript",
```

**Step 2: Create `crates/plugin/Cargo.toml`**

```toml
[package]
name = "plugin"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
```

**Step 3: Create `crates/plugin/src/types.rs`**

```rust
//! Public data types used by the EcosystemPlugin trait.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A task that can run for a package — name, command template, glob hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDef {
    /// The task script name, e.g. `build`, `typecheck`, `test`.
    pub name: String,
    /// Shell command template the plugin would run.
    /// Variable substitution is not implemented in this phase.
    pub command_template: String,
    /// Globs (relative to package root) the task typically reads.
    pub input_globs: Vec<String>,
    /// Globs (relative to package root) the task writes.
    pub output_globs: Vec<String>,
}

/// A path pattern the toolchain is known to read but is not the package's
/// own input — e.g. `**/node_modules/typescript/**`. Excluded from the
/// "undeclared read" warning report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub path_pattern: String,
    pub reason: String,
}

/// Per-plugin configuration — extend/exclude lists, sourced from `rage.json`'s
/// `plugins_config.<plugin-name>` block.
///
/// Mirrors `pipeline_config::PluginConfig` but is owned by this crate so the
/// `plugin` crate has no dependency on `pipeline-config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub extend_input_globs: Vec<String>,
    pub exclude_input_globs: Vec<String>,
}

/// Reference to an emitted output file — used by `abi_fingerprint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskdef_roundtrips_serde() {
        let t = TaskDef {
            name: "build".to_string(),
            command_template: "tsc".to_string(),
            input_globs: vec!["src/**/*.ts".to_string()],
            output_globs: vec!["dist/**".to_string()],
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: TaskDef = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn plugin_config_default_is_empty() {
        let c = PluginConfig::default();
        assert!(c.extend_input_globs.is_empty());
        assert!(c.exclude_input_globs.is_empty());
    }
}
```

(Note: `Cargo.toml` needs `serde_json = "1"` in `[dev-dependencies]`. Add it now.)

**Step 4: Create `crates/plugin/src/lib.rs`**

```rust
//! Ecosystem plugin contract.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! ecosystem plugins centralize the declaration burden — they tell rage what
//! TypeScript / Rust / Go / Python packages typically read, write, and emit.
//! User-supplied config augments these defaults.

pub mod types;

pub use types::{AllowlistEntry, OutputFile, PluginConfig, TaskDef};

use std::path::Path;

/// Implemented by each ecosystem (TypeScript, Rust, Go, ...).
///
/// A plugin is a value type; the runtime stores `Box<dyn EcosystemPlugin>`.
pub trait EcosystemPlugin: Send + Sync {
    /// Stable plugin id, used to look up `plugins_config.<id>` in `rage.json`.
    fn id(&self) -> &'static str;

    /// Globs (workspace-relative) that signal a package belongs to this plugin.
    /// Example: TypeScript returns `["tsconfig.json", "tsconfig.*.json"]`.
    fn detection_globs(&self) -> Vec<&'static str>;

    /// Given a package root, return the task definitions this plugin offers.
    /// Implementations may inspect manifest files (e.g. `package.json` scripts)
    /// to specialize.
    fn infer_tasks(&self, root: &Path) -> Vec<TaskDef>;

    /// Files the toolchain reads on every invocation but that are not package
    /// inputs (system libs, compiler internals). Used to suppress "undeclared
    /// read" warnings.
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;

    /// Globs feeding the *weak fingerprint* for cache lookup.
    ///
    /// `task_name` is the script being run (`build`, `typecheck`, ...).
    /// `config` carries `extend`/`exclude` overrides from `rage.json`.
    fn declared_input_globs(&self, task_name: &str, config: &PluginConfig) -> Vec<String>;

    /// Optional ABI fingerprint of `outputs`.
    ///
    /// Returns a deterministic hex string identifying the *semantic* shape of
    /// the outputs (e.g. TypeScript hashes `.d.ts` files; Go hashes exported
    /// symbols). Used for downstream early-cutoff: if a package's ABI didn't
    /// change, dependents may skip rebuilds.
    ///
    /// Returns `None` when this plugin doesn't support ABI-level cutoffs.
    fn abi_fingerprint(&self, outputs: &[OutputFile]) -> Option<String>;
}
```

**Step 5: Run, verify**

Run: `cargo test -p plugin`
Expected: pass.

**Step 6: Commit**

```
git add Cargo.toml crates/plugin && git commit -m "feat(plugin): EcosystemPlugin trait + TaskDef/AllowlistEntry/PluginConfig types"
```

---

## Task 2: Scaffold `plugin-typescript` crate

**Files:**
- Create: `crates/plugin-typescript/Cargo.toml`
- Create: `crates/plugin-typescript/src/lib.rs`

**Step 1: Cargo.toml**

```toml
[package]
name = "plugin-typescript"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
plugin = { path = "../plugin" }
blake3 = "1"

[dev-dependencies]
tempfile = "3"
```

**Step 2: Stub `lib.rs`**

```rust
//! TypeScript ecosystem plugin.
//!
//! Detects packages by `tsconfig.json`. Declares `typecheck` and `build`
//! tasks. ABI fingerprint hashes `.d.ts` outputs.

use plugin::{AllowlistEntry, EcosystemPlugin, OutputFile, PluginConfig, TaskDef};
use std::path::Path;

/// The TypeScript plugin.
#[derive(Debug, Default, Clone)]
pub struct TypeScriptPlugin;

impl TypeScriptPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl EcosystemPlugin for TypeScriptPlugin {
    fn id(&self) -> &'static str {
        "rage-typescript"
    }

    fn detection_globs(&self) -> Vec<&'static str> {
        Vec::new()
    }

    fn infer_tasks(&self, _root: &Path) -> Vec<TaskDef> {
        Vec::new()
    }

    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry> {
        Vec::new()
    }

    fn declared_input_globs(&self, _task_name: &str, _config: &PluginConfig) -> Vec<String> {
        Vec::new()
    }

    fn abi_fingerprint(&self, _outputs: &[OutputFile]) -> Option<String> {
        None
    }
}
```

**Step 3: Verify it builds**

Run: `cargo build -p plugin-typescript`
Expected: builds.

**Step 4: Commit**

```
git add Cargo.toml crates/plugin-typescript && git commit -m "feat(plugin-typescript): scaffold crate (all methods return empty)"
```

---

## Task 3: Implement `id()` + `detection_globs()`

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Add the failing test**

Append to `crates/plugin-typescript/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_rage_typescript() {
        assert_eq!(TypeScriptPlugin::new().id(), "rage-typescript");
    }

    #[test]
    fn detection_globs_match_tsconfig() {
        let p = TypeScriptPlugin::new();
        let globs = p.detection_globs();
        assert!(globs.contains(&"tsconfig.json"));
        assert!(globs.iter().any(|g| g.contains("tsconfig.")));
    }
}
```

**Step 2: Run, verify failure**

Run: `cargo test -p plugin-typescript detection_globs_match_tsconfig`
Expected: FAIL.

**Step 3: Implement**

In `lib.rs`:

```rust
    fn detection_globs(&self) -> Vec<&'static str> {
        vec!["tsconfig.json", "tsconfig.*.json"]
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p plugin-typescript`
Expected: pass.

**Step 5: Commit**

```
git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): id + detection_globs"
```

---

## Task 4: Implement `infer_tasks()`

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Add the failing test**

```rust
    #[test]
    fn infer_tasks_returns_typecheck_and_build() {
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_tasks(std::path::Path::new("/anywhere"));
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.name == "typecheck"));
        assert!(tasks.iter().any(|t| t.name == "build"));
    }

    #[test]
    fn typecheck_has_tsc_noemit() {
        let p = TypeScriptPlugin::new();
        let tc = p.infer_tasks(std::path::Path::new("/x"))
            .into_iter()
            .find(|t| t.name == "typecheck")
            .unwrap();
        assert_eq!(tc.command_template, "tsc --noEmit");
        assert!(tc.input_globs.iter().any(|g| g == "src/**/*.ts"));
        assert!(tc.input_globs.iter().any(|g| g == "tsconfig*.json"));
    }

    #[test]
    fn build_has_dist_outputs() {
        let p = TypeScriptPlugin::new();
        let b = p.infer_tasks(std::path::Path::new("/x"))
            .into_iter()
            .find(|t| t.name == "build")
            .unwrap();
        assert_eq!(b.command_template, "tsc");
        assert!(b.output_globs.iter().any(|g| g == "dist/**"));
        assert!(b.output_globs.iter().any(|g| g.contains("d.ts")));
    }
```

**Step 2: Run, verify failure**

Run: `cargo test -p plugin-typescript infer_tasks`
Expected: FAIL.

**Step 3: Implement**

```rust
    fn infer_tasks(&self, _root: &Path) -> Vec<TaskDef> {
        vec![
            TaskDef {
                name: "typecheck".to_string(),
                command_template: "tsc --noEmit".to_string(),
                input_globs: vec![
                    "src/**/*.ts".to_string(),
                    "src/**/*.tsx".to_string(),
                    "tsconfig*.json".to_string(),
                    "package.json".to_string(),
                ],
                output_globs: vec![],
            },
            TaskDef {
                name: "build".to_string(),
                command_template: "tsc".to_string(),
                input_globs: vec![
                    "src/**/*.ts".to_string(),
                    "src/**/*.tsx".to_string(),
                    "tsconfig*.json".to_string(),
                    "package.json".to_string(),
                ],
                output_globs: vec![
                    "dist/**".to_string(),
                    "lib/**".to_string(),
                    "**/*.d.ts".to_string(),
                ],
            },
        ]
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p plugin-typescript`
Expected: pass.

**Step 5: Commit**

```
git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): infer_tasks — typecheck and build"
```

---

## Task 5: Implement `toolchain_allowlist()`

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Add the failing test**

```rust
    #[test]
    fn allowlist_covers_typescript_internals() {
        let p = TypeScriptPlugin::new();
        let allow = p.toolchain_allowlist();
        assert!(allow.iter().any(|e| e.path_pattern.contains("typescript")));
        assert!(allow.iter().all(|e| !e.reason.is_empty()));
    }
```

**Step 2: Run, verify failure**

Expected: FAIL.

**Step 3: Implement**

```rust
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry> {
        vec![
            AllowlistEntry {
                path_pattern: "**/node_modules/typescript/**".to_string(),
                reason: "tsc compiler internals".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/node_modules/.pnpm/typescript@*/**".to_string(),
                reason: "tsc compiler internals (pnpm)".to_string(),
            },
            AllowlistEntry {
                path_pattern: "/usr/lib/**".to_string(),
                reason: "system libraries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "/Library/**".to_string(),
                reason: "macOS frameworks".to_string(),
            },
            AllowlistEntry {
                path_pattern: "/private/var/folders/**".to_string(),
                reason: "macOS temp dirs (V8 / node cache)".to_string(),
            },
        ]
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p plugin-typescript`
Expected: pass.

**Step 5: Commit**

```
git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): toolchain_allowlist for tsc + macOS system libs"
```

---

## Task 6: Implement `declared_input_globs()` with config extend/exclude

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Add the failing tests**

```rust
    #[test]
    fn declared_inputs_for_typecheck_has_src_and_tsconfig() {
        let p = TypeScriptPlugin::new();
        let g = p.declared_input_globs("typecheck", &PluginConfig::default());
        assert!(g.contains(&"src/**/*.ts".to_string()));
        assert!(g.contains(&"tsconfig*.json".to_string()));
        assert!(g.contains(&"package.json".to_string()));
    }

    #[test]
    fn declared_inputs_for_build_includes_test_files() {
        // build globs intentionally include tests so that test-affecting builds
        // recompute. exclude is configurable via PluginConfig.
        let p = TypeScriptPlugin::new();
        let g = p.declared_input_globs("build", &PluginConfig::default());
        assert!(g.iter().any(|s| s.contains("src/")));
    }

    #[test]
    fn declared_inputs_extends_with_user_config() {
        let p = TypeScriptPlugin::new();
        let cfg = PluginConfig {
            extend_input_globs: vec!["../../tsconfig.base.json".to_string()],
            exclude_input_globs: vec![],
        };
        let g = p.declared_input_globs("typecheck", &cfg);
        assert!(g.contains(&"../../tsconfig.base.json".to_string()));
    }

    #[test]
    fn declared_inputs_excludes_per_user_config() {
        let p = TypeScriptPlugin::new();
        let cfg = PluginConfig {
            extend_input_globs: vec![],
            exclude_input_globs: vec!["src/**/*.ts".to_string()],
        };
        let g = p.declared_input_globs("typecheck", &cfg);
        assert!(!g.contains(&"src/**/*.ts".to_string()));
        assert!(g.contains(&"tsconfig*.json".to_string()));
    }

    #[test]
    fn unknown_task_returns_generic_globs() {
        let p = TypeScriptPlugin::new();
        let g = p.declared_input_globs("custom-task", &PluginConfig::default());
        assert!(!g.is_empty(), "should return at least a generic ts/tsx glob");
    }
```

**Step 2: Run, verify failure**

Expected: FAIL.

**Step 3: Implement**

```rust
    fn declared_input_globs(&self, task_name: &str, config: &PluginConfig) -> Vec<String> {
        let mut globs: Vec<String> = match task_name {
            "typecheck" | "build" => vec![
                "src/**/*.ts".to_string(),
                "src/**/*.tsx".to_string(),
                "tsconfig*.json".to_string(),
                "package.json".to_string(),
            ],
            _ => vec![
                "**/*.ts".to_string(),
                "**/*.tsx".to_string(),
                "package.json".to_string(),
            ],
        };
        globs.extend(config.extend_input_globs.iter().cloned());
        globs.retain(|g| !config.exclude_input_globs.contains(g));
        globs
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p plugin-typescript`
Expected: pass.

**Step 5: Commit**

```
git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): declared_input_globs with config extend/exclude"
```

---

## Task 7: Implement `abi_fingerprint()` over `.d.ts` files

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Add the failing tests**

```rust
    use std::fs;
    use tempfile::tempdir;

    fn output_at(dir: &std::path::Path, name: &str, content: &[u8]) -> OutputFile {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        OutputFile { path: p }
    }

    #[test]
    fn abi_returns_none_when_no_dts() {
        let dir = tempdir().unwrap();
        let outs = vec![output_at(dir.path(), "index.js", b"console.log(1)")];
        let p = TypeScriptPlugin::new();
        assert!(
            p.abi_fingerprint(&outs).is_none()
                || p.abi_fingerprint(&outs) == Some(blake3::hash(b"").to_hex().to_string())
        );
    }

    #[test]
    fn abi_returns_hash_when_dts_present() {
        let dir = tempdir().unwrap();
        let outs = vec![
            output_at(dir.path(), "index.js", b"console.log(1)"),
            output_at(dir.path(), "index.d.ts", b"export declare const x: number;"),
        ];
        let p = TypeScriptPlugin::new();
        let h = p.abi_fingerprint(&outs);
        assert!(h.is_some());
        let hex = h.unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn abi_changes_with_dts_content() {
        let dir = tempdir().unwrap();
        let dts = dir.path().join("a.d.ts");
        fs::write(&dts, b"export declare const x: number;").unwrap();
        let outs = vec![OutputFile { path: dts.clone() }];
        let p = TypeScriptPlugin::new();
        let h1 = p.abi_fingerprint(&outs).unwrap();
        fs::write(&dts, b"export declare const x: string;").unwrap();
        let h2 = p.abi_fingerprint(&outs).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn abi_stable_across_path_order() {
        let dir = tempdir().unwrap();
        let a = output_at(dir.path(), "a.d.ts", b"declare const a: 1");
        let b = output_at(dir.path(), "b.d.ts", b"declare const b: 2");
        let p = TypeScriptPlugin::new();
        let h1 = p.abi_fingerprint(&[a.clone(), b.clone()]).unwrap();
        let h2 = p.abi_fingerprint(&[b, a]).unwrap();
        assert_eq!(h1, h2, "abi_fingerprint must sort outputs");
    }

    #[test]
    fn abi_ignores_non_dts() {
        let dir = tempdir().unwrap();
        let dts = output_at(dir.path(), "index.d.ts", b"declare const x: 1");
        let p = TypeScriptPlugin::new();
        let h_only = p.abi_fingerprint(&[dts.clone()]).unwrap();
        // adding a .js file must not affect the fingerprint
        let js = output_at(dir.path(), "index.js", b"x=1");
        let h_with_js = p.abi_fingerprint(&[dts, js]).unwrap();
        assert_eq!(h_only, h_with_js);
    }
```

**Step 2: Run, verify failure**

Expected: FAIL.

**Step 3: Implement**

In `crates/plugin-typescript/src/lib.rs`:

```rust
    fn abi_fingerprint(&self, outputs: &[OutputFile]) -> Option<String> {
        let mut dts_paths: Vec<&std::path::Path> = outputs
            .iter()
            .map(|o| o.path.as_path())
            .filter(|p| {
                let s = p.to_string_lossy();
                s.ends_with(".d.ts") || s.ends_with(".d.cts") || s.ends_with(".d.mts")
            })
            .collect();
        if dts_paths.is_empty() {
            return None;
        }
        dts_paths.sort();
        let mut hasher = blake3::Hasher::new();
        for path in dts_paths {
            // Hash the path so reorders/renames affect the fingerprint.
            hasher.update(path.as_os_str().as_encoded_bytes());
            if let Ok(content) = std::fs::read(path) {
                hasher.update(&content);
            }
        }
        Some(hasher.finalize().to_hex().to_string())
    }
```

**Step 4: Run, verify pass**

Run: `cargo test -p plugin-typescript`
Expected: pass (5 new abi tests + previous tests = 13 total).

**Step 5: Commit**

```
git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): abi_fingerprint hashes .d.ts files in sorted order"
```

---

## Task 8: Verification gate

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

All green required.

---

## Total tasks: 8
