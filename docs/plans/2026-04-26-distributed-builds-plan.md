# Phase: Distributed Build Hub/Spoke — Self-Hosted DTE

> **Execution:** Use the subagent-driven-development workflow.

**Goal:** Implement the hub/spoke distributed task execution described in
`docs/plans/2026-04-24-rage-daemon-config-cache-design.md` Section 4.
Validated with a real multi-node Docker Compose test using 3 Linux containers
(1 hub + 2 spokes) that coordinate build tasks from the lage workspace.

**MSRV:** Rust 1.91, `unsafe_code = "forbid"`.

**COE Constraints:**
1. gRPC transport between hub and spokes (tonic + prost). NOT REST polling.
2. Hub crash recovery: rerun from scratch (in-memory DAG, no SQLite).
3. Artifact routing through remote cache — hub is NOT a data plane.
4. Hub rendezvous via shared filesystem file (Docker volume replaces S3 for tests).
5. gRPC keepalive: set `tcp_keepalive` and `timeout` to survive Docker bridge network.
6. The proto file is `proto/coordinator.proto` — already written.
7. The hub binary is the same `rage` binary started with `daemon --hub` flag.
8. The novelty: open-source, self-hosted, no commercial license required.

---

## Architecture Recap (from design doc)

```
Standalone local dev:
  rage daemon
    ├── unix socket  ← CLI, desired state
    └── HTTP/WS      ← browser status page

Hub mode (CI coordinator):
  rage daemon --hub --hub-port 9650
    ├── gRPC server  ← spokes register + receive WorkItems
    ├── task DAG     ← in-memory, dispatches when unblocked
    └── HTTP/WS      ← status page (cluster-wide view)

Spoke mode (CI worker):
  rage daemon --spoke
    ├── reads hub address from $RAGE_HUB_ADDR_FILE or $RAGE_HUB_ADDRESS
    ├── connects to hub gRPC
    ├── receives WorkItems → executes → uploads to cache → Complete()
    └── reconnects on disconnect (exponential backoff, max 30s)
```

Hub rendezvous (Docker Compose):
```
1. Hub starts → writes {addr, token, build_id} to /shared/rage-hub.json
2. Spokes poll /shared/rage-hub.json until present (max 30s timeout)
3. Spokes connect to hub:9650 via Docker DNS
```

---

## New Crates

```
crates/
├── hub/          ← HubServer gRPC implementation
│   ├── Cargo.toml
│   ├── build.rs  ← prost-build for proto
│   └── src/
│       ├── lib.rs    ← HubServer, HubDag, SpokeState
│       └── dag.rs    ← task graph state machine
└── spoke-client/ ← SpokeClient gRPC implementation
    ├── Cargo.toml
    ├── build.rs
    └── src/
        └── lib.rs    ← SpokeClient, run_as_spoke
```

---

## Workspace Cargo.toml Changes

Add to `[workspace] members`:
```toml
"crates/hub",
"crates/spoke-client",
```

Add to `[workspace.dependencies]`:
```toml
tonic = { version = "0.12", features = ["transport", "codegen"] }
tonic-build = "0.12"
prost = "0.13"
tokio = { version = "1", features = ["full"] }
```

---

## Docker Files

**`docker/Dockerfile.hub-spoke`** — Multi-stage Linux build:
```dockerfile
FROM rust:1.91-bookworm AS builder
WORKDIR /build
COPY . .
RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
RUN cargo build --release -p rage-cli 2>&1

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates bash && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/rage /usr/local/bin/rage
ENTRYPOINT ["/usr/local/bin/rage"]
```

