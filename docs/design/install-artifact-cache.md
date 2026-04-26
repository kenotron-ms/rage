# Rage — Install Artifact Cache Design

## Goal

Design how rage caches the *outputs* of root preparation tasks — `yarn install`, `pnpm install`, `cargo fetch`, `uv sync`, `go mod download` — so that a cache hit actually restores the on-disk state those commands produced, not just a marker file. The unit of caching is the **per-package artifact** (one npm tarball, one Rust crate, one Python wheel, one Go module zip), stored in a content-addressed store and materialized into the workspace by hardlink whenever possible.

This document covers the artifact store, the cache key for installs, the EcosystemPlugin trait extensions, the materialization strategy, the polyglot extension path, and a three-phase migration that lets us ship a correctness fix this week, the performance fix next month, and the polyglot/distributed story over the following quarter.

## Background — The Bug We Found

The current root-task cache is a one-line skip marker. From `crates/scheduler/src/runner.rs:631–686`:

```rust
async fn run_root_task_two_phase(task: Task, cache: Arc<TwoPhaseCache>) -> Result<(), RunError> {
    let fp = root_task_fingerprint(&task);
    let marker = cache.dir().join(format!("root-{fp}.done"));

    if marker.exists() {
        eprintln!("[rage] {}#{} ✓ (cached)", task.package_name, task.script_name);
        return Ok(());                              // restores NOTHING
    }
    // ... run the install command ...
    if status.success() {
        let _ = std::fs::write(&marker, b"");       // stores an EMPTY file
    }
}
```

The artifact stored on cache PUT is an empty file. The artifact restored on cache GET is nothing. The cache key correctly invalidates when the lockfile changes (good), but on a cache hit it assumes the *effects* of the previous install — `node_modules/`, the global pnpm store, native addons compiled by `postinstall` — are still on disk.

This produces two failure modes:

| Scenario | Current behavior | What the user sees |
|---|---|---|
| Fresh CI runner, lockfile unchanged from last cached build | "✓ (cached)", install skipped, `node_modules` doesn't exist | Next task: `tsc: command not found`, exit 127 |
| Lockfile changed | Cache miss, full `yarn install` from npm registry | 30–90s on cold, 5–15s on warm registry |

The first is a **correctness bug** — we declare a cache hit and the cached effects don't exist. The second is a **performance ceiling** — even a successful install puts nothing in the cache that helps the next machine.

This document fixes both, and does so in a way that generalizes to every other ecosystem rage will support.

## What Other Build Systems Do (Research Findings)

We surveyed Nx, Turborepo, BuildXL, and Bazel/`rules_js` to learn how they handle the install step. The headline result: **none of the JS-monorepo tools cache `node_modules` as a tar artifact**. They either skip the problem or solve it at a finer granularity.

### Nx

Does not model `pnpm install` as an Nx task at all. `nx affected` runs *after* the install. The install step is the user's responsibility (or `actions/setup-node`'s package-manager cache). The Nx computation cache stores task *outputs* (build artifacts, dist, lint reports), keyed by inputs that include the lockfile — but the lockfile is an input that *invalidates downstream tasks*, not an input to a task that produces `node_modules`.

Why fast: package manager's global store (`~/.pnpm-store`, `~/.yarn/cache`, `~/.npm/_cacache`) is warmed by `actions/setup-node`'s cache action. A "fresh" `pnpm install` against a warm store completes in ~5–15s — it's mostly hardlinking from the store into `node_modules`, with no network.

### Turborepo

Same model as Nx. Turbo tasks have inputs and outputs; install is a prerequisite, not a task. Vercel's official position is that caching `node_modules` is not recommended — the integrity guarantees from re-running the package manager are stronger than the symlink/ABI fragility of restoring an arbitrary directory.

### BuildXL

Models the install as a sandboxed pip ("prep pip") inside the build graph. The install pip's outputs go into BuildXL's content-addressed store (CAS). On cache hit: the CAS contents are **hardlinked** into the workspace. No tar, no extraction, no network. The hash key includes lockfile content, host triple, and observed file accesses — the same two-phase fingerprinting BuildXL uses for everything else.

This is the model rage should converge on. It's the only one that treats install as a first-class cacheable computation while remaining correct across machines.

### Bazel / `rules_js` (Aspect)

Goes further: bypasses `npm install` entirely. Reads the lockfile (`pnpm-lock.yaml`) inside Bazel's repository rules, downloads each package tarball through Bazel's downloader (which is itself content-addressed and CAS-deduplicated), and lays out a virtual `node_modules` lazily — only the packages a given target actually depends on get linked in.

Benchmarks from Aspect: a target needing only `uuid` materializes its `node_modules` subset in ~1s on a 3,750-package monorepo, versus ~28s for `pnpm install --offline` over the same lockfile. This is the limit case of granular caching: the cost of "preparing dependencies" scales with what the *task* uses, not what the *workspace* declares.

### What This Means for Rage

The two right answers in the literature are:

1. **BuildXL's model** — install runs once per (lockfile, host triple), outputs go to CAS, cache hits hardlink the result.
2. **Bazel/rules_js's model** — never run a generic install; lay out `node_modules` lazily from per-package CAS entries.

Rage's three-phase migration walks from the current `.done`-marker model to BuildXL's model (Phase B), then toward rules_js's lazy materialization (Phase C). Phase A is a correctness fix that lands first because it ships value before either of those is built.

The wrong answer in the literature — "tar `node_modules` and restore it" — is what most teams reach for first and is what we will explicitly **not** do. See *Why Not Tar `node_modules`* below.

## Approach

Three architectural commitments drive the design:

1. **The unit of caching is the package, not the install command.** A workspace-level `node_modules` is a derived view assembled from per-package artifacts. Caching the view is fragile; caching the parts is robust and composes across ecosystems.
2. **Content addressing or nothing.** Artifacts are keyed by a hash of their bytes. The same package version produced on two machines is the same artifact. Cross-machine deduplication, remote-cache portability, and correctness all fall out of this.
3. **The plugin owns the install lifecycle.** The scheduler does not know what `node_modules` is. The `EcosystemPlugin` trait gains capture/materialize/verify methods, and every ecosystem implements them with knowledge of its own package manager.

