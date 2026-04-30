# Windows Support Phase 3 — Sandbox Integration Test + CI Hardening

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Add a real end-to-end Windows sandbox integration test (DLL injection → hook → pipe → events) and harden the `sandbox-smoke-windows` CI job with an exit-code assertion plus a step that runs the new test.

**Architecture:** A new Cargo integration-test binary at `crates/sandbox/tests/windows_integration.rs` (cfg-gated to Windows, `#[ignore]` by default) drives `sandbox::run_sandboxed` with a command that reads a known file under `C:\Windows\` via `CreateFileW`, then asserts the resulting `RunResult.path_set.reads` contains a Windows path. CI builds the Detours DLL, points `RAGE_SANDBOX_DLL_PATH` at it, runs the integration test with `--include-ignored`, and adds a `$LASTEXITCODE` check to the existing PowerShell smoke step so it can no longer silently pass after a crash.

**Tech Stack:** Rust 1.x (workspace edition), Cargo, `tokio`, `anyhow`, GitHub Actions (`windows-latest`), PowerShell 7.

---

## ⚠️ Heads-up: corrections from delegation instructions

The delegation instruction listed several "EXACT codebase facts" that did **not** match the current repo. The plan below uses the verified facts. The corrections:

| Claim in delegation | Reality (verified) |
|---|---|
| `run_sandboxed(cmd: &str, env: &[(String, String)]) -> anyhow::Result<Vec<AccessEvent>>` | `run_sandboxed(cmd: &str, cwd: &Path, env: &[(String, String)]) -> anyhow::Result<RunResult>` (`crates/sandbox/src/windows.rs:406`) |
| Test asserts on `Vec<AccessEvent>` | Real return is `RunResult { exit_code: i32, path_set: PathSet }` where `PathSet { reads: Vec<PathBuf>, writes: Vec<PathBuf> }` (`crates/sandbox/src/event.rs:50`) |
| `sandbox::AccessEvent` re-export | Not re-exported. `lib.rs` only re-exports `PathSet, RunResult`. Test asserts on `result.path_set.reads` (no `AccessEvent` import needed). |
| "`crates/sandbox/tests/` does not exist yet" | Directory already exists (contains `symlink_poc.rs`). Task 1 just creates one new file. |
| Smoke step uses `actions-rs/toolchain@v1` and `target/debug/...` | Real job uses `dtolnay/rust-toolchain@stable`, builds `--release`, copies DLL to `target/release/rage_sandbox.dll` (`.github/workflows/ci.yml:174-213`). Plan targets release paths. |

If you find more drift while implementing, **stop and flag it** — don't paper over it.

---

## Pre-flight (do this once before Task 1)

Confirm you're on `main` with a clean tree (or a fresh feature branch off `main`) and the existing tests pass on macOS:

```bash
cd /Users/ken/workspace/ms/rage
git status                    # expect clean (or known unrelated changes)
cargo check -p sandbox        # expect: Finished in N s
cargo test -p sandbox 2>&1 | tail -20   # expect: all tests passing on macOS
```

Expected last line of the test run: `test result: ok. N passed; 0 failed; ...`

If these don't pass, stop and ask.

---

## Task 1: Create the failing integration-test skeleton

**Files:**
- Create: `crates/sandbox/tests/windows_integration.rs`

**Background for the implementer (read this — don't skip):**

- `crates/sandbox/tests/` already exists. You're adding a sibling to `symlink_poc.rs`.
- Cargo automatically picks up `tests/*.rs` as separate integration-test binaries.
- The file MUST start with `#![cfg(target_os = "windows")]` so the whole module compiles to nothing on macOS/Linux. That lets us land it on `main` without breaking the macOS/Linux CI legs.
- The test must be `#[ignore]` by default — it requires the DLL artifact, which only exists after `cargo build -p sandbox-windows-detours`. CI will run it explicitly with `--include-ignored`.
- We use `cmd /c type C:\Windows\System32\drivers\etc\hosts` (NOT `dir`). `type` opens the file via `CreateFileW`, which is one of the two functions the DLL hooks. `dir` uses `FindFirstFileW`/`FindNextFileW` and would silently produce zero events.
- The actual return type of `sandbox::run_sandboxed` on Windows is `anyhow::Result<RunResult>`, where `RunResult { exit_code: i32, path_set: PathSet }` and `PathSet { reads: Vec<PathBuf>, writes: Vec<PathBuf> }`. We assert on `result.path_set.reads`, not `AccessEvent`.

**Step 1.1: Create the test file**

Write exactly this content to `crates/sandbox/tests/windows_integration.rs`:

```rust
#![cfg(target_os = "windows")]

//! End-to-end Windows sandbox integration test.
//!
//! Verifies the full pipeline: parent creates named pipe → DLL is injected
//! into the child → DLL hooks `CreateFileW` → events flow through the pipe
//! back to the parent → `run_sandboxed` returns them in `RunResult.path_set`.
//!
//! This test is `#[ignore]` by default because it needs the
//! `sandbox-windows-detours` DLL artifact. CI runs it explicitly via
//! `cargo test -p sandbox --test windows_integration -- --include-ignored`
//! after building the DLL and setting `RAGE_SANDBOX_DLL_PATH`.

use std::path::Path;

/// Drives `run_sandboxed` against `cmd /c type ...hosts` and asserts that the
/// hook captured at least one read under `C:\Windows\`.
///
/// Why this command? `type` opens the file via `CreateFileW` (which the DLL
/// hooks). `dir` uses `FindFirstFileW`/`FindNextFileW` (NOT hooked) and would
/// return zero events.
#[tokio::test]
#[ignore = "requires rage_sandbox.dll — build `cargo build -p sandbox-windows-detours` and run with --include-ignored"]
async fn dll_injection_produces_file_access_events() {
    // Resolve the DLL path via the workspace target dir. CARGO_MANIFEST_DIR
    // points at crates/sandbox/, so go up two levels to the workspace root.
    // Prefer the path the env var points at (CI sets it explicitly); fall
    // back to a debug build colocated with the workspace target/.
    if std::env::var("RAGE_SANDBOX_DLL_PATH").is_err() {
        let dll_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("debug")
            .join("rage_sandbox.dll");
        if dll_path.exists() {
            std::env::set_var(
                "RAGE_SANDBOX_DLL_PATH",
                dll_path.canonicalize().expect("canonicalize DLL path"),
            );
        }
        // If the DLL still isn't found, run_sandboxed below will return a
        // descriptive error that fails the test — that's the desired behavior.
    }

    let result = sandbox::run_sandboxed(
        "cmd /c type C:\\Windows\\System32\\drivers\\etc\\hosts",
        Path::new("C:\\"),
        &[],
    )
    .await
    .expect("run_sandboxed should succeed when the DLL is present");

    assert_eq!(
        result.exit_code, 0,
        "`type hosts` should exit 0 (file always exists on Windows)"
    );

    let saw_windows_read = result.path_set.reads.iter().any(|p| {
        let s = p.to_string_lossy().to_lowercase();
        s.starts_with("c:\\windows\\")
    });

    assert!(
        saw_windows_read,
        "expected at least one read under C:\\Windows\\ — got reads={:?}, writes={:?}",
        result.path_set.reads, result.path_set.writes
    );
}
```

**Step 1.2: Verify the file compiles on macOS as a no-op**

Run:

```bash
cargo test -p sandbox --test windows_integration 2>&1 | tail -20
```

Expected output (approximately):

```
   Compiling sandbox v0.0.0 (...)
    Finished `test` profile [unoptimized + debuginfo] target(s) in N.NNs
     Running tests/windows_integration.rs (target/debug/deps/windows_integration-XXX)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The `#![cfg(target_os = "windows")]` strips everything on macOS, so 0 tests is correct.

**If you see compile errors:** something is off — most likely a missing dependency (Task 2 fixes one of those, but the file as written should still compile because the cfg attribute strips it on macOS). Re-read the code for typos and stop to ask before patching anything.

**Step 1.3: Commit the failing-by-design skeleton**

```bash
git add crates/sandbox/tests/windows_integration.rs
git commit -m "test(sandbox): add Windows DLL-injection integration test skeleton (cfg-gated)"
```

---

## Task 2: Add `tokio` to `[dev-dependencies]` so the test compiles on Windows

**Files:**
- Modify: `crates/sandbox/Cargo.toml`

**Background:**

Integration test binaries in `tests/` are compiled as separate crates that link the library. They get the library's public API for free, but to use macros like `#[tokio::test]` they need `tokio` declared as a dev-dependency of the crate (the existing `[dependencies]` `tokio = { version = "1", features = ["full"] }` reaches inline `#[cfg(test)]` modules in `src/`, but **not** `tests/*.rs` integration-test binaries reliably across all toolchains — being explicit avoids confusion and matches Rust convention).

Today `crates/sandbox/Cargo.toml` only has `tempfile = "3"` under `[dev-dependencies]`. Add `tokio` with the runtime + macros features.

**Step 2.1: Read the current `[dev-dependencies]` block**

```bash
sed -n '/\[dev-dependencies\]/,$p' crates/sandbox/Cargo.toml
```

Expected current contents:

```toml
[dev-dependencies]
tempfile = "3"
```

**Step 2.2: Add the `tokio` dev-dependency**

Edit `crates/sandbox/Cargo.toml` and replace the `[dev-dependencies]` block with:

```toml
[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["rt", "macros", "rt-multi-thread"] }
```

**Step 2.3: Verify it still compiles on macOS**

```bash
cargo check -p sandbox 2>&1 | tail -10
cargo test -p sandbox --test windows_integration 2>&1 | tail -10
```

Both should finish cleanly. The integration-test run still reports `running 0 tests` on macOS.

**Step 2.4: Commit**

```bash
git add crates/sandbox/Cargo.toml
git commit -m "build(sandbox): add tokio dev-dependency for integration tests"
```

---

## Task 3: Verify existing tests still pass and audit the new layout

**Files:** none modified — this task is verification only.

**Step 3.1: Run all sandbox tests on macOS**

```bash
cargo test -p sandbox 2>&1 | tail -30
```

Expected: every existing inline test in `windows.rs` is cfg'd to `target_os = "windows"` so on macOS you should see all the existing macOS/event/pipe_proto tests pass and the integration test report `0 tests`. Final line should be `test result: ok.` for every test binary.

**If anything fails:** stop. Do not edit code. Re-read the failure, check `git diff` to see if Task 1 or 2 went sideways, and ask.

**Step 3.2: Confirm the integration test binary exists and is discoverable**

```bash
cargo test -p sandbox --test windows_integration --no-run 2>&1 | tail -5
ls crates/sandbox/tests/
```

Expected from the `ls`:

```
symlink_poc.rs
windows_integration.rs
```

**Step 3.3: Confirm no untracked changes you didn't expect**

```bash
git status
```

Expected: clean working tree (your last two commits already include the file and the Cargo.toml change).

This task does **not** produce a commit — it's a checkpoint.

---

## Task 4a: Wire the integration test into CI

**Files:**
- Modify: `.github/workflows/ci.yml` (the `sandbox-smoke-windows` job, around lines 174–213)

**Background:**

The existing `sandbox-smoke-windows` job:
1. Downloads the prebuilt rage Windows binary as an artifact.
2. Builds `sandbox-windows-detours` with `--release`.
3. Copies the resulting DLL to `target\release\rage_sandbox.dll`.
4. Runs `rage.exe run build .` and pipes through `Select-String`.

We need to add a step that runs the new integration test. It must run **after** the DLL has been copied to `target/release/rage_sandbox.dll` and **before** (or after — order is independent here) the smoke test step.

The integration test reads `RAGE_SANDBOX_DLL_PATH` at runtime, so we point it at the release DLL location. We use `${{ github.workspace }}` for an absolute path so the env var is correct regardless of cwd.

**Step 4a.1: Read the current smoke job to anchor the edit**

```bash
sed -n '170,220p' .github/workflows/ci.yml
```

You should see the steps in this order (verbatim from the file):

```
      - name: Build Windows Detours DLL
        run: cargo build --release -p sandbox-windows-detours

      - name: Copy DLL next to rage binary
        run: |
          copy target\release\sandbox_windows_detours.dll target\release\rage_sandbox.dll
        shell: cmd

      - name: Run Windows smoke test
        run: |
          $env:RAGE_CACHE_DIR = "C:\Temp\rage-ci-cache"
          .\target\release\rage.exe run build . 2>&1 | Select-String "workspace|FAILED|cached|restored"
        shell: pwsh
```

**Step 4a.2: Insert a new step between "Copy DLL next to rage binary" and "Run Windows smoke test"**

Use `edit_file` (or your editor) to insert, immediately after the `Copy DLL next to rage binary` step's `shell: cmd` line, this new step (keep the existing two-space leading indentation that puts step keys at column 6):

```yaml
      - name: Run Windows sandbox integration test
        run: |
          cargo test -p sandbox --test windows_integration -- --include-ignored --nocapture
        env:
          RAGE_SANDBOX_DLL_PATH: ${{ github.workspace }}\target\release\rage_sandbox.dll
        shell: pwsh
```

Notes on what each line does:
- `--include-ignored` opts the `#[ignore]`-by-default test in.
- `--nocapture` prints assertion failure context (the `reads`/`writes` debug dump) to the CI log — invaluable when the hook misbehaves.
- `RAGE_SANDBOX_DLL_PATH` uses the absolute `${{ github.workspace }}` path so cwd doesn't matter.
- Backslashes in the env-var value are valid YAML — no escaping needed inside `${{ }}` expressions.
- `shell: pwsh` matches the smoke-test step.

**Step 4a.3: Re-read the file to confirm indentation is correct**

```bash
sed -n '195,225p' .github/workflows/ci.yml
```

You should see your new step nested inside `steps:` at the same indentation as the surrounding `- name:` entries. If anything looks off (tabs, wrong column), fix before continuing.

**Step 4a.4: Validate YAML structure as best we can locally**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo "YAML OK"
```

Expected: `YAML OK`. If it errors, fix the indentation and re-check.

**Step 4a.5: Confirm the Rust workspace still compiles**

```bash
cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished ...`.

**Step 4a.6: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(sandbox-smoke-windows): run windows_integration test with DLL"
```

---

## Task 4b: Add the exit-code assertion to the smoke step

**Files:**
- Modify: `.github/workflows/ci.yml` (the `Run Windows smoke test` step inside `sandbox-smoke-windows`)

**Background:**

The current smoke step pipes `rage.exe` through `Select-String`. The pipe means the step's exit code is `Select-String`'s, not `rage.exe`'s. If `rage.exe` crashes with exit 1 the step still passes. We capture the output, run the filter, then explicitly check `$LASTEXITCODE`.

PowerShell gotcha: `$LASTEXITCODE` is set by **native** commands (like `rage.exe`), not by cmdlets (`Select-String`). It is set by the most recent native invocation in the current scope. Capturing into `$output = ...` runs the pipeline, so `$LASTEXITCODE` after that line reflects `rage.exe`. Do **not** put the `Select-String` call between the pipeline and the check on the same line — it's fine on the next line because `Select-String` is a cmdlet and does not touch `$LASTEXITCODE`.

**Step 4b.1: Replace the `Run Windows smoke test` step body**

Find this step in `.github/workflows/ci.yml`:

```yaml
      - name: Run Windows smoke test
        run: |
          $env:RAGE_CACHE_DIR = "C:\Temp\rage-ci-cache"
          .\target\release\rage.exe run build . 2>&1 | Select-String "workspace|FAILED|cached|restored"
        shell: pwsh
```

Replace its `run: |` block (keep the step name and `shell: pwsh`) with:

```yaml
      - name: Run Windows smoke test
        run: |
          $env:RAGE_CACHE_DIR = "C:\Temp\rage-ci-cache"
          $output = .\target\release\rage.exe run build . 2>&1
          $rageExit = $LASTEXITCODE
          $output | Select-String "workspace|FAILED|cached|restored"
          if ($rageExit -ne 0) {
            Write-Host "rage.exe exited with $rageExit"
            exit $rageExit
          }
        shell: pwsh
```

Why we cache `$LASTEXITCODE` into `$rageExit` immediately: `$output | Select-String ...` is itself a pipeline. While `Select-String` shouldn't reset `$LASTEXITCODE`, snapshotting it on the very next line is bulletproof and reads more clearly to a reviewer.

**Step 4b.2: Validate YAML again**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo "YAML OK"
```

Expected: `YAML OK`.

**Step 4b.3: Confirm the Rust workspace still compiles**

```bash
cargo check --workspace 2>&1 | tail -10
```

Expected: `Finished ...`. (No Rust files changed, but a sanity check costs nothing.)

**Step 4b.4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(sandbox-smoke-windows): assert rage.exe exit code in smoke step"
```

---

## Task 5: Final verification + handoff

**Files:** none modified.

**Step 5.1: Run the full workspace test suite locally (macOS)**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: every test binary reports `test result: ok.` The new `windows_integration` binary reports `running 0 tests` on macOS — that is correct.

**Step 5.2: Confirm the four expected commits are on the branch**

```bash
git log --oneline -n 4
```

Expected (top-to-bottom, newest first):

```
xxxxxxx ci(sandbox-smoke-windows): assert rage.exe exit code in smoke step
xxxxxxx ci(sandbox-smoke-windows): run windows_integration test with DLL
xxxxxxx build(sandbox): add tokio dev-dependency for integration tests
xxxxxxx test(sandbox): add Windows DLL-injection integration test skeleton (cfg-gated)
```

**Step 5.3: Confirm the working tree is clean**

```bash
git status
```

Expected: `nothing to commit, working tree clean` (apart from any pre-existing unrelated modifications you started with — see the heads-up at the top).

**Step 5.4: Confirm CI YAML one more time**

```bash
python3 -c "import yaml; cfg = yaml.safe_load(open('.github/workflows/ci.yml')); \
  job = cfg['jobs']['sandbox-smoke-windows']; \
  step_names = [s.get('name', '<unnamed>') for s in job['steps']]; \
  print('\n'.join(step_names))"
```

Expected output (order matters — the integration-test step must come **after** "Copy DLL next to rage binary" and **before** "Run Windows smoke test"):

```
<uses entries appear as <unnamed>>
Download rage binary
Install Node.js
Install Rust toolchain (for DLL build)
Cache Rust dependencies
Build Windows Detours DLL
Copy DLL next to rage binary
Run Windows sandbox integration test
Run Windows smoke test
```

(There may be one `<unnamed>` for the `actions/checkout@v4` step at the top — that's fine.)

**Step 5.5: Push and watch the Windows CI run**

```bash
git push -u origin HEAD
```

Open the GitHub Actions run for the `sandbox-smoke-windows` job and watch:

1. **Build Windows Detours DLL** — must succeed.
2. **Copy DLL next to rage binary** — must succeed.
3. **Run Windows sandbox integration test** — must report `1 passed; 0 failed`.
4. **Run Windows smoke test** — should pass; if `rage.exe` ever exits non-zero in future runs, the step will now fail loudly instead of silently.

**Possible failures and what they mean:**

| Symptom | Likely cause | Action |
|---|---|---|
| Integration test: `sandbox DLL not found at ...` | `RAGE_SANDBOX_DLL_PATH` env var not visible to the test process or the path is wrong. | Print `dir target\release\rage_sandbox.dll` in CI to confirm the file exists; check the env block of the new step. |
| Integration test: panic `expected at least one read under C:\\Windows\\` with empty `reads` | DLL injected but the `CreateFileW` hook didn't fire — possibly a regression in the DLL crate. | This is exactly what the test was designed to catch — file an issue against the DLL crate. **Do not** weaken the assertion. |
| Integration test: panic `expected at least one read under C:\\Windows\\` with non-empty `reads` that don't match | Hook is firing but path normalization is producing something unexpected (e.g., `\\?\C:\Windows\...`). | Capture the reads from the `--nocapture` log; widen the prefix check in the test only after you've understood the actual format. |
| Smoke step suddenly red on `main` | `$LASTEXITCODE` is now wired up; `rage.exe` is genuinely failing on something the old step ignored. | Investigate the `rage.exe` failure. **Do not** revert the exit-code check — that's the whole point of this hardening. |

**Step 5.6: Stop here**

This plan does not produce a final consolidating commit — each task already commits its slice. Do not squash unless the merge workflow explicitly asks for it.

---

## Done criteria

All of the following must be true:

- [ ] `crates/sandbox/tests/windows_integration.rs` exists, starts with `#![cfg(target_os = "windows")]`, and contains exactly one `#[ignore]`'d `#[tokio::test]`.
- [ ] `crates/sandbox/Cargo.toml` declares `tokio` under `[dev-dependencies]` with `rt`, `macros`, `rt-multi-thread`.
- [ ] `cargo test --workspace` on macOS still passes; new integration binary reports `running 0 tests`.
- [ ] `.github/workflows/ci.yml` `sandbox-smoke-windows` job has a `Run Windows sandbox integration test` step with `RAGE_SANDBOX_DLL_PATH` set to `${{ github.workspace }}\target\release\rage_sandbox.dll`, ordered after `Copy DLL next to rage binary`.
- [ ] The `Run Windows smoke test` step captures `$LASTEXITCODE` from `rage.exe` and exits non-zero if it's non-zero.
- [ ] Windows CI run on the pushed branch is green, with the integration test reporting `1 passed`.

---

## Out of scope (do not do these)

- Changing the DLL crate (`crates/sandbox-windows-detours`) in any way.
- Adding new hook coverage (e.g., `FindFirstFileW`).
- Touching the unix sandbox or `crates/sandbox/tests/symlink_poc.rs`.
- Changing `find_dll_path()` or any other function in `crates/sandbox/src/windows.rs`.
- Rewriting the smoke step to use a different command than `rage.exe run build .`.
- "While I'm here" cleanups in `ci.yml` outside the `sandbox-smoke-windows` job.

If you feel the urge to do any of the above, write it down as a follow-up issue and **do not** put it in this PR.
