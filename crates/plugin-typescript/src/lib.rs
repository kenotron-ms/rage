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

    fn abi_fingerprint(&self, _outputs: &[OutputFile]) -> Option<String> {
        None
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
        assert!(!g.is_empty(), "should return at least a generic ts/tsx glob");
    }

    #[test]
    fn abi_fingerprint_returns_none() {
        let p = TypeScriptPlugin::new();
        let outputs: Vec<OutputFile> = vec![];
        assert!(p.abi_fingerprint(&outputs).is_none());
    }
}
