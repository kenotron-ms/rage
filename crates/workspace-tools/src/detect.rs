//! Detect which JS package manager a workspace uses.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Pnpm,
    Yarn,
    Npm,
}

impl PackageManager {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Npm => "npm",
        }
    }
}

/// Detect the package manager for the workspace at `root`.
///
/// Returns `None` if the directory doesn't look like a JS workspace at all
/// (no `package.json` with `workspaces`, no `pnpm-workspace.yaml`).
pub fn detect_package_manager(root: &Path) -> Option<PackageManager> {
    // pnpm wins if pnpm-workspace.yaml exists
    if root.join("pnpm-workspace.yaml").exists() {
        return Some(PackageManager::Pnpm);
    }

    // Otherwise we need a package.json with a `workspaces` field
    let pkg_json_path = root.join("package.json");
    let raw = std::fs::read_to_string(&pkg_json_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed.get("workspaces")?;

    // Prefer yarn if lockfile or .yarnrc.yml present
    if root.join("yarn.lock").exists() || root.join(".yarnrc.yml").exists() {
        return Some(PackageManager::Yarn);
    }

    // Fall back to npm
    Some(PackageManager::Npm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .join("fixtures")
    }

    #[test]
    fn detects_pnpm() {
        let dir = fixtures_dir().join("js-pnpm");
        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Pnpm));
    }

    #[test]
    fn detects_yarn() {
        let dir = fixtures_dir().join("js-yarn");
        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Yarn));
    }

    #[test]
    fn detects_npm() {
        let dir = fixtures_dir().join("js-npm");
        assert_eq!(detect_package_manager(&dir), Some(PackageManager::Npm));
    }

    #[test]
    fn returns_none_for_non_workspace() {
        let dir = PathBuf::from("/tmp");
        assert_eq!(detect_package_manager(&dir), None);
    }
}
