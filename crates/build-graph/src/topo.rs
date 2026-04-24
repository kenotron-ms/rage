//! Topological ordering: dependencies before dependents.

use crate::dag::{DagError, WorkspaceDag};

/// Return package names in dependency order — each package's dependencies
/// appear before it.
pub fn topological_sort(dag: &WorkspaceDag) -> Result<Vec<String>, DagError> {
    use petgraph::visit::Reversed;
    let reversed = Reversed(&dag.graph);
    let order = petgraph::algo::toposort(reversed, None)
        .map_err(|_| DagError::Cycle("<cycle detected in topo>".to_string()))?;
    Ok(order.into_iter().map(|idx| dag.graph[idx].clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::build_dag;
    use std::path::PathBuf;
    use workspace_tools::Package;

    fn mk(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn pos(order: &[String], name: &str) -> usize {
        order.iter().position(|n| n == name).expect("name not in order")
    }

    #[test]
    fn single_package() {
        let dag = build_dag(vec![mk("solo", &[])]).unwrap();
        let order = topological_sort(&dag).unwrap();
        assert_eq!(order, vec!["solo"]);
    }

    #[test]
    fn dependencies_come_first() {
        let dag = build_dag(vec![
            mk("core", &[]),
            mk("utils", &["core"]),
            mk("ui", &["core", "utils"]),
            mk("app", &["ui", "core"]),
        ]).unwrap();
        let order = topological_sort(&dag).unwrap();
        assert_eq!(order.len(), 4);
        assert!(pos(&order, "core") < pos(&order, "utils"));
        assert!(pos(&order, "core") < pos(&order, "ui"));
        assert!(pos(&order, "utils") < pos(&order, "ui"));
        assert!(pos(&order, "ui") < pos(&order, "app"));
    }
}
