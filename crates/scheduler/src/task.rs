//! Task definition and workspace-to-task-list conversion.

use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use std::path::PathBuf;
use thiserror::Error;

/// A single unit of work: run `script_name` for `package_name`.
#[derive(Debug, Clone)]
pub struct Task {
    /// Package name, e.g. `@fixture/core`. For root tasks this is `"workspace"`.
    pub package_name: String,
    /// Script name to run, e.g. `build`. For root tasks this is the
    /// `name` field of the originating `RootTask` (e.g. `"install"`).
    pub script_name: String,
    /// Shell command to execute. For root tasks this comes verbatim from
    /// `RootTask::command` (e.g. `"pnpm install"`).
    pub command: String,
    /// Working directory. Package root for normal tasks; workspace root for root tasks.
    pub cwd: PathBuf,
    /// Sandbox mode to apply when executing this task.
    pub sandbox_mode: pipeline_config::SandboxMode,
    /// `true` for synthetic workspace-level tasks (e.g. `workspace#install`).
    /// Root tasks always run alone in wave 0 before any package task.
    pub is_root: bool,
    /// Files whose contents are hashed to fingerprint a root task
    /// (e.g. `pnpm-lock.yaml`). Empty for non-root tasks.
    pub input_paths: Vec<PathBuf>,
    /// Workspace root directory — used to build `{workspace_root}/node_modules/.bin`
    /// PATH prefix so locally-installed tools are found.
    pub workspace_root: PathBuf,
    /// Input globs (relative to package root) declared by the ecosystem plugin.
    /// Fed into the weak fingerprint so source changes invalidate the WF hash.
    /// Empty for root tasks (they use `input_paths` instead).
    pub declared_input_globs: Vec<String>,
    /// Immediate dependency package names (from the workspace DAG).
    /// Used to look up upstream ABI fingerprints for the early-cutoff mechanism:
    /// if all deps' ABI fingerprints are unchanged, this task's WF is unchanged.
    pub dep_package_names: Vec<String>,
    /// Output globs (relative to package root) produced by this task.
    /// Used to locate output files for ABI fingerprinting after a successful run.
    /// Empty for root tasks.
    pub output_globs: Vec<String>,
    /// Extra (key, value) pairs hashed alongside `input_paths` for root tasks.
    /// Empty for non-root tasks. Sourced from `RootTask::env_hash_inputs`.
    pub env_hash_inputs: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("no packages have a '{0}' script in this workspace")]
    NoMatchingScript(String),
}

/// Check whether `plugin`'s detection globs match any direct child of `pkg_path`.
///
/// Uses simple direct-file-existence checks for globs without wildcards, and a
/// manual scan + filename match for globs containing `*`. This avoids adding
/// `globset` as a scheduler dependency.
fn package_matches_plugin(
    pkg_path: &std::path::Path,
    plugin: &dyn plugin::EcosystemPlugin,
) -> bool {
    for glob_str in plugin.detection_globs() {
        if glob_str.contains('*') {
            // Wildcard glob: scan directory and check each entry
            if let Ok(entries) = std::fs::read_dir(pkg_path) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if simple_glob_match(glob_str, name) {
                            return true;
                        }
                    }
                }
            }
        } else {
            // Literal filename: direct existence check
            if pkg_path.join(glob_str).exists() {
                return true;
            }
        }
    }
    false
}

/// Minimal glob matcher: supports `*` as "zero or more non-separator chars".
/// Sufficient for patterns like `tsconfig.*.json`.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    // Split pattern on '*' and do prefix/suffix/infix matching.
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut s = name;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // Must be a prefix
            if !s.starts_with(part) {
                return false;
            }
            s = &s[part.len()..];
        } else if i == parts.len() - 1 {
            // Must be a suffix
            return s.ends_with(part);
        } else {
            // Must appear somewhere in s
            if let Some(pos) = s.find(part) {
                s = &s[pos + part.len()..];
            } else {
                return false;
            }
        }
    }
    true
}

/// Build a task list for `script_name` from the workspace DAG.
///
/// For each plugin in `plugins`, calls `plugin.infer_root_tasks(workspace_root)`
/// and prepends every returned root task as a synthesized
/// `workspace#<root_task.name>` `Task` (with `is_root: true`). The scheduler
/// itself contains zero package-manager-specific knowledge — that lives in
/// the plugin.
///
/// - Returns tasks in topological order: first all root tasks (in plugin order),
///   then package tasks ordered by the DAG.
/// - Packages without `scripts.{script_name}` are silently skipped.
/// - Returns `TaskError::NoMatchingScript` if **no package** has the script —
///   even if root tasks were synthesized. (A workspace where nobody declares
///   the script is still an error: the user typo'd the script name.)
pub fn build_task_list(
    dag: &WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
) -> Result<Vec<Task>, TaskError> {
    build_task_list_filtered(dag, script_name, workspace_root, plugins, &[])
}

