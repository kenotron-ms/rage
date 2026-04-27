# Postinstall Cache v2 — Manifest + Hardlink Model

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Replace the broken HashMap/base64 snapshot implementation with a per-file CAS manifest model that preserves permissions, handles symlinks, and never OOMs on large packages.

**Architecture:** Walk the package directory before and after running the postinstall script. Hash each file individually with blake3. Store changed files as separate CAS entries keyed by content hash (using `put_bytes_keyed`). Store a small JSON manifest (path → hash + mode + kind) keyed by the postinstall CAS key. Restore via hardlinks from CAS + `set_permissions`. Empty delta → write nothing, return `Ok(false)`.

**Tech Stack:** Rust, walkdir, blake3, serde + serde_json, std::os::unix::fs (hardlinks + symlinks + permissions)

---

## Verified codebase facts (read before starting)

- `LocalArtifactStore` field is named `root: PathBuf` (not `store_root`)
- CAS layout: `{root}/content/{hex[..2]}/{hex[2..]}/data` — three components plus `/data` filename
- Existing store methods to use: `put_bytes_keyed([u8;32], &[u8]) -> io::Result<()>` and `get_bytes_by_raw_key(&[u8;32]) -> io::Result<Option<Vec<u8>>>`
- `serde` with `features = ["derive"]` is NOT yet in `crates/scheduler/Cargo.toml` — must be added
- `hex`, `blake3`, `walkdir`, `serde_json` are already in `crates/scheduler/Cargo.toml`
- `plugin::PostinstallTask` has fields: `package_name`, `version`, `tarball_integrity`, `script`, `cwd`
- Existing tests use `#[allow(deprecated)]` before `artifact_store::LocalArtifactStore::new(...)`

---

## Non-negotiable constraints

- Empty delta MUST return `Ok(false)` — never claim a cache hit for a no-op
- Permissions MUST be restored — `mode` bits from manifest, set via `set_permissions` after hardlink
- Symlinks MUST be handled — record target, restore as symlink, never silently drop
- No file contents in RAM beyond one file at a time — read → hash → store → drop bytes
- Each changed file is its own CAS entry — not bundled into a blob
- The manifest is a small JSON index stored under the postinstall key — not a blob of file contents

---

## Task 1: Add `cas_file_path` to `LocalArtifactStore`

**Files:**
- Modify: `crates/artifact-store/src/local.rs`

### Step 1: Write the failing test

Add this to the existing `#[cfg(test)]` block at the bottom of `crates/artifact-store/src/local.rs` (inside the `#[cfg(test)]` — not creating a new one):

```rust
#[test]
fn cas_file_path_matches_expected_layout() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());

    // All-zero hash → hex is 64 zeros
    let hash = [0u8; 32];
    let path = store.cas_file_path(&hash);

    let hex = "0000000000000000000000000000000000000000000000000000000000000000";
    let expected = dir
        .path()
        .join("content")
        .join(&hex[..2])          // "00"
        .join(&hex[2..])          // 62 zeros
        .join("data");

    assert_eq!(
        path, expected,
        "cas_file_path must follow {{root}}/content/{{hex[..2]}}/{{hex[2..]}}/data layout"
    );
}

#[test]
fn cas_file_path_matches_put_bytes_keyed_location() {
    // Verify that cas_file_path returns the same path that put_bytes_keyed writes to.
    let dir = tempfile::tempdir().unwrap();
    let store = LocalArtifactStore::new(dir.path());

    let key = *blake3::hash(b"test content").as_bytes();
    store.put_bytes_keyed(key, b"test content").unwrap();

    let path = store.cas_file_path(&key);
    assert!(
        path.is_file(),
        "cas_file_path must point to where put_bytes_keyed wrote the file"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"test content",
        "file content mismatch"
    );
}
```

### Step 2: Run tests to verify they fail

```
cd /Users/ken/workspace/ms/rage
cargo test -p artifact-store cas_file_path 2>&1 | tail -20
```

Expected: FAIL — `no method named 'cas_file_path'`

### Step 3: Implement `cas_file_path`

Add this method inside the second `impl LocalArtifactStore` block in `crates/artifact-store/src/local.rs` (the block starting at line 97 that contains `put_bytes_keyed`):

```rust
/// Return the on-disk path for a 32-byte hash in the CAS.
///
/// Layout mirrors `put_bytes_keyed` and `content_path`:
/// `{root}/content/{hex[..2]}/{hex[2..]}/data`
///
/// The file may or may not exist yet — callers use this for
/// hardlinking into a target directory without re-reading the bytes.
pub fn cas_file_path(&self, hash: &[u8; 32]) -> PathBuf {
    let hex = hex::encode(hash);
    self.root
        .join("content")
        .join(&hex[..2])
        .join(&hex[2..])
        .join("data")
}
```

### Step 4: Run tests to verify they pass

```
cargo test -p artifact-store cas_file_path 2>&1 | tail -20
```

