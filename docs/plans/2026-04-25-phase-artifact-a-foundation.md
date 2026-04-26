# Observation-Driven Artifact Cache — Phase A: Foundation

> **For execution:** Use `/execute-plan` mode or the subagent-driven-development recipe.

**Goal:** Build the per-package content-addressed `artifact-store` crate, the `PathsetPackageExtractor` in the TypeScript plugin, and add `verify_install_effects()` to the `EcosystemPlugin` trait. **No runner integration in this plan** — that's Phase B.

**Architecture:** A new `crates/artifact-store` crate exposes an `ArtifactStore` trait with a `LocalArtifactStore` impl that stores files content-addressed under `{store_root}/content/{hex[0..2]}/{hex[2..]}/data` and hardlinks (or copies on EXDEV) on restore. A `pathset_extractor` module in `plugin-typescript` parses sandbox pathset reads and produces `PathsetPackageRef`s — pnpm's resolved paths give name+version directly; yarn/npm flat layouts cross-reference the lockfile. The `EcosystemPlugin` trait gains `verify_install_effects(workspace_root) -> bool` (default `true`) so plugins can declare whether their root-task effects (e.g. `node_modules/`) still exist on disk.

**Tech Stack:** Rust 2021, blake3, hex, serde/serde_json, tempfile (dev), regex.

**Prerequisite for:** Phase B — Integration (`docs/plans/2026-04-25-phase-artifact-b-integration.md`).

---

## COE constraints baked into this plan

1. Workspace packages (symlinks to `packages/`) are **excluded** from extraction — they are source files, not installed packages.
2. Cross-device hardlink failure must fall back to `std::fs::copy` silently — never error.
3. Scoped packages (`@types/node`) must preserve their `@scope/name` directory structure when restored.
4. Content-addressed dedup: a `put_bytes` for already-stored content must NOT rewrite the file.

---

## File map

**New files:**
- `crates/artifact-store/Cargo.toml`
- `crates/artifact-store/src/lib.rs`
- `crates/artifact-store/src/local.rs`
- `crates/artifact-store/src/content_hash.rs`
- `crates/artifact-store/src/package_manifest.rs`
- `crates/plugin-typescript/src/pathset_extractor.rs`

**Modified files:**
- `Cargo.toml` (workspace) — add `crates/artifact-store` to `members`
- `crates/plugin/src/lib.rs` — add `verify_install_effects` to `EcosystemPlugin` trait
- `crates/plugin-typescript/src/lib.rs` — declare `mod pathset_extractor` + impl `verify_install_effects`
- `crates/plugin-typescript/Cargo.toml` — add `regex` dependency

---

## Task 1: Skeleton — new `crates/artifact-store` crate

**Files:**
- Create: `crates/artifact-store/Cargo.toml`
- Create: `crates/artifact-store/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

**Step 1: Add the crate to the workspace.**

Edit `/Users/ken/workspace/ms/rage/Cargo.toml`. Inside `[workspace] members = [ ... ]`, add `"crates/artifact-store",` (anywhere in the list — alphabetical placement near `crates/cache` is fine).

**Step 2: Write the crate Cargo.toml.**

Create `crates/artifact-store/Cargo.toml`:
```toml
[package]
name = "artifact-store"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
blake3 = "1"
hex = "0.4"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"

[dev-dependencies]
tempfile = "3"
```

**Step 3: Write a placeholder lib.rs that fails compilation until Task 2.**

Create `crates/artifact-store/src/lib.rs`:
```rust
//! Per-package content-addressed artifact store.
//!
//! Powers observation-driven `node_modules` restoration: after a sandboxed
//! build, we know which packages were read; we capture each into the store
//! and later restore via hardlinks instead of re-running `yarn install`.

pub mod content_hash;
pub mod local;
pub mod package_manifest;

pub use content_hash::ContentHash;
pub use local::LocalArtifactStore;
pub use package_manifest::{PackageArtifact, WorkspacePackageManifest};

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("content not found in store: {0}")]
    NotFound(String),
}

pub trait ArtifactStore: Send + Sync {
    /// Store raw bytes. Returns the content hash. Idempotent: identical bytes
    /// yield the same hash and the second call is a cheap no-op.
    fn put_bytes(&self, bytes: &[u8]) -> Result<ContentHash, ArtifactError>;

    /// Hardlink (or copy on EXDEV) content into `target`. Creates parent dirs
    /// if missing. Errors with `NotFound` if `hash` is not in the store.
    fn link(&self, hash: &ContentHash, target: &Path) -> Result<(), ArtifactError>;

