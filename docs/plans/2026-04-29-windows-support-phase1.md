# Windows Support Phase 1 Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Make the `scheduler` crate's shell dispatch and Node version manager path discovery work on Windows by introducing a `shell::command` / `shell::std_command` helper and adding Windows lookup branches to `find_version_manager_bin`.

**Architecture:** Two independent, additive changes. (1) A new `crates/scheduler/src/shell.rs` module wraps `tokio::process::Command` and `std::process::Command` so callers say `shell::command("cmd string")` instead of hard-coding `Command::new("sh").arg("-c").arg(...)`. The body of the helper picks `sh -c` on Unix and `cmd /c` on Windows via `#[cfg]`. (2) `node_path::find_version_manager_bin` gains a `USERPROFILE` fallback for `HOME` and a `#[cfg(windows)]` block that looks up fnm, nvm-windows, and Volta in the canonical Windows locations.

**Tech Stack:** Rust 2021, Tokio, Cargo workspace. No new dependencies. No `unsafe`. All tests use `tempfile` (already a dev-dependency in scheduler).

**Reference:** Design doc at `docs/plans/2026-04-29-windows-support-design.md`.

---

## Pre-flight checklist

Before starting Task 1, confirm the following:

- [ ] You are on a clean working tree on `main` (or a fresh feature branch off `main`).
- [ ] `cargo check -p scheduler` passes from `/Users/ken/workspace/ms/rage`.
- [ ] You have read the design doc above.
- [ ] You understand the difference between `tokio::process::Command` (async, used in `runner.rs`) and `std::process::Command` (sync, used in `postinstall_cache.rs`). They are **not** interchangeable — you cannot `.await` a `std::process::Command`, and you cannot use `tokio::process::Command` from a non-async function without a runtime. That's why we need two helpers.

If any check fails, stop and ask before continuing.

---

## Section A — Shell dispatch (Tasks 1–8)

### Task 1: Write failing tests for `shell.rs`

**Files:**
- Create: `crates/scheduler/src/shell.rs`

**Step 1: Create the file with only the test module (no implementation yet)**

Write this exact content to `crates/scheduler/src/shell.rs`:

```rust
//! Cross-platform shell dispatch.
//!
//! Production code never reaches for `Command::new("sh")` directly. Instead it
//! calls [`command`] (async) or [`std_command`] (sync), which select `sh -c`
//! on Unix and `cmd /c` on Windows.

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn command_uses_sh_on_unix() {
        let output = command("echo hello")
            .output()
            .await
            .expect("failed to run command");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn command_uses_cmd_on_windows() {
        let output = command("echo hello")
            .output()
            .await
            .expect("failed to run command");
        assert!(output.status.success());
        // `cmd /c echo hello` includes a trailing newline.
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    #[cfg(unix)]
    fn std_command_uses_sh_on_unix() {
        let output = std_command("echo hello")
            .output()
            .expect("failed to run command");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    #[cfg(windows)]
    fn std_command_uses_cmd_on_windows() {
        let output = std_command("echo hello")
            .output()
            .expect("failed to run command");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }
}
```

**Step 2: Verify tests do not compile yet**

Run: `cargo check -p scheduler 2>&1 | head -40`

Expected: error about the file existing but not being declared as a module (because `pub mod shell;` is not yet in `lib.rs`). That's fine — we will wire it up after the implementation lands. If you see anything other than module/path errors, stop and investigate.

> Why we don't run the tests here: `shell.rs` is not yet declared in `lib.rs`, so `cargo test` won't find the test module at all. The test verification happens in Task 2 after implementation + module declaration.

**Step 3: Do not commit yet** — we need the implementation first.

---

### Task 2: Implement `shell::command` and `shell::std_command`

**Files:**
- Modify: `crates/scheduler/src/shell.rs`

**Step 1: Add the implementations above the `#[cfg(test)] mod tests` block**

Edit `crates/scheduler/src/shell.rs` and insert the following two functions immediately after the module-level docstring and before `#[cfg(test)]`:

