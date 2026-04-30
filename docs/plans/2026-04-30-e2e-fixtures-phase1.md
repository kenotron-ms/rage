# E2E Fixture Smoke Test Suite — Phase 1 Implementation Plan

> **Execution:** Use the `subagent-driven-development` workflow to implement this plan.

**Goal:** Fix three distributed-execution (DE) bugs in the rage hub/spoke crates and commit five LLM-generated TypeScript pnpm-workspace fixtures so Phase 2 can wire them into a Node-based smoke harness.

**Architecture:** Phase 1 has two halves. **Part A** patches three real bugs in `hub` and `spoke-client` (hardcoded workspace path, non-portable `sh -c`, and a non-cascading `mark_failed`) — these are prerequisites for the `distributed/` fixture to actually run. **Part B** materialises five pnpm workspaces under `tests/fixtures/`, each exercising one correctness property (cache, partial rebuild, error propagation, diamond dependency, distributed execution). Each package compiles real TypeScript via `tsc --build` and then increments `dist/run-count.txt` — the sentinel counter the future harness will read.

**Tech Stack:** Rust 2021 (cargo workspace), Tonic gRPC, Tokio, TypeScript with `tsc --build`, pnpm workspaces.

**Design doc:** `docs/plans/2026-04-30-e2e-fixtures-design.md`

---

## Important context for the implementer

You — friendly junior engineer about to execute this plan — read this section once, then never skip a step.

1. **You are working in a Cargo workspace at `/Users/ken/workspace/ms/rage`.** Every `cargo` command runs from the repo root unless stated otherwise.
2. **Run commands literally.** When a step says "Run: `cargo test -p hub 2>&1 | tail -20`", run exactly that. Do not get creative.
3. **Read before writing.** When a step says "read the file", actually open and read it. The line numbers in this plan are approximate — the actual code in your tree wins.
4. **TDD is the law.** Every code change is: write a failing test → see it fail → write the fix → see it pass. Do not write the fix first. If you do, you have no signal that the test actually exercises the bug.
5. **Commit after each logical unit.** Tasks 7 and 15 are the explicit commit gates. Do not amend or squash them — those checkpoints exist so we can revert one half without dragging the other.
6. **One correction to your instructions:** the bug-2 fix should call `scheduler::shell::command(...)` (returns `tokio::process::Command`), **not** `scheduler::shell::std_command(...)`. The spoke-client uses `.status().await`, which only compiles against the async tokio command. The async helper exists in `crates/scheduler/src/shell.rs` alongside `std_command` — use the async one.
7. **Fixture file contents are verbatim.** Where this plan shows a JSON or TS blob, paste it exactly. Even the `@fix-XX/` package-scope prefixes matter — the harness in Phase 2 will key off them.
8. **Do not run rage against the fixtures yet.** Phase 1 stops at "fixtures committed and `pnpm install` works". Wiring rage in is Phase 2.

---

## PART A — DE bug fixes

### Task 1: Add the failing test for `workspace_root` hardcode