    /// True if this hash is present in the store.
    fn contains(&self, hash: &ContentHash) -> bool;
}
```

Also create empty stubs so `cargo check` doesn't blow up immediately:
```bash
mkdir -p /Users/ken/workspace/ms/rage/crates/artifact-store/src
echo '// stub' > /Users/ken/workspace/ms/rage/crates/artifact-store/src/content_hash.rs
echo '// stub' > /Users/ken/workspace/ms/rage/crates/artifact-store/src/local.rs
echo '// stub' > /Users/ken/workspace/ms/rage/crates/artifact-store/src/package_manifest.rs
```

**Step 4: Write the failing test for `put_bytes` returning a deterministic hash.**

Append to `crates/artifact-store/src/lib.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_bytes_returns_consistent_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(dir.path());
        let h1 = store.put_bytes(b"hello world").unwrap();
        let h2 = store.put_bytes(b"hello world").unwrap();
        assert_eq!(h1, h2, "identical bytes must produce identical hashes");
    }
}
```

**Step 5: Run `cargo check -p artifact-store`. Expected: FAIL with errors about empty `content_hash`/`local`/`package_manifest` modules and missing `LocalArtifactStore::new`.**

Run: `cd /Users/ken/workspace/ms/rage && cargo check -p artifact-store`

Expected output: compile errors (`cannot find type 'ContentHash'` etc.). This is the failing-test state.

**Step 6: Commit the skeleton (compile-failing is OK — it's the RED of TDD).**

Run:
```
git add crates/artifact-store/ Cargo.toml
git commit -m "wip(artifact-store): scaffold crate (failing build — see task 2)"
```

---

## Task 2: Implement `ContentHash` and `LocalArtifactStore::put_bytes`

**Files:**
- Modify: `crates/artifact-store/src/content_hash.rs`
- Modify: `crates/artifact-store/src/local.rs`

**Step 1: Implement `ContentHash`.**

Replace `crates/artifact-store/src/content_hash.rs` with:
```rust
//! Blake3 content hash, displayable as 64-char hex.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn of_file(path: &Path) -> std::io::Result<Self> {
        Ok(Self::of(&std::fs::read(path)?))
    }

    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.hex())
    }
}
```

**Step 2: Implement `LocalArtifactStore::new` + `put_bytes`.**

Replace `crates/artifact-store/src/local.rs` with:
```rust
//! On-disk content-addressed store.
//!
//! Layout: `{root}/content/{hex[0..2]}/{hex[2..]}/data`
//! The two-level hex prefix prevents pathological flat directories on
//! filesystems that scale poorly past ~10k entries per directory.

use crate::{ArtifactError, ArtifactStore, ContentHash};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LocalArtifactStore {
    root: PathBuf,
}

impl LocalArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn content_path(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.hex();
        self.root
            .join("content")
            .join(&hex[..2])
            .join(&hex[2..])
            .join("data")
    }
}

impl ArtifactStore for LocalArtifactStore {
    fn put_bytes(&self, bytes: &[u8]) -> Result<ContentHash, ArtifactError> {
        let hash = ContentHash::of(bytes);
        let dest = self.content_path(&hash);

        // Dedup: if the content is already present, do nothing.
        if dest.is_file() {
            return Ok(hash);
        }

        let parent = dest.parent().expect("content_path has a parent");
        std::fs::create_dir_all(parent)?;

        // Atomic write: write to tempfile in the same dir, then rename.
        let tmp = parent.join(format!(".tmp-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        match std::fs::rename(&tmp, &dest) {
            Ok(()) => {}
            Err(_) if dest.is_file() => {
                // Another writer beat us — that's fine, the content matches.
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) => return Err(e.into()),
        }
        Ok(hash)
    }

    fn link(&self, _hash: &ContentHash, _target: &Path) -> Result<(), ArtifactError> {
        unimplemented!("Task 3")
    }

    fn contains(&self, _hash: &ContentHash) -> bool {
        unimplemented!("Task 4")
    }
}
```

**Step 3: Run the Task 1 test — expect PASS.**

Run: `cargo test -p artifact-store put_bytes_returns_consistent_hash`
Expected: 1 test passes.

**Step 4: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): ContentHash + LocalArtifactStore::put_bytes"`

---

## Task 3: Implement `LocalArtifactStore::link` (with EXDEV fallback)

**Files:**
- Modify: `crates/artifact-store/src/local.rs`
- Modify: `crates/artifact-store/src/lib.rs` (test)

**Step 1: Write the failing tests.**

Append to `crates/artifact-store/src/lib.rs` `tests` module:
```rust
#[test]
fn link_creates_file_with_same_content() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());
    let h = store.put_bytes(b"hello").unwrap();

    let target = dir.path().join("out").join("hello.txt");
    store.link(&h, &target).unwrap();

    assert!(target.is_file());
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");
}

#[test]
fn link_creates_missing_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());
    let h = store.put_bytes(b"data").unwrap();
    let target = dir.path().join("a/b/c/file.bin");
    store.link(&h, &target).unwrap();
    assert!(target.is_file());
}

#[test]
fn link_returns_not_found_when_hash_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());
    let bogus = ContentHash::of(b"never stored");
    let err = store.link(&bogus, &dir.path().join("x")).unwrap_err();
    assert!(matches!(err, ArtifactError::NotFound(_)));
}
```

**Step 2: Run tests — expect FAIL with `unimplemented!`.**

Run: `cargo test -p artifact-store link_`
Expected: 3 panics ("not yet implemented").

**Step 3: Implement `link` with EXDEV fallback.**

In `crates/artifact-store/src/local.rs`, replace the `link` method body:
```rust
    fn link(&self, hash: &ContentHash, target: &Path) -> Result<(), ArtifactError> {
        let src = self.content_path(hash);
        if !src.is_file() {
            return Err(ArtifactError::NotFound(hash.hex()));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // If the target already exists with the right content, no-op.
        if target.is_file() {
            // Best-effort: remove existing so the link/copy succeeds.
            let _ = std::fs::remove_file(target);
        }
        match std::fs::hard_link(&src, target) {
            Ok(()) => Ok(()),
            Err(e) => {
                // EXDEV (cross-device) → silently fall back to copy.
                // We also fall back on EPERM (some filesystems disallow hard links).
                let kind = e.kind();
                let raw = e.raw_os_error();
                let is_exdev = raw == Some(libc_exdev())
                    || matches!(kind, std::io::ErrorKind::Unsupported);
                let is_perm = matches!(kind, std::io::ErrorKind::PermissionDenied);
                if is_exdev || is_perm {
                    std::fs::copy(&src, target)?;
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }
}

#[cfg(unix)]
fn libc_exdev() -> i32 {
    18 // EXDEV on Linux/macOS
}

#[cfg(not(unix))]
fn libc_exdev() -> i32 {
    -1
}
```

**Step 4: Run tests — expect PASS.**

Run: `cargo test -p artifact-store link_`
Expected: all 3 pass.

**Step 5: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): link via hardlink with EXDEV→copy fallback"`

