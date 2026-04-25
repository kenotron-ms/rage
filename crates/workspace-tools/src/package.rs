//! A discovered workspace package.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The name from `package.json`, e.g. `@fixture/core`.
    pub name: String,
    /// The version from `package.json`.
    pub version: String,
    /// Absolute path to the package directory.
    pub path: PathBuf,
    /// Names of workspace-internal packages this package depends on.
    /// Populated after dependency resolution; raw discovery leaves this empty.
    pub dependencies: Vec<String>,
}

impl Package {
    /// Parse a `package.json` at `path` (a directory) into a `Package` with
    /// no resolved dependencies. Returns an error if the manifest is missing
    /// or malformed.
    pub fn from_manifest_dir(path: PathBuf) -> anyhow::Result<Self> {
        use anyhow::Context;
        let manifest_path = path.join("package.json");
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        let name = parsed
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("{} is missing a `name` field", manifest_path.display())
            })?
            .to_string();

        let version = parsed
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0")
            .to_string();

        Ok(Package {
            name,
            version,
            path,
            dependencies: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fixtures")
    }

    #[test]
    fn parses_core_package() {
        let dir = fixtures_dir().join("js-pnpm/packages/core");
        let pkg = Package::from_manifest_dir(dir.clone()).unwrap();
        assert_eq!(pkg.name, "@fixture/core");
        assert_eq!(pkg.version, "1.0.0");
        assert_eq!(pkg.path, dir);
        assert!(pkg.dependencies.is_empty());
    }

    #[test]
    fn parses_package_with_deps_ignoring_them_at_this_stage() {
        let dir = fixtures_dir().join("js-pnpm/packages/ui");
        let pkg = Package::from_manifest_dir(dir).unwrap();
        assert_eq!(pkg.name, "@fixture/ui");
        // Dependency resolution happens later in graph.rs
        assert!(pkg.dependencies.is_empty());
    }

    #[test]
    fn errors_on_missing_directory() {
        let dir = fixtures_dir().join("js-pnpm/packages/nonexistent");
        let err = Package::from_manifest_dir(dir).unwrap_err();
        assert!(
            err.to_string().contains("reading"),
            "expected read error, got: {err}"
        );
    }

    #[test]
    fn errors_on_missing_name_field() {
        // Write a temp package.json without a name field
        let tmp = std::env::temp_dir().join("rage-test-pkg");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("package.json"), r#"{"version":"1.0.0"}"#).unwrap();
        let err = Package::from_manifest_dir(tmp.clone()).unwrap_err();
        assert!(
            err.to_string().contains("missing a `name` field"),
            "got: {err}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
