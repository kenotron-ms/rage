//! Hub address rendezvous via shared filesystem file.
//!
//! The hub writes its gRPC address to a well-known file at startup.
//! Spokes poll this file until it appears, then connect.
//!
//! In Docker Compose: file lives on a shared volume (`/shared/rage-hub.json`).
//! In CI with shared cache: file is uploaded to the cache backend.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Hub address record written to the rendezvous file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubAddr {
    /// gRPC address spokes connect to, e.g. `"hub:9650"` or `"127.0.0.1:9650"`.
    pub addr: String,
    /// Bearer token for spoke authentication.
    pub token: String,
    /// Unique build ID for this hub session.
    pub build_id: String,
}

/// Write the hub address to `file` atomically (tmp + rename).
pub fn write_hub_addr(file: &Path, addr: &HubAddr) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(addr).context("serialize hub addr")?;
    let tmp = file.with_extension("json.tmp");
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, &json).context("write tmp hub addr file")?;
    std::fs::rename(&tmp, file).context("rename hub addr file")?;
    Ok(())
}

/// Poll `file` until it contains a valid `HubAddr`, or `timeout_secs` elapses.
///
/// Checks every 500ms. Used by spokes to discover the hub's address.
pub async fn read_hub_addr_with_timeout(file: &Path, timeout_secs: u32) -> anyhow::Result<HubAddr> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs as u64);
    loop {
        if let Ok(content) = std::fs::read_to_string(file) {
            if let Ok(addr) = serde_json::from_str::<HubAddr>(&content) {
                return Ok(addr);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "timed out after {}s waiting for hub addr file: {}",
                timeout_secs,
                file.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let addr = HubAddr {
            addr: "hub:9650".into(),
            token: "tok123".into(),
            build_id: "build-001".into(),
        };
        write_hub_addr(tmp.path(), &addr).unwrap();
        let read = read_hub_addr_with_timeout(tmp.path(), 5).await.unwrap();
        assert_eq!(read.addr, "hub:9650");
        assert_eq!(read.token, "tok123");
    }

    #[tokio::test]
    async fn timeout_when_file_absent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let absent = tmp.path().with_extension("absent");
        let result = read_hub_addr_with_timeout(&absent, 1).await;
        assert!(result.is_err());
    }
}