/// Internal implementation: builds the task list, skipping any package whose
/// name appears in `skip_packages`.
fn build_task_list_filtered(
    dag: &WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
    skip_packages: &[String],
) -> Result<Vec<Task>, TaskError> {
    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    // 1. Collect root tasks from every plugin (in plugin order, stable).
    let mut tasks: Vec<Task> = Vec::new();
    for p in plugins {
        for rt in p.infer_root_tasks(workspace_root) {
            tasks.push(Task {
                package_name: "workspace".to_string(),
                script_name: rt.name,
                command: rt.command,
                cwd: workspace_root.to_path_buf(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: true,
                input_paths: rt.input_paths,
                workspace_root: workspace_root.to_path_buf(),
                declared_input_globs: Vec::new(), // root tasks use input_paths, not globs
                dep_package_names: Vec::new(),    // root tasks have no package deps
                output_globs: Vec::new(),         // root tasks don't have output globs
                env_hash_inputs: rt.env_hash_inputs,
            });
        }
    }

    // 2. Walk the package DAG and synthesize per-package tasks for `script_name`.
    let mut package_tasks_added = 0usize;
    for pkg_name in &order {
        // Skip packages explicitly excluded via pipeline config.
        if skip_packages.iter().any(|s| s == pkg_name) {
            continue;
        }

        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

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
            // Collect input globs from all plugins that apply to this package.
            let mut globs: std::collections::HashSet<String> = std::collections::HashSet::new();
            for p in plugins {
                if package_matches_plugin(&pkg.path, *p) {
                    for g in p.declared_input_globs(script_name, &plugin::PluginConfig::default()) {
                        globs.insert(g);
                    }
                }
            }
            let mut declared_input_globs: Vec<String> = globs.into_iter().collect();
            declared_input_globs.sort(); // deterministic order

            // Collect output globs from matching plugins (for ABI fingerprinting)
            let mut out_globs: std::collections::HashSet<String> = std::collections::HashSet::new();
            for p in plugins {
                if package_matches_plugin(&pkg.path, *p) {
                    for td in p.infer_tasks(&pkg.path) {
                        if td.name == script_name {
                            out_globs.extend(td.output_globs.iter().cloned());
                        }
                    }
                }
            }
            let mut output_globs: Vec<String> = out_globs.into_iter().collect();
            output_globs.sort();

            tasks.push(Task {
                package_name: pkg_name.clone(),
                script_name: script_name.to_string(),
                command: cmd,
                cwd: pkg.path.clone(),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: false,
                input_paths: Vec::new(),
                workspace_root: workspace_root.to_path_buf(),
                declared_input_globs,
                dep_package_names: pkg.dependencies.clone(),
                output_globs,
                env_hash_inputs: Vec::new(),
            });
            package_tasks_added += 1;
        }
    }

    if package_tasks_added == 0 {
        return Err(TaskError::NoMatchingScript(script_name.to_string()));
    }

    Ok(tasks)
}

