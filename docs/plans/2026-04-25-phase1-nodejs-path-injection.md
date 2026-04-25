# Phase 1 — Node.js PATH Injection (version-manager extension)

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** When rage spawns a JS task, version-manager-installed Node.js binaries (`yarn`, `node`, `npm`) must be on PATH, and the `workspace#install` cache key must invalidate when the workspace's required Node.js version changes.

**Architecture:** A new `crates/scheduler/src/node_path.rs` module owns all PATH-construction logic. It extends the existing `node_bin_path` (which already prepends `node_modules/.bin` directories — that portion of Phase 1 is already merged on `main`) to additionally prepend the bin directory of the active Node.js version resolved through fnm / nvm / asdf / mise. The `RootTask` plugin contract gains an `env_hash_inputs` field so the TypeScript plugin can fold the resolved Node version into the root install fingerprint without the scheduler knowing anything Node-specific.

**Tech Stack:** Rust 2024 (MSRV 1.91), tokio, blake3, tempfile, the `plugin` and `plugin-typescript` crates.

---

## Context — what is already done

The git log on `main` shows that the `node_modules/.bin` portion of Phase 1 has already been implemented by an earlier sub-agent:

- `crates/scheduler/src/runner.rs` line 48–72: `node_bin_path(cwd, workspace_root)` returns an `OsString` PATH that prepends `{cwd}/node_modules/.bin` then `{workspace_root}/node_modules/.bin` (deduplicated) to the existing `PATH`.
- All 5 `Command::new("sh")` spawn sites (lines 253, 328, 572, 606, 710) already call `node_bin_path` and pass it as `.env("PATH", &new_path)`.
- `which_first(command, cwd, workspace_root)` (line 388) checks both `node_modules/.bin` directories before system PATH.
- `crates/scheduler/src/task.rs`: the `Task` struct already carries `workspace_root: PathBuf`.
- `crates/plugin-typescript/src/lib.rs` `toolchain_allowlist` already includes `**/node_modules/.bin/**`.

**This plan does not redo that work.** It (a) extracts the existing helpers into a new `node_path.rs` module so the new logic has a clean home, and (b) adds the version-manager layer + `NODE_VERSION` fingerprint contribution that the brief calls out and that the user just hit live (`yarn: command not found` when fnm manages Node 18).

---

## Files to create / modify

**Create:**
- `crates/scheduler/src/node_path.rs`

**Modify:**
- `crates/scheduler/src/lib.rs` — declare `mod node_path;`
- `crates/scheduler/src/runner.rs` — delete `node_bin_path` + `which_first` (move to `node_path.rs`); update call sites to `node_path::build_node_path` / `node_path::which_first`; pass `workspace_root` through so VM resolution sees the right `.node-version`
- `crates/plugin/src/lib.rs` — add `env_hash_inputs: Vec<(String, String)>` to `RootTask`
- `crates/plugin-typescript/src/lib.rs` — extend `toolchain_allowlist`; populate `env_hash_inputs` with `NODE_VERSION` in `infer_root_tasks`
- `crates/scheduler/src/task.rs` — propagate `env_hash_inputs` from `RootTask` into the synthesized `Task`
- `crates/scheduler/src/runner.rs` — `root_task_fingerprint` hashes `env_hash_inputs`
- `crates/cli/tests/integration.rs` — new integration test

**Out of scope (do NOT touch):**
- The existing `node_bin_path` semantics for `node_modules/.bin` ordering (already merged & tested)
- Any `Command::new("sh")` spawn sites beyond pointing them at the new module (no logic change)
- Phase 2/3/4 work (declared_input_globs, abi_fingerprint, eBPF)

---

## TDD Tasks (8 tasks, ~5 min each)

### Task 1 — Create `node_path.rs` skeleton with failing tests for `resolve_node_version`

**Files:**
- Create: `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs`
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/lib.rs`

**Step 1: Write the failing tests + module skeleton**

Create `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs` with:

```rust
//! PATH construction for JS tasks.
//!
//! Prepends, in priority order:
//!   1. `{task_cwd}/node_modules/.bin`             — package-local dev tools
//!   2. `{workspace_root}/node_modules/.bin`        — workspace-root dev tools
//!   3. Active Node.js version-manager bin dir      — yarn, npm, node themselves
//!      (resolved from `.node-version` / `.nvmrc` / `.tool-versions` at the
//!      workspace root, then looked up under fnm / nvm / asdf / mise)
//!   4. Existing system PATH
//!
//! Without (3), `yarn` and `node` are invisible to subprocesses when the user
//! manages Node via fnm/nvm: those tools live under per-version directories
//! that are normally injected only by the shell hook.

use std::path::{Path, PathBuf};

/// Resolve the Node.js version string declared by the workspace.
///
/// Lookup order:
///   1. `.node-version`   (fnm default, also honored by mise/asdf via plugin)
///   2. `.nvmrc`          (nvm)
///   3. `.tool-versions`  (asdf / mise; only the `nodejs <ver>` line)
///
/// Returns `None` if no version file exists. The returned string is whatever
/// the file contains, trimmed: e.g. `"18.20.4"`, `"v20.11.0"`, `"lts/iron"`.
/// Callers must handle a possible leading `v` themselves.
pub fn resolve_node_version(_workspace_root: &Path) -> Option<String> {
    None // intentionally unimplemented — see Task 2
}

