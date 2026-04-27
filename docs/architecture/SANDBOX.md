# File-Access Sandbox

The sandbox is rage's correctness primitive. It records every file a task reads or writes during execution and produces a `Pathset` that drives the strong fingerprint (see [`CACHING.md`](CACHING.md)). It is the difference between a build cache that *trusts* declared inputs and one that *verifies* observed inputs.

The sandbox is not a security boundary. It does not prevent malicious code from escaping. It is a correctness instrument: it observes file access and, optionally, blocks reads outside the declared set in `strict` mode.

## Output

Both backends produce the same Rust value:

```rust
pub struct Pathset {
    pub reads: BTreeSet<PathBuf>,
    pub writes: BTreeSet<PathBuf>,
}
```

`reads` includes every path opened for reading, `stat`'d, or otherwise accessed for content. `writes` includes paths opened for write, renamed-to, unlinked, or created. Symlinks are recorded as the symlink path **and** the resolved target — both contribute to the SF.

## Backends at a glance

| Aspect | macOS | Linux | Windows |
|---|---|---|---|
| Mechanism | DYLD interpose | eBPF tracepoints | Detours inline patching |
| Injection | Library load-time | Kernel attach | Suspended process DLL inject |
| IPC | Unix socket | BPF RingBuf | Named pipe |
| Scope | Per-library calls | PID subtree | Per-function trampoline |
| Kernel mode? | No | Yes (eBPF) | No |
| Crate | `sandbox-macos-dylib` | `sandbox-linux-ebpf` | `sandbox-windows-detours` |
| Status | Implemented | Implemented | Planned |

The three backends produce the same `Pathset` value. The differences are in how each OS exposes a hook point that fires before file content reaches the task.

## macOS — DYLD interpose

`crates/sandbox-macos-dylib/` builds a Mach-O dynamic library containing a `__DATA,__interpose` table. The table maps libc symbols to logging shims. The child process is launched with:

```
DYLD_INSERT_LIBRARIES=/path/to/librage_sandbox.dylib
DYLD_FORCE_FLAT_NAMESPACE=0
RAGE_SANDBOX_FIFO=/tmp/rage/sandbox/<task-id>.fifo
RAGE_SANDBOX_TASK_ID=<task-id>
```

dyld resolves the interpose table at load time. Every call to `open`, `openat`, `stat`, `lstat`, `fstatat`, `read`, `pread`, `write`, `pwrite`, `rename`, `renameat`, `unlink`, `unlinkat`, `mkdir`, `mkdirat`, `readdir`, and `access` from any image — main binary, dylibs, frameworks — goes through the shim.

The shim:

1. Calls the real syscall via `dlsym(RTLD_NEXT, ...)`.
2. Resolves the path argument(s) to absolute (handles `*at`-style fd-relative paths via `fcntl(F_GETPATH)`).
3. Writes a structured event line to the FIFO.
4. Returns the syscall's result unchanged.

Events are framed as length-prefixed CBOR records. The parent (rage scheduler) reads from the FIFO concurrently and accumulates the Pathset until the child exits and EOF is reached.

```c
// Conceptual sketch of an interpose entry:
struct interpose_entry { void *replacement; void *original; };
__attribute__((used)) static const struct interpose_entry
__attribute__((section("__DATA,__interpose")))
interpose_open = { (void*)rage_open, (void*)open };

int rage_open(const char *path, int flags, ...) {
    int result = open(path, flags, /* mode from va_args */);
    rage_log_event(EV_OPEN, path, flags, result);
    return result;
}
```

### Why DYLD interpose, not dtrace / EndpointSecurity

| Approach | Why we rejected it |
|---|---|
| **dtrace** | Requires root or `csrutil disable`, breaks on signed/notarized binaries, not portable across macOS versions, slow tracepoint dispatch. |
| **EndpointSecurity (ES)** | Requires a signed system extension and an Apple-issued entitlement (a paperwork process, not a technical one). Hard to ship in OSS. |
| **fseventsd / FSEvents** | Coalesces events with multi-second latency. Drops events under load. No PID filtering. Useless as a syscall observer. |
| **inotify clones** | Don't exist on macOS in any usable form. |
| **strace-style ptrace** | macOS does not expose `PTRACE_SYSCALL`-equivalent attach for non-debugger processes outside Xcode. SIP blocks general use. |
| **DYLD interpose** | Works on every Mac since 10.7. No root, no entitlements, no SIP exemption. Inherited by `posix_spawn` / `fork+exec` children automatically. The sandbox is a normal dylib. |

