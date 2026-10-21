// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Kernel soft-dirty page tracking for the minor collector (bliss-3uwzg).
//!
//! A minor collection must find every old-generation slot that points into the
//! nursery. Reference stores reach old objects from the interpreter, stdlib,
//! compiled code and foreign calls, and not all of them run the write barrier,
//! so the collector used to walk and trace the whole old generation twice per
//! minor collection: ~2.9 ms per MB of live conses (GitHub #471).
//!
//! Linux records every write to a page, from any instruction or system call, in
//! the page's soft-dirty bit. At the end of each minor collection the nursery
//! is empty, so no older object holds a nursery reference; the collector then
//! clears the bits (`/proc/self/clear_refs` = 4). An old-to-young reference can
//! only appear afterwards through a store, which sets its page's bit, so the
//! next minor collection reads `/proc/self/pagemap` and traces only the slots on
//! dirty pages.
//!
//! The tracker is armed only after a clear that succeeded for the current heap;
//! any failure, a major collection, or a heap replacement disarms it and the
//! next minor collection scans the whole old generation again. It is disabled
//! unless a self-test shows that the kernel really tracks the bit (without
//! `CONFIG_MEM_SOFT_DIRTY` every page reads as clean, which would be unsound),
//! and is used only while the old generation is at least a quarter of the
//! nursery's size (see `worthwhile`). `EGCL_GC_SOFT_DIRTY=0` turns it off and
//! `EGCL_GC_SOFT_DIRTY=1` uses it whatever the old generation's size. A tool that clears soft-dirty bits
//! from outside the process (CRIU pre-dump) would hide stores from the
//! collector; run such a process with `EGCL_GC_SOFT_DIRTY=0`.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Page-granular dirty bitmap over the heap, filled by [`read_dirty_pages`].
#[derive(Default)]
pub(crate) struct DirtyPages {
    heap_base: usize,
    heap_end: usize,
    page_shift: u32,
    bits: Vec<u64>,
}

impl DirtyPages {
    /// True if the page containing `addr` was written since the last clear.
    /// Addresses outside the heap read as dirty, so callers never skip them.
    #[inline]
    pub(crate) fn is_dirty(&self, addr: usize) -> bool {
        if addr < self.heap_base || addr >= self.heap_end {
            return true;
        }
        let page = (addr - self.heap_base) >> self.page_shift;
        self.bits[page / 64] & (1u64 << (page % 64)) != 0
    }

