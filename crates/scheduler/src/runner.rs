//! Wave-parallel task execution using Tokio.

use crate::task::Task;
use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use std::collections::HashMap;
use std::time::Instant;
use thiserror::Error;
use tokio::process::Command;
use tokio::task::JoinSet;

#[derive(Debug, Error)]
pub enum RunError {
    #[error("task {package}#{script} failed with exit code {code}")]
    TaskFailed {
        package: String,
        script: String,
        code: i32,
    },
    #[error("task {package}#{script} was terminated by signal")]
    Killed { package: String, script: String },
    #[error("failed to spawn task {package}#{script}: {source}")]
    Spawn {
        package: String,
        script: String,
        #[source]
        source: std::io::Error,
    },
}

/// Group tasks into parallel execution waves.
///
/// A task is placed in wave N where:
///   N = 1 + max(wave of deps that also have tasks), or 0 if no deps have tasks.
///
/// Within a wave, tasks are sorted by package name for determinism.
pub fn compute_task_levels(dag: &WorkspaceDag, tasks: &[Task]) -> Vec<Vec<Task>> {
    // Build lookup: package_name -> task
    let task_map: HashMap<&str, &Task> =
        tasks.iter().map(|t| (t.package_name.as_str(), t)).collect();

    // Get topo order (deps first)
    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    let mut level_of: HashMap<&str, usize> = HashMap::new();
    let mut levels: Vec<Vec<Task>> = Vec::new();

    for pkg_name in &order {
        if !task_map.contains_key(pkg_name.as_str()) {
            continue;
        }

        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

        // Level = 1 + max level of deps that have tasks; or 0
        let level = pkg
            .dependencies
            .iter()
            .filter(|dep| task_map.contains_key(dep.as_str()))
            .filter_map(|dep| level_of.get(dep.as_str()).copied())
            .max()
            .map(|max_dep_level| max_dep_level + 1)
            .unwrap_or(0);

        level_of.insert(pkg_name.as_str(), level);

        if level >= levels.len() {
            levels.resize_with(level + 1, Vec::new);
        }
        levels[level].push((*task_map[pkg_name.as_str()]).clone());
    }

    // Sort within each level for determinism
    for level in &mut levels {
        level.sort_by(|a, b| a.package_name.cmp(&b.package_name));
    }

    levels
}

/// Execute tasks in wave-parallel order using Tokio.
///
/// For each wave, all tasks run concurrently. Waves run sequentially.
/// On any task failure, the wave is aborted and the error is returned.
pub async fn run_tasks(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> anyhow::Result<()> {
    let levels = compute_task_levels(dag, &tasks);

    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();

        for task in level {
            let cache_clone = cache.clone();
            set.spawn(run_single_task(task, cache_clone));
        }

        let mut first_error: Option<RunError> = None;

        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    set.abort_all();
                }
                Err(_join_err) => {
                    if first_error.is_none() {
                        first_error = Some(RunError::Killed {
                            package: "unknown".to_string(),
                            script: "unknown".to_string(),
                        });
                    }
                }
            }
        }

        if let Some(e) = first_error {
            return Err(e.into());
        }
    }

    Ok(())
}

