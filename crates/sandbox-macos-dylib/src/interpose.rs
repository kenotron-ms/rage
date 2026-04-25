use crate::client::send_event;
use libc::{c_int, c_void, mode_t};
use std::ffi::CStr;

/// A Mach-O `__DATA,__interpose` section entry.
///
/// Placing an array of these in the `__DATA,__interpose` section causes dyld
/// to redirect calls to `original` to `replacement` for any image that is
/// loaded after this dylib.
#[repr(C)]
pub struct InterposeEntry {
    pub replacement: *const c_void,
    pub original: *const c_void,
}

// SAFETY: The raw pointers are function pointers to static C-ABI symbols and
// are valid for the lifetime of the process.  `InterposeEntry` only resides in
// the read-only interpose section and is never mutated after dylib load.
unsafe impl Sync for InterposeEntry {}

/// Interposed replacement for libc's `open(2)`.
///
/// # Safety
///
/// `path` may be null (callers occasionally pass NULL; we guard with an
/// explicit null-check before dereferencing).  When non-null it must point to
/// a valid, NUL-terminated C string — this is the contract imposed by the
/// POSIX `open` ABI that all callers must satisfy.
///
/// Forwarding is done via `libc::open(path, flags, mode)`.  Because this
/// function is registered in `__DATA,__interpose`, dyld rewrites *all* call
/// sites to `open` in loaded images to point at `rage_open` — but the symbol
/// `libc::open` inside this dylib resolves directly to the original syscall
/// stub, so there is no recursion.
unsafe extern "C" fn rage_open(path: *const libc::c_char, flags: c_int, mode: mode_t) -> c_int {
    // Guard against NULL path before dereferencing.
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            // Classify as write if any write-implying flag is set.
            let is_write = (flags & libc::O_WRONLY) != 0
                || (flags & libc::O_RDWR) != 0
                || (flags & libc::O_CREAT) != 0
                || (flags & libc::O_TRUNC) != 0;

            send_event(if is_write { "write" } else { "read" }, s);
        }
    }

    // Forward to the real libc open.  The interpose table ensures this
    // symbol resolves to the original syscall stub, not back to rage_open.
    // mode_t is u16 on macOS; open(2) is variadic so we must widen to c_uint.
    libc::open(path, flags, mode as libc::c_uint)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_OPEN: InterposeEntry = InterposeEntry {
    replacement: rage_open as *const c_void,
    original: libc::open as *const c_void,
};
