# Install Artifact Cache

The install artifact cache is rage's solution for caching `yarn install` / `pnpm install` / `npm install` and the postinstall scripts they run. It is content-addressed at per-package granularity, keyed by the lockfile integrity hashes already present in every modern lockfile, and restored by hardlinking from a local CAS — not by extracting tarballs and not by re-running the package manager.

This document covers the **package install cache** (Section 1) and the **postinstall cache** (Section 2). Both share the same `LocalArtifactStore` (`crates/artifact-store/src/local.rs`) and the same correctness guarantee: a cache hit is byte-identical to a cold install, or it is not a hit.

The install lifecycle is plugin-driven. Methods on `EcosystemPlugin` (defined in `crates/plugin/src/lib.rs`) describe how each ecosystem detects its lockfile, parses package integrity, and restores tarballs. The scheduler is ecosystem-agnostic — it never branches on plugin id.

## 1. Package install cache

### The bug we fixed

The original implementation cached `yarn install` as a one-line skip marker:

```rust
let marker = cache.dir().join(format!("root-{fp}.done"));
if marker.exists() {
    eprintln!("[rage] {}#{} ✓ (cached)", ..., ...);
    return Ok(());                              // restores nothing
}
// run the install
let _ = std::fs::write(&marker, b"");           // empty file as the "artifact"
```

On a fresh CI runner with the same lockfile as a previous build, the marker file existed in the cache, the install was skipped, and `node_modules/` did not exist. The next task ran `tsc` and got `command not found`. The cache reported a hit; the cache had cached nothing.

The fix is structural: cache the package contents themselves at per-package granularity and restore them on hit.

### Cache key — the lockfile integrity hash

Every modern lockfile already contains a content hash for every external package:

| Package manager | Lockfile | Hash field |
|---|---|---|
| pnpm | `pnpm-lock.yaml` | `integrity: sha512-...` |
| yarn classic | `yarn.lock` | `integrity sha512-...` |
| yarn berry | `yarn.lock` | `checksum: 10c0/sha512hex` |
| npm | `package-lock.json` | `"integrity": "sha512-..."` |
| Cargo | `Cargo.lock` | `checksum = "sha256-..."` |
| Go | `go.sum` | `h1:sha256-...` |

These are computed by the package manager from the tarball bytes downloaded from the registry. They are deterministic, machine-independent, and already verified by the PM on every install.

rage uses these hashes verbatim:

```
cas_key = blake3(integrity_string)
```

The CAS entry value is the **tarball bytes**, copied from the package manager's local cache (no re-download from the registry). Recovery is "extract tarball into `node_modules/{name}/`".

This design is structurally what Bazel `rules_js` and BuildXL do for npm: the lockfile is the source of truth; the integrity hash is the content address; the install is a fan-out of N tarball materializations.

### Per-package granularity in practice

A monorepo with 1,500 packages in its lockfile, where one package has been bumped:

| Approach | Cache behavior |
|---|---|
| Tar all of `node_modules/` keyed by lockfile hash | Lockfile changed → entire tarball invalidated → re-install everything (~30–90s) |
| **Per-package CAS keyed by integrity** | Exactly 1 of 1,500 keys is new → 1 download + 1,499 hardlinks (~50–200ms) |

The second is what rage does. The CAS is monotonic: once a package version is in CAS, it stays there until garbage-collected. Lockfile churn over months produces a CAS that contains every version your monorepo has ever pinned, ready to restore in milliseconds.

### Capture flow

```
1. yarn / pnpm / npm install completes successfully.
2. plugin.parse_lockfile(workspace_root) → Vec<LockfilePackage>
   (each with name, version, integrity, optional tarball_url)
3. plugin.local_pm_cache(workspace_root) → PathBuf
   (e.g. ~/.local/share/pnpm/store/v3/files/, .yarn/cache/, ~/.npm/_cacache/)
4. For each LockfilePackage:
     cas_key = blake3(integrity)
     if store.contains_raw_key(&cas_key): skip (already captured)
     locate tarball in PM cache by integrity
     store.put_bytes_keyed(cas_key, tarball_bytes)
5. Write the rage root-task marker AFTER capture completes.
```

