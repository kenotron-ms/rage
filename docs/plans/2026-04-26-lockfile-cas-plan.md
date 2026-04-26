# Phase: Lockfile-Integrity-Hash CAS for Install Artifacts

> **Execution:** Use the subagent-driven-development workflow.

**Goal:** Replace the current "walk node_modules + hash individual files" capture with a
lockfile-integrity-hash approach. CAS key = integrity hash from lockfile. Capture = find
PM tarballs in local cache → copy to rage CAS. Restore = extract tarballs from rage CAS.

**Design reference:** `docs/design/install-artifact-cache.md` Section 13B (added 2026-04-26).

**MSRV:** Rust 1.91, `unsafe_code = "forbid"`.

**IMPORTANT — DO NOT stray from this design:**
- CAS key = `Blake3(integrity_string.as_bytes())` — NOT a hash of file contents
- Capture = copy tarballs from PM local cache to rage CAS — NOT walk node_modules
- Restore = extract tarballs into `node_modules/` — NOT hardlink individual files
- `parse_lockfile()` returns `None` → always run install (no CAS capture)
- The SDD skill, writing-plans skill, TDD skill, and verification-before-completion skill
  MUST be followed.

---

## Context — What Already Exists

| File | What it does now | Change |
|------|-----------------|--------|
| `crates/plugin/src/lib.rs` | `EcosystemPlugin` trait | Add `LockfilePackage`, `parse_lockfile`, `local_pm_cache`, `restore_from_cas` |
| `crates/plugin-typescript/src/lib.rs` | TypeScript plugin | Implement new methods |
| `crates/scheduler/src/artifact_capture.rs` | Walks node_modules | Replace with lockfile-based capture |
| `crates/scheduler/src/artifact_restore.rs` | Reads per-pkg JSON | Replace with tarball extraction |
| `crates/scheduler/src/runner.rs` | Calls capture/restore | Update to thread plugin through |
| `crates/artifact-store/src/lib.rs` | File-level CAS | Add `get_bytes` method |

**The `capture_all_node_modules` function in artifact_capture.rs MUST be replaced.**
**The `restore_package` in artifact_restore.rs MUST be replaced with tarball extraction.**

---

## Constraints (COE)

1. **Parse, don't walk.** The CAS key is derived from the lockfile integrity string, never from
   computed file hashes. This makes captures deterministic across machines.
2. **Tarballs, not files.** The unit stored in the CAS is the npm package tarball (zip), not
   individual files within it. This avoids symlink complexity entirely.
3. **Fallback gracefully.** If the PM local cache doesn't have the tarball, skip capture for
   that package — don't fail the build. Log a debug message.
4. **No breaking changes.** Existing tests for `verify_install_effects` and the marker-based
   flow must continue passing.
5. **Yarn berry format.** lage uses yarn berry (v8). The lockfile has entries like:
   `"@pkg@npm:1.0.0": / resolution: "@pkg@npm:1.0.0" / checksum: 10c0/sha512hex / languageName: node`.
   The checksum format is `{cacheKey}/{sha512hex}`. The yarn cache files are at
   `{workspace}/.yarn/cache/{pkg-sanitized}-{sha1}.zip`. CAS key = Blake3 of the full checksum
   string (including the `10c0/` prefix, which is the yarn cache version).
6. **No flaky tests.** Tests use real tempdir fixtures, not mocks of filesystem I/O.
7. **Commit after each task** with the exact message specified.

---

## Files Created / Modified

### Modified
- `crates/plugin/src/lib.rs` — Add `LockfilePackage` struct + 3 new trait methods with defaults
- `crates/plugin-typescript/src/lib.rs` — Implement new methods; add `lockfile.rs` submodule
- `crates/plugin-typescript/Cargo.toml` — Add `serde_yml = "0.0"` if not already present
- `crates/scheduler/src/artifact_capture.rs` — Replace `capture_all_node_modules` with `capture_from_lockfile`
- `crates/scheduler/src/artifact_restore.rs` — Replace file-level restore with tarball extraction
- `crates/scheduler/src/runner.rs` — Thread plugin through capture/restore calls
- `crates/artifact-store/src/lib.rs` — Add `get_bytes` to `ArtifactStore` trait
- `crates/artifact-store/src/local.rs` — Implement `get_bytes`

### Created
- `crates/plugin-typescript/src/lockfile.rs` — Lockfile parsers (yarn berry, yarn classic, pnpm, npm)

