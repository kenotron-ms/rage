//! Cross-platform IPC transport for the rage daemon.
//!
//! On Unix, this wraps `tokio::net::UnixStream` / `UnixListener`.
//! On Windows, this wraps `tokio::net::windows::named_pipe::NamedPipe{Server,Client}`.
//!
//! See docs/plans/2026-04-29-windows-support-design.md.

#[allow(unused_imports)]
use crate::discovery::{self, DiscoveryFile};
use anyhow::Result;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("no daemon running for this workspace")]
    NotRunning,
    #[error("daemon discovery file was stale (removed); start daemon with `rage dev`")]
    Stale,
    #[error("transport error: {0}")]
    Transport(#[from] std::io::Error),
}
