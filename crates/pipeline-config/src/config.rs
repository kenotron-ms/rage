//! `rage.json` schema and loader.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    #[default]
    Strict,
    Observed,
    Loose,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct SandboxConfig {
    pub default: SandboxMode,
}

/// Remote cache backend configuration.
///
/// The backend variant determines which fields are used. Credentials are NEVER
/// stored here — use environment variables or the platform credential chain.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RemoteBackend {
    #[default]
    Local,
    S3,
    Azure,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Which storage backend to use. Default: "local".
    /// Valid values: "local", "s3", "azure"
    pub backend: String,
    /// Override the local cache directory. Default: .
    pub dir: Option<std::path::PathBuf>,
    // ── S3 config (used when backend = "s3") ────────────────────────────────
    /// S3 bucket name. Credentials from AWS standard credential chain.
    pub bucket: Option<String>,
    /// AWS region (e.g. "us-west-2"). Defaults to  env var.
    pub region: Option<String>,
    /// Key prefix in the bucket (e.g. "rage-cache/"). Default: "".
    #[serde(default)]
    pub s3_prefix: String,
    // ── Azure config (used when backend = "azure") ─────────────────────────
    /// Azure Blob container name.
    pub container: Option<String>,
    /// Azure storage account name. Credentials from env vars / managed identity.
    pub account: Option<String>,
    /// Blob name prefix. Default: "".
    #[serde(default)]
    pub azure_prefix: String,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            dir: None,
            bucket: None,
            region: None,
            s3_prefix: String::new(),
            container: None,
            account: None,
            azure_prefix: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Policy {
    pub selector: String,
    #[serde(default)]
    pub sandbox: Option<SandboxMode>,
}

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

/// Per-script pipeline configuration, mirroring lage's `pipeline` key.
///
/// Currently only supports `skip_packages` — a list of package names that
/// should be silently excluded when building the task list for this script.
/// Useful for packages whose `build` script is a guard/redirect rather than
/// a real build step (e.g. buildless packages that ship plain JS from `src/`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct PipelineTaskConfig {
    /// Package names to exclude from this script's task list.
    /// Supports exact package names only (e.g. `"@scope/pkg"`).
    pub skip_packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct RageConfig {
    pub plugins: Vec<String>,
    pub sandbox: SandboxConfig,
    pub cache: CacheConfig,
    pub policies: Vec<Policy>,
    pub plugins_config: std::collections::HashMap<String, PluginConfig>,
    /// Per-script pipeline settings (skip_packages, etc.).
    /// Keys are script names (e.g. `"build"`, `"test"`).
    pub pipeline: std::collections::HashMap<String, PipelineTaskConfig>,
    /// Maximum number of package tasks that may execute their subprocess
    /// concurrently.  `None` (the default) means "one slot per logical CPU"
    /// as reported by `std::thread::available_parallelism`.
    ///
    /// Lower this when your tasks already parallelize internally (e.g. jest
    /// with `--maxWorkers`) so they do not overwhelm the machine.
    ///
    /// Example in rage.json:
    /// ```json
    /// { "maxConcurrency": 4 }
    /// ```
    #[serde(rename = "maxConcurrency")]
    pub max_concurrency: Option<usize>,
}

/// Load `rage.json` from the workspace root. Returns `None` if absent.
pub fn load_config(workspace_root: &Path) -> Result<Option<RageConfig>> {
    let path = workspace_root.join("rage.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: RageConfig =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "rage-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            n,
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn returns_none_when_no_rage_json() {
        let d = tmpdir();
        assert!(load_config(&d).unwrap().is_none());
    }

    #[test]
    fn parses_minimal_rage_json() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(b"{}").unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg, RageConfig::default());
    }

    #[test]
    fn parses_full_rage_json() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(
            br#"{
            "plugins": ["rage-typescript", "rage-rust"],
            "sandbox": { "default": "observed" }
        }"#,
        )
        .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg.plugins, vec!["rage-typescript", "rage-rust"]);
        assert_eq!(cfg.sandbox.default, SandboxMode::Observed);
    }

    #[test]
    fn malformed_rage_json_errors() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(b"{ not json").unwrap();
        assert!(load_config(&d).is_err());
    }

    #[test]
    fn parses_cache_section() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(br#"{"cache": {"backend": "local", "dir": "/tmp/rage-cache"}}"#)
            .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert_eq!(cfg.cache.backend, "local");
        assert_eq!(
            cfg.cache.dir,
            Some(std::path::PathBuf::from("/tmp/rage-cache"))
        );
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
        assert_eq!(ts.input_globs.extend, vec!["../../tsconfig.base.json"]);
        assert_eq!(ts.input_globs.exclude, vec!["**/*.test.ts"]);
    }

    #[test]
    fn parses_pipeline_skip_packages() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(
            br#"{
            "pipeline": {
                "build": {
                    "skip_packages": ["@scope/pkg-a", "@scope/pkg-b"]
                },
                "test": {
                    "skip_packages": ["@scope/pkg-c"]
                }
            }
        }"#,
        )
        .unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        let build = cfg.pipeline.get("build").unwrap();
        assert_eq!(build.skip_packages, vec!["@scope/pkg-a", "@scope/pkg-b"]);
        let test = cfg.pipeline.get("test").unwrap();
        assert_eq!(test.skip_packages, vec!["@scope/pkg-c"]);
    }

    #[test]
    fn pipeline_defaults_to_empty() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(b"{}").unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        assert!(cfg.pipeline.is_empty());
    }

    #[test]
    fn pipeline_skip_packages_defaults_to_empty_vec() {
        let d = tmpdir();
        let mut f = std::fs::File::create(d.join("rage.json")).unwrap();
        f.write_all(br#"{"pipeline": {"build": {}}}"#).unwrap();
        let cfg = load_config(&d).unwrap().unwrap();
        let build = cfg.pipeline.get("build").unwrap();
        assert!(build.skip_packages.is_empty());
    }

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
}
