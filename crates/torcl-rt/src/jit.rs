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
    #[cfg(all(windows, target_arch = "x86_64"))]
    unwind:
        Option<Box<windows_sys::Win32::System::Diagnostics::Debug::IMAGE_RUNTIME_FUNCTION_ENTRY>>,
}

// SAFETY: after `new` the mapping is read+execute only; the pointer is never
// mutated, so it is safe to share across threads.
unsafe impl Send for JitBuffer {}
unsafe impl Sync for JitBuffer {}

impl JitBuffer {
    /// Allocate executable memory, copy `code` into it, and make it
    /// read+execute. Returns `None` if the mapping fails.
    /// On Windows x64, nonleaf functions also require unwind metadata; use
    /// `new_with_windows_unwind` when emitting a frame.
    #[cfg(any(unix, windows))]
    pub fn new(code: &[u8]) -> Option<Self> {
        if code.is_empty() {
            return None;
        }
        let page = crate::syscall::page_size();
        let len = code.len().checked_add(page - 1)? & !(page - 1);
        // SAFETY: standard anonymous mmap; a mapping failure returns Err.
        unsafe {
            let ptr = match crate::syscall::mmap(
                std::ptr::null_mut(),
                len,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                crate::syscall::MAP_PRIVATE | crate::syscall::MAP_ANONYMOUS,
                -1,
                0,
            ) {
                Ok(p) => p,
                Err(_) => return None,
            };
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
            // W^X: drop write, add execute.
            if crate::syscall::mprotect(
                ptr,
                len,
                crate::syscall::PROT_READ | crate::syscall::PROT_EXEC,
            )
            .is_err()
            {
                let _ = crate::syscall::munmap(ptr, len);
                return None;
            }
            #[cfg(windows)]
            if windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                ptr.cast(),
                code.len(),
            ) == 0
            {
                let _ = crate::syscall::munmap(ptr, len);
                return None;
            }
            // Flush the instruction cache (a no-op on x86, required on aarch64).
            #[cfg(target_arch = "aarch64")]
            {
                std::arch::asm!("isb", options(nostack, preserves_flags),);
            }
            Some(JitBuffer {
                ptr,
                len,
                #[cfg(all(windows, target_arch = "x86_64"))]
                unwind: None,
            })
        }
    }

    /// Install one Win64 function together with its serialized UNWIND_INFO.
    /// The metadata shares the immutable mapping with the code, and its OS
    /// registration lives until this buffer is dropped (or forever if leaked).
    ///
    /// # Safety
    /// `unwind_info` must be valid Windows x64 UNWIND_INFO describing `code`
    /// exactly, including every prologue operation and any referenced handler
    /// data. RVAs are relative to the returned code pointer. All executions
    /// and stack walks through this code must finish before dropping the buffer.
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub unsafe fn new_with_windows_unwind(code: &[u8], unwind_info: &[u8]) -> Option<Self> {
        use windows_sys::Win32::System::Diagnostics::Debug::*;

        if code.is_empty() || unwind_info.len() < 4 {
            return None;
        }
        let code_len = u32::try_from(code.len()).ok()?;
        let unwind_offset = code_len.checked_add(3)? & !3;
        let total = (unwind_offset as usize).checked_add(unwind_info.len())?;
        u32::try_from(total).ok()?;
        let mut image = Vec::new();
        image.try_reserve_exact(total).ok()?;
        image.extend_from_slice(code);
        image.resize(unwind_offset as usize, 0);
        image.extend_from_slice(unwind_info);
        let mut buffer = Self::new(&image)?;
        // The OS retains this table address: a Box keeps it stable when the
        // owning JitBuffer moves into an adapter/cache.
        let table = Box::new(IMAGE_RUNTIME_FUNCTION_ENTRY {
            BeginAddress: 0,
            EndAddress: code_len,
            Anonymous: IMAGE_RUNTIME_FUNCTION_ENTRY_0 {
                UnwindInfoAddress: unwind_offset,
            },
        });
        if !unsafe { RtlAddFunctionTable(&*table, 1, buffer.ptr as u64) } {
            return None;
        }
        buffer.unwind = Some(table);
        Some(buffer)
    }

    #[cfg(not(any(unix, windows)))]
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
        #[cfg(all(windows, target_arch = "x86_64"))]
        if let Some(table) = self.unwind.take() {
            // Unregister before freeing either the table or its code/xdata.
            if !unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::RtlDeleteFunctionTable(&*table)
            } {
                // A retained OS registration must never point into freed
                // storage. Conservatively retain both allocations on failure.
                std::mem::forget(table);
                return;
            }
        }
        #[cfg(any(unix, windows))]
        // SAFETY: `ptr`/`len` came from a successful mmap in `new`.
        unsafe {
            let _ = crate::syscall::munmap(self.ptr, self.len);
        }
    }
}
