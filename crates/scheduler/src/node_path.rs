//! PATH construction for JS tasks.
//!
//! Prepends, in priority order:
//!   1. `{task_cwd}/node_modules/.bin`             — package-local dev tools
//!   2. `{workspace_root}/node_modules/.bin`        — workspace-root dev tools
//!   3. Active Node.js version-manager bin dir      — yarn, npm, node themselves
//!      (resolved from `.node-version` / `.nvmrc` / `.tool-versions` at the
//!      workspace root, then looked up under fnm / nvm / asdf / mise)
//!   4. Existing system PATH
//!
//! Without (3), `yarn` and `node` are invisible to subprocesses when the user
//! manages Node via fnm/nvm: those tools live under per-version directories
//! that are normally injected only by the shell hook.

use std::path::{Path, PathBuf};

/// Resolve the Node.js version string declared by the workspace.
///
/// Lookup order:
///   1. `.node-version`   (fnm default, also honored by mise/asdf via plugin)
///   2. `.nvmrc`          (nvm, single line; may include a leading "v")
///   3. `.tool-versions`  (asdf / mise; only the `nodejs <ver>` line)
///
/// Returns `None` if no version file exists. The returned string is whatever
/// the file contains, trimmed: e.g. `"18.20.4"`, `"v20.11.0"`, `"lts/iron"`.
/// Callers must handle a possible leading `v` themselves.
pub fn resolve_node_version(_workspace_root: &Path) -> Option<String> {
    None // intentionally unimplemented — see Task 2
}

/// Locate the `bin/` directory for `version` under whichever supported version
/// manager has that version installed.
///
/// Check order:
///   1. fnm  — `$FNM_DIR/node-versions/v{ver}/installation/bin`
///             (defaults to `~/.local/share/fnm`)
///   2. nvm  — `$NVM_DIR/versions/node/v{ver}/bin`
///             (defaults to `~/.nvm`)
///   3. asdf — `~/.asdf/installs/nodejs/{ver}/bin`
///   4. mise — `~/.local/share/mise/installs/node/{ver}/bin`
///
/// `version` is taken verbatim except a single leading `v` is stripped before
/// constructing the candidate paths (so `"v18.20.4"` and `"18.20.4"` both work).
pub fn find_version_manager_bin(_version: &str) -> Option<PathBuf> {
    None // intentionally unimplemented — see Task 4
}

/// Build the PATH value for a JS task spawn (see module docs).
///
/// `system_path` is the existing `PATH` value to append after the prepended
/// directories. Pass `&std::env::var("PATH").unwrap_or_default()` in normal use.
///
/// Only directories that exist on disk are added — non-existent prepends would
/// just be noise.
pub fn build_node_path(
    _task_cwd: &Path,
    _workspace_root: &Path,
    _system_path: &str,
) -> String {
    String::new() // intentionally unimplemented — see Task 4
}

/// Resolve the tool path from the first whitespace-separated token of `command`.
///
/// Lookup order (first match wins):
///   1. `{cwd}/node_modules/.bin/{token}`
///   2. `{workspace_root}/node_modules/.bin/{token}` (skipped when equal to (1))
///   3. Active version manager bin dir / `{token}`
///   4. Each directory in the system PATH
///
/// If `token` contains `/`, it is returned verbatim (treated as an explicit
/// absolute or relative path).
pub fn which_first(
    _command: &str,
    _cwd: &Path,
    _workspace_root: &Path,
) -> Option<PathBuf> {
    None // intentionally unimplemented — see Task 6
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── resolve_node_version ──────────────────────────────────────────────────

    #[test]
    fn resolve_node_version_reads_dot_node_version() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_reads_nvmrc_when_no_dot_node_version() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "v20.11.0\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("v20.11.0".to_string())
        );
    }

    #[test]
    fn resolve_node_version_prefers_dot_node_version_over_nvmrc() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "20.11.0\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_reads_tool_versions_nodejs_line() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".tool-versions"),
            "python 3.12.4\nnodejs 18.20.4\nrust 1.91.0\n",
        )
        .unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_returns_none_when_no_files() {
        let dir = tempdir().unwrap();
        assert_eq!(resolve_node_version(dir.path()), None);
    }

    #[test]
    fn resolve_node_version_trims_whitespace_and_blank_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "  18.20.4  \n\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }
}
