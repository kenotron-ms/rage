# Rage — Daemon, Config, Observability & Cache Key Design

## Goal

Design the coordination and developer experience layer of rage: a Rust-based multi-ecosystem build system. This document covers the daemon architecture, desired state model, observability UX, configuration architecture, hub/spoke distributed builds, and cache key design.

The build engine core (`build-graph`, `scheduler`, `sandbox`, `scoping` crates) is covered in a separate existing spec (`BuildXL and Lage - Rust Reimplementation Spec.md`). This document captures design **beyond** that spec.

## Background

Rage unifies ideas from lage (declarative task graphs over package graphs), BuildXL (observable correctness via sandboxed file access, two-phase fingerprinting, distributed builds), and Nx (ergonomic daemon UX, plugin-based ecosystem inference). The existing spec addresses the mechanical core — graph construction, scheduling, caching, sandbox primitives. This document addresses the layers above: how a developer interacts with the system, how it configures itself at scale across thousands of packages and multiple ecosystems, how it coordinates from a single workstation to a distributed build cluster, and how its cache keys are structured to scale from JS monorepos to Windows-at-Microsoft.

## Approach

Three architectural bets drive the design:

1. **The daemon is the coordination primitive at every scale.** The same binary runs standalone (local dev), as a spoke in a distributed build, or as a hub coordinating a build cluster. Nx keeps local daemon and distributed execution as unrelated systems. Rage unifies them.
2. **Desired state, not imperative commands.** The developer declares an outcome ("I want affected packages typechecked and built"). The daemon converges toward it continuously — a Kubernetes-style reconciliation loop, made accurate by sandbox-observed file access data.
3. **Plugins, not users, bear the declaration burden.** Ecosystem plugins declare what files a TypeScript, Rust, Go, Python, or C++ task reads. The user only overrides at workspace or package scope when the plugin's defaults don't fit. This is what scales configuration across thousands of packages.

## Architecture

### Three Layers

**Build engine** — composable Rust crates: `build-graph`, `scheduler`, `cache` (two-phase fingerprinting), `sandbox` (file-access monitoring), `workspace-tools`, `scoping`, `telemetry`. Ecosystem-agnostic mechanical core. Defined in the existing spec.

**Daemon** — one per workspace, long-lived. Holds desired state for the session and runs a reconciliation loop to converge toward it. Two interfaces:
- **Unix socket** — CLI ↔ daemon IPC (fast, local-only, same pattern as Nx)
- **HTTP/WebSocket server** — browser, VS Code extension, any observer/controller

Three-hour idle shutdown. Self-healing version-mismatch restart.

**Hub mode** — the same daemon binary started with `--hub`. Accepts spoke registrations from remote daemons, schedules tasks across the spoke pool, coordinates the remote cache. Self-hosted, open source. gRPC between hub and spokes (binary-efficient, streaming, same transport choice as BuildXL). WebSocket stays for browser-only.

### Differentiators

| Concern | Nx | BuildXL | Rage |
|---|---|---|---|
| Local daemon | Yes (silent) | No | Yes (ambient status) |
| Distributed builds | Proprietary orchestrator | Open, manual worker provisioning | Open, same binary as daemon |
| Sandbox as first-class primitive | No | Yes | Yes |
| Cross-ecosystem plugin API | JS-centric | DScript (bespoke) | Rust trait, multi-ecosystem |
| Self-hosted distributed | Commercial license required | Yes, manual | Yes, default |

---

## Section 1 — Daemon: Desired State & Reconciliation Loop

The daemon accepts a **desired state** per session — a declared outcome, not an imperative invocation.

### Three-State Model

```
🔄 converging  — working toward desired state, tasks running
✅ ready        — desired state reached, all relevant tasks clean
❌ blocked      — a task failed, cannot converge without intervention
```

### Flow

1. Developer runs `rage dev` (or `rage dev --target typecheck,build`).
2. CLI connects to existing daemon via Unix socket — or starts one (detached child process with `unref()`, same pattern as Nx).
3. CLI sends a `SetDesiredState` message: which packages, which task types, scope.
4. CLI exits immediately, prints `→ http://localhost:<dynamic-port>` and returns control.
5. Daemon enters the reconciliation loop:
   - Scopes to affected packages (git diff or explicit).
   - Checks cache: which tasks have clean fingerprints?
   - Runs stale tasks, scheduling via the DAG.
   - As tasks complete, re-evaluates what's now unblocked downstream.
   - On file change (via the `notify` crate watcher): re-evaluates **exactly** which tasks are invalidated using observed sandbox read-sets — not glob patterns.

