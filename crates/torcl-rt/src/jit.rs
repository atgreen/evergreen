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
        Option<Box<[windows_sys::Win32::System::Diagnostics::Debug::IMAGE_RUNTIME_FUNCTION_ENTRY]>>,
}

/// One native-code range and its serialized Win64 UNWIND_INFO.
/// Offsets are relative to the beginning of the owning code buffer.
#[cfg(all(windows, target_arch = "x86_64"))]
#[derive(Clone, Copy)]
pub struct WindowsUnwindInfo<'a> {
    pub begin: u32,
    /// Exclusive end of the function's instruction range.
    pub end: u32,
    pub unwind_info: &'a [u8],
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
        let range = WindowsUnwindInfo {
            begin: 0,
            end: u32::try_from(code.len()).ok()?,
            unwind_info,
        };
        // SAFETY: the caller supplies valid metadata for this whole-code range.
        unsafe { Self::new_with_windows_unwind_ranges(code, &[range]) }
    }

    /// Install code containing multiple independently described unwind ranges.
    /// Ranges must be nonempty, sorted, disjoint, and contained in `code`.
    /// Gaps are allowed; callers must ensure they require no unwind entry.
    /// Each metadata record is copied after the code at a DWORD-aligned offset.
    /// The entire function table remains registered until this buffer is dropped.
    ///
    /// # Safety
    /// Each record must be valid Windows x64 UNWIND_INFO describing the machine
    /// state throughout its range, including prologue, body and epilogues.
    /// Embedded handler/chained RVAs are relative to the returned code pointer;
    /// they are not relocated. All executions and stack walks through this code
    /// must finish before dropping the buffer.
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub unsafe fn new_with_windows_unwind_ranges(
        code: &[u8],
        ranges: &[WindowsUnwindInfo<'_>],
    ) -> Option<Self> {
        use windows_sys::Win32::System::Diagnostics::Debug::*;

        if code.is_empty() || ranges.is_empty() {
            return None;
        }
        let code_len = u32::try_from(code.len()).ok()?;
        let count = u32::try_from(ranges.len()).ok()?;
        let mut total = code_len;
        let mut previous_end = 0;
        let mut table = Vec::new();
        table.try_reserve_exact(ranges.len()).ok()?;
        for range in ranges {
            if range.begin < previous_end
                || range.begin >= range.end
                || range.end > code_len
                || range.unwind_info.len() < 4
            {
                return None;
            }
            previous_end = range.end;
            let unwind_offset = total.checked_add(3)? & !3;
            total = unwind_offset.checked_add(u32::try_from(range.unwind_info.len()).ok()?)?;
            table.push(IMAGE_RUNTIME_FUNCTION_ENTRY {
                BeginAddress: range.begin,
                EndAddress: range.end,
                Anonymous: IMAGE_RUNTIME_FUNCTION_ENTRY_0 {
                    UnwindInfoAddress: unwind_offset,
                },
            });
        }
        let mut image = Vec::new();
        image.try_reserve_exact(total as usize).ok()?;
        image.extend_from_slice(code);
        for (entry, range) in table.iter().zip(ranges) {
            // SAFETY: every entry was initialized with UnwindInfoAddress above.
            image.resize(unsafe { entry.Anonymous.UnwindInfoAddress } as usize, 0);
            image.extend_from_slice(range.unwind_info);
        }
        let mut buffer = Self::new(&image)?;
        // Box before registration: the OS retains the table's stable address.
        let table = table.into_boxed_slice();
        if !unsafe { RtlAddFunctionTable(table.as_ptr(), count, buffer.ptr as u64) } {
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
                windows_sys::Win32::System::Diagnostics::Debug::RtlDeleteFunctionTable(
                    table.as_ptr(),
                )
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