The trade-off: interpose is bypassed by static binaries (rare in macOS userland) and by direct `syscall` invocations (rarer still). For build tools — `tsc`, `node`, `cargo`, `python` — every file access goes through libc.

### Code-signing and Hardened Runtime

`DYLD_INSERT_LIBRARIES` is ignored by hardened-runtime binaries unless they have the `com.apple.security.cs.allow-dyld-environment-variables` entitlement. In a developer monorepo this is fine: `node`, `tsc`, `cargo`, etc. don't run with hardened runtime. For binaries that do, rage falls back to running the task with `arch -arch arm64 -e DYLD_INSERT_LIBRARIES=...` via a re-spawn that strips the SIP-protected env var clearing — and if that fails, the sandbox emits a warning and the task runs in `loose` mode for that invocation.

## Linux — eBPF via aya

`crates/sandbox-linux-ebpf/` is the loader; `crates/sandbox-linux-ebpf-prog/` is the kernel-side program. The program is `#![no_std]`, compiles with the BPF target (`bpfel-unknown-none`), and is loaded by aya at runtime.

Tracepoints attached:

- `sys_enter_openat`, `sys_exit_openat`
- `sys_enter_stat`, `sys_enter_newfstatat`, `sys_enter_lstat`
- `sys_enter_read`, `sys_enter_pread64`
- `sys_enter_write`, `sys_enter_pwrite64`
- `sys_enter_rename`, `sys_enter_renameat2`
- `sys_enter_unlink`, `sys_enter_unlinkat`
- `sys_enter_mkdir`, `sys_enter_mkdirat`

Each tracepoint handler:

1. Reads the current PID via `bpf_get_current_pid_tgid()`.
2. Looks up the PID in a `BPF_MAP_TYPE_HASH` keyed by PID (populated by the loader before `execve`).
3. If found, copies the path argument from userspace via `bpf_probe_read_user_str` into a per-CPU buffer.
4. Emits a structured event into a `BPF_MAP_TYPE_RINGBUF`.

The loader (in userspace) reads the ring buffer with the aya `RingBuf` API and accumulates events into the per-task pathset until the child exits.

### Why eBPF, not strace / ptrace / fanotify

| Approach | Why we rejected it |
|---|---|
| **strace / ptrace** | Order-of-magnitude slowdown. ptrace is the wrong tool for production builds; it serializes every syscall through the parent. |
| **fanotify** | Notification-only with significant blind spots (no `stat`, no `read`/`write` content of opened files, only mounts or filesystems). Limited PID filtering. |
| **inotify** | Path-based; misses access patterns; not designed for syscall observation. |
| **LD_PRELOAD interpose** | Would mirror the macOS approach, but on Linux `LD_PRELOAD` is bypassed by static binaries (rust binaries by default), by `setuid` binaries, and by anything using direct `syscall(2)`. |
| **Linux Security Module (LSM)** | Requires kernel module or signed BPF-LSM; per-machine privilege model. |
| **eBPF tracepoints** | Kernel-supported, low overhead, robust against static binaries and direct syscalls. PID filtering is a single map lookup. Ring buffer is lock-free. |

The trade-off: eBPF requires kernel ≥ 5.8 for ring buffer support. Older kernels can fall back to `BPF_MAP_TYPE_PERF_EVENT_ARRAY` with one perf event per CPU, but rage v1 requires 5.8+.

### Capabilities

eBPF programs that attach to tracepoints require `CAP_BPF` (kernel ≥ 5.8) or `CAP_SYS_ADMIN` (older). In CI containers, this means running with `--cap-add=BPF` (or `--cap-add=SYS_ADMIN`) and `--security-opt seccomp=unconfined` if the default seccomp profile blocks `bpf(2)`. The loader emits a clear error if it lacks the capability and downgrades to `loose` mode for the offending task.

## Windows — Detours inline patching