---

## Task 4: Implement `LocalArtifactStore::contains`

**Files:**
- Modify: `crates/artifact-store/src/local.rs`
- Modify: `crates/artifact-store/src/lib.rs` (test)

**Step 1: Write the failing test.**

Append to `crates/artifact-store/src/lib.rs` `tests` module:
```rust
#[test]
fn contains_reflects_put_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());
    let h = ContentHash::of(b"abc");
    assert!(!store.contains(&h));
    store.put_bytes(b"abc").unwrap();
    assert!(store.contains(&h));
}
```

**Step 2: Run — expect FAIL.**

Run: `cargo test -p artifact-store contains_`
Expected: panic ("not yet implemented").

**Step 3: Implement `contains`.**

In `crates/artifact-store/src/local.rs`, replace the `contains` body:
```rust
    fn contains(&self, hash: &ContentHash) -> bool {
        self.content_path(hash).is_file()
    }
```

**Step 4: Run — expect PASS.**

Run: `cargo test -p artifact-store contains_`
Expected: 1 test passes.

**Step 5: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): contains() lookup"`

---

## Task 5: `PackageArtifact` and `WorkspacePackageManifest` types

**Files:**
- Modify: `crates/artifact-store/src/package_manifest.rs`
- Modify: `crates/artifact-store/src/lib.rs` (test)

**Step 1: Write the failing test for serde round-trip.**

Append to `crates/artifact-store/src/lib.rs` `tests` module:
```rust
#[test]
fn manifest_serde_round_trip() {
    use std::path::PathBuf;
    let m = WorkspacePackageManifest {
        captured_at: 1714000000,
        install_fingerprint: "abc123".to_string(),
        packages: vec![PackageArtifact {
            name: "ms".to_string(),
            version: "2.1.3".to_string(),
            files: vec![
                (PathBuf::from("index.js"), ContentHash::of(b"console.log(1)")),
                (PathBuf::from("package.json"), ContentHash::of(b"{}")),
            ],
        }],
    };
    let json = serde_json::to_string(&m).unwrap();
    let m2: WorkspacePackageManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(m.captured_at, m2.captured_at);
    assert_eq!(m.install_fingerprint, m2.install_fingerprint);
    assert_eq!(m.packages.len(), m2.packages.len());
    assert_eq!(m.packages[0].name, m2.packages[0].name);
    assert_eq!(m.packages[0].version, m2.packages[0].version);
    assert_eq!(m.packages[0].files, m2.packages[0].files);
}
```

**Step 2: Run — expect FAIL.**

Run: `cargo test -p artifact-store manifest_serde_round_trip`
Expected: compile error (types don't exist).

**Step 3: Implement the types.**

Replace `crates/artifact-store/src/package_manifest.rs` with:
```rust
//! Per-workspace package manifest — what got captured, what was its content.

use crate::ContentHash;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageArtifact {
    pub name: String,
    pub version: String,
    /// (relative_path_within_package, content_hash) pairs.
    /// Paths are relative to the package root (e.g. "index.js", "lib/foo.js").
    pub files: Vec<(PathBuf, ContentHash)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspacePackageManifest {
    /// Unix timestamp (seconds) when this manifest was last updated.
    pub captured_at: u64,
    /// The root-task install fingerprint this manifest is keyed against.
    /// Switching lockfiles produces a new fingerprint and thus a fresh manifest.
    pub install_fingerprint: String,
    pub packages: Vec<PackageArtifact>,
}
```

**Step 4: Run — expect PASS.**

Run: `cargo test -p artifact-store manifest_serde_round_trip`
Expected: 1 test passes.

**Step 5: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): PackageArtifact + WorkspacePackageManifest types"`

---

## Task 6: `extract_pnpm_packages` — happy path

**Files:**
- Create: `crates/plugin-typescript/src/pathset_extractor.rs`
- Modify: `crates/plugin-typescript/src/lib.rs` (declare module)
- Modify: `crates/plugin-typescript/Cargo.toml` (add `regex` dep)

**Step 1: Add `regex` dependency to plugin-typescript.**

Edit `crates/plugin-typescript/Cargo.toml` `[dependencies]` block:
```toml
[dependencies]
plugin = { path = "../plugin" }
pipeline-config = { path = "../pipeline-config" }
blake3 = "1"
regex = "1"
```

**Step 2: Declare the module in lib.rs.**

Edit `crates/plugin-typescript/src/lib.rs` near the top (after the file-level doc-comment, before `use plugin::...`). Add:
```rust
pub mod pathset_extractor;
```

**Step 3: Create the module file with the type and stub function.**

Create `crates/plugin-typescript/src/pathset_extractor.rs`:
```rust
//! Extract installed-package references from a sandbox pathset.
//!
//! The DYLD sandbox records every `open()` while a build runs. After the
//! build, we scan those reads for `node_modules/` paths and turn them into
//! `(name, version, package_root)` triples that the artifact-store can
//! capture content-addressed.
//!
//! Two layouts are supported:
//!   * **pnpm** — paths look like `node_modules/.pnpm/name@version/node_modules/name/...`.
//!     Both name and version come straight from the path. **Preferred**: the version is
//!     unambiguous without consulting any lockfile.
//!   * **flat (yarn classic / npm)** — paths look like `node_modules/name/...`. The
//!     version must be cross-referenced from the workspace lockfile.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsetPackageRef {
    /// Package name. Scoped packages keep their `@scope/name` form.
    pub name: String,
    pub version: String,
    /// Absolute path to the package's directory in the workspace.
    pub package_root: PathBuf,
}

/// Parse pnpm virtual-store paths from a pathset.
///
/// Recognises paths of the form:
///   `<workspace>/node_modules/.pnpm/<name>@<version>(_<peer>)?/node_modules/<name>/...`
/// and produces a deduplicated list of `PathsetPackageRef`.
///
/// Workspace symlinks (`packages/<x>` references) are NOT recognised by this
/// function — they aren't installed packages and are excluded by design.
pub fn extract_pnpm_packages(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
) -> Vec<PathsetPackageRef> {
    let _ = (pathset_reads, workspace_root);
    Vec::new() // implemented in step 5
}
```

