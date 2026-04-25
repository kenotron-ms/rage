//! eBPF kernel program for the rage Linux sandbox.
//!
//! Intercepts file-system syscalls for watched PIDs and emits
//! AccessEvent records to a ring buffer.
//!
//! Compiled to the `bpfel-unknown-none` target by
//! `sandbox-linux-ebpf/build.rs` — NOT part of the main workspace.

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::{bpf_get_current_pid_tgid, bpf_probe_read_user_str_bytes},
    macros::{map, tracepoint},
    maps::{HashMap, RingBuf},
    programs::TracePointContext,
    EbpfContext,
};

// ─── Maps ─────────────────────────────────────────────────────────────────

/// PIDs currently being traced. Value is 1 (sentinel).
#[map]
static mut WATCHED_PIDS: HashMap<u32, u8> = HashMap::with_max_entries(4096, 0);

/// Ring buffer that carries AccessEvent records to user space (8 MiB).
#[map]
static mut EVENTS: RingBuf = RingBuf::with_byte_size(8 * 1024 * 1024, 0);

// ─── AccessEvent layout ───────────────────────────────────────────────────

/// Written verbatim into the ring buffer.  User space parses this by casting
/// the raw bytes.
#[repr(C)]
pub struct AccessEvent {
    pub pid: u32,
    pub op: u8,       // OP_READ = 0, OP_WRITE = 1
    pub _pad: [u8; 3],
    pub path_len: u32,
    pub path: [u8; 4096],
}

const OP_READ: u8 = 0;
const OP_WRITE: u8 = 1;

// ─── Syscall argument layouts ─────────────────────────────────────────────

/// Layout of the `sys_enter_openat` tracepoint args.
/// `common_*` fields are the 2-field tracepoint header (16 bytes).
#[repr(C)]
struct OpenatArgs {
    _common: [u64; 2],
    dfd: i64,
    filename: u64, // user-space pointer to NUL-terminated path
    flags: i64,
    mode: u64,
}

/// Layout of the `sched_process_fork` tracepoint args.
#[repr(C)]
struct ForkArgs {
    _common: [u64; 2],
    parent_comm: [u8; 16],
    parent_pid: u32,
    child_comm: [u8; 16],
    child_pid: u32,
}

// ─── Helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
fn current_tid() -> u32 {
    (bpf_get_current_pid_tgid() >> 32) as u32
}

fn is_write_flags(flags: i64) -> bool {
    const O_WRONLY: i64 = 1;
    const O_RDWR: i64 = 2;
    const O_CREAT: i64 = 64; // 0100 octal
    const O_TRUNC: i64 = 512; // 01000 octal
    flags & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC) != 0
}

/// Try to emit an AccessEvent for `path_ptr` with the given `op`.
/// Returns `Ok(())` on success or if the PID is not watched.
fn try_emit(path_ptr: u64, op: u8) -> Result<(), u32> {
    let pid = current_tid();

    // Only emit for watched PIDs.
    // SAFETY: aya-ebpf map helpers are safe when used correctly.
    if unsafe { WATCHED_PIDS.get(&pid).is_none() } {
        return Ok(());
    }

    let mut entry = match unsafe { EVENTS.reserve::<AccessEvent>(0) } {
        Some(e) => e,
        None => return Ok(()), // ring buffer full — drop, continue
    };

    unsafe {
        (*entry.as_mut_ptr()).pid = pid;
        (*entry.as_mut_ptr()).op = op;
    }

    let path_slice = unsafe {
        core::slice::from_raw_parts_mut((*entry.as_mut_ptr()).path.as_mut_ptr(), 4096)
    };

    match bpf_probe_read_user_str_bytes(path_ptr as *const u8, path_slice) {
        Ok(s) => {
            let len = s.len() as u32;
            unsafe { (*entry.as_mut_ptr()).path_len = len; }
            entry.submit(0);
        }
        Err(_) => {
            entry.discard(0);
        }
    }
    Ok(())
}

// ─── Tracepoint probes ────────────────────────────────────────────────────

/// Intercept `openat(2)`.
#[tracepoint]
pub fn sys_enter_openat(ctx: TracePointContext) -> u32 {
    let args = unsafe { &*(ctx.as_ptr() as *const OpenatArgs) };
    let op = if is_write_flags(args.flags) { OP_WRITE } else { OP_READ };
    let _ = try_emit(args.filename, op);
    0
}

/// Track child processes: when a watched PID forks, add the child.
#[tracepoint]
pub fn sched_process_fork(ctx: TracePointContext) -> u32 {
    let args = unsafe { &*(ctx.as_ptr() as *const ForkArgs) };
    let parent = args.parent_pid;
    let child = args.child_pid;

    // SAFETY: checking + inserting into BPF map.
    if unsafe { WATCHED_PIDS.get(&parent).is_some() } {
        unsafe {
            let _ = WATCHED_PIDS.insert(&child, &1u8, 0);
        }
    }
    0
}

/// Track process exit: remove PID from watched set to avoid stale entries.
#[tracepoint]
pub fn sched_process_exit(ctx: TracePointContext) -> u32 {
    let pid = current_tid();
    unsafe {
        let _ = WATCHED_PIDS.remove(&pid);
    }
    0
}

// ─── Panic handler ────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