**`docker/compose.hub-spoke.yaml`**:
```yaml
services:
  hub:
    build:
      context: ..
      dockerfile: docker/Dockerfile.hub-spoke
    command: ["daemon", "--hub", "--hub-port", "9650", "--hub-addr-file", "/shared/rage-hub.json", "--workspace", "/workspace"]
    volumes:
      - type: bind
        source: /Users/ken/workspace/lage
        target: /workspace
      - shared:/shared
      - hub-cache:/root/.rage
    ports:
      - "9650:9650"
    environment:
      - RAGE_HUB_TOKEN=test-token-abc123

  spoke1:
    build:
      context: ..
      dockerfile: docker/Dockerfile.hub-spoke
    command: ["daemon", "--spoke", "--hub-addr-file", "/shared/rage-hub.json", "--workspace", "/workspace"]
    volumes:
      - type: bind
        source: /Users/ken/workspace/lage
        target: /workspace
      - shared:/shared
      - spoke1-cache:/root/.rage
    environment:
      - RAGE_HUB_TOKEN=test-token-abc123
    depends_on:
      - hub

  spoke2:
    build:
      context: ..
      dockerfile: docker/Dockerfile.hub-spoke
    command: ["daemon", "--spoke", "--hub-addr-file", "/shared/rage-hub.json", "--workspace", "/workspace"]
    volumes:
      - type: bind
        source: /Users/ken/workspace/lage
        target: /workspace
      - shared:/shared
      - spoke2-cache:/root/.rage
    environment:
      - RAGE_HUB_TOKEN=test-token-abc123
    depends_on:
      - hub

volumes:
  shared:
  hub-cache:
  spoke1-cache:
  spoke2-cache:
```

---

## Tasks

---

### Task 1 — Proto compilation + workspace setup

**Files:**
- `Cargo.toml` (workspace) — add `crates/hub`, `crates/spoke-client` to members
- `Cargo.toml` — add `tonic`, `prost`, `tonic-build` to `[workspace.dependencies]`
- `crates/hub/Cargo.toml`
- `crates/hub/build.rs`
- `crates/hub/src/lib.rs` (stub)
- `crates/spoke-client/Cargo.toml`
- `crates/spoke-client/build.rs`
- `crates/spoke-client/src/lib.rs` (stub)

**`crates/hub/build.rs`:**
```rust
fn main() {
    tonic_build::compile_protos("../../proto/coordinator.proto")
        .expect("failed to compile proto");
}
```

**`crates/hub/Cargo.toml`:**
```toml
[package]
name = "hub"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
tonic = { workspace = true }
prost = { workspace = true }
tokio = { workspace = true }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dashmap = "6"
tracing = "0.1"

[build-dependencies]
tonic-build = "0.12"
```

**`crates/spoke-client/Cargo.toml`:**
```toml
[package]
name = "spoke-client"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
tonic = { workspace = true }
prost = { workspace = true }
tokio = { workspace = true }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"

[build-dependencies]
tonic-build = "0.12"
```

**Stubs** — `crates/hub/src/lib.rs`:
```rust
// Hub gRPC server implementation.
// Generated proto code included below.
tonic::include_proto!("rage.coordinator.v1");

pub mod server;
```

**Tests:** Verify compilation.
```rust
// In lib.rs test:
#[test]
fn proto_types_compile() {
    // If the proto code compiles, this test passes.
    let _: WorkerInfo = WorkerInfo::default();
    let _: WorkItem = WorkItem::default();
}
```

**Run:** `cargo build -p hub -p spoke-client` — must compile with zero errors.

**Commit:** `feat(hub,spoke-client): new crates with tonic/prost proto compilation`

---

### Task 2 — HubDag: in-memory task graph state machine

**File:** `crates/hub/src/dag.rs`

The HubDag tracks task states and computes which tasks are unblocked.

**Task states:**
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Pending,              // dependencies not yet met
    Ready,                // all dependencies complete, not yet dispatched
    Dispatched(String),   // sent to spoke (worker_id)
    Completed,            // finished successfully
    Failed(String),       // failed with error
}
```

**`HubDag` struct:**
```rust
pub struct HubDag {
    tasks: HashMap<String, TaskNode>,    // task_id → task definition
    states: HashMap<String, TaskState>,  // task_id → current state
    deps: HashMap<String, Vec<String>>,  // task_id → dependency task_ids
    rdeps: HashMap<String, Vec<String>>, // task_id → reverse deps (dependents)
}

