use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryFile {
    pub pid: u32,
    pub unix_socket: PathBuf,
    pub http_port: u16,
    pub start_time: String, // ISO-8601
    pub version: String,
    pub workspace: PathBuf,
}

/// Stable 16-hex-char hash of an absolute workspace path.
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let s = canonical.to_string_lossy();
    let h = blake3::hash(s.as_bytes());
    h.to_hex()[..16].to_string()
}

pub fn daemons_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("HOME or USERPROFILE not set")?;
    let dir = PathBuf::from(home).join(".rage").join("daemons");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn discovery_path(workspace: &Path) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.json", workspace_hash(workspace))))
}

pub fn socket_path(workspace: &Path) -> Result<PathBuf> {
    Ok(daemons_dir()?.join(format!("{}.sock", workspace_hash(workspace))))
}

pub fn write_discovery(workspace: &Path, d: &DiscoveryFile) -> Result<()> {
    let path = discovery_path(workspace)?;
    let json = serde_json::to_string_pretty(d).context("serialising DiscoveryFile")?;
    std::fs::write(&path, json)
        .with_context(|| format!("writing discovery file {}", path.display()))?;
    Ok(())
}

pub fn read_discovery(workspace: &Path) -> Result<Option<DiscoveryFile>> {
    let path = discovery_path(workspace)?;
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let d: DiscoveryFile = serde_json::from_str(&contents)
                .with_context(|| format!("parsing discovery file {}", path.display()))?;
            Ok(Some(d))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading discovery file {}", path.display())),
    }
}

/// Deletes the `.json` discovery file and the `.sock` file (if present).
pub fn delete_discovery(workspace: &Path) -> Result<()> {
    let json_path = discovery_path(workspace)?;
    let sock_path = socket_path(workspace)?;

    // Remove the JSON discovery file; ignore not-found
    match std::fs::remove_file(&json_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("deleting discovery file {}", json_path.display()))
        }
    }

    // Remove the socket file; ignore not-found
    match std::fs::remove_file(&sock_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("deleting socket file {}", sock_path.display()))
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn workspace_hash_is_deterministic() {
        let path = Path::new("/tmp/test-workspace");
        let h1 = workspace_hash(path);
        let h2 = workspace_hash(path);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16, "hash must be exactly 16 hex chars");
        assert!(
            h1.chars().all(|c| c.is_ascii_hexdigit()),
            "hash must be hex"
        );
    }

    #[test]
    fn workspace_hash_distinguishes_paths() {
        let ha = workspace_hash(Path::new("/tmp/a"));
        let hb = workspace_hash(Path::new("/tmp/b"));
        assert_ne!(ha, hb, "different paths must produce different hashes");
    }

    #[test]
    fn discovery_roundtrips() {
        use std::env;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        // Override HOME so daemons_dir() uses our temp directory
        let home = tmp.path().to_str().unwrap();
        env::set_var("HOME", home);

        let workspace = tmp.path().join("my-project");
        std::fs::create_dir_all(&workspace).unwrap();

        let d = DiscoveryFile {
            pid: 12345,
            unix_socket: tmp.path().join("daemon.sock"),
            http_port: 8080,
            start_time: "2026-04-24T00:00:00Z".to_string(),
            version: "0.0.0".to_string(),
            workspace: workspace.clone(),
        };

        write_discovery(&workspace, &d).unwrap();
        let got = read_discovery(&workspace).unwrap();
        assert_eq!(got, Some(d));

        delete_discovery(&workspace).unwrap();
        let gone = read_discovery(&workspace).unwrap();
        assert_eq!(gone, None);
    }
}