**Step 4: Write the failing test.**

Append to the bottom of `crates/plugin-typescript/src/pathset_extractor.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnpm_resolved_path_extracts_name_and_version() {
        let reads = vec![
            PathBuf::from("/workspace/node_modules/.pnpm/ms@2.1.3/node_modules/ms/index.js"),
            PathBuf::from("/workspace/node_modules/.pnpm/typescript@5.3.2/node_modules/typescript/lib/typescript.js"),
            // The symlink path is captured too — should be ignored (no version in it).
            PathBuf::from("/workspace/node_modules/ms/index.js"),
            // Unrelated path — should be ignored.
            PathBuf::from("/workspace/src/main.ts"),
        ];
        let ws = Path::new("/workspace");
        let refs = extract_pnpm_packages(&reads, ws);
        assert_eq!(refs.len(), 2, "expected ms + typescript, got {refs:?}");
        assert!(refs.iter().any(|r| r.name == "ms" && r.version == "2.1.3"));
        assert!(refs
            .iter()
            .any(|r| r.name == "typescript" && r.version == "5.3.2"));

        // package_root resolution
        let ms = refs.iter().find(|r| r.name == "ms").unwrap();
        assert_eq!(
            ms.package_root,
            PathBuf::from("/workspace/node_modules/.pnpm/ms@2.1.3/node_modules/ms")
        );
    }
}
```

Run: `cargo test -p plugin-typescript pnpm_resolved_path_extracts_name_and_version`
Expected: assertion FAIL (`expected ms + typescript, got []`).

**Step 5: Implement `extract_pnpm_packages`.**

Replace the function body:
```rust
pub fn extract_pnpm_packages(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
) -> Vec<PathsetPackageRef> {
    // pnpm virtual-store path: .pnpm/<name>@<version>[<+/_><peer>]/node_modules/<name>/...
    //   - <name> may be scoped: @scope/pkg → encoded in pnpm as `@scope+pkg`
    //   - We capture the peer-suffix in the regex but discard it; only base version matters.
    let re = regex::Regex::new(
        r"(?x)
        \.pnpm/
        (?P<encoded_name>(?:@[^/+]+\+)?[^@/]+)
        @
        (?P<version>[^/_]+)
        (?:_[^/]+)?           # optional peer-deps suffix
        /node_modules/
        (?P<dir_name>(?:@[^/]+/)?[^/]+)
        /
        ",
    )
    .expect("static regex");

    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out: Vec<PathsetPackageRef> = Vec::new();

    for p in pathset_reads {
        let s = p.to_string_lossy();
        let Some(caps) = re.captures(&s) else { continue };
        let dir_name = caps.name("dir_name").unwrap().as_str().to_string();
        let version = caps.name("version").unwrap().as_str().to_string();

        // The folder under node_modules/ inside the pnpm dir is the canonical
        // package name (already in @scope/name form). Prefer it over the
        // `+`-encoded form.
        if !seen.insert((dir_name.clone(), version.clone())) {
            continue;
        }

        // Reconstruct: workspace_root / node_modules / .pnpm / <encoded>@<version> / node_modules / <dir_name>
        let encoded = caps.name("encoded_name").unwrap().as_str();
        let pnpm_dir = format!("{encoded}@{version}");
        let package_root = workspace_root
            .join("node_modules")
            .join(".pnpm")
            .join(&pnpm_dir)
            .join("node_modules")
            .join(&dir_name);

        out.push(PathsetPackageRef {
            name: dir_name,
            version,
            package_root,
        });
    }
    out
}
```

**Step 6: Run — expect PASS.**

Run: `cargo test -p plugin-typescript pnpm_resolved_path_extracts_name_and_version`
Expected: 1 test passes.

**Step 7: Commit.**

Run: `git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): extract_pnpm_packages — happy path"`

---

## Task 7: `extract_pnpm_packages` — edge cases (scoped, workspace, dedup)

**Files:**
- Modify: `crates/plugin-typescript/src/pathset_extractor.rs` (tests + impl tweaks)

**Step 1: Add the failing edge-case tests.**

Append to the `tests` module in `pathset_extractor.rs`:
```rust
    #[test]
    fn pnpm_scoped_package_extracted_with_full_name() {
        let reads = vec![PathBuf::from(
            "/ws/node_modules/.pnpm/@types+node@20.1.0/node_modules/@types/node/index.d.ts",
        )];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
        let r = &refs[0];
        assert_eq!(r.name, "@types/node");
        assert_eq!(r.version, "20.1.0");
        assert_eq!(
            r.package_root,
            PathBuf::from("/ws/node_modules/.pnpm/@types+node@20.1.0/node_modules/@types/node")
        );
    }

    #[test]
    fn pnpm_peer_dep_suffix_is_stripped_from_version() {
        // pnpm encodes peer-dep variants as `name@ver_peerhash` — we only want `ver`.
        let reads = vec![PathBuf::from(
            "/ws/node_modules/.pnpm/react-dom@18.2.0_react@18.2.0/node_modules/react-dom/index.js",
        )];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "react-dom");
        assert_eq!(refs[0].version, "18.2.0");
    }

    #[test]
    fn pnpm_workspace_symlink_paths_excluded() {
        // pnpm workspace packages live in `packages/` and are linked via `link:` —
        // they never appear under `.pnpm/`, so the regex naturally excludes them.
        let reads = vec![
            PathBuf::from("/ws/packages/my-lib/index.ts"),
            PathBuf::from("/ws/node_modules/@scope/my-lib/index.ts"), // symlink — no `.pnpm/`
        ];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert!(
            refs.is_empty(),
            "workspace packages must NOT be captured, got {refs:?}"
        );
    }

    #[test]
    fn pnpm_deduplicates_repeated_reads() {
        let reads = vec![
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/index.js"),
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/package.json"),
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/lib/util.js"),
        ];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
    }
```

