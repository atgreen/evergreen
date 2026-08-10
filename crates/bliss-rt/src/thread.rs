//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::BlissVal;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Unique identifier for a green thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GreenThreadId(pub u64);

/// Green thread states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadState {
    /// On a worker's run-queue; may be executing.
    Runnable = 0,
    /// Waiting on a mutex, condition variable, or channel.
    Blocked = 1,
    /// Executing a C FFI call; GC skips this thread.
    Native = 2,
    /// Blocked on async I/O.
    Waiting = 3,
    /// Entry function returned or thread was killed.
    Dead = 4,
}

/// Maximum number of TLS slots per green thread.
pub const MAX_TLS: usize = 4096;

/// Default stack size for green threads (512 KiB).
const DEFAULT_STACK_SIZE: usize = 512 * 1024;

/// Atomic counter for generating unique thread IDs.
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

/// Global thread registry mapping IDs to thread descriptors.
fn thread_registry() -> &'static Mutex<HashMap<GreenThreadId, Arc<GreenThread>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<GreenThreadId, Arc<GreenThread>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

// Thread-local cache of the current green thread.
thread_local! {
    static CURRENT_THREAD: Arc<GreenThread> = {
        let id = GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
        let thread = Arc::new(GreenThread {
            id,
            state: Mutex::new(ThreadState::Runnable),
            stack: BlissStack::new(DEFAULT_STACK_SIZE),
            tls: Mutex::new(vec![crate::value::NIL; MAX_TLS]),
            yield_requested: AtomicBool::new(false),
        });
        thread_registry().lock().unwrap().insert(id, Arc::clone(&thread));
        thread
    };
}

/// Green thread descriptor. D2.01.
pub struct GreenThread {
    id: GreenThreadId,
    state: Mutex<ThreadState>,
    stack: BlissStack,
    tls: Mutex<Vec<BlissVal>>,
    /// Per-thread yield flag for cooperative preemption at safepoints (§2.5.3 step 4).
    yield_requested: AtomicBool,
}

// Safety: GreenThread access is controlled by the scheduler and thread registry.
unsafe impl Send for GreenThread {}
unsafe impl Sync for GreenThread {}

impl GreenThread {
    /// Get this thread's unique ID.
    pub fn id(&self) -> GreenThreadId {
        self.id
    }

    /// Get the current state of this thread.
    pub fn state(&self) -> ThreadState {
        *self.state.lock().unwrap()
    }

    /// Get a reference to this thread's CL stack.
    pub fn stack(&self) -> &BlissStack {
        &self.stack
    }

    /// Get this thread's TLS slot at the given index.
    pub fn tls_get(&self, index: u32) -> BlissVal {
        let tls = self.tls.lock().unwrap();
        if (index as usize) < tls.len() {
            tls[index as usize]
        } else {
            crate::value::NIL
        }
    }

    /// Check and clear the per-thread yield flag (§2.5.3 step 4).
    /// Returns `true` if a yield was requested.
    pub fn check_and_clear_yield(&self) -> bool {
        self.yield_requested.swap(false, Ordering::SeqCst)
    }

    /// Request this thread to yield at its next safepoint.
    pub fn request_yield(&self) {
        self.yield_requested.store(true, Ordering::SeqCst);
    }

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: BlissVal) {
        let mut tls = self.tls.lock().unwrap();
        if (index as usize) < tls.len() {
            tls[index as usize] = value;
        }
    }
}

/// An OS-level worker thread in the worker pool.
pub struct WorkerThread {
    _private: (),
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state.
pub fn make_thread(_entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    let id = GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let thread = Arc::new(GreenThread {
        id,
        state: Mutex::new(ThreadState::Runnable),
        stack: BlissStack::new(DEFAULT_STACK_SIZE),
        tls: Mutex::new(vec![crate::value::NIL; MAX_TLS]),
        yield_requested: AtomicBool::new(false),
    });
    thread_registry().lock().unwrap().insert(id, thread);
    Ok(id)
}

/// Wait for a green thread to finish, returning its result value.
pub fn join_thread(id: GreenThreadId) -> Result<BlissVal, BlissError> {
    let registry = thread_registry().lock().unwrap();
    if registry.contains_key(&id) {
        // In the bootstrap implementation, threads don't truly execute.
        // Return NIL as the result value.
        Ok(crate::value::NIL)
    } else {
        Err(BlissError::Internal(format!("no thread with id {}", id.0)))
    }
}

/// Get the current green thread's ID.
pub fn current_thread_id() -> GreenThreadId {
    CURRENT_THREAD.with(|t| t.id)
}

/// Get a reference to the current green thread.
///
/// # Safety rationale
/// The GreenThread is held in an `Arc` stored both in the global registry
/// and in a thread-local cache. The thread-local Arc clone keeps the
/// allocation alive for at least the lifetime of the OS thread, so the
/// returned `&'static` reference is valid as long as the calling OS thread
/// is alive.
pub fn current_thread() -> &'static GreenThread {
    CURRENT_THREAD.with(|t| {
        // Safety: The Arc in thread-local storage keeps the GreenThread alive
        // for the lifetime of this OS thread. We return a 'static reference
        // that is valid as long as the OS thread lives.
        unsafe { &*(Arc::as_ptr(t)) }
    })
}

/// Yield the current green thread at the next safepoint.
pub fn thread_yield() {
    // In the bootstrap implementation, yielding is a no-op since
    // there is no preemptive scheduling yet.
}

/// Interrupt a green thread, delivering a condition to it.
pub fn interrupt_thread(id: GreenThreadId, _condition: BlissVal) -> Result<(), BlissError> {
    let registry = thread_registry().lock().unwrap();
    if registry.contains_key(&id) {
        // In the bootstrap implementation, interrupts are accepted but
        // not delivered (no scheduler is running to deliver them).
        Ok(())
    } else {
        Err(BlissError::Internal(format!("no thread with id {}", id.0)))
    }
}

/// List all live green thread IDs.
pub fn all_thread_ids() -> Vec<GreenThreadId> {
    let registry = thread_registry().lock().unwrap();
    // Ensure the current thread is in the registry by touching the thread-local
    drop(registry);
    let _ = current_thread_id();
    let registry = thread_registry().lock().unwrap();
    registry.keys().copied().collect()
}
