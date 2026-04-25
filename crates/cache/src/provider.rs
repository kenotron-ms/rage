//! CacheProvider trait — implemented by LocalCache and any future remote backends.

use crate::entry::CacheEntry;
use anyhow::Result;

/// Abstraction over cache storage backends.
///
/// All implementations must be `Send + Sync` so they can be shared across
/// async task threads via `Arc<dyn CacheProvider>`.
pub trait CacheProvider: Send + Sync {
    /// Look up a cache entry by fingerprint key.
    ///
    /// Returns `None` on a miss (key not found or data corrupt).
    fn get(&self, key: &str) -> Option<CacheEntry>;

    /// Store a cache entry under the given fingerprint key.
    fn put(&self, key: &str, entry: &CacheEntry) -> Result<()>;
}