**Status: planned.** `crates/sandbox-windows-detours/` will be the Windows backend. The mechanism is Microsoft Detours — the same library BuildXL uses for its Windows sandbox. This is intentional design alignment: a rage build on Windows should match BuildXL's correctness model, because BuildXL has already proven the approach works at extreme scale on NTFS.

### Mechanism

Detours is **inline function patching via trampolines**, not IAT hooking. For each target function, Detours:

1. Replaces the first 5+ bytes of the target function with a `JMP` to a detour handler.
2. Saves the displaced original bytes into a dynamically allocated *trampoline* page that ends with a jump back to the rest of the original function.
3. Wraps the patch in a transaction (`DetourTransactionBegin` / `DetourAttach` / `DetourTransactionCommit`) so the patch is applied atomically across all threads — every thread is suspended, every thread's instruction pointer is rewritten if it sits inside a patched range, then every thread resumes.

Because the patch is at the function prologue, the hook fires for every caller — main binary, statically linked libraries, dynamically loaded DLLs. There is no per-image interpose table (macOS) and no kernel attach (Linux); the hook is in the target process's own memory.

```
macOS DYLD interpose:        per-library, resolved at load time, table-driven
Linux eBPF tracepoints:       kernel-level, PID-subtree filtered, syscall-level
Windows Detours patching:     in-process, trampoline-based, per-function
```

### APIs hooked

The full set required for build-cache correctness:

```
NtCreateFile, NtOpenFile             ← NT native layer (catches direct syscalls)
CreateFileW, CreateFileA             ← Win32 layer (catches normal opens)
ReadFile, WriteFile                  ← content I/O tracking
DeleteFileW, MoveFileExW, CopyFileW  ← file mutations
CreateDirectoryW, RemoveDirectoryW   ← directory operations
FindFirstFileW, FindNextFileW        ← directory enumeration (detects implicit deps)
GetFileAttributesW                   ← existence probes
CreateSymbolicLinkW, CreateHardLinkW ← link tracking
```

Both Win32 (`CreateFileW`) and NT native (`NtCreateFile`) are hooked because the call path is `CreateFileW → NtCreateFile → syscall`, and some tools (notably parts of the .NET runtime and certain Microsoft compilers) call the NT layer directly. Hooking only Win32 would miss those; hooking only NT would lose access to the resolved Win32 path string. Hooking both, with a thread-local re-entry guard, catches every file open exactly once.

`FindFirstFileW` / `FindNextFileW` are mandatory: directory enumeration is how many tools discover their inputs (`tsc` walking `node_modules`, `webpack` resolving modules), and the *list of entries seen* is itself an implicit dependency. Treating an enumeration as a stat of every entry would over-record; not recording it loses correctness when a new sibling file appears.

What is **not** hooked: registry APIs, network APIs, process/thread APIs. The pathset is filesystem-only — registry reads and network fetches are out of scope, the same as on macOS and Linux. (BuildXL's `DetoursServices.dll` hooks 100+ APIs because it is also a full policy enforcement engine; rage's scope is narrower.)

### DLL injection flow

The parent process launches the child with `DetourCreateProcessWithDllsW`. Sequence:

1. Parent calls `DetourCreateProcessWithDllsW(child.exe, …, ["rage_sandbox.dll"])`. Internally this calls `CreateProcess` with `CREATE_SUSPENDED`, so the child exists but its primary thread has not run a single instruction.
2. Detours allocates a page in the child via `VirtualAllocEx`, writes the path of `rage_sandbox.dll` with `WriteProcessMemory`, and creates a remote thread with `CreateRemoteThread(LoadLibraryW)` that loads the DLL into the child.
3. The DLL's `DllMain(DLL_PROCESS_ATTACH)` runs inside the child. It opens a `DetourTransactionBegin` / `DetourAttach(...)` / `DetourTransactionCommit` block to install all hooks, then connects to the parent's named pipe.
4. The remote thread exits. Detours resumes the primary thread of the child via `ResumeThread`. The child's `main` runs with all hooks already in place — every file access from the first instruction onward goes through the hook handler.

Because the hooks are installed before the child's main thread runs, there is no startup window where file accesses are unobserved. This matches the macOS guarantee (DYLD installs interpose before `main`) and the Linux guarantee (eBPF attaches before `execve` returns to the child).

### IPC — named pipes

