# Honest Comparison

This is the long version. The short answer is "it depends on what you're optimizing for"; the rest of this document explains what each tool actually does, where it fails, and why rage is shaped the way it is.

We compare against five build systems: **lage**, **Turborepo**, **Nx**, **BuildXL**, and **Bazel**. Each has merits we will not understate. Each has gaps that motivate rage.

---

## TL;DR matrix

| | lage | Turborepo | Nx | BuildXL | Bazel | rage |
|---|---|---|---|---|---|---|
| Package graph | ✓ | ✓ | ✓ | ✗ (pip graph) | ✓ | ✓ |
| Task graph | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Two-phase fingerprint (WF→pathset→SF) | ✗ | ✗ | ✗ | ✓ | partial | ✓ |
| File-access sandbox | ✗ | ✗ | ✗ | ✓ | ✓ | ✓ |
| Cache correctness | trust declared | trust declared | trust declared | provable | provable | provable |
| ABI early-cutoff | ✗ | ✗ | ✗ | ✓ | ✓ | ✓ |
| Per-package install CAS | ✗ | ✗ | ✗ | ✓ | ✓ (rules_js) | ✓ |
| Postinstall caching | ✗ | ✗ | ✗ | ✓ | partial | ✓ |
| Distributed builds | ✗ | Vercel hosted | Nx Cloud / Agents | open, manual | open | **open, in-binary** |
| Self-host distributed | ✗ | ✗ | commercial | ✓ | ✓ | ✓ |
| Setup time, JS monorepo | hours | hours | hours | weeks | weeks | hours |
| Languages out of the box | JS/TS | JS/TS | JS/TS, plugins | C++/C#/JS | every language | JS/TS, plugin contract for others |
| Long-lived daemon | ✗ | ✗ | ✓ | partial (BuildXL service) | ✓ (Bazel server) | ✓ |
| Watch mode driven by sandbox data | ✗ | ✗ | ✗ | ✓ | ✗ | ✓ |
| OS first-class | macOS, Linux | macOS, Linux, Windows | macOS, Linux, Windows | Windows | Linux, macOS | macOS, Linux (Windows planned) |

The rows below explain the entries.

---

## vs lage

