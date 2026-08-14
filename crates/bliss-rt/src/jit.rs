//! Executable memory for JIT-compiled code (§4.7 codegen → execution).
//!
//! A `JitBuffer` owns a page-aligned mapping that holds machine code emitted by
//! the compiler. It is mapped W→X (writable while filling, then made
//! read+execute) so the process never holds a simultaneously writable+executable
//! mapping. The buffer is leaked for the lifetime of the code cache; installed
//! native code lives forever.

/// A block of executable machine code.
pub struct JitBuffer {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: after `new` the mapping is read+execute only; the pointer is never
// mutated, so it is safe to share across threads.
unsafe impl Send for JitBuffer {}
unsafe impl Sync for JitBuffer {}

impl JitBuffer {
    /// Allocate executable memory, copy `code` into it, and make it
    /// read+execute. Returns `None` if the mapping fails.
    #[cfg(unix)]
    pub fn new(code: &[u8]) -> Option<Self> {
        if code.is_empty() {
            return None;
        }
        let page = 4096usize;
        let len = code.len().div_ceil(page) * page;
        // SAFETY: standard anonymous mmap; we check for MAP_FAILED.
        unsafe {
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if ptr == libc::MAP_FAILED {
                return None;
            }
            let ptr = ptr as *mut u8;
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
            // W^X: drop write, add execute.
            if libc::mprotect(ptr as *mut libc::c_void, len, libc::PROT_READ | libc::PROT_EXEC) != 0
            {
                libc::munmap(ptr as *mut libc::c_void, len);
                return None;
            }
            // Flush the instruction cache (a no-op on x86, required on aarch64).
            #[cfg(target_arch = "aarch64")]
            {
                std::arch::asm!(
                    "isb",
                    options(nostack, preserves_flags),
                );
            }
            Some(JitBuffer { ptr, len })
        }
    }

    #[cfg(not(unix))]
    pub fn new(_code: &[u8]) -> Option<Self> {
        None
    }

    /// Pointer to the executable code.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Leak this buffer, returning its code pointer. The mapping lives forever
    /// (the code cache owns installed native code).
    pub fn leak(self) -> *const u8 {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }
}

impl Drop for JitBuffer {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: `ptr`/`len` came from a successful mmap in `new`.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.len);
        }
    }
}
