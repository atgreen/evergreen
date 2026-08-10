//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::BlissVal;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

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

/// Holds the result of a thread's execution and a signal for completion.
struct ThreadResult {
    /// The result value, set when the thread finishes.
    value: Mutex<Option<BlissVal>>,
    /// Condvar signaled when the thread transitions to Dead.
    done: Condvar,
}

impl ThreadResult {
    fn new() -> Self {
        ThreadResult {
            value: Mutex::new(None),
            done: Condvar::new(),
        }
    }

    /// Store the result and notify all waiters.
    fn complete(&self, val: BlissVal) {
        let mut guard = self.value.lock().unwrap();
        *guard = Some(val);
        self.done.notify_all();
    }

    /// Block until the result is available, then return it.
    fn wait(&self) -> BlissVal {
        let mut guard = self.value.lock().unwrap();
        while guard.is_none() {
            guard = self.done.wait(guard).unwrap();
        }
        guard.unwrap()
    }

    /// Check if the thread has finished without blocking.
    #[allow(dead_code)]
    fn is_done(&self) -> bool {
        self.value.lock().unwrap().is_some()
    }
}

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
            result: Arc::new(ThreadResult::new()),
            interrupt_pending: AtomicBool::new(false),
            interrupt_value: Mutex::new(crate::value::NIL),
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
    /// Shared result cell — written by the executing thread, read by joiners.
    result: Arc<ThreadResult>,
    /// Flag indicating an interrupt has been requested.
    interrupt_pending: AtomicBool,
    /// The condition value to deliver on interrupt.
    interrupt_value: Mutex<BlissVal>,
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

    /// Set the thread state.
    fn set_state(&self, new_state: ThreadState) {
        *self.state.lock().unwrap() = new_state;
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

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: BlissVal) {
        let mut tls = self.tls.lock().unwrap();
        if (index as usize) < tls.len() {
            tls[index as usize] = value;
        }
    }

    /// Check whether an interrupt is pending for this thread.
    pub fn has_interrupt(&self) -> bool {
        self.interrupt_pending.load(Ordering::Acquire)
    }

    /// Consume and return the pending interrupt condition, clearing the flag.
    /// Returns `None` if no interrupt is pending.
    pub fn take_interrupt(&self) -> Option<BlissVal> {
        if self.interrupt_pending.swap(false, Ordering::AcqRel) {
            let val = *self.interrupt_value.lock().unwrap();
            Some(val)
        } else {
            None
        }
    }

    /// Deliver an interrupt condition to this thread.
    fn post_interrupt(&self, condition: BlissVal) {
        *self.interrupt_value.lock().unwrap() = condition;
        self.interrupt_pending.store(true, Ordering::Release);
    }
}

/// An OS-level worker thread in the worker pool.
pub struct WorkerThread {
    _private: (),
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state and is immediately scheduled
/// on an OS worker thread. The entry value is treated as the thread's
/// body; in the current bootstrap runtime the entry value is not
/// callable, so the thread immediately completes with NIL as its result.
/// A full evaluator would invoke `entry` as a zero-argument function.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    let id = GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let result_cell = Arc::new(ThreadResult::new());
    let thread = Arc::new(GreenThread {
        id,
        state: Mutex::new(ThreadState::Runnable),
        stack: BlissStack::new(DEFAULT_STACK_SIZE),
        tls: Mutex::new(vec![crate::value::NIL; MAX_TLS]),
        result: Arc::clone(&result_cell),
        interrupt_pending: AtomicBool::new(false),
        interrupt_value: Mutex::new(crate::value::NIL),
    });

    // Register the thread before spawning so it is visible to other threads.
    thread_registry()
        .lock()
        .unwrap()
        .insert(id, Arc::clone(&thread));

    // Clone what we need for the OS thread closure.
    let thread_handle = Arc::clone(&thread);
    let _entry = entry;

    // Spawn an OS worker thread that runs the green thread's body.
    std::thread::Builder::new()
        .name(format!("bliss-green-{}", id.0))
        .spawn(move || {
            // The thread is now running.
            thread_handle.set_state(ThreadState::Runnable);

            // In a full implementation this would invoke `_entry` through
            // the evaluator.  For the bootstrap runtime we simply produce
            // NIL as the result since we have no evaluator to call the
            // entry function.  Any pending interrupt is checked and would
            // be delivered here in a full implementation.
            let result_val = crate::value::NIL;

            // Mark thread as dead and publish the result.
            thread_handle.set_state(ThreadState::Dead);
            result_cell.complete(result_val);
        })
        .map_err(|e| BlissError::Internal(format!("failed to spawn thread: {}", e)))?;

    Ok(id)
}

/// Wait for a green thread to finish, returning its result value.
///
/// Blocks the calling thread until the target green thread transitions
/// to `Dead` state and its result is available. Returns the result
/// value that the thread's entry function produced. If the thread ID
/// is not found in the registry, returns an error.
pub fn join_thread(id: GreenThreadId) -> Result<BlissVal, BlissError> {
    // Look up the thread descriptor to get its result cell.
    let result_cell = {
        let registry = thread_registry().lock().unwrap();
        match registry.get(&id) {
            Some(thread) => Arc::clone(&thread.result),
            None => {
                return Err(BlissError::Internal(format!(
                    "no thread with id {}",
                    id.0
                )));
            }
        }
    };

    // Block until the thread completes, then return the result.
    let val = result_cell.wait();
    Ok(val)
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
///
/// Hints to the OS scheduler that this thread is willing to give up
/// its time slice. In the M:N model this would switch to the next
/// green thread on the same worker; in the bootstrap implementation
/// it delegates to `std::thread::yield_now()`.
pub fn thread_yield() {
    std::thread::yield_now();
}

/// Interrupt a green thread, delivering a condition to it.
///
/// Sets the interrupt-pending flag on the target thread and stores the
/// condition value. The target thread will observe the interrupt at its
/// next safepoint poll (or when it calls `take_interrupt`). If the
/// thread ID is not found, returns an error.
pub fn interrupt_thread(id: GreenThreadId, condition: BlissVal) -> Result<(), BlissError> {
    let registry = thread_registry().lock().unwrap();
    match registry.get(&id) {
        Some(thread) => {
            thread.post_interrupt(condition);
            Ok(())
        }
        None => Err(BlissError::Internal(format!(
            "no thread with id {}",
            id.0
        ))),
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