Expected: PASS — both tests green

### Step 5: Commit

```
git add crates/artifact-store/src/local.rs
git commit -m "feat(artifact-store): add cas_file_path helper for hardlink restore"
```

---

## Task 2: Add `serde` dep + define new types in `postinstall_cache.rs`

**Files:**
- Modify: `crates/scheduler/Cargo.toml`
- Modify: `crates/scheduler/src/postinstall_cache.rs`

### Step 1: Write the failing test

Add this test module to `crates/scheduler/src/postinstall_cache.rs` (before the final `}`):

```rust
#[cfg(test)]
mod manifest_type_tests {
    use super::*;

    #[test]
    fn regular_entry_serde_roundtrip() {
        let entry = ManifestEntry {
            rel_path: std::path::PathBuf::from("bin/foo.node"),
            content_hash: [1u8; 32],
            mode: 0o755,
            kind: FileKind::Regular,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ManifestEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rel_path, entry.rel_path, "rel_path mismatch");
        assert_eq!(back.content_hash, entry.content_hash, "content_hash mismatch");
        assert_eq!(back.mode, entry.mode, "mode mismatch");
        assert_eq!(back.kind, entry.kind, "kind mismatch");
    }

    #[test]
    fn symlink_entry_serde_roundtrip() {
        let target = std::path::PathBuf::from("../../real/path");
        let entry = ManifestEntry {
            rel_path: std::path::PathBuf::from("link"),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(target.clone()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ManifestEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content_hash, [0u8; 32], "symlink hash must be zeroed");
        match back.kind {
            FileKind::Symlink(t) => assert_eq!(t, target, "symlink target mismatch"),
            FileKind::Regular => panic!("expected Symlink kind, got Regular"),
        }
    }

    #[test]
    fn empty_manifest_serializes_as_empty_array() {
        let manifest: PostinstallManifest = vec![];
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PostinstallManifest = serde_json::from_str(&json).unwrap();
        assert!(back.is_empty(), "empty manifest must round-trip as empty");
    }
}
```

### Step 2: Run test to verify it fails

```
cargo test -p scheduler manifest_type_tests 2>&1 | tail -20
```

Expected: FAIL — compile error `FileKind`, `ManifestEntry`, `PostinstallManifest` not defined

### Step 3: Add `serde` dependency

In `crates/scheduler/Cargo.toml`, add serde alongside serde_json:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

(Replace the existing `serde_json = "1"` line with both lines, keeping serde_json intact.)

### Step 4: Add the new types to `postinstall_cache.rs`

Add these type definitions near the top of `crates/scheduler/src/postinstall_cache.rs`, after the existing `use` statements and before `compute_delta`:

```rust
/// Whether a filesystem entry is a regular file or a symlink.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileKind {
    Regular,
    Symlink(std::path::PathBuf),
}

/// One entry in a postinstall manifest — describes a single file or symlink
/// relative to the package directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the package directory (e.g. `bin/esbuild`).
    pub rel_path: std::path::PathBuf,
    /// Blake3 hash of the file's contents. Zeroed (`[0u8; 32]`) for symlinks.
    pub content_hash: [u8; 32],
    /// Unix permission bits (`st_mode & 0o777`). Zero for symlinks.
    pub mode: u32,
    /// Whether this entry is a regular file or a symlink (with its target).
    pub kind: FileKind,
}

/// A postinstall manifest is a list of changed or new entries in the package directory.
pub type PostinstallManifest = Vec<ManifestEntry>;
```

### Step 5: Run test to verify it passes

```
cargo test -p scheduler manifest_type_tests 2>&1 | tail -20
```

Expected: PASS

### Step 6: Commit

```
git add crates/scheduler/Cargo.toml crates/scheduler/src/postinstall_cache.rs
git commit -m "feat(scheduler): define ManifestEntry/FileKind types for postinstall cache v2"
```

---

## Task 3: Implement `capture_dir`

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

### Step 1: Write the failing tests

Add this module to `postinstall_cache.rs` (before the final `}`):