This is fast because the PM has already done the network I/O — the tarball bytes are sitting on disk in a known location. Capture is `cp`-speed (or hardlink-speed where the PM cache and rage CAS share a filesystem).

### Restore flow

```
1. Marker for workspace#install exists.
2. plugin.verify_install_effects(workspace_root) → false (node_modules empty/missing)
3. plugin.parse_lockfile(workspace_root) → Vec<LockfilePackage>
4. Pre-flight: for each package, store.contains_raw_key(blake3(integrity))?
   If any missing → bail and run a real install.
5. plugin.restore_from_cas(packages, workspace_root, store):
     for each package:
        bytes = store.get_bytes_by_raw_key(&cas_key)?
        extract tarball into node_modules/{name}/
6. plugin.create_bin_links(workspace_root):
     walk node_modules/*/package.json
     for each "bin" field, create a symlink in node_modules/.bin/
```

Materialization is hardlink-first via `LocalArtifactStore::link()`:

```rust
match std::fs::hard_link(&src, target) {
    Ok(()) => Ok(()),
    Err(e) if is_exdev(&e) || is_perm(&e) => {
        std::fs::copy(&src, target)?;        // EXDEV / cross-device fallback
        Ok(())
    }
    Err(e) => Err(e.into()),
}
```

The `.bin` symlinks are recreated separately (`create_bin_links()`) because a tarball does not contain them — they are derived state from each package's `package.json:bin` field. This is O(n) symlink syscalls; it cannot be cached because it depends on the resolved layout.

### Yarn 4 (berry)

Yarn berry stores tarballs in a project-local `.yarn/cache/` directory rather than a global location. The TypeScript plugin's `local_pm_cache` returns this path when a `.yarnrc.yml` is detected. Yarn berry also uses a different integrity format: `10c0/sha512hex` (cache-version-prefixed sha512). The plugin parses this format, hashes the entire string with blake3, and uses the result as the CAS key. Cross-tool integrity is preserved: the CAS key for `react@18.2.0` from yarn berry differs from the same package from pnpm, but each format is internally consistent.

### pnpm and the virtual store

pnpm's `node_modules` is a symlink graph rooted at `node_modules/.pnpm/{name}@{version}/node_modules/{name}/`. Capturing the symlink graph as a tarball is the wrong abstraction; capturing per-package tarballs and reconstructing the graph on restore is correct. The TypeScript plugin's `restore_from_cas` for pnpm:

1. Extracts each tarball to `node_modules/.pnpm/{name}@{version}/node_modules/{name}/`.
2. Re-creates the workspace-root `node_modules/{name}` symlinks pointing into `.pnpm/`.
3. Re-creates the package-internal symlinks for transitive deps based on `pnpm-lock.yaml`.

The lockfile contains every edge in the graph. The plugin reconstructs it deterministically.

### What gets cached, what doesn't

| In the install CAS | Not in the install CAS |
|---|---|
| Tarball bytes for every external package | Workspace-local packages (no integrity hash) |
| `node_modules/` directory tree (after restore) | `.bin/` symlinks (recreated in `create_bin_links`) |
| Postinstall outputs (separate cache; see §2) | `~/.cache/playwright/`, `~/.cache/puppeteer/` (out-of-tree side effects) |

Out-of-tree side effects (Section 2 below) are what the postinstall cache addresses.

## 2. Postinstall cache

A postinstall script can do anything: compile a native addon, download a platform-specific binary, write to `~/.cache/`. The install cache restores tarballs faithfully, but a tarball does not include the side effects of running the package's postinstall. Without a separate cache, every fresh node_modules has to re-run every postinstall — and on a 200-package monorepo, that's where the install time actually lives.