```rust
/// Build a `tokio::process::Command` that runs `cmd` through the platform shell.
///
/// On Unix this is `sh -c <cmd>`. On Windows this is `cmd /c <cmd>`.
pub fn command(cmd: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/c").arg(cmd);
        c
    }
}

/// Build a `std::process::Command` that runs `cmd` through the platform shell.
///
/// Use this from synchronous code paths where a Tokio runtime is not available
/// (for example, the postinstall cache). On Unix this is `sh -c <cmd>`; on
/// Windows this is `cmd /c <cmd>`.
pub fn std_command(cmd: &str) -> std::process::Command {
    #[cfg(unix)]
    {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("cmd");
        c.arg("/c").arg(cmd);
        c
    }
}
```

**Step 2: Do not run tests yet** — module is still not declared in `lib.rs`. Move to Task 3.

---

### Task 3: Wire `shell` into `lib.rs`

**Files:**
- Modify: `crates/scheduler/src/lib.rs`

**Step 1: Add the module declaration in alphabetical order**

The existing declarations are:
```
pub mod artifact_capture;
pub mod artifact_restore;
pub mod bin_links;
pub mod node_path;
pub mod postinstall_cache;
pub mod resource_budget;
pub mod rss_monitor;
pub mod runner;
pub mod task;
```

Insert `pub mod shell;` between `pub mod runner;` and `pub mod task;`:

```rust
pub mod runner;
pub mod shell;
pub mod task;
```

**Step 2: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -20`

Expected: `Finished` with no errors. Warnings about unused functions are OK at this point — the helpers are not called from production code yet.

**Step 3: Run the shell tests**

Run: `cargo test -p scheduler shell 2>&1 | tail -20`

Expected on macOS/Linux: 2 tests pass (`command_uses_sh_on_unix`, `std_command_uses_sh_on_unix`). The Windows tests are gated by `#[cfg(windows)]` and won't be compiled. Output should look like:

```
running 2 tests
test shell::tests::std_command_uses_sh_on_unix ... ok
test shell::tests::command_uses_sh_on_unix ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; ...
```

**Step 4: Do not commit yet** — we'll commit the whole shell-dispatch refactor as one logical unit at Task 8.

---

### Task 4: Migrate the simple `runner.rs` call sites (L236, L314, L1197)

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (three sites)

**Step 1: Read the surrounding context for each site**

Run: `grep -n 'Command::new("sh")' crates/scheduler/src/runner.rs`

You should see five hits. Tasks 4 and 5 together rewrite all five. The three "simple" sites use the chained-builder form: `Command::new("sh").arg("-c").arg(&task.command)...`. The two "builder" sites assign to `let mut cmd = Command::new("sh");` first; those are handled in Task 5.

**Step 2: Replace each simple site**

For **L236, L314, and L1197**, find the pattern (the surrounding chain may differ — these are illustrative):

```rust
Command::new("sh")
    .arg("-c")
    .arg(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path)
    .status()
    .await
```

Replace with:

```rust
crate::shell::command(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path)
    .status()
    .await
```

The variable name passed to `shell::command` is whatever the original code passed to `.arg(...)` after `.arg("-c")` — usually `&task.command`, but check each site. **Do not** change `current_dir`, `env`, `status`, or anything downstream. Only the constructor + the two `.arg` calls go away.

> Note on `use` imports: leave `use tokio::process::Command;` alone for now — Task 5 will determine whether it's still needed after the builder-site rewrites.

**Step 3: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -20`

Expected: clean compile. If you see "unused import: tokio::process::Command", ignore it for now — Task 5 may still need it. If you see anything else, fix before continuing.

**Step 4: Run the scheduler tests**

Run: `cargo test -p scheduler 2>&1 | tail -30`

Expected: same pass count as before your changes. If a test fails, you almost certainly fat-fingered an `arg` chain — re-read the diff.

**Step 5: Do not commit yet.**

---

### Task 5: Migrate the builder-style `runner.rs` sites (L910, L960)

**Files:**
- Modify: `crates/scheduler/src/runner.rs` (two sites)

**Step 1: Locate the builder blocks**

Around L910 you'll see:

```rust
let mut cmd = Command::new("sh");
cmd.arg("-c")
    .arg(&task.command)
    .current_dir(&task.cwd)
    .env("PATH", &new_path);