/// Build a task list with sandbox modes resolved against `RageConfig` policies.
///
/// `workspace_root` is used to compute each package's path relative to the
/// workspace for glob policy matching.
///
/// Root tasks (`is_root: true`) ALWAYS receive `SandboxMode::Loose`, regardless
/// of policy. Sandboxing the package manager would block legitimate writes to
/// `node_modules`.
pub fn build_task_list_with_config(
    dag: &build_graph::dag::WorkspaceDag,
    script_name: &str,
    workspace_root: &std::path::Path,
    plugins: &[&dyn plugin::EcosystemPlugin],
    config: &pipeline_config::RageConfig,
) -> Result<Vec<Task>, TaskError> {
    let skip = config
        .pipeline
        .get(script_name)
        .map(|p| p.skip_packages.as_slice())
        .unwrap_or(&[]);
    let mut tasks = build_task_list_filtered(dag, script_name, workspace_root, plugins, skip)?;
    for task in &mut tasks {
        if task.is_root {
            // Root tasks bypass per-package sandbox policy — see COE constraint #2.
            task.sandbox_mode = pipeline_config::SandboxMode::Loose;
            continue;
        }
        let rel = task
            .cwd
            .strip_prefix(workspace_root)
            .unwrap_or(&task.cwd)
            .to_path_buf();
        task.sandbox_mode = pipeline_config::resolve_sandbox_mode(config, &rel);
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
        let dummy_root = PathBuf::from("/tmp");
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let err = build_task_list(&dag, "build", &dummy_root, &plugins).unwrap_err();
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
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let tasks = build_task_list(&dag, "build", &root, &plugins).unwrap();
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
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let err = build_task_list(&dag, "test", &root, &plugins).unwrap_err();
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
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };
        assert_eq!(t.sandbox_mode, pipeline_config::SandboxMode::Strict);
    }

    #[test]
    fn task_list_with_config_resolves_sandbox_per_policy() {
        use pipeline_config::{CacheConfig, Policy, RageConfig, SandboxConfig, SandboxMode};
        use std::collections::HashMap;
        use workspace_tools::{build_package_graph, discover_packages};

        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();

        let cfg = RageConfig {
            plugins: vec![],
            sandbox: SandboxConfig {
                default: SandboxMode::Observed,
            },
            cache: CacheConfig::default(),
            policies: vec![Policy {
                selector: "packages/core/**".to_string(),
                sandbox: Some(SandboxMode::Strict),
            }],
            plugins_config: HashMap::new(),
            pipeline: HashMap::new(),
            max_concurrency: None,
        };

        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let tasks = build_task_list_with_config(&dag, "build", &root, &plugins, &cfg).unwrap();
        let core = tasks
            .iter()
            .find(|t| t.package_name == "@fixture/core")
            .unwrap();
        let utils = tasks
            .iter()
            .find(|t| t.package_name == "@fixture/utils")
            .unwrap();
        assert_eq!(core.sandbox_mode, SandboxMode::Strict);
        assert_eq!(utils.sandbox_mode, SandboxMode::Observed);
    }

    #[test]
    fn skip_packages_excludes_named_package() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();

        let skip = vec!["@fixture/core".to_string()];
        let tasks =
            build_task_list_filtered(&dag, "build", &root, &plugins, &skip).unwrap();

        assert!(
            tasks.iter().all(|t| t.package_name != "@fixture/core"),
            "skipped package should not appear in task list"
        );
        // The other three packages should still be present.
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn skip_packages_via_config() {
        use pipeline_config::{CacheConfig, PipelineTaskConfig, RageConfig, SandboxConfig};
        use std::collections::HashMap;
        use workspace_tools::{build_package_graph, discover_packages};

        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();

        let mut pipeline = HashMap::new();
        pipeline.insert(
            "build".to_string(),
            PipelineTaskConfig {
                skip_packages: vec!["@fixture/core".to_string()],
            },
        );
        let cfg = RageConfig {
            plugins: vec![],
            sandbox: SandboxConfig::default(),
            cache: CacheConfig::default(),
            policies: vec![],
            plugins_config: HashMap::new(),
            pipeline,
                    max_concurrency: None,
        };

        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let tasks =
            build_task_list_with_config(&dag, "build", &root, &plugins, &cfg).unwrap();

        assert!(
            tasks.iter().all(|t| t.package_name != "@fixture/core"),
            "config skip_packages should exclude the named package"
        );
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn task_fields_are_populated_correctly() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let tasks = build_task_list(&dag, "build", &root, &plugins).unwrap();
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

    #[test]
    fn pnpm_workspace_includes_root_install_first() {
        use plugin::EcosystemPlugin;
        use plugin_typescript::TypeScriptPlugin;
        use tempfile::tempdir;
        use workspace_tools::{build_package_graph, discover_packages};

        // Stage a pnpm workspace inside a tempdir so we control whether
        // the lockfile exists. Copy the four js-pnpm packages over.
        let work = tempdir().unwrap();
        let src = fixtures_dir().join("js-pnpm");
        // Recursively copy fixture into tempdir so it's mutable.
        copy_dir_recursive(&src, work.path());
        // Stage a lockfile so the TypeScript plugin will detect pnpm.
        std::fs::write(work.path().join("pnpm-lock.yaml"), b"lockfileVersion: 6\n").unwrap();

        let raw = discover_packages(work.path()).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();

        let ts = TypeScriptPlugin::new();
        let plugins: Vec<&dyn EcosystemPlugin> = vec![&ts];

        let tasks = build_task_list(&dag, "build", work.path(), &plugins).unwrap();

        // First task is the synthesized workspace#install root task.
        assert!(tasks[0].is_root, "first task must be flagged is_root");
        assert_eq!(tasks[0].package_name, "workspace");
        assert_eq!(tasks[0].script_name, "install");
        assert_eq!(tasks[0].command, "pnpm install");
        assert_eq!(tasks[0].cwd, work.path());
        assert_eq!(
            tasks[0].input_paths,
            vec![work.path().join("pnpm-lock.yaml")]
        );

        // Followed by 4 package build tasks, none flagged is_root.
        assert_eq!(tasks.len(), 5, "1 install + 4 package builds");
        assert!(tasks[1..].iter().all(|t| !t.is_root));
    }

    /// Recursively copy a directory tree. Test helper only.
    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap().flatten() {
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir_recursive(&from, &to);
            } else {
                std::fs::copy(&from, &to).unwrap();
            }
        }
    }
}
