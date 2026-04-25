//! Linux eBPF sandbox runner.
//!
//! Loads the pre-compiled eBPF program (embedded via `include_bytes_aligned!`
//! from `OUT_DIR`), attaches tracepoints, spawns the task process, and
//! collects all file-system access events from the BPF ring buffer.

use anyhow::{Context, Result};
use aya::{
    maps::{AsyncRingBuf, HashMap},
    programs::TracePoint,
    Ebpf,
};
use sandbox::event::{AccessEvent, PathSet, RunResult};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Embedded eBPF program object file (compiled by build.rs on Linux).
/// On non-Linux hosts build.rs does nothing, so this file doesn't exist
/// at that path — the entire module is `#[cfg(target_os = "linux")]`.
static EBPF_PROG: &[u8] =
    aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/sandbox-linux-ebpf-prog"));

/// Size of an `AccessEvent` in the ring buffer (path field is 4096 bytes).
const EVENT_SIZE: usize = 4 + 1 + 3 + 4 + 4096; // pid u32 + op u8 + pad + path_len u32 + path[4096]

pub async fn run_sandboxed_linux(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<RunResult> {
    // ── Load eBPF program ──────────────────────────────────────────────────

    let mut bpf = Ebpf::load(EBPF_PROG).context("loading eBPF program")?;

    // ── Attach tracepoints ─────────────────────────────────────────────────

    for (category, name) in &[
        ("syscalls", "sys_enter_openat"),
        ("sched", "sched_process_fork"),
        ("sched", "sched_process_exit"),
    ] {
        let prog: &mut TracePoint = bpf
            .program_mut(name)
            .with_context(|| format!("eBPF program {name} not found"))?
            .try_into()
            .context("program is not a TracePoint")?;
        prog.load()
            .with_context(|| format!("loading tracepoint {name}"))?;
        prog.attach(category, name)
            .with_context(|| format!("attaching tracepoint {name}"))?;
    }

    // ── Spawn task process ─────────────────────────────────────────────────

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .spawn()
        .context("spawning sandboxed process")?;

    // ── Add PID to watched set ─────────────────────────────────────────────

    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("child has no PID (already exited?)"))?;

    {
        let mut watched: HashMap<_, u32, u8> = HashMap::try_from(
            bpf.map_mut("WATCHED_PIDS")
                .context("WATCHED_PIDS map not found")?,
        )
        .context("casting WATCHED_PIDS")?;
        watched
            .insert(pid, 1, 0)
            .context("inserting PID into WATCHED_PIDS")?;
    }

    // ── Collect events while process runs ─────────────────────────────────

    let ring = bpf.map_mut("EVENTS").context("EVENTS map not found")?;
    let mut ring_buf: AsyncRingBuf<_> =
        AsyncRingBuf::try_from(ring).context("casting EVENTS ring buffer")?;

    let mut reads: BTreeSet<PathBuf> = BTreeSet::new();
    let mut writes: BTreeSet<PathBuf> = BTreeSet::new();

    // Wait for exit while draining events concurrently
    let exit_code = tokio::select! {
        status = child.wait() => {
            status.context("waiting for child process")?.code().unwrap_or(-1)
        }
    };

    // Drain remaining events (give kernel a moment to flush)
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    drain_ring_buf(&mut ring_buf, &mut reads, &mut writes).await;

    let path_set = PathSet {
        reads: reads.into_iter().collect(),
        writes: writes.into_iter().collect(),
    };

    Ok(RunResult {
        exit_code,
        path_set,
    })
}

/// Drain all pending events from the ring buffer.
async fn drain_ring_buf(
    ring: &mut AsyncRingBuf<&mut aya::maps::MapData>,
    reads: &mut BTreeSet<PathBuf>,
    writes: &mut BTreeSet<PathBuf>,
) {
    let mut buf = [0u8; EVENT_SIZE];

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(10), ring.readable()).await {
            Err(_) => break, // timeout — no more events pending
            Ok(guard) => {
                let data = guard.data();
                if data.is_empty() {
                    break;
                }
                if let Some(event) = parse_event(data) {
                    // path is relative to '/' unless absolute — normalize
                    let path = std::str::from_utf8(event.path)
                        .ok()
                        .map(|s| s.trim_end_matches('\0'))
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from);
                    if let Some(p) = path {
                        if event.op == 0 {
                            reads.insert(p);
                        } else {
                            writes.insert(p);
                        }
                    }
                }
                // consume the data
                let _ = buf;
            }
        }
    }
}

/// Parse a single `AccessEvent` from raw ring buffer bytes.
fn parse_event(data: &[u8]) -> Option<ParsedEvent<'_>> {
    if data.len() < 4 + 1 + 3 + 4 {
        return None;
    }
    let pid = u32::from_ne_bytes(data[0..4].try_into().ok()?);
    let op = data[4];
    // 3 bytes padding at 5..8
    let path_len = u32::from_ne_bytes(data[8..12].try_into().ok()?) as usize;
    let path_start = 12;
    let path_end = path_start
        + path_len
            .min(4096)
            .min(data.len().saturating_sub(path_start));
    if path_end > data.len() {
        return None;
    }
    Some(ParsedEvent {
        pid,
        op,
        path: &data[path_start..path_end],
    })
}

struct ParsedEvent<'a> {
    pid: u32,
    op: u8,
    path: &'a [u8],
}
