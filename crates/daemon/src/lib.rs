//! rage build daemon.
//!
//! See docs/plans/2026-04-24-rage-daemon-config-cache-design.md Section 1.

pub mod daemon;
pub mod discovery;
pub mod http;
pub mod messages;
pub mod reconciler;
pub mod socket;
pub mod state;
pub mod transport;
pub mod watcher;

pub use daemon::Daemon;
pub use discovery::{discovery_path, workspace_hash, DiscoveryFile};
pub use http::{bind_dynamic, AppState, StateSnapshot};
pub use messages::{DaemonMessage, DaemonResponse, TaskStatusMsg};
pub use state::{BuildState, DaemonState, TaskStatus};
