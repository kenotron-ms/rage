//! Non-Linux stub — returns an immediate error.
use anyhow::bail;
use sandbox::event::RunResult;
use std::path::Path;

pub async fn run_sandboxed_stub(
    _cmd: &str,
    _cwd: &Path,
    _env: &[(String, String)],
) -> anyhow::Result<RunResult> {
    bail!("eBPF sandbox is only supported on Linux")
}