The sandbox read-set data is the key differentiator from traditional watch mode. When `src/utils.ts` changes, the daemon has the precise file access log from prior task runs. Only tasks that previously read that file are invalidated.

When a new `SetDesiredState` arrives (developer runs `rage dev` again in a new context), it replaces the prior session's state and the loop adjusts immediately.

### Daemon Discovery

Daemon writes a JSON file on startup (`rage-daemon.json`) to a workspace-scoped temp directory:

```json
{
  "pid": 12345,
  "unixSocket": "/tmp/rage/<workspace-hash>/daemon.sock",
  "httpPort": 7853,
  "startTime": "2026-04-24T12:00:00Z",
  "version": "0.1.0"
}
```

Clients read this file to find the live daemon. Same pattern as Nx's `server-process.json`.

### Lifecycle

- Three-hour idle timeout (resets on every request).
- Self-restarts on version mismatch (spawns new daemon before dying).
- CI environments: daemon disabled by default — short-lived commands don't benefit from daemon warm-up.

---

## Section 2 — Observability UX: HTTP/WS Status Page

The daemon runs an embedded HTTP server on a dynamic port (written to the discovery file). The CLI prints the URL on exit. `rage open` launches the browser directly.

### Endpoints

```
GET  /          → status page (HTML, vanilla JS, no framework)
GET  /api/state → JSON snapshot of current state
WS   /ws        → bidirectional real-time stream
```

### Status Page

One screen, three zones. Intentionally minimal.

```
┌─────────────────────────────────────────────────┐
│  rage  [workspace: /path/to/workspace]          │
│                                                 │
│  🔄 converging  packages/core#typecheck         │
│  ████████░░░░  6 / 9 tasks complete             │
│                                                 │
│  ✅ packages/utils#build       0.8s             │
│  ✅ packages/config#typecheck  1.2s             │
│  🔄 packages/core#typecheck    running…         │
│  ⏳ packages/app#build         waiting          │
└─────────────────────────────────────────────────┘
```

When a task fails, the page expands inline — no navigation, no modal:

```
│  ❌ packages/core#typecheck  BLOCKED            │
│  ─────────────────────────────────────────────  │
│  src/parser.ts:42 — Type 'string' not           │
│  assignable to type 'number'                    │
│                               [copy] [retry]    │
```

The `[retry]` button sends a `RetryTask` message over the WebSocket back to the daemon. This is why WS bidirectionality matters: the page is an observer **and** a controller.

### Implementation

Vanilla JS, no framework, no build step. One job: reflect three states, display task list, push WS events to DOM. Target ~200-300 lines of JS.

### Pluggability

VS Code extension and any other tooling connects to the same `/ws` endpoint and receives identical events. The daemon has one source of truth; consumers subscribe to it.

### Hub Cluster View

When the daemon runs in hub mode, the same status page shows cluster-wide state — tasks labeled with which spoke is running them. No separate UI.

---

## Section 3 — Config Architecture: `rage.json` & Ecosystem Plugins

`rage.json` lives at the workspace root. JSONC format (JSON with comments, like `tsconfig.json`). A `$schema` field provides editor completions and validation for free.

```jsonc
{
  "$schema": "https://rage.build/schema/rage.json",
  "plugins": ["rage-typescript", "rage-rust", "./internal/rage-dqps"],
  "cache": { "backend": "azure", "container": "build-cache" },
  "sandbox": { "default": "observed" },

  // Glob policies — apply to package sets without touching individual packages
  "policies": [
    { "selector": "packages/core/**",   "sandbox": "strict" },
    { "selector": "packages/legacy/**", "sandbox": "loose"  }
  ],

  // Plugin-level config override
  "plugins_config": {
    "rage-typescript": {
      "input_globs": {
        "extend":  ["../../tsconfig.base.json"],
        "exclude": ["**/*.test.ts"]
      }
    }
  }
}
```

### Three Config Tiers

Nothing touches individual packages by default.

| Tier | Location | Purpose |
|---|---|---|
| **Workspace** | `rage.json` | Global invariants, plugin registration, sandbox default, cache backend |
| **Glob policies** | `rage.json` `policies` array | Apply different rules to package sets without per-package config. CSS-for-packages. |
| **Per-package override** | Embedded in existing manifest | Only when a package genuinely diverges |

Per-package overrides live in the file the package author already owns — no new file type:

```toml
# Cargo.toml
[package.metadata.rage]
sandbox = "strict"
extra_input_globs = ["../../shared-config/"]
```

```json
// package.json
{ "rage": { "sandbox": "strict" } }
```

```toml
# pyproject.toml
[tool.rage]
sandbox = "strict"
```

