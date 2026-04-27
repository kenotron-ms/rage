//! Ecosystem plugin contract.
//!
//! Per docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 3,
//! ecosystem plugins centralize the declaration burden — they tell rage what
//! TypeScript / Rust / Go / Python packages typically read, write, and emit.
//! User-supplied config augments these defaults.

pub mod types;

pub use types::{AllowlistEntry, OutputFile, PluginConfig, TaskDef};

use std::path::{Path, PathBuf};

/// A package resolved from a lockfile, with its content integrity hash.
///
/// The integrity string is taken verbatim from the lockfile (e.g. `sha512-XXXX`
/// for npm/pnpm/yarn classic, `10c0/sha512hex` for yarn berry). It is used as
/// the basis for the CAS key (`Blake3(integrity.as_bytes())`), making CAS entries
/// deterministic and matching the package manager's own verification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockfilePackage {
    /// npm package name (e.g. `"ms"` or `"@types/node"`).
    pub name: String,
    /// Resolved version (e.g. `"2.1.3"`).
    pub version: String,
    /// Integrity hash string from the lockfile. Format varies by PM:
    /// - pnpm / yarn classic / npm: `sha512-XXXX` or `sha1-XXXX`
    /// - yarn berry: `10c0/sha512hex` (cache-version-prefixed sha512)
    pub integrity: String,
    /// URL of the tarball (optional). Present in pnpm/yarn classic/npm lockfiles.
    pub tarball_url: Option<String>,
}

/// A postinstall script that must run after a package is extracted from the tarball CAS.
///
/// Lifecycle: the scheduler detects packages whose `package.json` declares a `postinstall`
/// script and emits one `PostinstallTask` per such package. The task is run inside
/// `cwd` (i.e. `node_modules/{name}/`) before the workspace package graph is walked.
///
/// ## CAS key
///
/// The CAS key for the resulting outputs is
/// `blake3(tarball_integrity + ":" + platform_triple + ":" + node_version)`,
/// computed by `scheduler::postinstall_cache::postinstall_cas_key`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PostinstallTask {
    /// npm package name (e.g. `"esbuild"` or `"@prisma/client"`).
    pub package_name: String,
    /// Resolved version (e.g. `"0.21.5"`).
    pub version: String,
    /// Lockfile integrity string for this package — basis of the CAS key.
    pub tarball_integrity: String,
    /// Shell command from `package.json:scripts.postinstall`.
    pub script: String,
    /// Working directory for the script — `node_modules/{name}/`.
    pub cwd: PathBuf,
}

/// Minimal artifact store reference used by plugins for CAS restoration.
///
/// Defined here to avoid a direct dependency on the `artifact-store` crate from
/// plugin implementations. `artifact-store` implements this trait for
/// `LocalArtifactStore`.
pub trait ArtifactStoreRef: Send + Sync {
    /// Retrieve bytes stored under `key`. Returns `None` if absent.
    fn get_bytes(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, std::io::Error>;
    /// Cheap existence check for `key`.
    fn contains_key(&self, key: &[u8; 32]) -> bool;
}

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
    /// Extra `(key, value)` pairs folded into the fingerprint hash.
    /// Used by ecosystems to bake environment-derived state (e.g. resolved
    /// Node.js version) into the cache key without making the scheduler
    /// ecosystem-aware. Empty by default.
    pub env_hash_inputs: Vec<(String, String)>,
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

    /// Returns `true` when the on-disk artifacts left behind by this plugin's
    /// root task(s) still exist. Callers use this on a cache hit to decide
    /// whether the cached state is materially valid.
    ///
    /// Default returns `true`: ecosystems that have no installable side-effects
    /// preserve the existing cache-hit behaviour.
    fn verify_install_effects(&self, _workspace_root: &Path) -> bool {
        true
    }

    /// Parse the lockfile(s) and return all **external** packages with integrity hashes.
    ///
    /// Workspace-local packages (those without an integrity hash in the lockfile) MUST be
    /// excluded from the returned list.
    ///
    /// Returns `None` if this ecosystem has no lockfile (e.g. a bare `requirements.txt`).
    /// When `None` is returned, rage skips CAS capture and always runs the install command.
    fn parse_lockfile(&self, _workspace_root: &Path) -> Option<Vec<LockfilePackage>> {
        None
    }

