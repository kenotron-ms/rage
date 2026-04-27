# Distributed Builds — Hub & Spoke

rage's distributed build system is the same Rust binary running in three modes: local daemon, hub, spoke. There is no separate coordinator service, no proprietary product, no closed-source orchestrator. The protocol is gRPC; the rendezvous is a JSON file in shared storage; the artifacts move through the same content-addressed store the local cache uses.

```
                         ┌─────────────────────────────────┐
                         │      shared remote CAS          │
                         │   (S3 / Azure Blob / NFS)       │
                         └────────────▲────────────────────┘
                                      │ artifacts
                                      │ (uploads, downloads)
            ┌─────────────────────────┴────────────────────────┐
            │                                                  │
            │              gRPC Subscribe / Complete           │
            │                                                  │
   ┌────────▼─────────┐                                ┌───────▼────────┐
   │   rage hub       │                                │   rage spoke   │
   │   (CI main job)  │   pushes WorkItem stream  ───► │  (CI matrix)   │
   │                  │   ◄── receives Complete ───    │                │
   │   in-memory DAG  │                                │  scheduler +   │
   │                  │                                │  sandbox +     │
   │                  │                                │  cache         │
   └──────────────────┘                                └────────────────┘
            ▲
            │ writes rage-hub.json
            │
   ┌────────┴─────────┐
   │  rendezvous file │
   │   (shared vol /  │
   │   S3 object)     │
   └──────────────────┘
```

## Roles

`rage` is the binary in all three roles:

```
no hub config              → standalone (default; local dev)
RAGE_HUB_ADDRESS env var   → spoke
rage hub --listen X:Y      → hub
```

The local daemon is the same code path as a spoke. A spoke running on a CI agent is just a daemon whose `SetDesiredState` source is a remote gRPC stream rather than a local Unix socket. The hub is the same code path as the daemon's reconciliation loop, with the work source being the gRPC `Subscribe` stream rather than a CLI invocation.

## Protocol — gRPC

`proto/coordinator.proto`:

```protobuf
service Coordinator {
  // Spoke subscribes; hub streams WorkItems as the DAG unblocks.
  rpc Subscribe(WorkerInfo) returns (stream WorkItem);

  // Spoke reports a task's outcome.
  rpc Complete(CompletionReport) returns (Ack);
}

message WorkerInfo {
  string spoke_id = 1;
  string platform = 2;            // e.g. "linux-x86_64"
  string node_version = 3;        // for postinstall ABI cohort
  uint32 parallelism = 4;         // how many tasks the spoke can run concurrently
  string rage_version = 5;
}

message WorkItem {
  string task_id = 1;             // "{package}#{script}"
  string package = 2;
  string script = 3;
  string command = 4;
  repeated string upstream_outputs_to_fetch = 5;  // CAS keys
  bytes weak_fingerprint = 6;
  bytes strong_fingerprint_hint = 7;
  string sandbox_mode = 8;        // observed | strict | loose
  uint64 deadline_ms = 9;
}

message CompletionReport {
  string task_id = 1;
  oneof outcome {
    Success success = 2;
    Failure failure = 3;
  }
  bytes pathset_blake3 = 4;       // CAS key under which the pathset was stored
  bytes outputs_blake3 = 5;       // CAS key under which the outputs were stored
  uint64 wall_ms = 6;
}
```

Two RPCs. No leader election, no consensus, no heartbeating beyond gRPC's keepalive. The hub is a single process; if it dies, the build dies (see "Crash recovery" below).

### Why gRPC

- **Streaming.** `Subscribe` is server-streaming. Spokes hold one long-lived connection and receive work as the DAG produces it. No polling, no long-polling, no message broker.
- **Binary-efficient.** Task graphs of 10,000+ vertices serialize compactly in protobuf. A WebSocket + JSON would be 3-5× larger on the wire.
- **Tooling.** tonic + prost are mature; both sides of the wire are generated from the `.proto`. Same transport choice as BuildXL.
- **Authentication.** Standard bearer-token interceptor; mTLS available for environments that need it.

WebSockets remain in use for the local daemon's status page (browser observers). The hub does not speak WebSocket to spokes.

## Rendezvous — file-based

The hub does not need a registry service. At startup it writes its address to a path both sides can read:

```json
// rage-hub.json
{
  "address": "10.0.1.42:9650",
  "token": "redacted",
  "build_id": "ci-20260427-1245",
  "rage_version": "0.1.0"
}
```

