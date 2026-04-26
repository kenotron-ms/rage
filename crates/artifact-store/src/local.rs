//! On-disk content-addressed store.
//!
//! Layout: `{root}/content/{hex[0..2]}/{hex[2..]}/data`

use crate::{ArtifactError, ArtifactStore, ContentHash};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LocalArtifactStore {
    root: PathBuf,
}

impl LocalArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn content_path(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.hex();
        self.root
            .join("content")
            .join(&hex[..2])
            .join(&hex[2..])
            .join("data")
    }
}

impl ArtifactStore for LocalArtifactStore {
    fn put_bytes(&self, bytes: &[u8]) -> Result<ContentHash, ArtifactError> {
        let hash = ContentHash::of(bytes);
        let dest = self.content_path(&hash);

        // Dedup: if already present, do nothing.
        if dest.is_file() {
            return Ok(hash);
        }

        let parent = dest.parent().expect("content_path has a parent");
        std::fs::create_dir_all(parent)?;

        // Atomic write: tempfile + rename.
        let tmp = parent.join(format!(".tmp-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        match std::fs::rename(&tmp, &dest) {
            Ok(()) => {}
            Err(_) if dest.is_file() => {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) => return Err(e.into()),
        }
        Ok(hash)
    }

    fn link(&self, hash: &ContentHash, target: &Path) -> Result<(), ArtifactError> {
        let src = self.content_path(hash);
        if !src.is_file() {
            return Err(ArtifactError::NotFound(hash.hex()));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if target.is_file() {
            let _ = std::fs::remove_file(target);
        }
        match std::fs::hard_link(&src, target) {
            Ok(()) => Ok(()),
            Err(e) => {
                let kind = e.kind();
                let raw = e.raw_os_error();
                let is_exdev = raw == Some(libc_exdev())
                    || matches!(kind, std::io::ErrorKind::Unsupported);
                let is_perm = matches!(kind, std::io::ErrorKind::PermissionDenied);
                if is_exdev || is_perm {
                    std::fs::copy(&src, target)?;
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.content_path(hash).is_file()
    }
}

#[cfg(unix)]
fn libc_exdev() -> i32 {
    18 // EXDEV on Linux/macOS
}

#[cfg(not(unix))]
fn libc_exdev() -> i32 {
    -1
}
