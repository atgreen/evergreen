//! Safepoint mechanism — polling-page-based cooperative suspension.
//!
//! A single page is allocated page-aligned. Generated code performs a load
//! from this page at every safepoint poll site. When a safepoint is
//! requested, the page is flagged, and polling threads observe the flag
//! to enter a safepoint. See §2.5, §3.9.
//!
//! ## Coordination protocol
//!
//! 1. The GC (or other stop-the-world requester) calls `request_safepoint()`
//!    on the global `SafepointPage`, setting the `requested` flag.
//! 2. Each mutator thread polls via `poll_safepoint()` at every safepoint
//!    site. When the flag is observed, the thread calls `enter_safepoint()`.
//! 3. `enter_safepoint()` increments the `arrived` counter and parks on a
//!    condvar until the safepoint is cleared.
//! 4. `wait_for_all_threads()` spins/waits until `arrived` equals the
//!    number of registered mutator threads minus the requesting thread.
//! 5. `resume_all_threads()` clears the flag, resets the counter, and
//!    broadcasts the condvar to wake all parked threads.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use crate::error::BlissError;

/// Handle to the safepoint page.
pub struct SafepointPage {
    /// Page-aligned memory buffer (4096 bytes).
    page: *mut u8,
    /// Layout used for deallocation.
    layout: std::alloc::Layout,
    /// Whether a safepoint is currently requested.
    requested: AtomicBool,
}

// SafepointPage is shared across threads for GC coordination.
unsafe impl Send for SafepointPage {}
unsafe impl Sync for SafepointPage {}

impl Drop for SafepointPage {
    fn drop(&mut self) {
        if !self.page.is_null() {
            unsafe { std::alloc::dealloc(self.page, self.layout) };
        }
    }
}

impl SafepointPage {
    /// Allocate and map the safepoint page (called once at startup).
    pub fn init() -> Result<Self, BlissError> {
        let layout = std::alloc::Layout::from_size_align(4096, 4096)
            .map_err(|e| BlissError::Internal(format!("safepoint layout: {}", e)))?;
        let page = unsafe { std::alloc::alloc_zeroed(layout) };
        if page.is_null() {
            return Err(BlissError::Oom);
        }
        Ok(SafepointPage {
            page,
            layout,
            requested: AtomicBool::new(false),
        })
    }

    /// Get the address of the safepoint page (for code generation).
    pub fn address(&self) -> *const u8 {
        self.page as *const u8
    }