**Step 2: Run — expect at least one FAIL (the regex from Task 6 should already cover scoped + peer-dep + dedup; if all pass already, that's fine, document and move on).**

Run: `cargo test -p plugin-typescript pnpm_`
Expected: all four pnpm tests pass. If any fail, fix the regex / dedup logic in `extract_pnpm_packages` before continuing.

**Step 3: If a test fails — fix.** The regex above already handles `@scope+name`, peer-dep suffix `_…`, and dedup via the `HashSet`. If a fix is needed, tweak the regex/parsing in `extract_pnpm_packages` until all pnpm tests pass.

**Step 4: Run all plugin-typescript tests.**

Run: `cargo test -p plugin-typescript`
Expected: pre-existing tests + 4 new pnpm tests all pass.

**Step 5: Commit.**

Run: `git add crates/plugin-typescript && git commit -m "test(plugin-typescript): pnpm scoped/peer-dep/workspace/dedup edge cases"`

---

## Task 8: `extract_flat_packages` for yarn / npm

**Files:**
- Modify: `crates/plugin-typescript/src/pathset_extractor.rs`

**Step 1: Write the failing test.**

Append to the `tests` module:
```rust
    #[test]
    fn flat_extracts_from_npm_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // Minimal package-lock.json (npm v3-style) with `packages` map.
        std::fs::write(
            ws.join("package-lock.json"),
            r#"{
              "lockfileVersion": 3,
              "packages": {
                "node_modules/ms": { "version": "2.1.3" },
                "node_modules/lodash": { "version": "4.17.21" }
              }
            }"#,
        )
        .unwrap();

        let reads = vec![
            ws.join("node_modules/ms/index.js"),
            ws.join("node_modules/ms/package.json"),
            ws.join("node_modules/lodash/lodash.js"),
            ws.join("node_modules/.pnpm/ignored@1.0.0/node_modules/ignored/x.js"), // not flat
            ws.join("packages/local-pkg/index.ts"),                                 // workspace
        ];
        let refs = extract_flat_packages(&reads, ws, &ws.join("package-lock.json"));
        assert_eq!(refs.len(), 2, "got {refs:?}");
        assert!(refs.iter().any(|r| r.name == "ms" && r.version == "2.1.3"));
        assert!(refs
            .iter()
            .any(|r| r.name == "lodash" && r.version == "4.17.21"));
    }

    #[test]
    fn flat_extracts_from_yarn_classic_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // Minimal yarn.lock v1.
        std::fs::write(
            ws.join("yarn.lock"),
            r#"
ms@^2.1.3:
  version "2.1.3"
  resolved "https://..."
"#,
        )
        .unwrap();
        let reads = vec![ws.join("node_modules/ms/index.js")];
        let refs = extract_flat_packages(&reads, ws, &ws.join("yarn.lock"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "ms");
        assert_eq!(refs[0].version, "2.1.3");
    }

    #[test]
    fn flat_skips_packages_not_in_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("package-lock.json"), r#"{"packages":{}}"#).unwrap();
        let reads = vec![ws.join("node_modules/ghost/index.js")];
        let refs = extract_flat_packages(&reads, ws, &ws.join("package-lock.json"));
        assert!(refs.is_empty());
    }
```

Run: `cargo test -p plugin-typescript flat_`
Expected: compile error (function does not exist).

**Step 2: Implement `extract_flat_packages`.**

Append to `crates/plugin-typescript/src/pathset_extractor.rs` (after `extract_pnpm_packages`):
```rust
/// Extract package refs for flat node_modules layouts (yarn classic, npm).
///
/// Algorithm:
///   1. Find every read of the form `<workspace>/node_modules/<name>/...`
///      (excluding `.pnpm/` and excluding `.bin/`).
///   2. Look up the version of each name in the supplied lockfile.
///   3. Drop packages whose version cannot be determined.
pub fn extract_flat_packages(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    lockfile: &Path,
) -> Vec<PathsetPackageRef> {
    let nm_prefix = workspace_root.join("node_modules");

    // 1) Discover candidate package names from pathset reads.
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in pathset_reads {
        let Ok(rel) = p.strip_prefix(&nm_prefix) else { continue };
        let s = rel.to_string_lossy();
        if s.starts_with(".pnpm/") || s.starts_with(".bin/") || s.starts_with(".cache/") {
            continue;
        }
        // Take the first one or two path components ("@scope/name" or "name").
        let mut comps = rel.components();
        let first = match comps.next() {
            Some(c) => c.as_os_str().to_string_lossy().to_string(),
            None => continue,
        };
        let name = if first.starts_with('@') {
            let Some(second) = comps.next() else { continue };
            format!("{}/{}", first, second.as_os_str().to_string_lossy())
        } else {
            first
        };
        names.insert(name);
    }

    if names.is_empty() {
        return Vec::new();
    }

    // 2) Parse versions out of the lockfile.
    let lock_text = std::fs::read_to_string(lockfile).unwrap_or_default();
    let is_yarn = lockfile
        .file_name()
        .and_then(|s| s.to_str())
        .map(|n| n.ends_with("yarn.lock"))
        .unwrap_or(false);

    let mut out = Vec::new();
    for name in names {
        let version = if is_yarn {
            lookup_yarn_classic_version(&lock_text, &name)
        } else {
            lookup_npm_lock_version(&lock_text, &name)
        };
        if let Some(v) = version {
            out.push(PathsetPackageRef {
                name: name.clone(),
                version: v,
                package_root: nm_prefix.join(&name),
            });
        }
    }
    out
}

/// Find `"node_modules/<name>": { ..., "version": "<v>" }` in package-lock.json
/// without doing full JSON parsing — the lockfile is huge but the structure is
/// stable enough to use string scans.
fn lookup_npm_lock_version(lock_text: &str, name: &str) -> Option<String> {
    let key = format!("\"node_modules/{name}\"");
    let idx = lock_text.find(&key)?;
    let tail = &lock_text[idx..];
    // Find `"version"` within the next ~2KB; cap to avoid pulling versions from
    // unrelated entries in pathological lockfiles.
    let window_end = (tail.len()).min(4096);
    let window = &tail[..window_end];
    let v_idx = window.find("\"version\"")?;
    let after = &window[v_idx..];
    let q1 = after.find('"').and_then(|i| after[i + 1..].find('"').map(|j| (i, j)))?;
    // Skip the "version" key itself: find the *next* `"..."` after the colon.
    let colon = after[q1.0 + 1 + q1.1..].find(':')?;
    let after_colon = &after[q1.0 + 1 + q1.1 + colon..];
    let start = after_colon.find('"')? + 1;
    let rest = &after_colon[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// yarn.lock v1: each entry header is `name@<range>:` and the body has a
/// `version "x.y.z"` line. We find the first header that starts with
/// `<name>@` and take the next `version` value.
fn lookup_yarn_classic_version(lock_text: &str, name: &str) -> Option<String> {
    // Header lines start at column 0. Headers may be quoted: `"@scope/name@^1.2.3":`
    let needles = [format!("\n{name}@"), format!("\n\"{name}@")];
    let head = needles
        .iter()
        .filter_map(|n| lock_text.find(n.as_str()))
        .min()?;
    let after = &lock_text[head..];
    let v_idx = after.find("version ")?;
    let rest = &after[v_idx + "version ".len()..];
    let q = rest.find('"')? + 1;
    let body = &rest[q..];
    let end = body.find('"')?;
    Some(body[..end].to_string())
}
```

**Step 3: Run — expect PASS.**

Run: `cargo test -p plugin-typescript flat_`
Expected: 3 tests pass.

**Step 4: Commit.**

Run: `git add crates/plugin-typescript && git commit -m "feat(plugin-typescript): extract_flat_packages for yarn/npm via lockfile"`

---

## Task 9: Add `verify_install_effects` to `EcosystemPlugin` trait

**Files:**
- Modify: `crates/plugin/src/lib.rs`
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Write the failing test in plugin-typescript.**

Append a new test in the `tests` module of `crates/plugin-typescript/src/lib.rs`:
```rust
    #[test]
    fn verify_install_effects_true_when_node_modules_present_and_nonempty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/ms")).unwrap();
        std::fs::write(dir.path().join("node_modules/ms/index.js"), b"x").unwrap();
        let p = TypeScriptPlugin::new();
        assert!(p.verify_install_effects(dir.path()));
    }

    #[test]
    fn verify_install_effects_false_when_node_modules_missing() {
        let dir = tempdir().unwrap();
        let p = TypeScriptPlugin::new();
        assert!(!p.verify_install_effects(dir.path()));
    }

    #[test]
    fn verify_install_effects_false_when_node_modules_empty() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        let p = TypeScriptPlugin::new();
        assert!(!p.verify_install_effects(dir.path()));
    }
```

Run: `cargo test -p plugin-typescript verify_install_effects`
Expected: compile error (`no method named verify_install_effects`).

**Step 2: Add the trait method with default impl.**

Edit `crates/plugin/src/lib.rs`. After the `infer_root_tasks` method (currently the last one in `trait EcosystemPlugin`), add:
```rust
    /// Returns `true` when the on-disk artifacts left behind by this plugin's
    /// root task(s) still exist. Callers use this on a cache hit to decide
    /// whether the cached state is materially valid — e.g. for TypeScript,
    /// `node_modules/` must be present and non-empty even if the install
    /// fingerprint matches.
    ///
    /// Default returns `true`: ecosystems that have no installable side-effects
    /// (or that can't easily verify them) preserve the existing cache-hit
    /// behaviour.
    fn verify_install_effects(&self, _workspace_root: &Path) -> bool {
        true
    }
```

**Step 3: Implement in TypeScript plugin.**

Edit `crates/plugin-typescript/src/lib.rs`. Inside `impl EcosystemPlugin for TypeScriptPlugin`, after `infer_root_tasks`, add:
```rust
    fn verify_install_effects(&self, workspace_root: &Path) -> bool {
        let nm = workspace_root.join("node_modules");
        match std::fs::read_dir(&nm) {
            Ok(mut iter) => iter.next().is_some(),
            Err(_) => false,
        }
    }
```

**Step 4: Run — expect PASS.**

Run: `cargo test -p plugin-typescript verify_install_effects`
Expected: 3 tests pass.

**Step 5: Confirm workspace-wide compile.**

Run: `cargo check --workspace`
Expected: clean compile (existing plugin implementors get the default impl, no breakage).

**Step 6: Commit.**

Run: `git add crates/plugin crates/plugin-typescript && git commit -m "feat(plugin): EcosystemPlugin::verify_install_effects + TS impl"`

---

## Task 10: `capture_package` — store a directory's files content-addressed

**Files:**
- Modify: `crates/artifact-store/src/lib.rs`

**Step 1: Write the failing test.**

Append to the `tests` module in `crates/artifact-store/src/lib.rs`:
```rust
    #[test]
    fn capture_package_stores_all_files_and_returns_artifact() {
        use std::path::PathBuf;
        let store_dir = tempfile::tempdir().unwrap();
        let pkg_dir = tempfile::tempdir().unwrap();
        // Build a fake `ms@2.1.3` package on disk
        let pkg_root = pkg_dir.path().join("ms");
        std::fs::create_dir_all(pkg_root.join("lib")).unwrap();
        std::fs::write(pkg_root.join("index.js"), b"module.exports = function ms(s){return s}").unwrap();
        std::fs::write(pkg_root.join("package.json"), br#"{"name":"ms","version":"2.1.3"}"#).unwrap();
        std::fs::write(pkg_root.join("lib/util.js"), b"// util").unwrap();

        let store = LocalArtifactStore::new(store_dir.path());
        let pkg_ref = crate::package_manifest::PathsetPackageRef {
            name: "ms".into(),
            version: "2.1.3".into(),
            package_root: pkg_root.clone(),
        };
        let artifact = capture_package(&pkg_ref, &store).unwrap();
        assert_eq!(artifact.name, "ms");
        assert_eq!(artifact.version, "2.1.3");
        assert_eq!(artifact.files.len(), 3);

        // Verify each captured file is in the store with matching content
        for (rel, hash) in &artifact.files {
            assert!(store.contains(hash), "missing in store: {rel:?}");
            let on_disk = std::fs::read(pkg_root.join(rel)).unwrap();
            assert_eq!(*hash, ContentHash::of(&on_disk));
        }

        // The relative paths should NOT contain absolute prefixes
        let rels: Vec<&PathBuf> = artifact.files.iter().map(|(p, _)| p).collect();
        assert!(rels.iter().any(|p| p.to_string_lossy() == "index.js"));
        assert!(rels.iter().any(|p| p.to_string_lossy() == "package.json"));
        assert!(rels.iter().any(|p| p.ends_with("util.js")));
    }
```

**Step 2: Add `PathsetPackageRef` re-export — wait, this type lives in `plugin-typescript`, but `capture_package` needs to live in `artifact-store` to avoid a back-edge dependency. Mirror it.**

Edit `crates/artifact-store/src/package_manifest.rs` and append (so `capture_package` accepts a struct local to `artifact-store`, no dependency on the TS plugin):
```rust
/// Reference to a single installed package on disk. A free-standing copy of
/// `plugin_typescript::pathset_extractor::PathsetPackageRef` so the
/// `artifact-store` crate stays free of plugin dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsetPackageRef {
    pub name: String,
    pub version: String,
    pub package_root: std::path::PathBuf,
}
```

Update the re-exports near the top of `crates/artifact-store/src/lib.rs`:
```rust
pub use package_manifest::{PackageArtifact, PathsetPackageRef, WorkspacePackageManifest};
```

**Step 3: Implement `capture_package`.**

Append to `crates/artifact-store/src/lib.rs`:
```rust
/// Walk a package directory, content-address every regular file into the store,
/// and return a `PackageArtifact` whose `files` list is sorted by relative path
/// for determinism.
///
/// Errors only on store I/O failures; missing files are skipped silently.
pub fn capture_package(
    pkg_ref: &PathsetPackageRef,
    store: &dyn ArtifactStore,
) -> Result<PackageArtifact, ArtifactError> {
    use std::path::PathBuf;
    let mut files: Vec<(PathBuf, ContentHash)> = Vec::new();

    fn walk(
        root: &std::path::Path,
        cur: &std::path::Path,
        store: &dyn ArtifactStore,
        out: &mut Vec<(PathBuf, ContentHash)>,
    ) -> Result<(), ArtifactError> {
        let read = match std::fs::read_dir(cur) {
            Ok(r) => r,
            Err(_) => return Ok(()), // best-effort
        };
        for entry in read.flatten() {
            let path = entry.path();
            // Follow regular files only — symlinks and devices are not captured.
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                walk(root, &path, store, out)?;
            } else if ft.is_file() {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let h = store.put_bytes(&bytes)?;
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.push((rel, h));
            }
            // symlinks intentionally skipped
        }
        Ok(())
    }

    walk(&pkg_ref.package_root, &pkg_ref.package_root, store, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(PackageArtifact {
        name: pkg_ref.name.clone(),
        version: pkg_ref.version.clone(),
        files,
    })
}
```

**Step 4: Run — expect PASS.**

Run: `cargo test -p artifact-store capture_package_stores_all_files_and_returns_artifact`
Expected: 1 test passes.

**Step 5: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): capture_package — content-address a package tree"`

