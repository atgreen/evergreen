// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Evergreen Common Lisp — entry point.

use egcl::cli;

/// Account at the caller-facing allocator boundary. Inner allocators may use
/// alloc/dealloc to implement realloc or zeroing; those internal operations
/// must not count as additional live Rust requests.
struct HostTracked<A, R = HostAccounting>(A, R);

trait AllocationAccounting {
    fn alloc(&self, size: usize);
    fn dealloc(&self, size: usize);
    fn resize(&self, old: usize, new: usize);
}

struct HostAccounting;

impl AllocationAccounting for HostAccounting {
    #[inline]
    fn alloc(&self, size: usize) {
        egcl_rt::host_memory::record_alloc(size);
    }

    #[inline]
    fn dealloc(&self, size: usize) {
        egcl_rt::host_memory::record_dealloc(size);
    }

    #[inline]
    fn resize(&self, old: usize, new: usize) {
        egcl_rt::host_memory::record_resize(old, new);
    }
}

unsafe impl<A: std::alloc::GlobalAlloc, R: AllocationAccounting> std::alloc::GlobalAlloc
    for HostTracked<A, R>
{
    #[inline]
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc(layout) };
        if !pointer.is_null() {
            self.1.alloc(layout.size());
        }
        pointer
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc_zeroed(layout) };
        if !pointer.is_null() {
            self.1.alloc(layout.size());
        }
        pointer
    }

    #[inline]
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        unsafe { self.0.dealloc(pointer, layout) };
        self.1.dealloc(layout.size());
    }

    #[inline]
    unsafe fn realloc(
        &self,
        pointer: *mut u8,
        layout: std::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        let resized = unsafe { self.0.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            self.1.resize(layout.size(), new_size);
        }
        resized
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]
mod short_copy;

/// A per-thread free-list cache in front of the system allocator.
///
/// musl's `mallocng` has no equivalent of glibc's `tcache`, and that single
/// difference is the largest part of the gap between the two on this workload.
/// Same source, same benchmark (babel cold load + 10 no-op reloads):
///
/// ```text
///   glibc default                13.95 s
///   glibc tcache=0               17.99 s   (+4.04)
///   glibc tcache=0, arena_max=1  19.08 s   (+5.13)
///   musl mallocng                21.54 s   (+7.59)
/// ```
///
/// So ~53% of the gap is the thread cache, ~14% arena count, the rest
/// mallocng's per-operation cost. About 40% of the mutator's profile on a babel
/// load is `__lock`/`__unlock`/`__libc_malloc_impl`/`__libc_free` (bliss-05as).
///
/// Two wholesale replacements were tried and rejected first: mimalloc cannot be
/// built for musl here (no musl C toolchain), and dlmalloc — pure Rust, so it
/// builds — measured 8% SLOWER than mallocng, which is consistent with the
/// finding above since dlmalloc has one global lock and no thread cache.
///
/// Design notes:
///
/// * Blocks are cached by SIZE CLASS and are always allocated and freed from
///   `System` using the class layout, never the caller's layout. Rust's
///   `GlobalAlloc` contract requires deallocating with the same layout used to
///   allocate, so normalising both ends keeps that honest.
/// * A cached block is just a `System` block, so a block allocated on one
///   thread and freed on another is fine: it lands in the freeing thread's
///   cache and is reused there, or returned to `System`.
/// * The thread-local is `const`-initialised and has no `Drop`. A `Drop` impl
///   would register a Rust TLS destructor, and registration itself can
///   allocate — re-entering this allocator while it is mid-call.
/// * A dying thread still returns its cached blocks, through a POSIX
///   thread-specific-data destructor instead (bliss-orayb). musl's
///   `pthread_key_create` and `pthread_setspecific` use fixed tables and never
///   call `malloc`, so arming the destructor from inside the allocator cannot
///   re-enter it. Without this, every exited thread kept up to
///   `CLASSES * DEPTH * MAX_SIZE` (~270 KB) of free blocks forever: measured
///   at about 88 KB per joined thread on a thread-per-task workload whose
///   threads promote functions to T1, against ~8 KB with T1 disabled.
#[cfg(all(
    feature = "thread-cache-alloc",
    target_env = "musl",
    not(feature = "alloc-count")
))]
mod tcache {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::UnsafeCell;

