//! Task scheduler — builds task lists from workspace packages and runs
//! them in wave-parallel topological order using Tokio.

pub mod runner;
pub mod task;

pub use runner::{compute_task_levels, run_tasks, run_tasks_two_phase, RunError};
pub use task::{build_task_list, build_task_list_with_config, Task, TaskError};
