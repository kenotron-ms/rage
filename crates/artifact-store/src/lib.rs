//! Per-package content-addressed artifact store.

pub mod content_hash;
pub mod local;
pub mod package_manifest;

pub use content_hash::ContentHash;
pub use local::LocalArtifactStore;
pub use package_manifest::{PackageArtifact, PathsetPackageRef, WorkspacePackageManifest};

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("content not found in store: {0}")]
    NotFound(String),
}

pub trait ArtifactStore: Send + Sync {
    fn put_bytes(&self, bytes: &[u8]) -> Result<ContentHash, ArtifactError>;
    fn link(&self, hash: &ContentHash, target: &Path) -> Result<(), ArtifactError>;
    fn contains(&self, hash: &ContentHash) -> bool;
}

/// Walk a package directory, content-address every regular file into the store.
pub fn capture_package(
    pkg_ref: &PathsetPackageRef,
    store: &dyn ArtifactStore,
) -> Result<PackageArtifact, ArtifactError> {
    use std::path::PathBuf;
    let mut files: Vec<(PathBuf, ContentHash)> = Vec::new();

    fn walk(
        root: &std::path::Path,
        cur: &std::path::Path,
        store: &dyn ArtifactStore,
        out: &mut Vec<(std::path::PathBuf, ContentHash)>,
    ) -> Result<(), ArtifactError> {
        let read = match std::fs::read_dir(cur) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        for entry in read.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                walk(root, &path, store, out)?;
            } else if ft.is_file() {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let h = store.put_bytes(&bytes)?;
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.push((rel, h));
            }
        }
        Ok(())
    }

    walk(
        &pkg_ref.package_root,
        &pkg_ref.package_root,
        store,
        &mut files,
    )?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(PackageArtifact {
        name: pkg_ref.name.clone(),
        version: pkg_ref.version.clone(),
        files,
    })
}

