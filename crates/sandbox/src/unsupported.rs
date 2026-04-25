use anyhow::bail;

    use crate::event::RunResult;

    /// Platform stub – returns an error on non-macOS systems.
    pub async fn run_sandboxed(
        _cmd: &str,
        _cwd: &str,
        _env: &[(&str, &str)],
    ) -> anyhow::Result<RunResult> {
        bail!("rage sandbox is only supported on macOS in this phase")
    }
    