---

## Tasks

---

### Task 1 — Add `LockfilePackage` and extend `EcosystemPlugin` trait

**File:** `crates/plugin/src/lib.rs`

**What to implement (TDD):**

Write these **failing tests** first in a `#[cfg(test)]` block at the bottom of `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct NullPlugin;
    impl EcosystemPlugin for NullPlugin {
        fn id(&self) -> &'static str { "null" }
        fn detection_globs(&self) -> Vec<&'static str> { vec![] }
        fn infer_tasks(&self, _: &Path) -> Vec<TaskDef> { vec![] }
        fn toolchain_allowlist(&self) -> Vec<AllowlistEntry> { vec![] }
        fn declared_input_globs(&self, _: &str, _: &Path, _: &PluginConfig) -> Vec<String> { vec![] }
        fn abi_fingerprint(&self, _: &Path) -> Option<String> { None }
        fn infer_root_tasks(&self, _: &Path) -> Vec<RootTask> { vec![] }
        fn verify_install_effects(&self, _: &Path) -> bool { true }
    }

    #[test]
    fn default_parse_lockfile_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.parse_lockfile(tmp.path()).is_none());
    }

    #[test]
    fn default_local_pm_cache_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.local_pm_cache(tmp.path()).is_none());
    }

    #[test]
    fn lockfile_package_roundtrips_serde() {
        let pkg = LockfilePackage {
            name: "ms".to_string(),
            version: "2.1.3".to_string(),
            integrity: "sha512-abc123".to_string(),
            tarball_url: Some("https://registry.npmjs.org/ms/-/ms-2.1.3.tgz".to_string()),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let decoded: LockfilePackage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "ms");
        assert_eq!(decoded.integrity, "sha512-abc123");
    }
}
```

**Then implement:**

Add before the `EcosystemPlugin` trait definition:

```rust
/// A package resolved from a lockfile, with its content integrity hash.
///
/// The integrity string is taken verbatim from the lockfile (e.g. `sha512-XXXX`
/// for npm/pnpm/yarn classic, `10c0/sha512hex` for yarn berry). It is used as
/// the basis for the CAS key (`Blake3(integrity.as_bytes())`), making CAS entries
/// deterministic and compatible with the package manager's own verification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockfilePackage {
    /// npm package name (e.g. `"ms"` or `"@types/node"`).
    pub name: String,
    /// Resolved version (e.g. `"2.1.3"`).
    pub version: String,
    /// Integrity hash string from the lockfile. Format varies by PM:
    /// - pnpm / yarn classic / npm: `sha512-XXXX` or `sha1-XXXX`
    /// - yarn berry: `10c0/sha512hex` (cache-version-prefixed)
    pub integrity: String,
    /// URL of the tarball (optional). Present in most npm/pnpm/yarn lockfiles.
    pub tarball_url: Option<String>,
}
```

Add to `EcosystemPlugin` trait (after `verify_install_effects`):

```rust
    /// Parse the lockfile(s) and return all external packages with integrity hashes.
    ///
    /// Returns `None` if this ecosystem has no lockfile (e.g. bare `requirements.txt`).
    /// When `None`: rage skips CAS capture and always runs the install command.
    ///
    /// Workspace packages (those with no integrity hash in the lockfile) MUST be
    /// excluded from the returned list.
    fn parse_lockfile(&self, _workspace_root: &Path) -> Option<Vec<LockfilePackage>> {
        None
    }

    /// Path to this ecosystem's local package cache (where the PM stores downloaded tarballs).
    ///
    /// Used as the fast-path source during capture: rage copies tarballs from the PM cache
    /// instead of downloading from the registry.
    ///
    /// Returns `None` if the PM cache path cannot be determined or doesn't exist.
    fn local_pm_cache(&self, _workspace_root: &Path) -> Option<PathBuf> {
        None
    }

    /// Restore packages from the rage artifact CAS into the workspace.
    ///
    /// Called when: install marker present + `verify_install_effects` returns `false` +
    /// CAS contains tarballs for all packages returned by `parse_lockfile`.
    ///
    /// Implementations should:
    /// 1. For each package: `store.get_bytes(blake3(integrity))` → get tarball bytes
    /// 2. Extract tarball into `workspace_root/node_modules/{name}/`
    /// 3. Handle scoped packages: `@types/node` → `node_modules/@types/node/`
    ///
    /// Default is a no-op (falls through to full reinstall).
    fn restore_from_cas(
        &self,
        _packages: &[LockfilePackage],
        _workspace_root: &Path,
        _store: &dyn crate::ArtifactStoreRef,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }
```