    /// True if any page overlapping `[start, end)` is dirty.
    #[inline]
    pub(crate) fn any_dirty(&self, start: usize, end: usize) -> bool {
        if end <= start {
            return false;
        }
        if start < self.heap_base || end > self.heap_end {
            return true;
        }
        let first = (start - self.heap_base) >> self.page_shift;
        let last = (end - 1 - self.heap_base) >> self.page_shift;
        let mut page = first;
        while page <= last {
            let word = self.bits[page / 64] >> (page % 64);
            if word != 0 {
                let next = page + word.trailing_zeros() as usize;
                return next <= last;
            }
            page = (page / 64 + 1) * 64;
        }
        false
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn reset(&mut self, heap_base: usize, heap_size: usize, page_shift: u32) {
        self.heap_base = heap_base;
        self.heap_end = heap_base + heap_size;
        self.page_shift = page_shift;
        let pages = heap_size.div_ceil(1usize << page_shift);
        self.bits.resize(pages.div_ceil(64), 0);
        self.bits.fill(0);
    }

    #[cfg(test)]
    pub(crate) fn for_test(heap_base: usize, heap_size: usize, dirty: &[usize]) -> Self {
        let mut pages = DirtyPages::default();
        pages.reset(heap_base, heap_size, 12);
        for &addr in dirty {
            let page = (addr - heap_base) >> 12;
            pages.bits[page / 64] |= 1u64 << (page % 64);
        }
        pages
    }
}

/// Heap base the tracker is armed for (0 = not armed), and its size.
static ARMED_BASE: AtomicUsize = AtomicUsize::new(0);
static ARMED_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Forget the last clear: the next minor collection scans the whole old
/// generation and re-arms the tracker. Called after a major collection.
pub(crate) fn disarm() {
    ARMED_BASE.store(0, Ordering::Release);
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    use std::sync::Mutex;

    const PM_SOFT_DIRTY: u64 = 1 << 55;

    pub(super) struct Files {
        pid: u32,
        pagemap: File,
        clear_refs: File,
    }

    impl Files {
        fn open() -> Option<Files> {
            Some(Files {
                pid: std::process::id(),
                pagemap: File::open("/proc/self/pagemap").ok()?,
                clear_refs: std::fs::OpenOptions::new()
                    .write(true)
                    .open("/proc/self/clear_refs")
                    .ok()?,
            })
        }

        pub(super) fn clear(&mut self) -> bool {
            (&self.clear_refs).write_all(b"4").is_ok()
        }

        /// Read the soft-dirty bits of the pages covering `[start, end)`
        /// (page-aligned) and hand each dirty page's index to `mark`.
        pub(super) fn read(
            &self,
            start: usize,
            end: usize,
            page_shift: u32,
            buf: &mut Vec<u64>,
            mut mark: impl FnMut(usize),
        ) -> bool {
            let first = start >> page_shift;
            let count = (end - start) >> page_shift;
            buf.resize(count, 0);
            // SAFETY: u64 has no invalid bit patterns; the slice covers `buf`.
            let bytes =
                unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, count * 8) };
            if self.pagemap.read_exact_at(bytes, first as u64 * 8).is_err() {
                return false;
            }
            for (i, &entry) in buf.iter().enumerate() {
                if entry & PM_SOFT_DIRTY != 0 {
                    mark(first + i);
                }
            }
            true
        }
    }

    pub(super) struct Tracker {
        pub(super) files: Option<Files>,
        pub(super) buf: Vec<u64>,
        /// The files could not be opened or the self-test failed: never retry.
        pub(super) unsupported: bool,
    }

    pub(super) static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);

    /// Check that the kernel tracks writes: two touched pages, a clear, a
    /// write to the first, and the bits must read exactly `[dirty, clean]`.
    fn self_test(files: &mut Files, page_shift: u32) -> bool {
        let page = 1usize << page_shift;
        // SAFETY: a private anonymous mapping, unmapped before returning.
        let Ok(test) = (unsafe {
            crate::syscall::mmap(
                std::ptr::null_mut(),
                2 * page,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                crate::syscall::MAP_PRIVATE | crate::syscall::MAP_ANONYMOUS,
                -1,
                0,
            )
        }) else {
            return false;
        };
        let addr = test as usize;
        // SAFETY: both pages belong to the mapping above.
        unsafe {
            std::ptr::write_volatile(test, 1);
            std::ptr::write_volatile(test.add(page), 1);
        }
        let mut result = files.clear();
        if result {
            // SAFETY: as above.
            unsafe { std::ptr::write_volatile(test, 2) };
            let mut dirty = [false; 2];
            let mut buf = Vec::new();
            result = files.read(addr, addr + 2 * page, page_shift, &mut buf, |p| {
                dirty[p - (addr >> page_shift)] = true;
            }) && dirty == [true, false];
        }
        // SAFETY: the mapping created above.
        let _ = unsafe { crate::syscall::munmap(test, 2 * page) };
        result
    }

    /// The tracker's files, opened (and self-tested) for this process.
    /// The tracker's files for this process, if already open: a forked child
    /// must not read its parent's page tables.
    pub(super) fn open_files(tracker: &mut Tracker) -> Option<&mut Files> {
        let pid = std::process::id();
        tracker.files.as_mut().filter(|f| f.pid == pid)
    }

    /// Open (and self-test) the tracker's files for this process. The
    /// self-test clears every soft-dirty bit, so only `rearm` calls this.
    pub(super) fn files(tracker: &mut Tracker, page_shift: u32) -> Option<&mut Files> {
        if tracker.unsupported {
            return None;
        }
        let pid = std::process::id();
        if tracker.files.as_ref().is_some_and(|f| f.pid != pid) {
            tracker.files = None;
        }
        if tracker.files.is_none() {
            let files = Files::open()
                .and_then(|mut files| self_test(&mut files, page_shift).then_some(files));
            if files.is_none() {
                tracker.unsupported = true;
                return None;
            }
            tracker.files = files;
        }
        tracker.files.as_mut()
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn page_shift() -> u32 {
    crate::syscall::page_size().trailing_zeros()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Auto,
    On,
}

/// `EGCL_GC_SOFT_DIRTY`: `0` never tracks, `1` always tracks, anything else
/// (the default) tracks once the old generation is big enough to pay for it.
fn mode() -> Mode {
    static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return Mode::Off;
        }
        match std::env::var("EGCL_GC_SOFT_DIRTY")
            .as_deref()
            .map(str::trim)
        {
            Ok("0") => Mode::Off,
            Ok("1") => Mode::On,
            _ => Mode::Auto,
        }
    })
}

