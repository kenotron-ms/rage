//! Ecosystem plugin contract.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! ecosystem plugins centralize the declaration burden — they tell rage what
//! TypeScript / Rust / Go / Python packages typically read, write, and emit.
//! User-supplied config augments these defaults.

pub mod types;

pub use types::{AllowlistEntry, OutputFile, PluginConfig, TaskDef};

use std::path::{Path, PathBuf};

/// A workspace-level task that runs ONCE at the workspace root before any
/// per-package task. Examples:
///
/// - TypeScript / JavaScript: `pnpm install`, `yarn install`, `npm install`
/// - Python (future):         `uv sync`
/// - Go (future):              `go mod download`
/// - Rust (future):            `cargo fetch` — or no root task at all
///
/// Cache fingerprinting uses `command` plus the contents of every path in
/// `input_paths` (typically the lockfile). Changing the lockfile invalidates
/// the cache; a stable lockfile + same command = cache hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootTask {
    /// Human-readable script name. Becomes the `script_name` of the synthesized
    /// task (e.g. `"install"` → log line `workspace#install`).
    pub name: String,
    /// Shell command to execute at the workspace root.
    pub command: String,
    /// Files whose contents fingerprint this task. Most ecosystems supply a
    /// single lockfile path; an empty vec is allowed (the command alone
    /// becomes the fingerprint, which is rarely what you want).
    pub input_paths: Vec<PathBuf>,
}

/// Implemented by each ecosystem (TypeScript, Rust, Go, ...).
///
/// A plugin is a value type; the runtime stores `Box<dyn EcosystemPlugin>`.
pub trait EcosystemPlugin: Send + Sync {
    /// Stable plugin id, used to look up `plugins_config.<id>` in `rage.json`.
    fn id(&self) -> &'static str;

    /// Globs (workspace-relative) that signal a package belongs to this plugin.
    /// Example: TypeScript returns `["tsconfig.json", "tsconfig.*.json"]`.
    fn detection_globs(&self) -> Vec<&'static str>;

    /// Given a package root, return the task definitions this plugin offers.
    /// Implementations may inspect manifest files (e.g. `package.json` scripts)
    /// to specialize.
    fn infer_tasks(&self, root: &Path) -> Vec<TaskDef>;

    /// Files the toolchain reads on every invocation but that are not package
    /// inputs (system libs, compiler internals). Used to suppress "undeclared
    /// read" warnings.
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;

    /// Globs feeding the *weak fingerprint* for cache lookup.
    ///
    /// `task_name` is the script being run (`build`, `typecheck`, ...).
    /// `config` carries `extend`/`exclude` overrides from `rage.json`.
    fn declared_input_globs(&self, task_name: &str, config: &PluginConfig) -> Vec<String>;

    /// Optional ABI fingerprint of `outputs`.
    ///
    /// Returns a deterministic hex string identifying the *semantic* shape of
    /// the outputs (e.g. TypeScript hashes `.d.ts` files; Go hashes exported
    /// symbols). Used for downstream early-cutoff: if a package's ABI didn't
    /// change, dependents may skip rebuilds.
    ///
    /// Returns `None` when this plugin doesn't support ABI-level cutoffs.
    fn abi_fingerprint(&self, outputs: &[OutputFile]) -> Option<String>;

    /// Root tasks for this ecosystem — run ONCE at workspace root before any
    /// package tasks. Return an empty vec if this ecosystem has no preparation
    /// step (e.g. Rust/cargo handles dep fetch transparently during build).
    ///
    /// Implementations should detect their preparation step from filesystem
    /// signals (lockfile presence, manifest contents) and NOT from any
    /// scheduler-level enum, so the plugin remains self-contained.
    ///
    /// Default impl returns `vec![]` so existing implementors don't break;
    /// each ecosystem opts in by overriding.
    fn infer_root_tasks(&self, _workspace_root: &Path) -> Vec<RootTask> {
        Vec::new()
    }
}
