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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
static ACTIVE_ASYNC: AtomicBool = AtomicBool::new(false);

/// Forget the last clear: the next minor collection scans the whole old
/// generation and re-arms the tracker. Called after a major collection.
pub(crate) fn disarm() {
    ARMED_BASE.store(0, Ordering::Release);
    ACTIVE_ASYNC.store(false, Ordering::Release);
    crate::gc_async_wp::invalidate();
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    use std::sync::{Mutex, MutexGuard};

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

    pub(super) fn lock_tracker() -> Option<MutexGuard<'static, Option<Tracker>>> {
        // Fork can inherit a lock held by a vanished parent thread. Poison is
        // also unknown state: neither case may publish or consume clean bits.
        TRACKER.try_lock().ok()
    }

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

fn prefer_async(mode: Mode, disabled: bool) -> bool {
    mode == Mode::Auto && !disabled
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
    older_regions: &[crate::gc_async_wp::Range],
    out: &mut DirtyPages,
) -> bool {
    if !enabled()
        || ARMED_BASE.load(Ordering::Acquire) != heap_base
        || ARMED_SIZE.load(Ordering::Acquire) != heap_size
    {
        return false;
    }
    if ACTIVE_ASYNC.load(Ordering::Acquire) {
        let shift = page_shift();
        out.reset(heap_base, heap_size, shift);
        // A missing coverage entry must never look clean, including large-
        // object continuation pages. Only requested, validated extents may clear.
        out.bits.fill(u64::MAX);
        let Some(heap_end) = heap_base.checked_add(heap_size) else {
            disarm();
            return false;
        };
        for range in older_regions {
            if range.start < heap_base || range.end > heap_end || range.end <= range.start {
                disarm();
                return false;
            }
            for page in ((range.start - heap_base) >> shift)..((range.end - heap_base) >> shift) {
                out.bits[page / 64] &= !(1u64 << (page % 64));
            }
        }
        let ok = crate::gc_async_wp::read(
            (heap_base, heap_size, crate::gc::heap_identity_epoch()),
            older_regions,
            |start, end| {
                for page in ((start - heap_base) >> shift)..((end - heap_base) >> shift) {
                    out.bits[page / 64] |= 1u64 << (page % 64);
                }
            },
        );
        if !ok {
            disarm();
        }
        return ok;
    }
    #[cfg(target_os = "linux")]
    {
        let shift = page_shift();
        out.reset(heap_base, heap_size, shift);
        let Some(mut guard) = imp::lock_tracker() else {
            disarm();
            return false;
        };
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
        let _ = (ranges, older_regions, out);
        false
    }
}

