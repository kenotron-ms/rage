# Postinstall Task Caching Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Run package postinstall scripts as first-class cached rage tasks, respecting each PM's own script policy, so native deps and binary downloads are correct on first run and instant on subsequent runs.

**Architecture:** After CAS restore + bin_links materialize `node_modules/`, rage queries the TypeScript plugin for postinstall tasks filtered by PM config (yarn `enableScripts`, pnpm `onlyBuiltDependencies` / `neverBuiltDependencies`, npm `ignore-scripts`). Each task either restores from CAS (cache hit) or runs the script, snapshots the file delta, and stores in CAS (cache miss). CAS key = `blake3(tarball_integrity + platform + node_version)`.

**Tech Stack:** Rust 2021, blake3, serde_json, std::process::Command (no sandbox in v1).

**Scope boundary (v1):** No DYLD sandbox for postinstall. Network is allowed (binary downloads work). Deletion tracking is out of scope for the delta. Compiler-hash bound CAS keys (for `node-gyp`) deferred to v2.

---

## Conventions

- Test style: `tempfile::tempdir()` + inline assertions, no external fixtures.
- Commit style: conventional commits — `feat(scope): ...`, `test(scope): ...`.
- Logging: `eprintln!("[rage] ...")` exactly like the rest of the runner.
- Trait additions get default impls so existing implementors don't break.
- File-organization: one module per concern (`postinstall.rs`, `postinstall_cache.rs`).
- Run `cargo fmt`, `cargo clippy --workspace -- -D warnings`, and `cargo test -p <crate>` after each task.

---

## Task 1: Add `PostinstallTask` struct to plugin crate

**Files:**
- Modify: `crates/plugin/src/lib.rs` (add struct near `LockfilePackage`)

**Step 1: Add `use std::collections::HashSet;` is NOT needed yet — open the file and add the new struct.**

Add this block immediately after the `LockfilePackage` struct (currently ends at line 32):

```rust
/// A package's `postinstall` lifecycle script, captured by the plugin so the
/// scheduler can run it as a first-class cached task.
///
/// The CAS key for the resulting outputs is
/// `blake3(tarball_integrity + ":" + platform_triple + ":" + node_version)`,
/// computed by `scheduler::postinstall_cache::postinstall_cas_key`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PostinstallTask {
    /// npm package name (e.g. `"esbuild"` or `"@prisma/client"`).
    pub package_name: String,
    /// Resolved version (e.g. `"0.21.5"`).
    pub version: String,
    /// Lockfile integrity string for this package — basis of the CAS key.
    pub tarball_integrity: String,
    /// Shell command from `package.json:scripts.postinstall`.
    pub script: String,
    /// Working directory for the script — `node_modules/{name}/`.
    pub cwd: PathBuf,
}
```

**Step 2: Write the failing test**

Append to the existing `mod tests` block in the same file:

```rust
    #[test]
    fn postinstall_task_roundtrips_serde() {
        let t = PostinstallTask {
            package_name: "esbuild".to_string(),
            version: "0.21.5".to_string(),
            tarball_integrity: "sha512-xyz".to_string(),
            script: "node install.js".to_string(),
            cwd: PathBuf::from("/tmp/wsp/node_modules/esbuild"),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: PostinstallTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
```

**Step 3: Run test to verify it compiles + passes**

Run: `cargo test -p plugin postinstall_task_roundtrips_serde`
Expected: PASS.

**Step 4: Commit**
```
git add crates/plugin/src/lib.rs && \
git commit -m "feat(plugin): add PostinstallTask struct"
```

---

## Task 2: Add `postinstall_tasks` method to `EcosystemPlugin` trait

**Files:**
- Modify: `crates/plugin/src/lib.rs`

**Step 1: Open `crates/plugin/src/lib.rs` and add the new trait method**

Inside the `pub trait EcosystemPlugin` block, after the `restore_from_cas` method (currently ends around line 173), add:

```rust
    /// Postinstall scripts that should run as first-class cached tasks after
    /// the workspace install completes (or its CAS restore succeeds).
    ///
    /// Implementations should:
    ///   1. Read the package manager's own script policy
    ///      (yarn `enableScripts`, pnpm `onlyBuiltDependencies` /
    ///      `neverBuiltDependencies`, npm `ignore-scripts`).
    ///   2. Walk `node_modules/` and find packages with a
    ///      `scripts.postinstall` field in `package.json`.
    ///   3. Filter the list using the policy from step 1.
    ///   4. Look up each package's lockfile integrity hash for use as the
    ///      CAS key.
    ///
    /// **If the PM globally disables scripts → return `vec![]`. rage will
    /// not run anything the PM itself would skip.**
    ///
    /// Default returns `vec![]` so non-TypeScript ecosystems are unaffected.
    fn postinstall_tasks(&self, _workspace_root: &Path) -> Vec<PostinstallTask> {
        Vec::new()
    }
```

**Step 2: Update `NullPlugin` test coverage**

Append to the `mod tests` block:

```rust
    #[test]
    fn default_postinstall_tasks_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let p = NullPlugin;
        assert!(p.postinstall_tasks(tmp.path()).is_empty());
    }
```

**Step 3: Run tests**

Run: `cargo test -p plugin`
Expected: all green, including `default_postinstall_tasks_is_empty`.