```rust
#[cfg(test)]
mod capture_dir_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn nonexistent_dir_returns_empty() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let result = capture_dir(
            &store_dir.path().join("does_not_exist"),
            &store,
        )
        .unwrap();
        assert!(result.is_empty(), "nonexistent dir must yield empty manifest");
    }

    #[test]
    fn regular_file_produces_correct_hash_and_mode() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let content = b"hello world";
        let file = pkg_dir.path().join("hello.txt");
        std::fs::write(&file, content).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1, "expected 1 entry");

        let entry = &manifest[0];
        assert_eq!(entry.rel_path, std::path::PathBuf::from("hello.txt"));
        assert_eq!(entry.kind, FileKind::Regular);
        assert_eq!(entry.mode, 0o644);

        let expected_hash = *blake3::hash(content).as_bytes();
        assert_eq!(entry.content_hash, expected_hash, "content hash mismatch");

        // Verify bytes made it into CAS
        assert!(
            store.cas_file_path(&entry.content_hash).is_file(),
            "file must be stored in CAS"
        );
    }

    #[test]
    fn executable_mode_preserved() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let file = pkg_dir.path().join("runner");
        std::fs::write(&file, b"#!/bin/sh\necho hi").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].mode, 0o755, "executable bit must be captured");
    }

    #[test]
    fn symlink_entry_has_zero_hash_and_correct_target() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        // Create a real file and a symlink pointing to it
        std::fs::write(pkg_dir.path().join("real.txt"), b"data").unwrap();
        std::os::unix::fs::symlink("real.txt", pkg_dir.path().join("link.txt")).unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();

        // Find symlink entry
        let sym = manifest
            .iter()
            .find(|e| e.rel_path == std::path::PathBuf::from("link.txt"))
            .expect("symlink entry must appear in manifest");

        assert_eq!(sym.content_hash, [0u8; 32], "symlink hash must be zeroed");
        assert_eq!(sym.mode, 0, "symlink mode must be zero");
        match &sym.kind {
            FileKind::Symlink(target) => {
                assert_eq!(target, &std::path::PathBuf::from("real.txt"))
            }
            FileKind::Regular => panic!("expected Symlink kind"),
        }
    }

    #[test]
    fn nested_file_rel_path_is_relative() {
        let pkg_dir = tempfile::tempdir().unwrap();
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        std::fs::create_dir_all(pkg_dir.path().join("bin")).unwrap();
        std::fs::write(pkg_dir.path().join("bin/esbuild"), b"ELF").unwrap();

        let manifest = capture_dir(pkg_dir.path(), &store).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(
            manifest[0].rel_path,
            std::path::PathBuf::from("bin/esbuild"),
            "nested file rel_path must be relative to dir"
        );
    }
}
```

### Step 2: Run tests to verify they fail

```
cargo test -p scheduler capture_dir_tests 2>&1 | tail -20
```

Expected: FAIL — `capture_dir` not defined

### Step 3: Implement `capture_dir`

Add this function to `postinstall_cache.rs` (after the existing `use` statements, before the old `compute_delta` block):

```rust
/// Walk `dir` recursively and capture every regular file and symlink into
/// `store`. Returns a [`PostinstallManifest`] describing all entries found.
///
/// For regular files: reads bytes, computes blake3 hash, stores bytes in CAS
/// under that hash via [`artifact_store::LocalArtifactStore::put_bytes_keyed`].
///
/// For symlinks: records the link target; never follows the link.
///
/// Directories and unreadable entries are silently skipped.
/// Returns an empty manifest if `dir` does not exist.
pub fn capture_dir(
    dir: &std::path::Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<PostinstallManifest> {
    use std::os::unix::fs::PermissionsExt;

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut manifest = PostinstallManifest::new();

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let file_type = entry.file_type();

        // Skip directories — only files and symlinks go in the manifest.
        if file_type.is_dir() {
            continue;
        }

        let abs = entry.path();
        let rel = match abs.strip_prefix(dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };

        if file_type.is_symlink() {
            let target = match std::fs::read_link(abs) {
                Ok(t) => t,
                Err(_) => continue,
            };
            manifest.push(ManifestEntry {
                rel_path: rel,
                content_hash: [0u8; 32],
                mode: 0,
                kind: FileKind::Symlink(target),
            });
        } else if file_type.is_file() {
            let bytes = match std::fs::read(abs) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let content_hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
            // Store in CAS — deduplicates if already present.
            if let Err(_) = store.put_bytes_keyed(content_hash, &bytes) {
                continue; // silently skip on CAS write error
            }
            let mode = match abs.metadata() {
                Ok(m) => m.permissions().mode() & 0o777,
                Err(_) => 0o644,
            };
            manifest.push(ManifestEntry {
                rel_path: rel,
                content_hash,
                mode,
                kind: FileKind::Regular,
            });
        }
    }

    Ok(manifest)
}
```

### Step 4: Run tests to verify they pass

```
cargo test -p scheduler capture_dir_tests 2>&1 | tail -20
```

Expected: PASS — all 5 tests green

### Step 5: Commit

```
git add crates/scheduler/src/postinstall_cache.rs
git commit -m "feat(scheduler): implement capture_dir for postinstall cache v2"
```

---

## Task 4: Implement `diff_manifests`

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

### Step 1: Write the failing tests

Add this module to `postinstall_cache.rs`:

