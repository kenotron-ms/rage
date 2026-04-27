//! Wave-parallel task execution using Tokio.

use crate::task::Task;
use build_graph::dag::WorkspaceDag;
use build_graph::topo::topological_sort;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// Compute a content-addressed fingerprint for a root task.
///
/// Hashes the command plus the contents of every path in `task.input_paths`.
/// Missing files are folded in as `missing:{path}\0` so the fingerprint
/// remains deterministic across runs.
pub(crate) fn root_task_fingerprint(task: &Task) -> String {
    let mut hasher = blake3::Hasher::new();
    // v2: adds env_hash_inputs support. Bumping the version tag ensures
    // existing v1 cache entries are correctly invalidated.
    hasher.update(b"rage.root-task.v2\0");
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
    // Fold ecosystem-supplied env hash inputs (e.g. NODE_VERSION). Sort by
    // key so the order plugins push pairs in does not affect the fingerprint.
    let mut env_pairs = task.env_hash_inputs.clone();
    env_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in &env_pairs {
        hasher.update(b"env:");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
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

    let system_path = std::env::var("PATH").unwrap_or_default();
    let new_path = crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
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
                abi_fingerprint: None,
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
    let system_path = std::env::var("PATH").unwrap_or_default();
    let new_path = crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
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
                abi_fingerprint: None,
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

/// Execute tasks in wave-parallel order using `TwoPhaseCache`.
///
/// For each wave, all tasks run concurrently. Waves run sequentially.
/// On any task failure, the wave is aborted via `JoinSet::abort_all` and the
/// error is returned.
pub async fn run_tasks_two_phase(
    dag: &WorkspaceDag,
    tasks: Vec<Task>,
    cache: Arc<cache::TwoPhaseCache>,
    plugin: Arc<dyn plugin::EcosystemPlugin>,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> anyhow::Result<()> {
    let levels = compute_task_levels(dag, &tasks);

    for level in levels {
        let mut set: JoinSet<Result<(), RunError>> = JoinSet::new();

        for task in level {
            let cache_clone = cache.clone();
            let plugin_clone = Arc::clone(&plugin);
            let store_clone = Arc::clone(&artifact_store);
            set.spawn(run_single_task_two_phase(
                task,
                cache_clone,
                plugin_clone,
                store_clone,
            ));
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
                    set.abort_all();
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
    plugin: Arc<dyn plugin::EcosystemPlugin>,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> Result<(), RunError> {
    if task.is_root {
        return run_root_task_two_phase(task, cache, plugin.as_ref(), artifact_store).await;
    }

    use cache::pathset_store::StoredPathset;
    use cache::{CacheEntry, WeakFpInputs};
    use std::time::{SystemTime, UNIX_EPOCH};

    let tool_path = crate::node_path::which_first(&task.command, &task.cwd, &task.workspace_root)
        .unwrap_or_else(|| PathBuf::from("sh"));

    // Gather dep ABI fingerprints for early-cutoff WF computation
    let dep_abi_fps: Vec<(String, String)> = task
        .dep_package_names
        .iter()
        .filter_map(|dep| cache.get_pkg_abi_fp(dep).map(|fp| (dep.clone(), fp)))
        .collect();

    let inputs = WeakFpInputs {
        command: &task.command,
        tool_path: &tool_path,
        package_path: &task.cwd,
        declared_input_globs: &task.declared_input_globs,
        tracked_env: &[],
        dep_abi_fingerprints: &dep_abi_fps,
    };

    // ── Record why-miss snapshot (fire-and-forget in background) ────────
    // All file I/O is moved to spawn_blocking so we never block the tokio
    // worker thread on disk reads.  The snapshot is best-effort; failures
    // are silently ignored.
    {
        let cache_dir = cache.dir().to_path_buf();
        let cwd_snap = task.cwd.clone();
        let globs_snap = task.declared_input_globs.clone();
        let pkg_snap = task.package_name.clone();
        let script_snap = task.script_name.clone();
        let cmd_snap = task.command.clone();
        let tp_snap = tool_path.clone();
        let dep_fps_snap = dep_abi_fps.clone();
        // Fire-and-forget: don't await.
        tokio::task::spawn_blocking(move || {
            use std::time::{SystemTime, UNIX_EPOCH};
            let resolved = cache::weak_fp::resolve_globs_for_snapshot(&cwd_snap, &globs_snap);
            let tool_hash_str = cache::tool_hash::hash_tool_binary(&tp_snap)
                .unwrap_or_else(|| "<missing>".to_string());
            let snap = cache::why_miss::WhyMissSnapshot {
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                pkg: pkg_snap,
                script: script_snap,
                command: cmd_snap,
                tool_path: tp_snap.to_string_lossy().into_owned(),
                tool_hash: tool_hash_str,
                inputs: resolved
                    .into_iter()
                    .map(|(p, h)| cache::why_miss::InputEntry { path: p, hash: h })
                    .collect(),
                env: vec![],
                dep_abi_fps: dep_fps_snap,
            };
            cache::why_miss::record_snapshot(&cache_dir, snap);
        });
    }

    // Phase 1: cache lookup
    // Run in spawn_blocking: compute_weak_fingerprint does synchronous
    // file I/O (tool-binary hash + glob walk). Keeping it off the tokio
    // worker-thread pool prevents all worker threads from blocking in
    // parallel when a full wave of package tasks starts at once.
    let (cache_lookup_result, stored_pathset_reads) = {
        let c2 = Arc::clone(&cache);
        let cmd2 = task.command.clone();
        let tp2 = tool_path.clone();
        let pp2 = task.cwd.clone();
        let gl2 = task.declared_input_globs.clone();
        let df2 = dep_abi_fps.clone();
        tokio::task::spawn_blocking(move || {
            let wf_inputs = cache::WeakFpInputs {
                command: &cmd2,
                tool_path: &tp2,
                package_path: &pp2,
                declared_input_globs: &gl2,
                tracked_env: &[],
                dep_abi_fingerprints: &df2,
            };
            match c2.lookup_with_pathset_reads(&wf_inputs) {
                Some((sf, entry, reads)) => (Some((sf, entry)), reads),
                None => (None, vec![]),
            }
        })
        .await
        .unwrap_or((None, vec![]))
    };
    if let Some((sf, _entry)) = cache_lookup_result {
        // Populate CAS from the stored pathset on cache hit (fire-and-forget).
        // This ensures the artifact store is primed even when all builds are warm,
        // so that deleting node_modules can be restored without re-running install.
        if !stored_pathset_reads.is_empty() {
            let install_fp = find_latest_install_fingerprint(cache.dir());
            if let Some(fp) = install_fp {
                let artifact_dir = cache.dir().join("artifact-packages").join(&fp); // directory, not file
                crate::artifact_capture::schedule_capture(
                    stored_pathset_reads,
                    task.workspace_root.clone(),
                    artifact_dir,
                    fp,
                    Arc::clone(&artifact_store),
                );
            }
        }

        // Replay captured output from the original run.
        if let Some(out) = cache::output_store::read_output(cache.dir(), &sf) {
            print!("{}", out.stdout);
            eprint!("{}", out.stderr);
        }
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
    let mut captured_stdout = String::new();
    let mut captured_stderr = String::new();

    let (exit_code, pathset) = match task.sandbox_mode {
        pipeline_config::SandboxMode::Loose => {
            let system_path = std::env::var("PATH").unwrap_or_default();
            let new_path =
                crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
            let builder = {
                let mut cmd = Command::new("sh");
                cmd.arg("-c")
                    .arg(&task.command)
                    .current_dir(&task.cwd)
                    .env("PATH", &new_path);
                cmd
            };
            let (code, out, err) =
                spawn_capture_tee(builder)
                    .await
                    .map_err(|e| RunError::Spawn {
                        package: task.package_name.clone(),
                        script: task.script_name.clone(),
                        source: e,
                    })?;
            captured_stdout = out;
            captured_stderr = err;
            (code, StoredPathset::default())
        }
        _ => {
            let system_path = std::env::var("PATH").unwrap_or_default();
            let new_path =
                crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
            let env_pairs = vec![("PATH".to_string(), new_path.clone())];
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
                    let system_path2 = std::env::var("PATH").unwrap_or_default();
                    let new_path2 = crate::node_path::build_node_path(
                        &task.cwd,
                        &task.workspace_root,
                        &system_path2,
                    );
                    let builder2 = {
                        let mut cmd = Command::new("sh");
                        cmd.arg("-c")
                            .arg(&task.command)
                            .current_dir(&task.cwd)
                            .env("PATH", &new_path2);
                        cmd
                    };
                    let (code, out, err) =
                        spawn_capture_tee(builder2)
                            .await
                            .map_err(|e| RunError::Spawn {
                                package: task.package_name.clone(),
                                script: task.script_name.clone(),
                                source: e,
                            })?;
                    captured_stdout = out;
                    captured_stderr = err;
                    (code, StoredPathset::default())
                }
            }
        }
    };

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;

    if exit_code == 0 {
        // Compute ABI fingerprint BEFORE creating the CacheEntry so it can be
        // stored in entry.abi_fingerprint for downstream inspection.
        // Uses a plugin-agnostic .d.ts hasher (equivalent to TypeScriptPlugin::abi_fingerprint).
        let output_files = resolve_output_globs(&task.cwd, &task.output_globs);
        let abi_fp: Option<String> = if !output_files.is_empty() {
            compute_abi_fingerprint_from_outputs(&output_files)
        } else {
            None
        };
        // Persist to the pkg-abi store so downstream tasks can read it during WF computation.
        if let Some(fp) = &abi_fp {
            cache.set_pkg_abi_fp(&task.package_name, fp);
        }

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
            abi_fingerprint: abi_fp,
        };
        let pathset_reads_for_capture = pathset.reads.clone();
        let sf = cache.record(&inputs, pathset, entry).unwrap_or_default();

        // Store captured output for later replay on cache hit.
        if !sf.is_empty() {
            cache::output_store::write_output(
                cache.dir(),
                &sf,
                &cache::output_store::TaskOutput {
                    stdout: captured_stdout.clone(),
                    stderr: captured_stderr.clone(),
                    exit_code: 0,
                },
            );
        }

        // ── Observation-driven CAS capture (fire-and-forget) ─────────────
        if !pathset_reads_for_capture.is_empty() {
            let install_fp = find_latest_install_fingerprint(cache.dir());
            if let Some(fp) = install_fp {
                let artifact_dir = cache.dir().join("artifact-packages").join(&fp); // directory, not file
                crate::artifact_capture::schedule_capture(
                    pathset_reads_for_capture,
                    task.workspace_root.clone(),
                    artifact_dir,
                    fp,
                    Arc::clone(&artifact_store),
                );
            }
        }

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
    plugin: &dyn plugin::EcosystemPlugin,
    artifact_store: Arc<artifact_store::LocalArtifactStore>,
) -> Result<(), RunError> {
    let fp = root_task_fingerprint(&task);
    let marker = cache.dir().join(format!("root-{fp}.done"));

    if marker.exists() {
        // Verify the install task's on-disk effects are still present.
        if plugin.verify_install_effects(&task.workspace_root) {
            run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());
            eprintln!(
                "[rage] {}#{} \u{2713} (cached)",
                task.package_name, task.script_name
            );
            return Ok(());
        }
        // Effects gone — try CAS restoration before falling through to re-run.
        //
        // Strategy 1: lockfile-based restore (preferred).
        //   Parse lockfile → pre-flight all integrity-keyed tarballs in CAS →
        //   plugin.restore_from_cas() extracts zips into node_modules/.
        //
        // Strategy 2: file-level restore (fallback).
        //   Read per-package JSON manifests → pre-flight all file hashes in CAS →
        //   hardlink individual files into node_modules/.
        let artifact_dir = cache.dir().join("artifact-packages").join(&fp);

        // Try lockfile-based restore first
        let (lockfile_restored, lockfile_pkg_count) = {
            use plugin_typescript::lockfile::compute_cas_key;
            let lockfile_pkgs = plugin.parse_lockfile(&task.workspace_root);
            if let Some(pkgs) = lockfile_pkgs {
                // Pre-flight: all tarballs must be in CAS before touching workspace
                let all_present = pkgs.iter().all(|p| {
                    let key = compute_cas_key(&p.integrity);
                    artifact_store.contains_raw_key(&key)
                });

                if all_present && !pkgs.is_empty() {
                    let store_ref: &dyn plugin::ArtifactStoreRef = artifact_store.as_ref();
                    let n = pkgs.len();
                    match plugin.restore_from_cas(&pkgs, &task.workspace_root, store_ref) {
                        Ok(()) => (true, n),
                        Err(e) => {
                            eprintln!(
                                "[rage] {}#{} lockfile restore partial ({e}) — trying file-level restore",
                                task.package_name, task.script_name
                            );
                            (false, 0)
                        }
                    }
                } else {
                    (false, 0) // Some tarballs not yet in CAS — fall through
                }
            } else {
                (false, 0) // No lockfile support — fall through to file-level
            }
        };

        if lockfile_restored {
            // Create node_modules/.bin/ symlinks from each package's `bin` field.
            // Tarballs don't contain bin symlinks, so we generate them here to
            // avoid running the package manager just for bin-link creation.
            let bin_count = crate::bin_links::create_bin_links(&task.workspace_root).unwrap_or(0);
            eprintln!(
                "[rage] {}#{} \u{2713} (restored from artifact cache — {} packages, {} bin links)",
                task.package_name, task.script_name, lockfile_pkg_count, bin_count
            );
            run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());
            return Ok(());
        }

        // Fall back to file-level restore (original approach)
        match crate::artifact_restore::try_restore_from_cas(
            &artifact_dir,
            &task.workspace_root,
            artifact_store.as_ref(),
        ) {
            Ok(true) => {
                // Create node_modules/.bin/ symlinks from each package's `bin` field.
                // Hidden dirs are skipped by the walk-based capture strategy, so
                // .bin/ is absent after a file-level restore. We generate the
                // symlinks here instead of re-running the package manager.
                let bin_count =
                    crate::bin_links::create_bin_links(&task.workspace_root).unwrap_or(0);
                eprintln!(
                    "[rage] {}#{} \u{2713} (restored from artifact cache — {} bin links)",
                    task.package_name, task.script_name, bin_count
                );
                run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());
                return Ok(());
            }
            Ok(false) => {
                // CAS miss or partial — fall through and re-run install.
                eprintln!(
                    "[rage] {}#{} marker present but effects missing — re-running",
                    task.package_name, task.script_name
                );
                let _ = std::fs::remove_file(&marker);
            }
            Err(e) => {
                eprintln!(
                    "[rage] {}#{} CAS restore failed ({e}) — re-running",
                    task.package_name, task.script_name
                );
                let _ = std::fs::remove_file(&marker);
            }
        }
    }

    eprintln!("[rage] {}#{} starting", task.package_name, task.script_name);
    let start = Instant::now();
    let system_path = std::env::var("PATH").unwrap_or_default();
    let new_path = crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
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
        // Capture into CAS — blocking, runs once per lockfile change.
        // Done BEFORE writing the marker so marker only exists when CAS is populated.
        //
        // Extract lockfile data BEFORE spawn_blocking (plugin reference is not 'static).
        // Strategy 1 (preferred): lockfile-based — parse lockfile → find PM tarballs → store in CAS.
        // Strategy 2 (fallback):  walk node_modules → hash individual files → store in CAS.
        let lockfile_pkgs = plugin.parse_lockfile(&task.workspace_root);
        let pm_cache_path = plugin.local_pm_cache(&task.workspace_root);

        let ws = task.workspace_root.clone();
        let fp_for_dir = fp.clone();
        let cache_dir = cache.dir().to_path_buf();
        let store_clone = Arc::clone(&artifact_store);

        let captured = tokio::task::spawn_blocking(move || {
            let artifact_dir = cache_dir.join("artifact-packages").join(&fp_for_dir);

            if let Some(pkgs) = lockfile_pkgs {
                // Lockfile-based capture
                let n = crate::artifact_capture::capture_from_lockfile_packages(
                    &pkgs,
                    pm_cache_path.as_deref(),
                    &artifact_dir,
                    store_clone.as_ref(),
                )
                .unwrap_or(0);
                eprintln!("[rage] artifact cache (lockfile): {} packages captured", n);
                n
            } else {
                // No lockfile — fall back to walk-based capture
                let n = crate::artifact_capture::capture_all_node_modules(
                    &ws,
                    &artifact_dir,
                    store_clone.as_ref(),
                )
                .unwrap_or(0);
                eprintln!("[rage] artifact cache (walk): {} packages captured", n);
                n
            }
        })
        .await
        .unwrap_or(0);

        let _ = captured; // suppress unused warning

        // Best-effort marker write — cache failures must not break a build.
        let _ = std::fs::write(&marker, b"");
        eprintln!(
            "[rage] {}#{} \u{2713} {:.2}s",
            task.package_name,
            task.script_name,
            elapsed.as_secs_f64()
        );
        run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());
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

/// Find the most recent `root-{fp}.done` marker in `cache_dir` and return its `fp`.
/// Used by the capture hook to key the manifest against the install fingerprint.
fn find_latest_install_fingerprint(cache_dir: &Path) -> Option<String> {
    use std::time::SystemTime;
    let entries = std::fs::read_dir(cache_dir).ok()?;
    let mut best: Option<(SystemTime, String)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("root-") else {
            continue;
        };
        let Some(fp) = rest.strip_suffix(".done") else {
            continue;
        };
        let mtime = entry.metadata().and_then(|m| m.modified()).ok()?;
        match &best {
            None => best = Some((mtime, fp.to_string())),
            Some((old, _)) if mtime > *old => best = Some((mtime, fp.to_string())),
            _ => {}
        }
    }
    best.map(|(_, fp)| fp)
}

/// Run every postinstall task the plugin yields for `workspace_root`, using
/// `store` for cache restore + capture. Errors are swallowed (logged): a
/// failed postinstall is reported but never breaks the install task.
fn run_postinstall_phase(
    plugin: &dyn plugin::EcosystemPlugin,
    workspace_root: &Path,
    store: &artifact_store::LocalArtifactStore,
) {
    use crate::postinstall_cache::{
        capture_dir, diff_manifests, postinstall_cas_key, restore_manifest, run_postinstall,
        store_manifest,
    };

    let tasks = plugin.postinstall_tasks(workspace_root);
    for pt in &tasks {
        let key = postinstall_cas_key(pt);

        // Cache hit?
        match restore_manifest(&key, &pt.cwd, store) {
            Ok(true) => {
                eprintln!(
                    "[rage] {}#postinstall \u{2713} (restored from cache)",
                    pt.package_name
                );
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!(
                    "[rage] {}#postinstall restore error ({e}) \u{2014} re-running",
                    pt.package_name
                );
            }
        }

        // Cache miss — capture before, run, capture after, store delta.
        let before = capture_dir(&pt.cwd, store).unwrap_or_default();
        let start = std::time::Instant::now();
        let ran_ok = run_postinstall(pt).unwrap_or(false);
        let elapsed = start.elapsed();

        if ran_ok {
            let after = capture_dir(&pt.cwd, store).unwrap_or_default();
            let delta = diff_manifests(&before, &after);
            if let Err(e) = store_manifest(&key, &delta, store) {
                eprintln!(
                    "[rage] {}#postinstall capture error ({e}) \u{2014} ran but not cached",
                    pt.package_name
                );
            }
            eprintln!(
                "[rage] {}#postinstall \u{2713} {:.2}s",
                pt.package_name,
                elapsed.as_secs_f64()
            );
        } else {
            eprintln!("[rage] {}#postinstall \u{2717} FAILED", pt.package_name);
        }
    }
}

/// Spawn a command with piped stdout+stderr; tee to terminal in real time
/// while also collecting into memory buffers.
///
/// Returns `(exit_code, stdout_utf8, stderr_utf8)`.
async fn spawn_capture_tee(mut builder: Command) -> std::io::Result<(i32, String, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Child;

    builder.stdout(std::process::Stdio::piped());
    builder.stderr(std::process::Stdio::piped());

    let mut child: Child = builder.spawn()?;
    let mut stdout_pipe = child.stdout.take().expect("stdout pipe");
    let mut stderr_pipe = child.stderr.take().expect("stderr pipe");

    let mut stdout_bytes: Vec<u8> = Vec::new();
    let mut stderr_bytes: Vec<u8> = Vec::new();

    // Drain both pipes concurrently while writing to real terminal.
    let (r1, r2) = tokio::join!(
        async {
            let mut buf = [0u8; 8192];
            let mut out = tokio::io::stdout();
            loop {
                match stdout_pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        stdout_bytes.extend_from_slice(&buf[..n]);
                        let _ = out.write_all(&buf[..n]).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            let _ = out.flush().await;
            Ok(())
        },
        async {
            let mut buf = [0u8; 8192];
            let mut err = tokio::io::stderr();
            loop {
                match stderr_pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        stderr_bytes.extend_from_slice(&buf[..n]);
                        let _ = err.write_all(&buf[..n]).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            let _ = err.flush().await;
            Ok(())
        }
    );
    r1?;
    r2?;

    let status = child.wait().await?;
    let code = status.code().unwrap_or(-1);

    Ok((
        code,
        String::from_utf8_lossy(&stdout_bytes).into_owned(),
        String::from_utf8_lossy(&stderr_bytes).into_owned(),
    ))
}

fn resolve_output_globs(cwd: &std::path::Path, output_globs: &[String]) -> Vec<std::path::PathBuf> {
    if output_globs.is_empty() {
        return Vec::new();
    }
    use walkdir::WalkDir;

    // Build a simple glob matcher without pulling in extra deps.
    // We use the same simple_glob_match from task.rs would need, but here
    // we do a full recursive walk matching against each glob.
    // For correctness, we use globset from the cache crate indirectly:
    // since scheduler depends on cache which depends on globset, we can
    // use a simple path-matching approach here.
    let mut results = Vec::new();
    const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", ".git"];

    for entry in WalkDir::new(cwd)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(cwd) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        // Check against each output glob using simple prefix/suffix matching
        for glob in output_globs {
            if output_glob_matches(glob, &rel) {
                results.push(path.to_path_buf());
                break;
            }
        }
    }
    results
}

/// Simple glob matcher for output files: supports `*` and `**`.
fn output_glob_matches(glob: &str, path: &str) -> bool {
    // Normalize separators
    let glob = glob.replace('\\', "/");
    let path = path.replace('\\', "/");
    glob_match_recursive(glob.split('/').collect(), path.split('/').collect())
}

fn glob_match_recursive(mut pattern: Vec<&str>, mut path: Vec<&str>) -> bool {
    loop {
        match (pattern.first(), path.first()) {
            (None, None) => return true,
            (None, _) => return false,
            (Some(&"**"), _) => {
                pattern.remove(0);
                if pattern.is_empty() {
                    return true;
                }
                // Try matching rest of pattern from each position in path
                for i in 0..=path.len() {
                    if glob_match_recursive(pattern.clone(), path[i..].to_vec()) {
                        return true;
                    }
                }
                return false;
            }
            (_, None) => return false,
            (Some(p), Some(s)) => {
                if !simple_component_match(p, s) {
                    return false;
                }
                pattern.remove(0);
                path.remove(0);
            }
        }
    }
}

fn simple_component_match(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == s;
    }
    // Split on '*' and do prefix/infix/suffix matching
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = s;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            return remaining.ends_with(part);
        } else if let Some(pos) = remaining.find(part) {
            remaining = &remaining[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Compute an ABI fingerprint from a list of output files.
/// Hashes the contents of `.d.ts`, `.d.cts`, and `.d.mts` files.
/// Returns `None` if no declaration files are found.
fn compute_abi_fingerprint_from_outputs(files: &[std::path::PathBuf]) -> Option<String> {
    let mut dts_paths: Vec<&std::path::Path> = files
        .iter()
        .map(|p| p.as_path())
        .filter(|p| {
            let s = p.to_string_lossy();
            s.ends_with(".d.ts") || s.ends_with(".d.cts") || s.ends_with(".d.mts")
        })
        .collect();
    if dts_paths.is_empty() {
        return None;
    }
    dts_paths.sort();
    let mut hasher = blake3::Hasher::new();
    for path in dts_paths {
        hasher.update(path.as_os_str().as_encoded_bytes());
        if let Ok(content) = std::fs::read(path) {
            hasher.update(&content);
        }
    }
    Some(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::build_task_list;
    use build_graph::dag::build_dag;
    use std::path::PathBuf;
    use workspace_tools::Package;

    /// Test helper: create a no-op plugin + throwaway store for tests that
    /// don't care about the artifact-capture/restore behaviour.
    fn test_plugin() -> Arc<dyn plugin::EcosystemPlugin> {
        Arc::new(plugin_typescript::TypeScriptPlugin::new())
    }

    #[allow(deprecated)]
    fn test_store() -> Arc<artifact_store::LocalArtifactStore> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(artifact_store::LocalArtifactStore::new(dir.into_path()))
    }

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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
                declared_input_globs: Vec::new(),
                dep_package_names: Vec::new(),
                output_globs: Vec::new(),
                env_hash_inputs: Vec::new(),
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
                declared_input_globs: Vec::new(),
                dep_package_names: Vec::new(),
                output_globs: Vec::new(),
                env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };
        let pkg = mk_pkg("pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        run_tasks_two_phase(
            &dag,
            vec![task.clone()],
            two_phase.clone(),
            test_plugin(),
            test_store(),
        )
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

        run_tasks_two_phase(&dag, vec![task], two_phase, test_plugin(), test_store())
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };
        let fp1 = root_task_fingerprint(&task);
        let fp2 = root_task_fingerprint(&task);
        assert_eq!(
            fp1, fp2,
            "missing-file fingerprint must still be deterministic"
        );
        assert!(!fp1.is_empty());
    }

    #[test]
    fn root_task_fingerprint_changes_with_env_hash_inputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"v1\n").unwrap();
        let mk = |env: Vec<(String, String)>| Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "yarn install".to_string(),
            cwd: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![dir.path().join("yarn.lock")],
            workspace_root: dir.path().to_path_buf(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: env,
        };
        let fp_none = root_task_fingerprint(&mk(Vec::new()));
        let fp_v18 = root_task_fingerprint(&mk(vec![(
            "NODE_VERSION".to_string(),
            "18.20.4".to_string(),
        )]));
        let fp_v20 = root_task_fingerprint(&mk(vec![(
            "NODE_VERSION".to_string(),
            "20.11.0".to_string(),
        )]));
        assert_ne!(
            fp_none, fp_v18,
            "adding NODE_VERSION must change fingerprint"
        );
        assert_ne!(
            fp_v18, fp_v20,
            "different NODE_VERSION must change fingerprint"
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
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };
        let pkg = mk_pkg("test-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();
        let cache_dir = tempdir().unwrap();
        let cache = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap(),
        );
        run_tasks_two_phase(&dag, vec![task], cache, test_plugin(), test_store())
            .await
            .unwrap();
        assert!(
            sentinel.exists(),
            "fake-tsc must have been found via node_modules/.bin"
        );
    }

    // ── Phase 2 regression test: declared_input_globs wired ─────────────────

    #[tokio::test]
    async fn source_change_causes_cache_miss_with_input_globs() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("index.ts");
        std::fs::write(&src_file, b"export const v = 1;").unwrap();

        let task = Task {
            package_name: "ts-pkg".to_string(),
            script_name: "build".to_string(),
            command: "echo build".to_string(),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
            declared_input_globs: vec!["src/**/*.ts".to_string(), "package.json".to_string()],
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };

        let cache_dir = tempdir().unwrap();
        let cache = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap(),
        );
        let pkg = mk_pkg("ts-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        // First run — cache miss, WF computed from src/index.ts content
        run_tasks_two_phase(
            &dag,
            vec![task.clone()],
            cache.clone(),
            test_plugin(),
            test_store(),
        )
        .await
        .unwrap();

        let wf_files_after_first: Vec<_> = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("wf-"))
            .collect();
        assert_eq!(
            wf_files_after_first.len(),
            1,
            "one WF entry after first run"
        );

        // Mutate declared input — WF must change
        std::fs::write(&src_file, b"export const v = 2;").unwrap();

        // Second run — must MISS because WF changed (source file content changed)
        run_tasks_two_phase(&dag, vec![task], cache, test_plugin(), test_store())
            .await
            .unwrap();

        let wf_files_after_second: Vec<_> = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("wf-"))
            .collect();
        assert_eq!(
            wf_files_after_second.len(),
            2,
            "source change must produce a new WF entry (cache miss)"
        );
    }

    // ── Phase 3: ABI early-cutoff integration test ───────────────────────────

    #[tokio::test]
    async fn abi_early_cutoff_hit_when_dep_api_unchanged() {
        // Simulate: core builds and produces a .d.ts file (its ABI fingerprint is stored).
        // utils depends on core. When core's *implementation* changes but its .d.ts
        // doesn't, utils should hit the cache via WF (same dep ABI fingerprint).
        use tempfile::tempdir;
        let core_dir = tempdir().unwrap();
        let utils_dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();

        // Write a .d.ts file for "core" that will be its ABI output
        let dts_file = core_dir.path().join("index.d.ts");
        std::fs::write(&dts_file, b"export declare const x: number;\n").unwrap();

        // Pre-populate core's ABI fingerprint in cache (simulates core having run)
        let cache = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap(),
        );

        // Compute what core's ABI fingerprint would be
        let core_abi = {
            use plugin::OutputFile;
            use plugin_typescript::TypeScriptPlugin;
            let outputs = vec![OutputFile {
                path: dts_file.clone(),
            }];
            plugin::EcosystemPlugin::abi_fingerprint(&TypeScriptPlugin::new(), &outputs).unwrap()
        };
        cache.set_pkg_abi_fp("core", &core_abi);

        // utils task declares "core" as a dep
        let utils_task = Task {
            package_name: "utils".to_string(),
            script_name: "build".to_string(),
            command: "echo utils-build".to_string(),
            cwd: utils_dir.path().to_path_buf(),
            workspace_root: utils_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
            declared_input_globs: Vec::new(),
            dep_package_names: vec!["core".to_string()],
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };

        let pkg_utils = mk_pkg("utils", &["core"]);
        let pkg_core = mk_pkg("core", &[]);
        let dag = build_dag(vec![pkg_core, pkg_utils]).unwrap();

        // First run: utils runs (cache miss) — WF includes core's ABI fp
        run_tasks_two_phase(
            &dag,
            vec![utils_task.clone()],
            cache.clone(),
            test_plugin(),
            test_store(),
        )
        .await
        .unwrap();

        // Verify one WF entry for utils
        let wf_count_1 = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("wf-"))
            .count();
        assert_eq!(wf_count_1, 1, "first run: one WF entry");

        // utils runs AGAIN with the same core ABI fingerprint →
        // same WF → cache HIT → printed as "(cached, two-phase)"
        run_tasks_two_phase(
            &dag,
            vec![utils_task],
            cache.clone(),
            test_plugin(),
            test_store(),
        )
        .await
        .unwrap();

        // WF entry count must still be 1 (hit, not a second entry)
        let wf_count_2 = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("wf-"))
            .count();
        assert_eq!(
            wf_count_2, 1,
            "second run with same dep ABI: cache hit, still 1 WF entry"
        );
    }

    // ── Phase 3 fix: entry.abi_fingerprint populated from .d.ts outputs ───────────

    /// Verify that after a task with .d.ts output_globs runs, the CacheEntry
    /// stored in the two-phase cache carries a non-None abi_fingerprint.
    ///
    /// BUG: entry.abi_fingerprint was hardcoded None even though
    /// compute_abi_fingerprint_from_outputs() was called. Fix: compute ABI fp
    /// *before* CacheEntry construction and include it in the entry.
    #[tokio::test]
    async fn entry_abi_fingerprint_set_when_dts_outputs_exist() {
        use cache::TwoPhaseCache;
        use std::sync::Arc;
        use tempfile::tempdir;

        let cache_dir = tempdir().unwrap();
        let pkg_dir = tempdir().unwrap();

        // Pre-create a .d.ts file (simulates TypeScript compiler output)
        std::fs::write(
            pkg_dir.path().join("index.d.ts"),
            b"export declare const x: number;\n",
        )
        .unwrap();

        let cache = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());

        let task = Task {
            package_name: "ts-lib".to_string(),
            script_name: "build".to_string(),
            command: "echo ok".to_string(),
            cwd: pkg_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
            workspace_root: pkg_dir.path().to_path_buf(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: vec!["**/*.d.ts".to_string()],
            env_hash_inputs: Vec::new(),
        };

        let pkg = mk_pkg("ts-lib", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        run_tasks_two_phase(
            &dag,
            vec![task.clone()],
            cache.clone(),
            test_plugin(),
            test_store(),
        )
        .await
        .unwrap();

        let tool_path =
            crate::node_path::which_first(&task.command, &task.cwd, &task.workspace_root)
                .unwrap_or_else(|| std::path::PathBuf::from("echo"));

        let inputs = cache::WeakFpInputs {
            command: &task.command,
            tool_path: &tool_path,
            package_path: &task.cwd,
            declared_input_globs: &task.declared_input_globs,
            tracked_env: &[],
            dep_abi_fingerprints: &[],
        };

        let (_, entry) = cache
            .lookup(&inputs)
            .expect("entry must exist after first run");
        assert!(
            entry.abi_fingerprint.is_some(),
            "entry.abi_fingerprint must be Some(_) when .d.ts output files exist; \
             got None - fix: compute ABI fp before creating CacheEntry"
        );

        let pkg_abi = cache.get_pkg_abi_fp("ts-lib");
        assert_eq!(
            entry.abi_fingerprint, pkg_abi,
            "entry.abi_fingerprint and pkg-abi store must agree"
        );
    }

    // ── Phase 5b: output capture + replay tests ──────────────────────────

    #[tokio::test]
    async fn captured_output_replayed_on_cache_hit() {
        use cache::TwoPhaseCache;
        use std::sync::Arc;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();

        // A task that prints to stdout.
        let task = Task {
            package_name: "output-pkg".to_string(),
            script_name: "build".to_string(),
            command: "printf 'hello from output-pkg\\n'".to_string(),
            cwd: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: false,
            input_paths: Vec::new(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };

        let cache = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());
        let pkg = mk_pkg("output-pkg", &[]);
        let dag = build_dag(vec![pkg]).unwrap();

        // First run — executes task, captures output.
        run_tasks_two_phase(
            &dag,
            vec![task.clone()],
            cache.clone(),
            test_plugin(),
            test_store(),
        )
        .await
        .unwrap();

        // Verify output was stored.
        let output_files: Vec<_> = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".output.json"))
            .collect();
        assert!(
            !output_files.is_empty(),
            "expected sf-*.output.json to be written after first run"
        );

        // Read and verify stored output.
        let output_path = output_files[0].path();
        let stored: cache::output_store::TaskOutput =
            serde_json::from_str(&std::fs::read_to_string(&output_path).unwrap()).unwrap();
        assert!(
            stored.stdout.contains("hello from output-pkg"),
            "stored stdout should contain task output: {:?}",
            stored.stdout
        );
        assert_eq!(stored.exit_code, 0);

        // Second run — should be a cache hit with replayed output.
        run_tasks_two_phase(&dag, vec![task], cache.clone(), test_plugin(), test_store())
            .await
            .unwrap();
    }

    // ── cache-hit → schedule_capture integration ──────────────────────────────

    /// `find_latest_install_fingerprint` + `capture_now` together write per-package
    /// files for pnpm packages referenced in pathset reads.
    ///
    /// This covers the code path wired in `run_single_task_two_phase`:
    /// `stored_pathset_reads` non-empty + `find_latest_install_fingerprint` returns `Some(fp)`
    /// → `schedule_capture` fires and `artifact-packages/{fp}/ms@2.1.3.json` is written.
    #[test]
    fn cache_hit_capture_writes_per_package_files_for_pnpm_packages() {
        use tempfile::tempdir;

        let cache_dir = tempdir().unwrap();
        let ws = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let store = artifact_store::LocalArtifactStore::new(store_dir.path());

        // 1) Write a `root-{fp}.done` marker so find_latest_install_fingerprint returns "abc123".
        let fp = "abc123";
        std::fs::write(cache_dir.path().join(format!("root-{fp}.done")), b"").unwrap();

        // 2) Build a fake pnpm virtual store layout.
        let pnpm_pkg = ws
            .path()
            .join("node_modules/.pnpm/ms@2.1.3/node_modules/ms");
        std::fs::create_dir_all(&pnpm_pkg).unwrap();
        std::fs::write(pnpm_pkg.join("index.js"), b"// ms").unwrap();
        std::fs::write(
            pnpm_pkg.join("package.json"),
            br#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();

        // 3) Pathset reads that point into the pnpm virtual store.
        let pathset_reads = vec![pnpm_pkg.join("index.js"), pnpm_pkg.join("package.json")];

        // 4) find_latest_install_fingerprint should return "abc123".
        let found_fp = find_latest_install_fingerprint(cache_dir.path());
        assert_eq!(found_fp.as_deref(), Some(fp));

        // 5) Derive artifact directory and call capture_now (sync variant of schedule_capture).
        let artifact_dir = cache_dir.path().join("artifact-packages").join(fp);
        crate::artifact_capture::capture_now(&pathset_reads, ws.path(), &artifact_dir, fp, &store)
            .unwrap();

        // 6) Verify per-package file written and contains ms@2.1.3.
        let pkg_file = artifact_dir.join("ms@2.1.3.json");
        assert!(
            pkg_file.exists(),
            "per-package file must exist: {pkg_file:?}"
        );
        let text = std::fs::read_to_string(&pkg_file).unwrap();
        let artifact: artifact_store::PackageArtifact = serde_json::from_str(&text).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
        // Every file hash referenced in the per-package file must be present in the CAS.
        use artifact_store::ArtifactStore as _;
        for (_, hash) in &artifact.files {
            assert!(
                store.contains(hash),
                "CAS should contain hash for captured file"
            );
        }
    }

    // ── postinstall integration test ────────────────────────────────────────

    #[tokio::test]
    async fn postinstall_runs_after_fresh_install_and_restores_on_second_run() {
        use build_graph::dag::build_dag;
        use cache::TwoPhaseCache;
        use tempfile::tempdir;

        let ws_dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();
        let store_dir = tempdir().unwrap();

        // Set up fake package with postinstall script.
        // No lockfile → integrity falls back to `rage-fallback:fake-pkg`.
        let pkg_dir = ws_dir.path().join("node_modules").join("fake-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            br#"{"name":"fake-pkg","version":"1.0.0","scripts":{"postinstall":"touch did-run.flag"}}"#,
        )
        .unwrap();

        let cache = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());
        #[allow(deprecated)]
        let store = Arc::new(artifact_store::LocalArtifactStore::new(
            store_dir.path().to_path_buf(),
        ));

        let install = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "true".to_string(),
            cwd: ws_dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            workspace_root: ws_dir.path().to_path_buf(),
            input_paths: Vec::new(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };

        // An empty DAG — root task runs in wave 0 regardless.
        let dag = build_dag(vec![]).unwrap();

        // First run — fresh install; postinstall should execute.
        run_tasks_two_phase(
            &dag,
            vec![install.clone()],
            cache.clone(),
            test_plugin(),
            store.clone(),
        )
        .await
        .unwrap();

        assert!(
            pkg_dir.join("did-run.flag").exists(),
            "postinstall should have created did-run.flag on first run"
        );

        // Delete the flag to simulate a clean package state.
        std::fs::remove_file(pkg_dir.join("did-run.flag")).unwrap();

        // Second run — marker already exists; postinstall outputs should be
        // restored from CAS rather than re-executed.
        run_tasks_two_phase(
            &dag,
            vec![install.clone()],
            cache.clone(),
            test_plugin(),
            store.clone(),
        )
        .await
        .unwrap();

        assert!(
            pkg_dir.join("did-run.flag").exists(),
            "postinstall output should have been restored from CAS on second run"
        );
    }
}
