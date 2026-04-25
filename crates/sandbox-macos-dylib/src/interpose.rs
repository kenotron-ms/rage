use crate::client::send_event;
use libc::{c_char, c_int, c_void, mode_t};
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

/// Interposed replacement for libc's `openat(2)`.
///
/// # Safety
///
/// `path` may be null; we guard with an explicit null-check.  When non-null it
/// must point to a valid, NUL-terminated C string — the contract imposed by the
/// POSIX `openat` ABI.
unsafe extern "C" fn rage_openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: mode_t,
) -> c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            let is_write = (flags & libc::O_WRONLY) != 0
                || (flags & libc::O_RDWR) != 0
                || (flags & libc::O_CREAT) != 0
                || (flags & libc::O_TRUNC) != 0;

            send_event(if is_write { "write" } else { "read" }, s);
        }
    }

    libc::openat(dirfd, path, flags, mode as libc::c_uint)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_OPENAT: InterposeEntry = InterposeEntry {
    replacement: rage_openat as *const c_void,
    original: libc::openat as *const c_void,
};

/// Interposed replacement for libc's `stat(2)`.
///
/// # Safety
///
/// `path` may be null; we guard with an explicit null-check.  When non-null it
/// must point to a valid, NUL-terminated C string.  `buf` must be a valid
/// writable pointer to a `libc::stat` — this is the contract imposed by the
/// POSIX `stat` ABI.
unsafe extern "C" fn rage_stat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("read", s);
        }
    }

    libc::stat(path, buf)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_STAT: InterposeEntry = InterposeEntry {
    replacement: rage_stat as *const c_void,
    original: libc::stat as *const c_void,
};

/// Interposed replacement for libc's `lstat(2)`.
///
/// # Safety
///
/// `path` may be null; we guard with an explicit null-check.  When non-null it
/// must point to a valid, NUL-terminated C string.  `buf` must be a valid
/// writable pointer to a `libc::stat` — this is the contract imposed by the
/// POSIX `lstat` ABI.
unsafe extern "C" fn rage_lstat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("read", s);
        }
    }

    libc::lstat(path, buf)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_LSTAT: InterposeEntry = InterposeEntry {
    replacement: rage_lstat as *const c_void,
    original: libc::lstat as *const c_void,
};

/// Interposed replacement for libc's `rename(2)`.
///
/// # Safety
///
/// `old` and `new` may be null; we guard with explicit null-checks.  When
/// non-null they must point to valid, NUL-terminated C strings — the contract
/// imposed by the POSIX `rename` ABI.  Both paths are recorded as write events
/// because the old path is removed and the new path is created/replaced.
unsafe extern "C" fn rage_rename(old: *const c_char, new: *const c_char) -> c_int {
    if !old.is_null() {
        if let Ok(s) = CStr::from_ptr(old).to_str() {
            send_event("write", s);
        }
    }
    if !new.is_null() {
        if let Ok(s) = CStr::from_ptr(new).to_str() {
            send_event("write", s);
        }
    }

    libc::rename(old, new)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_RENAME: InterposeEntry = InterposeEntry {
    replacement: rage_rename as *const c_void,
    original: libc::rename as *const c_void,
};

/// Interposed replacement for libc's `unlink(2)`.
///
/// # Safety
///
/// `path` may be null; we guard with an explicit null-check.  When non-null it
/// must point to a valid, NUL-terminated C string — the contract imposed by the
/// POSIX `unlink` ABI.  Deleting a file is a write (mutation) event.
unsafe extern "C" fn rage_unlink(path: *const c_char) -> c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("write", s);
        }
    }

    libc::unlink(path)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_UNLINK: InterposeEntry = InterposeEntry {
    replacement: rage_unlink as *const c_void,
    original: libc::unlink as *const c_void,
};

/// Interposed replacement for libc's `mkdir(2)`.
///
/// # Safety
///
/// `path` may be null; we guard with an explicit null-check.  When non-null it
/// must point to a valid, NUL-terminated C string — the contract imposed by the
/// POSIX `mkdir` ABI.  Creating a directory is a write (mutation) event.
unsafe extern "C" fn rage_mkdir(path: *const c_char, mode: mode_t) -> c_int {
    if !path.is_null() {
        if let Ok(s) = CStr::from_ptr(path).to_str() {
            send_event("write", s);
        }
    }

    libc::mkdir(path, mode)
}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_MKDIR: InterposeEntry = InterposeEntry {
    replacement: rage_mkdir as *const c_void,
    original: libc::mkdir as *const c_void,
};
