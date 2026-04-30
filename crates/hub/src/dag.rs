//! In-memory task DAG for the hub coordinator.
//!
//! Tracks task states and computes which tasks become unblocked
//! when a dependency completes.

use std::collections::{HashMap, HashSet, VecDeque};

/// Possible states a task can be in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// All dependencies not yet met.
    Pending,
    /// All dependencies complete — ready to dispatch.
    Ready,
    /// Sent to a spoke worker (worker_id).
    Dispatched(String),
    /// Finished successfully.
    Completed,
    /// Finished with failure.
    Failed(String),
}

/// A task node in the build graph.
#[derive(Debug, Clone)]
pub struct TaskNode {
    pub task_id: String,
    pub package_name: String,
    pub script_name: String,
    pub command: String,
    pub package_path: String,
    pub depends_on: Vec<String>,
}

/// Summary statistics.
#[derive(Debug, Clone)]
pub struct DagStats {
    pub pending: usize,
    pub ready: usize,
    pub dispatched: usize,
    pub completed: usize,
    pub failed: usize,
}

/// In-memory task dependency graph and state machine.
pub struct HubDag {
    tasks: HashMap<String, TaskNode>,
    pub(crate) states: HashMap<String, TaskState>,
    /// task_id → set of task_ids that depend on it (reverse deps)
    rdeps: HashMap<String, HashSet<String>>,
    /// queue of Ready tasks in approximate topological order
    ready_queue: VecDeque<String>,
}

impl HubDag {
    pub fn new(task_nodes: Vec<TaskNode>) -> Self {
        let mut tasks: HashMap<String, TaskNode> = HashMap::new();
        let mut rdeps: HashMap<String, HashSet<String>> = HashMap::new();
        let mut initial_ready: Vec<String> = Vec::new();

        for task in &task_nodes {
            rdeps.entry(task.task_id.clone()).or_default();
            for dep in &task.depends_on {
                rdeps
                    .entry(dep.clone())
                    .or_default()
                    .insert(task.task_id.clone());
            }
        }

        let mut states: HashMap<String, TaskState> = HashMap::new();
        for task in task_nodes {
            let state = if task.depends_on.is_empty() {
                initial_ready.push(task.task_id.clone());
                TaskState::Ready
            } else {
                TaskState::Pending
            };
            states.insert(task.task_id.clone(), state);
            tasks.insert(task.task_id.clone(), task);
        }

        Self {
            tasks,
            states,
            rdeps,
            ready_queue: initial_ready.into_iter().collect(),
        }
    }

    /// Dispatch the next ready task to `worker_id`.
    /// Returns a clone of the task node if one was available.
    pub fn dispatch_next(&mut self, worker_id: &str) -> Option<TaskNode> {
        let task_id = self.ready_queue.pop_front()?;
        self.states.insert(
            task_id.clone(),
            TaskState::Dispatched(worker_id.to_string()),
        );
        self.tasks.get(&task_id).cloned()
    }

    /// Mark a task as completed. Returns task_ids that are now Ready.
    pub fn mark_complete(&mut self, task_id: &str) -> Vec<String> {
        self.states
            .insert(task_id.to_string(), TaskState::Completed);
        self.unblock_dependents(task_id)
    }

    /// Mark a task as failed and transitively fail all tasks that depend on it.
    /// Returns the list of newly-failed task IDs (NOT including `task_id` itself).
    pub fn mark_failed(&mut self, task_id: &str, error: &str) -> Vec<String> {
        self.states
            .insert(task_id.to_string(), TaskState::Failed(error.to_string()));

        let mut newly_failed: Vec<String> = Vec::new();
        let mut frontier: VecDeque<String> = VecDeque::new();
        frontier.push_back(task_id.to_string());

        while let Some(current) = frontier.pop_front() {
            let dependents: Vec<String> = self
                .rdeps
                .get(&current)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();

            for dep_id in dependents {
                // Only cascade into tasks that haven't already settled.
                match self.states.get(&dep_id) {
                    Some(TaskState::Completed) | Some(TaskState::Failed(_)) => continue,
                    _ => {}
                }
                self.states.insert(
                    dep_id.clone(),
                    TaskState::Failed(format!("dependency {current} failed")),
                );
                newly_failed.push(dep_id.clone());
                frontier.push_back(dep_id);
            }
        }

        // Drop any cascaded-failed tasks from the ready queue so they aren't dispatched.
        let states_ref = &self.states;
        self.ready_queue
            .retain(|id| !matches!(states_ref.get(id), Some(TaskState::Failed(_))));

        newly_failed
    }

    fn unblock_dependents(&mut self, completed_task: &str) -> Vec<String> {
        let mut newly_ready = Vec::new();

        let dependents: Vec<String> = self
            .rdeps
            .get(completed_task)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();

        for dep_task_id in dependents {
            if self.states.get(&dep_task_id) != Some(&TaskState::Pending) {
                continue;
            }

            // Check if ALL dependencies of dep_task_id are now complete
            let task = match self.tasks.get(&dep_task_id) {
                Some(t) => t,
                None => continue,
            };

            let all_done = task
                .depends_on
                .iter()
                .all(|dep| matches!(self.states.get(dep), Some(TaskState::Completed)));

            if all_done {
                self.states.insert(dep_task_id.clone(), TaskState::Ready);
                self.ready_queue.push_back(dep_task_id.clone());
                newly_ready.push(dep_task_id);
            }
        }

        newly_ready
    }

    /// Returns true when all tasks are complete or failed.
    pub fn is_done(&self) -> bool {
        self.states
            .values()
            .all(|s| matches!(s, TaskState::Completed | TaskState::Failed(_)))
    }