Wait — the plugin crate shouldn't depend on `artifact-store` crate to avoid circular deps.
Instead, use a trait object reference defined in the plugin crate itself:

```rust
/// Minimal artifact store interface used by plugins for restoration.
/// Avoids a direct dependency on the `artifact-store` crate.
pub trait ArtifactStoreRef: Send + Sync {
    fn get_bytes(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, std::io::Error>;
    fn contains_key(&self, key: &[u8; 32]) -> bool;
}
```

And `restore_from_cas` signature becomes:
```rust
    fn restore_from_cas(
        &self,
        _packages: &[LockfilePackage],
        _workspace_root: &Path,
        _store: &dyn ArtifactStoreRef,
    ) -> Result<(), anyhow::Error> {
        Ok(())
    }
```

In `crates/artifact-store/src/local.rs`, implement `ArtifactStoreRef` for `LocalArtifactStore`:
```rust
impl plugin::ArtifactStoreRef for LocalArtifactStore {
    fn get_bytes(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, std::io::Error> {
        let hash = ContentHash(*key);
        self.get_bytes_impl(&hash)
    }
    fn contains_key(&self, key: &[u8; 32]) -> bool {
        self.contains(&ContentHash(*key))
    }
}
```

**Run:** `cargo test -p plugin --lib` — 3 tests pass, 0 fail.

**Commit:** `feat(plugin): LockfilePackage + parse_lockfile/local_pm_cache/restore_from_cas trait methods`

---

### Task 2 — Add `get_bytes` to `LocalArtifactStore`

**File:** `crates/artifact-store/src/local.rs`

**What to implement (TDD):**

Write a failing test first:

```rust
#[test]
fn get_bytes_returns_stored_content() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(tmp.path());
    let data = b"hello world";
    let hash = store.put_bytes(data).unwrap();
    let retrieved = store.get_bytes_impl(&hash).unwrap().unwrap();
    assert_eq!(&retrieved, data);
}

#[test]
fn get_bytes_returns_none_for_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(tmp.path());
    let missing = ContentHash([0u8; 32]);
    let result = store.get_bytes_impl(&missing).unwrap();
    assert!(result.is_none());
}
```

Then implement:
```rust
pub fn get_bytes_impl(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, std::io::Error> {
    let path = self.content_path(hash);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
```