These commitments mirror the wider rage philosophy laid out in `docs/plans/2026-04-24-rage-daemon-config-cache-design.md`: plugins bear declaration burden, cache keys are content-addressed, the scheduler is ecosystem-agnostic.

---

## Section 1 — Three-Phase Migration

The design ships in three phases. Each phase is independently shippable. Each phase is correct on its own. Each phase is a strict superset of the previous one's capabilities.

### Phase A — Effect Verification (Correctness Fix, ~3 days)

**Problem solved:** the CI cold-machine failure where `(cached)` is reported but `node_modules` doesn't exist.

**Mechanism:** before declaring a root task cache hit, ask the plugin "are your effects still there?". If no, ignore the cache and run the install unconditionally.

**Trait surface:**

```rust
pub trait EcosystemPlugin {
    /// Verify that the on-disk effects of a successful install are still present.
    /// Called BEFORE declaring a root task cache hit.
    /// Returns false → re-run the install regardless of fingerprint match.
    ///
    /// Default returns true (preserves current behavior for plugins that don't
    /// implement it). TypeScript plugin overrides to check `node_modules/`.
    fn verify_install_effects(&self, _workspace_root: &Path) -> bool {
        true
    }
}
```

**TypeScript plugin implementation:**

```rust
fn verify_install_effects(&self, workspace_root: &Path) -> bool {
    let nm = workspace_root.join("node_modules");
    nm.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false)
}
```

**Runner integration** (`crates/scheduler/src/runner.rs`):

```rust
if marker.exists() && plugin.verify_install_effects(&task.workspace_root) {
    eprintln!("[rage] {}#{} ✓ (cached)", task.package_name, task.script_name);
    return Ok(());
}
// otherwise: run the install
```

**Storage format:** unchanged. Still a `.done` marker file. We are only fixing the GET-side correctness check.

**What Phase A does NOT solve:** the slow-cold-install problem. A cache miss (or a verify-failed cache hit) still runs `yarn install` from scratch. Phase A buys correctness; Phase B buys speed.

### Phase B — Per-Package Content-Addressed Store (Performance, ~3–4 weeks)

**Problem solved:** every other failure mode. Cache hits with missing `node_modules` restore by hardlink in milliseconds. New machines pay the install cost only for packages they don't already have in the local CAS. Lockfile-only changes invalidate only the packages whose entries actually changed.

**Mechanism:**

