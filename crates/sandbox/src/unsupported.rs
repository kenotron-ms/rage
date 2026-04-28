use anyhow::bail;
use std::path::Path;

use crate::event::RunResult;

/// Platform stub — returns an error on unsupported operating systems.
pub async fn run_sandboxed(
    _cmd: &str,
    _cwd: &Path,
    _env: &[(String, String)],
) -> anyhow::Result<RunResult> {
    bail!("rage sandbox is not supported on this platform")
}
