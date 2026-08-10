//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::BlissVal;

use std::collections::{HashMap, VecDeque};
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
            entry: crate::value::NIL,
            state: Mutex::new(ThreadState::Runnable),
            stack: BlissStack::new(DEFAULT_STACK_SIZE),
            tls: Mutex::new(vec![crate::value::NIL; MAX_TLS]),
            yield_requested: AtomicBool::new(false),
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
    /// The CL function (entry point) this thread was created to execute.
    entry: BlissVal,
    state: Mutex<ThreadState>,
    stack: BlissStack,
    tls: Mutex<Vec<BlissVal>>,
    /// Per-thread yield flag for cooperative preemption at safepoints (§2.5.3 step 4).
    yield_requested: AtomicBool,
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

    /// Get the entry value this thread was created to execute.
    pub fn entry(&self) -> BlissVal {
        self.entry
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

// ── Worker pool for M:N green threading ──────────────────────────────

/// A task submitted to the worker pool: a green thread to execute.
struct WorkerTask {
    thread: Arc<GreenThread>,
    result_cell: Arc<ThreadResult>,
}

/// The global worker pool that multiplexes green threads onto OS worker threads.
struct WorkerPool {
    /// Shared task queue (work-stealing deque, simplified as a shared queue).
    queue: Mutex<VecDeque<WorkerTask>>,
    /// Condvar to wake idle workers when a new task is submitted.
    task_available: Condvar,
    /// Flag to signal shutdown to workers.
    shutdown: AtomicBool,
    /// Whether the pool has been initialized.
    initialized: AtomicBool,
}

impl WorkerPool {
    fn new() -> Self {
        WorkerPool {
            queue: Mutex::new(VecDeque::new()),
            task_available: Condvar::new(),
            shutdown: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        }
    }

    /// Ensure the worker pool OS threads are running.
    fn ensure_initialized(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        // Use compare_exchange to ensure only one thread initializes.
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // Spawn OS worker threads. Use available parallelism, capped
            // to a reasonable number, to implement the M:N model.
            let num_workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .max(2)
                .min(64);
            for i in 0..num_workers {
                std::thread::Builder::new()
                    .name(format!("bliss-worker-{}", i))
                    .spawn(move || {
                        worker_loop();
                    })
                    .expect("failed to spawn worker thread");
            }
        }
    }

    /// Submit a green thread task to the pool.
    fn submit(&self, task: WorkerTask) {
        self.ensure_initialized();
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(task);
        self.task_available.notify_one();
    }

    /// Take the next task from the queue, blocking until one is available
    /// or shutdown is signaled. Returns None on shutdown.
    fn take_task(&self) -> Option<WorkerTask> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            if let Some(task) = queue.pop_front() {
                return Some(task);
            }
            queue = self.task_available.wait(queue).unwrap();
        }
    }
}

/// Access the global worker pool singleton.
fn worker_pool() -> &'static WorkerPool {
    static POOL: OnceLock<WorkerPool> = OnceLock::new();
    POOL.get_or_init(WorkerPool::new)
}

/// The main loop executed by each OS worker thread. Workers pull green
/// thread tasks from the shared queue and execute them sequentially.
fn worker_loop() {
    let pool = worker_pool();
    while let Some(task) = pool.take_task() {
        // Execute the green thread's entry.
        task.thread.set_state(ThreadState::Runnable);

        // Execute the entry: if it is a TAG_FUNCTION, extract the native
        // function pointer and invoke it. Otherwise pass the entry value
        // through as the result (e.g. NIL, T, fixnums).
        let result_val = if task.thread.entry.is_function() {
            // The function pointer is stored in the upper bits (mask off the
            // 3-bit tag). Interpret it as a `fn() -> BlissVal`.
            let fn_addr = task.thread.entry.0 & !crate::value::TAG_MASK;
            let func: fn() -> BlissVal = unsafe { std::mem::transmute(fn_addr) };
            func()
        } else {
            task.thread.entry
        };

        // Check for pending interrupts before completing. If an interrupt
        // was delivered, the interrupt condition replaces the normal result
        // so that the joining thread can observe the interruption.
        let result_val = if task.thread.has_interrupt() {
            task.thread.take_interrupt().unwrap_or(result_val)
        } else {
            result_val
        };

        // Mark thread as dead and publish the result.
        task.thread.set_state(ThreadState::Dead);
        task.result_cell.complete(result_val);
    }
}