```rust
#[cfg(test)]
mod diff_manifests_tests {
    use super::*;

    fn reg(path: &str, hash: [u8; 32], mode: u32) -> ManifestEntry {
        ManifestEntry {
            rel_path: std::path::PathBuf::from(path),
            content_hash: hash,
            mode,
            kind: FileKind::Regular,
        }
    }

    fn lnk(path: &str, target: &str) -> ManifestEntry {
        ManifestEntry {
            rel_path: std::path::PathBuf::from(path),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(std::path::PathBuf::from(target)),
        }
    }

    #[test]
    fn both_empty_yields_empty() {
        let delta = diff_manifests(&[], &[]);
        assert!(delta.is_empty(), "both empty → empty delta");
    }

    #[test]
    fn new_file_in_after_is_included() {
        let before = vec![];
        let after = vec![reg("a.txt", [1u8; 32], 0o644)];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "new file must appear in delta");
        assert_eq!(delta[0].rel_path, std::path::PathBuf::from("a.txt"));
    }

    #[test]
    fn unchanged_file_excluded() {
        let entry = reg("a.txt", [1u8; 32], 0o644);
        let before = vec![entry.clone()];
        let after = vec![entry];
        let delta = diff_manifests(&before, &after);
        assert!(delta.is_empty(), "unchanged file must NOT appear in delta");
    }

    #[test]
    fn changed_hash_included() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let after = vec![reg("a.txt", [2u8; 32], 0o644)];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "changed hash must appear in delta");
        assert_eq!(delta[0].content_hash, [2u8; 32]);
    }

    #[test]
    fn changed_mode_included() {
        let before = vec![reg("a.txt", [1u8; 32], 0o644)];
        let after = vec![reg("a.txt", [1u8; 32], 0o755)];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "changed mode must appear in delta");
        assert_eq!(delta[0].mode, 0o755);
    }

    #[test]
    fn deletion_not_tracked() {
        // File present in before but absent in after → NOT in delta
        let before = vec![reg("gone.txt", [1u8; 32], 0o644)];
        let after = vec![];
        let delta = diff_manifests(&before, &after);
        assert!(delta.is_empty(), "deletions are not tracked in the delta");
    }

    #[test]
    fn changed_symlink_target_included() {
        let before = vec![lnk("link", "old-target")];
        let after = vec![lnk("link", "new-target")];
        let delta = diff_manifests(&before, &after);
        assert_eq!(delta.len(), 1, "changed symlink target must appear in delta");
        match &delta[0].kind {
            FileKind::Symlink(t) => assert_eq!(t, &std::path::PathBuf::from("new-target")),
            FileKind::Regular => panic!("wrong kind"),
        }
    }

    #[test]
    fn unchanged_symlink_excluded() {
        let entry = lnk("link", "target");
        let delta = diff_manifests(&[entry.clone()], &[entry]);
        assert!(delta.is_empty(), "unchanged symlink must NOT appear in delta");
    }
}
```

### Step 2: Run tests to verify they fail

```
cargo test -p scheduler diff_manifests_tests 2>&1 | tail -20
```

Expected: FAIL — `diff_manifests` not defined

### Step 3: Implement `diff_manifests`

Add this function to `postinstall_cache.rs`:

```rust
/// Return the subset of `after` whose entries are either new (not in `before`)
/// or changed (different `content_hash`, `mode`, or `kind`) compared to `before`.
///
/// Deletions — entries present in `before` but absent in `after` — are NOT returned.
/// Returns an empty `Vec` when nothing changed.
pub fn diff_manifests(
    before: &PostinstallManifest,
    after: &PostinstallManifest,
) -> PostinstallManifest {
    // Build a lookup from rel_path → (hash, mode, kind) for the before snapshot.
    let before_map: std::collections::HashMap<&std::path::PathBuf, (&[u8; 32], u32, &FileKind)> =
        before
            .iter()
            .map(|e| (&e.rel_path, (&e.content_hash, e.mode, &e.kind)))
            .collect();

    after
        .iter()
        .filter(|e| {
            match before_map.get(&e.rel_path) {
                // Same path, same hash + mode + kind → unchanged, exclude.
                Some((hash, mode, kind)) => {
                    *hash != &e.content_hash || *mode != e.mode || **kind != e.kind
                }
                // Not present in before → new, include.
                None => true,
            }
        })
        .cloned()
        .collect()
}
```

### Step 4: Run tests to verify they pass

```
cargo test -p scheduler diff_manifests_tests 2>&1 | tail -20
```

Expected: PASS — all 8 tests green

### Step 5: Commit

```
git add crates/scheduler/src/postinstall_cache.rs
git commit -m "feat(scheduler): implement diff_manifests for postinstall cache v2"
```

---

## Task 5: Implement `store_manifest`

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

### Step 1: Write the failing tests

Add this module:

