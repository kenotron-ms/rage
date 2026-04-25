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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Policy {
    pub selector: String,
    #[serde(default)]
    pub sandbox: Option<SandboxMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct RageConfig {
    pub plugins: Vec<String>,
    pub sandbox: SandboxConfig,
    pub cache: CacheConfig,
    pub policies: Vec<Policy>,
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
        let d = std::env::temp_dir().join(format!(
            "rage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        f.write_all(br#"{"cache": {"backend": "local", "dir": "/tmp/rage-cache"}}"#).unwrap();
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
