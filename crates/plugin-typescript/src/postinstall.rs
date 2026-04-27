//! PM script-policy reader for the TypeScript plugin.
//!
//! Reads workspace configuration files (`.yarnrc.yml`, `.npmrc`, `package.json`)
//! to determine which packages are allowed to run postinstall scripts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────

/// Describes how the package manager is configured to handle postinstall scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPolicy {
    /// PM has globally disabled scripts (`enableScripts: false` / `ignore-scripts=true`).
    AllDisabled,
    /// pnpm `onlyBuiltDependencies` — only these packages may run scripts.
    Allowlist(HashSet<String>),
    /// pnpm `neverBuiltDependencies` — these packages are blocked from running scripts.
    NeverList(HashSet<String>),
    /// Default: PM runs all postinstall scripts.
    AllEnabled,
}

/// A single postinstall task discovered for a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPostinstallTask {
    pub package_name: String,
    pub script: String,
    pub cwd: PathBuf,
}

// ── Policy reader ─────────────────────────────────────────────────────────────

/// Read the PM script policy from the workspace root.
///
/// Precedence (first match wins):
/// 1. `.yarnrc.yml` with `enableScripts: false` → [`ScriptPolicy::AllDisabled`]
/// 2. `.npmrc` with `ignore-scripts=true` (case-insensitive) → [`ScriptPolicy::AllDisabled`]
/// 3. `package.json` `pnpm.onlyBuiltDependencies` array → [`ScriptPolicy::Allowlist`]
/// 4. `package.json` `pnpm.neverBuiltDependencies` array → [`ScriptPolicy::NeverList`]
/// 5. otherwise → [`ScriptPolicy::AllEnabled`]
pub fn read_pm_script_policy(workspace_root: &Path) -> ScriptPolicy {
    // 1. .yarnrc.yml — enableScripts: false
    if let Ok(content) = std::fs::read_to_string(workspace_root.join(".yarnrc.yml")) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("enableScripts:") {
                if rest.trim() == "false" {
                    return ScriptPolicy::AllDisabled;
                }
            }
        }
    }

    // 2. .npmrc — ignore-scripts=true (case-insensitive)
    if let Ok(content) = std::fs::read_to_string(workspace_root.join(".npmrc")) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(eq_pos) = trimmed.find('=') {
                let key = trimmed[..eq_pos].trim();
                let raw_value = trimmed[eq_pos + 1..].trim();
                // Trim any leading `=` characters from the value.
                let value = raw_value.trim_start_matches('=').trim();
                if key.eq_ignore_ascii_case("ignore-scripts")
                    && value.eq_ignore_ascii_case("true")
                {
                    return ScriptPolicy::AllDisabled;
                }
            }
        }
    }

    // 3 & 4. package.json — pnpm.onlyBuiltDependencies / pnpm.neverBuiltDependencies
    if let Ok(content) = std::fs::read_to_string(workspace_root.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            // onlyBuiltDependencies (checked first — higher priority)
            if let Some(arr) = json
                .get("pnpm")
                .and_then(|p| p.get("onlyBuiltDependencies"))
                .and_then(|v| v.as_array())
            {
                let set: HashSet<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                return ScriptPolicy::Allowlist(set);
            }

            // neverBuiltDependencies
            if let Some(arr) = json
                .get("pnpm")
                .and_then(|p| p.get("neverBuiltDependencies"))
                .and_then(|v| v.as_array())
            {
                let set: HashSet<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                return ScriptPolicy::NeverList(set);
            }
        }
    }

    ScriptPolicy::AllEnabled
}

// ── Scanner ───────────────────────────────────────────────────────────────────────────────────

/// Walk `workspace_root/node_modules/` and return one [`RawPostinstallTask`] for
/// each package that declares `scripts.postinstall` in its `package.json`.
///
/// Rules:
/// * Hidden directories (name starts with `.`) are skipped.
/// * Scoped packages (name starts with `@`) are descended one level; the full
///   package name is `"{scope}/{subname}"`.
/// * Non-directory entries are ignored.
/// * Returns an empty [`Vec`] when `node_modules/` does not exist.
pub fn scan_postinstall_scripts(workspace_root: &Path) -> Vec<RawPostinstallTask> {
    let node_modules = workspace_root.join("node_modules");
    let Ok(entries) = std::fs::read_dir(&node_modules) else {
        return Vec::new();
    };

    let mut tasks = Vec::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip hidden directories.
        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Resolve symlinks so we only process directories.
        let is_dir = if file_type.is_symlink() {
            path.is_dir()
        } else {
            file_type.is_dir()
        };
        if !is_dir {
            continue;
        }

        if name.starts_with('@') {
            // Scoped: recurse one level.
            let scope = name.to_string();
            let Ok(inner_entries) = std::fs::read_dir(&path) else {
                continue;
            };
            for inner_entry in inner_entries.flatten() {
                let inner_name = inner_entry.file_name();
                let subname = inner_name.to_string_lossy();
                if subname.starts_with('.') {
                    continue;
                }
                let inner_path = inner_entry.path();
                let Ok(inner_ft) = inner_entry.file_type() else {
                    continue;
                };
                let inner_is_dir = if inner_ft.is_symlink() {
                    inner_path.is_dir()
                } else {
                    inner_ft.is_dir()
                };
                if !inner_is_dir {
                    continue;
                }
                if let Some(script) = read_postinstall_field(&inner_path) {
                    tasks.push(RawPostinstallTask {
                        package_name: format!("{scope}/{subname}"),
                        script,
                        cwd: inner_path,
                    });
                }
            }
        } else {
            // Regular (non-scoped) package.
            if let Some(script) = read_postinstall_field(&path) {
                tasks.push(RawPostinstallTask {
                    package_name: name.to_string(),
                    script,
                    cwd: path,
                });
            }
        }
    }

    tasks
}

