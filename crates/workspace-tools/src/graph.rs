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

        // Tolerate missing manifests — in-memory test packages and packages
        // without a package.json on disk have no deps to resolve.
        let raw = match std::fs::read_to_string(&manifest_path) {
            Ok(r) => r,
            Err(_) => {
                out.push(pkg);
                continue;
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

    fn sorted_deps(pkg: &Package) -> Vec<String> {
        let mut d = pkg.dependencies.clone();
        d.sort();
        d
    }

    #[test]
    fn resolves_pnpm_workspace_deps() {
        let dir = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&dir).unwrap();
        let resolved = build_package_graph(raw).unwrap();

        assert_eq!(sorted_deps(get(&resolved, "@fixture/core")), Vec::<String>::new());
        assert_eq!(sorted_deps(get(&resolved, "@fixture/utils")), vec!["@fixture/core"]);
        assert_eq!(
            sorted_deps(get(&resolved, "@fixture/ui")),
            vec!["@fixture/core", "@fixture/utils"]
        );
        assert_eq!(
            sorted_deps(get(&resolved, "@fixture/app")),
            vec!["@fixture/core", "@fixture/ui"]
        );
    }

    #[test]
    fn resolves_yarn_workspace_deps() {
        let dir = fixtures_dir().join("js-yarn");
        let resolved = build_package_graph(discover_packages(&dir).unwrap()).unwrap();
        assert_eq!(sorted_deps(get(&resolved, "@yarn-fixture/core")), Vec::<String>::new());
        assert_eq!(
            sorted_deps(get(&resolved, "@yarn-fixture/lib")),
            vec!["@yarn-fixture/core"]
        );
        assert_eq!(
            sorted_deps(get(&resolved, "@yarn-fixture/app")),
            vec!["@yarn-fixture/core", "@yarn-fixture/lib"]
        );
    }

    #[test]
    fn resolves_npm_workspace_deps() {
        let dir = fixtures_dir().join("js-npm");
        let resolved = build_package_graph(discover_packages(&dir).unwrap()).unwrap();
        assert_eq!(
            sorted_deps(get(&resolved, "@npm-fixture/server")),
            vec!["@npm-fixture/shared"]
        );
        assert_eq!(
            sorted_deps(get(&resolved, "@npm-fixture/client")),
            vec!["@npm-fixture/shared"]
        );
        assert!(get(&resolved, "@npm-fixture/shared").dependencies.is_empty());
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