/// Clear every soft-dirty bit and arm the tracker for this heap, when an old
/// generation of `old_bytes` makes that worthwhile against a nursery of
/// `nursery_bytes`. Call with the world stopped, after the collector's last
/// write that could leave an old object referring into the nursery — i.e.
/// once the nursery is empty.
pub(crate) fn rearm(
    heap_base: usize,
    heap_size: usize,
    old_bytes: usize,
    nursery_bytes: usize,
    older_regions: &[crate::gc_async_wp::Range],
) {
    // Do not destroy a reusable async context between successful minor cycles.
    // External invalidation (major, heap replacement) still drops it via disarm.
    ARMED_BASE.store(0, Ordering::Release);
    ACTIVE_ASYNC.store(false, Ordering::Release);
    if !worthwhile(old_bytes, nursery_bytes) {
        crate::gc_async_wp::invalidate();
        return;
    }
    // A forked child must not block on an inherited async-context mutex or
    // issue an ioctl through its parent's MM. Stay fully unarmed until safe.
    if !crate::gc_async_wp::process_ready() {
        return;
    }
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let disabled =
        *DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_ASYNC_OLD_TRACKING").is_some());
    // Explicit On preserves the forced-legacy contract; only Auto selects async.
    if prefer_async(mode(), disabled) {
        match crate::gc_async_wp::rearm(
            (heap_base, heap_size, crate::gc::heap_identity_epoch()),
            older_regions,
        ) {
            crate::gc_async_wp::ArmResult::Armed => {
                ACTIVE_ASYNC.store(true, Ordering::Release);
                ARMED_SIZE.store(heap_size, Ordering::Release);
                ARMED_BASE.store(heap_base, Ordering::Release);
                return;
            }
            // Partial arming is unknown: the next minor must scan everything.
            crate::gc_async_wp::ArmResult::Failed => {
                disarm();
                return;
            }
            crate::gc_async_wp::ArmResult::Unavailable => {}
        }
    }
    crate::gc_async_wp::invalidate();
    #[cfg(target_os = "linux")]
    {
        let Some(mut guard) = imp::lock_tracker() else {
            disarm();
            return;
        };
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
    let _ = (heap_base, heap_size, older_regions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "isolated Linux UFFD/fork/heap replacement hardgates; run explicitly"]
    fn async_failure_fork_and_restore_hardgates() {
        const CASE: &str = "EGCL_ASYNC_HARDGATE_CASE";
        const TEST: &str = "gc_soft_dirty::tests::async_failure_fork_and_restore_hardgates";
        let Ok(case) = std::env::var(CASE) else {
            for case in ["read", "rearm", "restore", "fork", "poison"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        TEST,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(CASE, case)
                    .env("EGCL_GC_ASYNC_WP_LOG", "1")
                    .env_remove("EGCL_DISABLE_ASYNC_OLD_TRACKING");
                for (key, _) in
                    std::env::vars().filter(|(key, _)| key.starts_with("EGCL_GC_STRESS"))
                {
                    command.env_remove(key);
                }
                if matches!(case, "fork" | "poison") {
                    command.env("EGCL_GC_SOFT_DIRTY", "1");
                } else {
                    command.env_remove("EGCL_GC_SOFT_DIRTY");
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "case {case}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        if case == "fork" {
            crate::gc_async_wp::with_locked_parent_context(|range, unlock_context| {
                let mut legacy = imp::TRACKER.lock().unwrap();
                let tracker = legacy.get_or_insert_with(|| imp::Tracker {
                    files: None,
                    buf: Vec::new(),
                    unsupported: false,
                });
                assert!(
                    imp::files(tracker, page_shift()).is_some(),
                    "parent legacy Files must be real"
                );
                let pid = unsafe { libc::fork() };
                assert!(pid >= 0);
                if pid == 0 {
                    let mut pages = DirtyPages::default();
                    let size = range.end - range.start;
                    ARMED_BASE.store(range.start, Ordering::Release);
                    ARMED_SIZE.store(size, Ordering::Release);
                    ACTIVE_ASYNC.store(true, Ordering::Release);
                    let async_unknown = !read_dirty_pages(
                        range.start,
                        size,
                        &[(range.start, range.end)],
                        &[range],
                        &mut pages,
                    );
                    let async_unarmed = ARMED_BASE.load(Ordering::Acquire) == 0
                        && !ACTIVE_ASYNC.load(Ordering::Acquire);
                    // Select the inherited legacy state explicitly: it must not
                    // block on the parent-held mutex or inspect parent Files.
                    ARMED_BASE.store(range.start, Ordering::Release);
                    let legacy_unknown = !read_dirty_pages(
                        range.start,
                        size,
                        &[(range.start, range.end)],
                        &[range],
                        &mut pages,
                    );
                    unlock_context();
                    // Now own-context setup is possible, but the legacy mutex
                    // is still inherited locked: forced-legacy rearm fails closed.
                    rearm(range.start, size, size, size, &[range]);
                    let rearm_unknown = ARMED_BASE.load(Ordering::Acquire) == 0
                        && !ACTIVE_ASYNC.load(Ordering::Acquire);
                    unsafe {
                        libc::_exit(
                            if async_unknown && async_unarmed && legacy_unknown && rearm_unknown {
                                0
                            } else {
                                31
                            },
                        );
                    }
                }
                struct Child {
                    pid: i32,
                    alive: bool,
                }
                impl Drop for Child {
                    fn drop(&mut self) {
                        if self.alive {
                            unsafe {
                                libc::kill(self.pid, libc::SIGKILL);
                                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
                            }
                        }
                    }
                }
                let mut child = Child { pid, alive: true };
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                let mut status = 0;
                loop {
                    let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                    if result == pid {
                        child.alive = false;
                        break;
                    }
                    assert!(result >= 0);
                    if std::time::Instant::now() >= deadline {
                        panic!("fork child blocked on inherited tracking mutex");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                assert!(
                    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                    "child status {status}"
                );
            });
            return;
        }
        if case == "poison" {
            let _ = std::panic::catch_unwind(|| {
                let _guard = imp::TRACKER.lock().unwrap();
                panic!("intentional legacy tracker poison");
            });
            ARMED_BASE.store(4096, Ordering::Release);
            ARMED_SIZE.store(4096, Ordering::Release);
            let mut pages = DirtyPages::default();
            assert!(!read_dirty_pages(
                4096,
                4096,
                &[(4096, 8192)],
                &[],
                &mut pages
            ));
            rearm(4096, 4096, 4096, 4096, &[]);
            assert_eq!(ARMED_BASE.load(Ordering::Acquire), 0);
            return;
        }
        crate::gc::init_heap(&crate::gc::GcConfig {
            heap_size: 2 * 1024 * 1024,
            heap_max: 2 * 1024 * 1024,
            nursery_size: 32 * 1024,
            tlab_size: 256,
            region_size: 4096,
            promotion_threshold: 15,
            pause_target_ms: 10,
            gc_workers: 1,
            satb_buffer_size: 32,
            old_occupancy_trigger: 0.9,
        })
        .unwrap();
        fn cons(marker: i64) -> crate::value::EgclVal {
            let body = crate::gc::alloc_typed(16, crate::object::type_id::CONS).unwrap();
            unsafe {
                body.cast::<crate::value::EgclVal>()
                    .write(crate::value::EgclVal::from_fixnum(marker));
                body.cast::<crate::value::EgclVal>()
                    .add(1)
                    .write(crate::value::NIL);
                crate::value::EgclVal::from_cons_ptr(body)
            }
        }
        if case == "restore" {
            let heap_base = crate::gc::heap_base_address();
            let identity = crate::gc::heap_identity_epoch();
            let mut image = Vec::<u8>::new();
            {
                crate::rooted!(roots = Vec::<crate::value::EgclVal>::new());
                for i in 0..800 {
                    roots.push(cons(i));
                }
                crate::gc::collect_t0_minor().unwrap();
                assert!(
                    ACTIVE_ASYNC.load(Ordering::Acquire),
                    "real async arm required before restore"
                );
                crate::gc::walk_heap(|body, kind, size| {
                    image.extend_from_slice(&(body as usize as u64).to_le_bytes());
                    image.push(kind);
                    image.extend_from_slice(&(size as u32).to_le_bytes());
                    image.extend_from_slice(unsafe { std::slice::from_raw_parts(body, size) });
                    true
                })
                .unwrap();
            }
            crate::gc::restore_heap(&image).unwrap();
            assert_eq!(
                crate::gc::heap_base_address(),
                heap_base,
                "same-address heap replacement"
            );
            assert_ne!(crate::gc::heap_identity_epoch(), identity);
            assert_eq!(
                ARMED_BASE.load(Ordering::Acquire),
                0,
                "image invalidates prior armed coverage"
            );
            assert!(!ACTIVE_ASYNC.load(Ordering::Acquire));
            let restored_minor_count = crate::gc::heap_stats().minor_gc_count;
            let mut holder = crate::value::NIL;
            let mut restored_pages = std::collections::HashSet::new();
            crate::gc::walk_heap(|body, kind, size| {
                restored_pages.insert(body as usize & !4095usize);
                if holder.is_nil() && kind == crate::object::type_id::CONS && size == 16 {
                    holder = unsafe { crate::value::EgclVal::from_cons_ptr(body as *mut u8) };
                }
                true
            })
            .unwrap();
            assert!(holder.is_cons());
            crate::rooted!(holder = holder);
            for (ordinal, marker) in [910, 911].into_iter().enumerate() {
                // Image objects are restored pinned in Nursery. A referent in
                // the same region would be retained in place, proving nothing
                // about relocation. Fill its tail until the candidate is in a
                // different region, without collecting before the first store.
                let before = {
                    crate::rooted!(young = crate::value::NIL);
                    for _ in 0..512 {
                        *young = cons(marker);
                        if !restored_pages.contains(&(young.to_raw() as usize & !4095usize)) {
                            break;
                        }
                    }
                    assert!(
                        !restored_pages.contains(&(young.to_raw() as usize & !4095usize)),
                        "fresh referent must be outside every image-pinned region"
                    );
                    if ordinal == 0 {
                        assert_eq!(
                            crate::gc::heap_stats().minor_gc_count,
                            restored_minor_count,
                            "candidate selection must not collect before the first post-restore store"
                        );
                        assert_eq!(
                            ARMED_BASE.load(Ordering::Acquire),
                            0,
                            "first post-restore collection must still be unarmed"
                        );
                    }
                    unsafe {
                        (*holder)
                            .as_ptr()
                            .cast::<crate::value::EgclVal>()
                            .write(*young);
                    }
                    young.to_raw()
                };
                crate::gc::collect_t0_minor().unwrap();
                let value = unsafe { (*holder).as_ptr().cast::<crate::value::EgclVal>().read() };
                assert_ne!(
                    value.to_raw(),
                    before,
                    "restored sole young referent must move"
                );
                assert_eq!(
                    unsafe {
                        value
                            .as_ptr()
                            .cast::<crate::value::EgclVal>()
                            .read()
                            .as_fixnum()
                    },
                    marker
                );
                assert!(
                    ACTIVE_ASYNC.load(Ordering::Acquire),
                    "restored anonymous heap re-arms async after its full scan"
                );
            }
            return;
        }
        crate::rooted!(roots = Vec::<crate::value::EgclVal>::new());
        for i in 0..800 {
            roots.push(cons(i));
        }
        crate::gc::collect_t0_minor().unwrap();
        assert!(
            ACTIVE_ASYNC.load(Ordering::Acquire),
            "real async context required before failure injection"
        );
        crate::gc_async_wp::inject_after_chunk(case == "rearm");
        let before = {
            crate::rooted!(first = cons(920));
            crate::rooted!(last = cons(921));
            unsafe {
                roots[0]
                    .as_ptr()
                    .cast::<crate::value::EgclVal>()
                    .write(*first);
                roots
                    .last()
                    .unwrap()
                    .as_ptr()
                    .cast::<crate::value::EgclVal>()
                    .write(*last);
            }
            ((*first).to_raw(), (*last).to_raw())
        };
        let before = [before.0, before.1];
        crate::gc::collect_t0_minor().unwrap();
        assert_eq!(
            crate::gc_async_wp::injected_chunks(),
            (1, 1),
            "failure must occur after exactly one successful real chunk"
        );
        if case == "rearm" {
            assert_eq!(
                ARMED_BASE.load(Ordering::Acquire),
                0,
                "partial rearm leaves next minor fully unarmed"
            );
            let before2 = {
                crate::rooted!(first = cons(922));
                crate::rooted!(last = cons(923));
                unsafe {
                    roots[0]
                        .as_ptr()
                        .cast::<crate::value::EgclVal>()
                        .write(*first);
                    roots
                        .last()
                        .unwrap()
                        .as_ptr()
                        .cast::<crate::value::EgclVal>()
                        .write(*last);
                }
                [(*first).to_raw(), (*last).to_raw()]
            };
            crate::gc::collect_t0_minor().unwrap();
            for ((holder, old), marker) in [roots[0], *roots.last().unwrap()]
                .into_iter()
                .zip(before2)
                .zip([922, 923])
            {
                let value = unsafe { holder.as_ptr().cast::<crate::value::EgclVal>().read() };
                assert_ne!(value.to_raw(), old);
                assert_eq!(
                    unsafe {
                        value
                            .as_ptr()
                            .cast::<crate::value::EgclVal>()
                            .read()
                            .as_fixnum()
                    },
                    marker
                );
            }
        } else {
            for ((holder, old), marker) in [roots[0], *roots.last().unwrap()]
                .into_iter()
                .zip(before)
                .zip([920, 921])
            {
                let value = unsafe { holder.as_ptr().cast::<crate::value::EgclVal>().read() };
                assert_ne!(
                    value.to_raw(),
                    old,
                    "partial read must fall back over BOTH successful and unvisited older ranges"
                );
                assert_eq!(
                    unsafe {
                        value
                            .as_ptr()
                            .cast::<crate::value::EgclVal>()
                            .read()
                            .as_fixnum()
                    },
                    marker
                );
            }
        }
    }

    #[test]
    fn async_selection_preserves_off_and_forced_legacy_contracts() {
        assert!(!prefer_async(Mode::Off, false));
        assert!(!prefer_async(Mode::On, false));
        assert!(!prefer_async(Mode::Auto, true));
        assert!(prefer_async(Mode::Auto, false));
    }

    #[test]
    fn async_unknown_pages_default_dirty_including_large_object_tails() {
        let base = 0x10_0000;
        let mut pages = DirtyPages::default();
        pages.reset(base, 4 * 4096, 12);
        pages.bits.fill(u64::MAX);
        // One explicitly covered clean page; the untracked tail stays dirty.
        pages.bits[0] &= !(1 << 0);
        assert!(!pages.is_dirty(base));
        assert!(pages.is_dirty(base + 4096));
        assert!(pages.any_dirty(base + 3 * 4096, base + 4 * 4096));
    }

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