### Implementation

The implementation is in `crates/scheduler/src/postinstall_cache.rs`. The data model:

```rust
pub enum FileKind {
    Regular,
    Symlink(PathBuf),
}

pub struct ManifestEntry {
    pub rel_path: PathBuf,
    pub content_hash: [u8; 32],   // blake3; zeroed for symlinks
    pub mode: u32,                 // st_mode & 0o777; zero for symlinks
    pub kind: FileKind,
}

pub type PostinstallManifest = Vec<ManifestEntry>;
```

The cache key:

```rust
pub fn postinstall_cas_key(task: &PostinstallTask) -> [u8; 32] {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let node_version = read_node_version();
    let input = format!("{}:{}:{}", task.tarball_integrity, platform, node_version);
    blake3::hash(input.as_bytes()).into()
}
```

Three axes:

1. **`tarball_integrity`** — the lockfile integrity hash for this exact package version.
2. **`platform`** — `darwin-aarch64`, `linux-x86_64`, etc. Native binaries are platform-bound.
3. **`node_version`** — the running Node.js version. Native addons compiled against Node 18 ABI fail to load on Node 20.

A change in any axis breaks the cache. There is no way to restore a darwin-arm64 postinstall on linux-x86_64 by accident.

### Capture algorithm

```
For each PostinstallTask emitted by plugin.postinstall_tasks(workspace_root):
  before = capture_dir(task.cwd, store)        # walk + hash everything in node_modules/{pkg}/
  ok = run_postinstall(task)                   # `sh -c task.script` in task.cwd
  if !ok: continue                             # postinstall failed; don't cache
  after  = capture_dir(task.cwd, store)        # walk + hash again
  delta  = diff_manifests(&before, &after)     # entries new or changed in `after`
  store_manifest(&postinstall_cas_key(task), &delta, store)
```

`capture_dir` walks the package directory (post-extraction or post-postinstall) and:

- For each regular file: blake3 the contents, `put_bytes_keyed(content_hash, bytes)` into CAS, record `(rel_path, content_hash, mode, Regular)` in the manifest.
- For each symlink: read the link target, record `(rel_path, [0u8; 32], 0, Symlink(target))`.
- Skip directories, skip unreadable entries.

`diff_manifests` returns entries that are new or whose `(content_hash, mode, kind)` differs between `before` and `after`. **Deletions are not tracked** — a postinstall that deletes a file from the tarball will not invalidate the cache key for that file. This is a pragmatic choice: the install cache (§1) restores the tarball exactly, so the file will reappear; postinstalls that depend on file deletion are extremely rare.

### Empty delta → no cache entry

```rust
pub fn store_manifest(...) -> std::io::Result<bool> {
    if delta.is_empty() {
        return Ok(false);                      // do not write to CAS
    }
    let json = serde_json::to_vec(delta)?;
    store.put_bytes_keyed(*key, &json)?;
    Ok(true)
}
```

