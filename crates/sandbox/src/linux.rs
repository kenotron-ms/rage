//! Linux eBPF sandbox bridge.
//!
//! Delegates to `sandbox-linux-ebpf` which loads the compiled eBPF program,
//! attaches tracepoints, and collects file-system access events.
//! Converts the `EbpfRunResult` to the canonical `RunResult`.

use anyhow::Result;
use std::path::Path;

use crate::event::{PathSet, RunResult};

/// Run `cmd` inside the eBPF sandbox on Linux.
pub async fn run_sandboxed(cmd: &str, cwd: &Path, env: &[(String, String)]) -> Result<RunResult> {
    let r = sandbox_linux_ebpf::run_sandboxed(cmd, cwd, env).await?;
    Ok(RunResult {
        exit_code: r.exit_code,
        path_set: PathSet {
            reads: r.reads,
            writes: r.writes,
        },
    })
}
