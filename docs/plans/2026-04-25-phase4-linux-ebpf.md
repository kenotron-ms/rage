# Phase 4 — Linux eBPF Sandbox

**Status:** Planned  
**Branch:** `feat/phase4-linux-ebpf`  
**New crates:** `sandbox-linux-ebpf` (user-space), `sandbox-linux-ebpf-prog` (kernel, NOT in workspace)  
**Modified crates:** `sandbox`

---

## Problem

`crates/sandbox/src/unsupported.rs` is a no-op stub on Linux. No PathSet is ever
recorded. On Linux, TwoPhaseCache falls back to declared globs only (no SF-level
cache invalidation from actual file reads).

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  sandbox crate                                              │
│    lib.rs: #[cfg(target_os = "macos")] use macos::...       │
│            #[cfg(target_os = "linux")] use linux::...       │
│    linux.rs: calls sandbox_linux_ebpf::run_sandboxed        │
└─────────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────┐
│  sandbox-linux-ebpf crate (Linux-only, user-space)          │
│    - Loads eBPF program from embedded bytes                 │
│    - Attaches tracepoints: sys_enter_openat, open, stat,    │
│      rename, unlink, mkdir + sched_process_fork             │
│    - Tracks watched PID subtree via BPF HashMap              │
│    - Collects AccessEvents via RingBuf                      │
│    - Spawns task process, waits, drains ring buffer         │
│    - Returns PathSet (reads + writes)                       │
└─────────────────────────────────────────────────────────────┘
         │  (embedded eBPF bytecode)
         ▼
┌─────────────────────────────────────────────────────────────┐
│  sandbox-linux-ebpf-prog (eBPF kernel program)              │
│    - Compiled to bpfel-unknown-none target                  │
│    - tracepoint/sys_enter_openat: emit AccessEvent for      │
│      watched PIDs                                           │
│    - tracepoint/sched_process_fork: add child to watched    │
│      set when parent is watched                             │
└─────────────────────────────────────────────────────────────┘
```

---

## File structure

```
crates/
  sandbox/
    src/
      lib.rs            ← add #[cfg(target_os = "linux")] use linux::run_sandboxed
      linux.rs          ← NEW: bridges to sandbox-linux-ebpf
      macos.rs          ← unchanged
      unsupported.rs    ← removed / replaced

  sandbox-linux-ebpf/
    Cargo.toml          ← aya, aya-utils, anyhow, tokio
    build.rs            ← compiles sandbox-linux-ebpf-prog to bpf target
    src/
      lib.rs            ← pub async fn run_sandboxed(...)

  sandbox-linux-ebpf-prog/
    # NOT in main Cargo.toml workspace; built by sandbox-linux-ebpf/build.rs
    .cargo/config.toml  ← [build] target = "bpfel-unknown-none"
    Cargo.toml          ← aya-ebpf, aya-log-ebpf
    src/
      main.rs           ← eBPF kernel program
```

---

## eBPF kernel program (`sandbox-linux-ebpf-prog/src/main.rs`)

```rust
#![no_std]
#![no_main]

use aya_ebpf::{
    macros::{map, tracepoint},
    maps::{HashMap, RingBuf},
    programs::{TracePointContext},
    helpers::{bpf_get_current_pid_tgid, bpf_probe_read_user_str_bytes},
    EbpfContext,
};
use aya_log_ebpf::info;

// Map: watched PID → 1u8 sentinel
#[map]
static mut WATCHED_PIDS: HashMap<u32, u8> = HashMap::with_max_entries(4096, 0);

// Ring buffer for events (8 MiB)
#[map]
static mut EVENTS: RingBuf = RingBuf::with_byte_size(8 * 1024 * 1024, 0);

// AccessEvent layout (written into ring buffer)
#[repr(C)]
struct AccessEvent {
    pid: u32,
    op: u8,      // 0 = read, 1 = write
    path_len: u16,
    path: [u8; 4096],
}

const OP_READ: u8 = 0;
const OP_WRITE: u8 = 1;

// openat(2) sys_enter args
#[repr(C)]
struct SysEnterOpenatArgs {
    _unused: [u64; 2],  // common fields
    dfd:     i64,
    filename: *const u8,
    flags:   i64,
    mode:    u64,
}

fn is_write_flags(flags: i64) -> bool {
    const O_WRONLY: i64 = 1;
    const O_RDWR: i64 = 2;
    const O_CREAT: i64 = 64; // 0100 octal
    flags & (O_WRONLY | O_RDWR | O_CREAT) != 0
}

fn emit_event(ctx: &TracePointContext, path_ptr: *const u8, op: u8) -> Result<(), i64> {
    let pid = bpf_get_current_pid_tgid() as u32;
    
    // Only track watched PIDs
    if unsafe { WATCHED_PIDS.get(&pid).is_none() } {
        return Ok(());
    }
    
    let mut entry = match unsafe { EVENTS.reserve::<AccessEvent>(0) } {
        Some(e) => e,
        None => return Ok(()),  // ring buffer full — drop event, continue
    };
    
    let evt = entry.as_mut_ptr();
    unsafe {
        (*evt).pid = pid;
        (*evt).op = op;
    }
    
    // Read path string from user space
    let path_buf = unsafe { core::slice::from_raw_parts_mut((*evt).path.as_mut_ptr(), 4096) };
    let len = match bpf_probe_read_user_str_bytes(path_ptr, path_buf) {
        Ok(s) => s.len() as u16,
        Err(_) => {
            entry.discard(0);
            return Ok(());
        }
    };
    unsafe { (*evt).path_len = len; }
    entry.submit(0);
    Ok(())
}

