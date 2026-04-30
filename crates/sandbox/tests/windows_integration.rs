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