/// Read `pkg_dir/package.json` and return the `scripts.postinstall` value if it
/// is present and non-empty (after trimming whitespace).
fn read_postinstall_field(pkg_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let script = json
        .get("scripts")?
        .get("postinstall")?
        .as_str()?
        .trim()
        .to_string();
    if script.is_empty() {
        None
    } else {
        Some(script)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn yarnrc_yml_with_enable_scripts_false_disables() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".yarnrc.yml"),
            "# yarn configuration\nenableScripts: false\n",
        )
        .unwrap();
        assert_eq!(
            read_pm_script_policy(dir.path()),
            ScriptPolicy::AllDisabled
        );
    }

    #[test]
    fn npmrc_ignore_scripts_disables() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".npmrc"),
            "; npm config\nignore-scripts=true\n",
        )
        .unwrap();
        assert_eq!(
            read_pm_script_policy(dir.path()),
            ScriptPolicy::AllDisabled
        );
    }

    #[test]
    fn pnpm_only_built_dependencies_is_allowlist() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"w","pnpm":{"onlyBuiltDependencies":["esbuild","bcrypt"]}}"#,
        )
        .unwrap();
        let policy = read_pm_script_policy(dir.path());
        match policy {
            ScriptPolicy::Allowlist(set) => {
                assert_eq!(set.len(), 2);
                assert!(set.contains("esbuild"));
                assert!(set.contains("bcrypt"));
            }
            other => panic!("expected Allowlist, got {other:?}"),
        }
    }

    #[test]
    fn pnpm_never_built_dependencies_is_neverlist() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"pnpm":{"neverBuiltDependencies":["sketchy-pkg"]}}"#,
        )
        .unwrap();
        let policy = read_pm_script_policy(dir.path());
        match policy {
            ScriptPolicy::NeverList(set) => {
                assert!(set.contains("sketchy-pkg"));
            }
            other => panic!("expected NeverList, got {other:?}"),
        }
    }

    #[test]
    fn empty_workspace_is_all_enabled() {
        let dir = tempdir().unwrap();
        assert_eq!(read_pm_script_policy(dir.path()), ScriptPolicy::AllEnabled);
    }

    // ── Test helper ─────────────────────────────────────────────────────────────

    /// Create `node_modules/{name}/package.json` under `root`.
    ///
    /// When `postinstall` is `Some(script)` the manifest includes
    /// `{"scripts":{"postinstall":"<script>"}}`.  Otherwise the manifest has no
    /// `scripts` key.
    fn write_pkg(root: &Path, name: &str, postinstall: Option<&str>) {
        // `name` may be scoped, e.g. `@prisma/client` — create the nested dir.
        let pkg_dir = root.join("node_modules").join(name);
        std::fs::create_dir_all(&pkg_dir).unwrap();

        let manifest = if let Some(script) = postinstall {
            format!(
                r#"{{"name":"{name}","scripts":{{"postinstall":"{script}"}}}}"#
            )
        } else {
            format!(r#"{{"name":"{name}"}}"#)
        };

        std::fs::write(pkg_dir.join("package.json"), manifest).unwrap();
    }

    #[test]
    fn scan_returns_only_packages_with_postinstall() {
        let dir = tempdir().unwrap();
        write_pkg(dir.path(), "esbuild", Some("node install.js"));
        write_pkg(dir.path(), "lodash", None);

        let mut results = scan_postinstall_scripts(dir.path());
        assert_eq!(results.len(), 1, "expected exactly one result, got: {results:?}");
        let task = results.pop().unwrap();
        assert_eq!(task.package_name, "esbuild");
        assert_eq!(task.script, "node install.js");
    }

    #[test]
    fn scan_handles_scoped_packages() {
        let dir = tempdir().unwrap();
        write_pkg(dir.path(), "@prisma/client", Some("prisma generate"));

        let results = scan_postinstall_scripts(dir.path());
        assert_eq!(results.len(), 1, "expected exactly one result, got: {results:?}");
        assert_eq!(results[0].package_name, "@prisma/client");
        assert_eq!(results[0].script, "prisma generate");
    }

    #[test]
    fn scan_skips_hidden_dirs() {
        let dir = tempdir().unwrap();
        // Create a hidden directory (e.g. .bin) with a package.json that has a postinstall.
        let bin_dir = dir.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(
            bin_dir.join("package.json"),
            r#"{"name":".bin","scripts":{"postinstall":"should-be-skipped"}}"#,
        )
        .unwrap();
        // Normal package with postinstall.
        write_pkg(dir.path(), "ok", Some("noop"));

        let results = scan_postinstall_scripts(dir.path());
        assert_eq!(results.len(), 1, "expected exactly one result, got: {results:?}");
        assert_eq!(results[0].package_name, "ok");
    }

}