//! Affected-package computation — direct match + transitive dependent closure.

use build_graph::dag::WorkspaceDag;
use petgraph::visit::{Bfs, Reversed};
use std::collections::HashSet;
use std::path::PathBuf;
use workspace_tools::Package;

/// Return the set of package names affected by the given changed files.
///
/// A package is **directly affected** if any `changed_file` has a path prefix
/// matching `pkg.path`.
///
/// A package is **transitively affected** if it directly or transitively
/// depends on any directly-affected package (i.e., it would need a rebuild).
///
/// The returned set includes both directly and transitively affected packages.
pub fn affected_packages(
    packages: &[Package],
    dag: &WorkspaceDag,
    changed_files: &[PathBuf],
) -> HashSet<String> {
    // 1. Find directly-affected packages: those whose path contains a changed file
    let directly_affected: Vec<String> = packages
        .iter()
        .filter(|pkg| changed_files.iter().any(|f| f.starts_with(&pkg.path)))
        .map(|pkg| pkg.name.clone())
        .collect();

    if directly_affected.is_empty() {
        return HashSet::new();
    }

    // 2. Compute transitive dependents via reverse BFS in the dependency graph.
    //
    // The DAG has edges pkg → dep (pkg depends on dep).
    // Reversing gives dep → pkg (dep is depended on by pkg).
    // BFS from each directly-affected node in the reversed graph visits
    // all packages that (transitively) depend on the directly-affected package.
    let reversed = Reversed(&dag.graph);
    let mut affected: HashSet<String> = HashSet::new();

    for pkg_name in directly_affected {
        if let Some(&start) = dag.nodes.get(&pkg_name) {
            let mut bfs = Bfs::new(reversed, start);
            while let Some(nx) = bfs.next(reversed) {
                affected.insert(dag.graph[nx].clone());
            }
        }
    }

    affected
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;

    fn pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/workspace").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn file(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn empty_changed_files_returns_empty_set() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();
        let affected = affected_packages(&packages, &dag, &[]);
        assert!(affected.is_empty());
    }

    #[test]
    fn directly_affected_package_included() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();
        let changed = vec![file("/workspace/core/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);
        assert!(
            affected.contains("core"),
            "core should be directly affected"
        );
    }

    #[test]
    fn transitive_dependents_included() {
        // core ← utils ← ui ← app  (arrow = "depends on")
        let packages = vec![
            pkg("core", &[]),
            pkg("utils", &["core"]),
            pkg("ui", &["core", "utils"]),
            pkg("app", &["core", "ui"]),
        ];
        let dag = build_dag(packages.clone()).unwrap();

        // Only utils changed
        let changed = vec![file("/workspace/utils/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        // utils itself + ui (depends on utils) + app (depends on ui)
        assert!(affected.contains("utils"), "utils directly affected");
        assert!(affected.contains("ui"), "ui transitively affected");
        assert!(affected.contains("app"), "app transitively affected");
        // core is a dep OF utils, not a dependent — NOT affected
        assert!(!affected.contains("core"), "core should not be affected");
    }

    #[test]
    fn leaf_change_only_affects_leaf() {
        // core ← app
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();

        // app is a leaf (nothing depends on it)
        let changed = vec![file("/workspace/app/index.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert!(affected.contains("app"), "app directly affected");
        assert!(
            !affected.contains("core"),
            "core not affected when only app changes"
        );
    }

    #[test]
    fn root_change_affects_all() {
        let packages = vec![
            pkg("core", &[]),
            pkg("utils", &["core"]),
            pkg("ui", &["core", "utils"]),
            pkg("app", &["core", "ui"]),
        ];
        let dag = build_dag(packages.clone()).unwrap();

        // core changed — everything that depends on core (directly or transitively) is affected
        let changed = vec![file("/workspace/core/src/main.ts")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert_eq!(
            affected.len(),
            4,
            "all 4 packages should be affected when core changes"
        );
    }

    #[test]
    fn file_outside_any_package_affects_nothing() {
        let packages = vec![pkg("core", &[]), pkg("app", &["core"])];
        let dag = build_dag(packages.clone()).unwrap();

        // A file at the repo root, outside any package
        let changed = vec![file("/workspace/tsconfig.json")];
        let affected = affected_packages(&packages, &dag, &changed);

        assert!(
            affected.is_empty(),
            "a file outside any package should not affect any package"
        );
    }
}
