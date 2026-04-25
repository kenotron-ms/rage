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
pub fn resolve_node_version(workspace_root: &Path) -> Option<String> {
    // 1. .node-version — fnm/mise default, single line.
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".node-version")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }

    // 2. .nvmrc — nvm, single line; may include a leading "v".
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".nvmrc")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }

    // 3. .tool-versions — asdf/mise multi-tool format. We only care about
    //    the line beginning with `nodejs ` (or `node `). First whitespace-
    //    separated token after the tool name is the version.
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".tool-versions")) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let tool = parts.next().unwrap_or("");
            if tool == "nodejs" || tool == "node" {
                if let Some(v) = parts.next() {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }

    None
}

/// Locate the `bin/` directory for `version` under whichever supported version
/// manager has that version installed.
///
/// Check order:
///   1. fnm  — `$FNM_DIR/node-versions/v{ver}/installation/bin`
///      (defaults to `~/.local/share/fnm`)
///   2. nvm  — `$NVM_DIR/versions/node/v{ver}/bin`
///      (defaults to `~/.nvm`)
///   3. asdf — `~/.asdf/installs/nodejs/{ver}/bin`
///   4. mise — `~/.local/share/mise/installs/node/{ver}/bin`
///
/// `version` is taken verbatim except a single leading `v` is stripped before
/// constructing the candidate paths (so `"v18.20.4"` and `"18.20.4"` both work).
pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let v_no_prefix = version.strip_prefix('v').unwrap_or(version);
    let v_with_prefix = format!("v{v_no_prefix}");

    let home = std::env::var_os("HOME").map(PathBuf::from);

    // 1. fnm — honor $FNM_DIR, fall back to ~/.local/share/fnm
    let fnm_root = std::env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share/fnm")));
    if let Some(root) = fnm_root {
        let candidate = root
            .join("node-versions")
            .join(&v_with_prefix)
            .join("installation/bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 2. nvm — honor $NVM_DIR, fall back to ~/.nvm
    let nvm_root = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".nvm")));
    if let Some(root) = nvm_root {
        let candidate = root.join("versions/node").join(&v_with_prefix).join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 3. asdf — ~/.asdf/installs/nodejs/{ver}/bin (no leading 'v')
    if let Some(h) = &home {
        let candidate = h
            .join(".asdf/installs/nodejs")
            .join(v_no_prefix)
            .join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 4. mise — ~/.local/share/mise/installs/node/{ver}/bin (no leading 'v')
    if let Some(h) = &home {
        let candidate = h
            .join(".local/share/mise/installs/node")
            .join(v_no_prefix)
            .join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    None
}

/// Build the PATH value for a JS task spawn (see module docs).
///
/// `system_path` is the existing `PATH` value to append after the prepended
/// directories. Pass `&std::env::var("PATH").unwrap_or_default()` in normal use.
///
/// Only directories that exist on disk are added — non-existent prepends would
/// just be noise.
pub fn build_node_path(task_cwd: &Path, workspace_root: &Path, system_path: &str) -> String {
    #[cfg(unix)]
    const SEP: char = ':';
    #[cfg(windows)]
    const SEP: char = ';';

    let mut prepend: Vec<PathBuf> = Vec::new();

    // 1. {task_cwd}/node_modules/.bin
    let pkg_bin = task_cwd.join("node_modules/.bin");
    if pkg_bin.is_dir() {
        prepend.push(pkg_bin.clone());
    }

    // 2. {workspace_root}/node_modules/.bin (skip if same as #1)
    let ws_bin = workspace_root.join("node_modules/.bin");
    if ws_bin != pkg_bin && ws_bin.is_dir() {
        prepend.push(ws_bin);
    }

    // 3. Active Node.js version manager bin dir
    if let Some(version) = resolve_node_version(workspace_root) {
        if let Some(vm_bin) = find_version_manager_bin(&version) {
            prepend.push(vm_bin);
        }
    }

    // Concatenate: prepended dirs + system PATH.
    let mut out = String::new();
    for dir in &prepend {
        out.push_str(&dir.to_string_lossy());
        out.push(SEP);
    }
    out.push_str(system_path);
    out
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
pub fn which_first(command: &str, cwd: &Path, workspace_root: &Path) -> Option<PathBuf> {
    let first = command.split_whitespace().next()?;
    if first.contains('/') {
        return Some(PathBuf::from(first));
    }

    // 1. {cwd}/node_modules/.bin/{first}
    let pkg_candidate = cwd.join("node_modules/.bin").join(first);
    if pkg_candidate.is_file() {
        return Some(pkg_candidate);
    }

    // 2. {workspace_root}/node_modules/.bin/{first}
    if workspace_root != cwd {
        let ws_candidate = workspace_root.join("node_modules/.bin").join(first);
        if ws_candidate.is_file() {
            return Some(ws_candidate);
        }
    }

    // 3. Active Node.js version manager bin dir
    if let Some(version) = resolve_node_version(workspace_root) {
        if let Some(vm_bin) = find_version_manager_bin(&version) {
            let candidate = vm_bin.join(first);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 4. System PATH
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(first);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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

    // ── find_version_manager_bin ──────────────────────────────────────────────

    /// Build a fake fnm tree under `home`, return the bin dir we expect to find.
    fn fake_fnm(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".local/share/fnm/node-versions")
            .join(format!("v{version}"))
            .join("installation/bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_nvm(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".nvm/versions/node")
            .join(format!("v{version}"))
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_asdf(home: &Path, version: &str) -> PathBuf {
        let bin = home.join(".asdf/installs/nodejs").join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_mise(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".local/share/mise/installs/node")
            .join(version)
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    /// Run a closure with `HOME` (and `FNM_DIR`/`NVM_DIR` cleared) pointing
    /// at `home`, restoring the originals afterwards.
    ///
    /// NOTE: edition 2021 — set_var / remove_var are safe functions here.
    /// Tests in this module must be run sequentially (default for `cargo test --lib`
    /// with no explicit `#[test(parallel)]`).
    fn with_home<F: FnOnce()>(home: &Path, f: F) {
        let prev_home = std::env::var_os("HOME");
        let prev_fnm = std::env::var_os("FNM_DIR");
        let prev_nvm = std::env::var_os("NVM_DIR");
        std::env::set_var("HOME", home);
        std::env::remove_var("FNM_DIR");
        std::env::remove_var("NVM_DIR");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_fnm {
            Some(v) => std::env::set_var("FNM_DIR", v),
            None => std::env::remove_var("FNM_DIR"),
        }
        match prev_nvm {
            Some(v) => std::env::set_var("NVM_DIR", v),
            None => std::env::remove_var("NVM_DIR"),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn find_version_manager_bin_finds_fnm() {
        let home = tempdir().unwrap();
        let expected = fake_fnm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_strips_leading_v() {
        let home = tempdir().unwrap();
        let expected = fake_fnm(home.path(), "20.11.0");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("v20.11.0"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_nvm() {
        let home = tempdir().unwrap();
        let expected = fake_nvm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_asdf() {
        let home = tempdir().unwrap();
        let expected = fake_asdf(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_mise() {
        let home = tempdir().unwrap();
        let expected = fake_mise(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_returns_none_when_nothing_installed() {
        let home = tempdir().unwrap();
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), None);
        });
    }

    #[test]
    fn find_version_manager_bin_prefers_fnm_over_nvm() {
        let home = tempdir().unwrap();
        let fnm_bin = fake_fnm(home.path(), "18.20.4");
        let _nvm_bin = fake_nvm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(fnm_bin.clone()));
        });
    }

    // ── build_node_path ───────────────────────────────────────────────────────

    #[test]
    fn build_node_path_prepends_pkg_then_workspace_then_existing() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("packages/foo");
        let pkg_bin = pkg.join("node_modules/.bin");
        let ws_bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&pkg_bin).unwrap();
        std::fs::create_dir_all(&ws_bin).unwrap();

        let path = build_node_path(&pkg, ws.path(), "/usr/bin:/bin");
        let parts: Vec<&str> = path.split(':').collect();
        assert_eq!(parts[0], pkg_bin.to_str().unwrap());
        assert_eq!(parts[1], ws_bin.to_str().unwrap());
        assert!(parts.contains(&"/usr/bin"));
        assert!(parts.contains(&"/bin"));
    }

    #[test]
    fn build_node_path_omits_nonexistent_node_modules() {
        // pkg/node_modules/.bin doesn't exist — must not appear in PATH.
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("packages/foo");
        std::fs::create_dir_all(&pkg).unwrap(); // pkg dir but no node_modules

        let path = build_node_path(&pkg, ws.path(), "/usr/bin");
        assert!(
            !path.contains("node_modules/.bin"),
            "non-existent node_modules/.bin must not be on PATH; got {path}"
        );
    }

    #[test]
    fn build_node_path_dedupes_when_pkg_equals_workspace() {
        let ws = tempdir().unwrap();
        let bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();

        let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
        let count = path.matches(bin.to_str().unwrap()).count();
        assert_eq!(count, 1, "node_modules/.bin must appear exactly once");
    }

    #[test]
    fn build_node_path_includes_version_manager_bin_when_node_version_set() {
        let home = tempdir().unwrap();
        let vm_bin = fake_fnm(home.path(), "18.20.4");

        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                path.contains(vm_bin.to_str().unwrap()),
                "expected fnm bin {} in PATH, got: {path}",
                vm_bin.display()
            );
        });
    }

    #[test]
    fn build_node_path_skips_version_manager_when_no_version_file() {
        let home = tempdir().unwrap();
        let _vm_bin = fake_fnm(home.path(), "18.20.4");

        let ws = tempdir().unwrap();
        // No .node-version file — VM bin must NOT be added.

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                !path.contains("fnm"),
                "no .node-version means no VM bin on PATH; got: {path}"
            );
        });
    }

    #[test]
    fn build_node_path_skips_version_manager_when_version_not_installed() {
        // .node-version says 18.20.4 but no version manager has it — fall through.
        let home = tempdir().unwrap();
        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                !path.contains("fnm") && !path.contains(".nvm") && !path.contains(".asdf"),
                "missing VM install must yield no VM bin; got: {path}"
            );
        });
    }

    // ── which_first ───────────────────────────────────────────────────────────

    #[test]
    fn which_first_returns_path_token_verbatim() {
        let dir = tempdir().unwrap();
        let result = which_first("/bin/sh -c foo", dir.path(), dir.path());
        assert_eq!(result, Some(PathBuf::from("/bin/sh")));
    }

    #[test]
    fn which_first_finds_in_pkg_node_modules_bin() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("pkg");
        let bin = pkg.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tsc = bin.join("tsc");
        std::fs::write(&tsc, b"#!/bin/sh\n").unwrap();

        let result = which_first("tsc --noEmit", &pkg, ws.path());
        assert_eq!(result, Some(tsc));
    }

    #[test]
    fn which_first_falls_back_to_workspace_node_modules_bin() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        let ws_bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&ws_bin).unwrap();
        let yarn = ws_bin.join("yarn");
        std::fs::write(&yarn, b"#!/bin/sh\n").unwrap();

        let result = which_first("yarn install", &pkg, ws.path());
        assert_eq!(result, Some(yarn));
    }

    #[test]
    fn which_first_falls_back_to_version_manager_bin() {
        let home = tempdir().unwrap();
        let vm_bin = fake_fnm(home.path(), "18.20.4");
        let yarn = vm_bin.join("yarn");
        std::fs::write(&yarn, b"#!/bin/sh\n").unwrap();

        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let result = which_first("yarn install", ws.path(), ws.path());
            assert_eq!(result, Some(yarn.clone()));
        });
    }

    #[test]
    fn which_first_returns_none_when_token_not_found() {
        let dir = tempdir().unwrap();
        let result = which_first("definitely_not_a_real_tool_xyz", dir.path(), dir.path());
        assert_eq!(result, None);
    }
}