    /// Path to this ecosystem's local package cache (where the PM stores downloaded tarballs).
    ///
    /// Used as the fast-path source during capture: rage copies tarballs from the PM cache
    /// instead of downloading from the registry.
    ///
    /// Returns `None` if the PM cache path cannot be determined or doesn't exist.
    fn local_pm_cache(&self, _workspace_root: &Path) -> Option<PathBuf> {
        None
    }

    /// Restore packages from the rage artifact CAS into the workspace's `node_modules/`.
    ///
    /// Called when: install marker present + `verify_install_effects` returns `false` +
    /// CAS contains tarballs for all packages from `parse_lockfile`.
    ///
    /// Implementations should extract tarballs for each package into
    /// `workspace_root/node_modules/{name}/`, handling scoped packages correctly.
    ///
    /// Default is a no-op — falls through to full reinstall.
    fn restore_from_cas(
        &self,
        _packages: &[LockfilePackage],
        _workspace_root: &Path,
        _store: &dyn ArtifactStoreRef,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }

    /// Returns the set of postinstall scripts that must run after packages are
    /// extracted from the tarball CAS.
    ///
    /// Implementations should:
    ///
    /// 1. **Read the package manager's own script policy** — e.g. yarn's
    ///    `enableScripts`, pnpm's `onlyBuiltDependencies` / `neverBuiltDependencies`,
    ///    or npm's `ignore-scripts` flag — to determine whether scripts are
    ///    globally disabled or selectively filtered.
    /// 2. **Walk `node_modules/`** and find every package whose `package.json`
    ///    declares a `scripts.postinstall` field.
    /// 3. **Filter the list** using the policy resolved in step 1 — discard
    ///    packages whose scripts are suppressed by the policy.
    /// 4. **Look up each package's lockfile integrity hash** (from
    ///    `parse_lockfile`) for use as the CAS key when caching postinstall
    ///    outputs.
    ///
    /// If the PM globally disables scripts → return `vec![]`.
    ///
    /// Default returns `vec![]` so non-TypeScript ecosystems are unaffected.
    fn postinstall_tasks(&self, _workspace_root: &Path) -> Vec<PostinstallTask> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullPlugin;

    impl EcosystemPlugin for NullPlugin {
        fn id(&self) -> &'static str {
            "null"
        }
        fn detection_globs(&self) -> Vec<&'static str> {
            vec![]
        }
        fn infer_tasks(&self, _: &Path) -> Vec<TaskDef> {
            vec![]
        }
        fn toolchain_allowlist(&self) -> Vec<AllowlistEntry> {
            vec![]
        }
        fn declared_input_globs(&self, _: &str, _: &PluginConfig) -> Vec<String> {
            vec![]
        }
        fn abi_fingerprint(&self, _: &[OutputFile]) -> Option<String> {
            None
        }
    }

    #[test]
    fn default_parse_lockfile_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.parse_lockfile(tmp.path()).is_none());
    }

    #[test]
    fn default_local_pm_cache_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.local_pm_cache(tmp.path()).is_none());
    }

    #[test]
    fn postinstall_task_roundtrips_serde() {
        let task = PostinstallTask {
            package_name: "esbuild".to_string(),
            version: "0.21.5".to_string(),
            tarball_integrity: "sha512-xyz".to_string(),
            script: "node install.js".to_string(),
            cwd: PathBuf::from("/tmp/wsp/node_modules/esbuild"),
        };
        let json = serde_json::to_string(&task).unwrap();
        let decoded: PostinstallTask = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, task);
    }

    #[test]
    fn default_postinstall_tasks_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.postinstall_tasks(tmp.path()).is_empty());
    }

    #[test]
    fn lockfile_package_roundtrips_serde() {
        let pkg = LockfilePackage {
            name: "ms".to_string(),
            version: "2.1.3".to_string(),
            integrity: "sha512-abc123".to_string(),
            tarball_url: Some("https://registry.npmjs.org/ms/-/ms-2.1.3.tgz".to_string()),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let decoded: LockfilePackage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "ms");
        assert_eq!(decoded.integrity, "sha512-abc123");
    }
}
