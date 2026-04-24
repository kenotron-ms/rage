//! Discover workspace packages from a repo root.

use crate::detect::{detect_package_manager, PackageManager};
use crate::package::Package;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Discover all workspace packages under `root`.
///
/// Auto-detects the package manager. Returns packages with empty
/// `dependencies` — use `graph::build_package_graph` to resolve those.
pub fn discover_packages(root: &Path) -> Result<Vec<Package>> {
    let pm = detect_package_manager(root).ok_or_else(|| {
        anyhow!(
            "{} is not a recognized JS workspace (no pnpm-workspace.yaml or package.json with `workspaces`)",
            root.display()
        )
    })?;

    let globs = read_package_globs(root, pm)?;

    let mut packages = Vec::new();
    for pattern in &globs {
        for dir in resolve_glob(root, pattern)? {
            if !dir.join("package.json").exists() {
                continue;
            }
            let pkg = Package::from_manifest_dir(dir)?;
            packages.push(pkg);
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

fn read_package_globs(root: &Path, pm: PackageManager) -> Result<Vec<String>> {
    match pm {
        PackageManager::Pnpm => {
            let raw = std::fs::read_to_string(root.join("pnpm-workspace.yaml"))
                .context("reading pnpm-workspace.yaml")?;
            #[derive(serde::Deserialize)]
            struct PnpmWorkspace {
                packages: Vec<String>,
            }
            let parsed: PnpmWorkspace = serde_yml::from_str(&raw)
                .context("parsing pnpm-workspace.yaml")?;
            Ok(parsed.packages)
        }
        PackageManager::Yarn | PackageManager::Npm => {
            let raw = std::fs::read_to_string(root.join("package.json"))
                .context("reading root package.json")?;
            let parsed: serde_json::Value = serde_json::from_str(&raw)
                .context("parsing root package.json")?;
            let ws = parsed
                .get("workspaces")
                .ok_or_else(|| anyhow!("root package.json has no `workspaces` field"))?;

            let globs: Vec<String> = if let Some(arr) = ws.as_array() {
                arr.iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .ok_or_else(|| anyhow!("workspaces entries must be strings, got: {v}"))
                    })
                    .collect::<Result<Vec<String>>>()?
            } else if let Some(pkgs) = ws.get("packages").and_then(|v| v.as_array()) {
                pkgs.iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .ok_or_else(|| anyhow!("workspaces entries must be strings, got: {v}"))
                    })
                    .collect::<Result<Vec<String>>>()?
            } else {
                return Err(anyhow!(
                    "`workspaces` field must be a string array or {{ packages: string[] }}"
                ));
            };
            Ok(globs)
        }
    }
}

fn resolve_glob(root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let full = root.join(pattern);
    let pattern_str = full
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 path in glob: {:?}", full))?;
    let mut out = Vec::new();
    for entry in glob::glob(pattern_str).with_context(|| format!("bad glob {pattern}"))? {
        let path = entry.with_context(|| format!("glob entry error for {pattern}"))?;
        if path.is_dir() {
            out.push(
                path.canonicalize()
                    .with_context(|| format!("canonicalizing {}", path.display()))?,
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .join("fixtures")
    }

    fn names(pkgs: &[Package]) -> Vec<String> {
        let mut n: Vec<_> = pkgs.iter().map(|p| p.name.clone()).collect();
        n.sort();
        n
    }

    #[test]
    fn discovers_pnpm_packages() {
        let dir = fixtures_dir().join("js-pnpm");
        let pkgs = discover_packages(&dir).unwrap();
        assert_eq!(
            names(&pkgs),
            vec![
                "@fixture/app".to_string(),
                "@fixture/core".to_string(),
                "@fixture/ui".to_string(),
                "@fixture/utils".to_string(),
            ]
        );
        for p in &pkgs {
            assert!(p.path.is_absolute(), "path not absolute: {:?}", p.path);
            assert!(p.path.exists(), "path missing: {:?}", p.path);
        }
    }

    #[test]
    fn discovers_yarn_packages() {
        let dir = fixtures_dir().join("js-yarn");
        let pkgs = discover_packages(&dir).unwrap();
        assert_eq!(
            names(&pkgs),
            vec![
                "@yarn-fixture/app".to_string(),
                "@yarn-fixture/core".to_string(),
                "@yarn-fixture/lib".to_string(),
            ]
        );
    }

    #[test]
    fn discovers_npm_packages() {
        let dir = fixtures_dir().join("js-npm");
        let pkgs = discover_packages(&dir).unwrap();
        assert_eq!(
            names(&pkgs),
            vec![
                "@npm-fixture/client".to_string(),
                "@npm-fixture/server".to_string(),
                "@npm-fixture/shared".to_string(),
            ]
        );
    }

    #[test]
    fn errors_when_not_a_workspace() {
        let dir = PathBuf::from("/tmp");
        assert!(discover_packages(&dir).is_err());
    }

    /// Non-string entries in a flat `workspaces` array must produce an error,
    /// not be silently dropped (Fix 2 – direct array form).
    #[test]
    fn errors_on_non_string_workspace_entry_array() {
        use std::fs;
        let dir = std::env::temp_dir().join("rage_ws_test_nonstring_array");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"root","workspaces":[42]}"#,
        )
        .unwrap();
        let err = discover_packages(&dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("workspaces entries must be strings"),
            "expected error about non-string entries, got: {msg}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Non-string entries inside `workspaces.packages` must also produce an
    /// error (Fix 2 – nested `packages` form).
    #[test]
    fn errors_on_non_string_workspace_packages_entry() {
        use std::fs;
        let dir = std::env::temp_dir().join("rage_ws_test_nonstring_pkgs");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"name":"root","workspaces":{"packages":[null]}}"#,
        )
        .unwrap();
        let err = discover_packages(&dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("workspaces entries must be strings"),
            "expected error about non-string entries, got: {msg}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
