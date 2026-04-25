# Phase 6 — Remote Cache S3/Azure Blob

**Status:** Planned  
**Branch:** `feat/phase6-remote-cache`  
**Modified crates:** `cache`, `pipeline-config`, `cli`

---

## Problem

The cache is local-only. Teams sharing a build cache across machines (CI + dev)
have no benefit from previous runs on different machines.

---

## Design

### `CacheProvider` trait (existing)

```rust
pub trait CacheProvider: Send + Sync {
    fn get(&self, key: &str) -> Option<CacheEntry>;
    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()>;
}
```

Both remote backends use a **write-through local cache** pattern:
- `get(key)`: check local disk first; if miss, fetch from remote, store locally, return
- `put(key, entry)`: write locally immediately; async upload to remote in background
- Network errors are silently swallowed to never break a build

### Config schema extension (`rage.json`)

```json
{
  "cache": {
    "backend": "s3",
    "bucket": "my-cache-bucket",
    "region": "us-west-2",
    "prefix": "rage-cache/"
  }
}
```

or

```json
{
  "cache": {
    "backend": "azure",
    "container": "rage-cache",
    "account": "mystorageaccount"
  }
}
```

**Credentials are NEVER in config** — standard env var chains are used.

---

## Implementation

### Step 1 — Extend `pipeline-config` CacheConfig

Add `backend: CacheBackend` enum to `CacheConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum CacheBackend {
    #[default]
    Local,
    S3 {
        bucket: String,
        region: Option<String>,
        #[serde(default = "default_s3_prefix")]
        prefix: String,
    },
    Azure {
        container: String,
        account: String,
        #[serde(default = "default_azure_prefix")]
        prefix: String,
    },
}
```

### Step 2 — `S3Cache` in `crates/cache/src/s3.rs`

Uses `aws-sdk-s3` + `aws-config`.  Auth via standard credential chain
(env vars `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`, `~/.aws/credentials`,
IAM role, etc.).

```rust
pub struct S3Cache {
    local: LocalCache,
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    rt: tokio::runtime::Handle,
}

impl CacheProvider for S3Cache {
    fn get(&self, key: &str) -> Option<CacheEntry> {
        // 1. Check local first
        if let Some(e) = self.local.get(key) { return Some(e); }
        // 2. Fetch from S3
        let s3_key = format!("{}{}.json", self.prefix, key);
        let body = self.rt.block_on(async {
            self.client
                .get_object()
                .bucket(&self.bucket)
                .key(&s3_key)
                .send()
                .await
        }).ok()?;
        // parse body, store locally, return
        ...
    }

    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()> {
        // 1. Write locally
        self.local.put(key, entry)?;
        // 2. Upload in background (best-effort)
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let s3_key = format!("{}{}.json", self.prefix, key);
        let json = serde_json::to_string(entry)?;
        tokio::spawn(async move {
            let _ = client
                .put_object()
                .bucket(bucket)
                .key(s3_key)
                .body(json.into_bytes().into())
                .send()
                .await;
        });
        Ok(())
    }
}
```

### Step 3 — `AzureBlobCache` in `crates/cache/src/azure.rs`

Uses `azure_storage_blobs`.  Auth via `AZURE_STORAGE_ACCOUNT` +
`AZURE_STORAGE_KEY` env vars (or managed identity via DefaultAzureCredential).

```rust
pub struct AzureBlobCache {
    local: LocalCache,
    client: azure_storage_blobs::prelude::ContainerClient,
    rt: tokio::runtime::Handle,
}
```

### Step 4 — Wire in CLI

In `cmd_run`, after loading config, check `config.cache.backend`:

```rust
let cache = match &config.cache.backend {
    CacheBackend::Local => Arc::new(LocalCache::with_dir(cache_dir)?) as Arc<dyn CacheProvider>,
    CacheBackend::S3 { bucket, region, prefix } => {
        Arc::new(S3Cache::new(cache_dir, bucket, region.as_deref(), prefix)?) as Arc<dyn CacheProvider>
    }
    CacheBackend::Azure { container, account, prefix } => {
        Arc::new(AzureBlobCache::new(cache_dir, container, account, prefix)?) as Arc<dyn CacheProvider>
    }
};
```

---

## Dependencies

Add to `crates/cache/Cargo.toml` (feature-gated to keep default binary lean):

```toml
[features]
default = []
s3 = ["dep:aws-sdk-s3", "dep:aws-config", "dep:tokio"]
azure = ["dep:azure_storage_blobs", "dep:azure_storage", "dep:tokio"]

[dependencies]
aws-sdk-s3 = { version = "1", optional = true }
aws-config = { version = "1", optional = true }
azure_storage = { version = "0.21", optional = true }
azure_storage_blobs = { version = "0.21", optional = true }
tokio = { version = "1", features = ["rt-multi-thread", "macros"], optional = true }
reqwest = { version = "0.12", features = ["json"], optional = true }
```

For the CLI, enable the features:
```toml
# crates/cli/Cargo.toml
cache = { path = "../cache", features = ["s3", "azure"] }
```

---

## Integration tests

MinIO + Azurite Docker setup for integration tests.

### MinIO (S3-compatible)

```yaml
# docker-compose.test.yml
services:
  minio:
    image: minio/minio
    ports: ["9000:9000"]
    environment:
      MINIO_ROOT_USER: testkey
      MINIO_ROOT_PASSWORD: testsecret
    command: server /data
```

Test:
```rust
#[tokio::test]
#[cfg(feature = "s3")]
async fn s3_put_get_roundtrip() {
    // Requires MinIO running on localhost:9000
    let s3 = S3Cache::new_with_endpoint(
        tmpdir, "test-bucket", "us-east-1", "rage/", "http://localhost:9000"
    ).unwrap();
    s3.put("key1", &entry).unwrap();
    assert!(s3.get("key1").is_some());
}
```

---

## Acceptance criteria

- `rage run build` with S3 config writes SF entries to S3
- Second run from wiped local cache hits S3 and restores output without re-running
- Azure Blob backend works equivalently
- No credentials in any config file or log output
- Integration tests pass with MinIO + Azurite