    /// Returns true if any task has failed.
    pub fn has_failure(&self) -> bool {
        self.states
            .values()
            .any(|s| matches!(s, TaskState::Failed(_)))
    }

    /// Returns the first failed task ID and error, if any.
    pub fn first_failure(&self) -> Option<(&str, &str)> {
        for (id, state) in &self.states {
            if let TaskState::Failed(err) = state {
                return Some((id.as_str(), err.as_str()));
            }
        }
        None
    }

    /// Returns whether the ready queue is non-empty.
    pub fn has_ready(&self) -> bool {
        !self.ready_queue.is_empty()
    }

    /// Returns task stats.
    pub fn stats(&self) -> DagStats {
        let mut stats = DagStats {
            pending: 0,
            ready: 0,
            dispatched: 0,
            completed: 0,
            failed: 0,
        };
        for state in self.states.values() {
            match state {
                TaskState::Pending => stats.pending += 1,
                TaskState::Ready => stats.ready += 1,
                TaskState::Dispatched(_) => stats.dispatched += 1,
                TaskState::Completed => stats.completed += 1,
                TaskState::Failed(_) => stats.failed += 1,
            }
        }
        stats
    }

    pub fn total_tasks(&self) -> usize {
        self.tasks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: Vec<&str>) -> TaskNode {
        TaskNode {
            task_id: id.to_string(),
            package_name: id.to_string(),
            script_name: "build".to_string(),
            command: format!("echo {id}"),
            package_path: ".".to_string(),
            depends_on: deps.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn dag_dispatches_leaves_first() {
        // a → b → c (a must run before b, b before c)
        let tasks = vec![
            task("a", vec![]),
            task("b", vec!["a"]),
            task("c", vec!["b"]),
        ];
        let mut dag = HubDag::new(tasks);

        let t = dag.dispatch_next("worker1").unwrap();
        assert_eq!(t.task_id, "a");
        assert!(dag.dispatch_next("worker1").is_none());

        let newly_ready = dag.mark_complete("a");
        assert_eq!(newly_ready, vec!["b"]);

        let t2 = dag.dispatch_next("worker2").unwrap();
        assert_eq!(t2.task_id, "b");
    }

    #[test]
    fn dag_parallel_dispatch() {
        let tasks = vec![task("a", vec![]), task("b", vec![])];
        let mut dag = HubDag::new(tasks);

        let t1 = dag.dispatch_next("w1").unwrap();
        let t2 = dag.dispatch_next("w2").unwrap();
        let ids: HashSet<_> = [t1.task_id.as_str(), t2.task_id.as_str()].into();
        assert!(ids.contains("a") && ids.contains("b"));
    }

    #[test]
    fn dag_complete_signals_done() {
        let tasks = vec![task("only", vec![])];
        let mut dag = HubDag::new(tasks);
        dag.dispatch_next("w1");
        dag.mark_complete("only");
        assert!(dag.is_done());
        assert!(!dag.has_failure());
    }

    #[test]
    fn dag_failure_detected() {
        let tasks = vec![task("a", vec![])];
        let mut dag = HubDag::new(tasks);
        dag.dispatch_next("w1");
        dag.mark_failed("a", "exit code 1");
        assert!(dag.has_failure());
        let (id, err) = dag.first_failure().unwrap();
        assert_eq!(id, "a");
        assert!(err.contains("exit code 1"));
    }

    #[test]
    fn mark_failed_cascades_to_dependents() {
        // a -> b -> c, plus d (independent of a/b/c).
        // When a fails, b and c must also be marked Failed transitively.
        // d must remain Ready.  is_done() must return true.
        let tasks = vec![
            task("a", vec![]),
            task("b", vec!["a"]),
            task("c", vec!["b"]),
            task("d", vec![]),
        ];
        let mut dag = HubDag::new(tasks);

        // Dispatch and fail a.
        let dispatched = dag.dispatch_next("w1").unwrap();
        assert!(dispatched.task_id == "a" || dispatched.task_id == "d");
        // Drive deterministically: keep dispatching until we've pulled "a", then fail it.
        let mut ids_dispatched = vec![dispatched.task_id.clone()];
        if dispatched.task_id != "a" {
            let next = dag.dispatch_next("w2").unwrap();
            ids_dispatched.push(next.task_id.clone());
        }
        dag.mark_failed("a", "boom");

        // b and c must now be Failed (transitive cascade).
        assert!(
            matches!(dag.states.get("b"), Some(TaskState::Failed(_))),
            "b should cascade to Failed when its dep a fails, got {:?}",
            dag.states.get("b")
        );
        assert!(
            matches!(dag.states.get("c"), Some(TaskState::Failed(_))),
            "c should cascade to Failed transitively, got {:?}",
            dag.states.get("c")
        );

        // d is independent and must NOT be touched.
        let d_state = dag.states.get("d").unwrap();
        assert!(
            matches!(d_state, TaskState::Ready | TaskState::Dispatched(_) | TaskState::Completed),
            "d should be unaffected by a's failure, got {:?}",
            d_state
        );

        // After completing/failing whatever's left of d, the DAG must report done.
        if matches!(d_state, TaskState::Ready) {
            let _ = dag.dispatch_next("w3");
        }
        if matches!(dag.states.get("d"), Some(TaskState::Dispatched(_))) {
            dag.mark_complete("d");
        }
        assert!(
            dag.is_done(),
            "is_done() must be true once every task is Completed or Failed"
        );
    }
}