**Step 4: Commit**
```
git add crates/plugin/src/lib.rs && \
git commit -m "feat(plugin): add EcosystemPlugin::postinstall_tasks (default empty)"
```

---

## Task 3: Add PM policy reader to TypeScript plugin

**Files:**
- Create: `crates/plugin-typescript/src/postinstall.rs`
- Modify: `crates/plugin-typescript/src/lib.rs` (declare the module)

**Step 1: Create `crates/plugin-typescript/src/postinstall.rs` with the policy types and reader**

```rust
//! Postinstall script discovery and PM-policy filtering for TypeScript-ecosystem
//! workspaces (yarn 1, yarn berry, pnpm, npm).
//!
//! v1 scope: no sandbox; runs scripts the PM itself would have run, captures
//! the on-disk delta into CAS, restores on cache hit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Whether the package manager would let postinstall scripts run, and
/// optionally restrict them to a named set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPolicy {
    /// PM globally disabled scripts (`enableScripts: false`, `ignore-scripts=true`).
    AllDisabled,
    /// pnpm `onlyBuiltDependencies` — exact allow-list.
    Allowlist(HashSet<String>),
    /// pnpm `neverBuiltDependencies` — exact deny-list.
    NeverList(HashSet<String>),
    /// Default: PM runs all postinstall scripts.
    AllEnabled,
}

/// A postinstall script discovered in `node_modules/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPostinstallTask {
    pub package_name: String,
    pub script: String,
    pub cwd: PathBuf,
}

/// Read the active PM script policy from workspace files.
///
/// Precedence (first match wins):
///   1. `.yarnrc.yml` with `enableScripts: false` → `AllDisabled`
///   2. `.npmrc` with `ignore-scripts=true`        → `AllDisabled`
///   3. `package.json` `pnpm.onlyBuiltDependencies`  → `Allowlist`
///   4. `package.json` `pnpm.neverBuiltDependencies` → `NeverList`
///   5. otherwise → `AllEnabled`
pub fn read_pm_script_policy(workspace_root: &Path) -> ScriptPolicy {
    // 1. yarn berry: .yarnrc.yml with enableScripts: false
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".yarnrc.yml")) {
        for line in s.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("enableScripts:") {
                if rest.trim() == "false" {
                    return ScriptPolicy::AllDisabled;
                }
            }
        }
    }

    // 2. .npmrc: ignore-scripts=true (used by npm and pnpm)
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".npmrc")) {
        for line in s.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("ignore-scripts") {
                let val = rest.trim_start_matches('=').trim();
                if val.eq_ignore_ascii_case("true") {
                    return ScriptPolicy::AllDisabled;
                }
            }
        }
    }

    // 3 + 4. pnpm allow/deny list in workspace package.json.
    if let Ok(text) = std::fs::read_to_string(workspace_root.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(pnpm) = v.get("pnpm") {
                if let Some(arr) = pnpm.get("onlyBuiltDependencies").and_then(|x| x.as_array()) {
                    let set: HashSet<String> = arr
                        .iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect();
                    return ScriptPolicy::Allowlist(set);
                }
                if let Some(arr) = pnpm.get("neverBuiltDependencies").and_then(|x| x.as_array()) {
                    let set: HashSet<String> = arr
                        .iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect();
                    return ScriptPolicy::NeverList(set);
                }
            }
        }
    }

    ScriptPolicy::AllEnabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn yarnrc_yml_with_enable_scripts_false_disables() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".yarnrc.yml"), "enableScripts: false\n").unwrap();
        assert_eq!(read_pm_script_policy(dir.path()), ScriptPolicy::AllDisabled);
    }

    #[test]
    fn npmrc_ignore_scripts_disables() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".npmrc"), "ignore-scripts=true\n").unwrap();
        assert_eq!(read_pm_script_policy(dir.path()), ScriptPolicy::AllDisabled);
    }

    #[test]
    fn pnpm_only_built_dependencies_is_allowlist() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"w","pnpm":{"onlyBuiltDependencies":["esbuild","bcrypt"]}}"#,
        )
        .unwrap();
        match read_pm_script_policy(dir.path()) {
            ScriptPolicy::Allowlist(set) => {
                assert!(set.contains("esbuild"));
                assert!(set.contains("bcrypt"));
                assert_eq!(set.len(), 2);
            }
            other => panic!("expected Allowlist, got {other:?}"),
        }
    }

    #[test]
    fn pnpm_never_built_dependencies_is_neverlist() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"pnpm":{"neverBuiltDependencies":["sketchy-pkg"]}}"#,
        )
        .unwrap();
        match read_pm_script_policy(dir.path()) {
            ScriptPolicy::NeverList(set) => {
                assert!(set.contains("sketchy-pkg"));
            }
            other => panic!("expected NeverList, got {other:?}"),
        }
    }

    #[test]
    fn empty_workspace_is_all_enabled() {
        let dir = tempdir().unwrap();
        assert_eq!(read_pm_script_policy(dir.path()), ScriptPolicy::AllEnabled);
    }
}
```

**Step 2: Wire the module into `crates/plugin-typescript/src/lib.rs`**

Add at the top of the file, alongside the existing `pub mod lockfile;` line:

```rust
pub mod postinstall;
```

**Step 3: Run tests**

Run: `cargo test -p plugin-typescript postinstall::`
Expected: 5 PASS.

**Step 4: Commit**
```
git add crates/plugin-typescript/src/postinstall.rs crates/plugin-typescript/src/lib.rs && \
git commit -m "feat(plugin-typescript): read PM script policy (yarn/pnpm/npm)"
```

---

## Task 4: Scan `node_modules/` for postinstall scripts

**Files:**
- Modify: `crates/plugin-typescript/src/postinstall.rs`

**Step 1: Append the scanner**

Add after `read_pm_script_policy` (and any tests):

```rust
/// Walk `workspace_root/node_modules/` and return one [`RawPostinstallTask`]
/// for each package that declares `scripts.postinstall` in its `package.json`.
///
/// Hidden directories (`.bin`, `.cache`, `.modules.yaml`, etc.) are skipped.
/// Scoped packages (`@scope/name`) are recursed one level so their inner
/// `package.json` files are read.
pub fn scan_postinstall_scripts(workspace_root: &Path) -> Vec<RawPostinstallTask> {
    let nm = workspace_root.join("node_modules");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&nm) {
        Ok(it) => it,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if name_str.starts_with('@') {
            // Scoped package directory — recurse one level.
            if let Ok(inner) = std::fs::read_dir(&path) {
                for sub in inner.flatten() {
                    let sub_path = sub.path();
                    if !sub_path.is_dir() {
                        continue;
                    }
                    let sub_name = sub.file_name().to_string_lossy().into_owned();
                    let pkg_full = format!("{}/{}", name_str, sub_name);
                    if let Some(script) = read_postinstall_field(&sub_path) {
                        out.push(RawPostinstallTask {
                            package_name: pkg_full,
                            script,
                            cwd: sub_path,
                        });
                    }
                }
            }
        } else if let Some(script) = read_postinstall_field(&path) {
            out.push(RawPostinstallTask {
                package_name: name_str.into_owned(),
                script,
                cwd: path,
            });
        }
    }

    out
}

