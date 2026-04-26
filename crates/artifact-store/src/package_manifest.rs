//! Per-workspace package manifest — what got captured, what was its content.

use crate::ContentHash;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageArtifact {
    pub name: String,
    pub version: String,
    /// (relative_path_within_package, content_hash) pairs.
    /// Paths are relative to the package root (e.g. "index.js", "lib/foo.js").
    pub files: Vec<(PathBuf, ContentHash)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspacePackageManifest {
    /// Unix timestamp (seconds) when this manifest was last updated.
    pub captured_at: u64,
    /// The root-task install fingerprint this manifest is keyed against.
    /// Switching lockfiles produces a new fingerprint and thus a fresh manifest.
    pub install_fingerprint: String,
    pub packages: Vec<PackageArtifact>,
}

/// Reference to a single installed package on disk. A free-standing copy of
/// `plugin_typescript::pathset_extractor::PathsetPackageRef` so the
/// `artifact-store` crate stays free of plugin dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsetPackageRef {
    pub name: String,
    pub version: String,
    pub package_root: std::path::PathBuf,
}
