# rage

A Rust-native monorepo build system that combines BuildXL-style sandboxed file-access tracking and two-phase fingerprinting with the package-graph ergonomics of lage and Nx. 16 crates, ~40,000 lines of Rust, one binary that runs as a local daemon, a CI worker, or a cluster coordinator. Sandboxed, content-addressed, distributed, self-hosted.

## Why rage

**Existing JS-monorepo build systems are fast but not provably correct.** Turborepo and Nx fingerprint a task by hashing its declared inputs plus its command line. If a task reads a file you forgot to declare, the cache silently returns a stale result. There is no observation layer that catches the mistake.

**Existing correctness-first build systems are correct but not ergonomic.** BuildXL and Bazel solve cache correctness by demanding total declaration: every input, every output, every tool, in DScript or Starlark. They are proven at extreme scale, but a JS monorepo team measures the setup in weeks, not hours, and fights the system every time a postinstall script touches the wrong file.

**Distributed builds are a paywall.** Nx Cloud, Turbo Remote Cache, BuildXL's CB cluster — the orchestrators that let a build fan out across N machines are either commercial products or internal-only. You can self-host the cache. You cannot self-host the scheduler.

rage is the synthesis: BuildXL-style two-phase fingerprinting and a real syscall-level sandbox, lage-style declarative task graphs over a package graph, ecosystem plugins so users don't write input declarations by hand, and an open hub/spoke distributed scheduler in the same binary as the local daemon. Correctness comes from observation, not declaration. Speed comes from a per-package content-addressed store. Distribution comes for free.

## Architecture overview

```
                                   ┌──────────────────────────────────┐
                                   │  rage daemon (one per workspace) │
                                   │                                  │
  rage run / dev / status ─────────┼─► Unix socket (CLI ⇄ daemon)     │
                                   │                                  │
  Browser / VS Code   ─────────────┼─► HTTP + WebSocket (status page) │
                                   │                                  │
                                   │  ┌────────────────────────────┐  │
                                   │  │ Reconciliation loop        │  │
                                   │  │  desired state →           │  │
                                   │  │  build-graph + scheduler   │  │
                                   │  │  two-phase cache (WF→SF)   │  │
                                   │  │  sandbox (DYLD / eBPF)     │  │
                                   │  │  artifact-store (CAS)      │  │
                                   │  │  ecosystem plugins         │  │
                                   │  └────────────────────────────┘  │
                                   └──────────┬──────────┬────────────┘
                                              │          │
                              ┌───────────────┘          └───────────────┐
                              │                                          │
                       ┌──────▼──────┐                            ┌──────▼──────┐
                       │  rage hub   │ ◄── gRPC Subscribe stream ─┤ rage spoke  │
                       │ (CI coord)  │ ──► WorkItem / Complete  ─►│ (CI worker) │
                       └─────────────┘                            └─────────────┘
                              ▲                                          │
                              │                                          │
                              └────────── shared remote CAS ◄────────────┘
                                          (S3 / Azure / fs)
```

The daemon, the hub, and the spoke are all `rage` — same binary, different config. The hub is the in-memory DAG; the cache is the data plane; spokes pull artifacts directly without going through the hub. There is no proprietary coordinator.

## Key capabilities