Most packages never touch this.

### Ecosystem Plugin Contract

```rust
pub trait EcosystemPlugin {
    // Detection: which files signal a package lives here
    fn detection_globs(&self) -> Vec<&str>;

    // Inference: given a detected package root + manifest, return task definitions
    fn infer_tasks(&self, root: &Path, manifest: &Manifest) -> Vec<TaskDef>;

    // Toolchain allowlist: files the compiler always reads (OS libs, toolchain paths).
    // Declared once at plugin level, never pollutes per-package config.
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;

    // Weak fingerprint inputs: what files go into the WF for cache lookup.
    // Plugin declares these — NOT the user. Configurable via three-tier config.
    fn declared_input_globs(&self, task: &TaskDef, config: &PluginConfig) -> Vec<Glob>;

    // Optional: semantic/ABI fingerprint for early cutoff on downstream tasks.
    // TypeScript: hash .d.ts outputs. Go: hash exported symbols. Rust: cargo metadata.
    // If None: strong fingerprint carries correctness alone.
    fn abi_fingerprint(&self, outputs: &[Path]) -> Option<Hash>;
}
```

The plugin's `declared_input_globs` feeds the weak fingerprint for cache lookup at scale. The plugin is config-aware by construction — it reads its own config section from the resolved workspace+package config before returning. The `extend`/`exclude` model lets workspace and package tiers augment or trim plugin defaults without fully replacing them.

### Progressive Hermeticity

```
$ rage stats
strict (hermetic):   847  (19.6%)
observed (tracking): 3,201 (74.2%)
loose (unchecked):   264  (6.2%)

Observed mode violations (last 7 days — top undeclared reads):
  ../../tsconfig.base.json  (3,201 packages — add to workspace plugin config)
  packages/shared-utils/**  (   47 packages — should declare shared-utils dependency)
```

The observed-mode violation report tells developers exactly what to fix to promote a package from `observed` to `strict`.

---

## Section 4 — Hub/Spoke: Self-Hosted Distributed Builds

Same binary, three modes. No closed-source component. No commercial product required.

### Role Determination

Config-based, not dynamic discovery.

```
No hub config             → standalone (default, local dev)
RAGE_HUB_ADDRESS env var  → spoke (connects to hub at startup)
rage daemon --hub         → hub (accepts spoke registrations on gRPC port)
```

### Authentication

Bearer token. `RAGE_HUB_TOKEN` env var or `rage.json` `[hub]` section. mTLS optional for enterprise environments. Standard, same model as Nx Cloud.

### Rendezvous — Cache as Coordination Point

The hub is **not** a persistent always-on service. It starts on the main CI job (ephemeral machine). At startup it writes its address to the remote cache:

```
cache key: rage/hub-registry/{workspace-hash}/{build-id}
value:     {"addr": "10.0.1.42:9650", "token": "..."}
```

Spokes read this key, get the hub address, connect. No persistent infrastructure beyond the remote cache — which is already required for distributed builds.

This works when hub and spokes share a private network (self-hosted agent pools, same VPC). For public cloud runners (GitHub-hosted, etc.) where spokes cannot reach the hub directly, a relay service (`rage-cloud`, future) would solve the network problem. Self-hosted v1 requires same-network agents.

### Protocol — gRPC

```protobuf
service Coordinator {
  // Spoke subscribes — hub pushes work as DAG unblocks
  rpc Subscribe(WorkerInfo) returns (stream WorkItem);

  // Spoke reports completion
  rpc Complete(CompletionReport) returns (Ack);
}
```

Spoke opens streaming `Subscribe()`. Hub pushes `WorkItem`s as tasks become unblocked in the DAG. No polling. No external work queue. **The hub's in-memory DAG IS the queue.**

### Artifact Routing

Through the remote cache, **not** the hub.

1. Spoke completes a task → uploads outputs directly to the remote cache.
2. Hub records: "task X outputs at cache key Z."
3. Hub assigns downstream task Y to another spoke: "fetch outputs from cache key Z first."
4. Spoke fetches from cache directly — hub never touches the data.

Hub = scheduler and coordinator only. Not a data plane. Scales cleanly for large build artifacts.

This is the critical architectural departure from Nx Cloud, which routes all artifacts through its orchestrator and therefore becomes a data-plane bottleneck (and a commercial licensing choke point).

### Spoke Provisioning — V1: Manual DTE

