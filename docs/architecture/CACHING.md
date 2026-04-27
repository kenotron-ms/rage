# Caching

rage's task cache is a two-phase content-addressed system, modelled directly on BuildXL's fingerprinting scheme. It exists in `crates/cache` and is used by every task the scheduler runs except install lifecycle tasks (which use the install artifact cache; see [`INSTALL-CACHING.md`](INSTALL-CACHING.md)).

## Two-phase fingerprinting

```
                          ┌──────────────────────────────────┐
                          │ 1. Compute Weak Fingerprint (WF) │
                          └──────────────────────────────────┘
                                          │
                                          ▼
                  ┌────────────────────────────────────────────────┐
                  │ 2. Look up WF in pathset_store                 │
                  │    pathset_store: WF → [Pathset, Pathset, …]   │
                  └────────────────────────────────────────────────┘
                                          │
              ┌───────────────────────────┼───────────────────────────┐
              │                           │                           │
              ▼                           ▼                           ▼
        no pathsets              one or more pathsets           never seen
        (cold cache)             (warm cache)                  this task
              │                           │                           │
              │                           ▼                           │
              │      ┌──────────────────────────────────────────┐     │
              │      │ 3. For each candidate pathset P:         │     │
              │      │      SF = blake3(WF || hashes of P's     │     │
              │      │           current file contents)         │     │
              │      │      lookup SF in output_store           │     │
              │      └──────────────────────────────────────────┘     │
              │                           │                           │
              ▼                           ▼                           ▼
       ┌─────────────────────────────────────────────────────────────────┐
       │                       4. Cache miss path                        │
       │      Run task in child process with sandbox attached            │
       │      Sandbox emits new Pathset { reads, writes }                │
       │      SF = blake3(WF || hashes of new pathset's reads)           │
       │      pathset_store.append(WF, pathset)                          │
       │      output_store.put(SF, captured outputs + stdio)             │
       └─────────────────────────────────────────────────────────────────┘
                                          │
                                          ▼
                              ┌────────────────────────┐
                              │ Cache hit path:        │
                              │ replay outputs + stdio │
                              │ from output_store      │
                              └────────────────────────┘
```

The Weak Fingerprint discriminates by **what could matter** to the task. The Strong Fingerprint discriminates by **what actually mattered**, i.e. the content of files the task is observed to have read.

## Weak Fingerprint

`crates/cache/src/weak_fp.rs`:

```rust
pub struct WeakFpInputs<'a> {
    pub command: &'a str,
    pub tool_path: &'a Path,
    pub package_path: &'a Path,
    pub declared_input_globs: &'a [String],
    pub tracked_env: &'a [(String, String)],
    pub dep_abi_fingerprints: &'a [(String, String)],
}
```

The hash is `blake3` over:

