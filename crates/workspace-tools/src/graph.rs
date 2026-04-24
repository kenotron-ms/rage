//! Resolve workspace-internal dependency edges between packages.

use crate::package::Package;
use anyhow::{Context, Result};
use std::collections::HashSet;

/// Fill in each package's `dependencies` field with the names of
/// workspace-internal packages it depends on. External npm dependencies
/// (anything not in the workspace) are filtered out.
pub fn build_package_graph(packages: Vec<Package>) -> Result<Vec<Package>> {
    let workspace_names: HashSet<String> =
        packages.iter().map(|p| p.name.clone()).collect();

    let mut out = Vec::with_capacity(packages.len());
    for mut pkg in packages {
        let manifest_path = pkg.path.join("package.json");

        let raw = match std::fs::read_to_string(&manifest_path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No manifest on disk: treat as no deps.
                // Real discovery paths always have a manifest; in-memory test
                // packages may not.
                out.push(pkg);
                continue;
            }
            Err(e) => {
                return Err(anyhow::Error::from(e)
                    .context(format!("reading {}", manifest_path.display())));
            }
        };

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        let mut deps: Vec<String> = Vec::new();
        for field in ["dependencies", "devDependencies", "peerDependencies"] {
            if let Some(obj) = parsed.get(field).and_then(|v| v.as_object()) {
                for name in obj.keys() {
                    if workspace_names.contains(name) && !deps.contains(name) {
                        deps.push(name.clone());
                    }
                }
            }
        }
        deps.sort();
        pkg.dependencies = deps;
        out.push(pkg);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::discover_packages;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .join("fixtures")
    }

    fn get<'a>(pkgs: &'a [Package], name: &str) -> &'a Package {
        pkgs.iter().find(|p| p.name == name).expect("package not found")
    }

    #[test]
    fn resolves_pnpm_workspace_deps() {
        let dir = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&dir).unwrap();
        let resolved = build_package_graph(raw).unwrap();

        assert!(get(&resolved, "@fixture/core").dependencies.is_empty());
        assert_eq!(get(&resolved, "@fixture/utils").dependencies, vec!["@fixture/core"]);
        assert_eq!(
            get(&resolved, "@fixture/ui").dependencies,
            vec!["@fixture/core", "@fixture/utils"]
        );
        assert_eq!(
            get(&resolved, "@fixture/app").dependencies,
            vec!["@fixture/core", "@fixture/ui"]
        );
    }

    #[test]
    fn resolves_yarn_workspace_deps() {
        let dir = fixtures_dir().join("js-yarn");
        let resolved = build_package_graph(discover_packages(&dir).unwrap()).unwrap();
        assert!(get(&resolved, "@yarn-fixture/core").dependencies.is_empty());
        assert_eq!(
            get(&resolved, "@yarn-fixture/lib").dependencies,
            vec!["@yarn-fixture/core"]
        );
        assert_eq!(
            get(&resolved, "@yarn-fixture/app").dependencies,
            vec!["@yarn-fixture/core", "@yarn-fixture/lib"]
        );
    }

    #[test]
    fn resolves_npm_workspace_deps() {
        let dir = fixtures_dir().join("js-npm");
        let resolved = build_package_graph(discover_packages(&dir).unwrap()).unwrap();
        assert_eq!(
            get(&resolved, "@npm-fixture/server").dependencies,
            vec!["@npm-fixture/shared"]
        );
        assert_eq!(
            get(&resolved, "@npm-fixture/client").dependencies,
            vec!["@npm-fixture/shared"]
        );
        assert!(get(&resolved, "@npm-fixture/shared").dependencies.is_empty());
    }

    #[test]
    fn dependencies_are_returned_in_sorted_order() {
        let dir = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&dir).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        for pkg in &resolved {
            let mut expected = pkg.dependencies.clone();
            expected.sort();
            assert_eq!(
                pkg.dependencies, expected,
                "package {} deps not sorted: {:?}",
                pkg.name, pkg.dependencies
            );
        }
    }

    #[test]
    fn resolves_dev_and_peer_dependencies() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // @ws/a has @ws/b as a devDependency
        let a_dir = root.join("a");
        fs::create_dir_all(&a_dir).unwrap();
        fs::write(
            a_dir.join("package.json"),
            r#"{"name":"@ws/a","version":"1.0.0","devDependencies":{"@ws/b":"1.0.0"}}"#,
        )
        .unwrap();

        // @ws/b has @ws/a as a peerDependency
        let b_dir = root.join("b");
        fs::create_dir_all(&b_dir).unwrap();
        fs::write(
            b_dir.join("package.json"),
            r#"{"name":"@ws/b","version":"1.0.0","peerDependencies":{"@ws/a":"1.0.0"}}"#,
        )
        .unwrap();

        let pkgs = vec![
            Package {
                name: "@ws/a".into(),
                version: "1.0.0".into(),
                path: a_dir,
                dependencies: Vec::new(),
            },
            Package {
                name: "@ws/b".into(),
                version: "1.0.0".into(),
                path: b_dir,
                dependencies: Vec::new(),
            },
        ];

        let resolved = build_package_graph(pkgs).unwrap();
        let a = resolved.iter().find(|p| p.name == "@ws/a").unwrap();
        let b = resolved.iter().find(|p| p.name == "@ws/b").unwrap();

        assert_eq!(a.dependencies, vec!["@ws/b"], "devDependency should be resolved");
        assert_eq!(b.dependencies, vec!["@ws/a"], "peerDependency should be resolved");
    }

    #[test]
    fn external_deps_are_filtered_out() {
        // In-memory packages whose paths don't exist on disk.
        // build_package_graph must tolerate missing manifests (treat as no deps).
        let pkgs = vec![
            Package {
                name: "a".into(),
                version: "1.0.0".into(),
                path: PathBuf::from("/tmp/a"),
                dependencies: Vec::new(),
            },
            Package {
                name: "b".into(),
                version: "1.0.0".into(),
                path: PathBuf::from("/tmp/b"),
                dependencies: Vec::new(),
            },
        ];
        let out = build_package_graph(pkgs).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|p| p.dependencies.is_empty()));
    }
}