---

## Task 11: `restore_package` — hardlink files back into a target dir

**Files:**
- Modify: `crates/artifact-store/src/lib.rs`

**Step 1: Write the failing test.**

Append to the `tests` module:
```rust
    #[test]
    fn restore_package_recreates_directory_structure() {
        let store_dir = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let pkg_root = src_dir.path().join("ms");
        std::fs::create_dir_all(pkg_root.join("lib")).unwrap();
        std::fs::write(pkg_root.join("index.js"), b"INDEX").unwrap();
        std::fs::write(pkg_root.join("lib/util.js"), b"UTIL").unwrap();

        let artifact = capture_package(
            &PathsetPackageRef {
                name: "ms".into(),
                version: "2.1.3".into(),
                package_root: pkg_root,
            },
            &store,
        )
        .unwrap();

        // Restore into dst/node_modules
        let nm = dst_dir.path().join("node_modules");
        restore_package(&artifact, &nm, &store).unwrap();

        assert_eq!(std::fs::read(nm.join("ms/index.js")).unwrap(), b"INDEX");
        assert_eq!(std::fs::read(nm.join("ms/lib/util.js")).unwrap(), b"UTIL");
    }

    #[test]
    fn restore_package_preserves_scoped_directory_structure() {
        let store_dir = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let h = store.put_bytes(b"declare const x: number;").unwrap();
        let artifact = PackageArtifact {
            name: "@types/node".into(),
            version: "20.1.0".into(),
            files: vec![(std::path::PathBuf::from("index.d.ts"), h)],
        };
        let nm = dst.path().join("node_modules");
        restore_package(&artifact, &nm, &store).unwrap();

        assert!(
            nm.join("@types/node/index.d.ts").is_file(),
            "scoped package must preserve @scope/name dir"
        );
    }
```