```rust
#[cfg(test)]
mod store_manifest_tests {
    use super::*;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn empty_delta_returns_false_and_writes_nothing() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [7u8; 32];

        let result = store_manifest(&key, &[], &store).unwrap();
        assert!(!result, "empty delta must return Ok(false)");
        assert!(
            !store.contains_raw_key(&key),
            "empty delta must NOT write anything to CAS"
        );
    }

    #[test]
    fn non_empty_delta_returns_true_and_cas_entry_readable() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [42u8; 32];

        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("bin/tool"),
            content_hash: [1u8; 32],
            mode: 0o755,
            kind: FileKind::Regular,
        }];

        let result = store_manifest(&key, &delta, &store).unwrap();
        assert!(result, "non-empty delta must return Ok(true)");
        assert!(
            store.contains_raw_key(&key),
            "CAS must have an entry for the key after store_manifest"
        );

        // Must be valid JSON that deserializes back to the same manifest
        let bytes = store.get_bytes_by_raw_key(&key).unwrap().unwrap();
        let back: PostinstallManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.len(), 1, "deserialized manifest must have 1 entry");
        assert_eq!(back[0].rel_path, std::path::PathBuf::from("bin/tool"));
        assert_eq!(back[0].mode, 0o755);
    }

    #[test]
    fn idempotent_write_same_key_does_not_error() {
        // put_bytes_keyed skips if already present — calling store_manifest twice
        // with the same key and same delta must not return an error.
        let store_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());
        let key = [99u8; 32];
        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("a.txt"),
            content_hash: [2u8; 32],
            mode: 0o644,
            kind: FileKind::Regular,
        }];

        store_manifest(&key, &delta, &store).unwrap();
        store_manifest(&key, &delta, &store).unwrap(); // second call must not error
    }
}
```

### Step 2: Run tests to verify they fail

```
cargo test -p scheduler store_manifest_tests 2>&1 | tail -20
```

Expected: FAIL — `store_manifest` not defined

### Step 3: Implement `store_manifest`

```rust
/// Serialize `delta` as JSON and store it in `store` under `key`.
///
/// Returns `Ok(false)` without writing anything if `delta` is empty —
/// this prevents spurious cache hits for postinstall scripts that produce
/// no output files.
///
/// Returns `Ok(true)` when the manifest was successfully persisted.
///
/// Note: individual file bytes are NOT stored here. They must already be
/// in the CAS from a prior call to [`capture_dir`].
pub fn store_manifest(
    key: &[u8; 32],
    delta: &PostinstallManifest,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    if delta.is_empty() {
        return Ok(false);
    }
    let json = serde_json::to_vec(delta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    store.put_bytes_keyed(*key, &json)?;
    Ok(true)
}
```

### Step 4: Run tests to verify they pass

```
cargo test -p scheduler store_manifest_tests 2>&1 | tail -20
```

Expected: PASS — all 3 tests green

### Step 5: Commit

```
git add crates/scheduler/src/postinstall_cache.rs
git commit -m "feat(scheduler): implement store_manifest for postinstall cache v2"
```

---

## Task 6: Implement `restore_manifest`

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

### Step 1: Write the failing tests

Add this module:

```rust
#[cfg(test)]
mod restore_manifest_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[allow(deprecated)]
    fn make_store(dir: &std::path::Path) -> artifact_store::LocalArtifactStore {
        artifact_store::LocalArtifactStore::new(dir)
    }

    #[test]
    fn returns_false_when_key_not_in_cas() {
        let store_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let result = restore_manifest(&[0u8; 32], target_dir.path(), &store).unwrap();
        assert!(!result, "missing key must return Ok(false)");
    }

    #[test]
    fn roundtrip_regular_file_content_and_mode() {
        let store_dir = tempfile::tempdir().unwrap();
        let pkg_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        // Write file into CAS manually (as capture_dir would)
        let content = b"binary content";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        // Build and store a manifest
        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("lib/foo.node"),
            content_hash: hash,
            mode: 0o755,
            kind: FileKind::Regular,
        }];
        let key = [10u8; 32];
        store_manifest(&key, &delta, &store).unwrap();

        // Capture before restoring so we can verify the pkg_dir reference path
        let result = restore_manifest(&key, target_dir.path(), &store).unwrap();
        assert!(result, "valid key must return Ok(true)");

        let restored = target_dir.path().join("lib/foo.node");
        assert!(restored.is_file(), "restored file must exist");
        assert_eq!(std::fs::read(&restored).unwrap(), content, "content mismatch");
    }

    #[test]
    fn executable_permission_preserved() {
        let store_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let content = b"#!/bin/sh\necho hi";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("bin/runner"),
            content_hash: hash,
            mode: 0o755,
            kind: FileKind::Regular,
        }];
        let key = [20u8; 32];
        store_manifest(&key, &delta, &store).unwrap();

        restore_manifest(&key, target_dir.path(), &store).unwrap();

        let restored = target_dir.path().join("bin/runner");
        let actual_mode = restored.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(actual_mode, 0o755, "executable bit must survive restore");
    }

    #[test]
    fn symlink_restored_correctly() {
        let store_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("link.txt"),
            content_hash: [0u8; 32],
            mode: 0,
            kind: FileKind::Symlink(std::path::PathBuf::from("real.txt")),
        }];
        let key = [30u8; 32];
        store_manifest(&key, &delta, &store).unwrap();

        restore_manifest(&key, target_dir.path(), &store).unwrap();

        let link = target_dir.path().join("link.txt");
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "restored symlink must be a symlink"
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::PathBuf::from("real.txt"),
            "symlink target mismatch"
        );
    }

    #[test]
    fn parent_dirs_created_automatically() {
        let store_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let store = make_store(store_dir.path());

        let content = b"nested";
        let hash: [u8; 32] = *blake3::hash(content).as_bytes();
        store.put_bytes_keyed(hash, content).unwrap();

        let delta = vec![ManifestEntry {
            rel_path: std::path::PathBuf::from("a/b/c/deep.txt"),
            content_hash: hash,
            mode: 0o644,
            kind: FileKind::Regular,
        }];
        let key = [40u8; 32];
        store_manifest(&key, &delta, &store).unwrap();

        restore_manifest(&key, target_dir.path(), &store).unwrap();

        assert!(
            target_dir.path().join("a/b/c/deep.txt").is_file(),
            "deeply nested parent dirs must be created"
        );
    }
}
```

