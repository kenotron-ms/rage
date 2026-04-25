//! Parse the workspace `rage.json` config file.

pub mod config;
pub mod policy;

pub use config::{
    load_config, CacheConfig, InputGlobsConfig, Policy, PluginConfig, RageConfig, SandboxConfig,
    SandboxMode,
};
pub use policy::resolve_sandbox_mode;
