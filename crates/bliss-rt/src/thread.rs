//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::BlissVal;

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

/// Internal green thread data.
struct GreenThreadInner {
    id: GreenThreadId,
    state: ThreadState,
    stack: BlissStack,
    tls: Vec<BlissVal>,
    _entry: BlissVal,
}

/// Green thread descriptor. D2.01.
pub struct GreenThread {
    inner: UnsafeCell<GreenThreadInner>,
}

// Safety: GreenThread access is controlled by the scheduler and thread registry.
unsafe impl Send for GreenThread {}
unsafe impl Sync for GreenThread {}

impl GreenThread {
    /// Get this thread's unique ID.
    pub fn id(&self) -> GreenThreadId {
        unsafe { (*self.inner.get()).id }
    }

    /// Get the current state of this thread.
    pub fn state(&self) -> ThreadState {
        unsafe { (*self.inner.get()).state }
    }

    /// Get a reference to this thread's CL stack.
    pub fn stack(&self) -> &BlissStack {
        unsafe { &(*self.inner.get()).stack }
    }

    /// Get this thread's TLS slot at the given index.
    pub fn tls_get(&self, index: u32) -> BlissVal {
        unsafe {
            let inner = &*self.inner.get();
            if (index as usize) < inner.tls.len() {
                inner.tls[index as usize]
            } else {
                crate::value::NIL
            }
        }
    }

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: BlissVal) {
        unsafe {
            let inner = &mut *self.inner.get();
            let idx = index as usize;
            if idx >= inner.tls.len() {
                inner.tls.resize(idx + 1, crate::value::NIL);
            }
            inner.tls[idx] = value;
        }
    }
}

/// An OS-level worker thread in the worker pool.
pub struct WorkerThread {
    _private: (),
}

// ── Global thread registry ────────────────────────────────────────

static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

struct ThreadRegistry {
    threads: HashMap<u64, Arc<GreenThread>>,
}

static REGISTRY: std::sync::LazyLock<Mutex<ThreadRegistry>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(ThreadRegistry {
            threads: HashMap::new(),
        })
    });

fn allocate_thread_id() -> GreenThreadId {
    GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_green_thread(entry: BlissVal) -> Arc<GreenThread> {
    let id = allocate_thread_id();
    Arc::new(GreenThread {
        inner: UnsafeCell::new(GreenThreadInner {
            id,
            state: ThreadState::Runnable,
            stack: BlissStack::new(512 * 1024), // 512 KiB default
            tls: Vec::new(),
            _entry: entry,
        }),
    })
}

// ── Thread-local current thread ───────────────────────────────────

thread_local! {
    static CURRENT_THREAD_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Cached Arc to the current thread, avoiding repeated registry lookups.
    static CURRENT_THREAD_ARC: std::cell::RefCell<Option<Arc<GreenThread>>> =
        std::cell::RefCell::new(None);
}

fn ensure_current_thread() {
    CURRENT_THREAD_ID.with(|cell| {
        if cell.get() == 0 {
            // Bootstrap: create a thread for the current OS thread.
            let thread = new_green_thread(crate::value::NIL);
            let id = thread.id().0;
            CURRENT_THREAD_ARC.with(|arc_cell| {
                *arc_cell.borrow_mut() = Some(Arc::clone(&thread));
            });
            let mut reg = REGISTRY.lock().unwrap();
            reg.threads.insert(id, thread);
            cell.set(id);
        }
    });
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    let thread = new_green_thread(entry);
    let id = thread.id();
    let mut reg = REGISTRY.lock().unwrap();
    reg.threads.insert(id.0, thread);
    Ok(id)
}

/// Wait for a green thread to finish, returning its result value.
/// For now, transitions the thread to Dead and returns NIL.
pub fn join_thread(id: GreenThreadId) -> Result<BlissVal, BlissError> {
    let reg = REGISTRY.lock().unwrap();
    if let Some(thread) = reg.threads.get(&id.0) {
        // Mark thread as dead (joined).
        unsafe { (*thread.inner.get()).state = ThreadState::Dead; }
        Ok(crate::value::NIL)
    } else {
        Err(BlissError::Internal(format!("no such thread: {}", id.0)))
    }
}

/// Get the current green thread's ID.
pub fn current_thread_id() -> GreenThreadId {
    ensure_current_thread();
    CURRENT_THREAD_ID.with(|cell| GreenThreadId(cell.get()))
}

/// Get a reference to the current green thread.
///
/// # Safety rationale
/// The GreenThread is held in an `Arc` stored both in the global registry
/// and in a thread-local cache (`CURRENT_THREAD_ARC`). The thread-local
/// Arc clone keeps the allocation alive for at least the lifetime of the
/// OS thread, so the returned `&'static` reference is valid as long as
/// the calling OS thread is alive — which is always true for code running
/// on that thread.
pub fn current_thread() -> &'static GreenThread {
    ensure_current_thread();
    CURRENT_THREAD_ARC.with(|arc_cell| {
        let borrow = arc_cell.borrow();
        let arc = borrow.as_ref().expect("current thread Arc not set");
        // Safety: The Arc in the thread-local keeps the GreenThread alive for
        // the lifetime of this OS thread. Since we only return this reference
        // on the same OS thread, the 'static lifetime is sound.
        let ptr: *const GreenThread = Arc::as_ptr(arc);
        unsafe { &*ptr }
    })
}

/// Yield the current green thread at the next safepoint.
pub fn thread_yield() {
    // In a full implementation, this would set a yield flag checked at
    // the next safepoint poll, causing the scheduler to preempt this thread.
    // For cooperative scheduling, this is currently a hint/no-op.
}

/// Interrupt a green thread, delivering a condition to it.
pub fn interrupt_thread(id: GreenThreadId, _condition: BlissVal) -> Result<(), BlissError> {
    let reg = REGISTRY.lock().unwrap();
    if reg.threads.contains_key(&id.0) {
        // In a full implementation, this would set an interrupt flag on the
        // target thread, which would be checked at the next safepoint.
        Ok(())
    } else {
        Err(BlissError::Internal(format!("no such thread: {}", id.0)))
    }
}

/// List all live green thread IDs.
pub fn all_thread_ids() -> Vec<GreenThreadId> {
    ensure_current_thread();
    let reg = REGISTRY.lock().unwrap();
    reg.threads.keys().map(|&id| GreenThreadId(id)).collect()
}
