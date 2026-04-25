//! Wave-parallel task execution using Tokio.

use crate::task::Task;
use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
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

/// Build a PATH value that prepends the two node_modules/.bin dirs for this task.
///
/// Prepend order (highest priority first):
///   1. `{cwd}/node_modules/.bin`             (package-local binaries)
///   2. `{workspace_root}/node_modules/.bin`   (workspace-root binaries, if different)
///   3. existing system `PATH`
///
/// If `cwd` and `workspace_root` resolve to the same directory the bin dir is
/// only included once (no duplicate entries).
///
/// # Why not std::env::join_paths?
///
/// `join_paths` fails if any component contains the path separator (':' on Unix).
/// The existing PATH value already contains colons, so it cannot be passed as a
/// single segment. Instead we concatenate manually using OS-specific separator.
fn node_bin_path(cwd: &std::path::Path, workspace_root: &std::path::Path) -> std::ffi::OsString {
    let pkg_bin = cwd.join("node_modules/.bin");
    let ws_bin = workspace_root.join("node_modules/.bin");
    let existing = std::env::var_os("PATH").unwrap_or_default();

    // Build extra-dirs list, deduplicating when cwd == workspace_root.
    let mut extra: Vec<std::path::PathBuf> = vec![pkg_bin.clone()];
    if ws_bin != pkg_bin {
        extra.push(ws_bin);
    }

    // Manually concatenate with the OS path separator so we don't break on
    // existing PATH values that already contain the separator character.
    #[cfg(unix)]
    const SEP: &str = ":";
    #[cfg(windows)]
    const SEP: &str = ";";

    let mut result = std::ffi::OsString::new();
    for dir in &extra {
        result.push(dir.as_os_str());
        result.push(SEP);
    }
    result.push(&existing);
    result
}