```yaml
# CI YAML (GitHub Actions example)
jobs:
  main:
    steps:
      - run: rage ci --hub     # starts hub, writes addr to cache, sends task graph
      - run: rage wait         # waits for convergence signal from hub

  agents:
    strategy:
      matrix: { agent: [1, 2, 3] }
    steps:
      - run: rage daemon --spoke   # reads hub addr from cache, connects, polls
```

Task distribution is fully automatic once spokes register. The developer declares **how many** spokes in the CI matrix. The hub handles **what runs where**. Auto-provisioning (talking to CI provider APIs to provision agents dynamically, Nx Agents style) is v2.

### Crash Recovery

If the hub crashes, the in-memory queue is lost. Spokes reconnect, CI reruns the build. Re-evaluate from scratch on restart — no SQLite-based resumption. Resumption would require trusting in-flight task state (output written? sandbox consistent?), which is a correctness risk not worth the performance gain.

---

## Section 5 — Cache Key Design

### Two-Phase Fingerprinting

Same fundamental structure as BuildXL.

**Phase 1 — Weak fingerprint (WF) lookup:**

```
WF = hash(
  command line,
  tool binary hash,
  package path,
  plugin-declared input globs → resolved to file hashes,   ← plugin, not user
  tracked env vars from workspace config                   ← workspace, not per-task
)
```

The WF is used to retrieve candidate pathsets (prior observed file access sets) from the cache.

**Phase 2 — Strong fingerprint (SF) check:**

```
SF = hash(WF) ⊕ hash(contents of files in pathset)
```

The SF is the exact cache key. Computed by hashing the current contents of files in the stored pathset. If SF matches a stored entry: hit. Otherwise: miss, execute, record new pathset.

### The Key Departure from BuildXL

**Declared input hashes in the WF come from the ecosystem plugin, not the user.** The plugin's `declared_input_globs()` method returns what it knows a task typically reads. No DScript. No pip-level input declarations. No user action required.

This is what eliminates BuildXL's declaration "art" without sacrificing scale: the WF retains the pathset-discrimination property that matters at Windows-at-Microsoft scale, but the declaration is centralized in the plugin, augmented at the workspace tier, and overridden per-package only when necessary.

### Plugin ABI Fingerprint (Optional Optimization)

When a plugin provides `abi_fingerprint()` — TypeScript hashes `.d.ts` outputs, Java hashes header jars, Go hashes exported symbol tables, Rust uses `cargo metadata` — rage uses the semantic fingerprint for downstream cache cutoff. A formatting-only change that doesn't alter the `.d.ts` does not invalidate downstream packages.

If no ABI fingerprint is provided, the strong fingerprint carries correctness alone. Both paths are correct. The semantic fingerprint is a hit-rate optimization, not a correctness requirement.

| Package type | Cache key path | Correctness | Hit rate |
|---|---|---|---|
| Isolated TS (`isolatedDeclarations`) | Semantic (`.d.ts` hash) + SF | ✅ | Highest |
| Non-isolated TS | SF (package-level) | ✅ | Good |
| Go, Rust, Python (plugin w/ ABI) | Semantic + SF | ✅ | Highest |
| No-plugin / loose | WF only | ✅ (with caveats) | Weakest |

### Sandbox Execution Model

- Sandbox runs during task **execution** (cache miss) — records the pathset.
- Sandbox does **not** run during cache **lookup** — only file content hashing.
- Same model as BuildXL. Overhead is on execution, not lookup.
- First run: always a cache miss (no prior pathset). Sandbox records. Subsequent runs use the stored pathset for the SF check.

### Sandbox Modes

| Mode | Sandbox runs? | Blocks on undeclared reads? | Use case |
|---|---|---|---|
| `observed` (default) | Yes (on execution) | No (warns only) | Most packages, most of the time |
| `strict` | Yes | Yes | Hermetic-verified packages |
| `loose` | No | — (no enforcement) | Legacy packages, WF-only cache |

The strong fingerprint carries correctness for `observed` and `strict`. `loose` is an escape hatch for packages where sandbox overhead isn't worthwhile.

### Cache Miss Diagnostics — First-Class Feature

```
$ rage why-miss packages/core#typecheck

Cache miss: packages/core#typecheck
  Changed inputs since last run:
    src/parser.ts              (content changed)
    ../../tsconfig.base.json   (content changed — shared config)

  Unchanged (not the cause):
    src/utils.ts, src/types.ts  (88 files)

  Inherited from:
    packages/shared-utils#build  ✅ cache hit — .d.ts unchanged
```

This closes a known UX gap in BuildXL's `FingerprintStore`: cache miss explanation is actionable, per-task, and always available — not a debugging-only tool.

### Environment Variables