async fn run_single_task(
    task: Task,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> Result<(), RunError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Compute fingerprint if cache is provided
    let fingerprint = cache
        .as_ref()
        .and_then(|_| cache::fingerprint_task(&task.command, &task.cwd).ok());

    // Check cache — on hit, print and return early
    if let (Some(fp), Some(c)) = (&fingerprint, &cache) {
        if c.get(fp).is_some() {
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
    }

    // Cache miss (or no cache) — execute the task
    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
    let start = Instant::now();

    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .status()
        .await
        .map_err(|e| RunError::Spawn {
            package: task.package_name.clone(),
            script: task.script_name.clone(),
            source: e,
        })?;

    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let elapsed_ms = elapsed.as_millis() as u64;

    if status.success() {
        // Store in cache on success
        if let (Some(fp), Some(c)) = (&fingerprint, &cache) {
            let entry = cache::CacheEntry {
                fingerprint: fp.clone(),
                command: task.command.clone(),
                exit_code: 0,
                elapsed_ms,
                cached_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            let _ = c.put(fp, &entry); // ignore cache write errors
        }
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name, task.script_name, elapsed_secs
        );
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        eprintln!(
            "[rage] {}#{} \u{2717} FAILED (exit {code})",
            task.package_name, task.script_name
        );
        Err(RunError::TaskFailed {
            package: task.package_name,
            script: task.script_name,
            code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::build_task_list;
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

    fn mk_task(name: &str) -> Task {
        Task {
            package_name: name.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {name}"),
            cwd: PathBuf::from(format!("/tmp/{name}")),
            sandbox_mode: pipeline_config::SandboxMode::default(),
        }
    }

    fn mk_pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            path: PathBuf::from(format!("/tmp/{name}")),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── compute_task_levels tests ──────────────────────────────────────────

    #[test]
    fn single_package_is_level_zero() {
        let task = mk_task("a");
        let pkg = mk_pkg("a", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        let levels = compute_task_levels(&dag, &[task]);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[0][0].package_name, "a");
    }

    #[test]
    fn linear_chain_is_separate_levels() {
        let tasks: Vec<Task> = ["a", "b", "c"].iter().map(|n| mk_task(n)).collect();
        let packages = vec![mk_pkg("a", &[]), mk_pkg("b", &["a"]), mk_pkg("c", &["b"])];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 3, "linear chain → 3 levels");
        assert_eq!(levels[0][0].package_name, "a");
        assert_eq!(levels[1][0].package_name, "b");
        assert_eq!(levels[2][0].package_name, "c");
    }

    #[test]
    fn diamond_graph_correct_levels() {
        let tasks: Vec<Task> = ["core", "utils", "ui", "app"]
            .iter()
            .map(|n| mk_task(n))
            .collect();
        let packages = vec![
            mk_pkg("core", &[]),
            mk_pkg("utils", &["core"]),
            mk_pkg("ui", &["core", "utils"]),
            mk_pkg("app", &["ui", "core"]),
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // core: L0, utils: L1 (dep on core), ui: L2 (dep on utils), app: L3 (dep on ui)
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0][0].package_name, "core");
        assert_eq!(levels[1][0].package_name, "utils");
        assert_eq!(levels[2][0].package_name, "ui");
        assert_eq!(levels[3][0].package_name, "app");
    }

    #[test]
    fn independent_packages_share_level() {
        let tasks: Vec<Task> = ["a", "b"].iter().map(|n| mk_task(n)).collect();
        let packages = vec![mk_pkg("a", &[]), mk_pkg("b", &[])];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(levels.len(), 1, "two independent packages → 1 level");
        assert_eq!(levels[0].len(), 2, "both tasks in level 0");
    }

    #[test]
    fn pnpm_fixture_has_four_levels() {
        use workspace_tools::{build_package_graph, discover_packages};
        let root = fixtures_dir().join("js-pnpm");
        let raw = discover_packages(&root).unwrap();
        let resolved = build_package_graph(raw).unwrap();
        let dag = build_dag(resolved).unwrap();
        let tasks = build_task_list(&dag, "build").unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // core(L0) → utils(L1) → ui(L2) → app(L3)
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0].len(), 1); // core
        assert_eq!(levels[0][0].package_name, "@fixture/core");
        assert_eq!(levels[3].len(), 1); // app
        assert_eq!(levels[3][0].package_name, "@fixture/app");
    }

    // ── run_tasks tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_successful_task_runs() {
        let task = Task {
            package_name: "test-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo hello".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::default(),
        };
        let pkg = mk_pkg("test-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        run_tasks(&dag, vec![task], None).await.unwrap();
    }

    #[tokio::test]
    async fn failing_task_returns_error() {
        let task = Task {
            package_name: "failing-pkg".to_string(),
            script_name: "build".to_string(),
            command: "false".to_string(), // exits with code 1
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::default(),
        };
        let pkg = mk_pkg("failing-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        let err = run_tasks(&dag, vec![task], None).await.unwrap_err();
        assert!(err.to_string().contains("failing-pkg"));
    }

    #[tokio::test]
    async fn two_independent_tasks_both_run() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        let cmd_a = format!("touch '{}'", file_a.display());
        let cmd_b = format!("touch '{}'", file_b.display());

        let tasks = vec![
            Task {
                package_name: "a".to_string(),
                script_name: "build".to_string(),
                command: cmd_a,
                cwd: PathBuf::from("/tmp"),
                sandbox_mode: pipeline_config::SandboxMode::default(),
            },
            Task {
                package_name: "b".to_string(),
                script_name: "build".to_string(),
                command: cmd_b,
                cwd: PathBuf::from("/tmp"),
                sandbox_mode: pipeline_config::SandboxMode::default(),
            },
        ];
        let packages = vec![mk_pkg("a", &[]), mk_pkg("b", &[])];
        let dag = build_dag(packages).unwrap();
        run_tasks(&dag, tasks, None).await.unwrap();
        assert!(file_a.exists(), "task a should have run");
        assert!(file_b.exists(), "task b should have run");
    }

    #[tokio::test]
    async fn task_is_cached_on_second_run() {
        use cache::LocalCache;
        use std::sync::Arc;
        use tempfile::tempdir;

        let cache_dir = tempdir().unwrap();
        let local = LocalCache::with_dir(cache_dir.path().to_path_buf()).unwrap();
        let cache: Option<Arc<dyn cache::CacheProvider>> = Some(Arc::new(local));

        let pkg_dir = tempdir().unwrap();
        let task = Task {
            package_name: "cached-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo cached-test".to_string(),
            cwd: pkg_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::default(),
        };
        let pkg = mk_pkg("cached-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        // First run — should execute and write to cache
        run_tasks(&dag, vec![task.clone()], cache.clone())
            .await
            .unwrap();

        // Verify a cache entry was written (check cache_dir has at least one .json file)
        let json_files: Vec<_> = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        assert!(
            !json_files.is_empty(),
            "cache entry should have been written"
        );

        // Second run — should be a cache hit (same fingerprint)
        run_tasks(&dag, vec![task], cache).await.unwrap();
    }

    #[tokio::test]
    async fn no_cache_option_executes_normally() {
        let task = Task {
            package_name: "uncached-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo no-cache-test".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::default(),
        };
        let pkg = mk_pkg("uncached-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        // None = no cache — should just execute
        run_tasks(&dag, vec![task], None).await.unwrap();
    }
}