Named pipes are the Windows equivalent of Unix sockets. The parent creates `\\.\pipe\rage_sandbox_{pid}_{unique_id}` before launching the child and passes the pipe name via environment variable (`RAGE_SANDBOX_PIPE=…`). The child DLL connects on `DllMain` and writes `FileAccessEvent` records as the hooks fire:

```rust
#[repr(C)]
struct FileAccessEvent {
    op: u8,                 // CreateFile | NtCreateFile | ReadFile | …
    pid: u32,
    access_flags: u32,      // GENERIC_READ / GENERIC_WRITE / …
    path_len: u16,
    path: [u16; path_len],  // UTF-16, fully resolved (reparse points and symlinks expanded)
}
```

The format is binary, not CBOR or JSON, because Windows Unicode paths are UTF-16 native and the hook handler runs on the hot path of every file open — serialization cost matters. Named pipes are full-duplex, handle backpressure via the kernel, and work across 32/64-bit process boundaries (relevant if a 64-bit task launches a 32-bit child tool).

### Rust implementation path

Two viable crates:

| Crate | What it is | Trade-off |
|---|---|---|
| `windows-detours` | Safe bindings over Microsoft's C++ Detours library | Full feature set; requires MSVC toolchain; pulls in the Detours C++ surface |
| `detour-rs` | Pure Rust inline patching | No C++ dependency; cross-platform-compatible internals; smaller surface |

rage will use `detour-rs`. The rest of the workspace forbids unsafe code (`unsafe_code = "forbid"` at the workspace level), and `detour-rs` keeps the C++ surface out of the build. The DLL itself is a separate crate (`sandbox-windows-detours`) of `crate-type = ["cdylib"]` with a per-crate `#![allow(unsafe_code)]` opt-out — the same pattern as `sandbox-macos-dylib` and `sandbox-linux-ebpf-prog`. The rest of the workspace stays unsafe-free; the FFI boundary is contained.

### Cross-architecture

The top-level process that drives Detours must be 64-bit. rage's CLI binary is 64-bit on every supported Windows host. The injected DLL must match the target process architecture: a 64-bit child gets the 64-bit DLL, a 32-bit child gets the 32-bit DLL. For monorepo build tooling — `node`, `tsc`, `cargo`, `python`, MSVC `cl.exe` — every relevant process is 64-bit in 2026, and rage will ship only the 64-bit DLL in v1. A 32-bit fallback is straightforward but not on the critical path.

### Performance

Detours patching adds approximately 1–5% overhead on process execution — the same order of magnitude as DYLD interpose on macOS, and slightly higher than eBPF on Linux (where the hook lives in the kernel, not the process). The hot cost is two indirect jumps (into the trampoline, out to the original function) per hooked call. Detours caches resolved symlink and reparse-point paths inside the hook handler so repeated opens of the same file don't re-walk the filesystem.

### Trade-offs vs other Windows options

| Approach | Why we rejected it |
|---|---|
| **MiniFilter driver** | Kernel driver. Requires a signed driver, WHQL submission, per-machine privilege model. Not shippable in OSS. |
| **ETW (Event Tracing for Windows)** | Coalesced events, multi-millisecond latency, no per-process scoping without correlation work, drops events under load. The Windows analogue of fseventsd. |
| **Sandboxie / AppContainer** | Security boundaries, not observation tools. AppContainer's policy model conflicts with normal build-tool behavior (registry access, etc.). |
| **DLL injection without Detours** | Possible (manual `CreateRemoteThread` + `LoadLibrary`) but you still need the hook mechanism inside the DLL. Detours is that mechanism. |
| **Detours inline patching** | Userland, no driver, no signing, in-process, atomic across threads, used by BuildXL in production for over a decade. |

The trade-off: Detours is bypassed by processes that walk their own import table and call kernel32 / ntdll syscalls by ordinal lookup at runtime. This is rare in build tooling and absent from every common compiler.

## Modes

The sandbox runs in one of three modes per task:

| Mode | Sandbox attached? | Action on undeclared read | Use case |
|---|---|---|---|
| `observed` (default) | Yes | Records into the pathset; logs a violation; task continues | Most packages, most of the time |
| `strict` | Yes | Blocks the read with `EACCES`; task fails with a pointer to suggested config additions | Hermetic-verified packages |
| `loose` | No | — | Legacy packages where the sandbox overhead isn't worth it; WF-only cache key |

