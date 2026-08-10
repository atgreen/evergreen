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

/// Green thread descriptor. D2.01.
pub struct GreenThread {
    _private: (), // opaque
}

impl GreenThread {
    /// Get this thread's unique ID.
    pub fn id(&self) -> GreenThreadId {
        unimplemented!("GreenThread::id")
    }

    /// Get the current state of this thread.
    pub fn state(&self) -> ThreadState {
        unimplemented!("GreenThread::state")
    }

    /// Get a reference to this thread's CL stack.
    pub fn stack(&self) -> &BlissStack {
        unimplemented!("GreenThread::stack")
    }

    /// Get this thread's TLS slot at the given index.
    pub fn tls_get(&self, index: u32) -> BlissVal {
        unimplemented!("GreenThread::tls_get")
    }

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: BlissVal) {
        unimplemented!("GreenThread::tls_set")
    }
}

/// Maximum number of TLS slots per green thread.
pub const MAX_TLS: usize = 4096;

// ── Worker thread ──────────────────────────────────────────────────

/// An OS-level worker thread in the worker pool.
pub struct WorkerThread {
    _private: (),
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    unimplemented!("make_thread")
}

/// Wait for a green thread to finish, returning its result value.
pub fn join_thread(id: GreenThreadId) -> Result<BlissVal, BlissError> {
    unimplemented!("join_thread")
}

/// Get the current green thread's ID.
pub fn current_thread_id() -> GreenThreadId {
    unimplemented!("current_thread_id")
}

/// Get a reference to the current green thread.
pub fn current_thread() -> &'static GreenThread {
    unimplemented!("current_thread")
}

/// Yield the current green thread at the next safepoint.
pub fn thread_yield() {
    unimplemented!("thread_yield")
}

/// Interrupt a green thread, delivering a condition to it.
pub fn interrupt_thread(id: GreenThreadId, condition: BlissVal) -> Result<(), BlissError> {
    unimplemented!("interrupt_thread")
}

/// List all live green thread IDs.
pub fn all_thread_ids() -> Vec<GreenThreadId> {
    unimplemented!("all_thread_ids")
}
