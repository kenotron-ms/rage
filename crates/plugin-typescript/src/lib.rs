//! TypeScript ecosystem plugin.
//!
//! Detects packages by `tsconfig.json`. Declares `typecheck` and `build`
//! tasks. ABI fingerprint hashes `.d.ts` outputs.

pub mod lockfile;
pub mod pathset_extractor;
pub mod postinstall;

use plugin::{AllowlistEntry, EcosystemPlugin, OutputFile, PluginConfig, TaskDef};
use std::path::Path;

/// Read the Node.js version declared by the workspace.
///
/// Mirrors `scheduler::node_path::resolve_node_version`. Duplicated here so
/// `plugin-typescript` does not depend on `scheduler`.
fn read_node_version(workspace_root: &Path) -> Option<String> {
    // 1. .node-version
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".node-version")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    // 2. .nvmrc
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".nvmrc")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    // 3. .tool-versions (asdf/mise)
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".tool-versions")) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let tool = parts.next().unwrap_or("");
            if tool == "nodejs" || tool == "node" {
                if let Some(v) = parts.next() {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Convert a `rage.json`-parsed [`pipeline_config::PluginConfig`] into the flat
/// [`PluginConfig`] consumed by [`EcosystemPlugin::declared_input_globs`].
///
/// The two config types are kept separate so the [`plugin`] crate has zero dependency
/// on [`pipeline_config`]. A blanket `From` impl would violate the Rust orphan rule
/// (neither type is local to this crate), so an explicit bridge function is used instead.
/// Phase 9 scheduler code should call this when passing `rage.json` config to the plugin.
pub fn plugin_config_from_pipeline(c: pipeline_config::PluginConfig) -> PluginConfig {
    PluginConfig {
        extend_input_globs: c.input_globs.extend,
        exclude_input_globs: c.input_globs.exclude,
    }
}

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
        vec!["tsconfig.json", "tsconfig.*.json"]
    }

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
            AllowlistEntry {
                path_pattern: "**/node_modules/.bin/**".to_string(),
                reason: "locally-installed tool binaries (tsc, eslint, jest, etc.)".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.local/share/fnm/**".to_string(),
                reason: "fnm-managed Node.js binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.nvm/**".to_string(),
                reason: "nvm-managed Node.js binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.asdf/**".to_string(),
                reason: "asdf-managed runtime binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.local/share/mise/**".to_string(),
                reason: "mise-managed runtime binaries".to_string(),
            },
        ]
    }

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

    fn infer_root_tasks(&self, workspace_root: &Path) -> Vec<plugin::RootTask> {
        // Detect the JS package manager from lockfile presence.
        // Priority: pnpm > yarn > npm. Returning at most one root task —
        // the "install" step for the detected manager.

        // Build env_hash_inputs: fold in the resolved Node version so that
        // changing .node-version invalidates the workspace#install cache key.
        let mut env_hash_inputs: Vec<(String, String)> = Vec::new();
        if let Some(v) = read_node_version(workspace_root) {
            env_hash_inputs.push(("NODE_VERSION".to_string(), v));
        }

        let pnpm_lock = workspace_root.join("pnpm-lock.yaml");
        if pnpm_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "pnpm install".to_string(),
                input_paths: vec![pnpm_lock],
                env_hash_inputs: env_hash_inputs.clone(),
            }];
        }

        let yarn_lock = workspace_root.join("yarn.lock");
        if yarn_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "yarn install".to_string(),
                input_paths: vec![yarn_lock],
                env_hash_inputs: env_hash_inputs.clone(),
            }];
        }

        let npm_lock = workspace_root.join("package-lock.json");
        if npm_lock.is_file() {
            return vec![plugin::RootTask {
                name: "install".to_string(),
                command: "npm install".to_string(),
                input_paths: vec![npm_lock],
                env_hash_inputs,
            }];
        }

        // No lockfile found — no install task. (A future heuristic could
        // fall back to package.json#packageManager; that's out of scope.)
        Vec::new()
    }

    fn verify_install_effects(&self, workspace_root: &std::path::Path) -> bool {
        // node_modules/ must exist and be non-empty.
        // Bin-link creation is now handled by `bin_links::create_bin_links` after
        // every CAS restore, so we no longer need to verify .bin/ here.
        let nm = workspace_root.join("node_modules");
        nm.is_dir()
            && std::fs::read_dir(&nm)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
    }

    fn parse_lockfile(&self, workspace_root: &Path) -> Option<Vec<plugin::LockfilePackage>> {
        use crate::lockfile::*;

        // Detection priority: pnpm → yarn (berry/classic) → npm
        if let Ok(content) = std::fs::read_to_string(workspace_root.join("pnpm-lock.yaml")) {
            return Some(parse_pnpm_lockfile(&content));
        }
        if let Ok(content) = std::fs::read_to_string(workspace_root.join("yarn.lock")) {
            // Detect yarn version from __metadata presence
            if content.contains("__metadata:") {
                return Some(parse_yarn_berry_lockfile(&content));
            }
            return Some(parse_yarn_classic_lockfile(&content));
        }
        if let Ok(content) = std::fs::read_to_string(workspace_root.join("package-lock.json")) {
            return Some(parse_npm_lockfile(&content));
        }
        None
    }

    fn local_pm_cache(&self, workspace_root: &Path) -> Option<std::path::PathBuf> {
        // Yarn berry: uses global cache at ~/.yarn/berry/cache/ by default
        // Can be overridden by YARN_CACHE_FOLDER or cacheFolder in .yarnrc.yml
        if workspace_root.join("yarn.lock").exists() {
            // Check YARN_CACHE_FOLDER env var first
            if let Ok(cache) = std::env::var("YARN_CACHE_FOLDER") {
                let p = std::path::Path::new(&cache);
                if p.is_dir() {
                    return Some(p.to_path_buf());
                }
            }
            // Check workspace .yarn/cache (used for zero-installs or berry with local cache)
            let ws_cache = workspace_root.join(".yarn/cache");
            if ws_cache.is_dir() {
                return Some(ws_cache);
            }
            // Global yarn berry cache
            let home = std::env::var("HOME").unwrap_or_default();
            let global_cache = std::path::Path::new(&home).join(".yarn/berry/cache");
            if global_cache.is_dir() {
                return Some(global_cache);
            }
            // Yarn classic global cache
            let classic_cache = std::path::Path::new(&home).join(".yarn/cache");
            if classic_cache.is_dir() {
                return Some(classic_cache);
            }
        }

        // pnpm
        if workspace_root.join("pnpm-lock.yaml").exists() {
            let home = std::env::var("HOME").unwrap_or_default();
            for path in &[
                format!("{}/.local/share/pnpm/store/v3/files", home),
                format!("{}/Library/pnpm/store/v3/files", home),
            ] {
                let p = std::path::Path::new(path);
                if p.is_dir() {
                    return Some(p.to_path_buf());
                }
            }
        }

        // npm
        if workspace_root.join("package-lock.json").exists() {
            let home = std::env::var("HOME").unwrap_or_default();
            let npm_cache = std::path::Path::new(&home).join(".npm/_cacache");
            if npm_cache.is_dir() {
                return Some(npm_cache);
            }
        }

        None
    }

    fn restore_from_cas(
        &self,
        packages: &[plugin::LockfilePackage],
        workspace_root: &Path,
        store: &dyn plugin::ArtifactStoreRef,
    ) -> Result<(), anyhow::Error> {
        use crate::lockfile::{compute_cas_key, extract_yarn_zip_to_workspace};

        let mut restored = 0usize;
        let mut failed = 0usize;

        for pkg in packages {
            let cas_key = compute_cas_key(&pkg.integrity);
            match store.get_bytes(&cas_key)? {
                None => {
                    // CAS miss for this package — skip (should not happen if pre-flight passed)
                    failed += 1;
                }
                Some(zip_bytes) => {
                    // Extract yarn berry zip into workspace_root/
                    // The zip contains entries like node_modules/{name}/...
                    if let Err(e) = extract_yarn_zip_to_workspace(&zip_bytes, workspace_root) {
                        eprintln!(
                            "[rage] artifact restore: failed to extract {} — {e}",
                            pkg.name
                        );
                        failed += 1;
                    } else {
                        restored += 1;
                    }
                }
            }
        }

        if failed > 0 {
            Err(anyhow::anyhow!(
                "restore_from_cas: {failed} packages failed, {restored} succeeded"
            ))
        } else {
            eprintln!("[rage] artifact restore: {restored} packages extracted from CAS");
            Ok(())
        }
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin::{EcosystemPlugin, OutputFile, PluginConfig};

    #[test]
    fn typescript_plugin_id_is_rage_typescript() {
        let p = TypeScriptPlugin::new();
        assert_eq!(p.id(), "rage-typescript");
    }

    #[test]
    fn detection_globs_returns_tsconfig_patterns() {
        let p = TypeScriptPlugin::new();
        let globs = p.detection_globs();
        assert_eq!(globs, vec!["tsconfig.json", "tsconfig.*.json"]);
    }

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
        let tc = p
            .infer_tasks(std::path::Path::new("/x"))
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
        let b = p
            .infer_tasks(std::path::Path::new("/x"))
            .into_iter()
            .find(|t| t.name == "build")
            .unwrap();
        assert_eq!(b.command_template, "tsc");
        assert!(b.output_globs.iter().any(|g| g == "dist/**"));
        assert!(b.output_globs.iter().any(|g| g.contains("d.ts")));
    }

    #[test]
    fn allowlist_covers_typescript_internals() {
        let p = TypeScriptPlugin::new();
        let allow = p.toolchain_allowlist();
        assert!(allow.iter().any(|e| e.path_pattern.contains("typescript")));
        assert!(allow.iter().all(|e| !e.reason.is_empty()));
    }

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
        assert!(
            !g.is_empty(),
            "should return at least a generic ts/tsx glob"
        );
    }

    #[test]
    fn abi_fingerprint_returns_none() {
        let p = TypeScriptPlugin::new();
        let outputs: Vec<OutputFile> = vec![];
        assert!(p.abi_fingerprint(&outputs).is_none());
    }

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
        assert!(p.abi_fingerprint(&outs).is_none());
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
        let h_only = p.abi_fingerprint(std::slice::from_ref(&dts)).unwrap();
        // adding a .js file must not affect the fingerprint
        let js = output_at(dir.path(), "index.js", b"x=1");
        let h_with_js = p.abi_fingerprint(&[dts, js]).unwrap();
        assert_eq!(h_only, h_with_js);
    }

    #[test]
    fn from_pipeline_config_maps_extend_and_exclude() {
        use pipeline_config::{InputGlobsConfig, PluginConfig as PipelinePluginConfig};
        let pc = PipelinePluginConfig {
            input_globs: InputGlobsConfig {
                extend: vec!["../../tsconfig.base.json".to_string()],
                exclude: vec!["**/*.test.ts".to_string()],
            },
        };
        let tc = plugin_config_from_pipeline(pc);
        assert_eq!(tc.extend_input_globs, vec!["../../tsconfig.base.json"]);
        assert_eq!(tc.exclude_input_globs, vec!["**/*.test.ts"]);
    }

    #[test]
    fn from_pipeline_config_empty_is_default() {
        use pipeline_config::PluginConfig as PipelinePluginConfig;
        let pc = PipelinePluginConfig::default();
        let tc = plugin_config_from_pipeline(pc);
        assert!(tc.extend_input_globs.is_empty());
        assert!(tc.exclude_input_globs.is_empty());
    }

    // ── infer_root_tasks tests ────────────────────────────────────────────────

    #[test]
    fn infer_root_tasks_detects_pnpm_lockfile() {
        use plugin::RootTask;
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1, "exactly one install task for pnpm");
        let t = &tasks[0];
        assert_eq!(t.name, "install");
        assert_eq!(t.command, "pnpm install");
        assert_eq!(t.input_paths, vec![dir.path().join("pnpm-lock.yaml")]);
        // Confirm the type round-trips
        let _: RootTask = t.clone();
    }

    #[test]
    fn infer_root_tasks_detects_yarn_lockfile() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"# yarn lockfile v1\n").unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "install");
        assert_eq!(tasks[0].command, "yarn install");
        assert_eq!(tasks[0].input_paths, vec![dir.path().join("yarn.lock")]);
    }

    #[test]
    fn infer_root_tasks_detects_npm_package_lock() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package-lock.json"),
            br#"{"lockfileVersion":3,"requires":true,"packages":{}}"#,
        )
        .unwrap();
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "install");
        assert_eq!(tasks[0].command, "npm install");
        assert_eq!(
            tasks[0].input_paths,
            vec![dir.path().join("package-lock.json")]
        );
    }

    #[test]
    fn infer_root_tasks_returns_empty_when_no_lockfile() {
        let dir = tempdir().unwrap();
        // Empty workspace — no lockfile of any kind.
        let p = TypeScriptPlugin::new();
        let tasks = p.infer_root_tasks(dir.path());
        assert!(tasks.is_empty(), "no lockfile must yield no root tasks");
    }

    #[test]
    fn infer_root_tasks_prefers_pnpm_over_yarn_over_npm() {
        // If multiple lockfiles are present (rare, but possible during a migration),
        // pnpm wins, then yarn, then npm.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), b"v: 6\n").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"v1\n").unwrap();
        std::fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "pnpm install");
    }

    // ── env_hash_inputs / NODE_VERSION tests ─────────────────────────────────

    #[test]
    fn infer_root_tasks_includes_node_version_when_dot_node_version_exists() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].env_hash_inputs,
            vec![("NODE_VERSION".to_string(), "18.20.4".to_string())]
        );
    }

    #[test]
    fn infer_root_tasks_omits_env_hash_when_no_version_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].env_hash_inputs.is_empty());
    }

    #[test]
    fn allowlist_includes_version_manager_dirs() {
        let allow = TypeScriptPlugin::new().toolchain_allowlist();
        let patterns: Vec<&str> = allow.iter().map(|e| e.path_pattern.as_str()).collect();
        assert!(patterns.iter().any(|p| p.contains("fnm")));
        assert!(patterns.iter().any(|p| p.contains(".nvm")));
        assert!(patterns.iter().any(|p| p.contains(".asdf")));
        assert!(patterns.iter().any(|p| p.contains("mise")));
    }

    #[test]
    fn verify_install_effects_true_when_node_modules_non_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/ms")).unwrap();
        std::fs::write(dir.path().join("node_modules/ms/index.js"), b"x").unwrap();
        let p = TypeScriptPlugin::new();
        // Passes even without .bin/ — bin-link creation is handled by bin_links::create_bin_links.
        assert!(p.verify_install_effects(dir.path()));
    }

    #[test]
    fn verify_install_effects_false_when_node_modules_missing() {
        let dir = tempdir().unwrap();
        let p = TypeScriptPlugin::new();
        assert!(!p.verify_install_effects(dir.path()));
    }

    #[test]
    fn verify_install_effects_false_when_node_modules_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        let p = TypeScriptPlugin::new();
        assert!(!p.verify_install_effects(dir.path()));
    }

    /// After CAS restore, packages are present but .bin/ symlinks are created by
    /// bin_links::create_bin_links (not by the package manager). verify_install_effects
    /// no longer checks .bin/ — it only checks node_modules/ is non-empty.
    #[test]
    fn verify_install_effects_true_when_bin_dir_missing_but_packages_present() {
        let dir = tempdir().unwrap();
        // Simulate a CAS restore: packages present, .bin/ not yet created.
        std::fs::create_dir_all(dir.path().join("node_modules/ms")).unwrap();
        std::fs::write(dir.path().join("node_modules/ms/index.js"), b"x").unwrap();
        // No node_modules/.bin/ — bin_links::create_bin_links will create it later.
        let p = TypeScriptPlugin::new();
        assert!(
            p.verify_install_effects(dir.path()),
            "must return true when packages are present, even without .bin/              (bin-links are created separately by bin_links::create_bin_links)"
        );
    }

    #[test]
    fn verify_install_effects_true_when_bin_dir_empty_but_packages_present() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/ms")).unwrap();
        std::fs::write(dir.path().join("node_modules/ms/index.js"), b"x").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        // .bin/ exists but is empty — still OK, bin_links::create_bin_links populates it.
        let p = TypeScriptPlugin::new();
        assert!(p.verify_install_effects(dir.path()));
    }
}
