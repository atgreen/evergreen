//! Bliss Common Lisp — entry point.

use bliss::cli;

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
///   cargo build --release -p bliss-cli --features alloc-count
///   BLISS_PROBE_ALLOC_COUNT=1 ./bliss-cli … 2>&1 | grep @ALLOCS
/// Subtract a startup-only run to get the delta for the workload itself.
#[cfg(feature = "alloc-count")]
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static ALLOCS: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
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
static COUNTING_ALLOC: alloc_probe::Counting = alloc_probe::Counting;

fn main() {
    // Collect OS args, skipping argv[0] (the program name).
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = cli::run(&args);
    // JFR-style event stream (bliss-3gme): dump the recording if BLISS_EVENTS_DUMP
    // is set, whichever way the run ended, before we exit the process.
    #[cfg(feature = "alloc-count")]
    if std::env::var_os("BLISS_PROBE_ALLOC_COUNT").is_some() {
        use std::sync::atomic::Ordering;
        eprintln!(
            "@ALLOCS {} @BYTES {}",
            alloc_probe::ALLOCS.load(Ordering::Relaxed),
            alloc_probe::BYTES.load(Ordering::Relaxed)
        );
    }
    cli::events::maybe_report_direct_builtin_stats();
    cli::events::maybe_dump_on_exit();
    // bliss-jitrec: finalize the NDJSON stream (symbol + function records) and
    // close it, if BLISS_EVENTS_STREAM was set.
    cli::events::finalize_stream();
    // Lisp-aware statistical profiler (bliss-sc4t): dump folded stacks if
    // BLISS_SPROF was set.
    cli::sprof::dump_on_exit();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Use describe_err so symbols render by name (e.g. "undefined
            // function: FIND-IF") rather than "Symbol(194)".
            eprintln!("bliss: {}", cli::describe_err(&e));
            std::process::exit(1);
        }
    }
}