After a successful install, walk `node_modules/` (or pnpm's `.pnpm/` virtual store, or the cargo registry, or the venv `site-packages`), hash each file, and for each package emit:
- A **content** entry under `~/.rage/artifacts/content/{sha256}/` — the package's files, content-addressed.
- An **install state** entry under `~/.rage/artifacts/installs/{install-key}/` — a manifest mapping every file path under `node_modules/` to a `content_hash`.

On a cache hit, look up the install state by key, walk the manifest, and `hard_link()` each file from `content/` into the workspace. No `yarn install`. No network. No extraction. Same inode as the CAS file — zero copy.

**Storage layout:**

```
~/.rage/artifacts/
├── content/
│   └── {sha256}/                       ← content-addressed file blobs
│       └── data                         ← raw bytes of the file
└── installs/
    └── {install-key}/                  ← keyed by lockfile + host triple
        ├── state.json                   ← package list + provenance
        └── manifest.json                ← {file_path → {content_hash, mode}}
```

A worked example for a small workspace:

```
~/.rage/artifacts/
├── content/
│   ├── 7af3...e2/data                   ← @types/node/index.d.ts contents
│   ├── b91c...01/data                   ← typescript/lib/tsc.js contents
│   └── ... (one per unique file)
└── installs/
    └── 4c8a...d3/                       ← install_key for this lockfile+host
        ├── state.json
        │     {
        │       "ecosystem": "npm",
        │       "lockfile_path": "yarn.lock",
        │       "lockfile_hash": "...",
        │       "host_triple": "aarch64-apple-darwin",
        │       "node_abi": "115",
        │       "captured_at": "2026-04-25T19:00:00Z",
        │       "packages": [
        │         { "name": "typescript", "version": "5.4.2",
        │           "files": 247, "bytes": 38291204 }
        │       ]
        │     }
        └── manifest.json
              {
                "node_modules/typescript/package.json": {
                  "content_hash": "9d2a...44", "mode": 33188
                },
                "node_modules/typescript/lib/tsc.js": {
                  "content_hash": "b91c...01", "mode": 33188
                },
                "node_modules/.bin/tsc": {
                  "symlink_target": "../typescript/bin/tsc"
                }
              }
```

Symlinks are recorded as `symlink_target` instead of `content_hash`. This matters: pnpm's virtual store is *all* symlinks, and tarring them is the precise failure mode that breaks cross-machine restore. Capturing the symlink graph as data preserves it correctly.

**Materialization is hardlink-first, copy-on-fallback:**

```rust
fn link(cas_path: &Path, target: &Path) -> Result<()> {
    if let Err(e) = std::fs::hard_link(cas_path, target) {
        if is_cross_device(&e) {
            std::fs::copy(cas_path, target)?;          // EXDEV fallback
        } else {
            return Err(e.into());
        }
    }
    Ok(())
}
```

Hardlinks share inodes. Restoring a 200,000-file `node_modules` takes ~50–200ms because no bytes are copied; only directory entries are written. Cross-device fallback (a workspace on one volume, the CAS on another) degrades gracefully to copy without losing correctness.

**What Phase B preserves from Phase A:** `verify_install_effects` is still called on the GET path. If the manifest says a file should exist at `node_modules/foo/bar.js` and it doesn't (or its `content_hash` no longer matches the CAS), we treat the install as not-cached and re-run. Defense in depth — never trust cache state to be intact without checking.

### Phase C — Lazy Materialization & Remote CAS (Polyglot/Distributed, ~6+ months)

**Problems solved:**

- **Cross-machine reuse.** Local CAS hits are good; remote CAS hits eliminate the install cost across an entire team.
- **Per-target cost.** A task that only needs `uuid` shouldn't materialize all 3,750 packages. Match Bazel/rules_js performance by only laying out the subset of `node_modules` that the running task actually depends on.
- **Postinstall hermeticity.** Native addons compiled by `postinstall` scripts are platform-specific and not currently visible to the cache as a separate computation. Phase C captures them as their own cacheable build steps.

**Three additions on top of Phase B:**

1. **Remote `ArtifactStore` backends.** The trait in Phase B is implemented by `LocalArtifactStore`. Phase C adds `S3ArtifactStore`, `AzureArtifactStore`. Per-package granularity means we upload/download single packages, not monolithic archives — exactly the right size for a CDN-backed object store.

2. **Lazy materialization.** When the scheduler runs `packages/foo#build`, it knows from the dependency graph which npm packages the task transitively needs. Materialization restores only those. Pure Bazel-style: the install becomes a no-op for the workspace, replaced by per-task `node_modules` views assembled on demand.

3. **Postinstall as separate cacheable steps.** Currently postinstall runs *during* `yarn install` and its outputs are entangled with the install artifact. Phase C runs each package's postinstall under the sandbox as its own pip, with its own cache key (`package_content_hash + host_triple + node_abi`). Outputs are cached independently and replayed without re-execution.

The migration table:

| | Phase A | Phase B | Phase C |
|---|---|---|---|
| Cache key | lockfile + env | lockfile + host_triple + node_abi + env | same |
| Storage | `.done` marker | per-package CAS, install manifest | per-package CAS + remote backend |
| GET on hit | skip install (verify only) | hardlink from local CAS | hardlink from local, fetch missing from remote |
| Cold-machine cost | full `yarn install` | first time: full install; subsequent: hardlink | one-time download per package across all CI runners |
| Postinstall | bundled into install | bundled into install | separate cacheable step |
| Per-task scope | whole workspace | whole workspace | only declared dependencies |

Each row's value in a later column is a strict improvement over the prior column. There is no Phase B regression versus Phase A; there is no Phase C regression versus Phase B. The migration can stop after any phase if priorities shift.

---

## Section 2 — Cache Key Design

The cache key for an install determines the cache directory. Two installs with the same key are *interchangeable* outputs. Two installs with different keys are *independent* outputs. The key must therefore include every input that can change the output without exception — and exclude inputs that don't, so we don't fragment the cache.

### The Install Key

```
install_key = blake3(
    domain_separator    = b"rage.install.v1\0",
    ecosystem           = b"npm" | b"cargo" | b"python" | b"go",
    lockfile_content    = full bytes of every file returned by install_lockfiles(),
    host_triple         = target_lexicon::HOST.to_string(),
    node_abi_version    = if install_requires_host_abi() then process::abi else b"",
    env_hash_inputs     = sorted (key,value) pairs from RootTask.env_hash_inputs,
)
```

Each ingredient in detail:

| Ingredient | Why included | Failure if omitted |
|---|---|---|
| `domain_separator` | Prevents collision with other rage hash schemes | Cosmic; defense-in-depth |
| `ecosystem` | Same lockfile bytes could in principle collide across ecosystems | Two ecosystems share a key → catastrophic mix-up |
| `lockfile_content` | The thing that says what to install | Lockfile change → false cache hit → wrong packages |
| `host_triple` | Native modules are platform-specific | Linux artifact restored on macOS → ABI errors |
| `node_abi_version` | Same OS, different Node major → addons unloadable | Node 18 → 20 upgrade → `MODULE_VERSION` errors |
| `env_hash_inputs` | Some installs are env-conditional (e.g. `NODE_ENV=production` skips devDeps) | Mode-confused install; false hit |

#### Why host_triple, always

Even if we *think* this lockfile has no native modules, we don't know what postinstall scripts do. A pure-JS package's postinstall could be `node download-prebuilt-binary.js`, and the resulting binary is host-specific. We cannot prove the absence of native code from package metadata. Including `host_triple` unconditionally is correct-by-default; the fragmentation cost (one cache entry per platform) is negligible relative to the safety win.

The symmetric question — should we ever skip `host_triple`? — is addressed by `install_requires_host_abi()`. A future plugin (pure Python wheels, pure Go modules cross-compiled for a fixed target) can return `false` and explicitly opt out. The default is `true`.

#### Why a content hash, not a path/mtime

Lockfiles are checked into the repo. Their content is the input. Path-based or mtime-based keying breaks when a worktree is created with `git checkout`, when CI clones into a new directory, when rsync changes mtimes. Hashing bytes is the only stable approach.

### The Content Hash

Each file in the captured artifact is keyed by:

```
content_hash = sha256(file_bytes)
```

Why sha256, not blake3:
- npm publishes packages with `integrity: "sha512-..."` — sha-family is the ecosystem norm.
- Cargo's `.crate` files are fingerprinted with sha256 in the registry index.
- PyPI exposes per-file sha256 as `digests`.
- We can in many cases use the package manager's *already-computed* sha256 instead of recomputing.

A future optimization: when capturing an npm package whose manifest has an `integrity` field, prefer it over recomputation when available. This makes the capture path nearly free relative to the install itself.

### Pathset Note (Out of Scope)

The two-phase fingerprinting in `crates/cache` (WF → pathset → SF) applies to *task* caching, not install caching. Installs are deterministic functions of their lockfile + host — there is no observed pathset to record. The install cache is a single-phase content-addressed lookup; the WF/SF dance does not apply.

---

## Section 3 — EcosystemPlugin Trait Extensions

The current `EcosystemPlugin` trait (`crates/plugin/src/lib.rs`) defines task inference, fingerprint inputs, ABI fingerprints, and root tasks. It does not define how a root task's *outputs* are captured or restored. This section adds those methods.

The trait grows by **five methods**, all with sensible defaults so existing plugin implementors keep compiling.

```rust
use std::path::{Path, PathBuf};

pub trait EcosystemPlugin: Send + Sync {
    // ... existing methods (id, detection_globs, infer_tasks, ...) unchanged ...

    /// Lockfiles that determine the cache key of this ecosystem's install.
    /// Their *contents* are hashed into `install_key`.
    ///
    /// Examples:
    ///   - npm/yarn/pnpm: vec!["yarn.lock"] | vec!["pnpm-lock.yaml"] | vec!["package-lock.json"]
    ///   - cargo:        vec!["Cargo.lock"]
    ///   - uv:           vec!["uv.lock"]
    ///   - go modules:   vec!["go.sum"]
    ///
    /// Plugin returns paths it expects to exist; missing files contribute zero bytes.
    /// If the vec is empty, this ecosystem has no cacheable install.
    fn install_lockfiles(&self, _workspace_root: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Whether install outputs are host-ABI-specific.
    ///
    /// `true` (default, safe):
    ///   - npm: postinstall scripts may compile native addons (sharp, esbuild, bcrypt)
    ///   - cargo: compiled crates always target a triple
    ///   - python with C extensions
    ///
    /// `false` (opt-in, fast):
    ///   - pure-Python wheels (manylinux, abi3)
    ///   - go cross-compile pipelines (the install IS the download, host doesn't matter)
    ///
    /// When false, host_triple and node_abi are excluded from install_key, allowing
    /// cross-platform reuse. Get this wrong and you'll get cryptic ABI errors at runtime.
    fn install_requires_host_abi(&self) -> bool {
        true
    }

    /// Verify the on-disk effects of a successful install are still present.
    /// Called BEFORE declaring an install cache hit.
    ///
    /// Returns false → re-run install regardless of fingerprint.
    /// Default returns true (matches today's behavior for non-overriding plugins).
    fn verify_install_effects(&self, _workspace_root: &Path) -> bool {
        true
    }

    /// (Phase B) After a successful install, capture the installed packages
    /// into the artifact store and produce an `InstallManifest` that maps
    /// every workspace-relative file path back to a content hash.
    ///
    /// The default implementation returns an empty manifest — meaning Phase A
    /// behavior (no per-package storage). Plugins opt in to Phase B by overriding.
    fn capture_install(
        &self,
        _workspace_root: &Path,
        _store: &dyn ArtifactStore,
    ) -> Result<InstallManifest, CaptureError> {
        Ok(InstallManifest::empty())
    }

    /// (Phase B) Restore a captured install. Called when install_key hits in
    /// the artifact store but the workspace's effects (per verify_install_effects)
    /// are missing or stale.
    ///
    /// Implementations should hardlink files from the store via
    /// `store.link(content_hash, target_path)` and fall back to copy on EXDEV.
    /// Symlinks listed in the manifest are recreated as symlinks.
    fn materialize_install(
        &self,
        _workspace_root: &Path,
        _manifest: &InstallManifest,
        _store: &dyn ArtifactStore,
    ) -> Result<(), MaterializeError> {
        Ok(())
    }
}
```

**Why defaults that no-op:** plugins not yet upgraded for Phase B continue to work — they fall through to the Phase A path (effect verification, full install on miss). The trait remains backward-compatible. A plugin's "Phase B readiness" is observable by whether `capture_install` returns a non-empty manifest.

**Why these are plugin methods, not scheduler logic:** the scheduler does not know what `node_modules/` is, where `~/.cargo/registry/` lives, or that pnpm uses a virtual store. Embedding that knowledge in the scheduler would have to grow with every new ecosystem and would fail the polyglot test. Each plugin owns its own filesystem layout.

---

## Section 4 — The ArtifactStore Trait

A new crate `crates/artifact-store` provides the content-addressed storage primitive. It is parallel in role to `crates/cache` (which handles task output caching) but content-addressed at the per-file level.

```rust
use std::path::Path;

/// Content hash. 32-byte sha256 (or blake3 — choose at design time, then commit).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn hex(&self) -> String { /* lowercase hex */ }
    pub fn from_bytes(bytes: &[u8]) -> Self { /* sha256 */ }
}

/// Errors omitted for brevity. Each method is fallible.
pub trait ArtifactStore: Send + Sync {
    /// Insert content. Returns its hash. Idempotent: inserting the same bytes
    /// twice returns the same hash and is a no-op on the storage layer.
    fn put(&self, content: &[u8]) -> Result<ContentHash>;

    /// Retrieve content by hash. None if absent.
    fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;

    /// Materialize content to `target` by hardlink (preferred) or copy (fallback).
    /// Creates parent directories as needed. Sets file mode from `mode`.
    fn link(&self, hash: &ContentHash, target: &Path, mode: u32) -> Result<()>;

    /// Cheap existence check — does NOT read content.
    fn contains(&self, hash: &ContentHash) -> bool;

    /// Optional bulk pre-fetch hint for remote stores. Local store is a no-op.
    fn prefetch(&self, _hashes: &[ContentHash]) -> Result<()> { Ok(()) }
}
```

### Backends

**`LocalArtifactStore` (Phase B):**

```rust
pub struct LocalArtifactStore {
    root: PathBuf,    // e.g. ~/.rage/artifacts/content/
}
```

`put` writes to `{root}/{hex_hash}/data`, atomic via tmp-file + rename. `link` does `std::fs::hard_link` with copy fallback on EXDEV. `contains` is a `Path::exists` check. No locking required for the read path; writes are content-addressed so concurrent puts of the same hash are safe (last writer wins on identical bytes).

**`S3ArtifactStore` and `AzureArtifactStore` (Phase C):**

Implement the same trait against object stores. `put` is a `PutObject`; `get` is a `GetObject`; `link` downloads to local CAS first, then hardlinks. `contains` is a `HeadObject`. The same content-addressing means a package uploaded by CI runner A is verbatim usable by CI runner B — no re-encoding, no synchronization.

**Compositional `MultiTierArtifactStore` (Phase C):**

A wrapper that chains a local L1 over a remote L2: read-through (try L1, fall back to L2 + populate L1), write-through (write L1 and L2 in parallel). Same trait, same caller code. The scheduler doesn't know or care that there's a remote tier.

### Why a Separate Crate

`crates/cache` is about task fingerprinting and output replay. `crates/artifact-store` is about per-file content addressing. They share concepts (CAS, hashing) but have different APIs and responsibilities. Mixing them muddles which layer owns what.

In Phase C the two converge at the deployment level: the same S3 bucket holds task outputs (from `crates/cache`'s remote backend, per Phase 6 of `2026-04-25-phase6-remote-cache.md`) and per-package install artifacts (from `crates/artifact-store`'s S3 backend). One credential, one bucket, one operational story. Two crates, because they have different correctness properties internally.

---

## Section 5 — Materialization Strategy

Restoring a captured install means writing files to `node_modules/` (or the equivalent). There are three plausible strategies, in increasing order of complexity.

### Strategy 1 — Hardlink (default)

```rust
std::fs::hard_link(&cas_path, &workspace_path)?;
```

- Same inode as the CAS file. Zero bytes copied.
- Fast: ~50ns per file → 200k files in ~10ms of syscall time, dominated by directory creation.
- Restriction: must be on the same filesystem.
- Caveat: writes through the workspace `node_modules/foo/index.js` will edit the CAS copy too (it's the same inode). This is a footgun if a tool decides to mutate `node_modules` in place. Mitigation: capture the file mode without write bits where possible (`0o444`), or detect the corruption on next verify and re-materialize.

### Strategy 2 — Copy (fallback)

When hardlink fails with EXDEV (cross-device): `std::fs::copy`. Slower (~200MB/s sustained on modern SSDs) but always available. Phase B selects copy when the workspace and CAS are on different filesystems — common in containerized CI where `/workspace` is a volume mount but `~/.rage` is on the container's overlay.

### Strategy 3 — Reflink / clonefile (future)

On filesystems that support copy-on-write (APFS via `clonefile`, btrfs/xfs via `FICLONE` ioctl, ZFS via clone), reflinks give the speed of hardlinks plus the safety of copies — writes diverge instead of corrupting the source. This is the ideal when available. Implementation deferred until Phase B is shipping, then added behind an automatic detection at store-init time.

```
Filesystem detection priority:
  1. Hardlink (if same FS, no clonefile available)
  2. clonefile/reflink (if same FS + CoW supported)
  3. Copy (cross-FS or unsupported)
```

### Strategy 4 — Lazy symlinks (Phase C)

Like pnpm: the CAS holds canonical package directories, the workspace holds a symlink graph that views them. Materialization becomes "create symlinks." The `node_modules/.pnpm/` virtual store is exactly this pattern — rage's Phase C generalizes it across ecosystems.

This is more invasive: the workspace `node_modules/` is no longer a normal directory tree, it's a managed symlink view, and tools that walk it must be okay with symlinks (most are; some old build tools aren't). Reserved for Phase C when the per-task laziness payoff justifies the complexity.

---

## Section 6 — Postinstall Scripts and Other Hazards

Postinstall scripts are the most operationally messy part of the npm ecosystem, and the design has to confront them honestly.

### What postinstall does

`npm install` runs a package's `postinstall` script after extraction. Common uses:
- Compile native addons (`sharp`, `bcrypt`, `node-gyp` flows).
- Download platform-specific binaries (`puppeteer` → Chromium, `playwright` → browsers).
- Patch the package source (rare but observed).
- Touch files outside `node_modules` (very rare, but non-zero in the wild).

### Why this matters for caching

If we capture `node_modules/` after a successful install, postinstall outputs are *in there*. Two issues:

1. **Platform specificity.** A native addon compiled on `aarch64-apple-darwin` won't load on `x86_64-linux-gnu`. Restoring it cross-platform is a runtime crash.
2. **Out-of-tree side effects.** A postinstall that writes to `~/.cache/playwright/` is invisible to the install artifact. Restoring the artifact on a fresh machine means the side-effect file is missing and `playwright` fails at runtime.

### Design response

**Phase B** addresses (1) with `host_triple` + `node_abi_version` in `install_key`. A macOS-arm64 capture is keyed differently from a Linux-x64 capture. They can't collide. They get re-run on each platform the first time, then cached per-platform from then on.

**Phase B does NOT solve (2).** Out-of-tree side effects are still unsafe to assume cached. The mitigation is to declare that rage's install cache is *workspace-scoped*: anything outside `node_modules/` (or the ecosystem's equivalent) is not under our cache umbrella. Tools that put state in `~/.cache/` are expected to handle their own cold-start case. Most do — `playwright`'s startup checks for the binary and downloads if missing. The ones that don't are bugs upstream.