fn enabled() -> bool {
    mode() != Mode::Off
}

/// Whether tracking pays for itself. A clear write-protects every page of the
/// process, so each nursery page then takes one minor fault on its first
/// write: measured at ~1.7 us of system time per page, ~28 ms per 64 MB
/// nursery cycle. Scanning the old generation costs ~4 ms per MB of conses
/// (~0.3 ms per MB of pointer-free data), so below a quarter of the nursery's
/// size the whole-generation scan is the cheaper one.
fn worthwhile(old_bytes: usize, nursery_bytes: usize) -> bool {
    match mode() {
        Mode::Off => false,
        Mode::On => true,
        Mode::Auto => old_bytes >= nursery_bytes / 4,
    }
}

/// Fill `out` with the pages of `ranges` written since the last
/// [`rearm`] of this heap. Returns false — and the caller must scan the whole
/// old generation — when the tracker is not armed for this heap or the kernel
/// read fails. `ranges` are `[start, end)` byte ranges inside the heap.
pub(crate) fn read_dirty_pages(
    heap_base: usize,
    heap_size: usize,
    ranges: &[(usize, usize)],
    out: &mut DirtyPages,
) -> bool {
    if !enabled()
        || ARMED_BASE.load(Ordering::Acquire) != heap_base
        || ARMED_SIZE.load(Ordering::Acquire) != heap_size
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let shift = page_shift();
        out.reset(heap_base, heap_size, shift);
        let mut guard = imp::TRACKER.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tracker) = guard.as_mut() else {
            return false;
        };
        let mut buf = std::mem::take(&mut tracker.buf);
        let Some(files) = imp::open_files(tracker) else {
            tracker.buf = buf;
            disarm();
            return false;
        };
        let mask = (1usize << shift) - 1;
        let base_page = heap_base >> shift;
        let mut ok = true;
        for &(start, end) in ranges {
            let start = start & !mask;
            let end = (end + mask) & !mask;
            if end <= start {
                continue;
            }
            let bits = &mut out.bits;
            if !files.read(start, end, shift, &mut buf, |page| {
                let rel = page - base_page;
                bits[rel / 64] |= 1u64 << (rel % 64);
            }) {
                ok = false;
                break;
            }
        }
        tracker.buf = buf;
        if !ok {
            disarm();
        }
        ok
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (ranges, out);
        false
    }
}

/// Clear every soft-dirty bit and arm the tracker for this heap, when an old
/// generation of `old_bytes` makes that worthwhile against a nursery of
/// `nursery_bytes`. Call with the world stopped, after the collector's last
/// write that could leave an old object referring into the nursery — i.e.
/// once the nursery is empty.
pub(crate) fn rearm(heap_base: usize, heap_size: usize, old_bytes: usize, nursery_bytes: usize) {
    disarm();
    if !worthwhile(old_bytes, nursery_bytes) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let mut guard = imp::TRACKER.lock().unwrap_or_else(|e| e.into_inner());
        let tracker = guard.get_or_insert_with(|| imp::Tracker {
            files: None,
            buf: Vec::new(),
            unsupported: false,
        });
        let Some(files) = imp::files(tracker, page_shift()) else {
            return;
        };
        if files.clear() {
            ARMED_SIZE.store(heap_size, Ordering::Release);
            ARMED_BASE.store(heap_base, Ordering::Release);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (heap_base, heap_size);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_dirty_finds_pages_inside_the_range_only() {
        let base = 0x10_0000;
        let pages = DirtyPages::for_test(base, 1 << 20, &[base + 5 * 4096 + 8]);
        assert!(pages.is_dirty(base + 5 * 4096));
        assert!(!pages.is_dirty(base + 4 * 4096));
        assert!(pages.any_dirty(base + 4 * 4096 + 4000, base + 5 * 4096 + 1));
        assert!(!pages.any_dirty(base, base + 5 * 4096));
        assert!(!pages.any_dirty(base + 6 * 4096, base + 200 * 4096));
        assert!(pages.any_dirty(base, base + (1 << 20)));
        assert!(pages.is_dirty(base - 8), "outside the heap reads as dirty");
    }
}