If the postinstall script ran but produced no observable change to the package directory (it was a no-op, or it only touched out-of-tree files we don't track), `delta` is empty. We deliberately **do not** write a manifest in that case.

The reason: an empty cache entry is indistinguishable from a missing cache entry on restore (both produce "manifest not found, run the script"). If we wrote an empty manifest, a future restore would see a hit with zero files to materialize and silently skip the script. That would be wrong if the script has out-of-tree side effects we couldn't observe in the package dir. Returning `Ok(false)` ensures the script re-runs next time, which is the correct conservative default.

### Restore algorithm

```rust
pub fn restore_manifest(
    key: &[u8; 32],
    target_dir: &Path,
    store: &LocalArtifactStore,
) -> std::io::Result<bool> {
    let bytes = store.get_bytes_by_raw_key(key)?.ok_or(...)?;
    let manifest: PostinstallManifest = serde_json::from_slice(&bytes)?;
    for entry in &manifest {
        let dest = target_dir.join(&entry.rel_path);
        std::fs::create_dir_all(dest.parent().unwrap())?;
        match &entry.kind {
            FileKind::Regular => {
                let cas_path = store.cas_file_path(&entry.content_hash);
                let _ = std::fs::remove_file(&dest);
                if std::fs::hard_link(&cas_path, &dest).is_err() {
                    std::fs::copy(&cas_path, &dest)?;            // EXDEV fallback
                }
                std::fs::set_permissions(
                    &dest,
                    Permissions::from_mode(entry.mode),
                )?;
            }
            FileKind::Symlink(target) => {
                let _ = std::fs::remove_file(&dest);
                std::os::unix::fs::symlink(target, &dest)?;
            }
        }
    }
    Ok(true)
}
```

What this preserves correctly:

- **Executable bits.** A native addon written by `node-gyp` is `0o755`. The mode is stored in the manifest and restored via `set_permissions`. `bin/esbuild` remains executable.
- **Symlinks as symlinks.** A postinstall that creates `bin/foo → ../target/release/foo` is restored as a symlink, not a copy.
- **Hardlinks from CAS.** Each restored regular file is a hardlink into `~/.rage/artifacts/content/`. Zero-copy at the byte level; the kernel only writes a directory entry.

What this does not do:

- **No directory deletion.** If the postinstall *removed* a file from the package, the restore will not remove it. The install cache (§1) has already extracted the tarball exactly; the postinstall was meant to *add* native compiled output. If a postinstall deletes files in practice, those files reappear on cache restore — a divergence the user will notice. We accept this and document it.
- **No tracking of out-of-tree writes.** A postinstall that writes to `~/.cache/playwright/` is invisible to `capture_dir`. The package will be restored intact, but on first use, playwright will run its own download bootstrap. Most tools handle this gracefully (they self-heal on cold-start). Tracking out-of-tree writes via the sandbox is on the roadmap.

### PM-policy awareness

Plugins must respect the package manager's own postinstall policy. The TypeScript plugin's `postinstall_tasks(workspace_root)`:

1. Reads `.yarnrc.yml` for `enableScripts: false`. If set, returns `vec![]`.
2. Reads `.npmrc` for `ignore-scripts=true`. If set, returns `vec![]`.
3. Reads pnpm's `package.json:pnpm.onlyBuiltDependencies` and `neverBuiltDependencies`. Filters the discovered list accordingly.
4. Walks `node_modules/` and finds every `package.json` that declares `scripts.postinstall`.
5. For each surviving package, looks up its `tarball_integrity` from the lockfile parse and emits a `PostinstallTask`.

The scheduler runs only what the plugin emits. Users who already configured their PM to skip scripts get the same behavior under rage — no surprises.

### Scope: per-package, not per-workspace

Each `PostinstallTask` is one package's postinstall, not the entire `yarn install`'s postinstall phase. This is what makes the cache useful:

- A 200-package monorepo bumps `esbuild` from 0.21.4 to 0.21.5.
- The lockfile hash for `esbuild` changes; everything else is the same.
- Install cache: 1 tarball miss, 199 hardlink hits.
- Postinstall cache: 1 postinstall key changes (`esbuild`'s integrity differs), 199 keys are unchanged.
- The scheduler reruns `esbuild`'s postinstall, restores the other 199 from CAS via hardlinks.
- Total time: maybe 300ms.

Compare to: any cache that keys postinstall outputs at install-step granularity. That cache invalidates everything when one package version changes. We invalidate exactly one entry.

## 3. Storage layout

Both caches share `~/.rage/artifacts/` (configurable via `rage.json`):

```
~/.rage/artifacts/
└── content/
    ├── 7a/f3e2.../data            ← tarball for one package, OR
    ├── b9/1c01.../data            ← bytes of one file written by a postinstall, OR
    └── 4c/8ad3.../data            ← serialized PostinstallManifest JSON
```

Three kinds of value occupy the same address space:

1. **Tarballs** (install cache), keyed by `blake3(integrity)`.
2. **Individual file bytes** (postinstall cache), keyed by `blake3(content)`.
3. **Manifest JSON** (postinstall cache), keyed by `blake3(integrity:platform:node_version)`.

Collisions are impossible: blake3 is collision-resistant and the inputs to each key are disjoint by construction (a tarball's integrity hash is never a content hash and is never a postinstall key triple).

The store is monotonic. `rage gc` reclaims content not referenced by any pinned manifest; `rage gc --aggressive` reclaims by LRU on the file access time. There is no automatic GC in v1 — the store grows until you run `rage gc`.

## 4. Concurrency

The CAS is naturally concurrency-safe: writes use tmp-file + atomic rename, and identical bytes produce the same path so concurrent writers either hit a fast-path "already present" check or race harmlessly on the rename. The scheduler emits postinstall tasks serially (or with bounded parallelism); two `rage` processes against the same workspace do not corrupt each other's CAS.

What is not yet implemented: a workspace-level install lock to prevent two `rage run install` invocations from both running `yarn install` simultaneously. The PMs themselves usually catch this (yarn writes a `.yarn-integrity` lock; pnpm uses `.pnpm-lock`), but the rage-level coordination is on the roadmap.

## 5. Why not `tar node_modules`

The natural first instinct: `tar -cf node_modules.tar node_modules/`, store the tar, untar on hit. This is what most CI cache actions do. We rejected it for six reasons:

1. **Symlinks don't survive cleanly across platforms.** Cross-platform tar implementations differ on absolute vs relative targets, and pnpm's symlink graph is the precise failure mode that breaks under tar.
2. **Update granularity is the entire tarball.** One package change → re-upload a 1.4 GB tarball. Per-package CAS uploads only the changed package.
3. **ABI fragility.** A tarball contains compiled native addons; restoring on a different platform segfaults. We'd have to gate the tarball by host triple anyway.
4. **Postinstall side effects.** The tarball doesn't see `~/.cache/playwright/`. Per-package tracking can.
5. **Operational debugging.** A tar restore failure is opaque. A per-package CAS failure points at the failing key, the expected hash, the on-disk state.
6. **Storage scaling.** A monorepo's lifetime worth of tarballs is gigabytes; its lifetime worth of unique package versions in CAS is megabytes — most package versions are reused across builds.

The literature is consistent on this: BuildXL uses per-pip CAS, Bazel `rules_js` uses per-package CAS, pnpm's local store is itself a per-package CAS. We follow the consensus.

## 6. Polyglot extensibility

The `EcosystemPlugin` trait methods (`parse_lockfile`, `local_pm_cache`, `restore_from_cas`, `verify_install_effects`, `postinstall_tasks`) are designed to extend cleanly:

| Ecosystem | `parse_lockfile` source | `local_pm_cache` | `restore_from_cas` |
|---|---|---|---|
| **TypeScript** (implemented) | `yarn.lock`, `pnpm-lock.yaml`, `package-lock.json` | `~/.local/share/pnpm/store/`, `.yarn/cache/`, `~/.npm/_cacache/` | extract tarball + symlink graph |
| Rust (future) | `Cargo.lock`'s `checksum = "sha256-..."` | `~/.cargo/registry/cache/` | hardlink crate into registry |
| Python uv (future) | `uv.lock` per-wheel digest | `~/.cache/uv/wheels/` | extract wheel into `.venv/lib/.../site-packages/` |
| Go (future) | `go.sum` `h1:` hashes | `$GOPATH/pkg/mod/cache/download/` | hardlink module into `$GOPATH/pkg/mod/` |

The scheduler does not change. The CAS does not change. A new ecosystem is one trait implementation away.