    /// Largest block served from the cache. Above this, straight to `System`.
    const MAX_SIZE: usize = 512;
    /// Size-class granularity; also the alignment `malloc` already guarantees.
    const GRAN: usize = 16;
    const CLASSES: usize = MAX_SIZE / GRAN;
    /// Blocks held per class before we start returning them to `System`.
    const DEPTH: usize = 32;

    struct Bin {
        ptrs: [*mut u8; DEPTH],
        len: usize,
    }

    impl Bin {
        const NEW: Bin = Bin {
            ptrs: [std::ptr::null_mut(); DEPTH],
            len: 0,
        };
    }

    /// Whether this thread's cache may hold blocks, which is only once its
    /// exit-time flush is armed.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        /// No block cached yet; the flush is not armed.
        Unarmed,
        /// The thread-exit destructor will return every cached block.
        Armed,
        /// The flush has run, or could not be armed: never cache again, so a
        /// block freed by a later destructor goes straight back to `System`.
        Bypass,
    }

    struct Cache {
        bins: [Bin; CLASSES],
        state: State,
    }

    thread_local! {
        static CACHE: UnsafeCell<Cache> = const {
            UnsafeCell::new(Cache {
                bins: [const { Bin::NEW }; CLASSES],
                state: State::Unarmed,
            })
        };
    }

    /// The process-wide key whose destructor flushes an exiting thread's cache,
    /// or `None` if the key table was full.
    fn flush_key() -> Option<libc::pthread_key_t> {
        static KEY: std::sync::OnceLock<Option<libc::pthread_key_t>> = std::sync::OnceLock::new();
        *KEY.get_or_init(|| {
            let mut key: libc::pthread_key_t = 0;
            // SAFETY: `key` is a valid out-pointer and `flush_on_exit` has the
            // destructor signature. musl allocates no memory here.
            (unsafe { libc::pthread_key_create(&mut key, Some(flush_on_exit)) } == 0).then_some(key)
        })
    }

    /// Arm the exit-time flush for the current thread. Returns the new state.
    fn arm() -> State {
        let Some(key) = flush_key() else {
            return State::Bypass;
        };
        // Any non-null value makes the destructor run at thread exit. A
        // thread that never caches a block never pays for this.
        // SAFETY: `key` came from a successful `pthread_key_create`.
        if unsafe { libc::pthread_setspecific(key, std::ptr::dangling::<u8>().cast()) } == 0 {
            State::Armed
        } else {
            State::Bypass
        }
    }

    /// Thread-specific-data destructor: return every cached block to `System`
    /// and stop caching on this thread, since other destructors may still free.
    unsafe extern "C" fn flush_on_exit(_: *mut libc::c_void) {
        let _ = CACHE.try_with(|c| {
            // SAFETY: the cache is thread-local; this destructor runs on its
            // own thread, outside any allocator call, and `System.dealloc`
            // does not re-enter this allocator.
            let cache = unsafe { &mut *c.get() };
            cache.state = State::Bypass;
            for (class, bin) in cache.bins.iter_mut().enumerate() {
                for &ptr in &bin.ptrs[..bin.len] {
                    unsafe { System.dealloc(ptr, class_layout(class)) };
                }
                bin.len = 0;
            }
        });
    }

    /// The class index for a layout, or `None` if it must go to `System`.
    ///
    /// Zero-sized and over-aligned requests are excluded: `malloc` only
    /// guarantees 16-byte alignment, so anything stricter cannot be served from
    /// a pooled block.
    #[inline]
    fn class_of(layout: Layout) -> Option<usize> {
        let size = layout.size();
        if size == 0 || size > MAX_SIZE || layout.align() > GRAN {
            return None;
        }
        Some(size.div_ceil(GRAN) - 1)
    }

    /// The layout a class is actually allocated with — the same for every
    /// request that maps to it, which is what makes pooling sound.
    #[inline]
    fn class_layout(class: usize) -> Layout {
        // SAFETY: size is a non-zero multiple of GRAN and GRAN is a power of two.
        unsafe { Layout::from_size_align_unchecked((class + 1) * GRAN, GRAN) }
    }

    pub struct ThreadCached;

    // A yielding caller can resume on another carrier. Keep every allocator
    // operation out of line so LLVM cannot retain this native-thread TLS cache
    // address across that migration. No operation below yields or polls.
    unsafe impl GlobalAlloc for ThreadCached {
        #[inline(never)]
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let Some(class) = class_of(layout) else {
                return unsafe { System.alloc(layout) };
            };
            let hit = CACHE.try_with(|c| {
                // SAFETY: the cache is thread-local and no reference escapes
                // this closure, so there is no aliasing; nothing inside
                // allocates, so this allocator cannot re-enter here.
                let bin = unsafe { &mut (*c.get()).bins[class] };
                if bin.len == 0 {
                    return std::ptr::null_mut();
                }
                bin.len -= 1;
                bin.ptrs[bin.len]
            });
            match hit {
                Ok(p) if !p.is_null() => p,
                // Miss, or the thread-local is gone (during TLS teardown).
                _ => unsafe { System.alloc(class_layout(class)) },
            }
        }

        #[inline(never)]
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            let Some(class) = class_of(layout) else {
                return unsafe { System.dealloc(ptr, layout) };
            };
            let stored = CACHE
                .try_with(|c| {
                    // SAFETY: as in `alloc`; `arm` does not allocate.
                    let cache = unsafe { &mut *c.get() };
                    if cache.state == State::Unarmed {
                        cache.state = arm();
                    }
                    if cache.state != State::Armed {
                        return false;
                    }
                    let bin = &mut cache.bins[class];
                    if bin.len == DEPTH {
                        return false;
                    }
                    bin.ptrs[bin.len] = ptr;
                    bin.len += 1;
                    true
                })
                .unwrap_or(false);
            if !stored {
                unsafe { System.dealloc(ptr, class_layout(class)) };
            }
        }

        #[inline(never)]
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // Anything `System` serves directly keeps its `calloc` path, which
            // can hand back pre-zeroed pages. A pooled block has to be zeroed by
            // hand because it is recycled memory.
            if class_of(layout).is_none() {
                return unsafe { System.alloc_zeroed(layout) };
            }
            let ptr = unsafe { self.alloc(layout) };
            if !ptr.is_null() {
                unsafe { std::ptr::write_bytes(ptr, 0, layout.size()) };
            }
            ptr
        }

        #[inline(never)]
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // A pooled block was allocated with its CLASS layout, not the
            // caller's, so it cannot be handed to `System.realloc` with the
            // caller's layout. Only a block `System` owns under exactly
            // `layout` may take the in-place path.
            let old_class = class_of(layout);
            let new_layout = match Layout::from_size_align(new_size, layout.align()) {
                Ok(l) => l,
                Err(_) => return std::ptr::null_mut(),
            };
            if old_class.is_none() && class_of(new_layout).is_none() {
                return unsafe { System.realloc(ptr, layout, new_size) };
            }
            let new_ptr = unsafe { self.alloc(new_layout) };
            if !new_ptr.is_null() {
                let copy = layout.size().min(new_size);
                unsafe { std::ptr::copy_nonoverlapping(ptr, new_ptr, copy) };
                unsafe { self.dealloc(ptr, layout) };
            }
            new_ptr
        }
    }
}