// ... possibly more `.env(...)` calls ...
spawn_capture_tee_tracked(builder /* or similar */)
```

L960 has a near-identical block feeding `spawn_capture_tee(builder2)`.

**Step 2: Replace the constructor + the first two `.arg` calls**

Replace the two-line head:

```rust
let mut cmd = Command::new("sh");
cmd.arg("-c")
    .arg(&task.command)
    .current_dir(&task.cwd)
```

with the single-line head:

```rust
let mut cmd = crate::shell::command(&task.command);
cmd.current_dir(&task.cwd)
```

Leave every subsequent `.env(...)`, `.stdin(...)`, `.stdout(...)`, etc. exactly as it was. Apply the same edit to the L960 block (the binding may be `cmd2` or similar — match the local naming).

**Step 3: Remove the now-unused `use tokio::process::Command;` import if appropriate**

Run: `grep -n 'Command::new\|: Command\b' crates/scheduler/src/runner.rs`

If there are no remaining uses of the bare `Command` type, delete the `use tokio::process::Command;` line at L11. If any remain (e.g. type annotations in helper signatures), leave the import alone.

**Step 4: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -20`

Expected: clean compile, no warnings about unused imports.

**Step 5: Run the scheduler tests**

Run: `cargo test -p scheduler 2>&1 | tail -30`

Expected: all tests still pass. Pay special attention to any `runner` integration tests — those exercise the L910/L960 paths.

**Step 6: Do not commit yet.**

---

### Task 6: Migrate `postinstall_cache.rs` (L197)

**Files:**
- Modify: `crates/scheduler/src/postinstall_cache.rs`

**Step 1: Locate the call site**

Around L197 you'll see:

```rust
let status = std::process::Command::new("sh")
    .arg("-c")
    .arg(&task.script)
    .current_dir(&task.cwd)
    .env("PATH", new_path)
    .status()?;
```

**Step 2: Replace with the std-command helper**

```rust
let status = crate::shell::std_command(&task.script)
    .current_dir(&task.cwd)
    .env("PATH", new_path)
    .status()?;
```

**Step 3: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -20`

Expected: clean compile.

**Step 4: Run the scheduler tests**

Run: `cargo test -p scheduler 2>&1 | tail -30`

Expected: all tests pass. The postinstall cache has its own test module — make sure nothing there regressed.

**Step 5: Do not commit yet.**

---

### Task 7: Guard the `sh`-using test in `rss_monitor.rs`

**Files:**
- Modify: `crates/scheduler/src/rss_monitor.rs`

**Step 1: Locate the test**

Around L87 (inside `#[cfg(test)]`) you'll see:

```rust
let child = tokio::process::Command::new("sh")
    .arg("-c")
    .arg("sleep 0.2")
    .spawn()
    .expect("failed to spawn test subprocess");
```

This is **test-only** code. Production `track_peak_rss` uses `sysinfo` and is already cross-platform. We just need this test to skip on Windows — it relies on Unix's `sh` and `sleep`.

**Step 2: Add a `#[cfg(unix)]` guard to the test function**

Find the test function that contains the snippet above. It will look like:

```rust
#[tokio::test]
async fn some_test_name() {
    // ... setup ...
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 0.2")
        .spawn()
        .expect("failed to spawn test subprocess");
    // ... rest of test ...
}
```

Add `#[cfg(unix)]` immediately above `#[tokio::test]`:

```rust
#[cfg(unix)]
#[tokio::test]
async fn some_test_name() {
    // unchanged body
}
```

> Why not `#[cfg_attr(not(unix), ignore)]`? Because on Windows the body would still need to compile, and `Command::new("sh")` would be a runtime failure even if ignored. `#[cfg(unix)]` removes the function entirely from non-Unix builds, which is what we want.

**Step 3: Verify compilation**

Run: `cargo check -p scheduler --tests 2>&1 | tail -20`

Expected: clean compile.

**Step 4: Run the rss_monitor tests**

Run: `cargo test -p scheduler rss_monitor 2>&1 | tail -20`