The path is configurable. Three deployment patterns:

| Environment | Rendezvous path |
|---|---|
| Single CI runner with self-hosted matrix | Shared volume mount, e.g. `/shared/rage-hub.json` |
| Docker Compose | A volume both `hub` and `spokes` mount — `./rage-hub.json` |
| Cloud CI (BuildKite, Azure Pipelines) | An S3 / Azure Blob object the build job has write access to |

This is enough. There is no service discovery layer to operate, no Consul, no etcd, no DNS round-trip. The hub writes a 200-byte JSON file; the spokes read it; they connect.

## Hub internals — `crates/hub`

The hub holds the task DAG in memory:

```rust
pub struct HubState {
    dag: TaskDag,                                // build-graph output
    waiting: HashMap<TaskId, WaitingTask>,       // unblocked, not yet assigned
    in_flight: HashMap<TaskId, AssignedTask>,    // sent to a spoke
    completed: HashMap<TaskId, CompletionReport>,
    spokes: HashMap<SpokeId, SpokeChannel>,
}
```

There is no SQLite, no on-disk state, no persistent queue. When the hub starts:

1. Construct the build graph from the workspace (same code as the local daemon).
2. Scope to affected tasks (same code).
3. Compute the wave of root-ready tasks (no unsatisfied dependencies).
4. Open the gRPC server, write `rage-hub.json`.
5. Wait for spokes to `Subscribe`. As each does, push waiting tasks until the spoke's parallelism is saturated.
6. On each `Complete(report)`:
   - Check the report's success / failure.
   - On success: record outputs CAS key; walk the DAG to find tasks that just became unblocked; push them to the spoke pool round-robin.
   - On failure: mark the task failed; fail every transitive dependent; the build enters `blocked` state.
7. Idle when all tasks are complete or the DAG is fully blocked. Emit final report. Exit with non-zero on any failure.

The task scheduling policy is configurable but defaults to:

- Round-robin across spokes, biased toward whichever spoke has cached upstream outputs in its local CAS (warm-cache affinity).
- Tasks tagged with platform constraints (e.g. a postinstall pinned to `linux-aarch64`) only go to spokes matching that platform.

## Spoke internals — `crates/spoke-client`

The spoke is a daemon that:

1. Reads `rage-hub.json`.
2. Opens a gRPC channel; sends `Subscribe(WorkerInfo)` with its platform, parallelism, version.
3. Receives `WorkItem`s on the server-stream. Each item is queued into a bounded executor (size = `parallelism`).
4. For each work item:
   - Fetch upstream outputs from the shared CAS (S3 / Azure / NFS) directly. Hub never sees the bytes.
   - Compute the WF and SF locally — the spoke has its own `cache` and `sandbox` crates and may already have the SF in its local CAS (cache hit on a spoke that built this before).
   - On hit: replay outputs from local CAS, send `Complete{Success{...}}`.
   - On miss: run the task with the sandbox attached, capture pathset and outputs, upload them to the shared CAS, send `Complete{Success{outputs_blake3, pathset_blake3, ...}}`.
5. On task failure, the spoke sends `Complete{Failure{...}}` with stderr captured. The hub does not retry by default; configurable retry-on-flake is on the roadmap.

The spoke's local cache is a perf optimization: if the same spoke has built `packages/core#build` before with the same SF, a cache hit avoids re-downloading from the shared CAS. On warm spokes this is the common case.

## Artifact routing — through the CAS, not the hub

A common distributed-build mistake is to pipe artifacts through the coordinator. Nx Cloud does this; outputs flow client → coordinator → other clients. This makes the coordinator a data-plane bottleneck, a network egress cost, and a commercial licensing chokepoint.

rage routes artifacts directly through the shared CAS:

```
Spoke A finishes packages/core#build:
   1. Spoke A uploads outputs to s3://rage-cache/sf/{strong_fp_hex}/...  (parallel PutObject per file)
   2. Spoke A sends Complete{outputs_blake3} to hub  (just the key, ~32 bytes)
   3. Hub records "core#build done at key K"
   4. Hub assigns packages/api#build to Spoke B with upstream_outputs_to_fetch=[K]
   5. Spoke B fetches from S3 directly  (parallel GetObject)
```

The hub passes 32-byte CAS keys around. The bytes never traverse the hub. The hub's bandwidth is bounded by O(tasks × few-hundred-bytes), regardless of artifact sizes.

