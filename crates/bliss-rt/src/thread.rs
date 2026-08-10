//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::BlissVal;

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

/// Green thread descriptor. D2.01.
pub struct GreenThread {
    _private: (),
}

// Safety: GreenThread access is controlled by the scheduler and thread registry.
unsafe impl Send for GreenThread {}
unsafe impl Sync for GreenThread {}

impl GreenThread {
    /// Get this thread's unique ID.
    pub fn id(&self) -> GreenThreadId {
        unimplemented!()
    }

    /// Get the current state of this thread.
    pub fn state(&self) -> ThreadState {
        unimplemented!()
    }

    /// Get a reference to this thread's CL stack.
    pub fn stack(&self) -> &BlissStack {
        unimplemented!()
    }

    /// Get this thread's TLS slot at the given index.
    pub fn tls_get(&self, index: u32) -> BlissVal {
        unimplemented!()
    }

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: BlissVal) {
        unimplemented!()
    }
}

/// An OS-level worker thread in the worker pool.
pub struct WorkerThread {
    _private: (),
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    unimplemented!()
}

/// Wait for a green thread to finish, returning its result value.
pub fn join_thread(id: GreenThreadId) -> Result<BlissVal, BlissError> {
    unimplemented!()
}

/// Get the current green thread's ID.
pub fn current_thread_id() -> GreenThreadId {
    unimplemented!()
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
    unimplemented!()
}

/// Yield the current green thread at the next safepoint.
pub fn thread_yield() {
    unimplemented!()
}

/// Interrupt a green thread, delivering a condition to it.
pub fn interrupt_thread(id: GreenThreadId, _condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!()
}

/// List all live green thread IDs.
pub fn all_thread_ids() -> Vec<GreenThreadId> {
    unimplemented!()
}