### Step 2: Run tests to verify they fail

```
cargo test -p scheduler restore_manifest_tests 2>&1 | tail -20
```

Expected: FAIL — `restore_manifest` not defined

### Step 3: Implement `restore_manifest`

```rust
/// Look up a manifest in the CAS under `key`. If absent, return `Ok(false)`.
///
/// On a cache hit: deserialize the manifest, then for each entry:
/// - **Regular file**: hardlink from CAS (falling back to `fs::copy` on EXDEV/cross-device),
///   then call `set_permissions` to restore the recorded mode bits.
/// - **Symlink**: remove any existing file at the destination path (silently),
///   then create a symlink with the recorded target.
///
/// Parent directories are created automatically.
/// Returns `Ok(true)` on successful restore.
pub fn restore_manifest(
    key: &[u8; 32],
    target_dir: &std::path::Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let bytes = match store.get_bytes_by_raw_key(key)? {
        Some(b) => b,
        None => return Ok(false),
    };

    let manifest: PostinstallManifest = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    for entry in &manifest {
        let dest = target_dir.join(&entry.rel_path);

        // Ensure parent directory exists.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match &entry.kind {
            FileKind::Regular => {
                let cas_path = store.cas_file_path(&entry.content_hash);

                // Remove destination if it already exists (stale hardlink etc.)
                let _ = std::fs::remove_file(&dest);

                // Try hardlink first; fall back to copy on cross-device error.
                match std::fs::hard_link(&cas_path, &dest) {
                    Ok(()) => {}
                    Err(_) => {
                        std::fs::copy(&cas_path, &dest)?;
                    }
                }

                // Restore permissions.
                std::fs::set_permissions(
                    &dest,
                    std::fs::Permissions::from_mode(entry.mode),
                )?;
            }
            FileKind::Symlink(target) => {
                // Remove any existing entry at the destination first.
                let _ = std::fs::remove_file(&dest);
                std::os::unix::fs::symlink(target, &dest)?;
            }
        }
    }

    Ok(true)
}
```

### Step 4: Run tests to verify they pass

```
cargo test -p scheduler restore_manifest_tests 2>&1 | tail -20
```

Expected: PASS — all 5 tests green

### Step 5: Also verify all new tests still pass together

```
cargo test -p scheduler -- manifest_type_tests capture_dir_tests diff_manifests_tests store_manifest_tests restore_manifest_tests 2>&1 | tail -20
```

Expected: PASS — all 23 tests green

### Step 6: Commit

```
git add crates/scheduler/src/postinstall_cache.rs
git commit -m "feat(scheduler): implement restore_manifest with hardlink + symlink + permissions"
```

---

## Task 7: Delete old code from `postinstall_cache.rs`

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

Delete the following sections in order. After each deletion, run `cargo build -p scheduler` to catch any leftover references before proceeding to the next deletion.

### Step 1: Delete `compute_delta` and its test module

Find and delete this entire block (the function and its test module):

```rust
/// Return only the files that are NEW in `after` or whose content DIFFERS
/// from `before`. Files only present in `before` (deletions) are NOT captured.
pub fn compute_delta(
    before: &HashMap<PathBuf, Vec<u8>>,
    after: &HashMap<PathBuf, Vec<u8>>,
) -> HashMap<PathBuf, Vec<u8>> {
    ...
}

#[cfg(test)]
mod delta_tests {
    ...
}
```

### Step 2: Delete `snapshot_dir` and its test module

Find and delete this entire block:

```rust
/// Walk `dir` recursively and return a map of relative path → file contents
/// for every regular file found.
pub fn snapshot_dir(dir: &Path) -> std::io::Result<HashMap<PathBuf, Vec<u8>>> {
    ...
}

#[cfg(test)]
mod snapshot_tests {
    ...
}
```

### Step 3: Delete `base64_encode` and `base64_decode` and their test module

Find and delete these two private functions:

```rust
/// Encode `bytes` using standard Base64 (RFC 4648 §4).
fn base64_encode(bytes: &[u8]) -> String {
    ...
}

/// Decode a standard Base64 string.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    ...
}
```