fn read_postinstall_field(pkg_dir: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    let s = v.get("scripts")?.get("postinstall")?.as_str()?;
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}
```

**Step 2: Add tests**

Append inside `mod tests`:

```rust
    fn write_pkg(root: &Path, name: &str, postinstall: Option<&str>) {
        let dir = root.join("node_modules").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = match postinstall {
            Some(s) => format!(r#"{{"name":"{name}","scripts":{{"postinstall":"{s}"}}}}"#),
            None => format!(r#"{{"name":"{name}"}}"#),
        };
        std::fs::write(dir.join("package.json"), manifest).unwrap();
    }

    #[test]
    fn scan_returns_only_packages_with_postinstall() {
        let dir = tempdir().unwrap();
        write_pkg(dir.path(), "esbuild", Some("node install.js"));
        write_pkg(dir.path(), "lodash", None);
        let tasks = scan_postinstall_scripts(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].package_name, "esbuild");
        assert_eq!(tasks[0].script, "node install.js");
    }

    #[test]
    fn scan_handles_scoped_packages() {
        let dir = tempdir().unwrap();
        write_pkg(dir.path(), "@prisma/client", Some("prisma generate"));
        let tasks = scan_postinstall_scripts(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].package_name, "@prisma/client");
    }

    #[test]
    fn scan_skips_hidden_dirs() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/.bin")).unwrap();
        write_pkg(dir.path(), "ok", Some("noop"));
        let tasks = scan_postinstall_scripts(dir.path());
        assert_eq!(tasks.len(), 1);
    }
