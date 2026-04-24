    //! Parse the workspace `rage.json` config file.

    pub mod config;

    pub use config::{load_config, RageConfig, SandboxConfig, SandboxMode};
    