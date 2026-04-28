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

    /// Run a script across all workspace packages in dependency order.
    Run {
        /// Script name to run (e.g. `build`, `test`, `lint`).
        script: String,

        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,

        /// Disable the local cache — always re-execute tasks.
        #[arg(long)]
        no_cache: bool,

        /// Scope execution to packages affected since this git ref.
        /// Example: `--since HEAD~1` or `--since origin/main`.
        #[arg(long)]
        since: Option<String>,

        /// Scope to packages with uncommitted changes (staged, unstaged, untracked).
        /// Cannot be combined with --since.
        #[arg(long)]
        affected: bool,

        /// Scope to a specific package and all of its transitive dependencies.
        /// Use the exact package name (e.g. `--to @bebopjs/bebop`).
        /// Cannot be combined with --since or --affected.
        #[arg(long)]
        to: Option<String>,
    },

    /// Run the rage daemon in the foreground (for debugging).
    Daemon {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Send `SetDesiredState` to the daemon — start one if none is running.
    Dev {
        /// Script name to run (e.g. `build`).
        script: String,

        /// Comma-separated list of target packages.
        #[arg(long, value_delimiter = ',')]
        target: Option<Vec<String>>,

        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Print the daemon's current state for this workspace.
    Status {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Open the rage status page in the default browser.
    Open {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Diagnose why a task missed the cache — diff the two most recent runs.
    WhyMiss {
        /// Package name, e.g. `@lage-run/core`.
        package: String,
        /// Script name, e.g. `build`.
        script: String,
        /// Workspace root (defaults to cwd). Used to locate the cache directory.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,
    },

    /// Start rage as a distributed build hub (CI coordinator).
    ///
    /// Discovers the workspace task graph, starts a gRPC server, and dispatches
    /// tasks to spoke workers as they connect. Writes its address to --addr-file
    /// so spokes can discover it.
    Hub {
        /// Workspace root (defaults to cwd).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,

        /// Script to run across the workspace (e.g. `build`).
        #[arg(long, default_value = "build")]
        script: String,

        /// gRPC port to listen on.
        #[arg(long, default_value = "9650")]
        port: u16,

        /// File to write hub address to for spoke discovery.
        /// In Docker Compose: `/shared/rage-hub.json`
        #[arg(long)]
        addr_file: Option<PathBuf>,

        /// Bearer token for spoke authentication. Defaults to RAGE_HUB_TOKEN env var.
        #[arg(long)]
        token: Option<String>,
    },

    /// Start rage as a distributed build spoke (worker connecting to a hub).
    ///
    /// Connects to the hub, subscribes to tasks, executes them, and reports
    /// results back to the hub until the build completes.
    Spoke {
        /// Workspace root where tasks execute.
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Positional workspace path (overrides --workspace).
        workspace_pos: Option<PathBuf>,

        /// File to read hub address from (written by `rage hub --addr-file`).
        /// In Docker Compose: `/shared/rage-hub.json`
        #[arg(long)]
        addr_file: Option<PathBuf>,

        /// Direct hub address (overrides --addr-file). E.g. `http://hub:9650`
        #[arg(long)]
        hub_address: Option<String>,

        /// Bearer token for hub authentication. Defaults to RAGE_HUB_TOKEN env var.
        #[arg(long)]
        token: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Graph {
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_graph(&root)
        }
        Command::Run {
            script,
            workspace,
            workspace_pos,
            no_cache,
            since,
            affected,
            to,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_run(&root, &script, no_cache, since.as_deref(), affected, to.as_deref()).await
        }
        Command::Daemon {
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            daemon::Daemon::new(root).run().await
        }
        Command::Dev {
            script,
            target,
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_dev(&root, &script, target).await
        }
        Command::Status {
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_status(&root).await
        }
        Command::Open {
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_open(&root)
        }
        Command::WhyMiss {
            package,
            script,
            workspace,
            workspace_pos,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_why_miss(&root, &package, &script)
        }

        Command::Hub {
            workspace,
            workspace_pos,
            script,
            port,
            addr_file,
            token,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_hub(&root, &script, port, addr_file, token).await
        }

        Command::Spoke {
            workspace,
            workspace_pos,
            addr_file,
            hub_address,
            token,
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_spoke(&root, addr_file, hub_address, token).await
        }
    }
}

fn resolve_workspace(pos: Option<PathBuf>, named: Option<PathBuf>) -> PathBuf {
    pos.or(named)
        .map(|p| p.canonicalize().unwrap_or(p))
        .unwrap_or_else(|| std::env::current_dir().unwrap())
}

fn cmd_graph(root: &Path) -> Result<()> {
    let pm = workspace_tools::detect_package_manager(root)
        .with_context(|| format!("{} is not a recognized JS workspace", root.display()))?;

    let raw = workspace_tools::discover_packages(root).context("discovering workspace packages")?;
    let resolved =
        workspace_tools::build_package_graph(raw).context("resolving package dependency edges")?;

    eprintln!(
        "Found {} packages ({} workspace)",
        resolved.len(),
        pm.as_str()
    );

    let dag = build_graph::dag::build_dag(resolved).context("building package DAG")?;
    let dot = build_graph::dot::to_dot(&dag);
    print!("{dot}");
    Ok(())
}

async fn cmd_dev(root: &Path, script: &str, target: Option<Vec<String>>) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    let socket_path = daemon::discovery::socket_path(root)?;
    if !socket_path.exists() {
        spawn_detached_daemon(root)?;
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    let stream = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = stream.into_split();
    let msg = serde_json::json!({
        "type": "SetDesiredState",
        "workspace": root,
        "script": script,
        "targets": target,
    });
    let mut line = msg.to_string();
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    write.shutdown().await.ok();
    let mut lines = BufReader::new(read).lines();
    if let Ok(Some(resp)) = lines.next_line().await {
        eprintln!("[rage dev] daemon state: {resp}");
    }
    eprintln!("[rage dev] daemon running for {}", root.display());
    Ok(())
}

async fn cmd_status(root: &Path) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    let socket_path = daemon::discovery::socket_path(root)?;
    if !socket_path.exists() {
        eprintln!("no daemon running for {}", root.display());
        return Ok(());
    }
    let stream = UnixStream::connect(&socket_path).await?;
    let (read, mut write) = stream.into_split();
    write.write_all(b"{\"type\":\"GetState\"}\n").await?;
    write.shutdown().await.ok();
    let mut lines = BufReader::new(read).lines();
    if let Ok(Some(resp)) = lines.next_line().await {
        println!("{resp}");
    }
    Ok(())
}

fn cmd_open(root: &Path) -> Result<()> {
    let disc = daemon::discovery::read_discovery(root)?;
    let Some(d) = disc else {
        anyhow::bail!(
            "no daemon running for {} — run `rage dev` first",
            root.display()
        );
    };
    let url = format!("http://127.0.0.1:{}/", d.http_port);
    eprintln!("opening {url}");
    webbrowser::open(&url).context("opening browser")?;
    Ok(())
}

fn spawn_detached_daemon(root: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").arg(root);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // Note: setsid() would fully detach from the controlling terminal but
    // requires an unsafe pre_exec call, which the workspace lint policy
    // (`unsafe_code = "forbid"`) disallows.  The spawned daemon still
    // outlives the parent process on all supported platforms.
    let child = cmd.spawn().context("spawning detached daemon")?;
    drop(child); // do not wait
    Ok(())
}

/// Compute the transitive dependency closure for `target` within the workspace DAG.
///
/// Returns a set containing `target` itself plus every package it depends on,
/// transitively. Used to implement `--to <package>`.
fn transitive_dep_closure(
    dag: &build_graph::dag::WorkspaceDag,
    target: &str,
) -> std::collections::HashSet<String> {
    let mut closure = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(target.to_string());
    while let Some(pkg_name) = queue.pop_front() {
        if closure.insert(pkg_name.clone()) {
            if let Some(pkg) = dag.packages.get(&pkg_name) {
                for dep in &pkg.dependencies {
                    if !closure.contains(dep) {
                        queue.push_back(dep.clone());
                    }
                }
            }
        }
    }
    closure
}

async fn cmd_run(
    root: &Path,
    script: &str,
    no_cache: bool,
    since: Option<&str>,
    affected: bool,
    to: Option<&str>,
) -> Result<()> {
    let exclusive = [since.is_some(), affected, to.is_some()]
        .iter()
        .filter(|&&b| b)
        .count();
    if exclusive > 1 {
        anyhow::bail!("--since, --affected, and --to are mutually exclusive");
    }

    let pm = workspace_tools::detect_package_manager(root)
        .with_context(|| format!("{} is not a recognized JS workspace", root.display()))?;

    // Load rage.json (optional; absent → defaults).
    let config = pipeline_config::load_config(root)
        .with_context(|| "loading rage.json")?
        .unwrap_or_default();

    let raw = workspace_tools::discover_packages(root).context("discovering workspace packages")?;
    let resolved =
        workspace_tools::build_package_graph(raw).context("resolving package dependency edges")?;

    eprintln!(
        "Found {} packages ({} workspace)",
        resolved.len(),
        pm.as_str()
    );

    let dag = build_graph::dag::build_dag(resolved.clone()).context("building package DAG")?;

    // Compute scope (if --since or --affected was given)
    let scope: Option<std::collections::HashSet<String>> = if let Some(base_ref) = since {
        let changed_files = scoping::git_changed_files(root, base_ref)
            .with_context(|| format!("computing changed files since {base_ref}"))?;
        let aff = scoping::affected_packages(&resolved, &dag, &changed_files);
        eprintln!(
            "Scoping to packages affected since {base_ref}: {} affected ({} scoped out)",
            aff.len(),
            resolved.len().saturating_sub(aff.len())
        );
        Some(aff)
    } else if affected {
        let dirty_files =
            scoping::git_dirty_files(root).context("computing uncommitted changed files")?;
        let aff = scoping::affected_packages(&resolved, &dag, &dirty_files);
        eprintln!(
            "Scoping to packages with uncommitted changes: {} affected ({} scoped out)",
            aff.len(),
            resolved.len().saturating_sub(aff.len())
        );
        Some(aff)
    } else {
        None
    };

    // Active ecosystem plugins. For Phase 12 the TypeScript plugin is the
    // only one, but the type signature already supports a heterogeneous
    // mix (e.g. JS + Python).
    let ts_plugin = plugin_typescript::TypeScriptPlugin::new();
    let plugins: Vec<&dyn plugin::EcosystemPlugin> = vec![&ts_plugin];

    let mut tasks =
        scheduler::task::build_task_list_with_config(&dag, script, root, &plugins, &config)
            .with_context(|| format!("no packages have a '{script}' script"))?;

    // Filter tasks by scope if --since or --affected was given
    if let Some(ref scope_set) = scope {
        // Root tasks (workspace#install etc.) are never package-scoped — always retain.
        tasks.retain(|t| t.is_root || scope_set.contains(&t.package_name));
        if tasks.is_empty() {
            eprintln!("No affected packages have a '{script}' script. Nothing to do.");
            return Ok(());
        }
    }

    // Filter tasks by --to <package> (target + transitive deps)
    if let Some(target) = to {
        if !dag.packages.contains_key(target) {
            anyhow::bail!(
                "package '{}' not found in workspace — check the name in package.json",
                target
            );
        }
        let closure = transitive_dep_closure(&dag, target);
        eprintln!(
            "Scoping to '{}' and its transitive deps: {} packages",
            target,
            closure.len()
        );
        tasks.retain(|t| t.is_root || closure.contains(&t.package_name));
    }

    eprintln!("Running '{}' across {} packages", script, tasks.len());

    if no_cache {
        // Even without caching, postinstall scripts must run after install.
        // Build the same plugin + store that the two-phase path uses.
        let cache_dir_for_store = resolve_cache_dir(root);
        let store_root_nocache = cache_dir_for_store
            .parent() // ~/.rage
            .map(|p| p.join("artifacts"))
            .unwrap_or_else(|| cache_dir_for_store.join("artifacts"));
        std::fs::create_dir_all(&store_root_nocache).ok();
        let artifact_store_nocache =
            std::sync::Arc::new(artifact_store::LocalArtifactStore::new(&store_root_nocache));
        let plugin_arc_nocache: std::sync::Arc<dyn plugin::EcosystemPlugin> =
            std::sync::Arc::new(plugin_typescript::TypeScriptPlugin::new());
        scheduler::run_tasks(
            &dag,
            tasks,
            None,
            Some(plugin_arc_nocache),
            Some(artifact_store_nocache),
        )
        .await
        .with_context(|| format!("'{script}' run failed"))?;
    } else {
        let cache_dir = resolve_cache_dir(root);
        let two_phase = std::sync::Arc::new(
            cache::TwoPhaseCache::with_dir(cache_dir).context("opening two-phase cache")?,
        );
        // Note: remote S3/Azure backends wrap TwoPhaseCache in a future phase.
        // Currently the TwoPhaseCache itself handles two-phase WF/SF lookup;
        // the remote backend (S3Cache/AzureBlobCache) wraps the single-phase
        // CacheProvider trait and would be used for the legacy run_tasks path.
        // Full remote support for run_tasks_two_phase will integrate the remote
        // backend with TwoPhaseCache in a future iteration.
        let _ = &config.cache.backend; // referenced so Clippy is happy
                                       // Construct shared LocalArtifactStore at <RAGE_HOME>/artifacts.
                                       // Sits parallel to cache/ so it can be shared across workspaces on the host.
        let cache_dir_for_store = resolve_cache_dir(root);
        let store_root = cache_dir_for_store
            .parent() // ~/.rage
            .map(|p| p.join("artifacts")) // ~/.rage/artifacts
            .unwrap_or_else(|| cache_dir_for_store.join("artifacts"));
        std::fs::create_dir_all(&store_root).ok();
        let artifact_store =
            std::sync::Arc::new(artifact_store::LocalArtifactStore::new(&store_root));
        let plugin_arc: std::sync::Arc<dyn plugin::EcosystemPlugin> =
            std::sync::Arc::new(plugin_typescript::TypeScriptPlugin::new());
        scheduler::run_tasks_two_phase(
            &dag,
            tasks,
            two_phase,
            plugin_arc,
            artifact_store,
            config.max_concurrency,
        )
        .await
        .with_context(|| format!("'{script}' run failed"))?;
    }

    eprintln!("Done.");
    Ok(())
}
fn cmd_why_miss(root: &Path, pkg: &str, script: &str) -> Result<()> {
    let cache_dir = resolve_cache_dir(root);

    match cache::why_miss::read_snapshots(&cache_dir, pkg, script) {
        None => {
            eprintln!("[rage why-miss] no snapshots found for {pkg}#{script}",);
            eprintln!("  (run 'rage run {script}' at least twice to generate history)");
            Ok(())
        }
        Some((old, new)) => {
            eprintln!(
                "[rage why-miss] {}#{} — comparing run 2 vs run 1",
                new.pkg, new.script
            );
            eprintln!();
            print_why_miss_diff(&old, &new);
            Ok(())
        }
    }
}

fn resolve_cache_dir(root: &Path) -> std::path::PathBuf {
    // Load rage.json to get custom cache dir, else use default ~/.rage/cache
    let config = pipeline_config::load_config(root)
        .ok()
        .flatten()
        .unwrap_or_default();
    match &config.cache.dir {
        Some(d) => d.clone(),
        None => {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
            home.join(".rage").join("cache")
        }
    }
}

fn print_why_miss_diff(
    old: &cache::why_miss::WhyMissSnapshot,
    new: &cache::why_miss::WhyMissSnapshot,
) {
    use std::collections::HashMap;

    let mut any_change = false;

    // ── Command ─────────────────────────────────────────────────────────
    if old.command != new.command {
        any_change = true;
        eprintln!("  COMMAND CHANGED");
        eprintln!("    was: {}", old.command);
        eprintln!("    now: {}", new.command);
        eprintln!();
    }

    // ── Tool binary ──────────────────────────────────────────────────────
    if old.tool_hash != new.tool_hash {
        any_change = true;
        eprintln!("  TOOL BINARY CHANGED");
        eprintln!("    path:  {}", new.tool_path);
        eprintln!(
            "    was:   {}",
            &old.tool_hash[..8.min(old.tool_hash.len())]
        );
        eprintln!(
            "    now:   {}",
            &new.tool_hash[..8.min(new.tool_hash.len())]
        );
        eprintln!();
    }

    // ── Input files ──────────────────────────────────────────────────────
    let old_map: HashMap<_, _> = old.inputs.iter().map(|e| (&e.path, &e.hash)).collect();
    let new_map: HashMap<_, _> = new.inputs.iter().map(|e| (&e.path, &e.hash)).collect();

    let mut changed_files: Vec<_> = new_map
        .iter()
        .filter(|(path, hash)| {
            old_map
                .get(*path)
                .map(|h| h.as_str() != hash.as_str())
                .unwrap_or(false)
        })
        .map(|(p, _)| p)
        .collect();
    changed_files.sort();

    let mut added_files: Vec<_> = new_map
        .keys()
        .filter(|p| !old_map.contains_key(*p))
        .collect();
    added_files.sort();

    let mut removed_files: Vec<_> = old_map
        .keys()
        .filter(|p| !new_map.contains_key(*p))
        .collect();
    removed_files.sort();

    if !changed_files.is_empty() || !added_files.is_empty() || !removed_files.is_empty() {
        any_change = true;
        eprintln!("  CHANGED INPUT FILES");
        for path in &changed_files {
            let old_hash = old_map[*path];
            let new_hash = new_map[*path];
            eprintln!("    modified: {}", path.display());
            eprintln!("      was: {}", &old_hash[..8.min(old_hash.len())]);
            eprintln!("      now: {}", &new_hash[..8.min(new_hash.len())]);
        }
        for path in &added_files {
            eprintln!("    added:    {}", path.display());
        }
        for path in &removed_files {
            eprintln!("    removed:  {}", path.display());
        }
        eprintln!();
    }

    // ── Dep ABI fingerprints ─────────────────────────────────────────────
    let old_abi: HashMap<_, _> = old.dep_abi_fps.iter().map(|(k, v)| (k, v)).collect();
    let new_abi: HashMap<_, _> = new.dep_abi_fps.iter().map(|(k, v)| (k, v)).collect();
    let mut abi_changes: Vec<_> = new_abi
        .iter()
        .filter(|(k, v)| {
            old_abi
                .get(*k)
                .map(|ov| ov.as_str() != v.as_str())
                .unwrap_or(false)
        })
        .map(|(k, _)| k.as_str())
        .collect();
    abi_changes.sort();

    if !abi_changes.is_empty() {
        any_change = true;
        eprintln!("  DEP API CHANGED (upstream .d.ts signature changed)");
        for dep in &abi_changes {
            eprintln!("    {dep}");
        }
        eprintln!();
    }

    // ── Summary ──────────────────────────────────────────────────────────
    if !any_change {
        eprintln!("  (no differences found between the two snapshots)");
        eprintln!("  Note: the cache miss may be due to pathset changes (sandbox-observed reads)");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Hub mode
// ─────────────────────────────────────────────────────────────────────────────

async fn cmd_hub(
    workspace: &Path,
    script: &str,
    port: u16,
    addr_file: Option<PathBuf>,
    token_arg: Option<String>,
) -> Result<()> {
    use hub::dag::TaskNode;
    use hub::rendezvous::{write_hub_addr, HubAddr};
    use hub::server::HubServer;
    use std::net::SocketAddr;

    let token = token_arg
        .or_else(|| std::env::var("RAGE_HUB_TOKEN").ok())
        .unwrap_or_else(|| "rage-default-token".to_string());

    let build_id = format!("build-{}", std::process::id());

    // Discover packages
    let packages =
        workspace_tools::discover_packages(workspace).context("discovering workspace packages")?;
    let resolved =
        workspace_tools::build_package_graph(packages).context("resolving package graph")?;

    // Build task nodes for hub
    let mut task_nodes: Vec<TaskNode> = Vec::new();
    for pkg in &resolved {
        let pkg_json = pkg.path.join("package.json");
        let scripts = std::fs::read_to_string(&pkg_json)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("scripts").and_then(|s| s.as_object().cloned()));

        let Some(scripts) = scripts else { continue };
        let Some(cmd) = scripts.get(script).and_then(|c| c.as_str()) else {
            continue;
        };

        let task_id = format!("{}#{}", pkg.name, script);
        let pkg_path = pkg
            .path
            .strip_prefix(workspace)
            .unwrap_or(&pkg.path)
            .to_string_lossy()
            .to_string();

        let depends_on: Vec<String> = pkg
            .dependencies
            .iter()
            .map(|dep| format!("{}#{}", dep, script))
            .collect();

        task_nodes.push(TaskNode {
            task_id,
            package_name: pkg.name.clone(),
            script_name: script.to_string(),
            command: cmd.to_string(),
            package_path: pkg_path,
            depends_on,
        });
    }

    if task_nodes.is_empty() {
        anyhow::bail!("No packages have a '{}' script", script);
    }

    eprintln!(
        "[rage-hub] {} tasks to distribute across spokes",
        task_nodes.len()
    );

    let hub = HubServer::new(task_nodes, token.clone(), build_id.clone());

    // Write rendezvous file if requested
    if let Some(file) = &addr_file {
        let hostname_str = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "0.0.0.0".to_string());
        let addr_str = format!("http://{}:{}", hostname_str, port);
        let hub_addr = HubAddr {
            addr: addr_str.clone(),
            token: token.clone(),
            build_id: build_id.clone(),
        };
        write_hub_addr(file, &hub_addr)?;
        eprintln!("[rage-hub] rendezvous written to {}", file.display());
        eprintln!("[rage-hub] spokes connect to: {}", addr_str);
    }

    let bind: SocketAddr = format!("0.0.0.0:{port}").parse()?;
    hub.serve(bind).await
}

// ─────────────────────────────────────────────────────────────────────────────
// Spoke mode
// ─────────────────────────────────────────────────────────────────────────────

async fn cmd_spoke(
    workspace: &Path,
    addr_file: Option<PathBuf>,
    hub_address: Option<String>,
    token_arg: Option<String>,
) -> Result<()> {
    use hub::rendezvous::read_hub_addr_with_timeout;

    let token = token_arg
        .or_else(|| std::env::var("RAGE_HUB_TOKEN").ok())
        .unwrap_or_else(|| "rage-default-token".to_string());

    let hub_addr_str = if let Some(addr) = hub_address {
        addr
    } else if let Some(file) =
        addr_file.or_else(|| std::env::var("RAGE_HUB_ADDR_FILE").ok().map(PathBuf::from))
    {
        eprintln!(
            "[rage-spoke] polling for hub address from {}",
            file.display()
        );
        let hub_addr = read_hub_addr_with_timeout(&file, 60).await?;
        hub_addr.addr
    } else if let Ok(addr) = std::env::var("RAGE_HUB_ADDRESS") {
        addr
    } else {
        anyhow::bail!("No hub address: use --hub-address, --addr-file, RAGE_HUB_ADDRESS, or RAGE_HUB_ADDR_FILE");
    };

    eprintln!("[rage-spoke] connecting to hub at {}", hub_addr_str);
    spoke_client::run_as_spoke(hub_addr_str, token, workspace.to_path_buf()).await
}

#[cfg(test)]
mod tests {

    #[test]
    fn store_root_is_sibling_of_cache_not_grandparent() {
        // Demonstrates that store_root should go up ONE level from cache_dir
        // to reach the .rage directory, then join("artifacts")
        // Current buggy code does TWO parents: goes to ~ instead of .rage
        let cache_dir = std::path::PathBuf::from("/home/user/.rage/cache");

        // WRONG (buggy): two parents
        let store_root_buggy = cache_dir
            .parent() // /home/user/.rage
            .and_then(|p| p.parent()) // /home/user <- BUG: one too many
            .map(|p| p.join("artifacts"))
            .unwrap_or_else(|| cache_dir.join("artifacts"));
        assert_eq!(
            store_root_buggy,
            std::path::PathBuf::from("/home/user/artifacts")
        );

        // RIGHT (fixed): one parent
        let store_root_correct = cache_dir
            .parent() // /home/user/.rage
            .map(|p| p.join("artifacts")) // /home/user/.rage/artifacts
            .unwrap_or_else(|| cache_dir.join("artifacts"));
        assert_eq!(
            store_root_correct,
            std::path::PathBuf::from("/home/user/.rage/artifacts")
        );
    }
}