**Files:**
- Read: `crates/hub/src/server.rs` (the whole file — it's < 250 lines)
- Read: `crates/hub/src/dag.rs` (skim the `tests` module at the bottom for the `task(id, deps)` helper pattern)
- Read: `crates/cli/src/main.rs` lines 738–830 (`cmd_hub` — see how `workspace: &Path` is currently passed in and how `HubServer::new` is called at ~line 809)
- Modify: `crates/hub/src/server.rs` (add a `#[cfg(test)] mod tests` block at the bottom)

**Step 1: Confirm the bug location.**

Run:
```
grep -n '"/workspace"' crates/hub/src/server.rs
```
Expected: one hit, on a line inside the `subscribe` method that constructs a `WorkItem`. Note the line number — it's the line you will change in Task 2.

**Step 2: Append a failing test module to `crates/hub/src/server.rs`.**

Add this block at the very end of the file (after the closing `}` of `impl Coordinator for HubServer`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::TaskNode;
    use std::path::PathBuf;

    fn one_task() -> Vec<TaskNode> {
        vec![TaskNode {
            task_id: "pkg-a#build".to_string(),
            package_name: "pkg-a".to_string(),
            script_name: "build".to_string(),
            command: "echo hi".to_string(),
            package_path: "packages/pkg-a".to_string(),
            depends_on: vec![],
        }]
    }

    #[tokio::test]
    async fn work_item_carries_real_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace: PathBuf = tmp.path().to_path_buf();

        let hub = HubServer::new(
            one_task(),
            "tok".to_string(),
            "build-1".to_string(),
            workspace.clone(),
        );

        // Reach into the dag and dispatch directly — same code path the
        // gRPC subscribe() handler uses to build a WorkItem.
        let task = {
            let mut state = hub.state.lock().await;
            state.dag.dispatch_next("worker-1").unwrap()
        };

        // Reconstruct the WorkItem the way subscribe() does. If server.rs
        // still hardcodes "/workspace", this assertion will fail because
        // the hub will not have stored `workspace` anywhere.
        let work_item = hub.build_work_item(&task);

        assert_eq!(
            work_item.workspace_root,
            workspace.to_string_lossy().to_string(),
            "WorkItem.workspace_root must reflect the real workspace path, \
             not a hardcoded \"/workspace\""
        );
    }
}
```

This test depends on three things that don't yet exist: a 4-arg `HubServer::new`, a public `state` field, and a `build_work_item` helper. That's intentional — the test is the spec for the refactor in Task 2.

**Step 3: Run the test to confirm it fails to compile.**

Run:
```
cargo test -p hub work_item_carries_real_workspace_root 2>&1 | tail -30
```
Expected: a compile error mentioning `HubServer::new` taking 3 arguments (not 4) and/or `state` being private and/or `build_work_item` not found. **Do not fix it yet.** A red bar is the goal.

---

### Task 2: Fix `workspace_root` hardcode

**Files:**
- Modify: `crates/hub/src/server.rs`
- Modify: `crates/cli/src/main.rs` line ~809 (the `HubServer::new` call inside `cmd_hub`)

**Step 1: Add the workspace field and helper to `HubServer`.**

In `crates/hub/src/server.rs`:

1. Add `use std::path::PathBuf;` to the imports near the top.
2. Change the struct definition:

   ```rust
   #[derive(Clone)]
   pub struct HubServer {
       pub(crate) state: Arc<Mutex<HubState>>,
       token: String,
       notify: Arc<Notify>,
       workspace: PathBuf,
   }
   ```
   (Note `pub(crate)` on `state` so the test in the same crate can lock it.)

3. Replace the `new` signature and body:

   ```rust
   pub fn new(
       tasks: Vec<TaskNode>,
       token: String,
       build_id: String,
       workspace: PathBuf,
   ) -> Self {
       Self {
           state: Arc::new(Mutex::new(HubState {
               dag: HubDag::new(tasks),
               build_id,
           })),
           token,
           notify: Arc::new(Notify::new()),
           workspace,
       }
   }
   ```

4. Add a `build_work_item` helper just below `new` inside the same `impl HubServer` block:

   ```rust
   pub(crate) fn build_work_item(&self, task: &TaskNode) -> WorkItem {
       WorkItem {
           task_id: task.task_id.clone(),
           package_name: task.package_name.clone(),
           script_name: task.script_name.clone(),
           command: task.command.clone(),
           workspace_root: self.workspace.to_string_lossy().to_string(),
           package_path: task.package_path.clone(),
           input_refs: vec![],
           cache_backend_url: String::new(),
           env: std::collections::HashMap::new(),
       }
   }
   ```

5. Inside the `subscribe` async stream, replace the inline `WorkItem { ... }` construction with a call through the helper. Find the block currently reading:

   ```rust
   s.dag.dispatch_next(&worker_id).map(|task| WorkItem {
       task_id: task.task_id.clone(),
       /* ... lots of fields ... */
       workspace_root: "/workspace".to_string(),
       /* ... */
   })
   ```

   Replace it with:

   ```rust
   s.dag.dispatch_next(&worker_id).map(|task| self_for_stream.build_work_item(&task))
   ```

   Because the `async_stream::stream! { ... }` macro captures by move, you need to clone `self` before entering the stream. Just before `let stream = async_stream::stream! {`, add:

   ```rust
   let self_for_stream = self.clone();
   ```

   `HubServer` already derives `Clone` and all its fields are `Arc`/`PathBuf`, so this is cheap.

**Step 2: Update the only caller in `crates/cli/src/main.rs`.**

Find line ~809:
```rust
let hub = HubServer::new(task_nodes, token.clone(), build_id.clone());
```
Replace with:
```rust
let hub = HubServer::new(
    task_nodes,
    token.clone(),
    build_id.clone(),
    workspace.to_path_buf(),
);
```
(`workspace` is the `&Path` parameter of `cmd_hub`.)

**Step 3: Verify the test now passes.**

Run:
```
cargo test -p hub work_item_carries_real_workspace_root 2>&1 | tail -30
```
Expected: 1 passed.

**Step 4: Run the full hub test suite to make sure nothing else broke.**

Run:
```
cargo test -p hub 2>&1 | tail -30
```
Expected: all tests pass.

---

### Task 3: Fix `sh -c` in spoke-client

**Files:**
- Read: `crates/spoke-client/src/lib.rs` (the whole file — it's small)
- Read: `crates/spoke-client/Cargo.toml`
- Read: `crates/scheduler/src/shell.rs` (so you can see `command` vs `std_command`)
- Modify: `crates/spoke-client/Cargo.toml`
- Modify: `crates/spoke-client/src/lib.rs`

**Step 1: Add `scheduler` as a dependency.**

In `crates/spoke-client/Cargo.toml`, under `[dependencies]`, add:
```toml
scheduler = { path = "../scheduler" }
```
(Place it alphabetically near the other path-style deps if any exist; otherwise just append it under the existing `[dependencies]` block.)

**Step 2: Replace the hardcoded `sh -c` with the cross-platform helper.**

Find this block in `crates/spoke-client/src/lib.rs` (inside `async fn execute`, around line 140):

```rust
let status = tokio::process::Command::new("sh")
    .arg("-c")
    .arg(&item.command)
    .current_dir(&pkg_dir)
    .status()
    .await?;
```

Replace it with:

```rust
let status = scheduler::shell::command(&item.command)
    .current_dir(&pkg_dir)
    .status()
    .await?;
```

> **Why `command` not `std_command`?** This call site uses `.status().await`, which means we need the async tokio variant. `scheduler::shell::std_command` returns `std::process::Command` (no `.await`). `scheduler::shell::command` returns `tokio::process::Command`. Both pick `sh -c` on Unix and `cmd /c` on Windows — that's the cross-platform fix.

**Step 3: Verify the spoke-client compiles.**

Run:
```
cargo check -p spoke-client 2>&1 | tail -20
```
Expected: `Finished` with no errors. Warnings are OK.

**Step 4: Confirm no `Command::new("sh"` strings remain in spoke-client.**

Run:
```
grep -n 'Command::new("sh"' crates/spoke-client/src/lib.rs
```
Expected: zero matches.

---

### Task 4: Add the failing test for `mark_failed` cascade

**Files:**
- Read: `crates/hub/src/dag.rs` — specifically the `mark_failed` method (~line 105) and the existing `mod tests` block at the bottom for the `task(id, deps)` helper and assertion patterns.
- Modify: `crates/hub/src/dag.rs` (append a new test in the existing `mod tests`)

**Step 1: Append the failing test inside the existing test module.**

Locate the closing brace of `mod tests { ... }` at the bottom of `crates/hub/src/dag.rs`. Just before that closing brace, add:

```rust
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
```

This test will not compile yet — `dag.states` is private. That's fine; we'll address the visibility in Task 5 alongside the cascade fix. **Do not** make `states` public yet.

**Step 2: Make `states` test-visible.**

In `crates/hub/src/dag.rs`, find:
```rust
pub struct HubDag {
    tasks: HashMap<String, TaskNode>,
    states: HashMap<String, TaskState>,
    rdeps: HashMap<String, HashSet<String>>,
    ready_queue: VecDeque<String>,
}
```
Change `states` to `pub(crate) states`:
```rust
pub struct HubDag {
    tasks: HashMap<String, TaskNode>,
    pub(crate) states: HashMap<String, TaskState>,
    rdeps: HashMap<String, HashSet<String>>,
    ready_queue: VecDeque<String>,
}
```
(The test lives in the same crate, so `pub(crate)` is enough.)

**Step 3: Run the test and confirm it fails for the right reason.**

Run:
```
cargo test -p hub mark_failed_cascades_to_dependents 2>&1 | tail -30
```
Expected: 1 failed. The failure message should mention `b` (or `c`) being `Pending` when the test expects `Failed`. **If it fails to compile**, fix the compile error and re-run; the goal is a clean assertion-style failure.

---

### Task 5: Implement the cascading `mark_failed`

**Files:**
- Modify: `crates/hub/src/dag.rs`

**Step 1: Replace `mark_failed` with a cascading version.**

Find:
```rust
pub fn mark_failed(&mut self, task_id: &str, error: &str) -> Vec<String> {
    self.states
        .insert(task_id.to_string(), TaskState::Failed(error.to_string()));
    // In simple mode: unblock dependents anyway (they'll fail too when trying to run)
    // For now, we don't unblock dependents of failed tasks.
    Vec::new()
}
```
Replace with:
```rust
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
```

**Step 2: Run only the new test.**

Run:
```
cargo test -p hub mark_failed_cascades_to_dependents 2>&1 | tail -30
```
Expected: 1 passed.

**Step 3: Run the whole hub crate.**

Run:
```
cargo test -p hub 2>&1 | tail -30
```
Expected: every existing dag test (`dag_dispatches_leaves_first`, `dag_parallel_dispatch`, `dag_complete_signals_done`, `dag_failure_detected`) plus the two new ones pass.

---

### Task 6: Workspace-wide green build

**Files:** none — this is a verification step.

**Step 1: Run the full workspace test suite.**

Run:
```
cargo test --workspace 2>&1 | tail -40
```
Expected: every crate compiles, every test passes.

**Step 2: If anything red, fix only the genuine fallout.**

The most likely fallout is from changing the `HubServer::new` signature: any other call site would need updating. You already updated `cmd_hub` in Task 2. If `cargo` complains about a different file (test binary, doc test, etc.), update it the same way (`workspace.to_path_buf()` is the value to pass when there's an obvious workspace path in scope; a `tempfile::tempdir()` path is fine for tests).

Run again until clean:
```
cargo test --workspace 2>&1 | tail -40
```
Expected: zero failures.

**Step 3: Run clippy for the touched crates.**

Run:
```
cargo clippy -p hub -p spoke-client -p cli --all-targets 2>&1 | tail -30
```
Expected: no new warnings introduced. Pre-existing warnings in untouched code are not your problem.

---

### Task 7: Commit Part A

**Step 1: Stage exactly the files we changed.**

Run:
```
git status
```
Expected: modifications to `crates/hub/src/server.rs`, `crates/hub/src/dag.rs`, `crates/spoke-client/src/lib.rs`, `crates/spoke-client/Cargo.toml`, `crates/cli/src/main.rs`, and an updated `Cargo.lock`.

**Step 2: Commit.**

Run:
```
git add crates/hub/src/server.rs crates/hub/src/dag.rs crates/spoke-client/src/lib.rs crates/spoke-client/Cargo.toml crates/cli/src/main.rs Cargo.lock
git commit -m "fix(hub,spoke): real workspace_root, cascading mark_failed, cross-platform shell

- HubServer now stores the workspace path and threads it into every
  WorkItem, replacing the hardcoded \"/workspace\" that broke on
  macOS/Windows.
- HubDag::mark_failed now transitively fails all dependents and
  flushes them from the ready queue, so failures no longer stall
  the DAG.
- spoke-client now dispatches commands via scheduler::shell::command,
  picking sh on Unix and cmd on Windows."
```

Expected: commit succeeds. Run `git log -1 --stat` to confirm exactly the six expected paths landed.

---

## PART B — Five fixture workspaces

> **Conventions for every fixture below.**
>
> - Every fixture lives at `tests/fixtures/<fixture-name>/` (you will create the `tests/fixtures/` parent on first use).
> - Every fixture is its own pnpm workspace.
> - Every package has the layout `packages/<pkg-name>/{package.json, tsconfig.json, src/index.ts}`.
> - Every package's `scripts.build` (except `error-propagation/packages/pkg-b`) is the **counter build script** below.
> - Every package's `tsconfig.json` extends the fixture-root `tsconfig.base.json`.
> - Package names use the fixture-specific scope: `@fix-cc`, `@fix-pr`, `@fix-ep`, `@fix-dd`, `@fix-dist`.
>
> **The counter build script (paste verbatim into `scripts.build`):**
> ```
> tsc --build && node -e "const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))"
> ```
> When this appears inside JSON, it is one string under `"build"`. The inner double quotes are escaped — JSON does not allow newlines inside strings.
>
> **The shared `tsconfig.base.json` (identical at every fixture root):**
> ```json
> {
>   "compilerOptions": {
>     "target": "ES2020",
>     "module": "commonjs",
>     "moduleResolution": "node",
>     "declaration": true,
>     "declarationMap": true,
>     "sourceMap": true,
>     "outDir": "./dist",
>     "rootDir": "./src",
>     "strict": true,
>     "esModuleInterop": true,
>     "composite": true
>   }
> }
> ```
>
> **The shared `pnpm-workspace.yaml` (identical at every fixture root):**
> ```yaml
> packages:
>   - 'packages/*'
> ```
>
> **Per-package `tsconfig.json` template:**
> ```json
> {
>   "extends": "../../tsconfig.base.json",
>   "compilerOptions": {
>     "outDir": "./dist",
>     "rootDir": "./src"
>   },
>   "include": ["src"],
>   "references": []
> }
> ```
> Replace the `references` array per-package; if the package has no workspace deps, leave it as `[]`.
>
> **Per-package `package.json` template (you fill in name, deps, version):**
> ```json
> {
>   "name": "@fix-XX/pkg-NAME",
>   "version": "1.0.0",
>   "private": true,
>   "main": "dist/index.js",
>   "types": "dist/index.d.ts",
>   "scripts": {
>     "build": "tsc --build && node -e \"const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))\""
>   },
>   "dependencies": {}
> }
> ```
>
> **Per-fixture root `package.json` template:**
> ```json
> {
>   "name": "@fix-XX/root",
>   "version": "1.0.0",
>   "private": true
> }
> ```
> (No `workspaces` field — pnpm reads `pnpm-workspace.yaml` instead.)

---

### Task 8: Fixture 1 — `cache-correctness` (linear chain)

Graph: `pkg-core` → `pkg-utils` → `pkg-app`.

**Files (all created):**
- `tests/fixtures/cache-correctness/pnpm-workspace.yaml`
- `tests/fixtures/cache-correctness/package.json`
- `tests/fixtures/cache-correctness/tsconfig.base.json`
- `tests/fixtures/cache-correctness/.gitignore`
- `tests/fixtures/cache-correctness/packages/pkg-core/package.json`
- `tests/fixtures/cache-correctness/packages/pkg-core/tsconfig.json`
- `tests/fixtures/cache-correctness/packages/pkg-core/src/index.ts`
- `tests/fixtures/cache-correctness/packages/pkg-utils/package.json`
- `tests/fixtures/cache-correctness/packages/pkg-utils/tsconfig.json`
- `tests/fixtures/cache-correctness/packages/pkg-utils/src/index.ts`
- `tests/fixtures/cache-correctness/packages/pkg-app/package.json`
- `tests/fixtures/cache-correctness/packages/pkg-app/tsconfig.json`
- `tests/fixtures/cache-correctness/packages/pkg-app/src/index.ts`

**Step 1: Create the directory tree.**

Run:
```
mkdir -p tests/fixtures/cache-correctness/packages/pkg-core/src tests/fixtures/cache-correctness/packages/pkg-utils/src tests/fixtures/cache-correctness/packages/pkg-app/src
```

**Step 2: Write the workspace-root files.**

Write `tests/fixtures/cache-correctness/pnpm-workspace.yaml`:
```yaml
packages:
  - 'packages/*'
```

Write `tests/fixtures/cache-correctness/package.json`:
```json
{
  "name": "@fix-cc/root",
  "version": "1.0.0",
  "private": true
}
```

Write `tests/fixtures/cache-correctness/tsconfig.base.json`:
```json
{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "strict": true,
    "esModuleInterop": true,
    "composite": true
  }
}
```

Write `tests/fixtures/cache-correctness/.gitignore`:
```
node_modules/
packages/*/dist/
packages/*/tsconfig.tsbuildinfo
```

**Step 3: Write `pkg-core` (no deps).**

Write `tests/fixtures/cache-correctness/packages/pkg-core/package.json`:
```json
{
  "name": "@fix-cc/pkg-core",
  "version": "1.0.0",
  "private": true,
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {
    "build": "tsc --build && node -e \"const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))\""
  },
  "dependencies": {}
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-core/tsconfig.json`:
```json
{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": {
    "outDir": "./dist",
    "rootDir": "./src"
  },
  "include": ["src"],
  "references": []
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-core/src/index.ts`:
```ts
export const CORE_VERSION = "1.0.0";
export function greet(name: string): string {
  return `Hello, ${name}!`;
}
```

**Step 4: Write `pkg-utils` (depends on pkg-core).**

Write `tests/fixtures/cache-correctness/packages/pkg-utils/package.json`:
```json
{
  "name": "@fix-cc/pkg-utils",
  "version": "1.0.0",
  "private": true,
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {
    "build": "tsc --build && node -e \"const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))\""
  },
  "dependencies": {
    "@fix-cc/pkg-core": "workspace:*"
  }
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-utils/tsconfig.json`:
```json
{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": {
    "outDir": "./dist",
    "rootDir": "./src"
  },
  "include": ["src"],
  "references": [
    { "path": "../pkg-core" }
  ]
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-utils/src/index.ts`:
```ts
import { greet } from "@fix-cc/pkg-core";
export const formatGreeting = (name: string): string => greet(name).toUpperCase();
```

**Step 5: Write `pkg-app` (depends on pkg-utils).**

Write `tests/fixtures/cache-correctness/packages/pkg-app/package.json`:
```json
{
  "name": "@fix-cc/pkg-app",
  "version": "1.0.0",
  "private": true,
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {
    "build": "tsc --build && node -e \"const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))\""
  },
  "dependencies": {
    "@fix-cc/pkg-utils": "workspace:*"
  }
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-app/tsconfig.json`:
```json
{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": {
    "outDir": "./dist",
    "rootDir": "./src"
  },
  "include": ["src"],
  "references": [
    { "path": "../pkg-utils" }
  ]
}
```

Write `tests/fixtures/cache-correctness/packages/pkg-app/src/index.ts`:
```ts
import { formatGreeting } from "@fix-cc/pkg-utils";
export const main = (): void => {
  console.log(formatGreeting("World"));
};
```

**Step 6: Verify the directory shape.**

Run:
```
ls tests/fixtures/cache-correctness/packages/
```
Expected: three lines — `pkg-app`, `pkg-core`, `pkg-utils`.

---

### Task 9: Fixture 2 — `partial-rebuild` (chain plus independent)

Graph: `pkg-a` → `pkg-b` → `pkg-c`, plus `pkg-d` (independent of all of them).

**Step 1: Create the directory tree.**

Run:
```
mkdir -p tests/fixtures/partial-rebuild/packages/pkg-a/src tests/fixtures/partial-rebuild/packages/pkg-b/src tests/fixtures/partial-rebuild/packages/pkg-c/src tests/fixtures/partial-rebuild/packages/pkg-d/src
```

**Step 2: Write the four root files.**

Write `tests/fixtures/partial-rebuild/pnpm-workspace.yaml`, `tests/fixtures/partial-rebuild/tsconfig.base.json`, and `tests/fixtures/partial-rebuild/.gitignore` with the **same content** as in Task 8 (only the path changes).

Write `tests/fixtures/partial-rebuild/package.json`:
```json
{
  "name": "@fix-pr/root",
  "version": "1.0.0",
  "private": true
}
```

**Step 3: Write `pkg-a` (no deps, references `[]`).**

`packages/pkg-a/package.json` — name `@fix-pr/pkg-a`, empty `dependencies`, counter build script.

`packages/pkg-a/tsconfig.json` — standard template, `references: []`.

`packages/pkg-a/src/index.ts`:
```ts
export const VERSION_A = "1.0.0";
export const compute = (n: number): number => n * 2;
```

**Step 4: Write `pkg-b` (depends on pkg-a).**

`packages/pkg-b/package.json` — name `@fix-pr/pkg-b`, `dependencies: { "@fix-pr/pkg-a": "workspace:*" }`, counter build script.

`packages/pkg-b/tsconfig.json` — standard template, `references: [ { "path": "../pkg-a" } ]`.

`packages/pkg-b/src/index.ts`:
```ts
import { compute } from "@fix-pr/pkg-a";
export const transform = (n: number): number => compute(n) + 1;
```

**Step 5: Write `pkg-c` (depends on pkg-b).**

`packages/pkg-c/package.json` — name `@fix-pr/pkg-c`, `dependencies: { "@fix-pr/pkg-b": "workspace:*" }`, counter build script.

`packages/pkg-c/tsconfig.json` — standard template, `references: [ { "path": "../pkg-b" } ]`.

`packages/pkg-c/src/index.ts`:
```ts
import { transform } from "@fix-pr/pkg-b";
export const process = (n: number): number => transform(n) * 3;
```

**Step 6: Write `pkg-d` (independent, no deps).**

`packages/pkg-d/package.json` — name `@fix-pr/pkg-d`, empty `dependencies`, counter build script.

`packages/pkg-d/tsconfig.json` — standard template, `references: []`.

`packages/pkg-d/src/index.ts`:
```ts
export const ISOLATED = "independent";
export const helper = (): string => "helper";
```

**Step 7: Verify.**

Run:
```
ls tests/fixtures/partial-rebuild/packages/
```
Expected: `pkg-a`, `pkg-b`, `pkg-c`, `pkg-d`.

---

### Task 10: Fixture 3 — `error-propagation`

Graph: `pkg-a` (independent, succeeds), `pkg-b` (independent, **always fails**), `pkg-c` (depends on `pkg-b`, must never run).

**Step 1: Create the directory tree.**

Run:
```
mkdir -p tests/fixtures/error-propagation/packages/pkg-a/src tests/fixtures/error-propagation/packages/pkg-b/src tests/fixtures/error-propagation/packages/pkg-c/src
```

**Step 2: Write the root files** (same as the other fixtures, with `@fix-ep/root` as the root package name).

**Step 3: Write `pkg-a` (succeeds normally, counter script).**

`packages/pkg-a/package.json` — name `@fix-ep/pkg-a`, empty `dependencies`, **standard counter build script**.

`packages/pkg-a/tsconfig.json` — `references: []`.

`packages/pkg-a/src/index.ts`:
```ts
export const SUCCESS = true;
export const value = (): string => "ok";
```

**Step 4: Write `pkg-b` — the deliberately failing one.**

`packages/pkg-b/package.json` — special build script: **just `exit 1`**. No `tsc`, no counter. This is the only package in the entire suite that doesn't use the counter script.
```json
{
  "name": "@fix-ep/pkg-b",
  "version": "1.0.0",
  "private": true,
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {
    "build": "exit 1"
  },
  "dependencies": {}
}
```

`packages/pkg-b/tsconfig.json` — standard template, `references: []` (we still want a valid tsconfig so workspace tooling parses cleanly).

`packages/pkg-b/src/index.ts`:
```ts
export const WILL_FAIL = "never built";
```

**Step 5: Write `pkg-c` — depends on the failing pkg-b.**

`packages/pkg-c/package.json` — name `@fix-ep/pkg-c`, `dependencies: { "@fix-ep/pkg-b": "workspace:*" }`, **standard counter build script** (it should never run, so the counter must remain absent — which is exactly what we'll assert).

`packages/pkg-c/tsconfig.json` — `references: [ { "path": "../pkg-b" } ]`.

`packages/pkg-c/src/index.ts`:
```ts
export const DOWNSTREAM = "should never build";
```

**Step 6: Verify.**

Run:
```
ls tests/fixtures/error-propagation/packages/
```
Expected: `pkg-a`, `pkg-b`, `pkg-c`.

Run:
```
grep -l '"build": "exit 1"' tests/fixtures/error-propagation/packages/pkg-b/package.json
```
Expected: the file path printed back at you (proving the special script landed).

---

### Task 11: Fixture 4 — `diamond-dep`

Graph: `pkg-shared` → `pkg-a`, `pkg-shared` → `pkg-b`, plus `pkg-a` + `pkg-b` → `pkg-app`. All four packages use the standard counter script.

**Step 1: Create the directory tree.**

Run:
```
mkdir -p tests/fixtures/diamond-dep/packages/pkg-shared/src tests/fixtures/diamond-dep/packages/pkg-a/src tests/fixtures/diamond-dep/packages/pkg-b/src tests/fixtures/diamond-dep/packages/pkg-app/src
```

**Step 2: Write the root files** (`@fix-dd/root`, standard `tsconfig.base.json`, `pnpm-workspace.yaml`, `.gitignore`).

**Step 3: Write `pkg-shared` (no deps).**

`packages/pkg-shared/package.json` — name `@fix-dd/pkg-shared`, empty `dependencies`, counter script.

`packages/pkg-shared/tsconfig.json` — `references: []`.

`packages/pkg-shared/src/index.ts`:
```ts
export const SHARED = "shared-value";
export const double = (n: number): number => n * 2;
```

**Step 4: Write `pkg-a` (depends on pkg-shared).**

`packages/pkg-a/package.json` — name `@fix-dd/pkg-a`, `dependencies: { "@fix-dd/pkg-shared": "workspace:*" }`, counter script.

`packages/pkg-a/tsconfig.json` — `references: [ { "path": "../pkg-shared" } ]`.

`packages/pkg-a/src/index.ts`:
```ts
import { double } from "@fix-dd/pkg-shared";
export const A_RESULT: number = double(21);
```

**Step 5: Write `pkg-b` (depends on pkg-shared).**

`packages/pkg-b/package.json` — name `@fix-dd/pkg-b`, `dependencies: { "@fix-dd/pkg-shared": "workspace:*" }`, counter script.

`packages/pkg-b/tsconfig.json` — `references: [ { "path": "../pkg-shared" } ]`.

`packages/pkg-b/src/index.ts`:
```ts
import { SHARED } from "@fix-dd/pkg-shared";
export const B_RESULT: string = SHARED + "-b";
```

**Step 6: Write `pkg-app` (depends on both pkg-a and pkg-b).**

`packages/pkg-app/package.json` — name `@fix-dd/pkg-app`, dependencies map both `@fix-dd/pkg-a` and `@fix-dd/pkg-b` to `workspace:*`, counter script.

`packages/pkg-app/tsconfig.json` — `references: [ { "path": "../pkg-a" }, { "path": "../pkg-b" } ]`.

`packages/pkg-app/src/index.ts`:
```ts
import { A_RESULT } from "@fix-dd/pkg-a";
import { B_RESULT } from "@fix-dd/pkg-b";
export const APP_RESULT: string = `${A_RESULT}-${B_RESULT}`;
```

**Step 7: Verify.**

Run:
```
ls tests/fixtures/diamond-dep/packages/
```
Expected: `pkg-a`, `pkg-app`, `pkg-b`, `pkg-shared`.

---

### Task 12: Fixture 5 — `distributed`

Graph: roots `pkg-a` and `pkg-b` (no deps); `pkg-c` depends on `pkg-a`; `pkg-d` depends on `pkg-b`; `pkg-e` depends on both `pkg-c` and `pkg-d`.

No `rage.json` is needed in Phase 1 — the cache backend is still a stub.

**Step 1: Create the directory tree.**

Run:
```
mkdir -p tests/fixtures/distributed/packages/pkg-a/src tests/fixtures/distributed/packages/pkg-b/src tests/fixtures/distributed/packages/pkg-c/src tests/fixtures/distributed/packages/pkg-d/src tests/fixtures/distributed/packages/pkg-e/src
```

**Step 2: Write the root files** (`@fix-dist/root`, etc.).

**Step 3: Write `pkg-a` (no deps).**

`packages/pkg-a/package.json` — name `@fix-dist/pkg-a`, empty `dependencies`, counter script.

`packages/pkg-a/tsconfig.json` — `references: []`.

`packages/pkg-a/src/index.ts`:
```ts
export const NODE_A = "node-a";
export const computeA = (): number => NODE_A.length;
```

**Step 4: Write `pkg-b` (no deps).**

`packages/pkg-b/package.json` — name `@fix-dist/pkg-b`, empty `dependencies`, counter script.

`packages/pkg-b/tsconfig.json` — `references: []`.

`packages/pkg-b/src/index.ts`:
```ts
export const NODE_B = "node-b";
export const computeB = (): number => NODE_B.length;
```

**Step 5: Write `pkg-c` (depends on pkg-a).**

`packages/pkg-c/package.json` — name `@fix-dist/pkg-c`, `dependencies: { "@fix-dist/pkg-a": "workspace:*" }`, counter script.

`packages/pkg-c/tsconfig.json` — `references: [ { "path": "../pkg-a" } ]`.

`packages/pkg-c/src/index.ts`:
```ts
import { computeA } from "@fix-dist/pkg-a";
export const C: number = computeA() * 2;
```

**Step 6: Write `pkg-d` (depends on pkg-b).**

`packages/pkg-d/package.json` — name `@fix-dist/pkg-d`, `dependencies: { "@fix-dist/pkg-b": "workspace:*" }`, counter script.

`packages/pkg-d/tsconfig.json` — `references: [ { "path": "../pkg-b" } ]`.

`packages/pkg-d/src/index.ts`:
```ts
import { computeB } from "@fix-dist/pkg-b";
export const D: number = computeB() * 3;
```

**Step 7: Write `pkg-e` (depends on pkg-c and pkg-d).**

`packages/pkg-e/package.json` — name `@fix-dist/pkg-e`, `dependencies` maps both `@fix-dist/pkg-c` and `@fix-dist/pkg-d` to `workspace:*`, counter script.

`packages/pkg-e/tsconfig.json` — `references: [ { "path": "../pkg-c" }, { "path": "../pkg-d" } ]`.

`packages/pkg-e/src/index.ts`:
```ts
import { C } from "@fix-dist/pkg-c";
import { D } from "@fix-dist/pkg-d";
export const E: number = C + D;
```

**Step 8: Verify.**

Run:
```
ls tests/fixtures/distributed/packages/
```
Expected: `pkg-a`, `pkg-b`, `pkg-c`, `pkg-d`, `pkg-e`.

---

### Task 13: `pnpm install` + TypeScript build verification

This task does two things: (a) generates `pnpm-lock.yaml` for every fixture (we want lockfiles checked in), and (b) confirms the four buildable fixtures actually compile.

**Prerequisite:** Make sure `pnpm` is on your `PATH`. Run `pnpm --version`. Expected: a version string. If not found, run `corepack enable && corepack prepare pnpm@latest --activate` and retry.

**Step 1: Install + build `cache-correctness`.**

Run:
```
cd tests/fixtures/cache-correctness && pnpm install && pnpm -r exec tsc --build && cd -
```
Expected: `pnpm install` succeeds; `tsc --build` exits 0; `tests/fixtures/cache-correctness/pnpm-lock.yaml` exists.

**Step 2: Install + build `partial-rebuild`.**

Run:
```
cd tests/fixtures/partial-rebuild && pnpm install && pnpm -r exec tsc --build && cd -
```
Expected: same as above for `partial-rebuild`.

**Step 3: Install + build `diamond-dep`.**

Run:
```
cd tests/fixtures/diamond-dep && pnpm install && pnpm -r exec tsc --build && cd -
```
Expected: same.

**Step 4: Install + build `distributed`.**

Run:
```
cd tests/fixtures/distributed && pnpm install && pnpm -r exec tsc --build && cd -
```
Expected: same.

**Step 5: Install only — `error-propagation`.**

Do **not** run `tsc --build` on this one. `pkg-b`'s build script is `exit 1` (no compile), and `pkg-c`'s tsconfig references `pkg-b`, which has no `dist/` to consume. Verifying compilation here would be a false negative.

Run:
```
cd tests/fixtures/error-propagation && pnpm install && cd -
```
Expected: `pnpm install` succeeds; `pnpm-lock.yaml` is generated.

**Step 6: Clean up build artefacts before committing.**

You don't want to commit `dist/` directories or `tsbuildinfo` files. The `.gitignore` you created in each fixture already excludes them, but double-check:

Run:
```
git status tests/fixtures/ | head -30
```
Expected: only source files (`package.json`, `tsconfig*.json`, `src/*.ts`, `pnpm-workspace.yaml`, `pnpm-lock.yaml`, `.gitignore`) appear under "Untracked". No `dist/`, no `tsconfig.tsbuildinfo`, no `node_modules/`.

If anything unexpected appears, add it to the per-fixture `.gitignore` and re-run `git status`.

---

### Task 14: Lockfile audit

**Step 1: Verify all five lockfiles exist.**

Run:
```
ls tests/fixtures/*/pnpm-lock.yaml
```
Expected: exactly five paths printed:
```
tests/fixtures/cache-correctness/pnpm-lock.yaml
tests/fixtures/diamond-dep/pnpm-lock.yaml
tests/fixtures/distributed/pnpm-lock.yaml
tests/fixtures/error-propagation/pnpm-lock.yaml
tests/fixtures/partial-rebuild/pnpm-lock.yaml
```

**Step 2: If any are missing**, `cd` into that fixture and run `pnpm install`, then re-run the `ls`.

**Step 3: Confirm no stray top-level lockfile/node_modules made it in.**

Run:
```
ls tests/fixtures/pnpm-lock.yaml tests/fixtures/node_modules 2>/dev/null
```
Expected: both lines say "No such file or directory" (because the fixtures are independent workspaces — there's no parent workspace at `tests/fixtures/`). If something exists, delete it: `rm -rf tests/fixtures/node_modules tests/fixtures/pnpm-lock.yaml`.

---

### Task 15: Commit Part B

**Step 1: Inspect what's about to be staged.**

Run:
```
git status tests/fixtures/
```
Expected: a long list of new files under each of the five fixtures, all source/config (no `dist/`, no `node_modules/`, no `tsconfig.tsbuildinfo`).

**Step 2: Stage and commit.**

Run:
```
git add tests/fixtures/
git commit -m "feat(fixtures): five LLM-generated e2e fixture workspaces

Adds cache-correctness, partial-rebuild, error-propagation, diamond-dep,
and distributed pnpm workspaces under tests/fixtures/.  Each package
compiles real TypeScript via tsc --build and records its execution
in dist/run-count.txt — the sentinel counter the Phase 2 harness will
read.  pnpm-lock.yaml committed for each fixture so CI installs are
deterministic.

Phase 1 ships the data; Phase 2 wires the run.mjs harness."
```

**Step 3: Sanity-check the final state.**

Run:
```
git log --oneline -2
git status
```
Expected: two new commits (Part A and Part B); working tree clean.

---

## Definition of Done for Phase 1

- [ ] `cargo test --workspace` passes with zero failures.
- [ ] `cargo clippy -p hub -p spoke-client -p cli --all-targets` introduces no new warnings.
- [ ] `crates/hub/src/server.rs` no longer contains the literal `"/workspace"`.
- [ ] `crates/hub/src/dag.rs` has the new `mark_failed_cascades_to_dependents` test, and it passes.
- [ ] `crates/spoke-client/src/lib.rs` no longer contains `Command::new("sh"`.
- [ ] `crates/spoke-client/Cargo.toml` lists `scheduler = { path = "../scheduler" }`.
- [ ] Five fixture directories under `tests/fixtures/` exist, each with the layout above.
- [ ] `ls tests/fixtures/*/pnpm-lock.yaml` returns five lines.
- [ ] Two commits exist on `main` (or your feature branch): the bug-fix commit and the fixture commit.
- [ ] No `dist/`, `node_modules/`, or `tsconfig.tsbuildinfo` is tracked in git anywhere under `tests/fixtures/`.

When everything above is checked, Phase 1 is complete and Phase 2 (the Node harness) can begin.

---

## Quick command reference

| What | Command |
|---|---|
| Compile `hub` only | `cargo check -p hub` |
| Test `hub` only | `cargo test -p hub 2>&1 \| tail -30` |
| Test one specific test | `cargo test -p hub <test_name> 2>&1 \| tail -30` |
| Compile `spoke-client` only | `cargo check -p spoke-client` |
| Full workspace test | `cargo test --workspace 2>&1 \| tail -40` |
| Clippy on touched crates | `cargo clippy -p hub -p spoke-client -p cli --all-targets` |
| Install one fixture | `cd tests/fixtures/<name> && pnpm install && cd -` |
| Build one fixture | `cd tests/fixtures/<name> && pnpm -r exec tsc --build && cd -` |
| List fixture lockfiles | `ls tests/fixtures/*/pnpm-lock.yaml` |
