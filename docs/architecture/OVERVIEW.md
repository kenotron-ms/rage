# Architecture Overview

rage is a single Rust binary, 16 crates, ~40,000 lines. It runs in three modes — local daemon, hub, spoke — selected by config and CLI flags. The same scheduler, cache, sandbox, and plugin code runs in all three; only the work source and the work sink differ.

```
┌──────────────────────────────────────────────────────────────────────┐
│                              CLI (crates/cli)                        │
│   rage run · rage dev · rage daemon · rage status · rage open        │
│   rage why-miss · rage graph · rage hub · rage spoke                 │
└──────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼  Unix socket (local) / gRPC (cluster)
┌──────────────────────────────────────────────────────────────────────┐
│                           daemon (crates/daemon)                     │
│                                                                      │
│   Reconciliation loop:  desired-state → DAG diff → run / restore     │
│                                                                      │
│   ┌──────────────────┐  ┌──────────────────┐  ┌─────────────────┐    │
│   │ workspace-tools  │  │   build-graph    │  │     scoping     │    │
│   │ (manifests, PMs) │→ │ (package + task  │→ │ (affected, git  │    │
│   │                  │  │       DAG)       │  │  diff, scopes)  │    │
│   └──────────────────┘  └──────────────────┘  └─────────────────┘    │
│           │                       │                     │            │
│           ▼                       ▼                     ▼            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │                  scheduler (crates/scheduler)                │   │
│   │   topological execution · ABI-aware early cutoff · failure   │   │
│   │   propagation · postinstall lifecycle · streaming output     │   │
│   └──────────────────────────────────────────────────────────────┘   │
│           │                       │                     │            │
│           ▼                       ▼                     ▼            │
│   ┌──────────────┐    ┌─────────────────────┐    ┌─────────────┐     │
│   │  cache       │    │      sandbox        │    │  artifact-  │     │
│   │  (two-phase  │    │  macOS DYLD ·       │    │   store     │     │
│   │  WF→SF)      │    │  Linux eBPF · linux │    │  (CAS)      │     │
│   └──────────────┘    │  -ebpf-prog (BPF)   │    └─────────────┘     │
│                       └─────────────────────┘                        │
│                                                                      │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  plugin trait (crates/plugin)                                │   │
│   │  ┌──────────────────────┐                                    │   │
│   │  │ plugin-typescript    │   ← only ecosystem implemented v1  │   │
│   │  │ (yarn/pnpm/npm,      │                                    │   │
│   │  │  tsc, .d.ts ABI)     │                                    │   │
│   │  └──────────────────────┘                                    │   │
│   └──────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼ gRPC (tonic / prost)
┌──────────────────────────────────────────────────────────────────────┐
│                hub (crates/hub) ◄──► spoke-client (crates/spoke-…)   │
│   in-memory DAG; rendezvous via shared `rage-hub.json`               │
└──────────────────────────────────────────────────────────────────────┘
```

## Crates

