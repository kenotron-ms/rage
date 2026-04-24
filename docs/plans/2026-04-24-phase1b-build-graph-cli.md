# Phase 1b: build-graph + pipeline-config + CLI Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.
> **Prerequisite:** Phase 1a (`2026-04-24-phase1a-workspace-tools.md`) must be complete. That means `cargo test -p workspace-tools` passes and the `workspace-tools` crate exports `detect_package_manager`, `discover_packages`, `build_package_graph`, `Package`, and `PackageManager`.

**Goal:** Add three crates on top of Phase 1a: `build-graph` (DAG + topo sort + DOT output), `pipeline-config` (rage.json loader), and `cli` (the `rage` binary with a `graph` subcommand). End state: `rage graph fixtures/js-pnpm` emits valid DOT that `dot -Tsvg` renders.

**Architecture:** `build-graph` converts the resolved `Vec<Package>` from `workspace-tools` into a directed acyclic graph using `petgraph`, then renders DOT. `pipeline-config` is a minimal skeleton for future phases (only needed so the CLI can take `--workspace` and later layer in config). `cli` wires everything together behind `clap`.

**Tech Stack:** Rust 2021, `petgraph` for the DAG, `clap` for argument parsing, `serde_json` for rage.json parsing.

---

## Context For The Implementer

Phase 1a implemented workspace/package discovery. This phase turns that data into a renderable dependency graph and exposes it via a `rage` binary.

**Rules for this plan:**
- Follow each task's steps literally and in order.
- Do **not** add functionality beyond what this plan specifies. No "while I'm here" refactors. No extra CLI subcommands. No extra DOT attributes.
- Commit after each task with the exact message given.
- If a test fails unexpectedly, STOP and report. Do not change the test to match broken behavior.

---

## Task 1: Register `build-graph` Crate In Workspace

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/Cargo.toml`
- Create: `crates/build-graph/Cargo.toml`
- Create: `crates/build-graph/src/lib.rs`
- Create: `crates/build-graph/src/dag.rs`
- Create: `crates/build-graph/src/topo.rs`
- Create: `crates/build-graph/src/dot.rs`

**Step 1: Add the crate to the workspace members list**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml`. The `members` list currently reads:

```toml
members = [
    "crates/workspace-tools",
]
```

Change it to:

```toml
members = [
    "crates/workspace-tools",
    "crates/build-graph",
]
```

**Step 2: Create `crates/build-graph/Cargo.toml`**

Write exactly this to `/Users/ken/workspace/ms/rage/crates/build-graph/Cargo.toml`:

```toml
[package]
name = "build-graph"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
petgraph = "0.6"
thiserror = "2"
workspace-tools = { path = "../workspace-tools" }
```

**Step 3: Create the source files**

Write exactly this to `crates/build-graph/src/lib.rs`:

```rust
//! Package dependency graph, topological ordering, and DOT rendering.

pub mod dag;
pub mod dot;
pub mod topo;
```

Write exactly `// placeholder\n` to each of:
- `crates/build-graph/src/dag.rs`
- `crates/build-graph/src/topo.rs`
- `crates/build-graph/src/dot.rs`

**Step 4: Verify it compiles**

Run: `cd /Users/ken/workspace/ms/rage && cargo build -p build-graph 2>&1 | tail -5`

Expected: `Compiling build-graph v0.0.0 ...` then `Finished ...`. Warnings about unused placeholders are OK.

**Step 5: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add Cargo.toml crates/build-graph && \
  git commit -m "feat(build-graph): scaffold crate"