Mode is per-package, configured via the three-tier system (`rage.json` `policies` glob applies to a package set, per-package override in the manifest's `rage` field). The default is `observed`.

`strict` mode is implemented by checking each path in the shim against the declared input glob set; a path outside the set returns `-1` with `errno = EACCES` from the syscall. The task sees a normal permission denied error. The pathset still records the attempted access.

`loose` mode skips both the dylib injection (macOS) and the eBPF attach (Linux). The cache key for loose tasks is the WF alone — no SF — which is correct only if declared inputs are exhaustive. This is the same trust model as Turborepo and Nx; it exists for legacy packages whose maintainers haven't (yet) brought them under sandbox.

## Toolchain allowlist

Plugins declare files the toolchain reads on every invocation:

```rust
fn toolchain_allowlist(&self) -> Vec<AllowlistEntry>;
```

The TypeScript plugin returns `node` itself, the resolved `tsc` binary, the Node `lib/` directory (`/usr/lib/node`, `/opt/homebrew/lib/node_modules/typescript/`, etc.), and the system `libc` paths. These are stripped from the pathset before it is written to disk: they don't change between runs of the same tool, and including them in the SF would force a cache miss on every system update.

Stripping toolchain reads also makes the pathset portable across machines (inside a `host_triple` cohort) — a `tsc -b` run on macOS and Linux read different absolute paths for `lib.dom.d.ts`, but both should produce the same SF. The allowlist normalizes them out.

## Pathset storage

After the task completes, the pathset is:

1. Filtered against the toolchain allowlist.
2. Sorted (BTreeSet → Vec).
3. Serialized as JSON.
4. Stored in `pathset_store` keyed by the task's WF.

The same WF can have multiple pathsets attached if the task takes different code paths under different inputs. The strong-fingerprint check disambiguates them: the SF includes the contents of the files in *that specific* pathset.

## What the sandbox does not catch

- **Network access.** Outside scope. A task that downloads a file from the internet and uses its contents will not have those bytes in the pathset. Tasks that hit the network are expected to declare their fetched content as an output (or to be marked `loose`).
- **Time-dependent behavior.** A task that reads `/dev/urandom` records the read but not its content. Determinism is the user's problem.
- **In-memory state.** Shared memory, IPC, environment-driven branching — none of these are in the pathset. Tracked env vars are in the WF instead.
- **Process-internal state.** A task that mmaps a file and reads it page-by-page goes through the kernel; mmap establishes the read but reading from the mapped region after that is not visible to the syscall layer. We accept this gap; mmap-based tools are correctly fingerprinted because the initial mmap registers the file.

## Performance

Order of magnitude on a TypeScript build over a 200-package monorepo:

| Mode | Slowdown |
|---|---|
| `loose` | baseline |
| macOS `observed` (DYLD interpose, FIFO drain) | ~3–8% over baseline |
| Linux `observed` (eBPF tracepoints, ring-buffer drain) | ~2–5% over baseline |
| `strict` (path check on every read) | ~5–12% |

The dominant cost is event serialization to userspace — particularly on `read`/`write` syscalls which fire many times for large files. For tasks that re-read the same file repeatedly, the shim deduplicates writes to the FIFO via a per-FD cache.

## Why bother with a syscall sandbox at all

The same question keeps coming back: "Turborepo and Nx work fine without one. Why ours?"

Because they don't, in fact, work fine. They work *most of the time*. A monorepo of any size accumulates undeclared inputs over years — a `tsconfig.base.json` two directories up, a generated file consumed by code-gen, a `.env.local` read at build time, an env var read by a tool you don't control. The build system caches a stale result, the user gets a wrong build, the user spends two days debugging, the user adds the missed file to `inputs:` in the config. The cycle continues forever, because the system has no way to surface the mistake.

A sandbox closes the loop. The first time a task reads `tsconfig.base.json` from `../../`, it lands in the pathset, becomes part of the SF, and from then on changes to `tsconfig.base.json` invalidate the task. The user never had to know the file was an input. The plugin author never had to encode it in declared globs.

This is the BuildXL bet, restated for monorepos that don't have a Microsoft-scale team writing pip declarations. The sandbox does the bookkeeping. The user does the build.