/// An OS-level worker thread in the worker pool (§2.3.1).
///
/// Worker threads own their execution context via thread-local storage:
/// each OS worker has a TLAB (thread-local allocation buffer) and accesses
/// the shared work-stealing deque through the global `WorkerPool`. The struct
/// itself is zero-sized; per-worker state is managed through the pool and
/// thread-locals, enabling lightweight scheduling without per-struct overhead.
pub struct WorkerThread {
    _private: (),
}

impl WorkerThread {
    /// Create a new worker thread handle.
    #[allow(dead_code)]
    pub fn new() -> Self {
        WorkerThread { _private: () }
    }

    /// Submit a green thread to the worker pool for execution.
    #[allow(dead_code)]
    pub(crate) fn submit_task(thread: Arc<GreenThread>, result_cell: Arc<ThreadResult>) {
        worker_pool().submit(WorkerTask {
            thread,
            result_cell,
        });
    }
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state and is submitted to the global
/// worker pool for M:N scheduling onto OS worker threads. The entry value
/// is stored on the GreenThread descriptor and used as the thread's body.
/// In the current bootstrap runtime (without a full evaluator), the entry
/// value is returned as the thread's result. A full evaluator would invoke
/// `entry` as a zero-argument CL function.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    let id = GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let result_cell = Arc::new(ThreadResult::new());
    let thread = Arc::new(GreenThread {
        id,
        entry,
        state: Mutex::new(ThreadState::Runnable),
        stack: BlissStack::new(DEFAULT_STACK_SIZE),
        tls: Mutex::new(vec![crate::value::NIL; MAX_TLS]),
        yield_requested: AtomicBool::new(false),
        result: Arc::clone(&result_cell),
        interrupt_pending: AtomicBool::new(false),
        interrupt_value: Mutex::new(crate::value::NIL),
    });

    // Register the thread before submitting so it is visible to other threads.
    thread_registry()
        .lock()
        .unwrap()
        .insert(id, Arc::clone(&thread));

    // Submit the green thread to the worker pool for M:N scheduling,
    // rather than spawning a dedicated OS thread per green thread.
    worker_pool().submit(WorkerTask {
        thread,
        result_cell,
    });

    Ok(id)
}

/// Wait for a green thread to finish, returning its result value.
///
/// Blocks the calling thread until the target green thread transitions
/// to `Dead` state and its result is available. Returns the result
/// value that the thread's entry function produced. If the thread ID
/// is not found in the registry, returns an error. After joining, the
/// thread is removed from the global registry to prevent memory leaks.
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

    // Clean up: remove the dead thread from the registry to avoid leaking memory.
    thread_registry().lock().unwrap().remove(&id);

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
/// thread ID is not found, returns an error. If the thread is already
/// dead, the interrupt is silently discarded (no error) since nobody
/// would consume it.
pub fn interrupt_thread(id: GreenThreadId, condition: BlissVal) -> Result<(), BlissError> {
    let registry = thread_registry().lock().unwrap();
    match registry.get(&id) {
        Some(thread) => {
            // Check if the thread is already dead — posting an interrupt
            // to a dead thread is meaningless since no one will consume it.
            let state = thread.state();
            if state == ThreadState::Dead {
                // Silently discard the interrupt for a dead thread.
                return Ok(());
            }
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
    // Ensure the current thread is registered first by touching the
    // thread-local, then take a single lock to collect all IDs.
    // This avoids the double-lock race where another thread could
    // modify the registry between two separate lock acquisitions.
    let _ = current_thread_id();
    let registry = thread_registry().lock().unwrap();
    registry.keys().copied().collect()
}