Also implement `plugin::ArtifactStoreRef` for `LocalArtifactStore` (requires adding `plugin` to artifact-store's `Cargo.toml` dependencies).

**IMPORTANT**: To avoid circular deps:
- `artifact-store` can depend on `plugin` ONLY for implementing `ArtifactStoreRef`
- `plugin` MUST NOT depend on `artifact-store`

Add to `crates/artifact-store/Cargo.toml`:
```toml
[dependencies]
plugin = { path = "../plugin" }
```

**Run:** `cargo test -p artifact-store --lib` — all tests pass.

**Commit:** `feat(artifact-store): add get_bytes_impl + implement ArtifactStoreRef for LocalArtifactStore`

---

### Task 3 — Implement yarn berry lockfile parser

**File:** `crates/plugin-typescript/src/lockfile.rs` (new file)

The lage repo uses yarn berry (v8 format). Parse the yarn.lock format:

```
__metadata:
  version: 8
  cacheKey: 10c0

"@pkg/name@npm:1.0.0":
  version: 1.0.0
  resolution: "@pkg/name@npm:1.0.0"
  checksum: 10c0/sha512hexstring
  languageName: node
  linkType: hard
```

**What to implement (TDD):**

Create fixture file `crates/plugin-typescript/tests/fixtures/yarn-berry.lock`:
```
__metadata:
  version: 8
  cacheKey: 10c0

"ms@npm:2.1.3":
  version: 2.1.3
  resolution: "ms@npm:2.1.3"
  checksum: 10c0/abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  languageName: node
  linkType: hard

"@types/node@npm:20.0.0":
  version: 20.0.0
  resolution: "@types/node@npm:20.0.0"
  checksum: 10c0/fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210
  languageName: node
  linkType: hard

"workspace-a@workspace:packages/a":
  version: 0.0.0-use.local
  resolution: "workspace-a@workspace:packages/a"
  languageName: unknown
  linkType: soft
```

Write failing tests first:

```rust
#[test]
fn parse_yarn_berry_extracts_packages() {
    let fixture = include_str!("../tests/fixtures/yarn-berry.lock");
    let packages = parse_yarn_berry_lockfile(fixture);
    assert_eq!(packages.len(), 2, "workspace packages must be excluded");
    let ms = packages.iter().find(|p| p.name == "ms").unwrap();
    assert_eq!(ms.version, "2.1.3");
    assert!(ms.integrity.starts_with("10c0/"), "integrity must include cache prefix");
    let types = packages.iter().find(|p| p.name == "@types/node").unwrap();
    assert_eq!(types.version, "20.0.0");
}

#[test]
fn yarn_berry_skips_workspace_packages() {
    let fixture = include_str!("../tests/fixtures/yarn-berry.lock");
    let packages = parse_yarn_berry_lockfile(fixture);
    assert!(!packages.iter().any(|p| p.name.contains("workspace-a")));
}
```

**Implement** `parse_yarn_berry_lockfile(content: &str) -> Vec<LockfilePackage>`:
- Split content by blank lines to get entries
- For each entry:
  - First non-blank line: `"pkg@npm:version":` or `"@scope/pkg@npm:version":`
  - Extract `name` (strip quotes, strip `@version` suffix)
  - Find `version: X.Y.Z` line → version
  - Find `checksum: 10c0/hex` line → integrity
  - Find `languageName: unknown` → this is a workspace package, skip
  - Find `linkType: soft` → also skip (workspace alias)
- Skip entries without a checksum (workspace entries)

Also add `parse_yarn_classic_lockfile(content: &str) -> Vec<LockfilePackage>` for yarn v1 (not used by lage but for completeness):
- Format: `packagename@version:\n  resolved "url#sha1=xxx"\n  integrity sha512-xxx\n`

And `parse_pnpm_lockfile(content: &str) -> Vec<LockfilePackage>` for pnpm-lock.yaml:
- Format: YAML with `packages:` section, each entry has `integrity:` field

And `parse_npm_lockfile(content: &str) -> Vec<LockfilePackage>` for package-lock.json:
- Format: JSON with `packages["node_modules/name"].integrity` field

**Run:** `cargo test -p plugin-typescript --lib lockfile` — all tests pass.

**Commit:** `feat(plugin-typescript): lockfile parsers for yarn berry/classic, pnpm, npm`

---

### Task 4 — Implement `parse_lockfile` and `local_pm_cache` in TypeScript plugin

**File:** `crates/plugin-typescript/src/lib.rs`

**What to implement (TDD):**

Add to the `impl EcosystemPlugin for TypeScriptPlugin` block.

First write failing integration tests in `crates/plugin-typescript/tests/lockfile_integration.rs`:

```rust
use plugin_typescript::TypeScriptPlugin;
use plugin::EcosystemPlugin;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[test]
fn parse_lockfile_yarn_berry_returns_packages() {
    // Create a temp workspace with yarn.lock
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixtures_dir().join("yarn-berry.lock"),
        tmp.path().join("yarn.lock"),
    ).unwrap();
    
    let plugin = TypeScriptPlugin::new();
    let packages = plugin.parse_lockfile(tmp.path()).unwrap();
    assert!(!packages.is_empty());
    assert!(packages.iter().all(|p| !p.integrity.is_empty()));
}

#[test]
fn parse_lockfile_returns_none_when_no_lockfile() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin = TypeScriptPlugin::new();
    assert!(plugin.parse_lockfile(tmp.path()).is_none());
}

#[test]
fn local_pm_cache_yarn_berry_returns_yarn_cache_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // Create .yarn/cache directory
    std::fs::create_dir_all(tmp.path().join(".yarn/cache")).unwrap();
    let plugin = TypeScriptPlugin::new();
    // yarn.lock must exist for PM detection to work
    std::fs::write(tmp.path().join("yarn.lock"), "__metadata:\n  version: 8\n").unwrap();
    let cache = plugin.local_pm_cache(tmp.path());
    // Should point to workspace .yarn/cache for yarn berry
    assert!(cache.is_some());
    assert!(cache.unwrap().ends_with(".yarn/cache") || cache.unwrap().to_string_lossy().contains("yarn"));
}
```

**Implement:**

```rust
fn parse_lockfile(&self, workspace_root: &Path) -> Option<Vec<plugin::LockfilePackage>> {
    use crate::lockfile::*;
    
    // Detect PM from lockfile presence, same order as detect.rs
    if let Ok(content) = std::fs::read_to_string(workspace_root.join("pnpm-lock.yaml")) {
        return Some(parse_pnpm_lockfile(&content));
    }
    if let Ok(content) = std::fs::read_to_string(workspace_root.join("yarn.lock")) {
        // Detect yarn version from __metadata.version
        if content.contains("__metadata:") {
            return Some(parse_yarn_berry_lockfile(&content));
        }
        return Some(parse_yarn_classic_lockfile(&content));
    }
    if let Ok(content) = std::fs::read_to_string(workspace_root.join("package-lock.json")) {
        return Some(parse_npm_lockfile(&content));
    }
    None
}

fn local_pm_cache(&self, workspace_root: &Path) -> Option<std::path::PathBuf> {
    // Yarn berry: .yarn/cache/ in workspace
    let yarn_cache = workspace_root.join(".yarn/cache");
    if yarn_cache.is_dir() && workspace_root.join("yarn.lock").exists() {
        // Check if berry (has __metadata)
        if let Ok(content) = std::fs::read_to_string(workspace_root.join("yarn.lock")) {
            if content.contains("__metadata:") {
                return Some(yarn_cache);
            }
        }
    }
    
    // Yarn classic
    if workspace_root.join("yarn.lock").exists() {
        let home = std::env::var("HOME").unwrap_or_default();
        let yarn_classic_cache = std::path::Path::new(&home).join(".yarn/cache");
        if yarn_classic_cache.is_dir() {
            return Some(yarn_classic_cache);
        }
    }
    
    // pnpm
    if workspace_root.join("pnpm-lock.yaml").exists() {
        // Try standard pnpm store locations
        let home = std::env::var("HOME").unwrap_or_default();
        for path in &[
            format!("{}/.local/share/pnpm/store/v3/files", home),
            format!("{}/Library/pnpm/store/v3/files", home),
        ] {
            let p = std::path::Path::new(path);
            if p.is_dir() {
                return Some(p.to_path_buf());
            }
        }
    }
    
    // npm
    if workspace_root.join("package-lock.json").exists() {
        let home = std::env::var("HOME").unwrap_or_default();
        let npm_cache = std::path::Path::new(&home).join(".npm/_cacache");
        if npm_cache.is_dir() {
            return Some(npm_cache);
        }
    }
    
    None
}
```

**Run:** `cargo test -p plugin-typescript` — all tests pass.

**Commit:** `feat(plugin-typescript): implement parse_lockfile + local_pm_cache for yarn/pnpm/npm`

---

### Task 5 — Implement `restore_from_cas` in TypeScript plugin

**File:** `crates/plugin-typescript/src/lib.rs`

Implement tarball extraction from CAS into node_modules.

**What to implement (TDD):**

Add test in `crates/plugin-typescript/tests/lockfile_integration.rs`:

```rust
#[test]
fn restore_from_cas_extracts_tarball_into_node_modules() {
    use std::io::Write;
    
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    let store_dir = ws.join("cas-store");
    
    // Create a minimal valid npm tarball (.tgz = gzipped tar)
    // Structure: package/package.json, package/index.js
    let tarball_bytes = create_test_tarball("ms", "2.1.3");
    let integrity = "sha512-testintegrity123";
    
    // Store in fake CAS
    struct FakeStore { data: std::collections::HashMap<[u8; 32], Vec<u8>> }
    impl plugin::ArtifactStoreRef for FakeStore {
        fn get_bytes(&self, key: &[u8; 32]) -> Result<Option<Vec<u8>>, std::io::Error> {
            Ok(self.data.get(key).cloned())
        }
        fn contains_key(&self, key: &[u8; 32]) -> bool {
            self.data.contains_key(key)
        }
    }
    
    let cas_key = compute_cas_key(integrity);
    let mut store = FakeStore { data: std::collections::HashMap::new() };
    store.data.insert(cas_key, tarball_bytes);
    
    let pkgs = vec![plugin::LockfilePackage {
        name: "ms".to_string(),
        version: "2.1.3".to_string(),
        integrity: integrity.to_string(),
        tarball_url: None,
    }];
    
    let plugin = TypeScriptPlugin::new();
    plugin.restore_from_cas(&pkgs, ws, &store).unwrap();
    
    // Verify extraction
    assert!(ws.join("node_modules/ms/package.json").exists());
    assert!(ws.join("node_modules/ms/index.js").exists());
}
```

**Implement:**

```rust
fn restore_from_cas(
    &self,
    packages: &[plugin::LockfilePackage],
    workspace_root: &Path,
    store: &dyn plugin::ArtifactStoreRef,
) -> Result<(), anyhow::Error> {
    let nm = workspace_root.join("node_modules");
    std::fs::create_dir_all(&nm)?;
    
    for pkg in packages {
        let cas_key = compute_cas_key(&pkg.integrity);
        match store.get_bytes(&cas_key)? {
            None => {
                // CAS miss for this package — skip (caller should have verified all present)
                continue;
            }
            Some(tarball_bytes) => {
                extract_tarball_to_node_modules(&tarball_bytes, &pkg.name, &nm)?;
            }
        }
    }
    Ok(())
}
```

Helper functions to add in `lockfile.rs` or a new `tarball.rs`:

```rust
/// Compute CAS key from integrity string.
/// CAS key = Blake3(integrity_string.as_bytes())
pub fn compute_cas_key(integrity: &str) -> [u8; 32] {
    use blake3::Hasher;
    let mut h = Hasher::new();
    h.update(integrity.as_bytes());
    *h.finalize().as_bytes()
}

/// Extract an npm tarball (gzipped tar) into node_modules/{name}/.
/// Handles scoped packages: @types/node → node_modules/@types/node/
pub fn extract_tarball_to_node_modules(
    tarball: &[u8],
    pkg_name: &str,
    node_modules: &Path,
) -> Result<(), anyhow::Error> {
    use std::io::Cursor;
    
    let gz = flate2::read::GzDecoder::new(Cursor::new(tarball));
    let mut archive = tar::Archive::new(gz);
    
    // npm tarballs have a top-level "package/" directory; strip it
    let pkg_dir = node_modules.join(pkg_name);
    std::fs::create_dir_all(&pkg_dir)?;
    
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        
        // Strip leading "package/" component
        let stripped = path.strip_prefix("package").unwrap_or(&path);
        if stripped.as_os_str().is_empty() {
            continue;
        }
        
        let target = pkg_dir.join(stripped);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
    }
    Ok(())
}
```

Add to `crates/plugin-typescript/Cargo.toml`:
```toml
flate2 = "1"
tar = "0.4"
blake3 = "1"
```

**Run:** `cargo test -p plugin-typescript` — all tests pass.

**Commit:** `feat(plugin-typescript): restore_from_cas extracts tarballs into node_modules`

---

### Task 6 — Rewrite `artifact_capture.rs` to use lockfile-based capture

**File:** `crates/scheduler/src/artifact_capture.rs`

**What to implement (TDD):**

Write failing tests first. The key behavioral change:
- OLD: `capture_all_node_modules` walks node_modules, hashes files
- NEW: `capture_from_lockfile` parses lockfile, finds tarballs in PM cache, stores tarballs

```rust
#[test]
fn capture_from_lockfile_stores_tarball_in_cas() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    
    // Create a fake PM cache with a tarball
    let pm_cache = tmp.path().join("pm-cache");
    std::fs::create_dir_all(&pm_cache).unwrap();
    
    // Create a yarn berry style zip file (package tarball)
    let tarball_bytes = create_simple_tarball();
    // yarn berry names cache files as: @scope-pkg-npm-version-{hash}.zip
    // We need to find it by scanning the cache dir and checking the zip's integrity
    // Actually, simpler: we just look up by integrity string matching the filename
    // For the test, create a file named to match our test package's integrity
    let integrity = "10c0/abcdef123";
    let cas_key = compute_cas_key(integrity);
    
    // Create fake yarn berry zip file in PM cache
    // yarn berry file naming: {pkg-sanitized}-npm-{version}-{checksum_prefix}.zip
    let cache_file = pm_cache.join("ms-npm-2.1.3-abcdef123.zip");
    std::fs::write(&cache_file, &tarball_bytes).unwrap();
    
    // Fake plugin
    struct FakePlugin { pm_cache: PathBuf }
    impl plugin::EcosystemPlugin for FakePlugin {
        // ... required trait methods returning empty defaults ...
        fn parse_lockfile(&self, _: &Path) -> Option<Vec<plugin::LockfilePackage>> {
            Some(vec![plugin::LockfilePackage {
                name: "ms".to_string(),
                version: "2.1.3".to_string(),
                integrity: "10c0/abcdef123".to_string(),
                tarball_url: None,
            }])
        }
        fn local_pm_cache(&self, _: &Path) -> Option<PathBuf> {
            Some(self.pm_cache.clone())
        }
    }
    
    let store = LocalArtifactStore::new(tmp.path().join("cas"));
    let artifact_dir = tmp.path().join("artifact-packages/fp123");
    
    let count = capture_from_lockfile(
        ws,
        &artifact_dir,
        &FakePlugin { pm_cache },
        &store,
    ).unwrap();
    
    assert_eq!(count, 1);
    // Verify the tarball is in the CAS
    let cas_key = compute_cas_key("10c0/abcdef123");
    assert!(store.contains(&ContentHash(cas_key)));
}
```

**Implement** `capture_from_lockfile`:

```rust
/// Capture packages into the CAS using the lockfile's integrity hashes.
/// 
/// 1. Calls `plugin.parse_lockfile()` to get packages with integrity hashes.
/// 2. Calls `plugin.local_pm_cache()` to find where PM tarballs are cached.
/// 3. For each package: compute CAS key from integrity → if already in CAS, skip.
///    Otherwise: find tarball in PM cache → copy to CAS.
/// 4. Write per-package JSON manifest file immediately (survives process exit).
///
/// Returns the number of newly captured packages.
pub fn capture_from_lockfile(
    workspace_root: &Path,
    artifact_dir: &Path,
    plugin: &dyn plugin::EcosystemPlugin,
    store: &LocalArtifactStore,
) -> std::io::Result<usize> {
    let packages = match plugin.parse_lockfile(workspace_root) {
        Some(pkgs) => pkgs,
        None => return Ok(0), // No lockfile → skip
    };
    
    if packages.is_empty() {
        return Ok(0);
    }
    
    let pm_cache = plugin.local_pm_cache(workspace_root);
    
    std::fs::create_dir_all(artifact_dir)?;
    
    let mut captured = 0;
    for pkg in &packages {
        let cas_key_bytes = compute_cas_key(&pkg.integrity);
        let cas_hash = artifact_store::ContentHash(cas_key_bytes);
        
        // Skip if already in CAS (incremental capture)
        if store.contains(&cas_hash) {
            continue;
        }
        
        // Find tarball in PM cache
        let tarball_bytes = match &pm_cache {
            Some(cache_dir) => find_tarball_in_pm_cache(cache_dir, pkg),
            None => None,
        };
        
        let tarball = match tarball_bytes {
            Some(b) => b,
            None => {
                // PM cache miss — skip this package, will need install on restore
                continue;
            }
        };
        
        // Store in CAS
        if let Err(e) = store.put_bytes(&tarball) {
            eprintln!("[rage] artifact capture: failed to store {} — {e}", pkg.name);
            continue;
        }
        
        // Write per-package manifest immediately (atomic, survives process exit)
        write_package_manifest(artifact_dir, pkg, &cas_key_bytes)?;
        captured += 1;
    }
    
    Ok(captured)
}

fn compute_cas_key(integrity: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(integrity.as_bytes());
    *h.finalize().as_bytes()
}

/// Find a tarball in the PM cache directory by scanning for files matching the package.
///
/// Yarn berry: files named `{sanitized-pkg}-npm-{version}-{checksum_fragment}.zip`
/// npm / pnpm: content-addressed directories
fn find_tarball_in_pm_cache(cache_dir: &Path, pkg: &plugin::LockfilePackage) -> Option<Vec<u8>> {
    // For yarn berry: scan the cache directory for files matching the package name and version
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        // Sanitize name for yarn berry filename: @ and / become -
        let sanitized = pkg.name.replace('@', "").replace('/', "-");
        let prefix = format!("{}-npm-{}-", sanitized.trim_start_matches('-'), pkg.version);
        
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if fname_str.starts_with(&prefix) && (fname_str.ends_with(".zip") || fname_str.ends_with(".tgz")) {
                return std::fs::read(entry.path()).ok();
            }
        }
    }
    None
}

fn write_package_manifest(
    artifact_dir: &Path,
    pkg: &plugin::LockfilePackage,
    cas_key: &[u8; 32],
) -> std::io::Result<()> {
    let filename = format!(
        "{}-{}.json",
        pkg.name.replace('/', "+").replace('@', ""),
        pkg.version
    );
    let path = artifact_dir.join(&filename);
    let tmp = path.with_extension("tmp");
    
    let manifest = serde_json::json!({
        "name": pkg.name,
        "version": pkg.version,
        "integrity": pkg.integrity,
        "cas_key": hex::encode(cas_key),
    });
    
    std::fs::write(&tmp, serde_json::to_vec(&manifest)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
```

Add `blake3` and `hex` to `crates/scheduler/Cargo.toml` if not already present.

**Keep** `capture_all_node_modules` for backward compat but add `#[allow(dead_code)]` — it will be removed in a future cleanup.

**Update** `schedule_capture` to try `capture_from_lockfile` first, fall back to old approach:

Actually — **remove the old capture flow entirely** from `schedule_capture`. The only capture that matters now is the one in `run_root_task_two_phase`. The background "capture from build task pathset" was a workaround for the broken install-time approach. Now that install-time capture is correct, remove the pathset capture.

**Run:** `cargo test -p scheduler --lib artifact_capture` — all tests pass.

**Commit:** `feat(scheduler): capture_from_lockfile — CAS key from lockfile integrity hash`

---

### Task 7 — Update runner.rs and integration test

**File:** `crates/scheduler/src/runner.rs`

Update `run_root_task_two_phase` to use the new capture/restore:

1. After successful install: call `capture_from_lockfile(workspace_root, artifact_dir, plugin, store)`.
   (This REPLACES the old `capture_all_node_modules` call.)

2. In the restore path: call `plugin.restore_from_cas(packages, workspace_root, &store)` 
   INSTEAD of `artifact_restore::try_restore_from_cas`.

3. Pre-restoration check: before calling `restore_from_cas`, verify all packages have their
   tarballs in the CAS (pre-flight). If any missing → fall through to reinstall.

**Pre-flight check:**
```rust
let packages = match plugin.parse_lockfile(&task.workspace_root) {
    Some(pkgs) => pkgs,
    None => {
        // No lockfile → can't restore from CAS → always reinstall
        let _ = std::fs::remove_file(&marker);
        // fall through to install
        goto_install();
    }
};

// Pre-flight: all tarballs must be in CAS
let all_present = packages.iter().all(|pkg| {
    let key = compute_cas_key(&pkg.integrity);
    store.contains_key(&key)
});

if !all_present {
    // Partial cache → reinstall (will capture afterward)
    let _ = std::fs::remove_file(&marker);
    // fall through to install
}

// All present → restore
eprintln!("[rage] {}#{} restoring from artifact cache ({} packages)...",
    task.package_name, task.script_name, packages.len());
if let Err(e) = plugin.restore_from_cas(&packages, &task.workspace_root, store.as_ref()) {
    eprintln!("[rage] restore failed: {e} — re-running install");
    let _ = std::fs::remove_file(&marker);
    // fall through to install
} else {
    eprintln!("[rage] {}#{} ✓ (restored from artifact cache)", ...);
    return Ok(());
}
```

**Integration test:**

```rust
#[tokio::test]
async fn workspace_install_restores_from_cas_when_node_modules_deleted() {
    // 1. Create a workspace with yarn berry lock + fake PM cache
    // 2. Run install task → captures to CAS
    // 3. Delete node_modules
    // 4. Run install task again → restores from CAS (no yarn install executed)
    // Assert the marker shows "restored" not "starting"
}
```

**Run:** `cargo test --workspace` — all tests pass (includes ignoring macOS sandbox tests).

**Verify:**
```bash
cargo build --release -p rage-cli
# Prime CAS
rm -f ~/.rage/cache/root-*.done
rm -rf ~/.rage/cache/artifact-packages/
./target/release/rage run build ~/workspace/lage 2>&1 | grep "workspace#install"
# Expected: [rage] workspace#install ✓ Xs (after yarn install + capture)

# Delete node_modules
rm -rf ~/workspace/lage/node_modules

# Restore from CAS
./target/release/rage run build ~/workspace/lage 2>&1 | head -5
# Expected: [rage] workspace#install restoring from artifact cache...
# Expected: [rage] workspace#install ✓ (restored from artifact cache)
```

**Commit:** `feat(scheduler,runner): wire lockfile-CAS capture and tarball restore into root task flow`

**Final push:** push all commits to GitHub.
