//! Cross-platform shell dispatch helpers.
//!
//! Production code should call [`command`] (async) or [`std_command`] (sync)
//! instead of `Command::new("sh")` directly.  These helpers select `sh -c` on
//! Unix and `cmd /c` on Windows so that callers do not need to branch on the
//! target platform.

/// Build a `tokio::process::Command` that runs `cmd` through the platform shell.
/// On Unix this is `sh -c <cmd>`. On Windows this is `cmd /c <cmd>`.
#[cfg(unix)]
pub fn command(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

/// Build a `tokio::process::Command` that runs `cmd` through the platform shell.
/// On Unix this is `sh -c <cmd>`. On Windows this is `cmd /c <cmd>`.
///
/// On Windows, we use `raw_arg` to pass the command string verbatim to cmd.exe
/// without Rust's `CommandLineToArgvW`-style escaping.  cmd.exe and
/// `CommandLineToArgvW` use different quoting rules; using `.arg()` would
/// double-escape inner quotes (turning `"script"` into `\"script\"`), which
/// causes cmd.exe to mis-parse `&&`-chained scripts like npm build commands.
#[cfg(windows)]
pub fn command(cmd: &str) -> tokio::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = tokio::process::Command::new("cmd");
    c.arg("/c");
    c.as_std_mut().raw_arg(cmd);
    c
}

/// Build a `std::process::Command` that runs `cmd` through the platform shell.
/// Use this from synchronous code paths where a Tokio runtime is not available
/// (for example, the postinstall cache). On Unix this is `sh -c <cmd>`; on
/// Windows this is `cmd /c <cmd>`.
#[cfg(unix)]
pub fn std_command(cmd: &str) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

/// Build a `std::process::Command` that runs `cmd` through the platform shell.
/// Use this from synchronous code paths where a Tokio runtime is not available
/// (for example, the postinstall cache). On Unix this is `sh -c <cmd>`; on
/// Windows this is `cmd /c <cmd>`.
///
/// On Windows, we use `raw_arg` to pass the command string verbatim to cmd.exe
/// (see [`command`] for the full rationale).
#[cfg(windows)]
pub fn std_command(cmd: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new("cmd");
    c.arg("/c");
    c.raw_arg(cmd);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn command_uses_sh_on_unix() {
        let output = command("echo hello").output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn command_uses_cmd_on_windows() {
        let output = command("echo hello").output().await.unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    #[cfg(unix)]
    fn std_command_uses_sh_on_unix() {
        let output = std_command("echo hello").output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    #[cfg(windows)]
    fn std_command_uses_cmd_on_windows() {
        let output = std_command("echo hello").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }
}
