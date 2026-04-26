//! After-build hook: extract packages from a sandbox pathset and stuff them
//! into the per-package CAS, writing one JSON file per package immediately
//! after it is captured so the manifest survives process exit at any point.

use artifact_store::{
    capture_package, LocalArtifactStore, PackageArtifact, PathsetPackageRef,
};
use plugin_typescript::pathset_extractor::{
    extract_flat_from_node_modules, extract_pnpm_packages, PathsetPackageRef as TsPkgRef,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Spawn a fire-and-forget background task that captures every package
/// referenced by `pathset_reads` into the CAS, writing one JSON file per
/// package into `artifact_dir` immediately after each capture so the manifest
/// survives process exit at any point.
///
/// All errors are swallowed: capture is best-effort and must never break a build.
pub fn schedule_capture(
    pathset_reads: Vec<PathBuf>,
    workspace_root: PathBuf,
    artifact_dir: PathBuf,
    install_fingerprint: String,
    store: Arc<LocalArtifactStore>,
) {
    // Use a detached OS thread so the tokio runtime does NOT wait for this
    // to complete at shutdown — the build should exit immediately after tasks
    // finish. Each package file is written atomically so partial runs leave
    // valid state that the next run can read and extend.
    let _ = std::thread::Builder::new()
        .name("rage-artifact-capture".into())
        .spawn(move || {
            let _ = capture_now(
                &pathset_reads,
                &workspace_root,
                &artifact_dir,
                &install_fingerprint,
                store.as_ref(),
            );
        });
}

/// Synchronous variant — does the work inline. Used by integration tests.
pub fn capture_now(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    artifact_dir: &Path,
    _install_fingerprint: &str,
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

    // Create the artifact directory once at the top — shared by all packages.
    std::fs::create_dir_all(artifact_dir)?;

    // Only capture packages not already in the per-package file — avoid
    // redundant work. Cap at 20 new packages per invocation to keep background
    // time bounded. The CAS builds up gradually across multiple runs.
    const MAX_NEW_PACKAGES_PER_CAPTURE: usize = 20;

    let mut captured = 0usize;
    for r in ts_refs {
        if captured >= MAX_NEW_PACKAGES_PER_CAPTURE {
            break;
        }

        let pkg_file = artifact_dir.join(package_filename(&r.name, &r.version));

        // Skip if already captured — per-package file exists.
        if pkg_file.exists() {
            continue;
        }

        // Quick pre-check: skip if the package root is gone or package.json
        // is already in the CAS (means we've stored this content before, though
        // we still write the per-package file if it's absent).
        if !r.package_root.exists() {
            continue;
        }

        let pkg_ref = PathsetPackageRef {
            name: r.name,
            version: r.version,
            package_root: r.package_root,
        };

        // 2) Capture this package into CAS.
        match capture_package(&pkg_ref, store) {
            Ok(artifact) => {
                // 3) Write this package's entry IMMEDIATELY after capture
                //    (atomic temp → rename so readers never see partial state).
                if write_package_entry(&pkg_file, &artifact).is_ok() {
                    captured += 1;
                }
            }
            Err(_) => continue, // tolerate per-package capture errors
        }
    }

    Ok(())
}

/// Convert a package name + version to a filesystem-safe filename.
/// Scoped packages (`@types/node`) use `+` instead of `/` to avoid path
/// separators: `@types+node@20.1.0.json`.
pub(crate) fn package_filename(name: &str, version: &str) -> String {
    let safe_name = name.replace('/', "+");
    format!("{safe_name}@{version}.json")
}

/// Atomically write a `PackageArtifact` to `path` via a temp-file rename.
fn write_package_entry(path: &Path, artifact: &PackageArtifact) -> std::io::Result<()> {
    let json = serde_json::to_string(artifact)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// Capture EVERY package currently in `workspace_root/node_modules/` into the CAS.
/// Called once after a successful root install task (yarn/npm/pnpm install).
/// Runs synchronously in `spawn_blocking` — callers must ensure they are in an async context.
/// Returns the number of packages captured (newly written or already present in artifact_dir).
pub fn capture_all_node_modules(
    workspace_root: &Path,
    artifact_dir: &Path,
    store: &LocalArtifactStore,
) -> std::io::Result<usize> {
    let nm = workspace_root.join("node_modules");
    if !nm.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(artifact_dir)?;

    let mut captured = 0;

    // Walk top-level entries in node_modules/
    for entry in std::fs::read_dir(&nm)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden dirs (.bin, .cache, .pnpm, .yarn, etc.)
        if name_str.starts_with('.') {
            continue;
        }

        if name_str.starts_with('@') {
            // Scoped package — walk one level deeper
            let scope_dir = entry.path();
            if !scope_dir.is_dir() {
                continue;
            }
            for inner in std::fs::read_dir(&scope_dir)? {
                let inner = inner?;
                let inner_name = inner.file_name();
                let inner_name_str = inner_name.to_string_lossy();
                let full_name = format!("{}/{}", name_str, inner_name_str);
                let pkg_json = inner.path().join("package.json");
                if let Some(version) = read_version(&pkg_json) {
                    let pkg_file = artifact_dir.join(package_filename(&full_name, &version));
                    if !pkg_file.exists() {
                        let pkg_ref = PathsetPackageRef {
                            name: full_name,
                            version,
                            package_root: inner.path(),
                        };
                        if let Ok(artifact) = capture_package(&pkg_ref, store) {
                            let _ = write_package_entry(&pkg_file, &artifact);
                            captured += 1;
                        }
                    } else {
                        captured += 1; // already done
                    }
                }
            }
        } else {
            // Regular package
            let pkg_path = entry.path();
            if !pkg_path.is_dir() {
                continue;
            }
            let pkg_json = pkg_path.join("package.json");
            if let Some(version) = read_version(&pkg_json) {
                let pkg_file = artifact_dir.join(package_filename(&name_str, &version));
                if !pkg_file.exists() {
                    let pkg_ref = PathsetPackageRef {
                        name: name_str.to_string(),
                        version,
                        package_root: pkg_path,
                    };
                    if let Ok(artifact) = capture_package(&pkg_ref, store) {
                        let _ = write_package_entry(&pkg_file, &artifact);
                        captured += 1;
                    }
                } else {
                    captured += 1; // already done
                }
            }
        }
    }
    Ok(captured)
}

fn read_version(pkg_json: &Path) -> Option<String> {
    let text = std::fs::read_to_string(pkg_json).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("version")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use artifact_store::{ArtifactStore, LocalArtifactStore};

    #[test]
    fn schedule_capture_returns_immediately() {
        use std::time::{Duration, Instant};
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(LocalArtifactStore::new(tmp.path()));

        // Pass a large fake pathset — if this were spawn_blocking it would
        // block until the closure runs; with a detached thread it returns instantly.
        let reads: Vec<PathBuf> = (0..1000)
            .map(|i| tmp.path().join(format!("node_modules/pkg{i}/index.js")))
            .collect();

        let start = Instant::now();
        schedule_capture(
            reads,
            tmp.path().to_path_buf(),
            tmp.path().join("artifact-packages/fp123"),
            "fp123".to_string(),
            store,
        );
        let elapsed = start.elapsed();

        // Should return in well under 100ms — the thread is spawned but not awaited
        assert!(
            elapsed < Duration::from_millis(100),
            "schedule_capture blocked for {elapsed:?}"
        );
    }

    // ── NEW per-package file tests (RED) ──────────────────────────────────────

    #[test]
    fn package_filename_handles_scoped_packages() {
        assert_eq!(
            package_filename("@types/node", "20.1.0"),
            "@types+node@20.1.0.json"
        );
        assert_eq!(package_filename("ms", "2.1.3"), "ms@2.1.3.json");
    }

    #[test]
    fn capture_now_writes_per_package_files_immediately() {
        let store_dir = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let pnpm_dir = ws
            .path()
            .join("node_modules/.pnpm/ms@2.1.3/node_modules/ms");
        std::fs::create_dir_all(&pnpm_dir).unwrap();
        std::fs::write(pnpm_dir.join("index.js"), b"// ms").unwrap();
        std::fs::write(
            pnpm_dir.join("package.json"),
            br#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();

        let pathset_reads = vec![pnpm_dir.join("index.js"), pnpm_dir.join("package.json")];
        let artifact_dir = ws.path().join("artifact-packages/fp123");

        capture_now(&pathset_reads, ws.path(), &artifact_dir, "fp123", &store).unwrap();

        // Per-package file must exist immediately after capture
        let pkg_file = artifact_dir.join("ms@2.1.3.json");
        assert!(pkg_file.exists(), "per-package file must be written: {pkg_file:?}");

        // It must be a valid PackageArtifact (not WorkspacePackageManifest)
        let text = std::fs::read_to_string(&pkg_file).unwrap();
        let artifact: artifact_store::PackageArtifact = serde_json::from_str(&text).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
        for (_, h) in &artifact.files {
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
        let artifact_dir = ws.path().join("artifact-packages/fp-yarn");

        capture_now(
            &pathset_reads,
            ws.path(),
            &artifact_dir,
            "fp-yarn",
            &store,
        )
        .unwrap();

        let pkg_file = artifact_dir.join("ms@2.1.3.json");
        assert!(pkg_file.exists(), "flat yarn layout must produce per-package file");
        let text = std::fs::read_to_string(&pkg_file).unwrap();
        let artifact: artifact_store::PackageArtifact = serde_json::from_str(&text).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
    }

    #[test]
    fn capture_now_writes_pnpm_packages() {
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
        let artifact_dir = ws.path().join("artifact-packages/fp123");

        capture_now(
            &pathset_reads,
            ws.path(),
            &artifact_dir,
            "fp123",
            &store,
        )
        .unwrap();

        let pkg_file = artifact_dir.join("ms@2.1.3.json");
        let text = std::fs::read_to_string(&pkg_file).unwrap();
        let artifact: artifact_store::PackageArtifact = serde_json::from_str(&text).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
        assert!(!artifact.files.is_empty());
        for (_, h) in &artifact.files {
            assert!(store.contains(h));
        }
    }

    // ── capture_all_node_modules tests (RED) ─────────────────────────────────

    #[test]
    fn capture_all_node_modules_captures_every_package() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let store_dir = tmp.path().join("content");
        let artifact_dir = tmp.path().join("artifact-packages/fp123");
        let store = LocalArtifactStore::new(&store_dir);

        // Create flat node_modules with 2 packages
        for pkg in &["ms", "chalk"] {
            let dir = ws.join("node_modules").join(pkg);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("index.js"), b"// pkg").unwrap();
            std::fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"{}","version":"1.0.0"}}"#, pkg).as_bytes(),
            )
            .unwrap();
        }
        // Create scoped package
        let types_dir = ws.join("node_modules/@types/node");
        std::fs::create_dir_all(&types_dir).unwrap();
        std::fs::write(types_dir.join("index.d.ts"), b"// types").unwrap();
        std::fs::write(
            types_dir.join("package.json"),
            br#"{"name":"@types/node","version":"20.0.0"}"#,
        )
        .unwrap();

        let count = capture_all_node_modules(ws, &artifact_dir, &store).unwrap();

        assert_eq!(count, 3);
        assert!(
            artifact_dir.join("ms@1.0.0.json").exists(),
            "ms@1.0.0.json must exist"
        );
        assert!(
            artifact_dir.join("chalk@1.0.0.json").exists(),
            "chalk@1.0.0.json must exist"
        );
        assert!(
            artifact_dir.join("@types+node@20.0.0.json").exists(),
            "@types+node@20.0.0.json must exist"
        );
    }

    #[test]
    fn capture_all_skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let store = LocalArtifactStore::new(tmp.path().join("content"));
        let artifact_dir = tmp.path().join("artifact-packages/fp123");

        std::fs::create_dir_all(ws.join("node_modules/.bin")).unwrap();
        std::fs::create_dir_all(ws.join("node_modules/.cache")).unwrap();
        // No real package dirs

        let count = capture_all_node_modules(ws, &artifact_dir, &store).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn capture_all_returns_zero_when_no_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let store = LocalArtifactStore::new(tmp.path().join("content"));
        let artifact_dir = tmp.path().join("artifact-packages/fp123");

        // No node_modules directory at all
        let count = capture_all_node_modules(ws, &artifact_dir, &store).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn capture_all_skips_already_captured_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let store_dir = tmp.path().join("content");
        let artifact_dir = tmp.path().join("artifact-packages/fp123");
        let store = LocalArtifactStore::new(&store_dir);

        // Create one package
        let ms_dir = ws.join("node_modules/ms");
        std::fs::create_dir_all(&ms_dir).unwrap();
        std::fs::write(ms_dir.join("index.js"), b"// ms").unwrap();
        std::fs::write(
            ms_dir.join("package.json"),
            br#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();

        // First capture
        let count1 = capture_all_node_modules(ws, &artifact_dir, &store).unwrap();
        assert_eq!(count1, 1);

        // Second capture: file already exists, should still return 1 (already done)
        let count2 = capture_all_node_modules(ws, &artifact_dir, &store).unwrap();
        assert_eq!(count2, 1);
    }
}