#[tracepoint(category = "syscalls", name = "sys_enter_openat")]
pub fn sys_enter_openat(ctx: TracePointContext) -> u32 {
    let args = unsafe {
        &*(ctx.as_ptr() as *const SysEnterOpenatArgs)
    };
    let op = if is_write_flags(args.flags) { OP_WRITE } else { OP_READ };
    let _ = emit_event(&ctx, args.filename, op);
    0
}

#[tracepoint(category = "syscalls", name = "sys_enter_open")]
pub fn sys_enter_open(ctx: TracePointContext) -> u32 {
    // Similar to openat but with 3 args instead of 4
    // For simplicity, treat as read (conservative)
    0  // TODO: implement
}

// Track child processes
#[repr(C)]
struct SchedForkArgs {
    _pad: [u64; 2],
    parent_pid: u32,
    child_pid: u32,
}

#[tracepoint(category = "sched", name = "sched_process_fork")]
pub fn sched_process_fork(ctx: TracePointContext) -> u32 {
    let args = unsafe { &*(ctx.as_ptr() as *const SchedForkArgs) };
    
    // If parent is watched, add child to watched set
    if unsafe { WATCHED_PIDS.get(&args.parent_pid).is_some() } {
        unsafe { let _ = WATCHED_PIDS.insert(&args.child_pid, &1, 0); }
    }
    0
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
```

---

## User-space loader (`sandbox-linux-ebpf/src/lib.rs`)

```rust
use aya::{
    include_bytes_aligned,
    maps::{HashMap, AsyncRingBuf},
    programs::TracePoint,
    Ebpf, EbpfLoader,
};
use std::path::Path;
use tokio::process::Command;
use tokio::io::AsyncReadExt;
use sandbox::event::{AccessEvent, PathSet, RunResult};

const EBPF_PROG: &[u8] = include_bytes_aligned!(
    concat!(env!("OUT_DIR"), "/sandbox-linux-ebpf-prog")
);

pub async fn run_sandboxed(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
) -> anyhow::Result<RunResult> {
    let mut bpf = EbpfLoader::new()
        .load(EBPF_PROG)
        .map_err(|e| anyhow::anyhow!("eBPF load failed: {e}"))?;
    
    // Attach tracepoints
    for (cat, name) in &[
        ("syscalls", "sys_enter_openat"),
        ("sched", "sched_process_fork"),
    ] {
        let prog: &mut TracePoint = bpf.program_mut(name)
            .ok_or_else(|| anyhow::anyhow!("program {name} not found"))?
            .try_into()?;
        prog.load()?;
        prog.attach(cat, name)?;
    }
    
    // Spawn task
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn failed: {e}"))?;
    
    // Track PID
    let pid = child.id().ok_or_else(|| anyhow::anyhow!("child has no PID"))?;
    {
        let mut watched: HashMap<_, u32, u8> = 
            HashMap::try_from(bpf.map_mut("WATCHED_PIDS")?)?;
        watched.insert(pid, 1, 0)?;
    }
    
    // Collect events while process runs
    let mut ring_buf: AsyncRingBuf<_> = AsyncRingBuf::try_from(bpf.map_mut("EVENTS")?)?;
    let mut events = Vec::new();
    
    let exit_code = tokio::select! {
        status = child.wait() => {
            status?.code().unwrap_or(-1)
        }
    };
    
    // Drain remaining events
    let mut buf = bytes::BytesMut::with_capacity(4096);
    while let Ok(n) = ring_buf.read_buf(&mut buf).await {
        if n == 0 { break; }
        // Parse AccessEvent structs from buffer
        events.extend(parse_events(&buf));
        buf.clear();
    }
    
    let path_set = build_path_set(&events);
    Ok(RunResult { exit_code, path_set })
}
```

---

## Build script (`sandbox-linux-ebpf/build.rs`)

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Only build eBPF program on Linux
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ebpf_prog_dir = manifest_dir.parent().unwrap().join("sandbox-linux-ebpf-prog");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    
    println!("cargo:rerun-if-changed={}", ebpf_prog_dir.display());
    
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "-Z", "build-std=core",
            "--target", "bpfel-unknown-none",
        ])
        .current_dir(&ebpf_prog_dir)
        .env("CARGO_TARGET_DIR", &out_dir)
        .status()
        .expect("cargo build of eBPF program failed");
    
    assert!(status.success());
    
    // Copy object file to OUT_DIR
    let obj = out_dir
        .join("bpfel-unknown-none/release/sandbox-linux-ebpf-prog");
    let dst = out_dir.join("sandbox-linux-ebpf-prog");
    std::fs::copy(obj, dst).expect("copy eBPF object");
}
```

---

## Docker test setup (`docker/test-linux-ebpf.sh`)

```bash
#!/usr/bin/env bash
# Run Linux eBPF sandbox tests inside a container with CAP_BPF
set -euo pipefail

docker run --rm \
  --cap-add=CAP_BPF \
  --cap-add=SYS_ADMIN \
  -v "$(pwd):/rage" \
  -w /rage \
  rust:1.91 \
  cargo test -p sandbox-linux-ebpf -- --nocapture
```

---

## Acceptance criteria

1. On Linux (Docker), `cargo test -p sandbox-linux-ebpf` passes
2. A task running under SandboxMode::Observed on Linux produces a non-empty PathSet
3. PathSet is consistent with what `strace -e trace=file` would report
4. macOS builds are unaffected — macOS still uses the DYLD sandbox
5. All existing tests still pass on macOS