Minimal base env + explicit opt-in tracking. `rage.json` workspace config declares which env vars are tracked (their values go into the WF) versus passthrough (visible to the process but not in the WF). No auto-detection — Rice's theorem: you cannot prove which env vars affect output. Make the surface visible and auditable via `rage env-audit`.

---

## Data Flow

### Local Session (Standalone)

```
Developer → `rage dev` (CLI)
    ↓ Unix socket
Daemon (reconciliation loop)
    ├── build-graph + scheduler → DAG
    ├── cache (two-phase FP) → hit/miss decisions
    ├── sandbox → records pathsets on execution
    └── HTTP/WS server
          ↓
Browser status page / VS Code extension (observers & controllers)
```

### Distributed Build (Hub + Spokes)

```
CI: main job                  CI: spoke jobs (N matrix entries)
    ↓                             ↓
rage daemon --hub              rage daemon --spoke
    ↓ writes hub addr             ↓ reads hub addr
Remote Cache ←──────────────────→ Remote Cache
    ↑                             ↓ gRPC Subscribe()
    │                         Hub pushes WorkItem
    │                             ↓
    │                         Spoke executes (sandbox)
    │                             ↓ uploads outputs
    └─── artifacts via cache ←────┘
                                  ↓ Complete()
                              Hub schedules next unblocked tasks
```

Hub handles control plane only. Cache handles data plane.

---

## Error Handling

- **Task failure** → daemon enters `blocked` state; status page shows expanded error with `[retry]` button; WS event pushed to all observers.
- **Daemon crash** → client retries; on reconnect finds no daemon; starts a new one; reconciliation restarts from scratch (no SQLite resume — correctness over speed).
- **Version mismatch** → daemon self-restarts before dying; client retries against new daemon.
- **Hub crash** → in-memory queue lost; spokes disconnect; CI rerun. No distributed resumption in v1.
- **Spoke disconnect** → hub re-queues tasks assigned to that spoke; remaining spokes pick up the slack.
- **Undeclared sandbox read (observed mode)** → warning in task output, entry added to violation report, task continues.
- **Undeclared sandbox read (strict mode)** → task fails; error points at file and suggested plugin config additions.
- **Cache backend unavailable** → fallback to local-only execution; warning on status page; no correctness loss.

---

## Testing Strategy

- **Daemon lifecycle** — start/stop/restart, idle timeout, version mismatch restart, discovery file correctness, Unix-socket/HTTP-port binding.
- **Reconciliation loop** — desired state transitions, file-change-driven invalidation using stored pathsets (verify only affected tasks invalidate), SetDesiredState replacement semantics.
- **Status page / WS** — end-to-end: CLI sets desired state, page receives events in order, `[retry]` round-trips, reconnect behavior.
- **Config resolution** — three-tier merge semantics, `extend`/`exclude` on plugin globs, glob policy matching correctness.
- **Plugin contract** — reference implementations for `rage-typescript`, `rage-rust`, `rage-go`, `rage-python`, `rage-cpp`; contract tests for each plugin method.
- **Cache** — WF/SF correctness under input variations (command line, env vars, plugin globs); pathset retrieval across runs; ABI fingerprint early cutoff verified for TypeScript and at least one other ecosystem.
- **Sandbox modes** — `observed` records without blocking, `strict` blocks with actionable error, `loose` bypasses cleanly.
- **Hub/Spoke** — rendezvous via cache, gRPC Subscribe/Complete streaming, DAG re-evaluation on completion, crash recovery (spoke drop, hub crash → rerun), bearer token auth.
- **Cache miss diagnostics** — `rage why-miss` output correctness across representative scenarios.

---

## Open Questions

1. **gRPC proto design for spoke↔hub** — the `Coordinator` service, `WorkItem`, `CompletionReport`, `WorkerInfo` message schemas need specification before hub mode is implemented.
2. **Spoke auto-provisioning (v2)** — how rage talks to CI provider APIs (GitHub Actions, Azure Pipelines) to provision agents dynamically. Not required for v1 manual DTE.
3. **rage-cloud relay** — for public cloud runner environments where spokes can't reach the hub directly. Lightweight TCP relay. Future product/service, not v1.
4. **Plugin ABI fingerprint implementations** — TypeScript (`.d.ts` hashing) is clear. Go, Rust, Python ABI extraction strategies need per-language design.
5. **Pathset augmentation strategy** — when too many pathsets accumulate under one WF (BuildXL's `/pathSetThreshold` problem), rage needs a pruning/augmentation strategy. The observation-bootstrapped WF helps but doesn't eliminate this.
6. **`rage env-audit` command design** — shows tracked vs passthrough env vars per task type. UX and implementation need spec.
