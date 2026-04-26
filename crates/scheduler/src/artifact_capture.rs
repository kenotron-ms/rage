//! After-build hook: extract packages from a sandbox pathset and stuff them
//! into the per-package CAS, updating the workspace manifest atomically.

use artifact_store::{
    capture_package, LocalArtifactStore, PackageArtifact, PathsetPackageRef,
    WorkspacePackageManifest,
};
use plugin_typescript::pathset_extractor::{
    extract_flat_from_node_modules, extract_pnpm_packages, PathsetPackageRef as TsPkgRef,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Spawn a fire-and-forget background task that captures every package
/// referenced by `pathset_reads` into the CAS, then merges the new
/// `PackageArtifact`s into `{manifest_path}` (atomic write).
///
/// All errors are swallowed: capture is best-effort and must never break a build.
pub fn schedule_capture(
    pathset_reads: Vec<PathBuf>,
    workspace_root: PathBuf,
    manifest_path: PathBuf,
    install_fingerprint: String,
    store: Arc<LocalArtifactStore>,
) {
    tokio::task::spawn_blocking(move || {
        let _ = capture_now(
            &pathset_reads,
            &workspace_root,
            &manifest_path,
            &install_fingerprint,
            store.as_ref(),
        );
    });
}

/// Synchronous variant — does the work inline. Used by integration tests.
pub fn capture_now(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    manifest_path: &Path,
    install_fingerprint: &str,
    store: &LocalArtifactStore,
) -> std::io::Result<()> {
    // 1) Discover packages from pnpm-style pathset reads.
    let ts_refs: Vec<TsPkgRef> = extract_pnpm_packages(pathset_reads, workspace_root);

    // Fall back to flat layout (yarn/npm): read version from package.json.
    let ts_refs = if ts_refs.is_empty() {
        extract_flat_from_node_modules(pathset_reads, workspace_root)
    } else {
        ts_refs
    };

    if ts_refs.is_empty() {
        return Ok(());
    }

    // 2) Capture each package; map TsPkgRef → artifact_store::PathsetPackageRef.
    let mut artifacts: Vec<PackageArtifact> = Vec::with_capacity(ts_refs.len());
    for r in ts_refs {
        let pkg_ref = PathsetPackageRef {
            name: r.name,
            version: r.version,
            package_root: r.package_root,
        };
        match capture_package(&pkg_ref, store) {
            Ok(a) => artifacts.push(a),
            Err(_) => continue, // tolerate per-package capture errors
        }
    }
    if artifacts.is_empty() {
        return Ok(());
    }

    // 3) Merge into existing manifest (if any) — dedup by (name, version).
    let mut manifest = match std::fs::read(manifest_path) {
        Ok(b) => serde_json::from_slice::<WorkspacePackageManifest>(&b).unwrap_or_else(|_| {
            WorkspacePackageManifest {
                captured_at: 0,
                install_fingerprint: install_fingerprint.to_string(),
                packages: Vec::new(),
            }
        }),
        Err(_) => WorkspacePackageManifest {
            captured_at: 0,
            install_fingerprint: install_fingerprint.to_string(),
            packages: Vec::new(),
        },
    };
    // Drop any prior entries that match a newly-captured (name, version)
    let new_keys: std::collections::HashSet<(String, String)> = artifacts
        .iter()
        .map(|a| (a.name.clone(), a.version.clone()))
        .collect();
    manifest
        .packages
        .retain(|p| !new_keys.contains(&(p.name.clone(), p.version.clone())));
    manifest.packages.extend(artifacts);
    manifest.packages.sort_by(|a, b| {
        a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version))
    });
    manifest.captured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    manifest.install_fingerprint = install_fingerprint.to_string();

    // 4) Atomic write: tempfile → rename.
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = manifest_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&manifest).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, manifest_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use artifact_store::{ArtifactStore, LocalArtifactStore, WorkspacePackageManifest};

    #[test]
    fn capture_now_writes_manifest_with_pnpm_packages() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        // Build a fake pnpm virtual store layout so the extractor finds packages
        let pnpm_dir = ws.path().join("node_modules/.pnpm/ms@2.1.3/node_modules/ms");
        std::fs::create_dir_all(&pnpm_dir).unwrap();
        std::fs::write(pnpm_dir.join("index.js"), b"// ms").unwrap();
        std::fs::write(pnpm_dir.join("package.json"), br#"{"name":"ms","version":"2.1.3"}"#).unwrap();

        let pathset_reads = vec![
            pnpm_dir.join("index.js"),
            pnpm_dir.join("package.json"),
        ];
        let manifest_path = ws.path().join("artifact-packages/fp123.json");

        capture_now(
            &pathset_reads,
            ws.path(),
            &manifest_path,
            "fp123",
            &store,
        )
        .unwrap();

        let bytes = std::fs::read(&manifest_path).unwrap();
        let m: WorkspacePackageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m.install_fingerprint, "fp123");
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages[0].name, "ms");
        assert_eq!(m.packages[0].version, "2.1.3");
        for (_, h) in &m.packages[0].files {
            assert!(store.contains(h));
        }
    }

    #[test]
    fn capture_now_works_with_flat_yarn_layout() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        // Flat yarn layout: node_modules/ms/ (no .pnpm)
        let ms_dir = ws.path().join("node_modules/ms");
        std::fs::create_dir_all(&ms_dir).unwrap();
        std::fs::write(ms_dir.join("index.js"), b"// ms").unwrap();
        std::fs::write(
            ms_dir.join("package.json"),
            r#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();

        let pathset_reads = vec![ms_dir.join("index.js")];
        let manifest_path = ws.path().join("artifact-packages/fp-yarn.json");

        capture_now(
            &pathset_reads,
            ws.path(),
            &manifest_path,
            "fp-yarn",
            &store,
        )
        .unwrap();

        let bytes = std::fs::read(&manifest_path).unwrap();
        let m: WorkspacePackageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages[0].name, "ms");
        assert_eq!(m.packages[0].version, "2.1.3");
    }
}
