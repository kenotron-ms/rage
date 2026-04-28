//! Hub — rage's distributed build coordinator.
//!
//! Implements the hub side of the hub/spoke architecture described in
//! `docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 4.
//!
//! The hub holds the task DAG in memory and dispatches tasks to spokes
//! via gRPC streaming. All build artifacts are routed through the remote cache,
//! not through the hub itself.

pub mod dag;
pub mod rendezvous;
pub mod server;

// Include the gRPC generated code.
pub mod proto {
    tonic::include_proto!("rage.coordinator.v1");
}

pub use dag::{HubDag, TaskNode};
pub use rendezvous::{read_hub_addr_with_timeout, write_hub_addr, HubAddr};
pub use server::HubServer;