impl HubDag {
    pub fn new(tasks: Vec<TaskNode>) -> Self { ... }
    /// Returns task_ids now in Ready state after marking task_id complete.
    pub fn mark_complete(&mut self, task_id: &str) -> Vec<String> { ... }
    /// Returns task_ids now in Ready state after marking task_id failed.
    pub fn mark_failed(&mut self, task_id: &str, error: &str) -> Vec<String> { ... }
    /// Take one Ready task and mark it as Dispatched(worker_id).
    pub fn dispatch_next(&mut self, worker_id: &str) -> Option<&TaskNode> { ... }
    /// Is the entire build done?
    pub fn is_complete(&self) -> bool { ... }
    /// Did any task fail?
    pub fn has_failure(&self) -> Option<(&str, &str)> { ... }
    /// Count of tasks in each state.
    pub fn stats(&self) -> DagStats { ... }
}
```

**Tests:**
```rust
#[test]
fn dag_dispatches_leaves_first() {
    // a → b → c (a must run before b, b before c)
    let tasks = vec![
        task("a", vec![]),
        task("b", vec!["a"]),
        task("c", vec!["b"]),
    ];
    let mut dag = HubDag::new(tasks);
    // Only "a" should be ready initially
    let t = dag.dispatch_next("worker1").unwrap();
    assert_eq!(t.task_id, "a");
    assert!(dag.dispatch_next("worker1").is_none()); // b still waiting
    
    let newly_ready = dag.mark_complete("a");
    assert_eq!(newly_ready, vec!["b"]);
    
    let t2 = dag.dispatch_next("worker2").unwrap();
    assert_eq!(t2.task_id, "b");
}

#[test]
fn dag_parallel_dispatch() {
    // a and b are independent, both should be Ready
    let tasks = vec![task("a", vec![]), task("b", vec![])];
    let mut dag = HubDag::new(tasks);
    let t1 = dag.dispatch_next("w1").unwrap();
    let t2 = dag.dispatch_next("w2").unwrap();
    let ids: HashSet<_> = [t1.task_id.as_str(), t2.task_id.as_str()].into();
    assert!(ids.contains("a") && ids.contains("b"));
}