/// Locate the `bin/` directory for `version` under whichever supported version
/// manager has it installed.
///
/// Priority (first match on disk wins):
///   1. fnm   — `$FNM_DIR/node-versions/v{ver}/installation/bin`
///              fallback `~/.local/share/fnm/node-versions/v{ver}/installation/bin`
///   2. nvm   — `$NVM_DIR/versions/node/v{ver}/bin`
///              fallback `~/.nvm/versions/node/v{ver}/bin`
///   3. asdf  — `~/.asdf/installs/nodejs/{ver}/bin`
///   4. mise  — `~/.local/share/mise/installs/node/{ver}/bin`
///
/// `version` is taken verbatim except a single leading `v` is stripped before
/// constructing the candidate paths (so `"v18.20.4"` and `"18.20.4"` both work).
pub fn find_version_manager_bin(_version: &str) -> Option<PathBuf> {
    None // intentionally unimplemented — see Task 4
}

/// Build the PATH value for a JS task spawn (see module docs).
///
/// `system_path` is the existing `PATH` value to append after the prepended
/// directories. Pass `&std::env::var("PATH").unwrap_or_default()` in normal use.
///
/// Only directories that exist on disk are added — non-existent prepends would
/// just be noise.
pub fn build_node_path(
    _task_cwd: &Path,
    _workspace_root: &Path,
    _system_path: &str,
) -> String {
    String::new() // intentionally unimplemented — see Task 4
}

/// Resolve the tool path from the first whitespace-separated token of `command`.
///
/// Lookup order (first match wins):
///   1. `{cwd}/node_modules/.bin/{token}`
///   2. `{workspace_root}/node_modules/.bin/{token}` (skipped when equal to (1))
///   3. Active version manager bin dir / `{token}`
///   4. Each directory in the system PATH
///
/// If `token` contains `/`, it is returned verbatim (treated as an explicit
/// absolute or relative path).
pub fn which_first(
    _command: &str,
    _cwd: &Path,
    _workspace_root: &Path,
) -> Option<PathBuf> {
    None // intentionally unimplemented — see Task 5
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── resolve_node_version ─────────────────────────────────────────────

    #[test]
    fn resolve_node_version_reads_dot_node_version() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_reads_nvmrc_when_no_dot_node_version() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "v20.11.0\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("v20.11.0".to_string())
        );
    }

    #[test]
    fn resolve_node_version_prefers_dot_node_version_over_nvmrc() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "20.11.0\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_reads_tool_versions_nodejs_line() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".tool-versions"),
            "python 3.12.4\nnodejs 18.20.4\nrust 1.91.0\n",
        )
        .unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }

    #[test]
    fn resolve_node_version_returns_none_when_no_files() {
        let dir = tempdir().unwrap();
        assert_eq!(resolve_node_version(dir.path()), None);
    }

    #[test]
    fn resolve_node_version_trims_whitespace_and_blank_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".node-version"), "  18.20.4  \n\n").unwrap();
        assert_eq!(
            resolve_node_version(dir.path()),
            Some("18.20.4".to_string())
        );
    }
}
```

Add to `/Users/ken/workspace/ms/rage/crates/scheduler/src/lib.rs` (just below the existing `pub mod task;` / `pub mod runner;` declarations — find them and insert):

```rust
pub mod node_path;
```

**Step 2: Run tests to verify they fail**

Run:
```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path 2>&1 | tail -30
```

Expected output: 6 failures, all under `node_path::tests::resolve_node_version_*`, e.g.:
```
test node_path::tests::resolve_node_version_reads_dot_node_version ... FAILED
...
test result: FAILED. 0 passed; 6 failed
```

**Step 3: Commit the failing-test scaffold**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/node_path.rs crates/scheduler/src/lib.rs && git commit -m "test(scheduler): failing tests for resolve_node_version"
```

---

### Task 2 — Implement `resolve_node_version`

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs`

**Step 1: Replace the `resolve_node_version` body**

Replace the placeholder body with:

```rust
pub fn resolve_node_version(workspace_root: &Path) -> Option<String> {
    // 1. .node-version — fnm/mise default, single line.
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".node-version")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }

    // 2. .nvmrc — nvm, single line; may include a leading "v".
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".nvmrc")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }

    // 3. .tool-versions — asdf/mise multi-tool format. We only care about
    //    the line beginning with `nodejs ` (or `node `). First whitespace-
    //    separated token after the tool name is the version.
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".tool-versions")) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let tool = parts.next().unwrap_or("");
            if tool == "nodejs" || tool == "node" {
                if let Some(v) = parts.next() {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }

    None
}
```

**Step 2: Run the tests to verify they pass**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path::tests::resolve_node_version 2>&1 | tail -15
```

Expected:
```
test result: ok. 6 passed; 0 failed
```

**Step 3: Commit**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/node_path.rs && git commit -m "feat(scheduler): resolve_node_version reads .node-version/.nvmrc/.tool-versions"
```

---

### Task 3 — Failing tests for `find_version_manager_bin` and `build_node_path`

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs`

**Step 1: Append the new tests to the existing `mod tests` block**

Append the following inside the existing `mod tests` block (just before the closing `}` of `mod tests`):