Expected on macOS/Linux: same number of tests pass as before. The guard is a no-op on Unix.

**Step 5: Run the full scheduler suite one last time**

Run: `cargo test -p scheduler 2>&1 | tail -30`

Expected: every test passes.

---

### Task 8: Commit shell dispatch work

**Files (staged):**
- `crates/scheduler/src/shell.rs` (new)
- `crates/scheduler/src/lib.rs`
- `crates/scheduler/src/runner.rs`
- `crates/scheduler/src/postinstall_cache.rs`
- `crates/scheduler/src/rss_monitor.rs`

**Step 1: Stage the changes**

Run:
```
git add crates/scheduler/src/shell.rs crates/scheduler/src/lib.rs crates/scheduler/src/runner.rs crates/scheduler/src/postinstall_cache.rs crates/scheduler/src/rss_monitor.rs
```

**Step 2: Sanity check what you're about to commit**

Run: `git diff --cached --stat`

Expected: 5 files changed, with `shell.rs` being a brand-new file. Make sure no stray edits are included.

**Step 3: Commit**

Run:
```
git commit -m "feat(scheduler): add shell::command/std_command for cross-platform sh dispatch"
```

Expected: a single commit on the current branch.

---

## Section B — Node version manager paths (Tasks 9–15)

> All edits in this section are inside `crates/scheduler/src/node_path.rs`. The existing `mod tests` block starts at L269; helpers live near L339 (`fake_fnm`), L348 (`fake_nvm`), and L378 (`with_home`). Read those before editing so your additions match the surrounding style.

### Task 9: Add Windows test helpers and failing Windows-path tests

**Files:**
- Modify: `crates/scheduler/src/node_path.rs` (test module only)

**Step 1: Read the existing test infrastructure**

Run: `sed -n '260,400p' crates/scheduler/src/node_path.rs`

Note the conventions: helpers are plain `fn`s inside `mod tests`, environment isolation is via `with_home(...)` which saves/restores `HOME` around a closure, fake directory factories return the inner-most `PathBuf` that the function under test should yield.

**Step 2: Add the Windows env helper next to `with_home`**

Inside `mod tests`, immediately after the `with_home` function (around L378–L400 in the current file), add:

```rust
/// Saves and restores Windows-style env vars, runs `f()` in between.
/// Mirrors the existing `with_home` pattern. Catches panics so env vars
/// always get restored even on assertion failure.
fn with_windows_env<F: FnOnce()>(local_app_data: &Path, app_data: &Path, f: F) {
    let old_lad = std::env::var_os("LOCALAPPDATA");
    let old_ad = std::env::var_os("APPDATA");
    let old_fnm = std::env::var_os("FNM_DIR");
    let old_nvm = std::env::var_os("NVM_HOME");
    let old_volta = std::env::var_os("VOLTA_HOME");
    let old_up = std::env::var_os("USERPROFILE");

    std::env::set_var("LOCALAPPDATA", local_app_data);
    std::env::set_var("APPDATA", app_data);
    std::env::remove_var("FNM_DIR");
    std::env::remove_var("NVM_HOME");
    std::env::remove_var("VOLTA_HOME");
    std::env::remove_var("USERPROFILE");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    match old_lad {
        Some(v) => std::env::set_var("LOCALAPPDATA", v),
        None => std::env::remove_var("LOCALAPPDATA"),
    }
    match old_ad {
        Some(v) => std::env::set_var("APPDATA", v),
        None => std::env::remove_var("APPDATA"),
    }
    match old_fnm {
        Some(v) => std::env::set_var("FNM_DIR", v),
        None => std::env::remove_var("FNM_DIR"),
    }
    match old_nvm {
        Some(v) => std::env::set_var("NVM_HOME", v),
        None => std::env::remove_var("NVM_HOME"),
    }
    match old_volta {
        Some(v) => std::env::set_var("VOLTA_HOME", v),
        None => std::env::remove_var("VOLTA_HOME"),
    }
    match old_up {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }

    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
```

**Step 3: Add the Windows fake-path factories next to `fake_fnm` / `fake_nvm`**

