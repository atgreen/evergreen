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
//! 3. `enter_safepoint()` publishes the thread's stack top (sp/fp) so the
//!    GC can walk the thread's CL stack, increments the `arrived` counter,
//!    and parks on a condvar until the safepoint is cleared.
//! 4. `wait_for_all_threads()` spins/waits until `arrived` equals the
//!    number of registered mutator threads minus the requesting thread.
//! 5. `resume_all_threads()` clears the flag, resets the counter, and
//!    broadcasts the condvar to wake all parked threads.
//! 6. After being unparked, each thread checks its per-thread yield flag
//!    and yields to the scheduler if preemption was requested (§2.5.3 step 4).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

static SAFEPOINT_PAGE_ADDR: AtomicUsize = AtomicUsize::new(0);
static SAFEPOINT_PAGE_LEN: AtomicUsize = AtomicUsize::new(0);

/// Flag-only SIGUSR1 handoff. Directed delivery interrupts a blocking syscall;
/// ordinary code observes this flag at its next safepoint poll.
static SIGUSR1_PENDING: AtomicBool = AtomicBool::new(false);

pub(crate) extern "C" fn sigusr1_handler(_signal: i32) {
    SIGUSR1_PENDING.store(true, Ordering::Relaxed);
}

use crate::error::BlissError;

/// Handle to the safepoint page.
pub struct SafepointPage {
    /// Page-aligned memory buffer (4096 bytes).
    page: *mut u8,
    /// Whether a safepoint is currently requested.
    requested: AtomicBool,
    /// Whether the polling page is currently protected with PROT_NONE.
    protected: AtomicBool,
}

// SafepointPage is shared across threads for GC coordination.
unsafe impl Send for SafepointPage {}
unsafe impl Sync for SafepointPage {}

impl Drop for SafepointPage {
    fn drop(&mut self) {
        if !self.page.is_null() {
            let _ = unsafe { crate::syscall::munmap(self.page, 4096) };
        }
    }
}

