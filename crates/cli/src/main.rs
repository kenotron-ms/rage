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
