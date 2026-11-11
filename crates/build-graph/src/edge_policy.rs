//! Edge policies and transitive producer pruning for `build-graph`.
//!
//! In large monorepos (~2,800 packages), only a small subset of packages
//! actually generate files during `prepare` (such as GraphQL schemas, CSS
//! modules, or icon assets). A naive `"prepare": { "dependsOn": ["^prepare"] }`
//! rule forces long sequential wait chains through intermediate non-producer
//! packages and `devDependencies`.
//!
//! [`PrepareProducerPolicy`] rewrites `^prepare` edges using
//! [`EdgePolicy::ReplaceWithProducersReachableVia`]:
//! - `devDependency`-only edges can be dropped when `drop_dev_dependencies` is set.
//! - Edges to non-producer packages are replaced with direct edges to all
//!   transitive `prepare` producers reachable through that dependency.

use crate::dag::WorkspaceDag;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

pub use plugin::{EdgeOrigin, EdgePolicy};

/// Policy governing how `^prepare` (or any producer-sparse task) prunes
/// non-producer intermediate hops and `devDependency` edges.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareProducerPolicy {
    /// Set of package names that are true `prepare` producers.
    pub producers: BTreeSet<String>,
    /// When `true`, edges whose origin is purely [`EdgeOrigin::DevDependency`]
    /// are dropped during `prepare` scheduling.
    pub drop_dev_dependencies: bool,
    /// Optional explicit edge origins keyed by `from_pkg -> to_pkg`. Any DAG
    /// edge not present in this map defaults to [`EdgeOrigin::Dependency`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub edge_origins: BTreeMap<String, BTreeMap<String, EdgeOrigin>>,
}