/// Materialize a previously-captured package into `target_dir/<name>/...`.
pub fn restore_package(
    artifact: &PackageArtifact,
    target_dir: &Path,
    store: &dyn ArtifactStore,
) -> Result<(), ArtifactError> {
    let pkg_root = target_dir.join(&artifact.name);
    for (rel, hash) in &artifact.files {
        let dest = pkg_root.join(rel);
        store.link(hash, &dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_bytes_returns_consistent_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let h1 = store.put_bytes(b"hello world").unwrap();
        let h2 = store.put_bytes(b"hello world").unwrap();
        assert_eq!(h1, h2, "identical bytes must produce identical hashes");
    }

    #[test]
    fn link_creates_file_with_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let h = store.put_bytes(b"hello").unwrap();
        let target = dir.path().join("out").join("hello.txt");
        store.link(&h, &target).unwrap();
        assert!(target.is_file());
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn link_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let h = store.put_bytes(b"data").unwrap();
        let target = dir.path().join("a/b/c/file.bin");
        store.link(&h, &target).unwrap();
        assert!(target.is_file());
    }

    #[test]
    fn link_returns_not_found_when_hash_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let bogus = ContentHash::of(b"never stored");
        let err = store.link(&bogus, &dir.path().join("x")).unwrap_err();
        assert!(matches!(err, ArtifactError::NotFound(_)));
    }

    #[test]
    fn contains_reflects_put_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let h = ContentHash::of(b"abc");
        assert!(!store.contains(&h));
        store.put_bytes(b"abc").unwrap();
        assert!(store.contains(&h));
    }

    #[test]
    fn manifest_serde_round_trip() {
        use std::path::PathBuf;
        let m = WorkspacePackageManifest {
            captured_at: 1714000000,
            install_fingerprint: "abc123".to_string(),
            packages: vec![PackageArtifact {
                name: "ms".to_string(),
                version: "2.1.3".to_string(),
                files: vec![
                    (
                        PathBuf::from("index.js"),
                        ContentHash::of(b"console.log(1)"),
                    ),
                    (PathBuf::from("package.json"), ContentHash::of(b"{}")),
                ],
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: WorkspacePackageManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m.captured_at, m2.captured_at);
        assert_eq!(m.install_fingerprint, m2.install_fingerprint);
        assert_eq!(m.packages.len(), m2.packages.len());
        assert_eq!(m.packages[0].name, m2.packages[0].name);
        assert_eq!(m.packages[0].version, m2.packages[0].version);
        assert_eq!(m.packages[0].files, m2.packages[0].files);
    }

    #[test]
    fn capture_package_stores_all_files_and_returns_artifact() {
        use std::path::PathBuf;
        let store_dir = tempfile::tempdir().unwrap();
        let pkg_dir = tempfile::tempdir().unwrap();
        let pkg_root = pkg_dir.path().join("ms");
        std::fs::create_dir_all(pkg_root.join("lib")).unwrap();
        std::fs::write(
            pkg_root.join("index.js"),
            b"module.exports = function ms(s){return s}",
        )
        .unwrap();
        std::fs::write(
            pkg_root.join("package.json"),
            br#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();
        std::fs::write(pkg_root.join("lib/util.js"), b"// util").unwrap();

        let store = LocalArtifactStore::new(store_dir.path());
        let pkg_ref = PathsetPackageRef {
            name: "ms".into(),
            version: "2.1.3".into(),
            package_root: pkg_root.clone(),
        };
        let artifact = capture_package(&pkg_ref, &store).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
        assert_eq!(artifact.files.len(), 3);

        for (rel, hash) in &artifact.files {
            assert!(store.contains(hash), "missing in store: {rel:?}");
            let on_disk = std::fs::read(pkg_root.join(rel)).unwrap();
            assert_eq!(*hash, ContentHash::of(&on_disk));
        }

        let rels: Vec<&PathBuf> = artifact.files.iter().map(|(p, _)| p).collect();
        assert!(rels.iter().any(|p| p.to_string_lossy() == "index.js"));
        assert!(rels.iter().any(|p| p.to_string_lossy() == "package.json"));
        assert!(rels.iter().any(|p| p.ends_with("util.js")));
    }

    #[test]
    fn restore_package_recreates_directory_structure() {
        let store_dir = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let pkg_root = src_dir.path().join("ms");
        std::fs::create_dir_all(pkg_root.join("lib")).unwrap();
        std::fs::write(pkg_root.join("index.js"), b"INDEX").unwrap();
        std::fs::write(pkg_root.join("lib/util.js"), b"UTIL").unwrap();

        let artifact = capture_package(
            &PathsetPackageRef {
                name: "ms".into(),
                version: "2.1.3".into(),
                package_root: pkg_root,
            },
            &store,
        )
        .unwrap();

        let nm = dst_dir.path().join("node_modules");
        restore_package(&artifact, &nm, &store).unwrap();

        assert_eq!(std::fs::read(nm.join("ms/index.js")).unwrap(), b"INDEX");
        assert_eq!(std::fs::read(nm.join("ms/lib/util.js")).unwrap(), b"UTIL");
    }

    #[test]
    fn restore_package_preserves_scoped_directory_structure() {
        let store_dir = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let h = store.put_bytes(b"declare const x: number;").unwrap();
        let artifact = PackageArtifact {
            name: "@types/node".into(),
            version: "20.1.0".into(),
            files: vec![(std::path::PathBuf::from("index.d.ts"), h)],
        };
        let nm = dst.path().join("node_modules");
        restore_package(&artifact, &nm, &store).unwrap();

        assert!(
            nm.join("@types/node/index.d.ts").is_file(),
            "scoped package must preserve @scope/name dir"
        );
    }

    #[test]
    fn end_to_end_capture_delete_restore() {
        let store_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let pkgs = [
            ("ms", "2.1.3"),
            ("lodash", "4.17.21"),
            ("@types/node", "20.1.0"),
        ];
        let mut captured = Vec::new();
        for (name, version) in pkgs {
            let root = src.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("index.js"),
                format!("// {name}@{version}").as_bytes(),
            )
            .unwrap();
            std::fs::write(
                root.join("package.json"),
                format!(r#"{{"name":"{name}","version":"{version}"}}"#).as_bytes(),
            )
            .unwrap();
            let artifact = capture_package(
                &PathsetPackageRef {
                    name: name.into(),
                    version: version.into(),
                    package_root: root,
                },
                &store,
            )
            .unwrap();
            captured.push(artifact);
        }

        drop(src);

        let nm = dst.path().join("node_modules");
        for artifact in &captured {
            restore_package(artifact, &nm, &store).unwrap();
        }

        for artifact in &captured {
            for (rel, hash) in &artifact.files {
                let p = nm.join(&artifact.name).join(rel);
                let bytes = std::fs::read(&p).unwrap();
                assert_eq!(
                    ContentHash::of(&bytes),
                    *hash,
                    "file {p:?} content mismatch"
                );
            }
        }
    }
}