```

---

## Task 2: Implement `WorkspaceDag` And `build_dag`

**Files:**
- Modify: `crates/build-graph/src/dag.rs`

The DAG wraps `petgraph::Graph<String, ()>` where nodes are package names and edges point from a package to each of its dependencies (so edge A→B means "A depends on B"). We also store a name-indexed lookup to the full `Package` for DOT rendering.

**Step 1: Write the failing tests**

Overwrite `/Users/ken/workspace/ms/rage/crates/build-graph/src/dag.rs` with:

```rust
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
///
/// Returns `DagError::UnknownDependency` if any dependency name isn't in
/// the workspace. Returns `DagError::Cycle` if the graph has a cycle.
pub fn build_dag(packages: Vec<Package>) -> Result<WorkspaceDag, DagError> {
    todo!()
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
        assert!(matches!(err, DagError::Cycle(_)));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib dag 2>&1 | tail -15`

Expected: 4 tests fail with `not yet implemented`.

**Step 3: Implement `build_dag`**

Replace the `todo!()` body of `build_dag` in `crates/build-graph/src/dag.rs` with:

```rust
pub fn build_dag(packages: Vec<Package>) -> Result<WorkspaceDag, DagError> {
    let mut graph: DiGraph<String, ()> = DiGraph::new();
    let mut nodes: HashMap<String, NodeIndex> = HashMap::new();
    let mut pkg_map: HashMap<String, Package> = HashMap::new();

    // Add nodes
    for pkg in &packages {
        let idx = graph.add_node(pkg.name.clone());
        nodes.insert(pkg.name.clone(), idx);
    }

    // Add edges (package -> dep)
    for pkg in &packages {
        let src = nodes[&pkg.name];
        for dep in &pkg.dependencies {
            let dst = *nodes.get(dep).ok_or_else(|| {
                DagError::UnknownDependency(pkg.name.clone(), dep.clone())
            })?;
            graph.add_edge(src, dst, ());
        }
    }

    // Populate packages map
    for pkg in packages {
        pkg_map.insert(pkg.name.clone(), pkg);
    }

    // Detect cycles
    if petgraph::algo::is_cyclic_directed(&graph) {
        // Find a node that participates in a cycle for a better error message.
        let offender = graph
            .node_indices()
            .find(|&n| {
                let mut dfs = petgraph::visit::Dfs::new(&graph, n);
                // Walk from n and see if we return to n
                let mut visited_self = false;
                while let Some(m) = dfs.next(&graph) {
                    if m == n && visited_self {
                        return true;
                    }
                    if m == n {
                        visited_self = true;
                    }
                }
                false
            })
            .map(|i| graph[i].clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        return Err(DagError::Cycle(offender));
    }

    Ok(WorkspaceDag {
        graph,
        nodes,
        packages: pkg_map,
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib dag 2>&1 | tail -10`

Expected: `test result: ok. 4 passed; 0 failed`.

If the cycle detection test fails because the `offender` detection returns `<unknown>`: that's still fine — the test only checks that we return `DagError::Cycle(_)`, not what the string is. If any test actually fails, STOP and report.

**Step 5: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add crates/build-graph/src/dag.rs && \
  git commit -m "feat(build-graph): WorkspaceDag + build_dag with cycle detection"
```

---

## Task 3: Implement `topological_sort`

**Files:**
- Modify: `crates/build-graph/src/topo.rs`

Topological order is "leaves first" — a package appears before any package that depends on it. For our graph (edge A→B means "A depends on B"), that means nodes with no outgoing edges come first. In petgraph terms: reverse-toposort, or toposort on the graph with edges reversed.

**Step 1: Write the failing tests**

Overwrite `/Users/ken/workspace/ms/rage/crates/build-graph/src/topo.rs` with:

```rust
//! Topological ordering: dependencies before dependents.

use crate::dag::{DagError, WorkspaceDag};

/// Return package names in dependency order — each package's dependencies
/// appear before it.
pub fn topological_sort(dag: &WorkspaceDag) -> Result<Vec<String>, DagError> {
    todo!()
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
```

**Step 2: Run tests to verify they fail**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib topo 2>&1 | tail -15`

Expected: 2 tests fail with `not yet implemented`.

**Step 3: Implement `topological_sort`**

Replace the `todo!()` body in `crates/build-graph/src/topo.rs` with:

```rust
pub fn topological_sort(dag: &WorkspaceDag) -> Result<Vec<String>, DagError> {
    use petgraph::visit::Reversed;
    // Edges point package -> dependency. For dependency-first order, reverse.
    let reversed = Reversed(&dag.graph);
    let order = petgraph::algo::toposort(reversed, None)
        .map_err(|_| DagError::Cycle("<cycle detected in topo>".to_string()))?;
    Ok(order.into_iter().map(|idx| dag.graph[idx].clone()).collect())
}
```

**Step 4: Run tests to verify they pass**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib topo 2>&1 | tail -10`

Expected: `test result: ok. 2 passed; 0 failed`.

**Step 5: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add crates/build-graph/src/topo.rs && \
  git commit -m "feat(build-graph): topological_sort dependencies-first"
```

---

## Task 4: Implement DOT Output

**Files:**
- Modify: `crates/build-graph/src/dot.rs`

**Output format** (matches the design doc — edges mean "depends on"):

```
digraph workspace {
  rankdir="LR";
  node [shape="box", style="rounded", fontname="monospace"];
  "@fixture/core" [label="@fixture/core@1.0.0"];
  "@fixture/utils" [label="@fixture/utils@1.0.0"];
  ...
  "@fixture/utils" -> "@fixture/core";
  ...
}
```

Node and edge ordering must be deterministic (sorted alphabetically) so output is stable across runs.

**Step 1: Write the failing tests**

Overwrite `/Users/ken/workspace/ms/rage/crates/build-graph/src/dot.rs` with:

```rust
//! Render a WorkspaceDag as Graphviz DOT.

use crate::dag::WorkspaceDag;

/// Render the DAG as a DOT document. Deterministic: nodes and edges are
/// emitted in sorted order.
pub fn to_dot(dag: &WorkspaceDag) -> String {
    todo!()
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

        // Labels present
        assert!(dot.contains(r#""@fixture/core" [label="@fixture/core@1.0.0"]"#), "dot was: {dot}");
        assert!(dot.contains(r#""@fixture/app" [label="@fixture/app@1.0.0"]"#));

        // Edges present
        assert!(dot.contains(r#""@fixture/utils" -> "@fixture/core""#));
        assert!(dot.contains(r#""@fixture/ui" -> "@fixture/core""#));
        assert!(dot.contains(r#""@fixture/ui" -> "@fixture/utils""#));
        assert!(dot.contains(r#""@fixture/app" -> "@fixture/ui""#));
        assert!(dot.contains(r#""@fixture/app" -> "@fixture/core""#));

        // Node declarations come before edges
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
```

**Step 2: Run tests to verify they fail**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib dot 2>&1 | tail -15`

Expected: 3 tests fail with `not yet implemented`.

**Step 3: Implement `to_dot`**

Replace the `todo!()` body in `crates/build-graph/src/dot.rs` with:

```rust
pub fn to_dot(dag: &WorkspaceDag) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    writeln!(out, "digraph workspace {{").unwrap();
    writeln!(out, "  rankdir=\"LR\";").unwrap();
    writeln!(
        out,
        "  node [shape=\"box\", style=\"rounded\", fontname=\"monospace\"];"
    ).unwrap();

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
        ).unwrap();
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
```

**Step 4: Run tests to verify they pass**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph --lib dot 2>&1 | tail -10`

Expected: `test result: ok. 3 passed; 0 failed`.

**Step 5: Run the full `build-graph` test suite + clippy**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p build-graph 2>&1 | tail -10`

Expected: 9 tests pass (4 dag + 2 topo + 3 dot), 0 failed.

Run: `cd /Users/ken/workspace/ms/rage && cargo clippy -p build-graph --all-targets -- -D warnings 2>&1 | tail -10`

Expected: `Finished` with no warnings/errors. Fix anything clippy flags before committing.

**Step 6: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add crates/build-graph/src/dot.rs && \
  git commit -m "feat(build-graph): deterministic DOT rendering"
```

---

## Task 5: Create `pipeline-config` Crate

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/Cargo.toml`
- Create: `crates/pipeline-config/Cargo.toml`
- Create: `crates/pipeline-config/src/lib.rs`
- Create: `crates/pipeline-config/src/config.rs`

Minimal scaffold. Phase 1 only needs: parse `rage.json` at workspace root if present, return `None` if absent, error on malformed JSON. The config's actual fields won't be used until later phases, but we need the skeleton to justify the CLI accepting a `--workspace` flag that will later layer config.

**Step 1: Register in workspace**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml`. Change the `members` list to:

```toml
members = [
    "crates/workspace-tools",
    "crates/build-graph",
    "crates/pipeline-config",
]
```

**Step 2: Create the crate manifest**

Write exactly this to `/Users/ken/workspace/ms/rage/crates/pipeline-config/Cargo.toml`:

```toml
[package]
name = "pipeline-config"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
anyhow = "1"
```

**Step 3: Write the failing tests**

Write exactly this to `crates/pipeline-config/src/lib.rs`:

```rust
//! Parse the workspace `rage.json` config file.

pub mod config;

pub use config::{load_config, RageConfig, SandboxConfig, SandboxMode};
```

Write exactly this to `crates/pipeline-config/src/config.rs`:

```rust
//! `rage.json` schema and loader.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    Strict,
    Observed,
    Loose,
}

impl Default for SandboxMode {
    fn default() -> Self {
        Self::Strict
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct SandboxConfig {
    pub default: SandboxMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct RageConfig {
    pub plugins: Vec<String>,
    pub sandbox: SandboxConfig,
}

/// Load `rage.json` from the workspace root. Returns `None` if absent
/// (the config is optional). Returns an error if the file exists but is
/// malformed.
pub fn load_config(workspace_root: &Path) -> Result<Option<RageConfig>> {
    todo!()
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
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
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
        f.write_all(br#"{
            "plugins": ["rage-typescript", "rage-rust"],
            "sandbox": { "default": "observed" }
        }"#).unwrap();
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
}
```

**Step 4: Run tests to verify they fail**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p pipeline-config 2>&1 | tail -15`

Expected: 4 tests fail, 3 with `not yet implemented` and possibly 1 via `is_err()` returning success accidentally — but since `load_config` panics, even `is_err` will panic. You should see 4 failures total.

**Step 5: Implement `load_config`**

Replace the `todo!()` body in `crates/pipeline-config/src/config.rs` with:

```rust
pub fn load_config(workspace_root: &Path) -> Result<Option<RageConfig>> {
    let path = workspace_root.join("rage.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let cfg: RageConfig = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg))
}
```

**Step 6: Run tests to verify they pass**

Run: `cd /Users/ken/workspace/ms/rage && cargo test -p pipeline-config 2>&1 | tail -10`

Expected: `test result: ok. 4 passed; 0 failed`.

**Step 7: Clippy check**

Run: `cd /Users/ken/workspace/ms/rage && cargo clippy -p pipeline-config --all-targets -- -D warnings 2>&1 | tail -10`

Expected: no warnings.

**Step 8: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add Cargo.toml crates/pipeline-config && \
  git commit -m "feat(pipeline-config): rage.json loader skeleton"
```

---

## Task 6: Create `cli` Crate With `rage graph` Command

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/Cargo.toml`
- Create: `crates/cli/Cargo.toml`
- Create: `crates/cli/src/main.rs`

**Step 1: Register in workspace**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml`. Change the `members` list to:

```toml
members = [
    "crates/workspace-tools",
    "crates/build-graph",
    "crates/pipeline-config",
    "crates/cli",
]
```

**Step 2: Create the crate manifest**

Write exactly this to `/Users/ken/workspace/ms/rage/crates/cli/Cargo.toml`:

```toml
[package]
name = "rage-cli"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[[bin]]
name = "rage"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
anyhow = "1"
build-graph = { path = "../build-graph" }
pipeline-config = { path = "../pipeline-config" }
workspace-tools = { path = "../workspace-tools" }
```

**Step 3: Write `main.rs`**

Write exactly this to `crates/cli/src/main.rs`:

```rust
//! `rage` — the rage build system CLI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "rage", version, about = "rage build system")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the workspace package dependency graph as DOT.
    Graph {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Graph { workspace, workspace_pos } => {
            let root = workspace_pos
                .or(workspace)
                .map(|p| p.canonicalize().unwrap_or(p))
                .unwrap_or_else(|| std::env::current_dir().unwrap());
            cmd_graph(&root)
        }
    }
}

fn cmd_graph(root: &Path) -> Result<()> {
    let pm = workspace_tools::detect_package_manager(root)
        .with_context(|| format!(
            "{} is not a recognized JS workspace", root.display()
        ))?;

    let raw = workspace_tools::discover_packages(root)
        .context("discovering workspace packages")?;
    let resolved = workspace_tools::build_package_graph(raw)
        .context("resolving package dependency edges")?;

    eprintln!("Found {} packages ({} workspace)", resolved.len(), pm.as_str());

    let dag = build_graph::dag::build_dag(resolved)
        .context("building package DAG")?;
    let dot = build_graph::dot::to_dot(&dag);
    print!("{dot}");
    Ok(())
}
```

Note we accept both `--workspace <path>` and a positional path arg. The delegation instruction specified `rage graph [--workspace <path>]` but also `rage graph fixtures/js-pnpm` as a usage example — the positional form is what the final verification calls. We support both; the positional takes precedence if both are given.

**Step 4: Verify it compiles**

Run: `cd /Users/ken/workspace/ms/rage && cargo build -p rage-cli 2>&1 | tail -5`

Expected: `Compiling rage-cli v0.0.0 ...` then `Finished ...` with no errors.

**Step 5: Smoke test — the binary runs and shows help**

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage --help 2>&1`

Expected output includes lines mentioning `Usage: rage <COMMAND>` and `graph`.

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage graph --help 2>&1`

Expected output mentions `--workspace` and `[WORKSPACE_POS]`.

**Step 6: Smoke test — `rage graph` on the pnpm fixture emits DOT**

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage graph fixtures/js-pnpm 2>&1 | head -20`

Expected: stderr shows `Found 4 packages (pnpm workspace)` and stdout is a DOT document starting with `digraph workspace {`.

Split stderr from stdout to be sure:

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage graph fixtures/js-pnpm 2>/dev/null | head -5`

Expected:
```
digraph workspace {
  rankdir="LR";
  node [shape="box", style="rounded", fontname="monospace"];
  "@fixture/app" [label="@fixture/app@1.0.0"];
  "@fixture/core" [label="@fixture/core@1.0.0"];
```

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage graph fixtures/js-pnpm 2>&1 >/dev/null`

Expected: `Found 4 packages (pnpm workspace)`.

**Step 7: Commit**

```bash
cd /Users/ken/workspace/ms/rage && \
  git add Cargo.toml crates/cli && \
  git commit -m "feat(cli): rage graph command emits DOT to stdout"
```

---

## Task 7: Final Phase 1 Verification

**Step 1: Full workspace build + test**

Run: `cd /Users/ken/workspace/ms/rage && cargo build --workspace 2>&1 | tail -5`

Expected: `Finished` with no errors.

Run: `cd /Users/ken/workspace/ms/rage && cargo test --workspace 2>&1 | tail -20`

Expected summary: something like `27 passed; 0 failed` spread across `workspace-tools` (14), `build-graph` (9), `pipeline-config` (4). If totals differ by 1–2 that's fine (depending on how cargo counts integration harnesses), but **`0 failed` is non-negotiable**.

**Step 2: Clippy clean across the workspace**

Run: `cd /Users/ken/workspace/ms/rage && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`

Expected: `Finished` with no warnings.

**Step 3: DOT output renders for all three fixtures**

Run each of these and verify exit code is 0 and an SVG was written:

```bash
cd /Users/ken/workspace/ms/rage && \
  ./target/debug/rage graph fixtures/js-pnpm 2>/dev/null | dot -Tsvg -o /tmp/rage-pnpm.svg && \
  echo "pnpm DOT valid, SVG bytes: $(wc -c < /tmp/rage-pnpm.svg)"
```

Expected: prints `pnpm DOT valid, SVG bytes: <some positive number>`.

```bash
cd /Users/ken/workspace/ms/rage && \
  ./target/debug/rage graph fixtures/js-yarn 2>/dev/null | dot -Tsvg -o /tmp/rage-yarn.svg && \
  echo "yarn DOT valid, SVG bytes: $(wc -c < /tmp/rage-yarn.svg)"
```

Expected: prints `yarn DOT valid, SVG bytes: <some positive number>`.

```bash
cd /Users/ken/workspace/ms/rage && \
  ./target/debug/rage graph fixtures/js-npm 2>/dev/null | dot -Tsvg -o /tmp/rage-npm.svg && \
  echo "npm DOT valid, SVG bytes: $(wc -c < /tmp/rage-npm.svg)"
```

Expected: prints `npm DOT valid, SVG bytes: <some positive number>`.

If any of these prints nothing (because `dot` rejected the DOT with a syntax error), STOP and report — the DOT renderer is broken.

**Step 4: Manual sanity check — the DOT structure matches expectations**

Run: `cd /Users/ken/workspace/ms/rage && ./target/debug/rage graph fixtures/js-pnpm 2>/dev/null`

Verify the output contains:
- `digraph workspace {`
- All four `@fixture/...` node declarations with `[label="...@1.0.0"]`
- Edges: `"@fixture/app" -> "@fixture/core"`, `"@fixture/app" -> "@fixture/ui"`, `"@fixture/ui" -> "@fixture/core"`, `"@fixture/ui" -> "@fixture/utils"`, `"@fixture/utils" -> "@fixture/core"` — 5 edges total

**Step 5: Clean up temporary SVGs**

Run: `rm -f /tmp/rage-pnpm.svg /tmp/rage-yarn.svg /tmp/rage-npm.svg`

**Step 6: Check git log**

Run: `cd /Users/ken/workspace/ms/rage && git log --oneline`

Expected: the Phase 1a commits plus 6 new Phase 1b commits:
- `feat(build-graph): scaffold crate`
- `feat(build-graph): WorkspaceDag + build_dag with cycle detection`
- `feat(build-graph): topological_sort dependencies-first`
- `feat(build-graph): deterministic DOT rendering`
- `feat(pipeline-config): rage.json loader skeleton`
- `feat(cli): rage graph command emits DOT to stdout`

**Step 7: Phase 1 complete**

Report completion with:
- Last 5 lines of `cargo test --workspace`
- Last 5 lines of `cargo clippy --workspace --all-targets -- -D warnings`
- Full output of `git log --oneline`
- The first ~10 lines of `./target/debug/rage graph fixtures/js-pnpm 2>/dev/null`
- The stderr line `Found N packages (... workspace)` from each of the three fixtures
- Any deviations from this plan

**Do not start Phase 2.** Phase 2 (scheduler + basic cache) will be planned separately after Phase 1 is reviewed.

---

## Failure Triage

1. **`petgraph` API mismatch:** if `is_cyclic_directed` or `toposort` signatures differ in the version cargo resolves, adjust calls to match the actual API. The import paths in this plan target `petgraph = "0.6"`. Run `cargo doc --open -p petgraph` if you need to verify.
2. **DOT renders but `dot -Tsvg` errors:** double-check the DOT text — common mistakes are stray trailing commas, unquoted identifiers with special characters, or missing semicolons. Run `dot -Tsvg < /tmp/test.dot` with the output captured to a file to see the exact error.
3. **Clippy warnings in generated code:** fix them. Common ones: prefer `write!` over `format!` for push-to-String, remove unused imports, use `.unwrap_or_else` where applicable. Do NOT `#[allow(...)]` your way around them.
4. **`cargo test --workspace` counts don't match:** close enough is fine. Only **`0 failed`** matters.
5. **Unsure how to proceed:** STOP and ask. Do not invent behavior not in this plan.