```

**Step 3: Run tests**

Run: `cargo test -p plugin-typescript postinstall::`
Expected: all PASS (8 total).

**Step 4: Commit**
```
git add crates/plugin-typescript/src/postinstall.rs && \
git commit -m "feat(plugin-typescript): scan node_modules for postinstall scripts"
```

---

## Task 5: Apply policy filter

**Files:**
- Modify: `crates/plugin-typescript/src/postinstall.rs`

**Step 1: Append the filter function**

```rust
/// Apply a [`ScriptPolicy`] to a raw scan, dropping anything the PM would skip.
pub fn apply_policy(
    tasks: Vec<RawPostinstallTask>,
    policy: &ScriptPolicy,
) -> Vec<RawPostinstallTask> {
    match policy {
        ScriptPolicy::AllDisabled => Vec::new(),
        ScriptPolicy::AllEnabled => tasks,
        ScriptPolicy::Allowlist(set) => tasks
            .into_iter()
            .filter(|t| set.contains(&t.package_name))
            .collect(),
        ScriptPolicy::NeverList(set) => tasks
            .into_iter()
            .filter(|t| !set.contains(&t.package_name))
            .collect(),
    }
}
```

**Step 2: Tests**

Append inside `mod tests`:

```rust
    fn raw(name: &str) -> RawPostinstallTask {
        RawPostinstallTask {
            package_name: name.to_string(),
            script: "noop".to_string(),
            cwd: PathBuf::from(format!("/tmp/{name}")),
        }
    }

    #[test]
    fn apply_policy_all_disabled_returns_empty() {
        let out = apply_policy(vec![raw("esbuild")], &ScriptPolicy::AllDisabled);
        assert!(out.is_empty());
    }

    #[test]
    fn apply_policy_all_enabled_returns_all() {
        let out = apply_policy(
            vec![raw("esbuild"), raw("bcrypt")],
            &ScriptPolicy::AllEnabled,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn apply_policy_allowlist_keeps_named() {
        let mut set = HashSet::new();
        set.insert("esbuild".to_string());
        let out = apply_policy(
            vec![raw("esbuild"), raw("bcrypt")],
            &ScriptPolicy::Allowlist(set),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package_name, "esbuild");
    }

    #[test]
    fn apply_policy_neverlist_excludes_named() {
        let mut set = HashSet::new();
        set.insert("bcrypt".to_string());
        let out = apply_policy(
            vec![raw("esbuild"), raw("bcrypt")],
            &ScriptPolicy::NeverList(set),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].package_name, "esbuild");
    }
```

**Step 3: Run tests**

Run: `cargo test -p plugin-typescript postinstall::`
Expected: 12 PASS.

**Step 4: Commit**
```
git add crates/plugin-typescript/src/postinstall.rs && \
git commit -m "feat(plugin-typescript): apply PM script policy to scanned tasks"
```

---

## Task 6: Implement `EcosystemPlugin::postinstall_tasks` for `TypeScriptPlugin`

**Files:**
- Modify: `crates/plugin-typescript/src/lib.rs`

**Step 1: Read existing structure**

Re-read `crates/plugin-typescript/src/lib.rs` to find the `impl EcosystemPlugin for TypeScriptPlugin` block.

**Step 2: Add the method to that impl block**

Inside the `impl EcosystemPlugin for TypeScriptPlugin { ... }` block (just before its closing brace), add:

```rust
    fn postinstall_tasks(&self, workspace_root: &Path) -> Vec<plugin::PostinstallTask> {
        use crate::postinstall::{apply_policy, read_pm_script_policy, scan_postinstall_scripts};

        let policy = read_pm_script_policy(workspace_root);
        if matches!(policy, crate::postinstall::ScriptPolicy::AllDisabled) {
            return Vec::new();
        }

        let raw = scan_postinstall_scripts(workspace_root);
        let filtered = apply_policy(raw, &policy);
        if filtered.is_empty() {
            return Vec::new();
        }

        // Build a (name, version) → integrity index from the lockfile.
        let lockfile = self.parse_lockfile(workspace_root).unwrap_or_default();
        let mut by_name: std::collections::HashMap<&str, &plugin::LockfilePackage> =
            std::collections::HashMap::new();
        for pkg in &lockfile {
            // Name only — version comes from package.json on disk; we accept
            // the first integrity hash matching this package name.
            by_name.entry(pkg.name.as_str()).or_insert(pkg);
        }

        let mut out = Vec::with_capacity(filtered.len());
        for raw_task in filtered {
            // Resolve version + integrity. If we can't locate the package in
            // the lockfile we still run postinstall, but we use a placeholder
            // integrity built from the package name so the CAS key is stable
            // across runs (caching still works; just per-(name,platform,node)).
            let manifest_version = read_pkg_version(&raw_task.cwd).unwrap_or_default();
            let integrity = by_name
                .get(raw_task.package_name.as_str())
                .map(|p| p.integrity.clone())
                .unwrap_or_else(|| format!("rage-fallback:{}", raw_task.package_name));

            out.push(plugin::PostinstallTask {
                package_name: raw_task.package_name,
                version: manifest_version,
                tarball_integrity: integrity,
                script: raw_task.script,
                cwd: raw_task.cwd,
            });
        }
        out
    }
```

**Step 3: Add the small helper near the existing `read_node_version` helper**

Add at file scope (not inside the impl block):

```rust
fn read_pkg_version(pkg_dir: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    v.get("version").and_then(|s| s.as_str()).map(String::from)
}
```

**Step 4: Tests**

Append a new test module at the bottom of `crates/plugin-typescript/src/lib.rs`:

```rust
#[cfg(test)]
mod postinstall_integration_tests {
    use super::*;
    use plugin::EcosystemPlugin;
    use tempfile::tempdir;

    #[test]
    fn returns_empty_when_pm_globally_disables_scripts() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".npmrc"), "ignore-scripts=true\n").unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/esbuild")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/esbuild/package.json"),
            r#"{"name":"esbuild","version":"0.21.5","scripts":{"postinstall":"node install.js"}}"#,
        )
        .unwrap();

        let plugin = TypeScriptPlugin::new();
        assert!(plugin.postinstall_tasks(dir.path()).is_empty());
    }

    #[test]
    fn returns_task_when_postinstall_present_and_policy_allows() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/esbuild")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/esbuild/package.json"),
            r#"{"name":"esbuild","version":"0.21.5","scripts":{"postinstall":"node install.js"}}"#,
        )
        .unwrap();

        let plugin = TypeScriptPlugin::new();
        let tasks = plugin.postinstall_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].package_name, "esbuild");
        assert_eq!(tasks[0].version, "0.21.5");
        assert_eq!(tasks[0].script, "node install.js");
        // No lockfile → fallback integrity prefix.
        assert!(tasks[0].tarball_integrity.starts_with("rage-fallback:"));
    }
}
```

**Step 5: Run tests**

Run: `cargo test -p plugin-typescript`
Expected: all PASS.

**Step 6: Commit**
```
git add crates/plugin-typescript/src/lib.rs && \
git commit -m "feat(plugin-typescript): implement postinstall_tasks (lockfile + PM policy)"
```

---

## Task 7: `snapshot_dir` — read every regular file under a directory

**Files:**
- Create: `crates/scheduler/src/postinstall_cache.rs`
- Modify: `crates/scheduler/src/lib.rs` (declare the module)

**Step 1: Create the module**

```rust
//! Caching layer for package postinstall scripts.
//!
//! v1 strategy: snapshot package directory before + after running the script,
//! compute a delta of new/modified files, store in CAS keyed by
//! `blake3(tarball_integrity + ":" + platform + ":" + node_version)`.
//! Restore on cache hit by writing the delta files back into the package
//! directory. Deletions are out of scope for v1.

