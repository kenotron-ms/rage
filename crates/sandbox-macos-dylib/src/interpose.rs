/// A Mach-O `__DATA,__interpose` section entry.
///
/// Placing an array of these in the `__DATA,__interpose` section causes dyld
/// to redirect calls to `original` to `replacement` for any image that is
/// loaded after this dylib.
#[repr(C)]
pub struct InterposeEntry {
    pub replacement: *const libc::c_void,
    pub original: *const libc::c_void,
}

// SAFETY: The raw pointers are function pointers to static C-ABI symbols and
// are valid for the lifetime of the process.  `InterposeEntry` only resides in
// the read-only interpose section and is never mutated after dylib load.
unsafe impl Sync for InterposeEntry {}
