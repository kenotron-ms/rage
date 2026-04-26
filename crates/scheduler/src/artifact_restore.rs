//! CAS-backed restoration of a workspace's installed packages.

use artifact_store::{ArtifactError, ArtifactStore, LocalArtifactStore, PackageArtifact};
use std::path::Path;

/// Attempt to restore `node_modules/` from the per-package artifact directory.
///
/// The directory contains one JSON file per captured package (written
/// incrementally so partial captures are still usable). Each file is a
/// `PackageArtifact` serialised as JSON.
///
/// Returns:
/// - `Ok(true)`  — every package in the directory was hardlinked back into place.
/// - `Ok(false)` — the directory is missing, empty, or the CAS does not contain
///   every required hash (partial restore is forbidden — never run).
/// - `Err(_)`    — unexpected I/O failure while reading the directory.
pub fn try_restore_from_cas(
    artifact_dir: &Path,
    workspace_root: &Path,
    store: &LocalArtifactStore,
) -> Result<bool, ArtifactError> {
    if !artifact_dir.is_dir() {
        return Ok(false);
    }

    let mut packages: Vec<PackageArtifact> = Vec::new();
    for entry in std::fs::read_dir(artifact_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        if let Ok(artifact) = serde_json::from_str::<PackageArtifact>(&text) {
            packages.push(artifact);
        }
        // Skip files that don't parse as PackageArtifact — same as a miss.
    }

    if packages.is_empty() {
        return Ok(false);
    }

    // Pre-flight: every required hash must be present BEFORE we touch the
    // workspace. Partial restores are silent corruption — never allowed.
    for pkg in &packages {
        for (_, hash) in &pkg.files {
            if !store.contains(hash) {
                return Ok(false);
            }
        }
    }

    // All present — restore.
    let nm = workspace_root.join("node_modules");
    for pkg in &packages {
        artifact_store::restore_package(pkg, &nm, store)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use artifact_store::{
        capture_package, ContentHash, LocalArtifactStore, PackageArtifact, PathsetPackageRef,
        WorkspacePackageManifest,
    };
    use std::path::PathBuf;

    // ── NEW per-package directory tests (RED) ─────────────────────────────────

    #[test]
    fn restore_returns_false_for_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_dir = tmp.path().join("artifact-packages/fp123");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let store = LocalArtifactStore::new(tmp.path().join("content"));
        let result = try_restore_from_cas(&empty_dir, tmp.path(), &store).unwrap();
        assert!(!result, "empty dir should return false");
    }

    #[test]
    fn restore_reads_per_package_files_from_directory() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        // Capture ms into the CAS
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("ms")).unwrap();
        std::fs::write(src.path().join("ms/index.js"), b"INDEX").unwrap();
        std::fs::write(
            src.path().join("ms/package.json"),
            br#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();
        let pkg_ref = PathsetPackageRef {
            name: "ms".into(),
            version: "2.1.3".into(),
            package_root: src.path().join("ms"),
        };
        let artifact = capture_package(&pkg_ref, &store).unwrap();

        // Write a per-package JSON file into the artifact directory
        let artifact_dir = ws.path().join("artifact-packages/fp123");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let json = serde_json::to_string(&artifact).unwrap();
        std::fs::write(artifact_dir.join("ms@2.1.3.json"), json.as_bytes()).unwrap();

        // Restore should read the per-package file and restore the package
        let result = try_restore_from_cas(&artifact_dir, ws.path(), &store).unwrap();
        assert!(result, "restore should succeed");
        assert_eq!(
            std::fs::read(ws.path().join("node_modules/ms/index.js")).unwrap(),
            b"INDEX"
        );
    }

    // ── Legacy tests — kept for regression coverage ───────────────────────────

    #[test]
    fn returns_false_when_directory_missing() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());
        let result = try_restore_from_cas(
            &ws.path().join("does-not-exist"),
            ws.path(),
            &store,
        )
        .unwrap();
        assert!(!result);
    }

    #[test]
    fn returns_false_when_cas_missing_some_hashes() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let artifact = PackageArtifact {
            name: "ms".into(),
            version: "2.1.3".into(),
            files: vec![(PathBuf::from("index.js"), ContentHash::of(b"never stored"))],
        };

        let artifact_dir = ws.path().join("artifact-packages/fp");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(
            artifact_dir.join("ms@2.1.3.json"),
            serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();

        let result = try_restore_from_cas(&artifact_dir, ws.path(), &store).unwrap();
        assert!(!result);
        // node_modules must NOT have been touched
        assert!(!ws.path().join("node_modules").exists());
    }

    #[test]
    fn returns_true_and_restores_files_when_all_hashes_present() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("ms")).unwrap();
        std::fs::write(src.path().join("ms/index.js"), b"INDEX").unwrap();
        std::fs::write(src.path().join("ms/package.json"), br#"{"name":"ms","version":"2.1.3"}"#).unwrap();
        let pkg_ref = PathsetPackageRef {
            name: "ms".into(),
            version: "2.1.3".into(),
            package_root: src.path().join("ms"),
        };
        let artifact = capture_package(&pkg_ref, &store).unwrap();

        let artifact_dir = ws.path().join("artifact-packages/fp");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(
            artifact_dir.join("ms@2.1.3.json"),
            serde_json::to_vec(&artifact).unwrap(),
        )
        .unwrap();

        let result = try_restore_from_cas(&artifact_dir, ws.path(), &store).unwrap();
        assert!(result);
        assert_eq!(
            std::fs::read(ws.path().join("node_modules/ms/index.js")).unwrap(),
            b"INDEX"
        );
    }
}
