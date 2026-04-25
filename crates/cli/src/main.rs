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
        } => {
            let root = resolve_workspace(workspace_pos, workspace);
            cmd_run(&root, &script, no_cache).await
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

async fn cmd_run(root: &Path, script: &str, no_cache: bool) -> Result<()> {
    use cache::LocalCache;
    use std::sync::Arc;

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

    let tasks = scheduler::task::build_task_list(&dag, script)
        .with_context(|| format!("no packages have a '{script}' script"))?;

    eprintln!("Running '{}' across {} packages", script, tasks.len());

    let cache: Option<Arc<dyn cache::CacheProvider>> = if no_cache {
        None
    } else {
        match LocalCache::new() {
            Ok(lc) => Some(Arc::new(lc)),
            Err(e) => {
                eprintln!("[rage] warning: cache unavailable: {e}");
                None
            }
        }
    };

    scheduler::run_tasks(&dag, tasks, cache)
        .await
        .with_context(|| format!("'{script}' run failed"))?;

    eprintln!("Done.");
    Ok(())
}
