// implemented later

    use std::path::PathBuf;

    /// File used to advertise the daemon socket path.
    pub struct DiscoveryFile;

    /// Returns the path to the discovery file for the given workspace root.
    pub fn discovery_path(_workspace_root: &std::path::Path) -> PathBuf {
        PathBuf::new()
    }

    /// Returns a hex-encoded hash identifying the workspace.
    pub fn workspace_hash(_workspace_root: &std::path::Path) -> String {
        String::new()
    }
    