//! Workspace discovery for JS monorepos (pnpm, yarn, npm).
//!
//! Detects the package manager, walks the workspace package globs, and
//! produces a resolved list of packages with workspace-internal
//! dependency edges.

pub mod detect;
pub mod discovery;
pub mod graph;
pub mod package;

pub use detect::{detect_package_manager, PackageManager};
pub use discovery::discover_packages;
pub use graph::build_package_graph;
pub use package::Package;