use plugin::PostinstallTask;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Recursively read every regular file under `dir` and return
/// `{relative_path → file contents}`. Symlinks are skipped. If `dir` does not
/// exist, returns an empty map.
pub fn snapshot_dir(dir: &Path) -> std::io::Result<HashMap<PathBuf, Vec<u8>>> {
    let mut out = HashMap::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = entry.file_type();
        if ft.is_symlink() || !ft.is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = match abs.strip_prefix(dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        match std::fs::read(abs) {
            Ok(bytes) => {
                out.insert(rel, bytes);
            }
            Err(_) => continue,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn snapshot_missing_dir_returns_empty() {
        let dir = tempdir().unwrap();
        let snap = snapshot_dir(&dir.path().join("nope")).unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn snapshot_captures_all_files_with_relative_paths() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/b.txt"), b"world").unwrap();

        let snap = snapshot_dir(dir.path()).unwrap();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get(&PathBuf::from("a.txt")).unwrap(), b"hello");
        assert_eq!(
            snap.get(&PathBuf::from("nested").join("b.txt")).unwrap(),
            b"world"
        );
    }
}

// Suppress "unused import" warnings until later tasks fill the module.
#[allow(dead_code)]
const _UNUSED_PIN: fn(&PostinstallTask) = |_| {};
```

**Step 2: Add module declaration**

In `crates/scheduler/src/lib.rs`, add after the other `pub mod` lines:

```rust
pub mod postinstall_cache;
```

**Step 3: Run tests**

Run: `cargo test -p scheduler postinstall_cache::snapshot_tests`
Expected: 2 PASS.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs crates/scheduler/src/lib.rs && \
git commit -m "feat(scheduler): postinstall_cache::snapshot_dir"
```

---

## Task 8: `compute_delta` — new + modified files

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Replace the placeholder with the real `compute_delta`**

Remove the `_UNUSED_PIN` block (and its `#[allow]`) added in Task 7. Add:

```rust
/// Return only the files that are NEW in `after` or whose content DIFFERS
/// from `before`. Files only present in `before` (deletions) are NOT captured.
pub fn compute_delta(
    before: &HashMap<PathBuf, Vec<u8>>,
    after: &HashMap<PathBuf, Vec<u8>>,
) -> HashMap<PathBuf, Vec<u8>> {
    let mut delta = HashMap::new();
    for (path, after_bytes) in after {
        match before.get(path) {
            Some(before_bytes) if before_bytes == after_bytes => continue,
            _ => {
                delta.insert(path.clone(), after_bytes.clone());
            }
        }
    }
    delta
}
```

**Step 2: Tests**

Append:

```rust
#[cfg(test)]
mod delta_tests {
    use super::*;

    fn map(pairs: &[(&str, &[u8])]) -> HashMap<PathBuf, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| (PathBuf::from(k), v.to_vec()))
            .collect()
    }

    #[test]
    fn delta_picks_up_new_and_changed() {
        let before = map(&[("a", b"X"), ("b", b"Y")]);
        let after = map(&[("a", b"X"), ("b", b"Z"), ("c", b"W")]);
        let d = compute_delta(&before, &after);
        assert_eq!(d.len(), 2);
        assert_eq!(d.get(&PathBuf::from("b")).unwrap(), b"Z");
        assert_eq!(d.get(&PathBuf::from("c")).unwrap(), b"W");
    }

    #[test]
    fn delta_empty_when_unchanged() {
        let before = map(&[("a", b"X")]);
        let after = map(&[("a", b"X")]);
        let d = compute_delta(&before, &after);
        assert!(d.is_empty());
    }

    #[test]
    fn delta_ignores_deletions() {
        let before = map(&[("a", b"X"), ("b", b"Y")]);
        let after = map(&[("a", b"X")]);
        let d = compute_delta(&before, &after);
        assert!(d.is_empty(), "deletions are not part of v1 delta");
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p scheduler postinstall_cache::delta_tests`
Expected: 3 PASS.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "feat(scheduler): postinstall_cache::compute_delta"
```

---

## Task 9: `postinstall_cas_key` — blake3(integrity + platform + node version)

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Add the key function and helper**

```rust
/// Compute the CAS key under which a postinstall task's outputs are stored.
/// Inputs: tarball integrity + platform + node version. Each axis breaks the
/// cache so a task built for darwin-arm64 + node v20 cannot be restored on
/// linux-x86_64 + node v18.
pub fn postinstall_cas_key(task: &PostinstallTask) -> [u8; 32] {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let node_version = read_node_version();
    let input = format!(
        "{}:{}:{}",
        task.tarball_integrity, platform, node_version
    );
    blake3::hash(input.as_bytes()).into()
}

fn read_node_version() -> String {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}
```

**Step 2: Tests**

Append:

```rust
#[cfg(test)]
mod key_tests {
    use super::*;

    fn task(integrity: &str) -> PostinstallTask {
        PostinstallTask {
            package_name: "esbuild".to_string(),
            version: "0.21.5".to_string(),
            tarball_integrity: integrity.to_string(),
            script: "node install.js".to_string(),
            cwd: PathBuf::from("/tmp/p"),
        }
    }

    #[test]
    fn same_inputs_produce_same_key() {
        let t = task("sha512-abc");
        assert_eq!(postinstall_cas_key(&t), postinstall_cas_key(&t));
    }

    #[test]
    fn different_integrity_produces_different_key() {
        assert_ne!(
            postinstall_cas_key(&task("sha512-abc")),
            postinstall_cas_key(&task("sha512-xyz")),
        );
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p scheduler postinstall_cache::key_tests`
Expected: 2 PASS.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "feat(scheduler): postinstall_cache::postinstall_cas_key"
```

---

## Task 10: `store_postinstall_outputs` — serialize delta + write to CAS

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Add the store function**

```rust
/// Serialize `delta` as JSON `{path_str: base64(content)}` and write into CAS
/// under `key`.
pub fn store_postinstall_outputs(
    key: &[u8; 32],
    delta: &HashMap<PathBuf, Vec<u8>>,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<()> {
    let mut serializable: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (rel, bytes) in delta {
        let path_str = rel.to_string_lossy().into_owned();
        let b64 = base64_encode(bytes);
        serializable.insert(path_str, b64);
    }
    let json = serde_json::to_vec(&serializable)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    store.put_bytes_keyed(*key, &json)
}

/// Standard-library-only base64 encoder so we don't add a new crate dep.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHA[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i < bytes.len() {
        let chunk = &bytes[i..i + 4];
        let mut acc = 0u32;
        let mut chars_in_chunk = 4;
        for (j, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                chars_in_chunk = j;
                break;
            }
            acc = (acc << 6) | val(c)? as u32;
        }
        if chars_in_chunk < 4 {
            acc <<= 6 * (4 - chars_in_chunk);
        }
        if chars_in_chunk >= 2 {
            out.push(((acc >> 16) & 0xff) as u8);
        }
        if chars_in_chunk >= 3 {
            out.push(((acc >> 8) & 0xff) as u8);
        }
        if chars_in_chunk == 4 {
            out.push((acc & 0xff) as u8);
        }
        i += 4;
    }
    let _ = pad;
    Some(out)
}
```

**Step 2: Add `artifact-store` to scheduler `[dependencies]` if not already present** — it already is per `Cargo.toml`. No change needed.

**Step 3: Tests**

Append:

```rust
#[cfg(test)]
mod store_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn base64_roundtrip_handles_binary() {
        let cases: &[&[u8]] = &[b"", b"a", b"ab", b"abc", b"abcd", b"\x00\xff\x10"];
        for &c in cases {
            let enc = base64_encode(c);
            let dec = base64_decode(&enc).expect("decode");
            assert_eq!(dec.as_slice(), c, "roundtrip failed for {c:?}");
        }
    }

    #[test]
    fn store_writes_to_cas_under_key() {
        let dir = tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(dir.path().to_path_buf());

        let mut delta = HashMap::new();
        delta.insert(PathBuf::from("install.js.lock"), b"binary\xff".to_vec());

        let key = [7u8; 32];
        store_postinstall_outputs(&key, &delta, &store).unwrap();
        assert!(store.contains_raw_key(&key));
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p scheduler postinstall_cache::store_tests`
Expected: 2 PASS.

**Step 5: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "feat(scheduler): postinstall_cache::store_postinstall_outputs"
```

---

## Task 11: `restore_postinstall_outputs` — read CAS + write delta back

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Add the restore function**

```rust
/// Look up `key` in CAS. If absent, return `Ok(false)`. Otherwise deserialize
/// the JSON delta and write each entry under `target_dir`, creating parent
/// directories as needed.
pub fn restore_postinstall_outputs(
    key: &[u8; 32],
    target_dir: &Path,
    store: &artifact_store::LocalArtifactStore,
) -> std::io::Result<bool> {
    let bytes = match store.get_bytes_by_raw_key(key)? {
        Some(b) => b,
        None => return Ok(false),
    };

    let map: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    for (rel_str, b64) in map {
        let bytes = base64_decode(&b64)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad base64"))?;
        let dest = target_dir.join(&rel_str);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &bytes)?;
    }
    Ok(true)
}
```

**Step 2: Tests**

Append:

```rust
#[cfg(test)]
mod restore_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn restore_returns_false_when_key_missing() {
        let cas_dir = tempdir().unwrap();
        let target = tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(cas_dir.path().to_path_buf());
        let key = [9u8; 32];
        assert!(!restore_postinstall_outputs(&key, target.path(), &store).unwrap());
    }

    #[test]
    fn store_then_restore_recreates_files() {
        let cas_dir = tempdir().unwrap();
        let target = tempdir().unwrap();
        #[allow(deprecated)]
        let store = artifact_store::LocalArtifactStore::new(cas_dir.path().to_path_buf());

        let mut delta = HashMap::new();
        delta.insert(PathBuf::from("bin/foo.node"), b"\x7fELF...".to_vec());
        delta.insert(PathBuf::from("install.flag"), b"ok".to_vec());

        let key = [3u8; 32];
        store_postinstall_outputs(&key, &delta, &store).unwrap();

        let restored = restore_postinstall_outputs(&key, target.path(), &store).unwrap();
        assert!(restored);
        assert_eq!(
            std::fs::read(target.path().join("bin/foo.node")).unwrap(),
            b"\x7fELF..."
        );
        assert_eq!(
            std::fs::read(target.path().join("install.flag")).unwrap(),
            b"ok"
        );
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p scheduler postinstall_cache::restore_tests`
Expected: 2 PASS.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "feat(scheduler): postinstall_cache::restore_postinstall_outputs"
```

---

## Task 12: `run_postinstall` — execute the script via sh

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Add the runner**

```rust
/// Run `task.script` via `sh -c` in `task.cwd`. Returns `Ok(true)` when the
/// script exits 0, `Ok(false)` for any other exit. Stdout/stderr are inherited
/// (the user sees postinstall output live).
pub fn run_postinstall(task: &PostinstallTask) -> std::io::Result<bool> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&task.script)
        .current_dir(&task.cwd)
        .status()?;
    Ok(status.success())
}
```

**Step 2: Tests**

Append:

```rust
#[cfg(test)]
mod run_tests {
    use super::*;
    use tempfile::tempdir;

    fn task_with_script(cwd: &Path, script: &str) -> PostinstallTask {
        PostinstallTask {
            package_name: "p".to_string(),
            version: "1.0.0".to_string(),
            tarball_integrity: "sha512-x".to_string(),
            script: script.to_string(),
            cwd: cwd.to_path_buf(),
        }
    }

    #[test]
    fn run_succeeds_returns_true() {
        let dir = tempdir().unwrap();
        let t = task_with_script(dir.path(), "true");
        assert!(run_postinstall(&t).unwrap());
    }

    #[test]
    fn run_failure_returns_false() {
        let dir = tempdir().unwrap();
        let t = task_with_script(dir.path(), "exit 1");
        assert!(!run_postinstall(&t).unwrap());
    }

    #[test]
    fn run_executes_in_cwd() {
        let dir = tempdir().unwrap();
        let sentinel = dir.path().join("ran.txt");
        let t = task_with_script(dir.path(), "touch ran.txt");
        assert!(run_postinstall(&t).unwrap());
        assert!(sentinel.exists());
    }
}
```

**Step 3: Run tests**

Run: `cargo test -p scheduler postinstall_cache::run_tests`
Expected: 3 PASS.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "feat(scheduler): postinstall_cache::run_postinstall"
```

---

## Task 13: Wire postinstall into `run_root_task_two_phase`

**Files:**
- Modify: `crates/scheduler/src/runner.rs`

**Step 1: Add a small synchronous helper at the bottom of the file**

After the existing `find_latest_install_fingerprint` function (around line 927), add:

```rust
/// Run every postinstall task the plugin yields for `workspace_root`, using
/// `store` for cache restore + capture. Errors are swallowed (logged): a
/// failed postinstall is reported but never breaks the install task.
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
            eprintln!(
                "[rage] {}#postinstall \u{2717} FAILED",
                pt.package_name
            );
        }
    }
}
```

**Step 2: Call the helper from each successful path inside `run_root_task_two_phase`**

There are four successful return points in `run_root_task_two_phase`. Insert `run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());` immediately before each.

The four sites are:

**Site A** — pure-cached (marker valid + effects intact). Inside the `if marker.exists()` block, immediately before the existing `eprintln!("[rage] {}#{} \u{2713} (cached)", ...); return Ok(());`. Note both `task.workspace_root` and `artifact_store` are in scope.

**Site B** — lockfile-based restore success. Immediately before `return Ok(())` after the `lockfile_restored` `eprintln!` (around the `(restored from artifact cache — N packages, M bin links)` log).

**Site C** — file-level restore success. Immediately before `return Ok(())` inside the `Ok(true) =>` arm of the `try_restore_from_cas` match (after the `(restored from artifact cache — M bin links)` log).

**Site D** — fresh install success. Immediately before the final `Ok(())` at the bottom of the `if status.success()` branch — i.e. AFTER the `eprintln!` and AFTER the marker write and CAS capture call.

In each case the call is exactly:

```rust
run_postinstall_phase(plugin, &task.workspace_root, artifact_store.as_ref());
```

**Step 3: Add a fresh-install-path test**

Append to the existing `mod tests` in `runner.rs`:

```rust
    #[tokio::test]
    async fn postinstall_runs_after_fresh_install_and_restores_on_second_run() {
        // Plugin that returns one fake postinstall task that creates a sentinel file.
        use cache::TwoPhaseCache;
        use std::sync::Arc;
        use tempfile::tempdir;

        let ws = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        #[allow(deprecated)]
        let store = Arc::new(artifact_store::LocalArtifactStore::new(
            store_dir.path().to_path_buf(),
        ));
        let cache = Arc::new(TwoPhaseCache::with_dir(cache_dir.path().to_path_buf()).unwrap());

        // Lay down a fake `node_modules/fake-pkg/` whose postinstall touches a sentinel.
        let pkg_dir = ws.path().join("node_modules/fake-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"fake-pkg","version":"1.0.0","scripts":{"postinstall":"touch did-run.flag"}}"#,
        )
        .unwrap();
        // Lockfile is absent → falls back to "rage-fallback:fake-pkg" integrity.

        let install = Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "true".to_string(), // no-op install
            cwd: ws.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: Vec::new(),
            workspace_root: ws.path().to_path_buf(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: Vec::new(),
        };

        let dag = build_dag(vec![]).unwrap();
        run_tasks_two_phase(
            &dag,
            vec![install.clone()],
            cache.clone(),
            test_plugin(),
            store.clone(),
        )
        .await
        .unwrap();

        // After first run, sentinel exists (postinstall ran).
        assert!(
            pkg_dir.join("did-run.flag").exists(),
            "postinstall script must have run after fresh install"
        );

        // Delete sentinel + run again — postinstall should be RESTORED from CAS,
        // not re-executed (we know it was the cache because the script we
        // captured was 'touch did-run.flag', which the cache replays as a file
        // rather than re-running).
        std::fs::remove_file(pkg_dir.join("did-run.flag")).unwrap();

        run_tasks_two_phase(&dag, vec![install], cache, test_plugin(), store)
            .await
            .unwrap();

        assert!(
            pkg_dir.join("did-run.flag").exists(),
            "postinstall outputs must be restored from CAS on second run"
        );
    }