| Crate | Lines (approx) | Purpose |
|---|---|---|
| `cli` | 2,400 | Command parsing; spawns or talks to the daemon. Emits the URL of the status page and exits in milliseconds. |
| `daemon` | 3,500 | Reconciliation loop, Unix socket IPC, HTTP+WebSocket status server, `notify`-crate file watcher, three-state model (converging / ready / blocked). |
| `workspace-tools` | 1,800 | Detects yarn/pnpm/npm; parses `package.json`, `yarn.lock`, `pnpm-lock.yaml`, `package-lock.json`; resolves package graph. |
| `build-graph` | 2,100 | Package-graph + task-graph construction. Tasks are vertices; `dependsOn`, package deps, and `script_name` define edges. |
| `scoping` | 900 | `--affected` (git diff), explicit selectors, `--since <ref>`, target filters. |
| `scheduler` | 6,500 | Topological scheduler over the task DAG. Hosts the runner, the install/postinstall lifecycle, output capture, and the connection between `cache`, `sandbox`, and `artifact-store`. |
| `cache` | 4,800 | Two-phase fingerprinting (`weak_fp`, `strong_fp`, `pathset_store`), tool-binary hashing, output replay, `why_miss` diagnostics, S3/Azure provider backends. |
| `artifact-store` | 1,700 | Content-addressed file store. `LocalArtifactStore` is the v1 backend; layout is `{root}/content/{hex[..2]}/{hex[2..]}/data`. Handles both content-derived and externally-keyed entries (lockfile integrity, postinstall manifest). |
| `sandbox` | 2,000 | Cross-platform sandbox abstraction. Compiles to either macOS DYLD interpose or Linux eBPF backend. Produces a pathset (read set + write set) per task. |
| `sandbox-macos-dylib` | 1,400 | Mach-O `__DATA,__interpose` shared library. Hooks `open`, `openat`, `stat`, `lstat`, `read`, `write`, `rename`, `unlink`, `mkdir`. Loaded via `DYLD_INSERT_LIBRARIES`. |
| `sandbox-linux-ebpf` | 1,200 | aya-based eBPF loader. Pins programs to tracepoints (`sys_enter_openat`, `sys_enter_read`, etc.) and reads ring-buffer events from the kernel into per-task pathsets. |
| `sandbox-linux-ebpf-prog` | 600 | The kernel-side eBPF program (`#![no_std]`, compiled with the BPF target). Filters by PID and emits structured events. |
| `plugin` | 300 | The `EcosystemPlugin` trait, `LockfilePackage`, `RootTask`, `PostinstallTask`, `ArtifactStoreRef`. Designed so a new ecosystem plugin compiles independently and the scheduler stays ecosystem-agnostic. |
| `plugin-typescript` | 5,500 | The first plugin: yarn/pnpm/npm detection, `parse_lockfile` for all three lockfile formats (including yarn berry's `10c0/sha512hex` checksum), `local_pm_cache` for `~/.yarn/cache`, `~/.local/share/pnpm/store`, `~/.npm/_cacache`, `restore_from_cas` (extract tarball + `create_bin_links()`), `postinstall_tasks` (PM-policy-aware), and `.d.ts` ABI fingerprinting. |
| `pipeline-config` | 800 | `rage.json` JSONC parser, three-tier merge (workspace + glob policy + per-package), `$schema` validation. |
| `hub` | 2,200 | gRPC `Coordinator` server. Holds the task DAG in memory; pushes `WorkItem`s to subscribed spokes; receives `Complete` reports; re-evaluates the DAG. No SQLite, no on-disk state. |
| `spoke-client` | 1,000 | gRPC client. Reads hub address from rendezvous, opens `Subscribe(WorkerInfo)` stream, executes received work items using the same scheduler and sandbox as a local daemon, uploads outputs to the shared CAS. |

## Layers

### 1. Discovery — `workspace-tools`, `pipeline-config`, plugin `detection_globs`

A workspace is detected by walking up to find a manifest (`package.json`, `Cargo.toml`, `pyproject.toml`) and a workspace marker (`workspaces` in `package.json`, `[workspace]` in `Cargo.toml`). Plugins declare detection globs; any package matching at least one plugin's globs is in scope. `rage.json` JSONC config is loaded and merged across three tiers:

- **Workspace** — `rage.json` at the root.
- **Glob policy** — `rage.json` `policies` array; CSS-for-packages.
- **Per-package** — `rage` field embedded in the package's existing manifest (`package.json`, `[package.metadata.rage]` in Cargo, `[tool.rage]` in pyproject).

Most packages never set per-package config. The plugin's defaults plus the workspace config cover the common case.

### 2. Graph construction — `build-graph`

The package graph is the workspaces' inter-dependency graph as expressed in their manifests. The task graph is layered on top: each `(package, script)` becomes a task vertex. Edges come from three sources:

1. **Inter-package**: `packages/api#build` depends on `packages/core#build` if `api` declares `core` as a workspace dependency.
2. **Intra-package `dependsOn`**: `packages/api#test` depends on `packages/api#build` if `pipeline.test` says so.
3. **Root tasks**: every package task depends on `workspace#install` (if a plugin declared one).

Cycles are a hard error. Topological order is computed once at graph load and reused.

### 3. Cache — `cache`

Two-phase. Identical to BuildXL.

```
WF = blake3(
  command,
  blake3(tool_binary),
  package_path,
  declared_input_globs → resolved → blake3 of each file,
  tracked env vars,
  upstream_dep_abi_fingerprints,
)

WF → look up stored pathsets in pathset_store
For each candidate pathset:
  SF = blake3(WF || sorted(path || blake3(file_contents) for path in pathset))
  If SF matches a stored entry: cache hit, replay outputs.

Otherwise: cache miss, run the task, sandbox records a new pathset,
store (WF → pathset) and (SF → outputs).
```

Files inside `node_modules/` are excluded from the SF — they are pinned by the lockfile, which is already covered by the root-task fingerprint. Skipping them turns SF computation from O(thousands of files) into O(actual sources read).

See [`CACHING.md`](CACHING.md).

### 4. Sandbox — `sandbox`, `sandbox-macos-dylib`, `sandbox-linux-ebpf`

A task runs in a child process with the sandbox attached. On macOS, the child inherits `DYLD_INSERT_LIBRARIES=/path/to/librage-sandbox.dylib` and a Mach-O `__DATA,__interpose` table redirects libc file-access syscalls to logging shims. On Linux, an eBPF program is attached to `sys_enter_openat`, `sys_enter_read`, `sys_enter_stat`, etc., filtered by PID. Both backends produce the same output: a `Pathset { reads: BTreeSet<PathBuf>, writes: BTreeSet<PathBuf> }`.

The sandbox runs only on cache miss (during execution). Lookup never touches the sandbox.

Three modes: `observed` (record, don't block), `strict` (block undeclared reads, fail the task), `loose` (no sandbox, WF-only cache).

See [`SANDBOX.md`](SANDBOX.md).

### 5. Artifact store — `artifact-store`

Content-addressed file storage. Used for two purposes that share the same backend:

1. **Install cache**: lockfile integrity hashes are CAS keys; values are tarball bytes. One key per package version.
2. **Postinstall cache**: per-(package, platform, node-version) manifests of (rel_path → content-hash, mode, kind) reference per-file CAS entries.

Layout: `{root}/content/{hex[..2]}/{hex[2..]}/data`. Atomic writes via tmp-file + rename; idempotent on the same content. Hardlink restoration with EXDEV-safe copy fallback.

See [`INSTALL-CACHING.md`](INSTALL-CACHING.md).

### 6. Plugin — `plugin`, `plugin-typescript`

The `EcosystemPlugin` trait is the only seam between rage and a language ecosystem. The trait shape:

```rust
pub trait EcosystemPlugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn detection_globs(&self) -> Vec<&'static str>;
    fn infer_tasks(&self, root: &Path) -> Vec<TaskDef>;
    fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;
    fn declared_input_globs(&self, task_name: &str, config: &PluginConfig) -> Vec<String>;
    fn abi_fingerprint(&self, outputs: &[OutputFile]) -> Option<String>;

    // Root task & install lifecycle (defaulted; opt in per ecosystem):
    fn infer_root_tasks(&self, _ws: &Path) -> Vec<RootTask> { vec![] }
    fn verify_install_effects(&self, _ws: &Path) -> bool { true }
    fn parse_lockfile(&self, _ws: &Path) -> Option<Vec<LockfilePackage>> { None }
    fn local_pm_cache(&self, _ws: &Path) -> Option<PathBuf> { None }
    fn restore_from_cas(&self, _pkgs: &[LockfilePackage], _ws: &Path,
                        _store: &dyn ArtifactStoreRef) -> Result<()> { Ok(()) }
    fn postinstall_tasks(&self, _ws: &Path) -> Vec<PostinstallTask> { vec![] }
}
```

Every method has a sensible default. A new plugin (Rust, Go, Python) implements the four core methods and opts into the install lifecycle as needed. The scheduler never branches on plugin id.

### 7. Daemon — `daemon`

One per workspace. Long-lived. Three-hour idle timeout. Self-restarts on version mismatch.

Discovery: the daemon writes `rage-daemon.json` to a workspace-scoped temp dir on startup:

```json
{
  "pid": 12345,
  "unixSocket": "/tmp/rage/<workspace-hash>/daemon.sock",
  "httpPort": 7853,
  "startTime": "2026-04-24T12:00:00Z",
  "version": "0.1.0"
}
```

Reconciliation: each `SetDesiredState` message replaces the active session goal. The daemon walks the DAG, checks the cache, runs missing tasks, and on every file event from `notify` re-evaluates which tasks to invalidate using **stored pathsets**, not glob patterns.

Status server: `GET /` is a vanilla-JS, no-build-step status page (~250 LoC). `GET /api/state` returns a JSON snapshot. `WS /ws` streams events bidirectionally — the page is an observer and a controller (the `[retry]` button posts back over the same socket).

### 8. Hub / spoke — `hub`, `spoke-client`

The hub is the same binary in coordinator mode. It exposes a gRPC `Coordinator` service with two RPCs (`Subscribe`, `Complete`), holds the task DAG in memory, and pushes work as the DAG unblocks. Spokes connect, receive `WorkItem`s, execute them with their own scheduler and sandbox, upload outputs to the shared CAS, and report completion.

Rendezvous is via a shared `rage-hub.json` written by the hub at startup — to a shared volume, an S3 bucket, or any path both sides can read.

See [`DISTRIBUTED.md`](DISTRIBUTED.md).

## Data flow

### Local cache hit

```
rage run build
  → CLI connects to daemon over Unix socket
  → daemon walks DAG, computes WF for each task
  → WF → pathset_store yields candidate pathsets
  → for each candidate: compute SF; matching SF → output_store hit
  → outputs hardlinked into the workspace, stdout/stderr replayed
  → daemon publishes "ready" over WebSocket
  → CLI exits, status page shows green
```

### Local cache miss

```
... (same prefix)
  → no SF match → run the task in a child process with sandbox attached
  → sandbox emits Pathset { reads, writes } as task runs
  → on success: write outputs to output_store, pathset to pathset_store,
    (WF → pathset) and (SF → outputs) entries committed
  → next run with the same content of those files: hit
```

### Distributed

```
hub starts, writes rage-hub.json to shared path
spoke-1, spoke-2, ... start, each:
  → reads rage-hub.json
  → opens Subscribe(WorkerInfo) gRPC stream
hub computes DAG, pushes WorkItem(task=X, package=Y, deps=[Z]) to spoke-1
spoke-1 fetches Z's outputs from shared CAS, executes X, uploads X's outputs
spoke-1 sends Complete(report)
hub marks X done, walks DAG, pushes whatever X just unblocked
```

The hub is control plane only. Artifacts never traverse the hub — they go spoke ↔ CAS directly.

## What is intentionally not in the architecture

- **No persistent hub state.** A hub crash means the build re-runs from scratch. Resumption would require trusting in-flight task state across an opaque crash, which is a correctness risk we explicitly reject.
- **No closed-source coordinator.** The hub is the binary. There is no upgrade path to a paid distributed scheduler.
- **No proprietary remote cache protocol.** The CAS layout is open; backends are S3, Azure Blob, or the filesystem.
- **No DScript / Starlark / configuration language.** `rage.json` is JSONC. Per-package overrides live in the file the package author already owns. Programmatic configuration belongs in plugins, written in Rust.

## What rage borrows, and from where

- **Two-phase fingerprinting (WF→SF) and pathset storage** — BuildXL.
- **Sandbox-as-correctness-primitive** — BuildXL (Detours / VFS); rage's implementation differs by OS (DYLD on macOS, eBPF on Linux).
- **Per-package CAS for installs** — Bazel `rules_js` and BuildXL prep pips.
- **Package graph + task graph + script-level edges** — lage.
- **Daemon UX (status page, ambient daemon, plugin-driven inference)** — Nx; with the difference that rage's daemon is the coordinator at every scale.
- **JSONC config with `$schema`, three-tier overrides** — `tsconfig.json` precedent.
