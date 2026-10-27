// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Optional older-region-only Linux async write tracking. This module never
//! handles faults in userspace. A failed/unknown observation means full dirty.
//! Call only with mutators stopped. Nonempty older regions cannot be recycled
//! without `invalidate` (major collection/heap replacement); `kind` distinguishes
//! region transitions within an incarnation. Heap identity is checked separately.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Range {
    pub start: usize,
    pub end: usize,
    pub kind: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum ArmResult {
    Armed,
    Unavailable,
    Failed,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{ArmResult, Range};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};

    const WP_ASYNC: u64 = 1 << 15;
    const WP_UNPOPULATED: u64 = 1 << 13;
    const WP_ALLOWED: u64 = 1;
    const WRITTEN: u64 = 2;
    const WP_MATCHING: u64 = 1;
    const CHECK_ASYNC: u64 = 2;

    #[repr(C)]
    struct Api {
        api: u64,
        features: u64,
        ioctls: u64,
    }
    #[repr(C)]
    struct Register {
        start: u64,
        len: u64,
        mode: u64,
        ioctls: u64,
    }
    #[derive(Clone, Copy, Default)]
    #[repr(C)]
    struct PageRegion {
        start: u64,
        end: u64,
        categories: u64,
    }
    #[derive(Default)]
    #[repr(C)]
    struct Scan {
        size: u64,
        flags: u64,
        start: u64,
        end: u64,
        walk_end: u64,
        vec: u64,
        vec_len: u64,
        max_pages: u64,
        category_inverted: u64,
        category_mask: u64,
        category_anyof_mask: u64,
        return_mask: u64,
    }

    // libc supplies target-specific syscall/ioctl numbers. The default x86-64
    // build uses the existing direct syscall wrappers, not libc's syscall ABI.
    fn ioctl<T>(fd: i32, request: usize, value: &mut T) -> Result<usize, i32> {
        #[cfg(target_arch = "x86_64")]
        {
            crate::syscall::check(unsafe {
                crate::syscall::syscall3(
                    libc::SYS_ioctl as usize,
                    fd as usize,
                    request,
                    value as *mut T as usize,
                )
            })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let result = unsafe { libc::ioctl(fd, request as _, value as *mut T) };
            if result < 0 {
                Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(5))
            } else {
                Ok(result as usize)
            }
        }
    }

    fn userfaultfd() -> Result<OwnedFd, i32> {
        let flags = libc::O_CLOEXEC | libc::O_NONBLOCK | 1; // UFFD_USER_MODE_ONLY
        #[cfg(target_arch = "x86_64")]
        let fd = crate::syscall::check(unsafe {
            crate::syscall::syscall1(libc::SYS_userfaultfd as usize, flags as usize)
        })? as i32;
        #[cfg(not(target_arch = "x86_64"))]
        let fd = {
            let result = unsafe { libc::syscall(libc::SYS_userfaultfd, flags) };
            if result < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(5));
            }
            result as i32
        };
        // SAFETY: newly opened, uniquely owned FD. Drop only closes; in a child
        // never issue UNREGISTER through an inherited parent context.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    struct Context {
        fd: OwnedFd,
        pagemap: File,
        pid: u32,
        heap: (usize, usize, u64),
        coverage: Vec<Range>,
        armed: bool,
    }

    fn valid_range(range: Range, heap: (usize, usize, u64), page: usize) -> bool {
        let Some(heap_end) = heap.0.checked_add(heap.1) else {
            return false;
        };
        range.start < range.end
            && range.start >= heap.0
            && range.end <= heap_end
            && range.start % page == 0
            && range.end % page == 0
    }

    /// Validate partial-progress results before trusting or publishing any bit.
    fn validate_chunk(start: u64, end: u64, next: u64, regions: &[PageRegion], page: u64) -> bool {
        if next <= start || next > end || next % page != 0 {
            return false;
        }
        let mut previous = start;
        for region in regions {
            if region.start < previous
                || region.start >= region.end
                || region.end > next
                || region.start % page != 0
                || region.end % page != 0
                || region.categories & WRITTEN == 0
            {
                return false;
            }
            previous = region.end;
        }
        true
    }

    fn same_owner(
        saved_pid: u32,
        saved_heap: (usize, usize, u64),
        pid: u32,
        heap: (usize, usize, u64),
    ) -> bool {
        saved_pid == pid && saved_heap == heap
    }

    fn covered(coverage: &[Range], range: Range) -> bool {
        coverage.contains(&range)
    }

    #[cfg(test)]
    thread_local! {
        static FAIL_CHUNKS: std::cell::Cell<Option<(bool, usize)>> = const { std::cell::Cell::new(None) };
        static FAILURE_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static SUCCESSFUL_CHUNKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    impl Context {
        fn open(heap: (usize, usize, u64)) -> Result<Self, i32> {
            let fd = userfaultfd()?;
            let mut api = Api {
                api: 0xaa,
                features: WP_ASYNC | WP_UNPOPULATED,
                ioctls: 0,
            };
            ioctl(
                fd.as_raw_fd(),
                libc::_IOWR::<Api>(0xaa, 0x3f) as usize,
                &mut api,
            )?;
            if api.features & (WP_ASYNC | WP_UNPOPULATED) != WP_ASYNC | WP_UNPOPULATED
                || api.ioctls & 1 == 0
            {
                return Err(95);
            }
            let pagemap =
                File::open("/proc/self/pagemap").map_err(|e| e.raw_os_error().unwrap_or(5))?;
            Ok(Self {
                fd,
                pagemap,
                pid: std::process::id(),
                heap,
                coverage: Vec::new(),
                armed: false,
            })
        }

        fn register(&self, range: Range) -> Result<(), i32> {
            let mut arg = Register {
                start: range.start as u64,
                len: (range.end - range.start) as u64,
                mode: 2,
                ioctls: 0,
            };
            ioctl(
                self.fd.as_raw_fd(),
                libc::_IOWR::<Register>(0xaa, 0) as usize,
                &mut arg,
            )?;
            Ok(())
        }

        fn scan(
            &self,
            range: Range,
            arm: bool,
            mut written: impl FnMut(usize, usize),
        ) -> Result<(), i32> {
            let mut cursor = range.start as u64;
            let end = range.end as u64;
            let page = crate::syscall::page_size() as u64;
            while cursor < end {
                #[cfg(test)]
                if FAIL_CHUNKS.with(|slot| match slot.get() {
                    Some((operation, 0)) if operation == arm => {
                        slot.set(None);
                        FAILURE_HITS.with(|hits| hits.set(hits.get() + 1));
                        true
                    }
                    _ => false,
                }) {
                    return Err(5);
                }
                let mut regions = [PageRegion::default(); 32];
                let mut arg = Scan {
                    size: std::mem::size_of::<Scan>() as u64,
                    flags: CHECK_ASYNC | if arm { WP_MATCHING } else { 0 },
                    start: cursor,
                    end,
                    vec: if arm { 0 } else { regions.as_mut_ptr() as u64 },
                    vec_len: if arm { 0 } else { regions.len() as u64 },
                    category_mask: if arm { WP_ALLOWED } else { WRITTEN },
                    return_mask: WRITTEN,
                    ..Default::default()
                };
                let count = ioctl(
                    self.pagemap.as_raw_fd(),
                    libc::_IOWR::<Scan>(b'f' as u32, 16) as usize,
                    &mut arg,
                )?;
                if count > regions.len()
                    || (arm && count != 0)
                    || !validate_chunk(cursor, end, arg.walk_end, &regions[..count], page)
                {
                    return Err(5);
                }
                for region in &regions[..count] {
                    written(region.start as usize, region.end as usize);
                }
                #[cfg(test)]
                FAIL_CHUNKS.with(|slot| {
                    if let Some((operation, remaining)) = slot.get() {
                        if operation == arm && remaining > 0 {
                            slot.set(Some((operation, remaining - 1)));
                            SUCCESSFUL_CHUNKS.with(|count| count.set(count.get() + 1));
                        }
                    }
                });
                cursor = arg.walk_end;
            }
            Ok(())
        }
    }

    static UNSUPPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static CONTEXT: Mutex<Option<Context>> = Mutex::new(None);
    static OWNER_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn lock_context() -> Option<MutexGuard<'static, Option<Context>>> {
        match CONTEXT.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub(super) fn process_ready() -> bool {
        let pid = std::process::id();
        let owner = OWNER_PID.load(std::sync::atomic::Ordering::Acquire);
        if owner == pid {
            return true;
        }
        // Never wait for a mutex inherited locked by a vanished parent thread.
        // Until it is available the child remains unarmed and scans everything.
        let Some(mut guard) = lock_context() else {
            return false;
        };
        *guard = None; // close only; never unregister the parent's MM
        OWNER_PID.store(pid, std::sync::atomic::Ordering::Release);
        true
    }

    fn log(message: &[u8]) {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        if *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_ASYNC_WP_LOG").is_some()) {
            crate::syscall::dbg_write(message);
        }
    }

    pub(super) fn invalidate() {
        // Closing the child's inherited FD never unregisters the parent's MM.
        if let Some(mut guard) = lock_context() {
            *guard = None;
        }
    }

    pub(super) fn rearm(heap: (usize, usize, u64), ranges: &[Range]) -> ArmResult {
        if UNSUPPORTED.load(std::sync::atomic::Ordering::Relaxed) {
            return ArmResult::Unavailable;
        }
        let page = crate::syscall::page_size();
        if ranges.is_empty()
            || ranges.iter().any(|&r| !valid_range(r, heap, page))
            || ranges.windows(2).any(|pair| pair[0].end > pair[1].start)
        {
            invalidate();
            return ArmResult::Unavailable;
        }
        if !process_ready() {
            return ArmResult::Failed;
        }
        let Some(mut guard) = lock_context() else {
            return ArmResult::Failed;
        };
        // A removed/changed region or heap/PID change invalidates its complete
        // incarnation. Close only; do not UNREGISTER an inherited parent FD.
        let replace = guard.as_ref().is_some_and(|ctx| {
            !same_owner(ctx.pid, ctx.heap, std::process::id(), heap)
                || ctx.coverage.iter().any(|r| !ranges.contains(r))
        });
        if replace {
            *guard = None;
        }
        if guard.is_none() {
            let Ok(context) = Context::open(heap) else {
                UNSUPPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
                log(b"[gc-async-wp] unsupported; full/legacy fallback\n");
                return ArmResult::Unavailable;
            };
            *guard = Some(context);
        }
        let context = guard.as_mut().unwrap();
        context.armed = false;
        let result = (|| {
            for &range in ranges {
                if !covered(&context.coverage, range) {
                    context.register(range)?;
                }
                context.scan(range, true, |_, _| {})?;
            }
            Ok::<(), i32>(())
        })();
        if result.is_err() {
            *guard = None;
            UNSUPPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
            log(b"[gc-async-wp] rearm failed; full scan\n");
            return ArmResult::Failed;
        }
        context.coverage.clear();
        context.coverage.extend_from_slice(ranges);
        context.armed = true;
        log(b"[gc-async-wp] armed older ranges\n");
        ArmResult::Armed
    }

    pub(super) fn read(
        heap: (usize, usize, u64),
        ranges: &[Range],
        mut dirty: impl FnMut(usize, usize),
    ) -> bool {
        if !process_ready() {
            return false;
        }
        let Some(mut guard) = lock_context() else {
            return false;
        };
        let Some(context) = guard.as_mut() else {
            return false;
        };
        if !same_owner(context.pid, context.heap, std::process::id(), heap) || !context.armed {
            *guard = None;
            return false;
        }
        for &range in ranges {
            if !valid_range(range, heap, crate::syscall::page_size()) {
                *guard = None;
                return false;
            }
            if covered(&context.coverage, range) {
                if context.scan(range, false, &mut dirty).is_err() {
                    *guard = None;
                    UNSUPPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
                    log(b"[gc-async-wp] read failed; full scan\n");
                    return false;
                }
            } else {
                // No successful arm for this region incarnation: every page
                // must be visited, even if a same-address kernel bit reads clean.
                dirty(range.start, range.end);
            }
        }
        log(b"[gc-async-wp] snapshot older ranges\n");
        true
    }

    #[cfg(test)]
    pub(super) fn inject_after_chunk(arm: bool) {
        FAIL_CHUNKS.with(|slot| slot.set(Some((arm, 1))));
        FAILURE_HITS.with(|hits| hits.set(0));
        SUCCESSFUL_CHUNKS.with(|count| count.set(0));
    }
    #[cfg(test)]
    pub(super) fn injected_chunks() -> (usize, usize) {
        (
            SUCCESSFUL_CHUNKS.with(std::cell::Cell::get),
            FAILURE_HITS.with(std::cell::Cell::get),
        )
    }

    #[cfg(test)]
    pub(super) fn with_locked_parent_context(body: impl FnOnce(Range, &mut dyn FnMut())) {
        let size = crate::syscall::page_size();
        let p =
            unsafe { crate::syscall::mmap(std::ptr::null_mut(), size, 3, 0x22, -1, 0) }.unwrap();
        struct Mapping(*mut u8, usize);
        impl Drop for Mapping {
            fn drop(&mut self) {
                let _ = unsafe { crate::syscall::munmap(self.0, self.1) };
            }
        }
        let _mapping = Mapping(p, size);
        unsafe {
            p.write_volatile(1);
        }
        let range = Range {
            start: p as usize,
            end: p as usize + size,
            kind: 1,
        };
        let mut context = Context::open((range.start, size, 1)).unwrap();
        context.register(range).unwrap();
        context.scan(range, true, |_, _| {}).unwrap();
        context.coverage.push(range);
        context.armed = true;
        OWNER_PID.store(std::process::id(), std::sync::atomic::Ordering::Release);
        let mut guard = Some(CONTEXT.lock().unwrap());
        **guard.as_mut().unwrap() = Some(context);
        // Only the forked child calls unlock: it releases its own copied mutex
        // guard, then drops its inherited FD through process_ready (no ioctl).
        body(range, &mut || {
            drop(guard.take());
        });
        let context = guard.as_ref().unwrap().as_ref().unwrap();
        let mut count = 0;
        context.scan(range, false, |_, _| count += 1).unwrap();
        assert_eq!(count, 0, "child must not alter parent page protection");
        unsafe {
            p.write_volatile(2);
        }
        context.scan(range, false, |_, _| count += 1).unwrap();
        assert_eq!(count, 1, "parent registration must survive child cleanup");
        **guard.as_mut().unwrap() = None;
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn async_wp_rejects_partial_invalid_progress_and_uncovered_ranges() {
            let region = PageRegion {
                start: 4096,
                end: 8192,
                categories: WRITTEN,
            };
            assert!(validate_chunk(4096, 12288, 8192, &[region], 4096));
            assert!(!validate_chunk(4096, 12288, 4096, &[], 4096));
            assert!(!validate_chunk(4096, 12288, 16384, &[], 4096));
            assert!(!validate_chunk(4096, 12288, 8193, &[], 4096));
            assert!(!validate_chunk(
                4096,
                12288,
                8192,
                &[PageRegion {
                    end: 12288,
                    ..region
                }],
                4096
            ));
            assert!(!valid_range(
                Range {
                    start: usize::MAX - 4095,
                    end: usize::MAX,
                    kind: 1
                },
                (4096, usize::MAX, 1),
                4096
            ));
        }

        #[test]
        fn async_wp_region_kind_extent_and_heap_identity_are_not_interchangeable() {
            let old = Range {
                start: 4096,
                end: 8192,
                kind: 1,
            };
            assert!(same_owner(7, (4096, 8192, 1), 7, (4096, 8192, 1)));
            assert!(!same_owner(7, (4096, 8192, 1), 8, (4096, 8192, 1)));
            assert!(!same_owner(7, (4096, 8192, 1), 7, (4096, 8192, 2)));
            assert!(covered(&[old], old));
            assert!(!covered(&[], old));
            assert!(!covered(&[old], Range { kind: 2, ..old }));
            assert!(!covered(&[old], Range { end: 12288, ..old }));
            assert!(!valid_range(old, (8192, 8192, 1), 4096));
            assert!(!valid_range(
                Range { start: 4097, ..old },
                (4096, 8192, 1),
                4096
            ));
        }

        #[test]
        fn async_wp_failed_registration_and_snapshot_report_failure() {
            // A real FD which cannot implement either ioctl exercises errors
            // without changing a mapping or depending on kernel UFFD support.
            let context = Context {
                fd: File::open("/dev/null").unwrap().into(),
                pagemap: File::open("/dev/null").unwrap(),
                pid: std::process::id(),
                heap: (4096, 8192, 1),
                coverage: Vec::new(),
                armed: true,
            };
            let range = Range {
                start: 4096,
                end: 8192,
                kind: 1,
            };
            assert!(context.register(range).is_err());
            let mut visits = 0;
            assert!(context.scan(range, false, |_, _| visits += 1).is_err());
            assert_eq!(visits, 0);
        }

        #[test]
        #[ignore = "requires Linux async userfaultfd capability; run explicitly"]
        fn async_wp_actual_page_user_and_kernel_writes() {
            let size = crate::syscall::page_size();
            let p = unsafe { crate::syscall::mmap(std::ptr::null_mut(), size, 3, 0x22, -1, 0) }
                .unwrap();
            struct Mapping(*mut u8, usize);
            impl Drop for Mapping {
                fn drop(&mut self) {
                    let _ = unsafe { crate::syscall::munmap(self.0, self.1) };
                }
            }
            let _mapping = Mapping(p, size);
            unsafe {
                p.write_volatile(1);
            }
            let range = Range {
                start: p as usize,
                end: p as usize + size,
                kind: 1,
            };
            let context =
                Context::open((range.start, size, 1)).expect("required positive capability probe");
            context.register(range).unwrap();
            context.scan(range, true, |_, _| {}).unwrap();
            let mut count = 0;
            context.scan(range, false, |_, _| count += 1).unwrap();
            assert_eq!(count, 0);
            unsafe {
                p.write_volatile(2);
            }
            context
                .scan(range, false, |start, end| {
                    assert_eq!((start, end), (range.start, range.end));
                    count += 1;
                })
                .unwrap();
            assert_eq!(count, 1);
            context.scan(range, true, |_, _| {}).unwrap();
            let mut pipe = [0; 2];
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
            let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            assert_eq!(
                unsafe { libc::write(write.as_raw_fd(), [3u8].as_ptr().cast(), 1) },
                1
            );
            assert_eq!(unsafe { libc::read(read.as_raw_fd(), p.cast(), 1) }, 1);
            assert_eq!(unsafe { p.read_volatile() }, 3);
            count = 0;
            context.scan(range, false, |_, _| count += 1).unwrap();
            assert_eq!(count, 1);
            drop(context); // Unregister before unmapping, including on failure.
        }
    }
}

pub(crate) fn invalidate() {
    #[cfg(target_os = "linux")]
    imp::invalidate();
}
pub(crate) fn rearm(heap: (usize, usize, u64), ranges: &[Range]) -> ArmResult {
    #[cfg(target_os = "linux")]
    {
        imp::rearm(heap, ranges)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (heap, ranges);
        ArmResult::Unavailable
    }
}
pub(crate) fn read(
    heap: (usize, usize, u64),
    ranges: &[Range],
    dirty: impl FnMut(usize, usize),
) -> bool {
    #[cfg(target_os = "linux")]
    {
        imp::read(heap, ranges, dirty)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (heap, ranges, dirty);
        false
    }
}

pub(crate) fn process_ready() -> bool {
    #[cfg(target_os = "linux")]
    {
        imp::process_ready()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn inject_after_chunk(arm: bool) {
    imp::inject_after_chunk(arm);
}
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn injected_chunks() -> (usize, usize) {
    imp::injected_chunks()
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn with_locked_parent_context(body: impl FnOnce(Range, &mut dyn FnMut())) {
    imp::with_locked_parent_context(body);
}