```

**Step 4: Run tests**

Run: `cargo test -p scheduler -- --nocapture postinstall_runs_after_fresh_install_and_restores_on_second_run`
Expected: PASS.

Then run the full suite: `cargo test -p scheduler`
Expected: all green.

**Step 5: Lint + format**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings`
Expected: clean.

**Step 6: Commit**
```
git add crates/scheduler/src/runner.rs && \
git commit -m "feat(scheduler): wire postinstall caching into run_root_task_two_phase"
```

---

## Task 14: Full-roundtrip integration test

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Append the full roundtrip test**

```rust
#[cfg(test)]
mod roundtrip_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn full_postinstall_roundtrip_through_cas() {
        // 1. A package directory simulating node_modules/fake-pkg/.
        let pkg = tempdir().unwrap();
        std::fs::write(pkg.path().join("package.json"), b"{}").unwrap();

        let task = PostinstallTask {
            package_name: "fake-pkg".to_string(),
            version: "1.0.0".to_string(),
            tarball_integrity: "sha512-fake".to_string(),
            script: "echo built > built.flag && mkdir -p bin && printf 'bin' > bin/native"
                .to_string(),
            cwd: pkg.path().to_path_buf(),
        };

        // 2. Snapshot before.
        let before = snapshot_dir(&task.cwd).unwrap();
        assert_eq!(before.len(), 1, "only package.json before run");

        // 3. Run script.
        assert!(run_postinstall(&task).unwrap());

        // 4. Snapshot after, compute delta.
        let after = snapshot_dir(&task.cwd).unwrap();
        let delta = compute_delta(&before, &after);
        assert!(delta.contains_key(&PathBuf::from("built.flag")));
        assert!(delta.contains_key(&PathBuf::from("bin").join("native")));

        // 5. Store delta in CAS.
        let store_dir = tempdir().unwrap();
        #[allow(deprecated)]
        let store =
            artifact_store::LocalArtifactStore::new(store_dir.path().to_path_buf());
        let key = postinstall_cas_key(&task);
        store_postinstall_outputs(&key, &delta, &store).unwrap();

        // 6. Delete the new files (simulate node_modules clean).
        std::fs::remove_file(task.cwd.join("built.flag")).unwrap();
        std::fs::remove_dir_all(task.cwd.join("bin")).unwrap();

        // 7. Restore from CAS.
        let restored = restore_postinstall_outputs(&key, &task.cwd, &store).unwrap();
        assert!(restored);

        // 8. Verify files exist with correct content.
        assert_eq!(
            std::fs::read(task.cwd.join("built.flag")).unwrap(),
            b"built\n"
        );
        assert_eq!(
            std::fs::read(task.cwd.join("bin").join("native")).unwrap(),
            b"bin"
        );
    }
}
```