#[test]
fn dag_complete_signals_all_done() {
    let tasks = vec![task("only", vec![])];
    let mut dag = HubDag::new(tasks);
    dag.dispatch_next("w1");
    dag.mark_complete("only");
    assert!(dag.is_complete());
}
```

**Commit:** `feat(hub): HubDag in-memory task graph state machine`

---

### Task 3 — HubServer gRPC implementation

**File:** `crates/hub/src/server.rs`

**`HubServer` struct:**
```rust
pub struct HubServer {
    dag: Arc<Mutex<HubDag>>,
    token: String,
    notify: Arc<Notify>,  // woken when new tasks become Ready
    build_events: Arc<broadcast::Sender<BuildEvent>>,
}
```

**`Subscribe` implementation:**
```rust
async fn subscribe(
    &self,
    request: Request<WorkerInfo>,
) -> Result<Response<Self::SubscribeStream>, Status> {
    let info = request.into_inner();
    if info.token != self.token {
        return Err(Status::unauthenticated("invalid token"));
    }
    
    let dag = Arc::clone(&self.dag);
    let notify = Arc::clone(&self.notify);
    let stream = async_stream::stream! {
        loop {
            // Wait until a task is ready
            notify.notified().await;
            
            let work = {
                let mut dag = dag.lock().await;
                dag.dispatch_next(&info.worker_id).map(|task| {
                    WorkItem {
                        task_id: task.task_id.clone(),
                        package_name: task.package_name.clone(),
                        script_name: task.script_name.clone(),
                        command: task.command.clone(),
                        // ... other fields
                    }
                })
            };
            
            if let Some(item) = work {
                yield Ok(item);
            }
            
            // Check if build is complete
            let done = dag.lock().await.is_complete();
            if done { break; }
        }
    };
    
    Ok(Response::new(Box::pin(stream)))
}
```

**`Complete` implementation:**
```rust
async fn complete(
    &self,
    request: Request<CompletionReport>,
) -> Result<Response<Ack>, Status> {
    let report = request.into_inner();
    let mut dag = self.dag.lock().await;
    if report.success {
        dag.mark_complete(&report.task_id);
    } else {
        dag.mark_failed(&report.task_id, &report.stderr_tail);
    }
    // Wake all waiting spokes — some may now have work
    self.notify.notify_waiters();
    Ok(Response::new(Ack { accepted: true, reason: String::new() }))
}
```

**Tests:** Use `tonic::transport::Server::builder` to start an in-process server.
```rust
#[tokio::test]
async fn hub_dispatches_tasks_to_subscribed_spokes() {
    let tasks = vec![
        TaskNode { task_id: "a".into(), command: "echo a".into(), depends_on: vec![], .. },
        TaskNode { task_id: "b".into(), command: "echo b".into(), depends_on: vec!["a".into()], .. },
    ];
    
    let hub = HubServer::new(tasks, "test-token");
    let addr = "127.0.0.1:0".parse().unwrap();
    let server_handle = hub.start_test_server(addr).await;
    
    let mut client = CoordinatorClient::connect(server_handle.addr()).await.unwrap();
    let stream = client.subscribe(WorkerInfo { token: "test-token".into(), worker_id: "w1".into(), .. }).await.unwrap().into_inner();
    
    // Should get task "a" immediately (no dependencies)
    // ... verify stream yields task "a"
}
```

**Commit:** `feat(hub): HubServer gRPC service — Subscribe, Complete, Ping`

---

### Task 4 — SpokeClient: connect, receive work, execute, report

**File:** `crates/spoke-client/src/lib.rs`

**`SpokeClient` struct:**
```rust
pub struct SpokeClient {
    hub_address: String,
    token: String,
    worker_id: String,
    workspace_root: PathBuf,
    parallelism: u32,
}
```

**`run_as_spoke` implementation:**
```rust
pub async fn run_as_spoke(client: SpokeClient) -> anyhow::Result<()> {
    let mut backoff = Duration::from_millis(500);
    
    loop {
        match client.connect_and_work().await {
            Ok(()) => {
                // Build completed normally
                return Ok(());
            }
            Err(e) => {
                eprintln!("[rage-spoke] disconnected: {e} — reconnecting in {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(30));
            }
        }
    }
}

async fn connect_and_work(&self) -> anyhow::Result<()> {
    let channel = tonic::transport::Channel::from_shared(self.hub_address.clone())?
        .tcp_keepalive(Some(Duration::from_secs(20)))
        .connect()
        .await?;
    
    let mut grpc_client = CoordinatorClient::new(channel);
    
    let stream = grpc_client.subscribe(WorkerInfo {
        worker_id: self.worker_id.clone(),
        token: self.token.clone(),
        parallelism: self.parallelism,
        platform: std::env::consts::OS.to_string(),
    }).await?.into_inner();
    
    let mut stream = stream;
    while let Some(item) = stream.message().await? {
        let task_id = item.task_id.clone();
        let result = self.execute_work_item(item).await;
        
        let report = match result {
            Ok(output_hash) => CompletionReport {
                task_id: task_id.clone(),
                worker_id: self.worker_id.clone(),
                success: true,
                exit_code: 0,
                output_sf_hash: output_hash,
                ..Default::default()
            },
            Err(e) => CompletionReport {
                task_id,
                worker_id: self.worker_id.clone(),
                success: false,
                stderr_tail: e.to_string(),
                ..Default::default()
            },
        };
        
        grpc_client.complete(report).await?;
    }
    
    Ok(())
}

async fn execute_work_item(&self, item: WorkItem) -> anyhow::Result<String> {
    let pkg_dir = self.workspace_root.join(&item.package_path);
    
    eprintln!("[rage-spoke] running {}#{}", item.package_name, item.script_name);
    
    let status = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&item.command)
        .current_dir(&pkg_dir)
        .status()
        .await?;
    
    if status.success() {
        Ok(String::new())  // No remote cache in v1 — empty hash
    } else {
        Err(anyhow::anyhow!("task failed with exit code {:?}", status.code()))
    }
}
```

**Tests:**
```rust
#[tokio::test]
async fn spoke_executes_received_task() {
    // Start a mock hub that sends one task then closes
    // Verify spoke executes `echo hello` and calls Complete(success=true)
}
```

**Commit:** `feat(spoke-client): SpokeClient — connect, subscribe, execute, report Complete`

---

### Task 5 — Hub address rendezvous file

**File:** `crates/hub/src/rendezvous.rs`

The hub writes its address to a file when it starts. Spokes read this file.

```rust
#[derive(serde::Serialize, serde::Deserialize)]
pub struct HubAddr {
    pub addr: String,         // e.g. "hub:9650" or "127.0.0.1:9650"
    pub token: String,
    pub build_id: String,
}

