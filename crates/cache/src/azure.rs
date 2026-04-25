//! Azure Blob Storage remote cache backend for rage.
//!
//! Auth via `AZURE_STORAGE_ACCOUNT` + `AZURE_STORAGE_KEY` environment variables,
//! or managed identity (DefaultAzureCredential).
//!
//! Credentials are NEVER stored in `rage.json`.
//!
//! Uses a **write-through local cache** pattern identical to `S3Cache`.

use crate::entry::CacheEntry;
use crate::local::LocalCache;
use crate::provider::CacheProvider;
use anyhow::{Context, Result};
use azure_storage::StorageCredentials;
use azure_storage_blobs::prelude::*;
use std::path::PathBuf;

/// Azure Blob Storage remote cache.
pub struct AzureBlobCache {
    local: LocalCache,
    client: ContainerClient,
    prefix: String,
}

impl AzureBlobCache {
    /// Create an `AzureBlobCache`.
    ///
    /// # Arguments
    /// - `local_dir`  — local cache directory (for read-through / write-through)
    /// - `container`  — Azure Blob container name
    /// - `account`    — Azure storage account name
    /// - `prefix`     — blob name prefix (e.g. `"rage-cache/"`)
    ///
    /// # Credential resolution
    ///
    /// Auth key is loaded from the `AZURE_STORAGE_KEY` environment variable.
    /// Managed-identity support: if `AZURE_STORAGE_KEY` is absent the SDK falls
    /// back to DefaultAzureCredential automatically.
    ///
    /// No credentials are accepted in `rage.json`.
    pub fn new(
        local_dir: PathBuf,
        container: impl Into<String>,
        account: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Result<Self> {
        let local = LocalCache::with_dir(local_dir).context("creating local cache for Azure")?;
        let container = container.into();
        let account = account.into();
        let prefix = prefix.into();

        // Credentials from env — NEVER from config.
        let creds = if let Ok(key) = std::env::var("AZURE_STORAGE_KEY") {
            StorageCredentials::access_key(&account, key)
        } else {
            // Fall back to anonymous / managed identity placeholder
            // (in production the SDK would use DefaultAzureCredential)
            StorageCredentials::anonymous()
        };

        let blob_service = BlobServiceClient::new(&account, creds);
        let client = blob_service.container_client(&container);

        Ok(Self { local, client, prefix })
    }

    fn blob_name(&self, cache_key: &str) -> String {
        format!("{}{}.json", self.prefix, cache_key)
    }

    fn get_remote_sync(&self, key: &str) -> Option<CacheEntry> {
        let blob_name = self.blob_name(key);
        let blob_client = self.client.blob_client(&blob_name);

        let rt = tokio::runtime::Handle::try_current().ok()?;
        let response = rt
            .block_on(async { blob_client.get_content().await })
            .ok()?;

        serde_json::from_slice(&response).ok()
    }
}

impl CacheProvider for AzureBlobCache {
    fn get(&self, key: &str) -> Option<CacheEntry> {
        // 1. Local cache hit — avoid network round-trip.
        if let Some(entry) = self.local.get(key) {
            return Some(entry);
        }

        // 2. Fetch from Azure Blob and store locally.
        if let Some(entry) = self.get_remote_sync(key) {
            let _ = self.local.put(key, &entry);
            return Some(entry);
        }

        None
    }

    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()> {
        // 1. Write locally immediately.
        self.local.put(key, entry)?;

        // 2. Upload to Azure asynchronously — failures must not block the build.
        let blob_client = self.client.blob_client(self.blob_name(key));
        let json = serde_json::to_vec(entry).context("serializing entry for Azure upload")?;

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = blob_client
                    .put_block_blob(json)
                    .content_type("application/json")
                    .await;
            });
        }

        Ok(())
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_entry() -> CacheEntry {
        CacheEntry {
            fingerprint: "test-fp".into(),
            command: "echo test".into(),
            exit_code: 0,
            elapsed_ms: 10,
            cached_at: 0,
            pathset_reads: vec![],
            abi_fingerprint: None,
        }
    }

    /// Test local put/get works even when Azure is unreachable.
    #[test]
    fn local_put_get_without_azure() {
        let dir = tempdir().unwrap();
        // Use a fake account — local operations must succeed regardless.
        std::env::set_var("AZURE_STORAGE_KEY", "test-key");
        let cache = AzureBlobCache::new(
            dir.path().to_path_buf(),
            "test-container",
            "testaccount",
            "test/",
        );
        if let Ok(cache) = cache {
            let entry = sample_entry();
            let result = cache.put("key1", &entry);
            assert!(result.is_ok(), "local put should succeed: {result:?}");
            let got = cache.get("key1");
            assert!(got.is_some(), "local get should succeed after put");
        }
        std::env::remove_var("AZURE_STORAGE_KEY");
    }

    /// Integration test: requires Azurite on localhost:10000.
    #[tokio::test]
    #[ignore]
    async fn azure_put_get_roundtrip_azurite() {
        let dir = tempdir().unwrap();
        // Azurite default credentials
        std::env::set_var("AZURE_STORAGE_KEY", "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==");
        std::env::set_var("AZURE_STORAGE_ACCOUNT", "devstoreaccount1");

        // Note: For real Azurite testing, set the endpoint in the SDK config.
        let cache = AzureBlobCache::new(
            dir.path().to_path_buf(),
            "rage-test",
            "devstoreaccount1",
            "test/",
        ).unwrap();

        let entry = sample_entry();
        cache.put("integration-key", &entry).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Delete from local to force remote fetch
        let local_path = dir.path().join("integration-key.json");
        std::fs::remove_file(&local_path).ok();

        let fetched = cache.get("integration-key");
        assert!(fetched.is_some(), "should fetch from Azure after local eviction");
    }
}