Immediately after `fake_nvm` (around L348–L375), add:

```rust
fn fake_fnm_windows(local_app_data: &Path, version: &str) -> PathBuf {
    let bin = local_app_data
        .join("fnm")
        .join("node-versions")
        .join(format!("v{}", version))
        .join("installation");
    std::fs::create_dir_all(&bin).unwrap();
    bin
}

fn fake_nvm_windows(app_data: &Path, version: &str) -> PathBuf {
    let bin = app_data.join("nvm").join(format!("v{}", version));
    std::fs::create_dir_all(&bin).unwrap();
    bin
}

fn fake_volta_windows(local_app_data: &Path) -> PathBuf {
    let bin = local_app_data.join("Volta").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    bin
}
```

**Step 4: Add the failing tests at the bottom of `mod tests`**

Append (still inside `mod tests`):

```rust
#[test]
#[cfg(windows)]
fn finds_fnm_windows_default_path() {
    let tmp = tempfile::tempdir().unwrap();
    let local_app_data = tmp.path().join("local_app_data");
    let app_data = tmp.path().join("app_data");
    let expected = fake_fnm_windows(&local_app_data, "18.20.4");
    with_windows_env(&local_app_data, &app_data, || {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
    });
}

#[test]
#[cfg(windows)]
fn finds_fnm_windows_via_fnm_dir_env() {
    let tmp = tempfile::tempdir().unwrap();
    let fnm_dir = tmp.path().join("custom_fnm");
    let bin = fnm_dir
        .join("node-versions")
        .join("v18.20.4")
        .join("installation");
    std::fs::create_dir_all(&bin).unwrap();
    let old = std::env::var_os("FNM_DIR");
    std::env::set_var("FNM_DIR", &fnm_dir);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(bin.clone()));
    }));
    match old {
        Some(v) => std::env::set_var("FNM_DIR", v),
        None => std::env::remove_var("FNM_DIR"),
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[cfg(windows)]
fn finds_nvm_windows_default_path() {
    let tmp = tempfile::tempdir().unwrap();
    let local_app_data = tmp.path().join("local_app_data");
    let app_data = tmp.path().join("app_data");
    let expected = fake_nvm_windows(&app_data, "18.20.4");
    with_windows_env(&local_app_data, &app_data, || {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
    });
}

#[test]
#[cfg(windows)]
fn finds_volta_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let local_app_data = tmp.path().join("local_app_data");
    let app_data = tmp.path().join("app_data");
    let expected = fake_volta_windows(&local_app_data);
    with_windows_env(&local_app_data, &app_data, || {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(expected.clone()));
    });
}

#[test]
fn userprofile_used_as_home_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    // Create fnm structure under fake USERPROFILE (Unix-style layout, since
    // this test exercises the *fallback* path on any OS).
    let userprofile = tmp.path();
    let fnm_bin = userprofile
        .join(".local")
        .join("share")
        .join("fnm")
        .join("node-versions")
        .join("v18.20.4")
        .join("installation")
        .join("bin");
    std::fs::create_dir_all(&fnm_bin).unwrap();

    let old_home = std::env::var_os("HOME");
    let old_up = std::env::var_os("USERPROFILE");
    std::env::remove_var("HOME");
    std::env::set_var("USERPROFILE", userprofile);
    std::env::remove_var("FNM_DIR");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(fnm_bin.clone()));
    }));

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_up {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
```

> Why `#[cfg(windows)]` on four of the five tests: the Windows lookup branch we add in Task 11 is itself behind `#[cfg(windows)]`. On macOS/Linux those tests would always return `None` — vacuous and confusing. The `userprofile_used_as_home_fallback` test runs on **all** platforms because the `HOME` / `USERPROFILE` fallback we add in Task 10 is platform-independent.

**Step 5: Run the tests — they MUST fail (or be skipped on this platform)**

Run: `cargo test -p scheduler node_path 2>&1 | tail -30`

