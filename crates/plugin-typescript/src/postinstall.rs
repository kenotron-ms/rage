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

// ── Tests ─────────────────────────────────────────────────────────────────────

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
}
