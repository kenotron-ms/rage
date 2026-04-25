//! Task definition and workspace-to-task-list conversion.

use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use std::path::PathBuf;
use thiserror::Error;

/// A single unit of work: run `script_name` for `package_name`.
#[derive(Debug, Clone)]
pub struct Task {
    /// Package name, e.g. `@fixture/core`
    pub package_name: String,
    /// Script name to run, e.g. `build`
    pub script_name: String,
    /// Shell command from `package.json` scripts[script_name]
    pub command: String,
    /// Working directory (package root)
    pub cwd: PathBuf,
    /// Sandbox mode to apply when executing this task
    pub sandbox_mode: pipeline_config::SandboxMode,
}

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("no packages have a '{0}' script in this workspace")]
    NoMatchingScript(String),
}

/// Build a task list for `script_name` from the workspace DAG.
///
/// - Returns tasks in topological order (dependencies before dependents).
/// - Packages without `scripts.{script_name}` in their `package.json` are silently skipped.
/// - Returns `TaskError::NoMatchingScript` if no package has the script.
pub fn build_task_list(dag: &WorkspaceDag, script_name: &str) -> Result<Vec<Task>, TaskError> {
    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    let mut tasks: Vec<Task> = Vec::new();

    for pkg_name in &order {
        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

        // Try to read package.json and extract scripts[script_name]
        let manifest_path = pkg.path.join("package.json");
        let command = if let Ok(raw) = std::fs::read_to_string(&manifest_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                json.get("scripts")
                    .and_then(|s| s.get(script_name))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(cmd) = command {
            tasks.push(Task {
                package_name: pkg_name.clone(),
                script_name: script_name.to_string(),
                command: cmd,
                cwd: pkg.path.clone(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
            });
        }
        // else: silently skip packages without the script
    }

    if tasks.is_empty() {
        return Err(TaskError::NoMatchingScript(script_name.to_string()));
    }

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;
    use workspace_tools::Package;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap() // crates/
            .parent()
            .unwrap() // workspace root
            .join("fixtures")
    }

    fn mk(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from("/tmp").join(name),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_matching_script_is_an_error() {
        // In-memory packages with no filesystem backing; path doesn't exist
        let packages = vec![mk("a", &[]), mk("b", &["a"])];
        let dag = build_dag(packages).unwrap();
        let err = build_task_list(&dag, "build").unwrap_err();
        assert!(matches!(err, TaskError::NoMatchingScript(_)));
        assert!(err.to_string().contains("build"));
    }

    #[test]
    fn finds_build_tasks_in_pnpm_fixture() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        // 4 packages in the fixture, all have build scripts
        assert_eq!(tasks.len(), 4);
        // All tasks reference script_name = "build"
        assert!(tasks.iter().all(|t| t.script_name == "build"));
        // All commands are non-empty
        assert!(tasks.iter().all(|t| !t.command.is_empty()));
        // Dependencies come before dependents:
        let pos = |name: &str| tasks.iter().position(|t| t.package_name == name).unwrap();
        assert!(pos("@fixture/core") < pos("@fixture/utils"));
        assert!(pos("@fixture/core") < pos("@fixture/ui"));
        assert!(pos("@fixture/utils") < pos("@fixture/ui"));
        assert!(pos("@fixture/ui") < pos("@fixture/app"));
    }

    #[test]
    fn skips_packages_without_the_script() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        // "test" script is not defined in any fixture package
        let err = build_task_list(&dag, "test").unwrap_err();
        assert!(matches!(err, TaskError::NoMatchingScript(_)));
    }

    #[test]
    fn task_carries_sandbox_mode() {
        let t = Task {
            package_name: "x".to_string(),
            script_name: "build".to_string(),
            command: "echo".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Strict,
        };
        assert_eq!(t.sandbox_mode, pipeline_config::SandboxMode::Strict);
    }

    #[test]
    fn task_fields_are_populated_correctly() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        let core = tasks
            .iter()
            .find(|t| t.package_name == "@fixture/core")
            .unwrap();
        assert_eq!(core.script_name, "build");
        assert!(
            core.command.contains("@fixture/core"),
            "command should reference the package"
        );
        assert!(core.cwd.ends_with("core"));
        assert!(core.cwd.is_absolute());
    }
}