Run: `cargo test -p artifact-store restore_package`
Expected: compile error (function does not exist).

**Step 2: Implement `restore_package`.**

Append to `crates/artifact-store/src/lib.rs`:
```rust
/// Materialize a previously-captured package into `target_dir/<name>/...`.
///
/// `target_dir` is typically `<workspace_root>/node_modules`. Scoped packages
/// (e.g. `@types/node`) preserve their `@scope/name` directory layout because
/// `name` is used verbatim as a path component.
///
/// Files are hardlinked (or copied on EXDEV) — extremely fast and disk-cheap.
pub fn restore_package(
    artifact: &PackageArtifact,
    target_dir: &Path,
    store: &dyn ArtifactStore,
) -> Result<(), ArtifactError> {
    let pkg_root = target_dir.join(&artifact.name);
    for (rel, hash) in &artifact.files {
        let dest = pkg_root.join(rel);
        store.link(hash, &dest)?;
    }
    Ok(())
}
```

**Step 3: Run — expect PASS.**

Run: `cargo test -p artifact-store restore_package`
Expected: 2 tests pass.

**Step 4: Commit.**

Run: `git add crates/artifact-store && git commit -m "feat(artifact-store): restore_package preserves scoped layout"`

---

## Task 12: End-to-end roundtrip + workspace-wide checks

