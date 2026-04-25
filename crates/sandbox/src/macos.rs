use anyhow::bail;

    use crate::event::RunResult;

    /// macOS-specific sandbox runner (not yet implemented).
    pub async fn run_sandboxed(
        _cmd: &str,
        _cwd: &str,
        _env: &[(&str, &str)],
    ) -> anyhow::Result<RunResult> {
        bail!("not yet implemented")
    }
    