**Step 2: Run the test**

Run: `cargo test -p scheduler postinstall_cache::roundtrip_tests`
Expected: PASS.

**Step 3: Final full-workspace check**

Run: `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean + all green.

**Step 4: Commit**
```
git add crates/scheduler/src/postinstall_cache.rs && \
git commit -m "test(scheduler): full postinstall snapshot→delta→CAS→restore roundtrip"
```

---

## Files at end of plan

**Created:**
- `crates/plugin-typescript/src/postinstall.rs` (PM policy + scan + filter)
- `crates/scheduler/src/postinstall_cache.rs` (snapshot, delta, CAS key, store, restore, run)

**Modified:**
- `crates/plugin/src/lib.rs` (`PostinstallTask` struct, `EcosystemPlugin::postinstall_tasks` default)
- `crates/plugin-typescript/src/lib.rs` (`mod postinstall`, `TypeScriptPlugin` impl of `postinstall_tasks`)
- `crates/scheduler/src/lib.rs` (`pub mod postinstall_cache`)
- `crates/scheduler/src/runner.rs` (`run_postinstall_phase` helper, 4 call sites in `run_root_task_two_phase`)

## Deferred (v2)

- DYLD-sandbox postinstall execution for strict mode.
- Compiler hash (clang/cc + node-gyp version) folded into the CAS key for native builds.
- Deletion tracking in the delta (files removed by postinstall).
- Parallel postinstall execution within the install wave.
- pnpm-specific virtual-store / hardlink restore path.
