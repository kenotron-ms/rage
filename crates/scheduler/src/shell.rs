//! Cross-platform shell dispatch helpers.
//!
//! Production code should call [`command`] (async) or [`std_command`] (sync)
//! instead of `Command::new("sh")` directly.  These helpers select `sh -c` on
//! Unix and `cmd /c` on Windows so that callers do not need to branch on the
//! target platform.

/// Build a `tokio::process::Command` that runs `cmd` through the platform shell.
/// On Unix this is `sh -c <cmd>` placed in its own process group (`process_group(0)`)
/// with `kill_on_drop(true)`. On Windows this is `cmd /c <cmd>` with `kill_on_drop(true)`.
#[cfg(unix)]
pub fn command(cmd: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    c.process_group(0);
    c.kill_on_drop(true);
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
    c.kill_on_drop(true);
    c
}

/// Drop guard that ensures a spawned [`tokio::process::Child`] and its entire
/// process group (on Unix) or process tree (on Windows) are terminated if the
/// owning future is dropped/cancelled before [`ProcessGroupGuard::wait`] completes.
pub struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    /// Create a guard for a newly spawned `child`.
    pub fn new(child: &tokio::process::Child) -> Self {
        Self { pid: child.id() }
    }

    /// Disarm the guard after the child has exited normally.
    pub fn disarm(&mut self) {
        self.pid = None;
    }

    /// Explicitly terminate the process group / tree right now.
    pub fn kill_tree_sync(&mut self) {
        if let Some(pid) = self.pid.take() {
            kill_pid_tree_sync(pid);
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_tree_sync();
    }
}

fn kill_pid_tree_sync(pid: u32) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System};

    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, false, ProcessRefreshKind::nothing());

    let root = Pid::from_u32(pid);
    // Collect all descendants in the process tree rooted at `pid` so we can
    // terminate children and grandchildren directly via the OS API (`sysinfo`
    // uses `kill(pid, SIGKILL)` on Unix and `TerminateProcess` on Windows)
    // without relying on PATH or external `kill`/`taskkill` binaries.
    let mut to_kill = vec![root];
    let mut idx = 0;
    while idx < to_kill.len() {
        let current = to_kill[idx];
        idx += 1;
        for (&proc_pid, proc_info) in sys.processes() {
            if proc_info.parent() == Some(current) && !to_kill.contains(&proc_pid) {
                to_kill.push(proc_pid);
            }
        }
    }

    // Kill leaves before parents so parents cannot respawn children during teardown.
    for &target_pid in to_kill.iter().rev() {
        if let Some(proc_info) = sys.process(target_pid) {
            let _ = proc_info
                .kill_with(Signal::Kill)
                .unwrap_or_else(|| proc_info.kill());
        }
    }

    // On Unix, also signal the process group (`-pid`) via `/bin/kill` as a
    // belt-and-suspenders sweep for any child that forked after the snapshot.
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", &format!("-{pid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Spawn `cmd`, wait for it to finish, and ensure the entire process group /
/// tree is terminated if the awaiting future is cancelled or dropped.
pub async fn spawn_and_wait_status(
    mut cmd: tokio::process::Command,
) -> std::io::Result<std::process::ExitStatus> {
    let mut child = cmd.spawn()?;
    let mut guard = ProcessGroupGuard::new(&child);
    let res = child.wait().await;
    guard.disarm();
    res
}

/// Terminate the process group (on Unix) and reap `child`.
pub async fn kill_process_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        kill_pid_tree_sync(pid);
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
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