Expected on macOS/Linux:
- `userprofile_used_as_home_fallback` **fails** (USERPROFILE fallback doesn't exist yet — `HOME` is unset, function returns `None`, assertion fails).
- The four `#[cfg(windows)]` tests are not compiled, so they don't appear in the output.

If `userprofile_used_as_home_fallback` passes, you missed something — re-read the existing `find_version_manager_bin`. The current code only reads `HOME`, so removing `HOME` should make it return `None`.

**Step 6: Do not commit yet.**

---

### Task 10: Add USERPROFILE fallback for HOME

**Files:**
- Modify: `crates/scheduler/src/node_path.rs` (production code)

**Step 1: Locate the `home` binding**

Around L117 in `find_version_manager_bin`:

```rust
pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // ... existing fnm / nvm / asdf / mise lookups follow ...
```

**Step 2: Replace the `home` binding**

Change to:

```rust
pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    // ... existing fnm / nvm / asdf / mise lookups unchanged ...
```

That's the entire change — every existing Unix lookup keeps using `home` and gets the fallback for free.

**Step 3: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -10`

Expected: clean compile.

**Step 4: Run the targeted test**

Run: `cargo test -p scheduler node_path::tests::userprofile_used_as_home_fallback -- --nocapture 2>&1 | tail -20`

Expected: `test ... ok`. If it fails, read the output carefully — the most likely cause is forgetting `.or_else`.

**Step 5: Run the full node_path test module**

Run: `cargo test -p scheduler node_path 2>&1 | tail -20`

Expected: every existing test still passes (the new fallback is additive — when `HOME` is set, behavior is unchanged).

**Step 6: Do not commit yet.**

---

### Task 11: Add the Windows lookup block to `find_version_manager_bin`

**Files:**
- Modify: `crates/scheduler/src/node_path.rs` (production code)

**Step 1: Locate the end of the function**

Read the function body from L117 to its closing brace. The existing structure is roughly:

```rust
pub fn find_version_manager_bin(version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);

    // fnm checks (Unix paths)
    // ...

    // nvm checks (Unix paths)
    // ...

    // asdf checks
    // ...

    // mise checks
    // ...

    None
}
```

**Step 2: Insert the Windows block immediately before the trailing `None`**

Add this block (note: it sits next to the trailing `None`, *not* after it):

```rust
    #[cfg(windows)]
    {
        // fnm on Windows: %FNM_DIR% (if set) or %LOCALAPPDATA%\fnm.
        // Layout: <fnm>\node-versions\v{ver}\installation\
        // (no `bin/` subdirectory — node.exe sits directly in `installation\`.)
        let fnm_dir = std::env::var_os("FNM_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("fnm"))
            });
        if let Some(fnm_dir) = fnm_dir {
            let versions_dir = fnm_dir.join("node-versions");
            if let Some(ver_dir) = resolve_version_dir(&versions_dir, version) {
                let bin = ver_dir.join("installation");
                if bin.is_dir() {
                    return Some(bin);
                }
            }
        }

        // nvm-windows: %NVM_HOME% or %APPDATA%\nvm.
        // Layout: <nvm>\v{ver}\ (node.exe directly in version dir)
        let nvm_home = std::env::var_os("NVM_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("nvm"))
            });
        if let Some(nvm_dir) = nvm_home {
            if let Some(ver_dir) = resolve_version_dir(&nvm_dir, version) {
                if ver_dir.is_dir() {
                    return Some(ver_dir);
                }
            }
        }

        // Volta on Windows: %VOLTA_HOME%\bin or %LOCALAPPDATA%\Volta\bin.
        let volta_bin = std::env::var_os("VOLTA_HOME")
            .map(|p| PathBuf::from(p).join("bin"))
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("Volta").join("bin"))
            });
        if let Some(bin) = volta_bin {
            if bin.is_dir() {
                return Some(bin);
            }
        }
    }

    None
}
```

> Reuse note: `resolve_version_dir` already lives at L85 and tries both `{ver}` and `v{ver}` subdirectories. We use it here so version coercion behaves identically on every platform.

**Step 3: Verify compilation on macOS/Linux**

Run: `cargo check -p scheduler 2>&1 | tail -10`

Expected: clean compile. The `#[cfg(windows)]` block is dead code on this platform, which is fine.