[lage](https://github.com/microsoft/lage) is rage's spiritual predecessor — Ken (the author of rage) co-created lage at Microsoft. It is a TypeScript-native task runner over a yarn/pnpm workspace. lage's contribution to the field is the formalization of "package graph + task graph + script-level dependencies" as a declarative configuration in `lage.config.js`. Most of what Turborepo and Nx do at the task-graph level traces its lineage to either lage or one of its contemporaries.

### What lage does well

- **Declarative.** Tasks are pipelines; pipelines are graphs; graphs are computed from package.json + lage config. There is no imperative orchestration code.
- **Fast.** Written in TypeScript but operationally lean — no daemon, just a CLI that reads the workspace and dispatches.
- **Workspace-native.** Reads yarn workspaces directly, runs npm scripts directly, doesn't reinvent the package layer.
- **Caching with packageOverrides.** Task-level cache by hashing inputs. The same content-addressed concept Turbo and Nx popularized later.

### Where lage stops

- **No file-access sandbox.** Cache correctness depends on the user declaring inputs accurately. If a task reads a file outside its declared globs, lage caches a stale result and silently returns it later.
- **No CAS.** Cache outputs are stored as tarballs in a `node_modules/.cache/lage/` directory. No content-addressing, no per-package deduplication, no remote backend that scales beyond a single team's S3 bucket.
- **No distributed builds.** lage scales up to "one machine with many cores." Beyond that, you're on your own.
- **No install caching.** `yarn install` is the user's problem. Cold CI runners pay 30-90s every time.
- **Single language.** TypeScript-shaped from the ground up. Adding Rust or Go would be a rewrite, not a configuration change.

### The rage difference

rage is what lage would have become if we'd had four more years and a budget for correctness work.

- The task graph and package graph constructions are conceptually identical to lage's; rage's `build-graph` crate could be described as "lage's algorithm in Rust."
- The cache, sandbox, and artifact store are the layers lage didn't have time to build.
- The plugin trait is what lage's `lageOptions.tasks` would have grown into if it had to cover Rust and Go, not just JS.
- Distributed builds are the missing leg of the lage stool.

If you're a Microsoft team currently using lage and you've outgrown its single-machine ceiling, rage is the migration path. The mental model is preserved.

### When to use lage instead

- Small JS monorepo, single team, no CI distribution needs.
- You've explicitly opted out of correctness-first caching because your tasks are simple enough that you trust your declared inputs.
- You don't want to deal with a daemon.

---

## vs Turborepo

Turborepo is the current default for new JS monorepos in 2026. It's fast, it's elegant, the DX is excellent, the docs are excellent, and Vercel ships consistent improvements.

### What Turborepo does well

- **Excellent DX.** `turbo run build` just works. The output is clean. The mental model is small.
- **Fast.** Rust core (post-Vercel acquisition) keeps it competitive on cold runs and dominant on warm runs.
- **Workspace + task graph.** Same model as lage; same model as rage.
- **Remote caching with Vercel Remote Cache.** Out of the box.
- **Cross-platform.** Windows, macOS, Linux all first-class.

### Where Turborepo falls short

- **No file-access sandbox.** Turborepo's cache key is `hash(declared_inputs, command)`. If your task reads a file you didn't declare, the cache returns stale results. Vercel's official position is "be careful about your declarations." This is the same trust model as lage; it is not a correctness model.
- **No way to detect missing input declarations.** The system has no observation layer. A miss-declared input is a latent bug that surfaces as a wrong build at runtime, possibly months later, with no diagnostic.
- **Distributed execution is Vercel-hosted only.** Turbo's remote cache is open. Distributed *execution* — fanning a build across N machines — is not. There is no `turbo cluster` you can run on your CI.
- **No install caching.** Same as lage: `yarn install` / `pnpm install` is on you.
- **Single-language affinity.** Plugins exist for some non-JS work, but the model is JS-first.

### The specific mechanism difference

rage replaces Turborepo's `hash(declared_inputs)` with two-phase fingerprinting backed by sandbox observation:

```
Turborepo: cache_key = blake3(declared_inputs ‖ command)
           hit = cache_key matches stored entry

rage:      WF = blake3(declared_inputs_for_lookup ‖ command ‖ tool ‖ env)
           candidate_pathsets = pathset_store[WF]   # observed by sandbox previously
           SF = blake3(WF ‖ contents of candidate_pathset's files)
           hit = SF matches stored entry
```

The key property: rage's pathset is what the task **actually read**, not what the user **said it would read**. Forgetting to declare a file in Turborepo is a silent stale-cache bug. Forgetting to declare a file in rage is a one-time cache miss while the sandbox observes the read; the next run is correct and cached.

### Honest counter-points

- Turborepo is faster for simple workloads where the user really has declared every input correctly. The two-phase scheme has overhead that flat hashing doesn't.
- Turborepo is more polished for non-Microsoft / non-Vercel stacks. rage is pre-1.0.
- Turborepo runs on Windows. rage v1 does not.

### When to use Turborepo instead

- Small to mid-sized JS monorepo where you trust your declared inputs and the team understands the failure mode.
- You're on Vercel infrastructure and remote caching there is where you want to land.
- You don't have a correctness-bug-from-stale-cache postmortem in your team's recent history.

---

## vs Nx

Nx is the most feature-rich of the JS-monorepo build systems. The plugin ecosystem is large, the docs are deep, the project graph is excellent, the affected-detection is mature, and the daemon UX is the closest precedent to what rage's daemon mode looks like.

### What Nx does well

- **Daemon UX.** Nx's silent background daemon is a precedent rage builds on. `nx daemon` keeps a hot cache of the project graph and reduces cold-start latency.
- **Plugin ecosystem.** `@nx/react`, `@nx/node`, `@nx/playwright`, `@nx/cypress`, etc. — every common stack has a plugin. Plugin authors have a stable API.
- **Affected detection.** `nx affected` is the gold standard. Computes the dependent set from a git diff, runs only what changed.
- **Distributed via Nx Cloud / Nx Agents.** Distributed task execution exists and is the most mature in the JS ecosystem.
- **Cross-platform.** Windows, macOS, Linux.

### Where Nx falls short

- **No file-access sandbox.** Same as Turbo: cache correctness depends on declaration accuracy. Nx's "named inputs" are configuration, not observation.
- **Distributed execution is commercial.** Nx Cloud's distributed task execution and Nx Agents' auto-provisioning are paid features. The local runner is open source. The cluster orchestrator is not.
- **Artifact data plane goes through Nx Cloud.** Outputs flow client → Nx Cloud → other clients. The orchestrator is also a data plane, which makes it a network egress cost and a commercial chokepoint.
- **No install caching at the package level.** Same gap as Turbo.
- **TypeScript-shaped.** The plugin model accommodates other languages, but the trait surface and the inferred-task language is JS-native.

### The specific mechanism difference

Nx Cloud is closed-source and commercial above a free tier. rage's hub is open source and self-hosted. Both schedule tasks across a worker pool; both upload outputs to a shared cache. The difference is that the rage hub is the same binary you already have:

```
Nx Cloud:    nx-cloud → managed orchestrator → uploads outputs → other workers fetch
             (you pay per task above the free tier; orchestrator is closed-source)

rage hub:    rage hub --listen X:Y → in-memory DAG + gRPC → uploads outputs → workers fetch
             (you run it yourself; the binary is open source; spokes pay zero coordination cost)
```

For a team that's already paying for Nx Cloud and is happy with it, this distinction is academic. For a team that wants distributed builds without licensing it from a vendor, it matters.

The other mechanism difference is the sandbox. Nx's cache trusts declared inputs; rage's cache trusts observed inputs. The same critique applies as for Turbo: a misdeclared input is silently stale in Nx and self-corrects in rage.

### Honest counter-points

- Nx's plugin ecosystem is far more mature than rage's. rage v1 ships with one plugin (TypeScript). Nx ships with dozens.
- Nx's `affected` is more battle-tested than rage's `scoping` crate.
- Nx's docs and onboarding are better than anything rage will have for a long time.
- Nx Cloud's UX, dashboard, and reporting are polished products. rage has a 250-line vanilla-JS status page.

### When to use Nx instead

- You want a turnkey commercial distributed build offering and you're willing to pay for it.
- You want a deep plugin ecosystem covering every common JS stack out of the box.
- You're on Windows and need a build system today.

---

## vs BuildXL

BuildXL is Microsoft's internal build system, the engine behind Windows builds and other extreme-scale Microsoft monorepos. It is the most rigorous correctness-first build system in widespread use. rage borrows directly from it.

### What BuildXL does well

- **VFS sandbox.** A virtual filesystem layer (Detours on Windows, file-access tracker on Linux) records every file access in every pip. Correctness is provable.
- **Two-phase fingerprinting.** Weak fingerprint discriminates by declared inputs; strong fingerprint discriminates by observed pathset content. rage's caching architecture is BuildXL's caching architecture.
- **Distributed builds at extreme scale.** Windows builds run across hundreds of agents. The model has been proven for over a decade.
- **Hermetic enforcement.** Pips can be marked sealed; undeclared reads fail the build. This is what rage's `strict` mode mirrors.
- **Mature observability.** FingerprintStore exposes "why did this miss?" data for debugging. rage's `why-miss` command is inspired by it.

### Where BuildXL is hard to live with

- **DScript.** BuildXL's pip declarations live in DScript, a bespoke configuration language. It's powerful but it's not Rust, not TypeScript, not anything you'd already know. Onboarding a JS monorepo means learning DScript, which is a multi-week investment per engineer.
- **Pip-level declaration burden.** Every input file, every output file, every transitive dependency must be declared. The sandbox catches *violations* of declarations, but the declarations themselves are the user's job. A 1,000-package monorepo means 1,000 sets of pip declarations, often spread across multiple DScript files per package.
- **Windows-first.** BuildXL ships on Windows and Linux; macOS support has been lighter and the file-access tracker has historically been less battle-tested off Windows. NTFS-specific assumptions are baked in.
- **No open coordinator.** The CB cluster (Microsoft's distributed BuildXL) is internal. The build engine is open source; the scheduler infrastructure is not.
- **Single-team operational profile.** BuildXL is operated by full-time build engineers at Microsoft. The OSS deployment story is "good luck."

### The specific mechanism difference

rage's correctness mechanism is BuildXL's correctness mechanism. The differences are at the seams:

| Concern | BuildXL | rage |
|---|---|---|
| Pip / task declarations | DScript per pip | Plugin-declared defaults; user overrides per package |
| Onboarding a JS monorepo | weeks (write DScript) | hours (install rage, write `rage.json`, run) |
| Sandbox on macOS | partial | DYLD interpose, full coverage |
| Sandbox on Linux | file-access tracker | eBPF tracepoints |
| Sandbox on Windows | Detours (`DetoursServices.dll`, 100+ APIs hooked) | Detours (planned `sandbox-windows-detours`, file-access subset for cache correctness) |
| Distributed coordinator | internal (CB cluster) | open-source `rage hub`, self-hosted |
| Config language | DScript | JSONC + per-package manifest fields |
| Cache rendezvous | internal | open CAS layout (S3 / Azure / fs) |

The key shift is **plugin-borne declaration**. In BuildXL, the user writes DScript for each pip declaring what `tsc` reads. In rage, the TypeScript plugin declares "tsc reads `**/*.ts` and `tsconfig*.json`" once, and the workspace inherits it. The user only overrides when their package genuinely diverges. The two-phase fingerprinting and sandbox-observation parts of correctness are unchanged; the declaration burden has been moved off the user and into the plugin.

### Sandbox alignment on Windows

BuildXL's Windows sandbox is `DetoursServices.dll` — a 7,516-line C++ DLL that uses Microsoft Detours to inline-patch `CreateFileW`, `NtCreateFile`, and roughly 100 other file/process APIs, with a named-pipe channel back to the BuildXL host. rage's planned Windows sandbox uses the **same mechanism**: Detours inline patching, DLL injection via `DetourCreateProcessWithDllsW`, named-pipe IPC. The scope is narrower (rage hooks the file read/write pathset for cache correctness; BuildXL also enforces a full policy engine), but the underlying technique is identical.

This is intentional. If BuildXL's Windows sandbox is the proven correct way to observe file access on NTFS without a kernel driver — and a decade of Windows builds at Microsoft scale says it is — then rage on Windows should follow that path rather than invent a parallel one. The alignment also means a team that has lived with BuildXL on Windows can read rage's Windows sandbox source and recognize it.

See [`SANDBOX.md`](SANDBOX.md) for the implementation details.

### Honest counter-points

- BuildXL is more battle-tested at scale than rage will be for years. Windows builds at ten thousand+ pips, 200-machine clusters, 10-year continuity.
- BuildXL's Windows sandbox via Detours is more comprehensive than rage's macOS DYLD interpose. Windows has APIs for VFS that macOS lacks.
- BuildXL has features rage doesn't: more sophisticated pip scheduling policies, cross-build caching with per-pip granularity, fine-grained credential isolation per pip.
- BuildXL is open source; you *can* run the cluster yourself if you have the operational team to do it. rage is just easier.

### When to use BuildXL instead

- You are Microsoft, and your team is already operating BuildXL in production.
- You are building something Windows-shaped and need NTFS-aware sandbox features rage doesn't have.
- Your monorepo is large enough that the DScript investment is amortized across thousands of engineer-years.

---

## vs Bazel

Bazel is Google's open-source build system, the gold standard for hermetic correctness. rules_js (Aspect's npm support) brought first-class JS support to Bazel.

### What Bazel does well

- **Ultimate hermeticity.** Sandbox-exec on macOS, user namespaces on Linux, full process isolation. A Bazel build runs in a chroot-like environment by default.
- **Multi-language.** C++, Java, Go, Rust, Swift, Python, JS, every language has rules. The build system is the language layer.
- **Decades of scale proof.** Google's monorepo, 2 billion lines of code, runs on Bazel's lineage (blaze internally). It scales.
- **Remote execution and remote caching.** The Remote Execution API is an open protocol; multiple implementations (Buildbarn, BuildBuddy, etc.) interoperate.
- **Cross-platform.** Windows, macOS, Linux.

### Where Bazel is hard to live with for JS monorepos

- **rules_js complexity.** Setting up Bazel for a JS monorepo means learning Bazel + Starlark + WORKSPACE / MODULE.bazel + BUILD.bazel files + rules_js conventions + lifecycle_hooks. The 10-minute setup is "hello world." The week-long setup is your actual monorepo.
- **Explicit lifecycle declaration.** rules_js's `lifecycle_hooks` requires you to declare which packages need postinstall to run. There is no auto-detection. If you don't list `esbuild`, `esbuild`'s postinstall doesn't run, and you get a non-functional install.
- **BUILD.bazel file proliferation.** Every directory needs a BUILD file describing its targets. Tooling helps; the conceptual overhead doesn't go away.
- **Refusal to build on declaration errors.** Bazel's correctness comes from refusing to compile if your declarations are wrong. This is correct behavior — but for a team migrating from npm scripts, it's a wall of "missing dependency" errors that has to be paid down.
- **Rust/Cargo-shaped tools fit awkwardly.** Bazel wants to drive `rustc` directly, not call out to `cargo`. Many existing Rust workflows resist this.

### The specific mechanism difference

| Concern | Bazel + rules_js | rage |
|---|---|---|
| Postinstall detection | manual declaration in `lifecycle_hooks` | plugin walks `node_modules`, reads `package.json:scripts.postinstall`, respects PM policy |
| Input declaration | required, refuses to build on miss | plugin defaults; sandbox observes; user overrides |
| Setup time, JS monorepo | weeks | hours |
| Build language | Starlark in BUILD files | TypeScript / npm scripts unchanged |
| Postinstall caching | partial (per the Bazel pkg's repository_rule) | per-package, platform-keyed, plugin-driven |

rage's bet is that **plugin-driven auto-discovery is the right default for JS monorepos**, and **Bazel's required-declaration model is the right default for ten-language hermetic builds**. They are both internally consistent. They optimize for different points.

For a JS monorepo team, the question is: do you want correctness from required declarations (Bazel) or correctness from observed sandbox + plugin defaults (rage)? Both are correct. rage gets you there with an order of magnitude less setup.

### Honest counter-points

- Bazel's hermeticity guarantees are stronger than rage's. A Bazel sandbox on Linux uses user namespaces and cgroups; rage's eBPF observation can be bypassed by direct `syscall(2)` (rare, but possible).
- Bazel scales to truly extreme monorepos (Google scale) in ways no other open-source build system has demonstrated.
- Bazel's remote execution protocol is an industry standard. rage's gRPC is rage-specific.
- Bazel runs on Windows. rage v1 does not.
- Bazel's plugin ecosystem covers every language. rage v1 ships with one (TypeScript).

### When to use Bazel instead

- You have a multi-language monorepo (e.g., Go + C++ + Rust + JS) and Bazel rules cover all of them.
- You need remote execution, not just remote caching, and you want to use BuildBuddy / Buildbarn / Engflow as your backend.
- You are willing to pay the upfront declaration cost for the hermeticity guarantee.
- Your team has at least one engineer who genuinely enjoys Starlark.

---

## What rage is not trying to be

- **A drop-in Bazel replacement.** Bazel will always handle multi-language, hermetic, extreme-scale builds better than rage will.
- **The fastest cache on warm hits.** Turborepo on a small workload with all-correct declarations beats rage by a few percent. The two-phase scheme has overhead.
- **A Windows-first build system.** v1 ships on macOS and Linux. Windows is planned, with a Detours-based sandbox modeled directly on BuildXL's `DetoursServices.dll` — same mechanism, narrower scope. Until that crate lands, rage on Windows is not a supported configuration.
- **A SaaS.** There is no rage Cloud. There may be a `rage-cloud` relay for cross-network rendezvous in the future, but it will be open-source and self-hostable.

## What rage is trying to be

A build system for JS-and-then-other monorepos that wants:

1. **Correctness from sandbox observation**, not from declaration discipline.
2. **Performance from per-package CAS and two-phase fingerprinting**, not from "trust the user."
3. **Distribution in the open-source binary**, not behind a paywall.
4. **Setup measured in hours**, not weeks.
5. **One tool that scales from local dev to a CI cluster**, not three different products glued together.

If those tradeoffs are what you want, rage is for you. If you want any of them more strongly than rage delivers, one of the five tools above is a better fit, and this document is the honest map of which one and why.

---

## Decision tree

```
Are you on Windows and need it today?
  → Turborepo (or Nx, or BuildXL if you're Microsoft)

Multi-language monorepo where every language has different toolchains?
  → Bazel

Single team, small JS monorepo, simple inputs, no CI fan-out?
  → lage or Turborepo

Mid-sized JS monorepo, want a polished commercial DX, OK with paying for distribution?
  → Nx + Nx Cloud

Microsoft-internal team already operating BuildXL?
  → BuildXL

Mid-to-large JS monorepo, want self-hosted distributed builds, want sandbox-observed correctness, ready to be on macOS/Linux:
  → rage
```
