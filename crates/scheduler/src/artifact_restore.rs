//! CAS-backed restoration of a workspace's installed packages.

use artifact_store::{ArtifactError, ArtifactStore, LocalArtifactStore, WorkspacePackageManifest};
use std::path::Path;

/// Attempt to restore `node_modules/` from the per-workspace package manifest.
///
/// Returns:
/// - `Ok(true)`  — every package in the manifest was hardlinked back into place.
/// - `Ok(false)` — the manifest is missing, or the CAS does not contain every
///   required hash (partial restore is forbidden — never run).
/// - `Err(_)`    — unexpected I/O failure while reading the manifest.
pub fn try_restore_from_cas(
    manifest_path: &Path,
    workspace_root: &Path,
    store: &LocalArtifactStore,
) -> Result<bool, ArtifactError> {
    let bytes = match std::fs::read(manifest_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let manifest: WorkspacePackageManifest = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => return Ok(false), // corrupt manifest — same as miss
    };

    // Pre-flight: every required hash must be present BEFORE we touch the
    // workspace. Partial restores are silent corruption — never allowed.
    for pkg in &manifest.packages {
        for (_, hash) in &pkg.files {
            if !store.contains(hash) {
                return Ok(false);
            }
        }
    }

    // All present — restore.
    let nm = workspace_root.join("node_modules");
    for pkg in &manifest.packages {
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

    fn write_manifest(path: &Path, m: &WorkspacePackageManifest) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, serde_json::to_vec(m).unwrap()).unwrap();
    }

    #[test]
    fn returns_false_when_manifest_missing() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());
        let result = try_restore_from_cas(
            &ws.path().join("does-not-exist.json"),
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

        let manifest = WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: "fp".into(),
            packages: vec![PackageArtifact {
                name: "ms".into(),
                version: "2.1.3".into(),
                files: vec![(PathBuf::from("index.js"), ContentHash::of(b"never stored"))],
            }],
        };
        let mp = ws.path().join("manifest.json");
        write_manifest(&mp, &manifest);

        let result = try_restore_from_cas(&mp, ws.path(), &store).unwrap();
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

        let manifest = WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: "fp".into(),
            packages: vec![artifact],
        };
        let mp = ws.path().join("manifest.json");
        write_manifest(&mp, &manifest);

        let result = try_restore_from_cas(&mp, ws.path(), &store).unwrap();
        assert!(result);
        assert_eq!(
            std::fs::read(ws.path().join("node_modules/ms/index.js")).unwrap(),
            b"INDEX"
        );
    }
}