impl PrepareProducerPolicy {
    /// Construct a policy from an iterator of producer package names.
    pub fn new<I, S>(producers: I, drop_dev_dependencies: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            producers: producers.into_iter().map(Into::into).collect(),
            drop_dev_dependencies,
            edge_origins: BTreeMap::new(),
        }
    }

    /// Record the [`EdgeOrigin`] of a dependency edge `(from_pkg -> to_pkg)`.
    pub fn with_edge_origin(
        mut self,
        from_pkg: impl Into<String>,
        to_pkg: impl Into<String>,
        origin: EdgeOrigin,
    ) -> Self {
        self.edge_origins
            .entry(from_pkg.into())
            .or_default()
            .insert(to_pkg.into(), origin);
        self
    }

    /// Return `true` if `pkg` is a declared `prepare` producer.
    pub fn is_producer(&self, pkg: &str) -> bool {
        self.producers.contains(pkg)
    }

    /// Look up the [`EdgeOrigin`] for `(from_pkg, to_pkg)`, defaulting to
    /// [`EdgeOrigin::Dependency`].
    pub fn origin_of(&self, from_pkg: &str, to_pkg: &str) -> EdgeOrigin {
        self.edge_origins
            .get(from_pkg)
            .and_then(|targets| targets.get(to_pkg))
            .copied()
            .unwrap_or(EdgeOrigin::Dependency)
    }

    /// Evaluate the [`EdgePolicy`] for a direct edge `(from_pkg -> to_pkg)` in `dag`.
    ///
    /// - If `drop_dev_dependencies` is enabled and the edge has
    ///   [`EdgeOrigin::DevDependency`], returns [`EdgePolicy::Drop`].
    /// - If `to_pkg` is itself a producer, returns [`EdgePolicy::Keep`].
    /// - Otherwise, traverses `dag` from `to_pkg` (respecting `drop_dev_dependencies`
    ///   on intermediate hops) and returns
    ///   [`EdgePolicy::ReplaceWithProducersReachableVia`] with all reachable
    ///   producer package names in sorted deterministic order (or [`EdgePolicy::Drop`]
    ///   if no producer is reachable).
    pub fn evaluate_edge(&self, dag: &WorkspaceDag, from_pkg: &str, to_pkg: &str) -> EdgePolicy {
        let origin = self.origin_of(from_pkg, to_pkg);
        if self.drop_dev_dependencies && origin == EdgeOrigin::DevDependency {
            return EdgePolicy::Drop;
        }
        if self.is_producer(to_pkg) {
            return EdgePolicy::Keep;
        }

        let reachable = self.reachable_producers_from(dag, to_pkg);
        if reachable.is_empty() {
            EdgePolicy::Drop
        } else {
            EdgePolicy::ReplaceWithProducersReachableVia(reachable)
        }
    }

    /// Compute the effective `prepare` dependencies for `from_pkg` in `dag`
    /// after applying [`EdgePolicy`] across all its outgoing dependency edges.
    pub fn effective_dependencies(&self, dag: &WorkspaceDag, from_pkg: &str) -> Vec<String> {
        let Some(&src_idx) = dag.nodes.get(from_pkg) else {
            return Vec::new();
        };

        let mut out = BTreeSet::new();
        for dst_idx in dag.graph.neighbors_directed(src_idx, Direction::Outgoing) {
            let to_pkg = &dag.graph[dst_idx];
            match self.evaluate_edge(dag, from_pkg, to_pkg) {
                EdgePolicy::Keep => {
                    out.insert(to_pkg.clone());
                }
                EdgePolicy::Drop => {}
                EdgePolicy::ReplaceWithProducersReachableVia(producers) => {
                    out.extend(producers);
                }
            }
        }
        out.into_iter().collect()
    }

    /// Return a new [`WorkspaceDag`] whose edges and package `dependencies`
    /// lists have been rewritten according to this [`PrepareProducerPolicy`].
    pub fn rewrite_dag(&self, dag: &WorkspaceDag) -> WorkspaceDag {
        let mut graph = petgraph::graph::DiGraph::new();
        let mut nodes = HashMap::new();
        let mut packages = HashMap::new();

        let mut names: Vec<&String> = dag.packages.keys().collect();
        names.sort();

        for name in &names {
            let idx = graph.add_node((*name).clone());
            nodes.insert((*name).clone(), idx);
        }

        for name in &names {
            let effective_deps = self.effective_dependencies(dag, name);
            let src = nodes[*name];
            for dep in &effective_deps {
                if let Some(&dst) = nodes.get(dep) {
                    graph.add_edge(src, dst, ());
                }
            }
            if let Some(pkg) = dag.packages.get(*name) {
                let mut updated = pkg.clone();
                updated.dependencies = effective_deps;
                packages.insert((*name).clone(), updated);
            }
        }

        WorkspaceDag {
            graph,
            nodes,
            packages,
        }
    }

    fn reachable_producers_from(&self, dag: &WorkspaceDag, start_pkg: &str) -> Vec<String> {
        let Some(&start_idx) = dag.nodes.get(start_pkg) else {
            return Vec::new();
        };

        let mut visited: HashSet<petgraph::graph::NodeIndex> = HashSet::new();
        let mut queue: VecDeque<petgraph::graph::NodeIndex> = VecDeque::new();
        let mut found: BTreeSet<String> = BTreeSet::new();

        visited.insert(start_idx);
        queue.push_back(start_idx);

        while let Some(curr_idx) = queue.pop_front() {
            let curr_name = &dag.graph[curr_idx];
            for next_idx in dag.graph.neighbors_directed(curr_idx, Direction::Outgoing) {
                let next_name = &dag.graph[next_idx];
                if self.drop_dev_dependencies
                    && self.origin_of(curr_name, next_name) == EdgeOrigin::DevDependency
                {
                    continue;
                }
                if self.is_producer(next_name) {
                    found.insert(next_name.clone());
                }
                if visited.insert(next_idx) {
                    queue.push_back(next_idx);
                }
            }
        }

        found.into_iter().collect()
    }
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
    fn rewrites_non_producer_hops_to_transitive_producers() {
        // app -> feature-shell (non-producer) -> relay-schema (producer)
        //                                     -> utils (non-producer) -> icon-gen (producer)
        //                                     -> leaf-pure (non-producer)
        let dag = build_dag(vec![
            mk("relay-schema", &[]),
            mk("icon-gen", &[]),
            mk("leaf-pure", &[]),
            mk("utils", &["icon-gen", "leaf-pure"]),
            mk("feature-shell", &["relay-schema", "utils"]),
            mk("app", &["feature-shell"]),
        ])
        .unwrap();

        let policy = PrepareProducerPolicy::new(["relay-schema", "icon-gen"], true);

        assert_eq!(
            policy.evaluate_edge(&dag, "app", "feature-shell"),
            EdgePolicy::ReplaceWithProducersReachableVia(vec![
                "icon-gen".to_string(),
                "relay-schema".to_string(),
            ])
        );
        assert_eq!(
            policy.evaluate_edge(&dag, "utils", "leaf-pure"),
            EdgePolicy::Drop
        );
        assert_eq!(
            policy.evaluate_edge(&dag, "utils", "icon-gen"),
            EdgePolicy::Keep
        );

        let rewritten = policy.rewrite_dag(&dag);
        assert_eq!(
            rewritten.packages["app"].dependencies,
            vec!["icon-gen".to_string(), "relay-schema".to_string()]
        );
        assert_eq!(
            rewritten.packages["utils"].dependencies,
            vec!["icon-gen".to_string()]
        );
    }

    #[test]
    fn prunes_dev_dependency_prepare_edges() {
        // app --(Dependency)--> ui --(DevDependency)--> test-codegen (producer)
        // app --(DevDependency)--> storybook-gen (producer)
        let dag = build_dag(vec![
            mk("test-codegen", &[]),
            mk("storybook-gen", &[]),
            mk("ui", &["test-codegen"]),
            mk("app", &["ui", "storybook-gen"]),
        ])
        .unwrap();

        let policy = PrepareProducerPolicy::new(["test-codegen", "storybook-gen"], true)
            .with_edge_origin("app", "storybook-gen", EdgeOrigin::DevDependency)
            .with_edge_origin("ui", "test-codegen", EdgeOrigin::DevDependency);

        assert_eq!(
            policy.evaluate_edge(&dag, "app", "storybook-gen"),
            EdgePolicy::Drop
        );
        assert_eq!(
            policy.evaluate_edge(&dag, "app", "ui"),
            EdgePolicy::Drop,
            "transitive producer reachable only via DevDependency must be pruned"
        );
        assert!(policy.effective_dependencies(&dag, "app").is_empty());

        // When drop_dev_dependencies is false, both producers remain reachable.
        let keep_dev_policy = PrepareProducerPolicy {
            drop_dev_dependencies: false,
            ..policy
        };
        assert_eq!(
            keep_dev_policy.effective_dependencies(&dag, "app"),
            vec!["storybook-gen".to_string(), "test-codegen".to_string()]
        );
    }

    #[test]
    fn policy_with_edge_origins_roundtrips_json() {
        let policy = PrepareProducerPolicy::new(["relay-schema", "icon-gen"], true)
            .with_edge_origin("app", "storybook-gen", EdgeOrigin::DevDependency)
            .with_edge_origin("ui", "relay-schema", EdgeOrigin::Dependency);

        let json = serde_json::to_string(&policy).expect("serialize policy with edge_origins");
        let decoded: PrepareProducerPolicy =
            serde_json::from_str(&json).expect("deserialize policy with edge_origins");
        assert_eq!(decoded, policy);
        assert_eq!(
            decoded.origin_of("app", "storybook-gen"),
            EdgeOrigin::DevDependency
        );
    }
}