/// Compute a content-addressed fingerprint for a root task.
///
/// Hashes the command plus the contents of every path in `task.input_paths`.
/// Missing files are folded in as `missing:{path}\0` so the fingerprint
/// remains deterministic across runs.
pub(crate) fn root_task_fingerprint(task: &Task) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rage.root-task.v1\0");
    hasher.update(task.command.as_bytes());
    hasher.update(b"\0");
    for path in &task.input_paths {
        match std::fs::read(path) {
            Ok(bytes) => {
                hasher.update(b"present:");
                hasher.update(path.to_string_lossy().as_bytes());
                hasher.update(b"\0");
                hasher.update(&bytes);
            }
            Err(_) => {
                hasher.update(b"missing:");
                hasher.update(path.to_string_lossy().as_bytes());
                hasher.update(b"\0");
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

/// Group tasks into parallel execution waves.
///
/// Root tasks (`is_root: true`) are placed alone in wave 0 — they are not in
/// the package DAG and run before any package task. All package tasks shift
/// down by one wave when at least one root task is present.
///
/// Within a wave, tasks are sorted by package name for determinism.
pub fn compute_task_levels(dag: &WorkspaceDag, tasks: &[Task]) -> Vec<Vec<Task>> {
    // Partition: root tasks live in their own wave 0; package tasks go through
    // the normal topological levelling pass.
    let (root_tasks, package_tasks): (Vec<&Task>, Vec<&Task>) =
        tasks.iter().partition(|t| t.is_root);

    let task_map: HashMap<&str, &Task> = package_tasks
        .iter()
        .map(|t| (t.package_name.as_str(), *t))
        .collect();

    let order = topological_sort(dag).expect("DAG is acyclic by construction");

    let mut level_of: HashMap<&str, usize> = HashMap::new();
    let mut package_levels: Vec<Vec<Task>> = Vec::new();

    for pkg_name in &order {
        if !task_map.contains_key(pkg_name.as_str()) {
            continue;
        }

        let pkg = match dag.packages.get(pkg_name) {
            Some(p) => p,
            None => continue,
        };

        let level = pkg
            .dependencies
            .iter()
            .filter(|dep| task_map.contains_key(dep.as_str()))
            .filter_map(|dep| level_of.get(dep.as_str()).copied())
            .max()
            .map(|max_dep_level| max_dep_level + 1)
            .unwrap_or(0);

        level_of.insert(pkg_name.as_str(), level);

        if level >= package_levels.len() {
            package_levels.resize_with(level + 1, Vec::new);
        }
        package_levels[level].push((*task_map[pkg_name.as_str()]).clone());
    }

    for level in &mut package_levels {
        level.sort_by(|a, b| a.package_name.cmp(&b.package_name));
    }

    // Prepend the root-task wave when there are any root tasks.
    if root_tasks.is_empty() {
        package_levels
    } else {
        let mut root_wave: Vec<Task> = root_tasks.into_iter().cloned().collect();
        root_wave.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        let mut out = Vec::with_capacity(package_levels.len() + 1);
        out.push(root_wave);
        out.extend(package_levels);
        out
    }
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
    if task.is_root {
        return run_root_task_legacy(task, cache).await;
    }

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
    eprintln!(
        "[rage] {}#{} starting [sandbox={:?}]",
        task.package_name, task.script_name, task.sandbox_mode
    );
    let start = Instant::now();

    let new_path = node_bin_path(&task.cwd, &task.workspace_root);
    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .env("PATH", &new_path)
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
                pathset_reads: vec![],
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

async fn run_root_task_legacy(
    task: Task,
    cache: Option<std::sync::Arc<dyn cache::CacheProvider>>,
) -> Result<(), RunError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let fp = root_task_fingerprint(&task);

    // Cache hit?
    if let Some(c) = &cache {
        if c.get(&fp).is_some() {
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
    }

    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
    let start = Instant::now();
    let new_path = node_bin_path(&task.cwd, &task.workspace_root);
    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .env("PATH", &new_path)
        .status()
        .await
        .map_err(|e| RunError::Spawn {
            package: task.package_name.clone(),
            script: task.script_name.clone(),
            source: e,
        })?;
    let elapsed = start.elapsed();

    if status.success() {
        if let Some(c) = &cache {
            let entry = cache::CacheEntry {
                fingerprint: fp.clone(),
                command: task.command.clone(),
                exit_code: 0,
                elapsed_ms: elapsed.as_millis() as u64,
                cached_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                pathset_reads: vec![],
            };
            let _ = c.put(&fp, &entry);
        }
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name,
            task.script_name,
            elapsed.as_secs_f64()
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

/// Resolve the tool path from the first token of `command`.
///
/// Search order (highest priority first):
///   1. `{cwd}/node_modules/.bin/{token}`            (package-local)
///   2. `{workspace_root}/node_modules/.bin/{token}` (workspace-root)
///   3. Directories in the system `PATH` environment variable
///
/// If the first token contains `/` it is used as-is (absolute or relative path).
/// Returns `None` if the token cannot be found anywhere.
fn which_first(
    command: &str,
    cwd: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Option<PathBuf> {
    let first = command.split_whitespace().next()?;
    if first.contains('/') {
        return Some(PathBuf::from(first));
    }

    // 1. Package-local node_modules/.bin
    let pkg_bin = cwd.join("node_modules/.bin").join(first);
    if pkg_bin.is_file() {
        return Some(pkg_bin);
    }

    // 2. Workspace-root node_modules/.bin (skip if same as cwd)
    if workspace_root != cwd {
        let ws_bin = workspace_root.join("node_modules/.bin").join(first);
        if ws_bin.is_file() {
            return Some(ws_bin);
        }
    }

    // 3. System PATH
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(first);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Execute tasks in wave-parallel order using `TwoPhaseCache`.
///
/// For each wave, all tasks run concurrently. Waves run sequentially.
/// On any task failure, the wave is aborted via `JoinSet::abort_all` and the
/// error is returned.
pub async fn run_tasks_two_phase(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Arc<cache::TwoPhaseCache>,
) -> anyhow::Result<()> {
    let levels = compute_task_levels(dag, &tasks);

    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();

        for task in level {
            let cache_clone = cache.clone();
            set.spawn(run_single_task_two_phase(task, cache_clone));
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

async fn run_single_task_two_phase(
    task: Task,
    cache: Arc<cache::TwoPhaseCache>,
) -> Result<(), RunError> {
    if task.is_root {
        return run_root_task_two_phase(task, cache).await;
    }

    use cache::pathset_store::StoredPathset;
    use cache::{CacheEntry, WeakFpInputs};
    use std::time::{SystemTime, UNIX_EPOCH};

    let tool_path = which_first(&task.command, &task.cwd, &task.workspace_root)
        .unwrap_or_else(|| PathBuf::from("sh"));

    let inputs = WeakFpInputs {
        command: &task.command,
        tool_path: &tool_path,
        package_path: &task.cwd,
        declared_input_globs: &[],
        tracked_env: &[],
    };

    // Phase 1: cache lookup
    if cache.lookup(&inputs).is_some() {
        eprintln!(
            "[rage] {}#{} \u{2713} (cached, two-phase)",
            task.package_name, task.script_name
        );
        return Ok(());
    }

    // Cache miss — execute
    eprintln!(
        "[rage] {}#{} starting [sandbox={:?}]",
        task.package_name, task.script_name, task.sandbox_mode
    );
    let start = Instant::now();

    let (exit_code, pathset) = match task.sandbox_mode {
        pipeline_config::SandboxMode::Loose => {
            let new_path = node_bin_path(&task.cwd, &task.workspace_root);
            let status = Command::new("sh")
                .arg("-c")
                .arg(&task.command)
                .current_dir(&task.cwd)
                .env("PATH", &new_path)
                .status()
                .await
                .map_err(|e| RunError::Spawn {
                    package: task.package_name.clone(),
                    script: task.script_name.clone(),
                    source: e,
                })?;
            let code = status.code().unwrap_or(-1);
            (code, StoredPathset::default())
        }
        _ => {
            let new_path = node_bin_path(&task.cwd, &task.workspace_root);
            let env_pairs = vec![("PATH".to_string(), new_path.to_string_lossy().into_owned())];
            match sandbox::run_sandboxed(&task.command, &task.cwd, &env_pairs).await {
                Ok(r) => {
                    let ps = StoredPathset {
                        reads: r.path_set.reads,
                        writes: r.path_set.writes,
                    };
                    (r.exit_code, ps)
                }
                Err(_) => {
                    // Sandbox unavailable — fall back to plain sh execution
                    let new_path2 = node_bin_path(&task.cwd, &task.workspace_root);
                    let status = Command::new("sh")
                        .arg("-c")
                        .arg(&task.command)
                        .current_dir(&task.cwd)
                        .env("PATH", &new_path2)
                        .status()
                        .await
                        .map_err(|e| RunError::Spawn {
                            package: task.package_name.clone(),
                            script: task.script_name.clone(),
                            source: e,
                        })?;
                    let code = status.code().unwrap_or(-1);
                    (code, StoredPathset::default())
                }
            }
        }
    };

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    if exit_code == 0 {
        let entry = CacheEntry {
            fingerprint: String::new(),
            command: task.command.clone(),
            exit_code: 0,
            elapsed_ms,
            cached_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            pathset_reads: vec![],
        };
        let _ = cache.record(&inputs, pathset, entry); // ignore write errors
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name,
            task.script_name,
            elapsed.as_secs_f64()
        );
        Ok(())
    } else {
        eprintln!(
            "[rage] {}#{} \u{2717} FAILED (exit {exit_code})",
            task.package_name, task.script_name
        );
        Err(RunError::TaskFailed {
            package: task.package_name,
            script: task.script_name,
            code: exit_code,
        })
    }
}

async fn run_root_task_two_phase(
    task: Task,
    cache: Arc<cache::TwoPhaseCache>,
) -> Result<(), RunError> {
    let fp = root_task_fingerprint(&task);
    let marker = cache.dir().join(format!("root-{fp}.done"));

    if marker.exists() {
        eprintln!(
            "[rage] {}#{} \u{2713} (cached)",
            task.package_name, task.script_name
        );
        return Ok(());
    }

    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
    let start = Instant::now();
    let new_path = node_bin_path(&task.cwd, &task.workspace_root);
    let status = Command::new("sh")
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.cwd)
        .env("PATH", &new_path)
        .status()
        .await
        .map_err(|e| RunError::Spawn {
            package: task.package_name.clone(),
            script: task.script_name.clone(),
            source: e,
        })?;
    let elapsed = start.elapsed();

    if status.success() {
        // Best-effort marker write — cache failures must not break a build.
        let _ = std::fs::write(&marker, b"");
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name,
            task.script_name,
            elapsed.as_secs_f64()
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
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
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

    // ── compute_task_levels tests ─────────────────────────────────────────────

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
        let plugins: Vec<&dyn plugin::EcosystemPlugin> = Vec::new();
        let tasks = build_task_list(&dag, "build", &root, &plugins).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // core(L0) → utils(L1) → ui(L2) → app(L3)
        assert_eq!(levels.len(), 4);
        assert_eq!(levels[0].len(), 1); // core
        assert_eq!(levels[0][0].package_name, "@fixture/core");
        assert_eq!(levels[3].len(), 1); // app
        assert_eq!(levels[3][0].package_name, "@fixture/app");
    }

    #[test]
    fn root_task_alone_in_wave_zero_pushes_package_to_wave_one() {
        let root_task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![PathBuf::from("/tmp/pnpm-lock.yaml")],
            workspace_root: PathBuf::from("/tmp"),
        };
        let pkg_task = mk_task("core");
        let dag = build_dag(vec![mk_pkg("core", &[])]).unwrap();
        let levels = compute_task_levels(&dag, &[root_task, pkg_task]);
        assert_eq!(levels.len(), 2, "expected two waves: [install] then [core]");
        assert_eq!(levels[0].len(), 1);
        assert!(levels[0][0].is_root);
        assert_eq!(levels[0][0].package_name, "workspace");
        assert_eq!(levels[1].len(), 1);
        assert_eq!(levels[1][0].package_name, "core");
        assert!(!levels[1][0].is_root);
    }

    #[test]
    fn root_task_pushes_diamond_down_one_wave() {
        let root_task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![],
            workspace_root: PathBuf::from("/tmp"),
        };
        let mut tasks: Vec<Task> = ["core", "utils", "ui", "app"]
            .iter()
            .map(|n| mk_task(n))
            .collect();
        tasks.insert(0, root_task);
        let packages = vec![
            mk_pkg("core", &[]),
            mk_pkg("utils", &["core"]),
            mk_pkg("ui", &["core", "utils"]),
            mk_pkg("app", &["ui", "core"]),
        ];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        // 1 install wave + 4 package waves
        assert_eq!(levels.len(), 5);
        assert!(levels[0][0].is_root);
        assert_eq!(levels[1][0].package_name, "core");
        assert_eq!(levels[2][0].package_name, "utils");
        assert_eq!(levels[3][0].package_name, "ui");
        assert_eq!(levels[4][0].package_name, "app");
    }

    #[test]
    fn no_root_tasks_means_no_extra_wave() {
        // Sanity: if there are no root tasks, behaviour matches the legacy version.
        let tasks: Vec<Task> = ["a", "b"].iter().map(|n| mk_task(n)).collect();
        let packages = vec![mk_pkg("a", &[]), mk_pkg("b", &[])];
        let dag = build_dag(packages).unwrap();
        let levels = compute_task_levels(&dag, &tasks);
        assert_eq!(
            levels.len(),
            1,
            "two independent packages, no root → 1 wave"
        );
        assert_eq!(levels[0].len(), 2);
    }

    // ── run_tasks tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_successful_task_runs() {
        let task = Task {
            package_name: "test-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo hello".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::default(),
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
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
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
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
                is_root: false,
                input_paths: Vec::new(),
                workspace_root: PathBuf::from("/tmp"),
            },
            Task {
                package_name: "b".to_string(),
                script_name: "build".to_string(),
                command: cmd_b,
                cwd: PathBuf::from("/tmp"),
                sandbox_mode: pipeline_config::SandboxMode::default(),
                is_root: false,
                input_paths: Vec::new(),
                workspace_root: PathBuf::from("/tmp"),
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
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: pkg_dir.path().to_path_buf(),
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
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
        };
        let pkg = mk_pkg("uncached-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        // None = no cache — should just execute
        run_tasks(&dag, vec![task], None).await.unwrap();
    }

    #[tokio::test]
    async fn task_logs_sandbox_mode_in_starting_line() {
        // Smoke test: just verify runner accepts SandboxMode-bearing tasks.
        let task = Task {
            package_name: "smoke".to_string(),
            script_name: "build".to_string(),
            command: "true".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Strict,
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: PathBuf::from("/tmp"),
        };
        let pkg = mk_pkg("smoke", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        run_tasks(&dag, vec![task], None).await.unwrap();
    }

    #[tokio::test]
    async fn two_phase_cache_first_run_misses_second_run_hits() {
        use cache::TwoPhaseCache;
        use std::sync::Arc;
        use tempfile::tempdir;

        let cache_dir = tempdir().unwrap();
        let pkg_dir = tempdir().unwrap();
        std::fs::create_dir_all(pkg_dir.path().join("src")).unwrap();
        std::fs::write(pkg_dir.path().join("src/index.ts"), b"export const x = 1;").unwrap();

        let two_phase = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());

        let task = Task {
            package_name: "pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo build".to_string(),
            cwd: pkg_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: pkg_dir.path().to_path_buf(),
        };
        let pkg = mk_pkg("pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        run_tasks_two_phase(&dag, vec![task.clone()], two_phase.clone())
            .await
            .unwrap();

        let entries: Vec<_> = std::fs::read_dir(cache_dir.path()).unwrap().collect();
        assert!(
            entries.iter().any(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("wf-")),
            "expected wf-*.pathsets file"
        );
        assert!(
            entries.iter().any(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("sf-")),
            "expected sf-*.entry file"
        );

        run_tasks_two_phase(&dag, vec![task], two_phase)
            .await
            .unwrap();
    }

    // ── root task fingerprint tests ───────────────────────────────────────────

    #[test]
    fn root_task_fingerprint_changes_with_lockfile_contents() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let lock = dir.path().join("pnpm-lock.yaml");

        std::fs::write(&lock, b"version: 1\n").unwrap();
        let task_a = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![lock.clone()],
            workspace_root: dir.path().to_path_buf(),
        };
        let fp_a = root_task_fingerprint(&task_a);

        // Same task, same lockfile bytes → same fingerprint.
        let fp_a_again = root_task_fingerprint(&task_a);
        assert_eq!(fp_a, fp_a_again);

        // Mutate the lockfile → fingerprint changes.
        std::fs::write(&lock, b"version: 2\n").unwrap();
        let fp_b = root_task_fingerprint(&task_a);
        assert_ne!(fp_a, fp_b, "fingerprint must change with lockfile contents");

        // Different command → different fingerprint.
        let task_c = Task {
            command: "yarn install".to_string(),
            ..task_a.clone()
        };
        let fp_c = root_task_fingerprint(&task_c);
        assert_ne!(fp_b, fp_c);
    }

    #[test]
    fn root_task_fingerprint_handles_missing_lockfile() {
        // Missing files are hashed as a deterministic sentinel — the fingerprint
        // is still stable, just different from the "file present" case.
        let task = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "pnpm install".to_string(),
            cwd: PathBuf::from("/tmp"),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![PathBuf::from("/this/does/not/exist/pnpm-lock.yaml")],
            workspace_root: PathBuf::from("/tmp"),
        };
        let fp1 = root_task_fingerprint(&task);
        let fp2 = root_task_fingerprint(&task);
        assert_eq!(
            fp1, fp2,
            "missing-file fingerprint must still be deterministic"
        );
        assert!(!fp1.is_empty());
    }

    // ── node_bin_path unit tests ──────────────────────────────────────────────

    #[test]
    fn node_bin_path_deduplicates_when_cwd_is_workspace_root() {
        let dir = PathBuf::from("/ws/packages/foo");
        let result = node_bin_path(&dir, &dir);
        let s = result.to_string_lossy().into_owned();
        let count = s.matches("node_modules/.bin").count();
        assert_eq!(count, 1, "same dir should only appear once: {s}");
    }

    #[test]
    fn node_bin_path_prepends_pkg_before_workspace() {
        let cwd = PathBuf::from("/ws/packages/foo");
        let ws = PathBuf::from("/ws");
        let result = node_bin_path(&cwd, &ws);
        let s = result.to_string_lossy().into_owned();
        assert!(
            s.contains("/ws/packages/foo/node_modules/.bin"),
            "pkg bin missing: {s}"
        );
        assert!(s.contains("/ws/node_modules/.bin"), "ws bin missing: {s}");
        let pkg_pos = s.find("/ws/packages/foo/node_modules/.bin").unwrap();
        let ws_pos = s.find("/ws/node_modules/.bin").unwrap();
        assert!(
            pkg_pos < ws_pos,
            "package-local bin must come before workspace bin"
        );
    }

    // ── which_first unit test ─────────────────────────────────────────────────

    #[test]
    fn which_first_prefers_local_node_modules_bin() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let tsc = bin_dir.join("tsc");
        std::fs::write(&tsc, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tsc, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let result = which_first("tsc --noEmit", dir.path(), dir.path());
        assert_eq!(
            result.as_deref(),
            Some(tsc.as_path()),
            "which_first should return the local tsc"
        );
    }

    // ── PATH injection integration test ─────────────────────────────────────

    #[tokio::test]
    async fn node_modules_bin_is_on_path_during_task() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin_path = bin_dir.join("fake-tsc");
        let sentinel = dir.path().join("fake-tsc-ran.txt");
        std::fs::write(
            &bin_path,
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()).as_bytes(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let task = Task {
            package_name: "test-pkg".to_string(),
            script_name: "build".to_string(),
            command: "fake-tsc".to_string(),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
        };
        let pkg = mk_pkg("test-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        let cache_dir = tempdir().unwrap();
        let cache = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap(),
        );
        run_tasks_two_phase(&dag, vec![task], cache).await.unwrap();
        assert!(
            sentinel.exists(),
            "fake-tsc must have been found via node_modules/.bin"
        );
    }
}