```rust
    // ── find_version_manager_bin ────────────────────────────────────────

    /// Build a fake fnm tree under `home`, return the bin dir we expect to find.
    fn fake_fnm(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".local/share/fnm/node-versions")
            .join(format!("v{version}"))
            .join("installation/bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_nvm(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".nvm/versions/node")
            .join(format!("v{version}"))
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_asdf(home: &Path, version: &str) -> PathBuf {
        let bin = home.join(".asdf/installs/nodejs").join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn fake_mise(home: &Path, version: &str) -> PathBuf {
        let bin = home
            .join(".local/share/mise/installs/node")
            .join(version)
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    /// Run a closure with `HOME` (and `FNM_DIR`/`NVM_DIR` cleared) pointing
    /// at `home`, restoring the originals afterwards. Tests must not run
    /// concurrently with other tests that touch these vars: this whole
    /// suite uses one HOME at a time.
    fn with_home<F: FnOnce()>(home: &Path, f: F) {
        let prev_home = std::env::var_os("HOME");
        let prev_fnm = std::env::var_os("FNM_DIR");
        let prev_nvm = std::env::var_os("NVM_DIR");
        // SAFETY: setting env vars is safe in single-threaded test context.
        // These tests must NOT use #[test(parallel)]; cargo's default
        // parallel test harness is fine because we restore in a guard.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::remove_var("FNM_DIR");
            std::env::remove_var("NVM_DIR");
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_fnm {
                Some(v) => std::env::set_var("FNM_DIR", v),
                None => std::env::remove_var("FNM_DIR"),
            }
            match prev_nvm {
                Some(v) => std::env::set_var("NVM_DIR", v),
                None => std::env::remove_var("NVM_DIR"),
            }
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn find_version_manager_bin_finds_fnm() {
        let home = tempdir().unwrap();
        let expected = fake_fnm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_strips_leading_v() {
        let home = tempdir().unwrap();
        let expected = fake_fnm(home.path(), "20.11.0");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("v20.11.0"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_nvm() {
        let home = tempdir().unwrap();
        let expected = fake_nvm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_asdf() {
        let home = tempdir().unwrap();
        let expected = fake_asdf(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_falls_back_to_mise() {
        let home = tempdir().unwrap();
        let expected = fake_mise(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
        });
    }

    #[test]
    fn find_version_manager_bin_returns_none_when_nothing_installed() {
        let home = tempdir().unwrap();
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), None);
        });
    }

    #[test]
    fn find_version_manager_bin_prefers_fnm_over_nvm() {
        let home = tempdir().unwrap();
        let fnm_bin = fake_fnm(home.path(), "18.20.4");
        let _nvm_bin = fake_nvm(home.path(), "18.20.4");
        with_home(home.path(), || {
            assert_eq!(find_version_manager_bin("18.20.4"), Some(fnm_bin.clone()));
        });
    }

    // ── build_node_path ─────────────────────────────────────────────────

    #[test]
    fn build_node_path_prepends_pkg_then_workspace_then_existing() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("packages/foo");
        let pkg_bin = pkg.join("node_modules/.bin");
        let ws_bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&pkg_bin).unwrap();
        std::fs::create_dir_all(&ws_bin).unwrap();

        let path = build_node_path(&pkg, ws.path(), "/usr/bin:/bin");
        let parts: Vec<&str> = path.split(':').collect();
        assert_eq!(parts[0], pkg_bin.to_str().unwrap());
        assert_eq!(parts[1], ws_bin.to_str().unwrap());
        assert!(parts.contains(&"/usr/bin"));
        assert!(parts.contains(&"/bin"));
    }

    #[test]
    fn build_node_path_omits_nonexistent_node_modules() {
        // pkg/node_modules/.bin doesn't exist — must not appear in PATH.
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("packages/foo");
        std::fs::create_dir_all(&pkg).unwrap(); // pkg dir but no node_modules

        let path = build_node_path(&pkg, ws.path(), "/usr/bin");
        assert!(
            !path.contains("node_modules/.bin"),
            "non-existent node_modules/.bin must not be on PATH; got {path}"
        );
    }

    #[test]
    fn build_node_path_dedupes_when_pkg_equals_workspace() {
        let ws = tempdir().unwrap();
        let bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();

        let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
        let count = path.matches(bin.to_str().unwrap()).count();
        assert_eq!(count, 1, "node_modules/.bin must appear exactly once");
    }

    #[test]
    fn build_node_path_includes_version_manager_bin_when_node_version_set() {
        let home = tempdir().unwrap();
        let vm_bin = fake_fnm(home.path(), "18.20.4");

        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                path.contains(vm_bin.to_str().unwrap()),
                "expected fnm bin {} in PATH, got: {path}",
                vm_bin.display()
            );
        });
    }

    #[test]
    fn build_node_path_skips_version_manager_when_no_version_file() {
        let home = tempdir().unwrap();
        let _vm_bin = fake_fnm(home.path(), "18.20.4");

        let ws = tempdir().unwrap();
        // No .node-version file — VM bin must NOT be added.

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                !path.contains("fnm"),
                "no .node-version means no VM bin on PATH; got: {path}"
            );
        });
    }

    #[test]
    fn build_node_path_skips_version_manager_when_version_not_installed() {
        // .node-version says 18.20.4 but no version manager has it — fall through.
        let home = tempdir().unwrap();
        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let path = build_node_path(ws.path(), ws.path(), "/usr/bin");
            assert!(
                !path.contains("fnm") && !path.contains(".nvm") && !path.contains(".asdf"),
                "missing VM install must yield no VM bin; got: {path}"
            );
        });
    }
```

**Step 2: Run the tests to verify they fail**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path 2>&1 | tail -20
```

Expected: 13 failures (the 6 passing from Task 2 are still passing; the 7 new `find_version_manager_bin_*` and 6 new `build_node_path_*` fail — actually 13 new failures total: 7 + 6).

Actual count:
```
test result: FAILED. 6 passed; 13 failed
```

**Step 3: Commit the failing tests**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/node_path.rs && git commit -m "test(scheduler): failing tests for find_version_manager_bin and build_node_path"
```