**Step 4: Confirm cross-compile compiles cleanly (best-effort)**

Run: `cargo check -p scheduler --target x86_64-pc-windows-gnu 2>&1 | head -30`

Expected outcomes:
- **If the toolchain is installed:** clean compile, exit 0.
- **If the toolchain is missing:** an error like `error[E0463]: can't find crate for 'core'` or `the toolchain ... is not installed`. **This is acceptable for Phase 1** — log the result and continue. We will run real Windows CI in a later phase.

If you see a `#[cfg(windows)]`-specific error (a typo, an undeclared name, a borrow problem), fix it before continuing. If you can't tell whether an error is toolchain-related or code-related, paste the full output into your work log and ask.

**Step 5: Run the scheduler tests**

Run: `cargo test -p scheduler node_path 2>&1 | tail -20`

Expected: same set of tests pass as after Task 10 (the four `#[cfg(windows)]` tests still don't compile on this host).

**Step 6: Do not commit yet.**

---

### Task 12: Add Volta to the Unix block + a Unix test for it

**Files:**
- Modify: `crates/scheduler/src/node_path.rs` (production code + test module)

**Step 1: Write the failing test first**

Inside `mod tests`, append a new test that uses the existing `with_home` helper:

```rust
#[test]
fn finds_volta_unix() {
    let home = tempfile::tempdir().unwrap();
    let volta_bin = home.path().join(".volta").join("bin");
    std::fs::create_dir_all(&volta_bin).unwrap();
    with_home(home.path(), || {
        assert_eq!(find_version_manager_bin("18.20.4"), Some(volta_bin.clone()));
    });
}
```

> Note: `with_home` does **not** clear `VOLTA_HOME`. If this test ever flakes on a machine where `VOLTA_HOME` is set in the environment, we'll add it to `with_home`. For now, keep parity with existing tests.

**Step 2: Run the test — it must fail**

Run: `cargo test -p scheduler node_path::tests::finds_volta_unix 2>&1 | tail -20`

Expected: `test ... FAILED`. The assertion compares against `Some(volta_bin)` but the function returns `None` because the Unix block has no Volta lookup yet.

**Step 3: Add the Volta lookup to the Unix-side of `find_version_manager_bin`**

Inside `find_version_manager_bin`, immediately after the existing `mise` (or last Unix-side) check and **before** the `#[cfg(windows)]` block from Task 11, add:

```rust
    // Volta on Unix: $VOLTA_HOME/bin or ~/.volta/bin.
    if let Some(ref home) = home {
        let volta_bin = std::env::var_os("VOLTA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".volta"))
            .join("bin");
        if volta_bin.is_dir() {
            return Some(volta_bin);
        }
    }
```

**Step 4: Verify compilation**

Run: `cargo check -p scheduler 2>&1 | tail -10`

Expected: clean compile.

**Step 5: Re-run the targeted test**

Run: `cargo test -p scheduler node_path::tests::finds_volta_unix 2>&1 | tail -20`

Expected: `test ... ok`.

**Step 6: Re-run the full node_path module**

Run: `cargo test -p scheduler node_path 2>&1 | tail -30`

Expected: all tests pass, no regressions.

**Step 7: Do not commit yet.**

---

### Task 13: Run the full scheduler suite

**Files:** none (verification only).

**Step 1: Run every test in the scheduler crate**

Run: `cargo test -p scheduler 2>&1 | tail -30`

Expected: every test passes. The summary line should look like `test result: ok. N passed; 0 failed; ...`.

**Step 2: If any test fails**

Read the failure carefully. The most likely causes, by likelihood:

1. A typo in a Windows-block path join (compiles but returns wrong path on Windows — wouldn't affect macOS, so this is unlikely to surface here).
2. An accidental edit to a non-Windows code path — diff `crates/scheduler/src/node_path.rs` against `git HEAD` and look for unintended changes.
3. A missed call site in `runner.rs` — re-run `grep -n 'Command::new("sh")' crates/scheduler/src/runner.rs`. Expected: zero hits in production code (the `rss_monitor.rs` test still has its own `Command::new("sh")` but it's now `#[cfg(unix)]`-guarded).

Fix and re-run until clean. **Do not move on with red tests.**

---

### Task 14: Cross-platform compile check

**Files:** none (verification only).

**Step 1: Try cross-compiling to Windows**

Run: `cargo check -p scheduler --target x86_64-pc-windows-gnu 2>&1 | head -50`

**Step 2: Interpret the output**

- **Clean compile, exit 0:** great — the Windows block is at least syntactically valid against the windows-gnu target.
- **Toolchain not installed** (e.g. `error: toolchain 'stable-x86_64-pc-windows-gnu' is not installed` or `can't find crate for 'core'`): acceptable for Phase 1. Note it in your work log and proceed to Step 3.
- **Code-level error in our edits** (something specific to `node_path.rs` or `shell.rs` with `windows` in the cfg): fix before committing.

**Step 3: Always run the host-target check**

Run: `cargo check -p scheduler 2>&1 | tail -10`

Expected: clean compile. This must succeed regardless of whether the Windows cross-compile worked.

---

### Task 15: Commit node path work

**Files (staged):**
- `crates/scheduler/src/node_path.rs`

**Step 1: Stage the changes**

Run: `git add crates/scheduler/src/node_path.rs`

**Step 2: Sanity check what you're committing**

Run: `git diff --cached --stat`

Expected: 1 file changed. Run `git diff --cached crates/scheduler/src/node_path.rs | head -80` and skim the diff — confirm you see (a) the `USERPROFILE` fallback, (b) the Volta-on-Unix block, (c) the `#[cfg(windows)]` block with fnm/nvm/Volta, (d) the new test helpers and tests.

**Step 3: Commit**

Run:
```
git commit -m "feat(scheduler): add Windows node version manager paths (fnm, nvm-windows, Volta)"
```

**Step 4: Verify commit log**

Run: `git log --oneline -3`

Expected:
```
<hash> feat(scheduler): add Windows node version manager paths (fnm, nvm-windows, Volta)
<hash> feat(scheduler): add shell::command/std_command for cross-platform sh dispatch
<hash> docs: revise Windows support design after adversarial review
```

---

## Final verification (do this before declaring Phase 1 complete)

1. `cargo test -p scheduler 2>&1 | tail -30` — all green.
2. `cargo check -p scheduler 2>&1 | tail -10` — clean.
3. `grep -n 'Command::new("sh")' crates/scheduler/src/runner.rs crates/scheduler/src/postinstall_cache.rs` — zero hits.
4. `grep -n 'Command::new("sh")' crates/scheduler/src/rss_monitor.rs` — one hit, inside a `#[cfg(unix)]`-guarded test.
5. `git log --oneline -2` — exactly two new commits matching the messages above.

If any of these checks fail, do not declare done — fix the issue and re-verify.

---

## Reference command crib sheet

| Task | Command |
|------|---------|
| Compile-check the scheduler crate | `cargo check -p scheduler` |
| Run all scheduler tests | `cargo test -p scheduler 2>&1 | tail -30` |
| Run only the `shell` module's tests | `cargo test -p scheduler shell 2>&1 | tail -20` |
| Run only the `node_path` module's tests | `cargo test -p scheduler node_path 2>&1 | tail -30` |
| Run a single test by path | `cargo test -p scheduler node_path::tests::<name> -- --nocapture` |
| Cross-compile to Windows (if toolchain present) | `cargo check -p scheduler --target x86_64-pc-windows-gnu 2>&1 | head -50` |
| Find remaining `sh` call sites | `grep -rn 'Command::new("sh")' crates/scheduler/src/` |

---

## What's explicitly out of scope for Phase 1

These belong to later phases — do **not** start them, even if you finish Phase 1 early:

- Path-separator handling for `PATH` env injection (Phase 2).
- Symlink-vs-junction handling in `bin_links.rs` (Phase 2).
- `.cmd` / `.ps1` shim wrappers for npm-installed binaries (Phase 2).
- File-locking semantics on Windows (Phase 3).
- Real Windows CI runner (Phase 3).

If you find yourself reaching for any of these, stop and flag it.