**Phase C** addresses (2) by running each postinstall under the sandbox as a separate cacheable pip:

```
For each package P in the install:
    1. Materialize P's source files from CAS (hardlink, fast).
    2. If P has a postinstall script:
         a. Compute postinstall_key = hash(content_of(P) + host_triple + node_abi).
         b. Look up postinstall_key in cache. If hit, replay observed outputs. Done.
         c. Else, run postinstall under sandbox; record outputs (in-tree AND
            out-of-tree); store under postinstall_key.
```

This is the BuildXL "prep pip" model applied at per-package granularity. Postinstall outputs that touch `~/.cache/playwright/` get captured by the sandbox's read/write logging and are restored as part of the cache replay. Out-of-tree side effects become first-class cacheable.

Phase C is hard. It requires sandbox-observed I/O (which rage's `crates/sandbox` provides) and a bookkeeping layer that maps `postinstall_key → output_set`. We defer it until Phase B is in production and we have data on which postinstalls actually matter.

### Honest deferral

For Phases A and B, the rage install cache will be *correct* for any package that confines its effects to `node_modules/` (the vast majority) and *incorrect* for packages that touch out-of-tree state on first install but assume that state survives. We accept this gap, document it, and plan to close it in Phase C. This is the same gap every other build cache has; we are not making it worse, and we are committing to fixing it where others have not.

---

## Section 7 — Why Not Tar `node_modules`?

The instinctive answer to "install is slow, cache its output" is "tar the output directory." This section documents why that approach is wrong, so we don't accidentally rebuild it under a different name.

### Reason 1 — Symlinks don't survive

pnpm's virtual store is symlinks. `tar` can preserve them, but cross-platform behavior is uneven (Windows tar implementations notoriously mishandle them; macOS BSD tar versus GNU tar differ on absolute vs relative targets). Restoring a tarball produced on Linux onto macOS produces a `node_modules/` that *looks* fine but has subtle symlink target failures that surface in obscure runtime errors hours later.

### Reason 2 — Size

A typical Lage workspace has a `node_modules/` of 500MB–2GB compressed. Storage cost: trivial. Network cost on a remote cache: not trivial. Worse: changing one dependency invalidates the entire tarball. Per-package CAS only invalidates the changed package's entry — orders of magnitude less data churn.

### Reason 3 — ABI fragility

A tarball captured on `aarch64-apple-darwin` contains darwin-arm64 native addons. Untar it on `x86_64-linux-gnu` and the next `require('sharp')` segfaults. We'd have to gate the tarball by host_triple anyway — and once we do, the per-package CAS is strictly better at the same correctness level.

### Reason 4 — Postinstall side effects

(See Section 6.) A tarball captures only `node_modules/`. Postinstall side effects to `~/.cache/playwright/` are silently dropped. The workspace looks restored; the tooling fails opaquely.

### Reason 5 — Update granularity

Add one dependency to `package.json`: tarball changes from 1.4GB to 1.41GB, but the *content* is 99% identical. A naive tar cache uploads the full 1.41GB. A delta-aware tar cache (xdelta/zstd dictionary) helps, but requires intricate state. Per-package CAS naturally handles this: 99% of package hashes are unchanged, only the new package's hash is new.

### Reason 6 — Operational debugging

When a tar restore fails, the debug surface is "the entire untar." When a per-package CAS restore fails, the debug surface is "this single package failed verification, here's its hash, here's what was expected, here's what's on disk." The CAS model makes per-package failures observable; the tar model makes them indistinguishable.

### What about a hybrid?

"Per-package CAS for fast paths, tarball as a fallback for first-time hits." Tempting, but it doubles the storage layer (tarballs *and* CAS) for marginal speedup, and the speedup vanishes once the local CAS is warm. Reject as YAGNI.

---

## Section 8 — Polyglot Extension Path

The design is shaped for JavaScript first because that's what's broken right now. But the trait, the cache key, and the artifact store generalize. This section sketches each ecosystem's mapping.

### JavaScript / TypeScript (Phase B target)

| Concern | Mapping |
|---|---|
| Lockfiles | `yarn.lock` ∣ `pnpm-lock.yaml` ∣ `package-lock.json` |
| Install command | `yarn install` ∣ `pnpm install --frozen-lockfile` ∣ `npm ci` |
| Captured tree | `node_modules/` (yarn/npm) or `node_modules/.pnpm/` + symlink graph (pnpm) |
| Per-package unit | One npm package version (`react@18.3.1`) |
| Native ABI? | Yes — `install_requires_host_abi() = true` |
| Side effects | Postinstall scripts, sometimes `~/.cache/{tool}/` |
| Materialization | Hardlink in flat layout; symlink+hardlink for pnpm virtual store |

### Rust (future plugin)

| Concern | Mapping |
|---|---|
| Lockfiles | `Cargo.lock` |
| Install command | `cargo fetch --locked` |
| Captured tree | `~/.cargo/registry/src/index.crates.io-{hash}/{crate}-{version}/` |
| Per-package unit | One crate version (`serde@1.0.197`) |
| Native ABI? | Crate sources are platform-independent; build artifacts (target/) are not. The *fetch* step is host-independent. → `install_requires_host_abi() = false` |
| Side effects | None (cargo fetch is hermetic) |
| Materialization | Hardlink each crate dir into the cargo registry path |

Note: `cargo build`'s output is *not* covered by the install cache. That's a downstream task and goes through `crates/cache`'s normal task-output caching. The install cache covers only the `cargo fetch` step.

### Python / uv (future plugin)

| Concern | Mapping |
|---|---|
| Lockfiles | `uv.lock` ∣ `requirements.txt` (with hashes) |
| Install command | `uv sync` ∣ `pip install -r requirements.txt` |
| Captured tree | `.venv/lib/python3.x/site-packages/` |
| Per-package unit | One wheel (`numpy-1.26.4-cp311-cp311-manylinux_2_17_x86_64.whl`) |
| Native ABI? | Mixed — pure-Python wheels (abi3, none) are portable; cp311-specific wheels are not. Plugin must inspect wheel tags. |
| Side effects | None for pure wheels; some wheels run setup.py with side effects |
| Materialization | Extract wheel contents into site-packages; or hardlink pre-extracted |

The wheel tag ABI awareness is a nontrivial design point: a Python plugin would override `install_requires_host_abi` to return `false` for pure-abi3 environments and `true` otherwise, possibly per-package. This may motivate a future trait method `install_abi_dimension()` returning a string keyed into the install_key, finer-grained than the binary host/no-host switch.

### Go (future plugin)

| Concern | Mapping |
|---|---|
| Lockfiles | `go.sum` (+ `go.mod`) |
| Install command | `go mod download` |
| Captured tree | `$GOPATH/pkg/mod/cache/download/` and `$GOPATH/pkg/mod/{module}@{version}/` |
| Per-package unit | One module version (`github.com/foo/bar@v1.2.3`) |
| Native ABI? | No — sources only. → `install_requires_host_abi() = false` |
| Side effects | None |
| Materialization | Hardlink module dir into the module cache path |

### What stays the same across all of them

The trait. The cache key shape. The artifact store. The materialization strategy. The verify_install_effects pattern. The polyglot story is "implement the trait for your ecosystem and you get all of rage's caching machinery for free." This is exactly the abstraction win the project is built around.

---

## Section 9 — Runner Integration

This section walks through the `run_root_task_two_phase` rewrite for Phase B. Phase A is a strict subset of this — same control flow without the artifact-store calls.

```rust
async fn run_root_task_with_artifact_cache(
    task: Task,
    plugin: Arc<dyn EcosystemPlugin>,
    cache: Arc<TwoPhaseCache>,           // unchanged — still keys the marker
    store: Arc<dyn ArtifactStore>,       // new
) -> Result<(), RunError> {
    let install_key = compute_install_key(&plugin, &task)?;
    let install_dir = artifact_root().join("installs").join(install_key.hex());

    // ── GET path ────────────────────────────────────────────────────────────
    if install_dir.exists() {
        let manifest = read_install_manifest(&install_dir)?;

        // Defense in depth: even with a manifest hit, verify before trusting.
        if plugin.verify_install_effects(&task.workspace_root) {
            // The workspace already has the effects (warm developer machine
            // case). Nothing to do.
            eprintln!("[rage] {}#{} ✓ (cached, effects present)", ...);
            return Ok(());
        }

        // Cache hit, effects missing. Restore from CAS.
        eprintln!("[rage] {}#{} restoring from artifact cache...", ...);
        let start = Instant::now();
        plugin.materialize_install(&task.workspace_root, &manifest, &*store)?;
        eprintln!("[rage] {}#{} ✓ restored {} ms", ..., start.elapsed().as_millis());
        return Ok(());
    }

    // ── EXECUTE path ────────────────────────────────────────────────────────
    eprintln!("[rage] {}#{} starting (artifact cache miss)", ...);
    let start = Instant::now();
    run_install_command(&task).await?;     // existing yarn/pnpm/npm invocation

    // ── PUT path ────────────────────────────────────────────────────────────
    let manifest = plugin.capture_install(&task.workspace_root, &*store)?;
    write_install_manifest(&install_dir, &manifest)?;

    eprintln!("[rage] {}#{} ✓ {:.2}s", ..., start.elapsed().as_secs_f64());
    Ok(())
}
```

Key properties:

- **Phase A behavior** is preserved exactly: if `install_dir.exists()` (the manifest exists from a prior install) and `verify_install_effects` returns true, we return immediately without touching the store. Same fast path as today.
- **Phase B's new fast path** is the "manifest hit, effects missing" case: we materialize from CAS instead of re-running install. This is the cold-CI case.
- **Phase B's PUT** runs after a successful install, capturing the result. If `capture_install` fails, the install still succeeded and the workspace is fine — we just don't get a future cache hit. Capture errors are logged but never failed-out.

### Failure modes the runner handles

| Failure | Response |
|---|---|
| `compute_install_key` IO error (lockfile missing) | Run install unconditionally (treat as miss) |
| `read_install_manifest` corrupt JSON | Treat as miss, blow away install_dir, run install |
| `materialize_install` fails partway | Attempt to clean up partially restored tree, run install |
| `capture_install` fails after successful install | Log warning, return Ok — install succeeded, cache not populated |
| Concurrent run by another rage process | Both write the same content_hash files (idempotent); manifest dir uses tmp+rename for atomicity |

### Concurrency

The local CAS is naturally concurrency-safe: writes are content-addressed, the same bytes produce the same path, and either-writer-wins on a race. The install_dir is written via a tmp directory + atomic rename, so partial writes are never observed. Two parallel rage runs against the same workspace racing on `yarn install` is a pre-existing issue that this design does not change (and arguably makes worse — both will try to capture). Mitigation: a workspace-level `.rage/install.lock` file is in scope for Phase B.

---

## Section 10 — Configuration

Keep `rage.json` minimal. Most users should never touch this section.

```jsonc
{
  "install_cache": {
    // Strategy:
    //   "marker"   — Phase A: skip-marker only (legacy default)
    //   "cas"      — Phase B: per-package CAS, local
    //   "lazy-cas" — Phase C: lazy materialization + remote
    "strategy": "cas",

    // Phase A & B & C: verify on-disk effects before declaring cache hit.
    // Default true. Setting false is a footgun and is logged as a warning.
    "verify_effects": true,

    // Phase B & C: where the local CAS lives.
    "artifact_dir": "~/.rage/artifacts",

    // Phase B & C: storage backend. "local" is the default.
    // Phase C adds "s3" and "azure", configured the same way as the existing
    // remote cache (sharing credentials and bucket where possible).
    "backend": "local"
  }
}
```

### Defaults

- New installs of rage default to `strategy: "marker"` until Phase B ships, then to `"cas"` once Phase B is the default in a release.
- `verify_effects: true` is the default for all strategies including `marker` — this is the Phase A correctness fix.
- `artifact_dir` defaults to `~/.rage/artifacts/` and may be overridden for systems where `$HOME` is not writable.
- `backend: "local"` is always the default. Remote backends require explicit configuration and credentials.

### Migration

Phase A ships and silently upgrades existing installs to `verify_effects: true`. No config change required for existing users.

Phase B ships with `strategy: "cas"` as the new default. Existing users on `strategy: "marker"` retain that behavior; they opt in to CAS by setting the field or by `rage --upgrade-config`.

Phase C ships with remote backend support. No default change to local users.

---

## Section 11 — Performance Targets

From the research benchmarks, the speed hierarchy we are targeting:

| Scenario | Time | Source |
|---|---|---|
| Phase B local CAS hit, effects missing → hardlink restore | 50–200ms | Hardlink syscall budget for ~200k files |
| Phase B local CAS hit, effects present → no-op | <10ms | `verify_install_effects` only |
| Phase A miss / Phase B miss, warm package manager store | 5–15s | Standard `yarn install --offline` performance |
| Phase B miss, cold package manager store | 30–90s | Network-bound npm registry pull |
| Phase C remote CAS hit (cold local, warm remote) | 1–5s | Per-package S3 fetches + hardlink |
| Phase C lazy materialization (single target needs ~10 packages) | <1s | Only restores the dependency closure of the running task |

Phase B's central performance claim: **the cold CI machine that today pays 30–90s for `yarn install` will pay 50–200ms after Phase B ships**, so long as the lockfile and host triple match a previously-cached install. This is a 200×–500× speedup on the path that matters most operationally.

Phase C's claim is harder to summarize because it depends on what fraction of `node_modules` a given task actually uses. The Aspect benchmarks suggest 1s for tasks that use single-digit numbers of packages; we expect similar.

---

## Section 12 — Testing Strategy

- **Phase A** — `verify_install_effects` correctness: build a workspace, install, delete `node_modules`, run rage, assert install is re-executed despite marker presence. Inverse: with `node_modules` intact, assert no re-execution.
- **Phase B unit** — `LocalArtifactStore` round-trip: put bytes, get hash, link to target, assert hardlink shares inode (`fs::metadata().ino()` equality on Unix). EXDEV fallback test using a tmpfs mount on Linux CI.
- **Phase B integration** — full install/capture/materialize cycle on a small fixture workspace (5 npm packages). After capture: blow away `node_modules`, materialize from CAS, assert tree byte-equal to the original.
- **Phase B symlink fidelity** — a pnpm fixture: capture, blow away, materialize, assert symlinks are recreated (not dereferenced into copies) and point to the right targets.
- **Cache key correctness** — install_key computation under input variations: lockfile change → different key, host_triple change → different key, env_hash_inputs change → different key, file timestamp/path change → SAME key (content-addressed).
- **Defense in depth** — corrupted `manifest.json` → treated as miss; partial workspace restore (some files exist, some don't) → `verify_install_effects` returns false → re-install.
- **Plugin compatibility** — a plugin that doesn't override `capture_install` falls through to Phase A behavior cleanly. No type errors. No runtime panics. No performance regression.
- **Postinstall** — fixture with a postinstall script that compiles a native addon: capture, materialize on the same host, assert addon loads. Cross-host materialization (mock the host_triple): assert install_key differs and re-runs install.
- **Concurrent runs** — two rage processes installing the same workspace simultaneously. Assert no corruption, both succeed, only one capture wins (or both succeed atomically).
- **Phase C remote backend** — same integration tests against an S3-backed `MultiTierArtifactStore` with a mock S3 (minio/localstack).

---

## Section 13 — Open Questions

1. **Hash function — sha256 or blake3?** sha256 aligns with npm/cargo/PyPI conventions and lets us reuse package-manager-computed hashes verbatim. blake3 is faster and aligns with existing rage hashes in `crates/cache`. The decision is a tradeoff between integration-friction and computational cost. Lean: sha256 for content hashes (interop), blake3 for the install_key (internal), explicitly document the boundary.

2. **pnpm virtual store mapping.** pnpm's `node_modules/.pnpm/{name}@{version}/node_modules/{name}/` layout has a unique symlink graph. Capturing it correctly requires walking both the virtual store and the package's actual symlinks at the workspace root. Implementation strategy needs prototyping before committing to a manifest schema.

3. **Concurrent install protection.** Two `rage` runs in the same workspace racing on `yarn install` is a pre-existing issue. A workspace-level `.rage/install.lock` is the obvious fix. Scope: in or out of this design?

4. **Wheel ABI granularity (Python).** Pure-abi3 wheels and version-specific wheels coexist in the same install. Does the plugin override `install_requires_host_abi` at workspace level (coarse) or per-package level (precise)? The coarse path is simpler; the precise path matches Python reality. Defer to the Python plugin's design phase.

5. **GC of the local CAS.** `~/.rage/artifacts/content/` grows monotonically. We need a GC pass (LRU by access time? mark-and-sweep from referenced manifests?). Phase B can ship without GC; users rerun with `rage gc` to reclaim. Phase C should automate it.

6. **Remote CAS authentication for `rules_js`-style lazy materialization.** When a task on a CI runner needs to fetch 500 individual packages from S3 in parallel, do we need request batching, S3 directory bucket optimization, or a per-job credential cache? Performance work for Phase C, not blocking.

7. **`rage why-cold-install` command.** Symmetric to `rage why-miss` for tasks. When an install runs unexpectedly, the user should be able to ask why: lockfile differs, host_triple differs, manifest corrupted, etc. UX work for Phase B+.

8. **Cross-OS portability of capture for npm.** When a postinstall produces a different `node_modules` shape on macOS vs Linux (some optional native deps installed only on Linux), the install_key correctly differs but the manifest schema needs to gracefully handle "this file exists on Linux capture, doesn't exist on macOS capture." Per-platform manifests under the same install_key prefix? Investigate during Phase B implementation.

9. **Interaction with hub/spoke distributed builds.** Phase C's remote CAS is naturally what spokes pull from. But should the hub coordinate per-task install materialization (push the dependency closure to each spoke before scheduling), or should each spoke pull lazily on first use? Lazy is simpler; pushed is faster. Investigate during Phase C, after the hub itself is in production.

10. **Telemetry.** The cache hit rate of the install cache is a key health metric. What goes in `telemetry`'s schema? Hit/miss counts, hardlink-vs-copy ratios, average restore time, capture sizes. Define alongside the existing task-cache telemetry.

---

## Summary

The current `workspace#install` cache is an empty marker file that fails on cold CI machines and offers no speedup beyond skipping a single fingerprint comparison. The fix is a three-phase migration:

- **Phase A** adds `verify_install_effects` to the plugin trait, immediately fixing the "cached but broken" CI failure mode in ~3 days of work.
- **Phase B** introduces a per-package content-addressed artifact store and `capture_install`/`materialize_install` plugin methods, replacing the marker file with hardlink-restorable artifacts. Cold-CI install times drop from 30–90s to 50–200ms.
- **Phase C** generalizes the artifact store to remote backends (S3, Azure), introduces lazy per-task materialization (Bazel/`rules_js` style), and treats postinstall as a separately cacheable computation.

The design is shaped to extend cleanly to Cargo, Python, and Go through the same trait. The cache key is content-addressed, host-triple-aware, and ABI-aware. The materialization strategy is hardlink-first with copy fallback. The whole thing lives in a new `crates/artifact-store` crate parallel to `crates/cache`, sharing remote backends with the existing remote task-output cache when Phase C ships.

We deliberately do not tar `node_modules`. The literature is consistent on this: per-package CAS is the only model that's fast, correct, polyglot-extensible, and operationally debuggable. We follow BuildXL's prep-pip model in Phase B and converge toward Bazel/`rules_js`'s lazy materialization in Phase C.

Phase A is the next shippable unit of work.