impl SafepointPage {
    /// Allocate and map the safepoint page (called once at startup).
    pub fn init() -> Result<Self, BlissError> {
        let page = unsafe {
            crate::syscall::mmap(
                std::ptr::null_mut(),
                4096,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
                crate::syscall::MAP_PRIVATE | crate::syscall::MAP_ANONYMOUS,
                -1,
                0,
            )
        }
        .map_err(|errno| BlissError::Internal(format!("safepoint mmap failed: errno {errno}")))?;
        SAFEPOINT_PAGE_ADDR.store(page as usize, Ordering::Release);
        SAFEPOINT_PAGE_LEN.store(4096, Ordering::Release);
        Ok(SafepointPage {
            page,
            requested: AtomicBool::new(false),
            protected: AtomicBool::new(false),
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
        unsafe { crate::syscall::mprotect(self.page, 4096, crate::syscall::PROT_NONE) }.map_err(
            |errno| BlissError::Internal(format!("safepoint mprotect failed: errno {errno}")),
        )?;
        self.protected.store(true, Ordering::SeqCst);
        self.requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Resume normal operation by restoring the page to readable.
    ///
    /// Clears the `requested` flag. Threads that have not yet observed
    /// the flag will continue running without parking.
    pub fn resume(&self) -> Result<(), BlissError> {
        unsafe {
            crate::syscall::mprotect(
                self.page,
                4096,
                crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
            )
        }
        .map_err(|errno| {
            BlissError::Internal(format!("safepoint mprotect restore failed: errno {errno}"))
        })?;
        self.protected.store(false, Ordering::SeqCst);
        self.requested.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Check if a safepoint is currently requested.
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub fn poll_page_is_protected(&self) -> bool {
        self.protected.load(Ordering::SeqCst)
    }
}

pub fn safepoint_page_contains(addr: usize) -> bool {
    let base = SAFEPOINT_PAGE_ADDR.load(Ordering::Acquire);
    let len = SAFEPOINT_PAGE_LEN.load(Ordering::Acquire);
    base != 0 && addr.wrapping_sub(base) < len
}

pub(crate) fn recover_poll_page_sigsegv() -> bool {
    let base = SAFEPOINT_PAGE_ADDR.load(Ordering::Acquire);
    let len = SAFEPOINT_PAGE_LEN.load(Ordering::Acquire);
    if base == 0 || len == 0 {
        return false;
    }
    let restored = unsafe {
        crate::syscall::mprotect(
            base as *mut u8,
            len,
            crate::syscall::PROT_READ | crate::syscall::PROT_WRITE,
        )
    }
    .is_ok();
    if restored {
        SIGUSR1_PENDING.store(true, Ordering::Relaxed);
    }
    restored
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
    PAGE.get_or_init(|| SafepointPage::init().expect("failed to allocate global safepoint page"))
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
    let interrupted = SIGUSR1_PENDING.swap(false, Ordering::Relaxed);
    if page.is_requested() || interrupted {
        enter_safepoint();
    } else {
        check_preemption();
    }
}

/// Cooperative scheduling is checked at every safepoint, independently of a
/// GC stop-the-world request. A time-slice expiry only sets this flag; the
/// mounted fiber performs the actual context switch at this safe boundary.
fn check_preemption() {
    if let Some(fiber) = crate::thread::current_fiber() {
        if fiber.check_and_clear_yield() {
            let _ = crate::thread::fiber_yield();
        }
    } else {
        let thread = crate::thread::current_thread();
        if thread.check_and_clear_yield() {
            crate::thread::thread_yield();
        }
    }
}

/// Enter a safepoint: publish stack top, signal arrival, then park until
/// the requester resumes all threads.
///
/// This function is called by mutator threads when they observe that a
/// safepoint has been requested (either via `poll_safepoint` or at an
/// explicit safepoint such as allocation or backward branch).
///
/// Protocol:
/// 1. Publish the thread's stack top (sp/fp) so the GC can scan it (§2.5.3).
/// 2. Increment the `arrived` counter (under `arrival_mutex`).
/// 3. Notify the GC requester (via `arrival_condvar`) that another
///    thread has arrived.
/// 4. Wait on `park_condvar` until `parked` is cleared.
/// 5. After waking, check the per-thread yield flag and yield if set (§2.5.3 step 4).
///
/// Note: this function only coordinates when called through the
/// `wait_for_all_threads`/`resume_all_threads` protocol. The `parked`
/// flag (set by `wait_for_all_threads` before requesting the safepoint)
/// gates participation: if `parked` is false, no coordination cycle is
/// active, so the thread publishes its stack top but does not
/// increment `arrived` or park. Callers of `request_safepoint()` that
/// want threads to coordinate MUST go through `wait_for_all_threads`.
pub fn enter_safepoint() {
    let coord = coordinator();

    // §2.5.3: Publish the thread's stack top (sp/fp) to its thread
    // descriptor so the GC can scan the CL stack while parked.
    crate::thread::current_stack().publish_top();

    // Issue #1 fix: if no coordination cycle is active (`parked` is
    // false), return immediately — the thread has already published its
    // stack top. `request_safepoint()` alone does NOT cause coordination;
    // only `wait_for_all_threads` (which sets `parked = true` before
    // requesting) triggers the full arrive-and-park protocol.
    if !coord.parked.load(Ordering::SeqCst) {
        check_preemption();
        return;
    }

    crate::gc::retire_current_t0_tlab_for_safepoint();

    // Issue #5 fix: increment `arrived` while holding `arrival_mutex`
    // so the notification cannot be lost between the increment and
    // the condvar wait in `wait_for_all_threads`.
    //
    // Note: we do NOT re-check `parked` under the lock here. The
    // ordering guarantee is that `wait_for_all_threads` sets
    // `parked = true` (SeqCst) BEFORE `request_safepoint()`, and
    // `resume_all_threads` clears `parked` AFTER all threads have
    // been accounted for. So if we saw `parked == true` above, the
    // coordination cycle is active and we must participate.
    {
        let _lock = coord.arrival_mutex.lock().unwrap();
        coord.arrived.fetch_add(1, Ordering::SeqCst);
        coord.arrival_condvar.notify_all();
    }

    // Park until the requester clears `parked`.
    {
        let mut guard = coord.park_mutex.lock().unwrap();
        while coord.parked.load(Ordering::SeqCst) {
            guard = coord.park_condvar.wait(guard).unwrap();
        }
    }

    // §2.5.3 step 4: check per-thread yield flag and yield if preemption
    // was requested.
    check_preemption();
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

    let current = crate::thread::current_thread_id();
    // Determine how many other mutator threads need to arrive.
    // Native threads are excluded because they cannot observe poll sites
    // while executing foreign code and therefore must not block the handshake.
    let other_count = crate::thread::safepoint_participant_count_excluding(current);

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
    // Issue #5 fix: `arrived` is now incremented under `arrival_mutex`,
    // so we hold the same lock when checking and waiting, preventing
    // lost notifications.
    let fallback_delay = std::env::var("BLISS_SIGUSR1_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100);
    let mut guard = coord.arrival_mutex.lock().unwrap();
    let mut timeouts = 0u32;
    while coord.arrived.load(Ordering::SeqCst) < other_count {
        // Use a timed wait to avoid deadlock if a thread died before
        // reaching its next poll point.
        let (new_guard, timeout) = coord
            .arrival_condvar
            .wait_timeout(guard, std::time::Duration::from_millis(fallback_delay))
            .unwrap();
        guard = new_guard;

        if timeout.timed_out() {
            // Re-check: a thread may have arrived between the load
            // and the wait call.
            if coord.arrived.load(Ordering::SeqCst) >= other_count {
                break;
            }
            timeouts += 1;
            if timeouts == 1 {
                // A mutator may be blocked in a syscall and unable to touch the
                // polling page. Directed SIGUSR1 delivery is flag-only and
                // exists solely to make that syscall return EINTR.
                crate::runtime::install_signal_handlers()?;
                let _ = crate::thread::signal_safepoint_participants(current);
            }
            // In the bootstrap runtime the thread registry can contain
            // worker threads that never participate in safepoint polling.
            if timeouts >= 10 {
                let arrived = coord.arrived.load(Ordering::SeqCst);
                let _ = resume_all_threads();
                return Err(BlissError::Internal(format!(
                    "safepoint handshake failed: arrived {arrived}/{other_count} mutator threads"
                )));
            }
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

    // Issue #4 fix: acquire park_mutex before storing parked = false
    // and notifying, so the condvar notification cannot be lost.
    {
        let _guard = coord.park_mutex.lock().unwrap();
        coord.parked.store(false, Ordering::SeqCst);
        coord.park_condvar.notify_all();
    }

    // Reset counters for the next safepoint cycle.
    coord.arrived.store(0, Ordering::SeqCst);
    coord.expected.store(0, Ordering::SeqCst);

    Ok(())
}