    /// Request all threads to reach a safepoint by poisoning the page.
    ///
    /// Sets the `requested` flag so that `poll_safepoint()` will cause
    /// mutator threads to enter the safepoint and park.
    pub fn request_safepoint(&self) -> Result<(), BlissError> {
        self.requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Resume normal operation by restoring the page to readable.
    ///
    /// Clears the `requested` flag. Threads that have not yet observed
    /// the flag will continue running without parking.
    pub fn resume(&self) -> Result<(), BlissError> {
        self.requested.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Check if a safepoint is currently requested.
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

// ── Global safepoint coordination state ──────────────────────────────

/// Coordination state shared between the GC requester and mutator threads.
struct SafepointCoordinator {
    /// Number of mutator threads that have arrived at the safepoint.
    arrived: AtomicUsize,
    /// Total number of mutator threads that must arrive (set by requester).
    expected: AtomicUsize,
    /// Whether threads should remain parked at the safepoint.
    parked: AtomicBool,
    /// Mutex + condvar for mutator threads to park on while at a safepoint.
    park_mutex: Mutex<()>,
    park_condvar: Condvar,
    /// Mutex + condvar for the GC requester to wait until all threads arrive.
    arrival_mutex: Mutex<()>,
    arrival_condvar: Condvar,
}

/// Access the global coordinator (lazily initialized).
fn coordinator() -> &'static SafepointCoordinator {
    static COORD: OnceLock<SafepointCoordinator> = OnceLock::new();
    COORD.get_or_init(|| SafepointCoordinator {
        arrived: AtomicUsize::new(0),
        expected: AtomicUsize::new(0),
        parked: AtomicBool::new(false),
        park_mutex: Mutex::new(()),
        park_condvar: Condvar::new(),
        arrival_mutex: Mutex::new(()),
        arrival_condvar: Condvar::new(),
    })
}

/// Access the global safepoint page (lazily initialized).
fn global_safepoint_page() -> &'static SafepointPage {
    static PAGE: OnceLock<SafepointPage> = OnceLock::new();
    PAGE.get_or_init(|| {
        SafepointPage::init().expect("failed to allocate global safepoint page")
    })
}

// ── Public coordination functions ────────────────────────────────────

/// Inline safepoint poll — checks the safepoint flag.
///
/// In generated code this compiles to a single load + branch. If the
/// safepoint page is poisoned (i.e. a safepoint has been requested),
/// the calling thread enters the safepoint and parks until the requester
/// resumes all threads.
#[inline(always)]
pub fn poll_safepoint() {
    let page = global_safepoint_page();
    if page.is_requested() {
        enter_safepoint();
    }
}

/// Enter a safepoint: signal arrival, then park until the requester
/// resumes all threads.
///
/// This function is called by mutator threads when they observe that a
/// safepoint has been requested (either via `poll_safepoint` or at an
/// explicit safepoint such as allocation or backward branch).
///
/// Protocol:
/// 1. Increment the `arrived` counter.
/// 2. Notify the GC requester (via `arrival_condvar`) that another
///    thread has arrived.
/// 3. Wait on `park_condvar` until `parked` is cleared.
pub fn enter_safepoint() {
    let coord = coordinator();

    // If no safepoint coordination is active (parked == false and no
    // one is waiting), just return — this handles the case where
    // poll_safepoint fires but no wait_for_all_threads is pending.
    if !coord.parked.load(Ordering::SeqCst) {
        return;
    }

    // Signal arrival.
    let prev = coord.arrived.fetch_add(1, Ordering::SeqCst);
    let expected = coord.expected.load(Ordering::SeqCst);

    // If we are the last expected thread, wake the requester.
    if prev + 1 >= expected {
        let _lock = coord.arrival_mutex.lock().unwrap();
        coord.arrival_condvar.notify_all();
    }

    // Park until the requester clears `parked`.
    let mut guard = coord.park_mutex.lock().unwrap();
    while coord.parked.load(Ordering::SeqCst) {
        guard = coord.park_condvar.wait(guard).unwrap();
    }
}

/// Wait until all mutator threads have reached a safepoint.
///
/// The caller (typically the GC) sets up coordination state, requests
/// the safepoint, and then blocks until all expected threads have
/// arrived via `enter_safepoint()`.
///
/// In the bootstrap runtime with only a single thread context the
/// current thread counts as already at a safepoint, so this returns
/// immediately when there are no other registered threads.
pub fn wait_for_all_threads() -> Result<(), BlissError> {
    let coord = coordinator();
    let page = global_safepoint_page();

    // Determine how many other mutator threads need to arrive.
    // In the bootstrap single-threaded runtime this is typically 0.
    // We use the thread registry to count live threads, excluding the
    // current (requesting) thread.
    let all_ids = crate::thread::all_thread_ids();
    let current = crate::thread::current_thread_id();
    let other_count = all_ids.iter().filter(|id| **id != current).count();

    if other_count == 0 {
        // Single-threaded: trivially at a safepoint already.
        // Still set the flag so poll_safepoint returns correctly if called.
        page.request_safepoint()?;
        return Ok(());
    }

    // Prepare coordination state.
    coord.arrived.store(0, Ordering::SeqCst);
    coord.expected.store(other_count, Ordering::SeqCst);
    coord.parked.store(true, Ordering::SeqCst);

    // Poison the safepoint page so polling threads enter the safepoint.
    page.request_safepoint()?;

    // Wait until all expected threads have arrived.
    let mut guard = coord.arrival_mutex.lock().unwrap();
    while coord.arrived.load(Ordering::SeqCst) < other_count {
        // Use a timed wait to avoid deadlock if a thread died before
        // reaching its next poll point.
        let (new_guard, timeout) = coord
            .arrival_condvar
            .wait_timeout(guard, std::time::Duration::from_millis(100))
            .unwrap();
        guard = new_guard;

        if timeout.timed_out() {
            // Re-check: a thread may have arrived between the load
            // and the wait call.
            if coord.arrived.load(Ordering::SeqCst) >= other_count {
                break;
            }
            // Continue waiting — the thread may be in a long-running
            // native call and will poll eventually.
        }
    }

    Ok(())
}

/// Resume all threads after a safepoint operation (e.g. GC is done).
///
/// Clears the safepoint request, resets coordination counters, and
/// wakes all parked mutator threads so they can continue execution.
pub fn resume_all_threads() -> Result<(), BlissError> {
    let coord = coordinator();
    let page = global_safepoint_page();

    // Clear the safepoint page — new polls will no longer enter.
    page.resume()?;

    // Clear the parked flag and wake all waiting threads.
    coord.parked.store(false, Ordering::SeqCst);
    coord.park_condvar.notify_all();

    // Reset counters for the next safepoint cycle.
    coord.arrived.store(0, Ordering::SeqCst);
    coord.expected.store(0, Ordering::SeqCst);

    Ok(())
}
