//! Render a WorkspaceDag as Graphviz DOT.

use crate::dag::WorkspaceDag;

/// Render the DAG as a DOT document. Deterministic: nodes and edges are
/// emitted in sorted order.
pub fn to_dot(dag: &WorkspaceDag) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    writeln!(out, "digraph workspace {{").unwrap();
    writeln!(out, "  rankdir=\"LR\";").unwrap();
    writeln!(out, "  node [shape=\"box\", style=\"rounded\", fontname=\"monospace\"];").unwrap();

    // Nodes — sorted by name
    let mut names: Vec<&String> = dag.packages.keys().collect();
    names.sort();
    for name in &names {
        let pkg = &dag.packages[*name];
        writeln!(
            out,
            "  \"{name}\" [label=\"{name}@{version}\"];",
            name = name,
            version = pkg.version
        )
        .unwrap();
    }

    // Edges — sorted by (source, target)
    let mut edges: Vec<(String, String)> = Vec::new();
    for edge in dag.graph.edge_indices() {
        let (s, t) = dag.graph.edge_endpoints(edge).unwrap();
        edges.push((dag.graph[s].clone(), dag.graph[t].clone()));
    }
    edges.sort();
    for (src, dst) in edges {
        writeln!(out, "  \"{src}\" -> \"{dst}\";").unwrap();
    }

    writeln!(out, "}}").unwrap();
    out
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

    #[test]
    fn empty_dag() {
        let dag = build_dag(Vec::new()).unwrap();
        let dot = to_dot(&dag);
        assert!(dot.starts_with("digraph workspace {"));
        assert!(dot.trim_end().ends_with('}'));
        assert!(dot.contains("rankdir=\"LR\""));
    }

    #[test]
    fn nodes_and_edges_sorted_and_rendered() {
        let dag = build_dag(vec![
            mk("@fixture/core", &[]),
            mk("@fixture/utils", &["@fixture/core"]),
            mk("@fixture/ui", &["@fixture/core", "@fixture/utils"]),
            mk("@fixture/app", &["@fixture/ui", "@fixture/core"]),
        ]).unwrap();
        let dot = to_dot(&dag);

        assert!(dot.contains(r#""@fixture/core" [label="@fixture/core@1.0.0"]"#), "dot was: {dot}");
        assert!(dot.contains(r#""@fixture/app" [label="@fixture/app@1.0.0"]"#));

        assert!(dot.contains(r#""@fixture/utils" -> "@fixture/core""#));
        assert!(dot.contains(r#""@fixture/ui" -> "@fixture/core""#));
        assert!(dot.contains(r#""@fixture/ui" -> "@fixture/utils""#));
        assert!(dot.contains(r#""@fixture/app" -> "@fixture/ui""#));
        assert!(dot.contains(r#""@fixture/app" -> "@fixture/core""#));

        let app_node = dot.find(r#""@fixture/app" [label"#).unwrap();
        let app_edge = dot.find(r#""@fixture/app" -> "@fixture/core""#).unwrap();
        assert!(app_node < app_edge);
    }

    #[test]
    fn output_is_deterministic() {
        let pkgs1 = vec![
            mk("a", &["b"]),
            mk("b", &[]),
            mk("c", &["a", "b"]),
        ];
        let pkgs2 = vec![
            mk("c", &["b", "a"]),
            mk("b", &[]),
            mk("a", &["b"]),
        ];
        let dot1 = to_dot(&build_dag(pkgs1).unwrap());
        let dot2 = to_dot(&build_dag(pkgs2).unwrap());
        assert_eq!(dot1, dot2, "DOT output must be deterministic regardless of input order");
    }
}
