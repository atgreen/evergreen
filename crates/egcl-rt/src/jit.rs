// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Executable memory for JIT-compiled code (§4.7 codegen → execution).
//!
//! A `JitBuffer` owns a page-aligned mapping that holds machine code emitted by
//! the compiler. It is mapped W→X (writable while filling, then made
//! read+execute) so the process never holds a simultaneously writable+executable
//! mapping. Installed native code owns its buffer until its last activation or
//! compiled caller releases it. Process-lifetime trampolines may leak a buffer.

/// A block of executable machine code.
pub struct JitBuffer {
    ptr: *mut u8,
    len: usize,
    code_len: usize,
    debug_registration: Option<crate::jit_debug::Registration>,
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
            // Make the instructions visible to the fetch path (nothing to do on
            // x86-64, which has coherent caches).
            #[cfg(target_arch = "aarch64")]
            flush_instruction_cache(ptr, code.len());
            Some(JitBuffer {
                ptr,
                len,
                code_len: code.len(),
                debug_registration: None,
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
        buffer.code_len = code.len();
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

    /// Emit shared DWARF and register this installed code with supported native
    /// debuggers. Only Rust memory is allocated; no Lisp GC can run here.
    pub fn install_debug_info(
        &mut self,
        name: &str,
    ) -> Result<std::sync::Weak<crate::jit_debug::DwarfImage>, String> {
        if self.debug_registration.is_some() {
            return Err("JIT debug information already installed".into());
        }
        // SAFETY: this buffer owns the immutable mapping for the whole call.
        let code = unsafe { std::slice::from_raw_parts(self.ptr, self.code_len) };
        let image = std::sync::Arc::new(crate::jit_debug::DwarfImage::new(
            name,
            self.ptr as u64,
            code,
        )?);
        let registration = crate::jit_debug::Registration::new(image);
        let weak = registration.image();
        self.debug_registration = Some(registration);
        Ok(weak)
    }

    /// Leak this buffer, returning its code pointer. The mapping lives forever
    /// (reserved for process-lifetime code such as recovery trampolines).
    pub fn leak(self) -> *const u8 {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }
}

/// Make newly written instructions visible to instruction fetch.
///
/// AArch64's instruction and data caches are **not** coherent with each other
/// (Arm ARM B2.4.4, "Concurrent modification and execution of instructions"):
/// code written with ordinary stores sits in the data cache, while instruction
/// fetch reads the instruction cache. The lines must be cleaned to the point of
/// unification and then invalidated in the I-cache, exactly as a compiler's
/// `__clear_cache` does. An `ISB` alone — which is what this used to be — only
/// discards the prefetch pipeline and does nothing about either cache; it
/// appeared to work solely because Linux happens to flush when `mprotect` makes
/// a page executable, which is an implementation detail and is no help at all
/// when patching code that is *already* executable (tier promotion).
///
/// # Safety
/// `ptr..ptr + len` must be a mapped range in this process.
#[cfg(target_arch = "aarch64")]
unsafe fn flush_instruction_cache(ptr: *const u8, len: usize) {
    if len == 0 {
        return;
    }
    // CTR_EL0 gives each cache's minimum line size as a log2 count of 4-byte
    // words: D-cache in bits 19:16 (DminLine), I-cache in bits 3:0 (IminLine).
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable from EL0 on Linux (trapped and emulated if the
    // hardware does not permit it directly).
    unsafe {
        std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags));
    }
    let data_line = 4usize << ((ctr >> 16) & 0xf);
    let inst_line = 4usize << (ctr & 0xf);
    let end = ptr as usize + len;

    // Clean the data cache by VA to the point of unification, one line at a
    // time from the containing line's start, then order those cleans before the
    // invalidations below.
    let mut addr = (ptr as usize) & !(data_line - 1);
    while addr < end {
        // SAFETY: `addr` is within the mapped range, rounded down to a line.
        unsafe {
            std::arch::asm!("dc cvau, {}", in(reg) addr, options(nostack, preserves_flags));
        }
        addr += data_line;
    }
    // SAFETY: a barrier; touches no memory operand.
    unsafe {
        std::arch::asm!("dsb ish", options(nostack, preserves_flags));
    }

    let mut addr = (ptr as usize) & !(inst_line - 1);
    while addr < end {
        // SAFETY: as above; `ic ivau` is permitted from EL0 on Linux.
        unsafe {
            std::arch::asm!("ic ivau, {}", in(reg) addr, options(nostack, preserves_flags));
        }
        addr += inst_line;
    }
    // Order the invalidations, then discard anything already prefetched.
    // SAFETY: barriers only.
    unsafe {
        std::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

impl Drop for JitBuffer {
    fn drop(&mut self) {
        // Field destruction would happen AFTER munmap. Unregister explicitly,
        // even when a debugger reader still holds an Arc to the immutable bytes.
        drop(self.debug_registration.take());
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