**Files:**
- Modify: `crates/artifact-store/src/lib.rs` (one final test)

**Step 1: Add the round-trip test.**

Append to the `tests` module:
```rust
    #[test]
    fn end_to_end_capture_delete_restore() {
        // Build a multi-package source tree, capture, delete sources, restore
        // into a fresh dir, and verify byte-identical content.
        let store_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let store = LocalArtifactStore::new(store_dir.path());

        let pkgs = [
            ("ms", "2.1.3"),
            ("lodash", "4.17.21"),
            ("@types/node", "20.1.0"),
        ];
        let mut captured = Vec::new();
        for (name, version) in pkgs {
            let root = src.path().join(name); // works for scoped because mkdir -p
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("index.js"), format!("// {name}@{version}").as_bytes()).unwrap();
            std::fs::write(
                root.join("package.json"),
                format!(r#"{{"name":"{name}","version":"{version}"}}"#).as_bytes(),
            )
            .unwrap();
            let artifact = capture_package(
                &PathsetPackageRef {
                    name: name.into(),
                    version: version.into(),
                    package_root: root,
                },
                &store,
            )
            .unwrap();
            captured.push(artifact);
        }

        // Wipe sources — the store is the only remaining copy.
        drop(src);

        // Restore each into dst/node_modules
        let nm = dst.path().join("node_modules");
        for artifact in &captured {
            restore_package(artifact, &nm, &store).unwrap();
        }

        // Verify content
        for artifact in &captured {
            for (rel, hash) in &artifact.files {
                let p = nm.join(&artifact.name).join(rel);
                let bytes = std::fs::read(&p).unwrap();
                assert_eq!(ContentHash::of(&bytes), *hash, "file {p:?} content mismatch");
            }
        }
    }
```

**Step 2: Run.**

Run: `cargo test -p artifact-store end_to_end_capture_delete_restore`
Expected: 1 test passes.

**Step 3: Run the entire workspace test suite.**

Run: `cargo test --workspace`
Expected: all tests pass (including the existing plugin/scheduler/cache suites — Plan A introduces no integration changes).

**Step 4: Run clippy.**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean output. Fix any warnings before committing.

**Step 5: Final commit.**

Run:
```
git add -A
git commit -m "feat(artifact-store,plugin): observation-driven artifact cache foundation

PathsetPackageExtractor (pnpm + flat) + LocalArtifactStore + verify_install_effects.
Phase A — see docs/plans/2026-04-25-phase-artifact-a-foundation.md.
Phase B (runner integration) is the next plan."
```

---

## Definition of Done

- [ ] `crates/artifact-store/` exists with `LocalArtifactStore` impl
- [ ] `put_bytes`, `link` (with EXDEV→copy fallback), `contains` all work
- [ ] `capture_package` walks a directory and stores all files content-addressed
- [ ] `restore_package` hardlinks files back, preserving scoped layout
- [ ] `extract_pnpm_packages` handles plain, scoped, peer-deps, dedup, workspace-exclusion
- [ ] `extract_flat_packages` cross-references npm and yarn classic lockfiles
- [ ] `EcosystemPlugin::verify_install_effects` exists with `true` default
- [ ] TypeScript plugin overrides `verify_install_effects` to check `node_modules/` is non-empty
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] All commits are small and incremental — one per task

**Plan B unblocked:** runner integration can now consume these primitives.
