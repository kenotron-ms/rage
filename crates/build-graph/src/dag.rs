//! Directed graph of workspace packages.

use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use thiserror::Error;
use workspace_tools::Package;

#[derive(Debug, Error)]
pub enum DagError {
    #[error("package {0} depends on {1} which is not in the workspace")]
    UnknownDependency(String, String),
    #[error("workspace contains a dependency cycle involving: {0}")]
    Cycle(String),
}

#[derive(Debug)]
pub struct WorkspaceDag {
    pub graph: DiGraph<String, ()>,
    pub nodes: HashMap<String, NodeIndex>,
    pub packages: HashMap<String, Package>,
}

impl WorkspaceDag {
    pub fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

/// Build a `WorkspaceDag` from a resolved package list.
pub fn build_dag(packages: Vec<Package>) -> Result<WorkspaceDag, DagError> {
    let mut graph: DiGraph<String, ()> = DiGraph::new();
    let mut nodes: HashMap<String, NodeIndex> = HashMap::new();
    let mut pkg_map: HashMap<String, Package> = HashMap::new();

    for pkg in &packages {
        let idx = graph.add_node(pkg.name.clone());
        nodes.insert(pkg.name.clone(), idx);
    }

    for pkg in &packages {
        let src = nodes[&pkg.name];
        for dep in &pkg.dependencies {
            let dst = *nodes
                .get(dep)
                .ok_or_else(|| DagError::UnknownDependency(pkg.name.clone(), dep.clone()))?;
            graph.add_edge(src, dst, ());
        }
    }

    for pkg in packages {
        pkg_map.insert(pkg.name.clone(), pkg);
    }

    // Detect cycles — toposort returns the offending node on failure
    if let Err(cycle) = petgraph::algo::toposort(&graph, None) {
        return Err(DagError::Cycle(graph[cycle.node_id()].clone()));
    }

    Ok(WorkspaceDag {
        graph,
        nodes,
        packages: pkg_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn builds_empty_dag() {
        let dag = build_dag(Vec::new()).unwrap();
        assert_eq!(dag.package_count(), 0);
        assert_eq!(dag.edge_count(), 0);
    }

    #[test]
    fn builds_dag_with_edges() {
        let pkgs = vec![
            mk("core", &[]),
            mk("utils", &["core"]),
            mk("ui", &["core", "utils"]),
            mk("app", &["ui", "core"]),
        ];
        let dag = build_dag(pkgs).unwrap();
        assert_eq!(dag.package_count(), 4);
        assert_eq!(dag.edge_count(), 5); // utils->core, ui->core, ui->utils, app->ui, app->core
    }

    #[test]
    fn unknown_dependency_is_an_error() {
        let pkgs = vec![mk("a", &["does-not-exist"])];
        let err = build_dag(pkgs).unwrap_err();
        assert!(matches!(err, DagError::UnknownDependency(_, _)));
    }

    #[test]
    fn cycle_is_an_error() {
        // a -> b -> a
        let pkgs = vec![mk("a", &["b"]), mk("b", &["a"])];
        let err = build_dag(pkgs).unwrap_err();
        let DagError::Cycle(name) = err else {
            panic!("expected Cycle, got: {err:?}")
        };
        assert!(name == "a" || name == "b", "cycle node name was: {name}");
    }
}
