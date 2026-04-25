//! Content-addressed local cache for rage build tasks.

    pub mod entry;
    pub mod fingerprint;
    pub mod local;
    pub mod provider;

    pub use entry::CacheEntry;
    pub use fingerprint::fingerprint_task;
    pub use local::LocalCache;
    pub use provider::CacheProvider;
    