pub fn write_hub_addr(file: &Path, addr: &HubAddr) -> anyhow::Result<()> {
    let tmp = file.with_extension("tmp");
    let json = serde_json::to_vec_pretty(addr)?;
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, file)?;
    Ok(())
}

pub async fn read_hub_addr_with_timeout(file: &Path, timeout_secs: u32) -> anyhow::Result<HubAddr> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs as u64);
    loop {
        if let Ok(content) = std::fs::read_to_string(file) {
            if let Ok(addr) = serde_json::from_str::<HubAddr>(&content) {
                return Ok(addr);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!("timed out waiting for hub addr file: {}", file.display()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
```

**Tests:**
```rust
#[tokio::test]
async fn rendezvous_write_then_read() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let addr = HubAddr { addr: "hub:9650".into(), token: "tok".into(), build_id: "b1".into() };
    write_hub_addr(tmp.path(), &addr).unwrap();
    let read = read_hub_addr_with_timeout(tmp.path(), 1).await.unwrap();
    assert_eq!(read.addr, "hub:9650");
}
```

**Commit:** `feat(hub): rendezvous file — write_hub_addr + read_hub_addr_with_timeout`

---

### Task 6 — CLI integration

**File:** `crates/cli/src/main.rs`

Add new subcommand flags to the `rage daemon` command:

```rust
// In the Commands enum, add to existing Daemon subcommand:

#[derive(clap::Args)]
struct DaemonArgs {
    /// Run in hub mode (CI coordinator).
    #[arg(long)]
    hub: bool,
    
    /// gRPC port for hub (default 9650).
    #[arg(long, default_value = "9650")]
    hub_port: u16,
    
    /// File to write hub address to (for spoke discovery).
    #[arg(long)]
    hub_addr_file: Option<PathBuf>,
    
    /// Run in spoke mode (worker connecting to hub).
    #[arg(long)]
    spoke: bool,
    
    /// Workspace root for this node.
    #[arg(long)]
    workspace: Option<PathBuf>,
}
```

Add `rage ci` subcommand:
```rust
// rage ci --workspace ~/workspace/lage --script build
// Starts the hub, submits the build graph, waits for completion
#[derive(clap::Args)]
struct CiArgs {
    /// Workspace to build.
    workspace: PathBuf,
    /// Script to run (e.g. "build").
    #[arg(long, default_value = "build")]
    script: String,
    /// Hub address file (written by rage daemon --hub).
    #[arg(long)]
    hub_addr_file: Option<PathBuf>,
}
```

**Implementation sketch:**
```rust
Commands::Daemon(args) if args.hub => {
    // Start hub gRPC server
    let tasks = build_task_list_for_workspace(&args.workspace.unwrap_or_default(), &args.script)?;
    let hub = HubServer::new(tasks, std::env::var("RAGE_HUB_TOKEN").unwrap_or_default());
    if let Some(file) = args.hub_addr_file {
        hub.write_addr_file(&file)?;
    }
    hub.serve(args.hub_port).await?;
}
Commands::Daemon(args) if args.spoke => {
    let addr_file = args.hub_addr_file.or_else(|| std::env::var("RAGE_HUB_ADDR_FILE").ok().map(PathBuf::from));
    let hub_addr = match addr_file {
        Some(f) => read_hub_addr_with_timeout(&f, 30).await?,
        None => HubAddr { addr: std::env::var("RAGE_HUB_ADDRESS")?, .. },
    };
    let client = SpokeClient::new(hub_addr, args.workspace.unwrap_or_default());
    run_as_spoke(client).await?;
}
```

**Tests:** CLI argument parsing.

**Commit:** `feat(cli): daemon --hub/--spoke flags + rage ci subcommand`

---

### Task 7 — Docker Compose integration test

**Files created:**
- `docker/Dockerfile.hub-spoke` (multi-stage Linux build)
- `docker/compose.hub-spoke.yaml`
- `scripts/test-distributed.sh`

**`scripts/test-distributed.sh`:**
```bash
#!/usr/bin/env bash
set -euo pipefail

echo "=== rage distributed build test ==="

WORKSPACE_PATH="/Users/ken/workspace/lage"
TOKEN="test-token-abc123"
COMPOSE_FILE="docker/compose.hub-spoke.yaml"

echo "Building Docker image..."
docker compose -f "$COMPOSE_FILE" build --quiet

echo "Starting hub + 2 spokes..."
docker compose -f "$COMPOSE_FILE" up -d

echo "Waiting for hub to be ready (30s max)..."
for i in $(seq 1 30); do
    if docker compose -f "$COMPOSE_FILE" exec -T hub rage daemon --ping 2>/dev/null; then
        echo "Hub ready after ${i}s"
        break
    fi
    sleep 1
done

echo "Submitting build to hub..."
docker compose -f "$COMPOSE_FILE" exec -T hub \
    rage ci --workspace /workspace --script build \
    --hub-addr-file /shared/rage-hub.json 2>&1 &
BUILD_PID=$!

echo "Waiting for build to complete (120s max)..."
wait $BUILD_PID || true

echo "=== Distributed build results ==="
echo "Hub logs:"
docker compose -f "$COMPOSE_FILE" logs --no-color hub 2>&1 | grep "\[rage\]" | tail -20

echo "Spoke1 logs:"
docker compose -f "$COMPOSE_FILE" logs --no-color spoke1 2>&1 | grep "\[rage-spoke\]" | tail -10

echo "Spoke2 logs:"
docker compose -f "$COMPOSE_FILE" logs --no-color spoke2 2>&1 | grep "\[rage-spoke\]" | tail -10

echo "Verifying tasks were distributed across both spokes..."
SPOKE1_TASKS=$(docker compose -f "$COMPOSE_FILE" logs --no-color spoke1 2>&1 | grep -c "running.*#" || true)
SPOKE2_TASKS=$(docker compose -f "$COMPOSE_FILE" logs --no-color spoke2 2>&1 | grep -c "running.*#" || true)

echo "Spoke1 ran $SPOKE1_TASKS tasks"
echo "Spoke2 ran $SPOKE2_TASKS tasks"

if [ "$SPOKE1_TASKS" -gt 0 ] && [ "$SPOKE2_TASKS" -gt 0 ]; then
    echo "✅ PASS: Tasks distributed across both spokes"
else
    echo "❌ FAIL: Tasks not distributed (spoke1=$SPOKE1_TASKS, spoke2=$SPOKE2_TASKS)"
    docker compose -f "$COMPOSE_FILE" down
    exit 1
fi

echo "Tearing down..."
docker compose -f "$COMPOSE_FILE" down

echo "=== Distributed build test PASSED ==="
```

**Run the test:**
```bash
chmod +x scripts/test-distributed.sh
./scripts/test-distributed.sh 2>&1
```

**Expected output includes:**
```
[rage] workspace#build submitted (25 tasks)
[rage-spoke] worker1 running @lage-run/core#build
[rage-spoke] worker2 running @lage-run/utils#build
...
✅ PASS: Tasks distributed across both spokes
```

**Commit:** `feat(docker): hub/spoke compose setup + distributed test script`

---

## Verification

After all tasks complete:

```bash
# Tests
cargo test --workspace 2>&1 | tail -5
# Expected: test result: ok. N passed; 0 failed

# Docker test
./scripts/test-distributed.sh

# Manual hub/spoke test (local)
./target/release/rage daemon --hub --hub-port 9651 --hub-addr-file /tmp/rage-hub.json &
./target/release/rage daemon --spoke --hub-addr-file /tmp/rage-hub.json --workspace ~/workspace/lage &
```

**Final push:** `git push origin main`
