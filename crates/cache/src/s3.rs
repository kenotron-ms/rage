//! S3-compatible remote cache backend for rage.
//!
//! Uses the AWS SDK for Rust with the standard credential chain:
//!   1. `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` environment variables
//!   2. `~/.aws/credentials` profile file
//!   3. IAM role attached to the host (EC2, ECS, Lambda, etc.)
//!
//! Credentials are NEVER stored in `rage.json`.
//!
//! Uses a **write-through local cache** pattern:
//! - `get(key)`: check local disk first; on miss fetch from S3, store locally, return.
//! - `put(key, entry)`: write locally immediately; spawn a background tokio task to
//!   upload to S3 asynchronously.
//! - Network failures are silently swallowed — they must never break a build.
//!
//! Compatible with any S3-compatible storage: Amazon S3, Cloudflare R2, MinIO, etc.
//! Override the endpoint via the `AWS_ENDPOINT_URL` environment variable.

use crate::entry::CacheEntry;
use crate::local::LocalCache;
use crate::provider::CacheProvider;
use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use std::path::PathBuf;

/// S3-compatible remote cache. Wraps a `LocalCache` for local read-through.
pub struct S3Cache {
    local: LocalCache,
    client: Client,
    bucket: String,
    prefix: String,
}

impl S3Cache {
    /// Create an `S3Cache`.
    ///
    /// # Arguments
    /// - `local_dir` — local cache directory (for read-through / write-through)
    /// - `bucket`    — S3 bucket name
    /// - `region`    — AWS region (e.g. `"us-east-1"`). `None` to auto-detect.
    /// - `prefix`    — key prefix in the bucket (e.g. `"rage-cache/"`)
    ///
    /// # Credential chain
    ///
    /// Credentials are loaded from the standard AWS credential chain — env vars,
    /// `~/.aws/credentials`, IAM role, etc.  No credentials are accepted in config.
    pub fn new(
        local_dir: PathBuf,
        bucket: impl Into<String>,
        region: Option<&str>,
        prefix: impl Into<String>,
    ) -> Result<Self> {
        let local = LocalCache::with_dir(local_dir).context("creating local cache for S3")?;
        let bucket = bucket.into();
        let prefix = prefix.into();

        // Build AWS config from environment / credential chain.
        // `block_on` is safe here because we're not inside a tokio context yet.
        let aws_conf = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("creating temp tokio runtime")?
            .block_on(async {
                let mut builder = aws_config::defaults(BehaviorVersion::latest());
                if let Some(r) = region {
                    builder = builder.region(aws_config::Region::new(r.to_string()));
                }
                builder.load().await
            });

        let client = Client::new(&aws_conf);
        Ok(Self { local, client, bucket, prefix })
    }

    fn s3_key(&self, cache_key: &str) -> String {
        format!("{}{}.json", self.prefix, cache_key)
    }

    fn get_remote_sync(&self, key: &str) -> Option<CacheEntry> {
        let s3_key = self.s3_key(key);
        let client = self.client.clone();
        let bucket = self.bucket.clone();

        let rt = tokio::runtime::Handle::try_current().ok()?;
        let body = rt.block_on(async {
            client
                .get_object()
                .bucket(&bucket)
                .key(&s3_key)
                .send()
                .await
        })
        .ok()?;

        let bytes = rt
            .block_on(async { body.body.collect().await })
            .ok()?
            .into_bytes();

        serde_json::from_slice(&bytes).ok()
    }
}

impl CacheProvider for S3Cache {
    fn get(&self, key: &str) -> Option<CacheEntry> {
        // 1. Local cache hit — avoid network round-trip.
        if let Some(entry) = self.local.get(key) {
            return Some(entry);
        }

        // 2. Fetch from S3 and store locally for future reads.
        if let Some(entry) = self.get_remote_sync(key) {
            let _ = self.local.put(key, &entry);
            return Some(entry);
        }

        None
    }

    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()> {
        // 1. Write locally immediately so the current process benefits right away.
        self.local.put(key, entry)?;

        // 2. Upload to S3 asynchronously — network failures must not block the build.
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let s3_key = self.s3_key(key);
        let json = serde_json::to_vec(entry).context("serializing entry for S3 upload")?;

        // Spawn detached — if no tokio runtime is active we silently skip.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = client
                    .put_object()
                    .bucket(bucket)
                    .key(s3_key)
                    .body(json.into())
                    .send()
                    .await;
            });
        }

        Ok(())
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Integration tests require MinIO running — enable manually.
    // `cargo test -p cache --features s3 -- --ignored`

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

    /// Test that local write-through works even when S3 is unavailable.
    #[test]
    fn local_put_get_without_s3() {
        let dir = tempdir().unwrap();
        // Use an invalid bucket — local operations must succeed regardless.
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");
        // Build with a fake endpoint to avoid real AWS calls.
        std::env::set_var("AWS_ENDPOINT_URL", "http://127.0.0.1:19999"); // unreachable
        let cache = S3Cache::new(
            dir.path().to_path_buf(),
            "test-bucket",
            Some("us-east-1"),
            "test/",
        );
        if let Ok(cache) = cache {
            // put must succeed (writes locally)
            let entry = sample_entry();
            let put_result = cache.put("key1", &entry);
            assert!(put_result.is_ok(), "local put should succeed: {put_result:?}");
            // get must succeed from local cache
            let got = cache.get("key1");
            assert!(got.is_some(), "local get should succeed after put");
        }
        // Cleanup env
        std::env::remove_var("AWS_ENDPOINT_URL");
    }

    /// Integration test: requires MinIO on localhost:9000.
    #[tokio::test]
    #[ignore]
    async fn s3_put_get_roundtrip_minio() {
        let dir = tempdir().unwrap();
        std::env::set_var("AWS_ACCESS_KEY_ID", "minioadmin");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "minioadmin");
        std::env::set_var("AWS_ENDPOINT_URL", "http://localhost:9000");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        // Ensure bucket exists (ignore error if already created)
        let conf = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::Region::new("us-east-1"))
            .endpoint_url("http://localhost:9000")
            .load()
            .await;
        let client = Client::new(&conf);
        let _ = client
            .create_bucket()
            .bucket("rage-test-bucket")
            .send()
            .await;

        let cache = S3Cache::new(
            dir.path().to_path_buf(),
            "rage-test-bucket",
            Some("us-east-1"),
            "test/",
        ).unwrap();

        let entry = sample_entry();
        cache.put("integration-key", &entry).unwrap();
        // Give the background upload a moment
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Delete from local to force S3 fetch
        let local_path = dir.path().join("integration-key.json");
        std::fs::remove_file(&local_path).ok();

        let fetched = cache.get("integration-key");
        assert!(fetched.is_some(), "should fetch from S3 after local eviction");
        assert_eq!(fetched.unwrap().fingerprint, "test-fp");
    }
}