| Component | Source | Why |
|---|---|---|
| `command` | The shell command the task runs (`tsc -b` etc.) | Different command → different output. |
| `blake3(tool_binary)` | Bytes of the resolved tool binary on disk | Compiler upgrade → different output, same command. |
| `package_path` | Workspace-relative package path | Differentiates `packages/api#build` from `packages/web#build` even if commands collide. |
| `declared_input_globs → file hashes` | Glob expansion against the package directory; sorted; each file hashed with blake3 | The plugin says "TypeScript reads `**/*.ts` and `tsconfig*.json`". The WF includes their content hashes. |
| `tracked_env` | `(key, value)` pairs from `rage.json` `tracked_env` | Some tasks branch on env (`NODE_ENV=production`); explicitly opt-in, never auto-detected (Rice's theorem). |
| `dep_abi_fingerprints` | For each direct dependency package, its ABI hash from the upstream task's last run | Enables ABI-aware downstream cutoff. |

Plugin authors define `declared_input_globs(task_name, config)`. The `config` argument carries the resolved three-tier config — workspace `rage.json` plus glob policies plus per-package overrides — with `extend` and `exclude` already applied. Users never write input globs directly for the common case.

`tool_path` resolution is deliberate: if a workspace uses `npx tsc`, the resolved tool is `node_modules/.bin/tsc` (a symlink into `typescript/bin/tsc`); if it uses `pnpm exec`, similar. The hash follows the symlink. A `typescript` upgrade flips the WF for every TypeScript task.

## Pathset

The pathset is what the sandbox produced during a prior execution of this task:

```rust
pub struct Pathset {
    pub reads: BTreeSet<PathBuf>,    // every file the task read
    pub writes: BTreeSet<PathBuf>,   // every file the task wrote
}
```

Stored in `pathset_store` keyed by WF. A single WF can have **multiple** pathsets attached — for example, the same `tsc -b` invocation may follow different code paths under different inputs and read different files. The strong-fingerprint phase disambiguates them.

`pathset_store` is a content-addressed file under `~/.rage/cache/pathsets/{wf_prefix}/{wf_hex}.json`. New pathsets are appended (never replace) so multiple recorded pathsets coexist under one WF.

## Strong Fingerprint

`crates/cache/src/strong_fp.rs`:

```rust
pub fn compute_strong_fingerprint(weak_fp: &str, pathset_reads: &[PathBuf]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"wf:");
    hasher.update(weak_fp.as_bytes());
    hasher.update(b"\n");

    let mut sorted: Vec<&Path> = pathset_reads.iter()
        .map(|p| p.as_path())
        .filter(|p| !p.components()
            .any(|c| c.as_os_str() == OsStr::new("node_modules")))
        .collect();
    sorted.sort();
    sorted.dedup();

    for p in sorted {
        hasher.update(b"read:");
        hasher.update(p.as_os_str().as_encoded_bytes());
        hasher.update(b":");
        let content = std::fs::read(p).unwrap_or_default();
        hasher.update(blake3::hash(&content).as_bytes());
        hasher.update(b"\n");
    }

    hasher.finalize().to_hex().to_string()
}
```

The SF is the cache key. Notable details:

- **`node_modules` files are excluded** from the SF. They are pinned by the lockfile, which is already covered by the root-task fingerprint (`workspace#install`); a lockfile change invalidates the install task and cascades to all downstream tasks. Excluding them turns a TypeScript SF computation from O(thousands of files) — every `.d.ts` in the stdlib closure — into O(actual sources).
- The path string is included alongside the hash. Two files swapping paths must be detected as a change.
- A missing file hashes as the empty buffer. The path is still in the SF, so a present-vs-absent transition is a different SF.

## Cache lookup algorithm

```rust
fn lookup(task: &Task) -> Option<CachedOutput> {
    let wf = compute_weak_fingerprint(&WeakFpInputs { ... });
    let candidates = pathset_store.get_pathsets(&wf)?;          // possibly many
    for pathset in candidates {
        let sf = compute_strong_fingerprint(&wf, &pathset.reads);
        if let Some(output) = output_store.get(&sf) {
            return Some(output);                                 // hit
        }
    }
    None                                                         // miss
}
```

Cost on a hit:

1. blake3 over the declared input globs (~ms for typical TypeScript packages).
2. JSON read of one or more pathsets.
3. blake3 over the pathset's actual file contents.
4. Filesystem read of the output_store entry.

The slow step is (3): hashing the contents of every file on the pathset. In practice, with `node_modules` excluded, this is hundreds of kilobytes of source per task — sub-millisecond on warm caches.

Cost on a miss is the cost of running the task plus a one-time pathset/output write.

## Why two phases

The naive design — "hash all declared inputs and use that as the cache key" — fails in two opposite directions:

1. **Over-declaration** (Bazel's solution): every input must be declared. Miss one → the build is wrong. The user pays correctness with declaration effort.
2. **Under-declaration** (Turborepo's default): hash only declared inputs. The system trusts you. Miss one → silent stale cache.

The two-phase scheme separates *discrimination* (the WF, cheap, runs on every lookup) from *verification* (the SF, runs only when a candidate pathset exists). The pathset is **observed**, not declared. The user only declares the WF inputs — and even those are usually the plugin's defaults — and the sandbox supplies the rest. Correctness is a property of the mechanism, not of declaration discipline.

The cost of a false WF match is a single SF computation, then a miss. The cost of a false SF match is theoretically zero — every byte that affects the output is in the SF input by construction.

## ABI fingerprint and downstream cutoff

When a plugin implements `abi_fingerprint(outputs: &[OutputFile]) -> Option<String>`, rage records the ABI hash alongside the task's outputs. Downstream tasks fold that hash into *their* WF via `dep_abi_fingerprints`.

Concrete example (TypeScript plugin):

```
packages/utils#build  outputs: dist/index.js, dist/index.d.ts
                      abi_fingerprint = blake3(all .d.ts contents)

packages/api#build    WF includes: ("packages/utils", utils.abi_fingerprint)
```

If you change a comment inside `utils/src/foo.ts` that doesn't affect the public type surface:

1. `utils#build` runs (WF changed because source changed).
2. `tsc` writes the same `.d.ts` as before.
3. `abi_fingerprint(outputs) = same hash as before`.
4. `api#build`'s WF is **unchanged**.
5. `api#build` is a cache hit despite `utils` being rebuilt.

This is the BuildXL early-cutoff mechanism. Without an ABI fingerprint, every change to `utils` would invalidate every dependent. With it, only changes that affect the public surface propagate.

ABI fingerprinting is plugin-defined and optional. Plugins that can't cheaply expose ABI return `None` from `abi_fingerprint`; the SF still carries correctness alone, just without the cutoff bonus.

## Tool binary hashing

`crates/cache/src/tool_hash.rs` resolves the tool path via `which::which_in` against the task's `PATH`, then hashes its bytes. To avoid hashing massive multi-hundred-megabyte binaries, the implementation hashes a representative window (header + size + mtime) for binaries above a threshold and the full file otherwise. The resulting hash is folded into the WF.

This is what makes a `node` upgrade or a `tsc` upgrade automatically invalidate caches. Users who pin tool versions in `package.json` get free correctness here; users who don't get a slightly weaker guarantee (the same binary path may resolve to a different binary) but the path still hashes deterministically per machine.

## Output replay

`output_store` (`crates/cache/src/output_store.rs`) keyed by SF. Each entry contains:

- The set of files the task wrote (relative paths, content-addressed).
- Captured stdout and stderr.
- Exit status.
- Sandbox metadata (mode, violation list if `observed`).

On a hit, the runner:

1. Hardlinks each captured output file into its workspace location (with EXDEV-safe copy fallback).
2. Replays stdout/stderr to the user's terminal, prefixed with the task name.
3. Synthesizes a fake completion event for the daemon's status stream.

The replay is byte-identical to the original execution from the user's point of view. There is no "skipped (cached)" shorthand that hides what the cache returned.

## Why not single-phase content hashing

A simpler design would hash every file in the package (or every file under declared globs) into the cache key directly, skipping the WF→pathset→SF dance.

Two reasons that fails:

1. **Over-hashing.** A typical TypeScript package has hundreds of files; only a handful are read by `tsc -b` for any given task. Hashing all of them per lookup makes the cache so slow that warm hits cost more than running the task.
2. **Under-discrimination.** Even hashing every file in the package misses files outside the package — `tsconfig.base.json` two directories up, `node_modules/typescript/lib/lib.dom.d.ts`. The pathset includes them; a single-phase package-only hash doesn't.

The two-phase design lets the WF be small (so lookups are cheap) and the SF be exact (so correctness is preserved).

## why-miss diagnostics

`crates/cache/src/why_miss.rs` answers the question "why did this task miss the cache?" by replaying the last successful entry's WF inputs against the current ones and diffing.

```
$ rage why-miss packages/api#typecheck

Cache miss: packages/api#typecheck
  Weak fingerprint changed because:
    declared input globs:
      src/parser.ts          content changed: prev 7af3…e2 → now 9d2a…44
      src/utils.ts           unchanged
      tsconfig.json          unchanged
    upstream ABI:
      packages/core#build    abi unchanged

  After re-running task:
    pathset stored as r/o   2 new files in pathset:
      ../../tsconfig.base.json  (added by sandbox observation)
```

This closes the gap that BuildXL's `FingerprintStore` left open: cache miss explanation is a first-class command, not a debugging-only feature.

## Remote backends

`crates/cache/src/provider.rs` is the abstraction; `local.rs`, `s3.rs`, `azure.rs` are concrete backends. The two-phase scheme is unchanged across backends — pathsets and outputs are individual content-addressed objects, fetched on demand. A spoke fetching upstream outputs from S3 issues N parallel `GetObject` calls (N = files in the pathset), not one monolithic blob.

The same S3 bucket can host the install artifact CAS and the task output cache. They share content addressing and never collide because keys are blake3 of disjoint inputs.

## Sandbox and the cache: who calls whom

The cache calls into the sandbox only when execution is required. The sandbox produces a pathset; the cache consumes it. The boundary:

```
scheduler::run_task(task)
  ├── cache.lookup(task)? → Some(output) → replay, done
  └── otherwise:
        sandbox.attach(child_process)         # mode-dependent
        run task
        pathset = sandbox.collect()
        cache.store(WF, pathset, outputs)
```

Loose mode skips the sandbox entirely. The cache then has only the WF — the SF computation is empty (no pathset). The cache key is effectively `WF` alone. This is correct only if the user trusts their declared inputs (the same trust model as Turborepo). It exists as an escape hatch for legacy packages, not as the default.

## What the cache does not do

- It does not write to the user's source tree on a hit. Hardlinks land in declared output paths only.
- It does not detect file changes after a hit. That's the daemon's job, with `notify` and stored pathsets.
- It does not GC itself. `rage gc` is the explicit reclamation command (LRU on access time, with manifest-pinning protection).
- It does not encrypt at rest. Remote backends rely on bucket-level controls.