Also delete the `store_tests` module that tests them (it starts with `mod store_tests`).

### Step 4: Delete `store_postinstall_outputs` and `restore_postinstall_outputs`

Find and delete:

```rust
/// Serialize `delta` (path → bytes) as a JSON BTreeMap...
pub fn store_postinstall_outputs(
    key: &[u8; 32],
    delta: &HashMap<PathBuf, Vec<u8>>,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<()> {
    ...
}

/// Look up `key` in CAS. If absent, return `Ok(false)`. Otherwise deserialize
/// JSON delta and write each entry under `target_dir`...
pub fn restore_postinstall_outputs(
    key: &[u8; 32],
    target_dir: &Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    ...
}
```

Also delete the `restore_tests` and `roundtrip_tests` modules that test them.

### Step 5: Remove now-unused `use` statements

At the top of `postinstall_cache.rs`, remove these no-longer-needed imports:

```rust
use std::collections::HashMap;
```

(Keep `use std::path::{Path, PathBuf}` — it's still used by `postinstall_cas_key` and `run_postinstall`.)

### Step 6: Verify clean compile

```
cargo build -p scheduler 2>&1 | grep -E "^error" | head -20
```

Expected: zero errors

### Step 7: Run all remaining scheduler tests

```
cargo test -p scheduler 2>&1 | tail -30
```

Expected: all tests pass, zero failures. The surviving tests should include:
- `key_tests::same_inputs_produce_same_key`
- `key_tests::different_integrity_produces_different_key`
- `run_tests::run_succeeds_returns_true`
- `run_tests::run_failure_returns_false`
- `run_tests::run_executes_in_cwd`
- All the new `manifest_type_tests`, `capture_dir_tests`, `diff_manifests_tests`, `store_manifest_tests`, `restore_manifest_tests`

### Step 8: Commit

```
git add crates/scheduler/src/postinstall_cache.rs
git commit -m "refactor(scheduler): delete snapshot/delta/base64 implementation (postinstall cache v1)"
```

---

## Task 8: Update `run_postinstall_phase` in `runner.rs`

**Files:**
- Modify: `crates/scheduler/src/runner.rs`

### Step 1: Locate the function to replace

In `crates/scheduler/src/runner.rs`, find the `run_postinstall_phase` function (around line 933). It currently looks like:

```rust
fn run_postinstall_phase(
    plugin: &dyn plugin::EcosystemPlugin,
    workspace_root: &Path,
    store: &artifact_store::LocalArtifactStore,
) {
    use crate::postinstall_cache::{
        compute_delta, postinstall_cas_key, restore_postinstall_outputs, run_postinstall,
        snapshot_dir, store_postinstall_outputs,
    };

    let tasks = plugin.postinstall_tasks(workspace_root);
    for pt in &tasks {
        let key = postinstall_cas_key(pt);

        // Cache hit?
        match restore_postinstall_outputs(&key, &pt.cwd, store) {
            Ok(true) => {
                eprintln!(
                    "[rage] {}#postinstall \u{2713} (restored from cache)",
                    pt.package_name
                );
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!(
                    "[rage] {}#postinstall restore error ({e}) \u{2014} re-running",
                    pt.package_name
                );
            }
        }

        // Cache miss — snapshot, run, capture.
        let before = snapshot_dir(&pt.cwd).unwrap_or_default();
        let start = std::time::Instant::now();
        let ran_ok = run_postinstall(pt).unwrap_or(false);
        let elapsed = start.elapsed();

        if ran_ok {
            let after = snapshot_dir(&pt.cwd).unwrap_or_default();
            let delta = compute_delta(&before, &after);
            if let Err(e) = store_postinstall_outputs(&key, &delta, store) {
                eprintln!(
                    "[rage] {}#postinstall capture error ({e}) \u{2014} ran but not cached",
                    pt.package_name
                );
            }
            eprintln!(
                "[rage] {}#postinstall \u{2713} {:.2}s",
                pt.package_name,
                elapsed.as_secs_f64()
            );
        } else {
            eprintln!("[rage] {}#postinstall \u{2717} FAILED", pt.package_name);
        }
    }
}
```

### Step 2: Replace the entire function body

Replace that function with:

```rust
fn run_postinstall_phase(
    plugin: &dyn plugin::EcosystemPlugin,
    workspace_root: &Path,
    store: &artifact_store::LocalArtifactStore,
) {
    use crate::postinstall_cache::{
        capture_dir, diff_manifests, postinstall_cas_key, restore_manifest, run_postinstall,
        store_manifest,
    };

    let tasks = plugin.postinstall_tasks(workspace_root);
    for pt in &tasks {
        let key = postinstall_cas_key(pt);

        // Cache hit — restore files from CAS and skip running the script.
        match restore_manifest(&key, &pt.cwd, store) {
            Ok(true) => {
                eprintln!(
                    "[rage] {}#postinstall \u{2713} (restored from cache)",
                    pt.package_name
                );
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!(
                    "[rage] {}#postinstall restore error ({e}) \u{2014} re-running",
                    pt.package_name
                );
            }
        }

        // Cache miss — walk before, run script, walk after, store delta.
        let before = capture_dir(&pt.cwd, store).unwrap_or_default();
        let start = std::time::Instant::now();
        let ran_ok = run_postinstall(pt).unwrap_or(false);
        let elapsed = start.elapsed();

        if ran_ok {
            let after = capture_dir(&pt.cwd, store).unwrap_or_default();
            let delta = diff_manifests(&before, &after);
            match store_manifest(&key, &delta, store) {
                Ok(true) => {
                    eprintln!(
                        "[rage] {}#postinstall \u{2713} {:.2}s",
                        pt.package_name,
                        elapsed.as_secs_f64()
                    );
                }
                Ok(false) => {
                    eprintln!(
                        "[rage] {}#postinstall (no cacheable changes) {:.2}s",
                        pt.package_name,
                        elapsed.as_secs_f64()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[rage] {}#postinstall capture error ({e}) \u{2014} ran but not cached",
                        pt.package_name
                    );
                }
            }
        } else {
            eprintln!("[rage] {}#postinstall \u{2717} FAILED", pt.package_name);
        }
    }
}
```

### Step 3: Verify it compiles

```
cargo build -p scheduler 2>&1 | grep -E "^error" | head -20
```

Expected: zero errors

### Step 4: Run runner tests

```
cargo test -p scheduler -p artifact-store 2>&1 | tail -30
```

Expected: all tests pass

### Step 5: Commit

```
git add crates/scheduler/src/runner.rs
git commit -m "feat(scheduler): wire postinstall cache v2 (manifest+hardlink) into run_postinstall_phase"
```

---

## Task 9: Full workspace compile + lint

**Files:** (none — verification only)

### Step 1: Full workspace test

```
cargo test --workspace 2>&1 | tail -40
```

Expected: `test result: ok` for every crate, zero failures

### Step 2: Clippy (must be zero warnings treated as errors)

```
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```

Expected: clean — no `error` lines

### Step 3: Fix any clippy issues found

If clippy reports issues:
- `dead_code` on old imports → remove unused `use` lines
- `unused_variable` → add `_` prefix or `let _ = ...`
- `needless_pass_by_ref` → follow clippy suggestion
- Do NOT suppress with `#[allow(...)]` unless the warning is provably incorrect

Re-run both commands until clean.

### Step 4: Commit if fixes were needed

```
git add -A
git commit -m "fix(scheduler): clippy warnings after postinstall cache v2"
```

---

## Task 10: Smoke test

**Files:** (none — integration test)

### Step 1: Check whether the smoke-test script exists

```
ls /Users/ken/workspace/ms/rage/scripts/test-postinstall-cache.sh
```

**If it exists:** run it per Step 2. **If it does not exist:** skip to Step 3.

### Step 2: Run the smoke test (if script exists)

```
bash /Users/ken/workspace/ms/rage/scripts/test-postinstall-cache.sh 2>&1 | tail -30
```

Expected: `✅ PASS — timestamps match` with esbuild postinstall showing `restored from cache` on second run

### Step 3: Manual smoke test (if script does not exist)

Run a quick end-to-end check using a real repo that has a postinstall script (esbuild or similar). The sequence to verify:

1. **First run** — should show `✓ X.XXs` (ran the script, stored in cache)
2. **Second run** — should show `✓ (restored from cache)` (hardlinks + permissions restored from CAS)
3. **Verify executable bit** — `ls -la node_modules/esbuild/bin/esbuild` should show `-rwxr-xr-x`

```bash
# From a workspace with esbuild in node_modules (adjust path as needed):
cargo run -p rage-cli -- install 2>&1 | grep "postinstall"
# Delete the generated binary to force a restore
rm -f node_modules/esbuild/bin/esbuild
cargo run -p rage-cli -- install 2>&1 | grep "postinstall"
# Should print: [rage] esbuild#postinstall ✓ (restored from cache)
ls -la node_modules/esbuild/bin/esbuild
# Should print permissions including 'x'
```

### Step 4: Final commit if nothing was already committed

```
git log --oneline -8
```

Verify the commit log shows all 8 implementation commits in order.

---

## Summary of commits expected

```
feat(artifact-store): add cas_file_path helper for hardlink restore
feat(scheduler): define ManifestEntry/FileKind types for postinstall cache v2
feat(scheduler): implement capture_dir for postinstall cache v2
feat(scheduler): implement diff_manifests for postinstall cache v2
feat(scheduler): implement store_manifest for postinstall cache v2
feat(scheduler): implement restore_manifest with hardlink + symlink + permissions
refactor(scheduler): delete snapshot/delta/base64 implementation (postinstall cache v1)
feat(scheduler): wire postinstall cache v2 (manifest+hardlink) into run_postinstall_phase
```