This is the BuildXL prep-pip + content-store model applied to the entire build graph.

## Crash recovery — correctness over speed

If the hub crashes, the in-memory DAG is gone. There is no on-disk state to resume from. The build is restarted from scratch.

This is a deliberate design choice. Resumption from a crashed hub would require trusting:

- That every spoke's idea of "this task succeeded" is consistent with the hub's idea.
- That outputs uploaded to the CAS are atomic and complete (no half-written objects).
- That sandbox state on each spoke survives a hub crash without contamination.

The cost of getting any of these wrong is a wrong build that succeeds. The cost of restarting from scratch is one CI run. We choose the second.

What does survive a hub crash:

- The shared CAS. Every output already uploaded is still there.
- Spoke local CAS. Every task already cached on a spoke is still warm.

The next hub run will see cache hits for everything that completed before the crash. Restart is not free, but it is not paying twice for tasks that already succeeded.

Spoke crashes are different: the hub re-queues any in-flight tasks assigned to that spoke. Other spokes pick them up. No build-level restart.

## Authentication

Bearer token. The token is generated by the hub at startup, written into `rage-hub.json`, read by spokes. Standard practice:

```
$ rage hub --listen 0.0.0.0:9650
[rage] hub started; address written to s3://my-bucket/builds/ci-X/rage-hub.json
[rage] auth: bearer token (32 bytes), valid for this hub session only
```

mTLS is supported for environments that prefer it. The protocol is unchanged; the token check becomes a certificate check.

## Spoke provisioning

v1 is **manual DTE**: the user declares N spokes in their CI YAML matrix. The hub schedules across whatever subscribes.

```yaml
# GitHub Actions example
jobs:
  hub:
    runs-on: self-hosted
    steps:
      - run: rage hub --listen 0.0.0.0:9650 --rendezvous s3://my-bucket/builds/$BUILD_ID/rage-hub.json
      - run: rage wait                                # blocks until convergence

  spokes:
    needs: []
    strategy:
      matrix:
        spoke: [1, 2, 3, 4, 5, 6, 7, 8]
    runs-on: self-hosted
    steps:
      - run: rage spoke --rendezvous s3://my-bucket/builds/$BUILD_ID/rage-hub.json
```

The hub and spokes start in parallel; spokes find the hub via the rendezvous file as it appears.

v2 (roadmap) is **auto-provisioning**: rage talks to CI provider APIs (GitHub Actions runners, Azure Pipelines pools) to scale the spoke pool dynamically based on DAG depth. This is what Nx Agents does; the difference is that rage's auto-provisioner will be a config you write, not a SaaS you buy.

## Network topology constraints

Spokes need to reach two things:

1. **The hub address** (typically a private IP on a self-hosted runner network, or a service mesh address in Kubernetes). Public-cloud-hosted runners (GitHub-hosted) cannot reach a private hub directly.
2. **The shared CAS** (S3, Azure Blob, NFS).

For self-hosted agent pools, both are trivial: same VPC, same network. For mixed environments (GitHub-hosted runners as spokes against a self-hosted hub), an inbound tunnel or a relay is required. A future `rage cloud relay` (lightweight TCP forwarder) will handle the public-runner case. v1 is same-network only.

## Comparison to local daemon

Most of the daemon and spoke code is shared. The differences are at the seams:

| Concern | Local daemon | Spoke |
|---|---|---|
| Work source | `SetDesiredState` over Unix socket | `WorkItem` stream over gRPC |
| Cache backend | local CAS at `~/.rage/` | local CAS + remote CAS (S3/Azure) |
| Reporting | WebSocket events to status page | `CompletionReport` over gRPC |
| File watcher | yes (notify) | no |
| Status page | yes | no (hub has the cluster view) |

The hub's status page is the daemon's status page with cluster awareness — task rows are labeled with the spoke that ran them, and the URL of each spoke's local status page is linked.

## What the hub does not do

- **No data-plane involvement.** Bytes go spoke ↔ CAS, never via the hub.
- **No persistent state.** Every hub run is a fresh in-memory DAG.
- **No retry policy beyond reschedule.** Failed tasks fail the build; flaky-task retry is opt-in (and usually wrong — it hides bugs).
- **No cross-build coordination.** Two hub runs are independent. Cache sharing happens at the CAS layer; the hub doesn't need to know.
- **No commercial license layer.** The hub binary is the same license as the rest of rage. There is no "Pro" tier.