#[cfg(all(
    feature = "thread-cache-alloc",
    target_env = "musl",
    not(feature = "alloc-count")
))]
#[global_allocator]
static GLOBAL_ALLOC: HostTracked<tcache::ThreadCached> =
    HostTracked(tcache::ThreadCached, HostAccounting);

/// Rust-side allocation counting, behind the `alloc-count` cargo feature.
///
/// The release profile puts ~26.5% of a bytecode-call benchmark in the C
/// allocator (__libc_free + __libc_malloc_impl + musl's __lock/__unlock), but
/// perf cannot unwind out of musl's malloc, so callers do not resolve and the
/// cost cannot be attributed from a profile alone. Wrapping the global
/// allocator gives an exact count instead.
///
/// Measured this way: a bytecode call costs ~7 Rust heap allocations and ~184
/// bytes (bliss-iry5). Note this counts the RUST heap; Lisp consing goes
/// through the GC's bump allocator and is a different, much cheaper thing —
/// ROOM's total-consed will not show any of this.
///
/// Usage:
///   cargo build --release -p egcl --features alloc-count
///   EGCL_PROBE_ALLOC_COUNT=1 ./egcl … 2>&1 | grep @ALLOCS
/// Subtract a startup-only run to get the delta for the workload itself.
#[cfg(feature = "alloc-count")]
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static ALLOCS: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    /// Sample every Nth allocation and record where it came from.
    /// `EGCL_PROBE_ALLOC_SAMPLE=N` enables it; 0/unset disables.
    pub static SAMPLE_EVERY: AtomicU64 = AtomicU64::new(0);
    pub static SAMPLES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    thread_local! {
        /// Capturing a backtrace allocates, which would re-enter this allocator
        /// and recurse forever. `const` init so reading the flag cannot itself
        /// allocate on first touch.
        static IN_PROBE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn maybe_sample(n: u64) {
        let every = SAMPLE_EVERY.load(Ordering::Relaxed);
        if every == 0 || n % every != 0 {
            return;
        }
        let already = IN_PROBE.with(|f| f.replace(true));
        if already {
            return; // allocation made BY the probe itself
        }
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        if let Ok(mut s) = SAMPLES.lock() {
            s.push(bt);
        }
        IN_PROBE.with(|f| f.set(false));
    }

    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let n = ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
            maybe_sample(n);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.realloc(p, l, n) }
        }
    }
}
#[cfg(feature = "alloc-count")]
#[global_allocator]
static COUNTING_ALLOC: HostTracked<alloc_probe::Counting> =
    HostTracked(alloc_probe::Counting, HostAccounting);

