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
/// Scan `base` for a version directory that matches `version`.
///
/// Tries exact match first (`v18.20.4`, `18.20.4`), then falls back to a
/// prefix scan so that a bare major like `"18"` matches `v18.20.8`.
fn resolve_version_dir(base: &Path, version: &str) -> Option<PathBuf> {
    // Strip leading 'v' from both sides for comparison.
    let version_bare = version.trim_start_matches('v');

    // Try exact match first (version might already be full semver like "18.20.8").
    for candidate in [version.to_string(), format!("v{version_bare}")] {
        let p = base.join(&candidate);
        if p.is_dir() {
            return Some(p);
        }
    }

    // Prefix match: given "18" or "18.20", find highest v18.x.y installed.
    // Collect all entries whose bare name starts with version_bare + "."
    // OR whose bare name == version_bare (exact after stripping v).
    let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let bare = name.trim_start_matches('v');
            bare == version_bare || bare.starts_with(&format!("{version_bare}."))
        })
        .collect();

    // Sort descending so highest semver patch wins.
    matches.sort_by(|a, b| b.cmp(a));
    matches.into_iter().next()
}

pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);

    // 1. fnm — honor $FNM_DIR, fall back to ~/.local/share/fnm
    let fnm_root = std::env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share/fnm")));
    if let Some(root) = fnm_root {
        let base = root.join("node-versions");
        if let Some(version_dir) = resolve_version_dir(&base, version) {
            let candidate = version_dir.join("installation/bin");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // 2. nvm — honor $NVM_DIR, fall back to ~/.nvm
    let nvm_root = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".nvm")));
    if let Some(root) = nvm_root {
        let base = root.join("versions/node");
        if let Some(version_dir) = resolve_version_dir(&base, version) {
            let candidate = version_dir.join("bin");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // 3. asdf — ~/.asdf/installs/nodejs/{ver}/bin
    if let Some(h) = &home {
        let base = h.join(".asdf/installs/nodejs");
        if let Some(version_dir) = resolve_version_dir(&base, version) {
            let candidate = version_dir.join("bin");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // 4. mise — ~/.local/share/mise/installs/node/{ver}/bin
    if let Some(h) = &home {
        let base = h.join(".local/share/mise/installs/node");
        if let Some(version_dir) = resolve_version_dir(&base, version) {
            let candidate = version_dir.join("bin");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // Volta on Unix: $VOLTA_HOME/bin or ~/.volta/bin.
    if let Some(ref home) = home {
        let volta_bin = std::env::var_os("VOLTA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".volta"))
            .join("bin");
        if volta_bin.is_dir() {
            return Some(volta_bin);
        }
    }

    #[cfg(windows)]
    {
        // fnm on Windows: %FNM_DIR% (if set) or %LOCALAPPDATA%\fnm.
        // Layout: <fnm>\node-versions\v{ver}\installation\
        // (no `bin/` subdirectory — node.exe sits directly in `installation\`.)
        let fnm_dir = std::env::var_os("FNM_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("fnm"))
            });
        if let Some(fnm_dir) = fnm_dir {
            let versions_dir = fnm_dir.join("node-versions");
            if let Some(ver_dir) = resolve_version_dir(&versions_dir, version) {
                let bin = ver_dir.join("installation");
                if bin.is_dir() {
                    return Some(bin);
                }
            }
        }

        // nvm-windows: %NVM_HOME% or %APPDATA%\nvm.
        // Layout: <nvm>\v{ver}\ (node.exe directly in version dir)
        let nvm_home = std::env::var_os("NVM_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("nvm"))
            });
        if let Some(nvm_dir) = nvm_home {
            if let Some(ver_dir) = resolve_version_dir(&nvm_dir, version) {
                if ver_dir.is_dir() {
                    return Some(ver_dir);
                }
            }
        }

        // Volta on Windows: %VOLTA_HOME%\bin or %LOCALAPPDATA%\Volta\bin.
        let volta_bin = std::env::var_os("VOLTA_HOME")
            .map(|p| PathBuf::from(p).join("bin"))
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("Volta").join("bin"))
            });
        if let Some(bin) = volta_bin {
            if bin.is_dir() {
                return Some(bin);
            }
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

    fn fake_fnm_windows(local_app_data: &Path, version: &str) -> PathBuf {
        let dir = local_app_data
            .join("fnm/node-versions")
            .join(format!("v{version}"))
            .join("installation");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_nvm_windows(app_data: &Path, version: &str) -> PathBuf {
        let dir = app_data.join("nvm").join(format!("v{version}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_volta_windows(local_app_data: &Path) -> PathBuf {
        let dir = local_app_data.join("Volta/bin");
        std::fs::create_dir_all(&dir).unwrap();
        dir
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

    /// Serialises all env-var-mutating tests in this module so they never
    /// race against each other (process-wide env is global mutable state).
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run a closure with `HOME` (and `FNM_DIR`/`NVM_DIR`/`VOLTA_HOME` cleared)
    /// pointing at `home`, restoring the originals afterwards.
    ///
    /// Acquires `ENV_MUTEX` for the duration so concurrent tests cannot
    /// observe inconsistent env state.
    fn with_home<F: FnOnce()>(home: &Path, f: F) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let prev_fnm = std::env::var_os("FNM_DIR");
        let prev_nvm = std::env::var_os("NVM_DIR");
        let prev_volta_home = std::env::var_os("VOLTA_HOME");
        std::env::set_var("HOME", home);
        std::env::remove_var("FNM_DIR");
        std::env::remove_var("NVM_DIR");
        std::env::remove_var("VOLTA_HOME");
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
        match prev_volta_home {
            Some(v) => std::env::set_var("VOLTA_HOME", v),
            None => std::env::remove_var("VOLTA_HOME"),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// Run a closure with `LOCALAPPDATA` and `APPDATA` set to the given paths,
    /// with `FNM_DIR`, `NVM_HOME`, `VOLTA_HOME`, and `USERPROFILE` removed.
    /// Restores all six variables afterwards (panic-safe).
    fn with_windows_env<F: FnOnce()>(local_app_data: &Path, app_data: &Path, f: F) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_local_app_data = std::env::var_os("LOCALAPPDATA");
        let prev_app_data = std::env::var_os("APPDATA");
        let prev_fnm_dir = std::env::var_os("FNM_DIR");
        let prev_nvm_home = std::env::var_os("NVM_HOME");
        let prev_volta_home = std::env::var_os("VOLTA_HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");

        std::env::set_var("LOCALAPPDATA", local_app_data);
        std::env::set_var("APPDATA", app_data);
        std::env::remove_var("FNM_DIR");
        std::env::remove_var("NVM_HOME");
        std::env::remove_var("VOLTA_HOME");
        std::env::remove_var("USERPROFILE");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

        match prev_local_app_data {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        match prev_app_data {
            Some(v) => std::env::set_var("APPDATA", v),
            None => std::env::remove_var("APPDATA"),
        }
        match prev_fnm_dir {
            Some(v) => std::env::set_var("FNM_DIR", v),
            None => std::env::remove_var("FNM_DIR"),
        }
        match prev_nvm_home {
            Some(v) => std::env::set_var("NVM_HOME", v),
            None => std::env::remove_var("NVM_HOME"),
        }
        match prev_volta_home {
            Some(v) => std::env::set_var("VOLTA_HOME", v),
            None => std::env::remove_var("VOLTA_HOME"),
        }
        match prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
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

    // ── resolve_version_dir ───────────────────────────────────────────────────

    #[test]
    fn resolve_version_dir_matches_major_only() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulate fnm layout: v18.20.8 installed
        std::fs::create_dir_all(tmp.path().join("v18.20.8")).unwrap();
        std::fs::create_dir_all(tmp.path().join("v20.11.0")).unwrap();

        // "18" should resolve to v18.20.8
        let result = resolve_version_dir(tmp.path(), "18").unwrap();
        assert_eq!(result.file_name().unwrap(), "v18.20.8");

        // "v18" should also resolve
        let result2 = resolve_version_dir(tmp.path(), "v18").unwrap();
        assert_eq!(result2.file_name().unwrap(), "v18.20.8");

        // "18.20" should resolve to v18.20.8
        let result3 = resolve_version_dir(tmp.path(), "18.20").unwrap();
        assert_eq!(result3.file_name().unwrap(), "v18.20.8");

        // exact full semver still works
        let result4 = resolve_version_dir(tmp.path(), "v18.20.8").unwrap();
        assert_eq!(result4.file_name().unwrap(), "v18.20.8");

        // non-matching returns None
        assert!(resolve_version_dir(tmp.path(), "16").is_none());
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

    // ── Windows-specific version-manager tests ───────────────────────────────

    #[test]
    #[cfg(windows)]
    fn finds_fnm_windows_default_path() {
        let tmp = tempdir().unwrap();
        let local_app_data = tmp.path().join("LocalAppData");
        let app_data = tmp.path().join("AppData");
        std::fs::create_dir_all(&local_app_data).unwrap();
        std::fs::create_dir_all(&app_data).unwrap();
        let expected = fake_fnm_windows(&local_app_data, "18.20.4");
        with_windows_env(&local_app_data, &app_data, || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    #[cfg(windows)]
    fn finds_fnm_windows_via_fnm_dir_env() {
        let tmp = tempdir().unwrap();
        let local_app_data = tmp.path().join("LocalAppData");
        let app_data = tmp.path().join("AppData");
        let custom_fnm = tmp.path().join("custom_fnm");
        std::fs::create_dir_all(&local_app_data).unwrap();
        std::fs::create_dir_all(&app_data).unwrap();
        let bin = custom_fnm
            .join("node-versions")
            .join("v18.20.4")
            .join("installation");
        std::fs::create_dir_all(&bin).unwrap();
        with_windows_env(&local_app_data, &app_data, || {
            let prev_fnm_dir = std::env::var_os("FNM_DIR");
            std::env::set_var("FNM_DIR", &custom_fnm);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_eq!(find_version_manager_bin("18.20.4"), Some(bin.clone()));
            }));
            match prev_fnm_dir {
                Some(v) => std::env::set_var("FNM_DIR", v),
                None => std::env::remove_var("FNM_DIR"),
            }
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        });
    }

    #[test]
    #[cfg(windows)]
    fn finds_nvm_windows_default_path() {
        let tmp = tempdir().unwrap();
        let local_app_data = tmp.path().join("LocalAppData");
        let app_data = tmp.path().join("AppData");
        std::fs::create_dir_all(&local_app_data).unwrap();
        std::fs::create_dir_all(&app_data).unwrap();
        let expected = fake_nvm_windows(&app_data, "18.20.4");
        with_windows_env(&local_app_data, &app_data, || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    #[cfg(windows)]
    fn finds_volta_windows() {
        let tmp = tempdir().unwrap();
        let local_app_data = tmp.path().join("LocalAppData");
        let app_data = tmp.path().join("AppData");
        std::fs::create_dir_all(&local_app_data).unwrap();
        std::fs::create_dir_all(&app_data).unwrap();
        let expected = fake_volta_windows(&local_app_data);
        with_windows_env(&local_app_data, &app_data, || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn finds_volta_unix() {
        let home = tempfile::tempdir().unwrap();
        let volta_bin = home.path().join(".volta").join("bin");
        std::fs::create_dir_all(&volta_bin).unwrap();
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(volta_bin.clone()));
        });
    }

    // ── Cross-platform USERPROFILE fallback test ─────────────────────────────

    #[test]
    fn userprofile_used_as_home_fallback() {
        let tmp = tempdir().unwrap();
        let userprofile = tmp.path().join("userprofile");
        let fnm_bin = userprofile
            .join(".local/share/fnm/node-versions/v18.20.4/installation/bin");
        std::fs::create_dir_all(&fnm_bin).unwrap();

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        let prev_fnm_dir = std::env::var_os("FNM_DIR");

        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", &userprofile);
        std::env::remove_var("FNM_DIR");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(
                find_version_manager_bin("18.20.4"),
                Some(fnm_bin.clone())
            );
        }));

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        match prev_fnm_dir {
            Some(v) => std::env::set_var("FNM_DIR", v),
            None => std::env::remove_var("FNM_DIR"),
        }

        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