| Capability | Mechanism |
|---|---|
| **Provable cache correctness** | Sandbox observes every `open`/`stat`/`read` during task execution and records a pathset. The strong fingerprint is computed over the contents of files actually accessed — not files you remembered to declare. |
| **Two-phase fingerprinting** | Weak fingerprint (command + tool hash + declared globs + tracked env) retrieves candidate pathsets; strong fingerprint hashes the pathset's actual contents. Identical to BuildXL's mechanism. |
| **Per-package install cache** | Lockfile integrity hashes are the CAS keys. One package version bump → one cache miss; 1,499 other packages still hit. Restore is a hardlink loop, not a tar extraction. |
| **Postinstall caching** | Each `postinstall` script's effects are captured per (tarball, platform, node-version), stored as a manifest of (path, content-hash, mode, kind). Native addons and downloaded binaries replay from CAS, executable bits intact. |
| **ABI-aware downstream cutoff** | TypeScript plugin hashes `.d.ts` after `tsc`. Downstream weak fingerprints include this hash. Formatting changes that don't alter the public surface let dependents short-circuit. |
| **Self-hosted distributed builds** | gRPC `Coordinator` service: spokes `Subscribe(WorkerInfo)` and stream `WorkItem`s; hub holds the DAG in memory and writes its address to a shared rendezvous. Same binary as the local daemon. |
| **Sandbox by syscall, not by trust** | macOS: `DYLD_INSERT_LIBRARIES` interpose hooks on the libc file-access syscalls. Linux: eBPF programs attached to `sys_enter_openat` etc. via aya. No dtrace. No ptrace. No file watcher heuristics. |
| **Daemon as reconciliation loop** | `rage dev` declares a desired state and exits in milliseconds. The daemon converges toward it continuously. File changes invalidate exactly the tasks whose stored read-sets included the changed file. |

## Honest comparison

| | lage | Turborepo | Nx | BuildXL | Bazel | **rage** |
|---|---|---|---|---|---|---|
| Package graph | ✓ | ✓ | ✓ | ✗ (pip graph) | ✓ | ✓ |
| File-access sandbox | ✗ | ✗ | ✗ | ✓ (Detours/VFS) | ✓ (sandbox-exec / Linux ns) | ✓ (DYLD / eBPF) |
| Two-phase fingerprint | ✗ | ✗ | ✗ | ✓ | partial | ✓ |
| Per-package install CAS | ✗ | ✗ | ✗ | ✓ | ✓ (rules_js) | ✓ |
| Postinstall caching | ✗ | ✗ | ✗ | ✓ | partial | ✓ |
| Distributed builds | ✗ | Vercel only | Nx Cloud only | open, manual | open | **open, same binary** |
| Setup time for JS monorepo | hours | hours | hours | weeks | weeks | hours |
| Auto-discovery of inputs | partial | declared | declared (plugins) | declared (DScript) | declared (BUILD) | plugin-driven |

See [`docs/architecture/COMPARISON.md`](docs/architecture/COMPARISON.md) for the full breakdown — what each tool does well, where it fails, and which one to use.

## Quick start

```bash
# Build from source
cargo install --path crates/cli

# In a yarn / pnpm / npm workspace:
cd my-monorepo
rage run build                  # build everything; daemon starts in the background
rage dev --target typecheck     # converge toward "all packages typechecked"; opens status page
rage why-miss packages/api#build   # explain a cache miss
rage graph                       # render the task DAG
rage status                      # current daemon state (converging / ready / blocked)
```

Distributed:

```bash
# CI hub
rage hub --listen 0.0.0.0:9650

# CI spokes (matrix job, N replicas)
rage spoke --hub $RAGE_HUB_ADDRESS
```

## Documentation

- [`docs/architecture/OVERVIEW.md`](docs/architecture/OVERVIEW.md) — system architecture and crate layout
- [`docs/architecture/CACHING.md`](docs/architecture/CACHING.md) — two-phase fingerprinting end to end
- [`docs/architecture/SANDBOX.md`](docs/architecture/SANDBOX.md) — DYLD interpose on macOS, eBPF on Linux
- [`docs/architecture/INSTALL-CACHING.md`](docs/architecture/INSTALL-CACHING.md) — install + postinstall artifact cache
- [`docs/architecture/DISTRIBUTED.md`](docs/architecture/DISTRIBUTED.md) — hub/spoke gRPC scheduler
- [`docs/architecture/COMPARISON.md`](docs/architecture/COMPARISON.md) — vs lage, Turborepo, Nx, BuildXL, Bazel

## Status

Pre-1.0. macOS and Linux supported. Windows is not yet implemented. The TypeScript plugin is the first ecosystem; the trait is designed for Rust, Go, and Python plugins to follow without scheduler changes.