---

### Task 4 — Implement `find_version_manager_bin` and `build_node_path`

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs`

**Step 1: Replace the placeholder bodies**

Replace the body of `find_version_manager_bin` with:

```rust
pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let v_no_prefix = version.strip_prefix('v').unwrap_or(version);
    let v_with_prefix = format!("v{v_no_prefix}");

    let home = std::env::var_os("HOME").map(PathBuf::from);

    // 1. fnm — honor $FNM_DIR, fall back to ~/.local/share/fnm
    let fnm_root = std::env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share/fnm")));
    if let Some(root) = fnm_root {
        let candidate = root
            .join("node-versions")
            .join(&v_with_prefix)
            .join("installation/bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 2. nvm — honor $NVM_DIR, fall back to ~/.nvm
    let nvm_root = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".nvm")));
    if let Some(root) = nvm_root {
        let candidate = root.join("versions/node").join(&v_with_prefix).join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 3. asdf — ~/.asdf/installs/nodejs/{ver}/bin (no leading 'v')
    if let Some(h) = &home {
        let candidate = h.join(".asdf/installs/nodejs").join(v_no_prefix).join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    // 4. mise — ~/.local/share/mise/installs/node/{ver}/bin (no leading 'v')
    if let Some(h) = &home {
        let candidate = h
            .join(".local/share/mise/installs/node")
            .join(v_no_prefix)
            .join("bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }

    None
}
```

Replace the body of `build_node_path` with:

```rust
pub fn build_node_path(task_cwd: &Path, workspace_root: &Path, system_path: &str) -> String {
    #[cfg(unix)]
    const SEP: char = ':';
    #[cfg(windows)]
    const SEP: char = ';';

    let mut prepend: Vec<PathBuf> = Vec::new();

    // 1. {task_cwd}/node_modules/.bin
    let pkg_bin = task_cwd.join("node_modules/.bin");
    if pkg_bin.is_dir() {
        prepend.push(pkg_bin.clone());
    }

    // 2. {workspace_root}/node_modules/.bin (skip if same as #1)
    let ws_bin = workspace_root.join("node_modules/.bin");
    if ws_bin != pkg_bin && ws_bin.is_dir() {
        prepend.push(ws_bin);
    }

    // 3. Active Node.js version manager bin dir
    if let Some(version) = resolve_node_version(workspace_root) {
        if let Some(vm_bin) = find_version_manager_bin(&version) {
            prepend.push(vm_bin);
        }
    }

    // Concatenate: prepended dirs + system PATH.
    let mut out = String::new();
    for dir in &prepend {
        out.push_str(&dir.to_string_lossy());
        out.push(SEP);
    }
    out.push_str(system_path);
    out
}
```

**Step 2: Run tests to verify all pass**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path 2>&1 | tail -10
```

Expected:
```
test result: ok. 19 passed; 0 failed
```

**Step 3: Run the full scheduler suite to confirm nothing else broke**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler 2>&1 | tail -10
```

Expected: all tests pass (no regressions).

**Step 4: Commit**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/node_path.rs && git commit -m "feat(scheduler): build_node_path & find_version_manager_bin (fnm/nvm/asdf/mise)"
```

---

### Task 5 — Migrate `runner.rs` to use `node_path` module

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs`

The existing `node_bin_path` and `which_first` helpers in `runner.rs` are now superseded by `crate::node_path::build_node_path` and `crate::node_path::which_first`. We delete the inline copies and update call sites. The new `build_node_path` requires the **system PATH as an argument** (whereas `node_bin_path` read it internally), so call sites change shape.

**Step 1: Delete the inline `node_bin_path` function**

In `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs`, delete the entire `fn node_bin_path(...)` definition (lines 32–72 of the current file — the doc comment block plus the function). Replace nothing in its place.

**Step 2: Delete the inline `which_first` function and its tests**

Delete the `fn which_first(...)` definition starting around line 379 (`/// Resolve the tool path from the first token of \`command\`.`) through its closing `}`.

Also delete the two `node_bin_path_*` tests in the existing `runner.rs` test module (search for `fn node_bin_path_deduplicates_when_cwd_is_workspace_root` and `fn node_bin_path_prepends_pkg_before_workspace`); their replacements live in `node_path.rs`.

If there is a `which_first_*` test block in `runner.rs`, delete those too — they will be re-added in Task 6 after we extend `which_first` with VM support.

**Step 3: Update all call sites in `runner.rs`**

Search for every `node_bin_path(` occurrence and replace with the new call. There are 5 spawn sites; the pattern is the same at each. Example transformation:

Before:
```rust
let new_path = node_bin_path(&task.cwd, &task.workspace_root);
let status = Command::new("sh")
    .arg("-c")
    .arg(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path)
```

After:
```rust
let system_path = std::env::var("PATH").unwrap_or_default();
let new_path = crate::node_path::build_node_path(&task.cwd, &task.workspace_root, &system_path);
let status = Command::new("sh")
    .arg("-c")
    .arg(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path)
```

Apply that transformation at every site. Sites to update (search for `node_bin_path(`):
- Around line 252 (single-phase runner)
- Around line 327 (root task legacy runner)
- Around line 570 (two-phase Loose branch)
- Around line 592 (two-phase sandbox branch)
- Around line 604 (two-phase sandbox-fallback branch)
- Around line 709 (two-phase root task runner)

For the `which_first` call site (currently around line 485, in `run_single_task_two_phase`):

Before:
```rust
let tool_path = which_first(&task.command, &task.cwd, &task.workspace_root)
```

After:
```rust
let tool_path = crate::node_path::which_first(&task.command, &task.cwd, &task.workspace_root)
```

**Step 4: Verify it compiles**

```sh
cd /Users/ken/workspace/ms/rage && cargo build -p scheduler 2>&1 | tail -15
```

Expected: clean build, no warnings about unused imports. If `tokio::process::Command` or other imports become unused, drop them; otherwise leave intact.

**Step 5: Run all scheduler tests**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler 2>&1 | tail -15
```

Expected: all tests pass. (The `node_path` module's tests cover what the deleted `node_bin_path_*` tests covered.)

**Step 6: Commit**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/runner.rs && git commit -m "refactor(scheduler): runner uses node_path module"
```

---

### Task 6 — Extend `which_first` to check version manager bin dir

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/node_path.rs`

The current placeholder for `which_first` returns `None`. Implement it now and add tests.

**Step 1: Add failing tests**

Append to the `mod tests` block in `node_path.rs`:

```rust
    // ── which_first ─────────────────────────────────────────────────────

    #[test]
    fn which_first_returns_path_token_verbatim() {
        let dir = tempdir().unwrap();
        let result = which_first("/bin/sh -c foo", dir.path(), dir.path());
        assert_eq!(result, Some(PathBuf::from("/bin/sh")));
    }

    #[test]
    fn which_first_finds_in_pkg_node_modules_bin() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("pkg");
        let bin = pkg.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tsc = bin.join("tsc");
        std::fs::write(&tsc, b"#!/bin/sh\n").unwrap();

        let result = which_first("tsc --noEmit", &pkg, ws.path());
        assert_eq!(result, Some(tsc));
    }

    #[test]
    fn which_first_falls_back_to_workspace_node_modules_bin() {
        let ws = tempdir().unwrap();
        let pkg = ws.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        let ws_bin = ws.path().join("node_modules/.bin");
        std::fs::create_dir_all(&ws_bin).unwrap();
        let yarn = ws_bin.join("yarn");
        std::fs::write(&yarn, b"#!/bin/sh\n").unwrap();

        let result = which_first("yarn install", &pkg, ws.path());
        assert_eq!(result, Some(yarn));
    }

    #[test]
    fn which_first_falls_back_to_version_manager_bin() {
        let home = tempdir().unwrap();
        let vm_bin = fake_fnm(home.path(), "18.20.4");
        let yarn = vm_bin.join("yarn");
        std::fs::write(&yarn, b"#!/bin/sh\n").unwrap();

        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join(".node-version"), "18.20.4\n").unwrap();

        with_home(home.path(), || {
            let result = which_first("yarn install", ws.path(), ws.path());
            assert_eq!(result, Some(yarn.clone()));
        });
    }

    #[test]
    fn which_first_returns_none_when_token_not_found() {
        let dir = tempdir().unwrap();
        let result = which_first("definitely_not_a_real_tool_xyz", dir.path(), dir.path());
        assert_eq!(result, None);
    }
```

**Step 2: Run tests to verify they fail**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path::tests::which_first 2>&1 | tail -10
```

Expected: 5 failures (`which_first_*`).

**Step 3: Implement `which_first`**

Replace the placeholder body:

```rust
pub fn which_first(command: &str, cwd: &Path, workspace_root: &Path) -> Option<PathBuf> {
    let first = command.split_whitespace().next()?;
    if first.contains('/') {
        return Some(PathBuf::from(first));
    }

    // 1. {cwd}/node_modules/.bin/{first}
    let pkg_candidate = cwd.join("node_modules/.bin").join(first);
    if pkg_candidate.is_file() {
        return Some(pkg_candidate);
    }

    // 2. {workspace_root}/node_modules/.bin/{first}
    if workspace_root != cwd {
        let ws_candidate = workspace_root.join("node_modules/.bin").join(first);
        if ws_candidate.is_file() {
            return Some(ws_candidate);
        }
    }

    // 3. Active Node.js version manager bin dir
    if let Some(version) = resolve_node_version(workspace_root) {
        if let Some(vm_bin) = find_version_manager_bin(&version) {
            let candidate = vm_bin.join(first);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 4. System PATH
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(first);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
```

**Step 4: Run tests to verify they pass**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler --lib node_path 2>&1 | tail -10
```

Expected:
```
test result: ok. 24 passed; 0 failed
```

**Step 5: Run full scheduler suite**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p scheduler 2>&1 | tail -10
```

Expected: all tests pass.

**Step 6: Commit**

```sh
cd /Users/ken/workspace/ms/rage && git add crates/scheduler/src/node_path.rs && git commit -m "feat(scheduler): which_first checks version manager bin dir"
```

---

### Task 7 — Add `env_hash_inputs` to `RootTask`; thread through `Task` and fingerprint

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/plugin/src/lib.rs`
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/task.rs`
- Modify: `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs`
- Modify: `/Users/ken/workspace/ms/rage/crates/plugin-typescript/src/lib.rs`

**Step 1: Write the failing tests**

Append to `/Users/ken/workspace/ms/rage/crates/plugin-typescript/src/lib.rs` (inside `mod tests`, near the existing `infer_root_tasks_*` tests):

```rust
    #[test]
    fn infer_root_tasks_includes_node_version_when_dot_node_version_exists() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        std::fs::write(dir.path().join(".node-version"), "18.20.4\n").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].env_hash_inputs,
            vec![("NODE_VERSION".to_string(), "18.20.4".to_string())]
        );
    }

    #[test]
    fn infer_root_tasks_omits_env_hash_when_no_version_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"").unwrap();
        let tasks = TypeScriptPlugin::new().infer_root_tasks(dir.path());
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].env_hash_inputs.is_empty());
    }

    #[test]
    fn allowlist_includes_version_manager_dirs() {
        let allow = TypeScriptPlugin::new().toolchain_allowlist();
        let patterns: Vec<&str> = allow.iter().map(|e| e.path_pattern.as_str()).collect();
        assert!(patterns.iter().any(|p| p.contains("fnm")));
        assert!(patterns.iter().any(|p| p.contains(".nvm")));
        assert!(patterns.iter().any(|p| p.contains(".asdf")));
        assert!(patterns.iter().any(|p| p.contains("mise")));
    }
```

Append to `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs` test module (a new test for `root_task_fingerprint` honoring `env_hash_inputs` — search for the existing `root_task_fingerprint_*` tests if any, otherwise add to the bottom of `mod tests`):

```rust
    #[test]
    fn root_task_fingerprint_changes_with_env_hash_inputs() {
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("yarn.lock"), b"v1\n").unwrap();
        let mk = |env: Vec<(String, String)>| Task {
            package_name: "workspace".to_string(),
            script_name: "install".to_string(),
            command: "yarn install".to_string(),
            cwd: dir.path().to_path_buf(),
            sandbox_mode: pipeline_config::SandboxMode::Loose,
            is_root: true,
            input_paths: vec![dir.path().join("yarn.lock")],
            workspace_root: dir.path().to_path_buf(),
            declared_input_globs: Vec::new(),
            dep_package_names: Vec::new(),
            output_globs: Vec::new(),
            env_hash_inputs: env,
        };
        let fp_none = root_task_fingerprint(&mk(Vec::new()));
        let fp_v18 = root_task_fingerprint(&mk(vec![(
            "NODE_VERSION".to_string(),
            "18.20.4".to_string(),
        )]));
        let fp_v20 = root_task_fingerprint(&mk(vec![(
            "NODE_VERSION".to_string(),
            "20.11.0".to_string(),
        )]));
        assert_ne!(fp_none, fp_v18, "adding NODE_VERSION must change fingerprint");
        assert_ne!(fp_v18, fp_v20, "different NODE_VERSION must change fingerprint");
    }
```

Run:
```sh
cd /Users/ken/workspace/ms/rage && cargo test -p plugin-typescript --lib infer_root_tasks_includes_node_version 2>&1 | tail -5
```

Expected: compile error — `RootTask` has no `env_hash_inputs` field. Good.

**Step 2: Add `env_hash_inputs` to `RootTask`**

In `/Users/ken/workspace/ms/rage/crates/plugin/src/lib.rs`, modify the `RootTask` struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootTask {
    pub name: String,
    pub command: String,
    pub input_paths: Vec<PathBuf>,
    /// Extra `(key, value)` pairs folded into the fingerprint hash.
    /// Used by ecosystems to bake environment-derived state (e.g. resolved
    /// Node.js version) into the cache key without making the scheduler
    /// ecosystem-aware. Empty by default.
    pub env_hash_inputs: Vec<(String, String)>,
}
```

**Step 3: Update `Task` to carry `env_hash_inputs`**

In `/Users/ken/workspace/ms/rage/crates/scheduler/src/task.rs`, add the field on `Task`:

```rust
pub struct Task {
    // ... existing fields ...
    /// Extra (key, value) pairs hashed alongside `input_paths` for root tasks.
    /// Empty for non-root tasks. Sourced from `RootTask::env_hash_inputs`.
    pub env_hash_inputs: Vec<(String, String)>,
}
```

In the same file, in `build_task_list`, when creating the synthesized root `Task`, copy the field across:

```rust
tasks.push(Task {
    package_name: "workspace".to_string(),
    script_name: rt.name,
    command: rt.command,
    cwd: workspace_root.to_path_buf(),
    sandbox_mode: pipeline_config::SandboxMode::default(),
    is_root: true,
    input_paths: rt.input_paths,
    workspace_root: workspace_root.to_path_buf(),
    declared_input_globs: Vec::new(),
    dep_package_names: Vec::new(),
    output_globs: Vec::new(),
    env_hash_inputs: rt.env_hash_inputs,
});
```

For the **non-root** package task construction in the same function, add `env_hash_inputs: Vec::new(),` to the struct literal.

For every other `Task` struct literal in the codebase (test fixtures), add `env_hash_inputs: Vec::new(),`. Find them with:

```sh
cd /Users/ken/workspace/ms/rage && grep -n "workspace_root: " crates/scheduler/src/runner.rs crates/scheduler/src/task.rs
```

Each construction site needs the new field. Do this mechanically — every `Task { ... }` literal must add `env_hash_inputs: Vec::new(),`. Likewise for the test in `crates/plugin/src/lib.rs` (if any) — search for `RootTask { ... }` everywhere and add `env_hash_inputs: Vec::new(),`:

```sh
cd /Users/ken/workspace/ms/rage && grep -rn "RootTask {" crates/
```

Update each construction site.

**Step 4: Update `root_task_fingerprint` to hash `env_hash_inputs`**

In `/Users/ken/workspace/ms/rage/crates/scheduler/src/runner.rs`, find `pub(crate) fn root_task_fingerprint(task: &Task) -> String` (around line 78) and extend it. Add this block AFTER the existing `for path in &task.input_paths { ... }` loop and BEFORE the final `hasher.finalize()` call:

```rust
    // Fold ecosystem-supplied env hash inputs (e.g. NODE_VERSION). Sort by
    // key so the order plugins push pairs in does not affect the fingerprint.
    let mut env_pairs = task.env_hash_inputs.clone();
    env_pairs.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in &env_pairs {
        hasher.update(b"env:");
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\0");
    }
```

Also bump the version tag at the start of the function from `"rage.root-task.v1\0"` to `"rage.root-task.v2\0"` so existing v1 cache entries are correctly invalidated:

```rust
hasher.update(b"rage.root-task.v2\0");
```

**Step 5: Update `TypeScriptPlugin::infer_root_tasks` to populate `env_hash_inputs`**

In `/Users/ken/workspace/ms/rage/crates/plugin-typescript/src/lib.rs`, the function returns one `RootTask` per detected lockfile. Add a small helper at the top of the file (just after `use plugin::...`):

```rust
fn read_node_version(workspace_root: &std::path::Path) -> Option<String> {
    // Mirrors scheduler::node_path::resolve_node_version. Duplicated here so
    // plugin-typescript does not depend on scheduler.
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".node-version")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".nvmrc")) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    if let Ok(s) = std::fs::read_to_string(workspace_root.join(".tool-versions")) {
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let tool = parts.next().unwrap_or("");
            if tool == "nodejs" || tool == "node" {
                if let Some(v) = parts.next() {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}
```

Then in `infer_root_tasks`, before each `return vec![plugin::RootTask { ... }]`, build the env vec:

```rust
fn infer_root_tasks(&self, workspace_root: &Path) -> Vec<plugin::RootTask> {
    let mut env_hash_inputs: Vec<(String, String)> = Vec::new();
    if let Some(v) = read_node_version(workspace_root) {
        env_hash_inputs.push(("NODE_VERSION".to_string(), v));
    }

    let pnpm_lock = workspace_root.join("pnpm-lock.yaml");
    if pnpm_lock.is_file() {
        return vec![plugin::RootTask {
            name: "install".to_string(),
            command: "pnpm install".to_string(),
            input_paths: vec![pnpm_lock],
            env_hash_inputs: env_hash_inputs.clone(),
        }];
    }

    let yarn_lock = workspace_root.join("yarn.lock");
    if yarn_lock.is_file() {
        return vec![plugin::RootTask {
            name: "install".to_string(),
            command: "yarn install".to_string(),
            input_paths: vec![yarn_lock],
            env_hash_inputs: env_hash_inputs.clone(),
        }];
    }

    let npm_lock = workspace_root.join("package-lock.json");
    if npm_lock.is_file() {
        return vec![plugin::RootTask {
            name: "install".to_string(),
            command: "npm install".to_string(),
            input_paths: vec![npm_lock],
            env_hash_inputs,
        }];
    }

    Vec::new()
}
```

**Step 6: Extend the `toolchain_allowlist`**

In the same file, append to the `Vec` returned by `toolchain_allowlist`:

```rust
            AllowlistEntry {
                path_pattern: "**/.local/share/fnm/**".to_string(),
                reason: "fnm-managed Node.js binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.nvm/**".to_string(),
                reason: "nvm-managed Node.js binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.asdf/**".to_string(),
                reason: "asdf-managed runtime binaries".to_string(),
            },
            AllowlistEntry {
                path_pattern: "**/.local/share/mise/**".to_string(),
                reason: "mise-managed runtime binaries".to_string(),
            },
```

(The existing `**/node_modules/.bin/**` entry is already present — leave it.)

**Step 7: Run all tests**

```sh
cd /Users/ken/workspace/ms/rage && cargo test --workspace 2>&1 | tail -20
```

Expected: all tests pass. If a `Task { ... }` struct literal anywhere is missing `env_hash_inputs`, compile error will tell you the file and line — add `env_hash_inputs: Vec::new(),` and re-run.

**Step 8: Commit**

```sh
cd /Users/ken/workspace/ms/rage && git add -A && git commit -m "feat(plugin): RootTask.env_hash_inputs; TS plugin folds NODE_VERSION + VM allowlist"
```

---

### Task 8 — Integration test + workspace verification

**Files:**
- Modify: `/Users/ken/workspace/ms/rage/crates/cli/tests/integration.rs`

**Step 1: Add the integration test**

Append to `/Users/ken/workspace/ms/rage/crates/cli/tests/integration.rs`. First, check the existing structure:

```sh
cd /Users/ken/workspace/ms/rage && head -40 crates/cli/tests/integration.rs
```

Add the following test (using whatever helpers/assertions already exist in that file — match the surrounding style; if `assert_cmd` is in use, follow that idiom):

```rust
#[test]
fn js_task_path_includes_node_modules_bin() {
    use std::fs;
    use tempfile::tempdir;

    // Build a minimal yarn workspace with one package whose `build` script
    // resolves a tool that ONLY exists at workspace_root/node_modules/.bin.
    let work = tempdir().unwrap();
    let root = work.path();
    fs::write(
        root.join("package.json"),
        br#"{"name":"r","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    fs::write(root.join("yarn.lock"), b"# yarn lockfile v1\n").unwrap();

    // Stub binary at workspace root: prints "FOUND" to stdout, exit 0.
    let ws_bin = root.join("node_modules/.bin");
    fs::create_dir_all(&ws_bin).unwrap();
    let stub = ws_bin.join("my-stub-tool");
    fs::write(&stub, b"#!/bin/sh\necho FOUND\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&stub, perms).unwrap();
    }

    // One package with a build script that just runs the stub.
    let pkg = root.join("packages/p");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(
        pkg.join("package.json"),
        br#"{"name":"p","version":"0.0.0","scripts":{"build":"my-stub-tool"}}"#,
    )
    .unwrap();
    // Avoid TS detection so we don't try to actually run yarn install.
    // The synthesized workspace#install will still run "yarn install" which
    // will fail without network — so we test PATH another way: invoke the
    // scheduler in-process via the public API.

    use plugin::EcosystemPlugin;
    use plugin_typescript::TypeScriptPlugin;
    use scheduler::node_path::build_node_path;

    let path = build_node_path(&pkg, root, "/usr/bin:/bin");
    assert!(
        path.contains(&format!("{}/node_modules/.bin", pkg.display())),
        "package node_modules/.bin missing from PATH: {path}"
    );
    assert!(
        path.contains(&format!("{}/node_modules/.bin", root.display())),
        "workspace node_modules/.bin missing from PATH: {path}"
    );

    // And verify that NODE_VERSION flows into the install task's env_hash_inputs.
    fs::write(root.join(".node-version"), "18.20.4\n").unwrap();
    let ts = TypeScriptPlugin::new();
    let rts = ts.infer_root_tasks(root);
    assert_eq!(rts.len(), 1);
    assert_eq!(
        rts[0].env_hash_inputs,
        vec![("NODE_VERSION".to_string(), "18.20.4".to_string())]
    );
}
```

If `crates/cli/Cargo.toml`'s `[dev-dependencies]` does not already include `scheduler`, `plugin`, `plugin-typescript`, and `tempfile`, add them (path-deps for the workspace crates, `tempfile` from crates.io). Verify with:

```sh
cd /Users/ken/workspace/ms/rage && grep -A 20 '\[dev-dependencies\]' crates/cli/Cargo.toml
```

**Step 2: Run the integration test**

```sh
cd /Users/ken/workspace/ms/rage && cargo test -p cli --test integration js_task_path_includes_node_modules_bin 2>&1 | tail -10
```

Expected: PASS.

**Step 3: Run the full workspace test suite**

```sh
cd /Users/ken/workspace/ms/rage && cargo test --workspace 2>&1 | tail -10
```

Expected: all tests pass, 0 failures (ignored macOS sandbox E2E tests are fine).

**Step 4: Run cargo fmt + clippy**

```sh
cd /Users/ken/workspace/ms/rage && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15
```

Expected: no diffs from fmt; clippy clean.

**Step 5: Manual verification on the live `lage` workspace**

Build release binary and run against the user's actual lage workspace:

```sh
cd /Users/ken/workspace/ms/rage && cargo build --release -p cli 2>&1 | tail -5
./target/release/rage run build ~/workspace/lage 2>&1 | head -20
```

Expected output:
- Line 1: `Found 25 packages (yarn workspace)` (or similar count)
- Line 2: `Running 'build' across 26 packages`
- Subsequent lines: `[rage] workspace#install ✓ (cached)` or starting/ok lines
- **No** `yarn: command not found` errors
- **No** `tsc: command not found` errors

If the install task starts (cache miss) it should run to completion with `yarn install` resolving correctly via the fnm-managed Node 18 bin directory.

**Step 6: Commit and final verification**

```sh
cd /Users/ken/workspace/ms/rage && git add -A && git commit -m "test(cli): integration coverage for node PATH injection + NODE_VERSION fingerprint"
```

Final check:
```sh
cd /Users/ken/workspace/ms/rage && cargo test --workspace 2>&1 | grep -E "test result|FAILED" | tail -20
```

Expected: every line is `test result: ok.` — zero failures across all crates.

---

## Acceptance criteria (verify before declaring Phase 1 done)

1. `cargo test --workspace` passes with zero failures.
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
3. `cargo fmt --all --check` returns 0.
4. `./target/release/rage run build ~/workspace/lage` shows no `command not found` errors for `yarn`, `node`, or `tsc`.
5. Toggling the workspace's `.node-version` (e.g. from `18.20.4` to `20.11.0`) and re-running `rage run build ~/workspace/lage` causes `workspace#install` to be a cache miss (re-runs), confirming NODE_VERSION is in the fingerprint. To check this manually:
   ```sh
   cd ~/workspace/lage && cat .node-version  # note current
   /path/to/rage run build .                 # first run — caches
   /path/to/rage run build .                 # second run — install cached
   echo "20.11.0" > .node-version
   /path/to/rage run build .                 # install must re-run
   ```
6. The `node_modules/.bin/**` entries plus `.local/share/fnm/**`, `.nvm/**`, `.asdf/**`, `.local/share/mise/**` all appear in `TypeScriptPlugin::toolchain_allowlist()`.

## COE constraints (must hold throughout)

1. PATH injection occurs at every `Command::new("sh")` site in `runner.rs` — by virtue of all sites using `node_path::build_node_path`. Use `grep -n 'Command::new("sh")' crates/scheduler/src/runner.rs` and confirm each is preceded by a `build_node_path` call within the same function.
2. `build_node_path` includes the resolved version manager bin dir whenever a `.node-version` (or `.nvmrc` / `.tool-versions`) file exists at the workspace root AND a matching VM install is on disk.
3. `which_first` checks `node_modules/.bin` (package then workspace) BEFORE the version manager bin dir, BEFORE the system PATH.
4. Version-file lookup priority: `.node-version` → `.nvmrc` → `.tool-versions`.
5. Missing version file is a no-op (no VM bin appended; no `NODE_VERSION` in `env_hash_inputs`).
6. `build_node_path` filters non-existent prepend candidates so the resulting PATH has no garbage entries.
