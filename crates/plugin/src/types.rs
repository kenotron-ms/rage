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
