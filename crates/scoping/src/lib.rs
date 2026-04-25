//! Git-based scoping — determines which packages are affected by recent changes.

pub mod affected;
pub mod git;

pub use affected::affected_packages;
pub use git::git_changed_files;
pub use git::git_dirty_files;