#[cfg(not(any(
    feature = "alloc-count",
    all(feature = "thread-cache-alloc", target_env = "musl")
)))]
#[global_allocator]
static SYSTEM_ALLOC: HostTracked<std::alloc::System> =
    HostTracked(std::alloc::System, HostAccounting);

fn main() {
    egcl_rt::host_memory::enable_tracking();
    #[cfg(feature = "alloc-count")]
    if let Ok(n) = std::env::var("EGCL_PROBE_ALLOC_SAMPLE") {
        if let Ok(n) = n.parse::<u64>() {
            alloc_probe::SAMPLE_EVERY.store(n, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // Collect OS args, skipping argv[0] (the program name).
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = cli::run(&args);
    // JFR-style event stream (bliss-3gme): dump the recording if EGCL_EVENTS_DUMP
    // is set, whichever way the run ended, before we exit the process.
    #[cfg(feature = "alloc-count")]
    if let Some(path) = std::env::var_os("EGCL_PROBE_ALLOC_SAMPLE_OUT") {
        if let Ok(samples) = alloc_probe::SAMPLES.lock() {
            let _ = std::fs::write(&path, samples.join("\n=====\n"));
            eprintln!("@SAMPLES {} written to {:?}", samples.len(), path);
        }
    }
    #[cfg(feature = "alloc-count")]
    if std::env::var_os("EGCL_PROBE_ALLOC_COUNT").is_some() {
        use std::sync::atomic::Ordering;
        eprintln!(
            "@ALLOCS {} @BYTES {}",
            alloc_probe::ALLOCS.load(Ordering::Relaxed),
            alloc_probe::BYTES.load(Ordering::Relaxed)
        );
    }
    cli::events::maybe_report_direct_builtin_stats();
    cli::events::maybe_dump_on_exit();
    // egcl-jitrec: finalize the NDJSON stream (symbol + function records) and
    // close it, if EGCL_EVENTS_STREAM was set.
    cli::events::finalize_stream();
    // Lisp-aware statistical profiler (bliss-sc4t): dump folded stacks if
    // EGCL_SPROF was set.
    cli::sprof::dump_on_exit();
    cli::handoff_census::dump_on_exit();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Use describe_err so symbols render by name (e.g. "undefined
            // function: FIND-IF") rather than "Symbol(194)".
            eprintln!("egcl: {}", cli::describe_err(&e));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod host_memory_tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    // Each fixture owns its accounting, independent of allocations by libtest
    // or concurrently running tests. Production uses only HostAccounting.
    #[derive(Default)]
    struct Recording {
        live: Cell<usize>,
        operations: Cell<usize>,
    }

    impl AllocationAccounting for Recording {
        fn alloc(&self, size: usize) {
            self.live.set(self.live.get() + size);
            self.operations.set(self.operations.get() + 1);
        }

        fn dealloc(&self, size: usize) {
            self.live.set(self.live.get() - size);
            self.operations.set(self.operations.get() + 1);
        }

        fn resize(&self, old: usize, new: usize) {
            self.live.set(self.live.get() - old + new);
            self.operations.set(self.operations.get() + 1);
        }
    }

    struct SplitRealloc {
        fail: Cell<bool>,
    }

    unsafe impl GlobalAlloc for SplitRealloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if self.fail.get() {
                std::ptr::null_mut()
            } else {
                unsafe { System.alloc(layout) }
            }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let new_layout = Layout::from_size_align(size, layout.align()).unwrap();
            let resized = unsafe { self.alloc(new_layout) };
            if !resized.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(pointer, resized, size.min(layout.size())) };
                unsafe { self.dealloc(pointer, layout) };
            }
            resized
        }
    }

    #[test]
    fn failed_alloc_and_resize_preserve_live_requested_bytes() {
        let allocator = HostTracked(
            SplitRealloc {
                fail: Cell::new(true),
            },
            Recording::default(),
        );
        let layout = Layout::from_size_align(9, 8).unwrap();
        assert!(unsafe { allocator.alloc(layout) }.is_null());
        assert_eq!(allocator.1.live.get(), 0);
        assert_eq!(allocator.1.operations.get(), 0);
        allocator.0.fail.set(false);
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(!pointer.is_null());
        unsafe { std::ptr::write_bytes(pointer, 0xab, layout.size()) };
        allocator.0.fail.set(true);
        assert!(unsafe { allocator.realloc(pointer, layout, 19) }.is_null());
        assert_eq!(allocator.1.live.get(), 9);
        assert_eq!(allocator.1.operations.get(), 1);
        assert_eq!(unsafe { *pointer }, 0xab, "failed realloc retains old data");
        allocator.0.fail.set(false);
        let pointer = unsafe { allocator.realloc(pointer, layout, 19) };
        assert!(!pointer.is_null());
        assert_eq!(allocator.1.live.get(), 19);
        assert_eq!(allocator.1.operations.get(), 2, "one net resize operation");
        assert_eq!(unsafe { *pointer.add(8) }, 0xab);
        let layout = Layout::from_size_align(19, 8).unwrap();
        let pointer = unsafe { allocator.realloc(pointer, layout, 5) };
        assert!(!pointer.is_null());
        assert_eq!(allocator.1.live.get(), 5);
        assert_eq!(allocator.1.operations.get(), 3);
        unsafe { allocator.dealloc(pointer, Layout::from_size_align(5, 8).unwrap()) };
        assert_eq!(allocator.1.live.get(), 0);
        assert_eq!(allocator.1.operations.get(), 4);
    }

    #[test]
    fn zeroed_allocation_records_only_the_outer_requested_size() {
        let allocator = HostTracked(
            SplitRealloc {
                fail: Cell::new(false),
            },
            Recording::default(),
        );
        let layout = Layout::from_size_align(13, 8).unwrap();
        let pointer = unsafe { allocator.alloc_zeroed(layout) };
        assert!(!pointer.is_null());
        assert_eq!(unsafe { std::slice::from_raw_parts(pointer, 13) }, &[0; 13]);
        assert_eq!(allocator.1.live.get(), 13);
        assert_eq!(allocator.1.operations.get(), 1);
        unsafe { allocator.dealloc(pointer, layout) };
        assert_eq!(allocator.1.live.get(), 0);
        assert_eq!(allocator.1.operations.get(), 2);
        allocator.0.fail.set(true);
        assert!(unsafe { allocator.alloc_zeroed(layout) }.is_null());
        assert_eq!(allocator.1.operations.get(), 2);
    }

    #[test]
    #[cfg(all(
        feature = "thread-cache-alloc",
        target_env = "musl",
        not(feature = "alloc-count")
    ))]
    fn thread_cache_counts_requests_instead_of_retained_size_classes() {
        let allocator = HostTracked(tcache::ThreadCached, Recording::default());
        let layout = Layout::from_size_align(9, 8).unwrap();
        let pointer = unsafe { allocator.alloc_zeroed(layout) };
        assert!(!pointer.is_null());
        assert_eq!(allocator.1.live.get(), 9, "not the 16-byte cache class");
        let pointer = unsafe { allocator.realloc(pointer, layout, 13) };
        assert!(!pointer.is_null());
        assert_eq!(allocator.1.live.get(), 13);
        assert_eq!(allocator.1.operations.get(), 2);
        unsafe { allocator.dealloc(pointer, Layout::from_size_align(13, 8).unwrap()) };
        assert_eq!(
            allocator.1.live.get(),
            0,
            "cached blocks are not live requests"
        );
        assert_eq!(allocator.1.operations.get(), 3);
    }
}
