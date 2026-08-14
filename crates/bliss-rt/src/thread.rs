//! Green thread model — M:N threading with work-stealing scheduler.
//!
//! See §2.3 of the spec.

use crate::error::BlissError;
use crate::stack::BlissStack;
use crate::value::{BlissVal, NIL};

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

/// Usable `BlissStack` size for a green thread, honouring `BLISS_STACK_SIZE`
/// (accepts a raw byte count or a `k`/`m`/`g` suffix), defaulting to 512 KiB.
///
/// Deep interpreted recursion is bounded by this size once CL activations live
/// on the `BlissStack` (bliss-nmq): overflow raises `STORAGE-CONDITION` (R2.20).
fn default_stack_size() -> usize {
    fn parse_size(s: &str) -> Option<usize> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (num, mult) = match s.chars().last().unwrap().to_ascii_lowercase() {
            'k' => (&s[..s.len() - 1], 1024),
            'm' => (&s[..s.len() - 1], 1024 * 1024),
            'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
            _ => (s, 1),
        };
        num.trim().parse::<usize>().ok().map(|n| n * mult)
    }
    std::env::var("BLISS_STACK_SIZE")
        .ok()
        .and_then(|v| parse_size(&v))
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_STACK_SIZE)
}

/// Atomic counter for generating unique thread IDs.
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

/// Holds the result of a thread's execution and a signal for completion.
pub(crate) struct ThreadResult {
    /// The result value, set when the thread finishes.
    value: Mutex<Option<Result<BlissVal, BlissError>>>,
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
    fn complete(&self, val: Result<BlissVal, BlissError>) {
        let mut guard = self.value.lock().unwrap();
        *guard = Some(val);
        self.done.notify_all();
    }

    /// Block until the result is available, then return it.
    fn wait(&self) -> Result<BlissVal, BlissError> {
        let mut guard = self.value.lock().unwrap();
        while guard.is_none() {
            guard = self.done.wait(guard).unwrap();
        }
        guard.take().unwrap()
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
            entry: NIL,
            state: Mutex::new(ThreadState::Runnable),
            stack: BlissStack::new(default_stack_size()),
            tls: Mutex::new(vec![NIL; MAX_TLS]),
            yield_requested: AtomicBool::new(false),
            result: Arc::new(ThreadResult::new()),
            interrupt_pending: AtomicBool::new(false),
            interrupt_value: Mutex::new(NIL),
        });
        thread_registry().lock().unwrap().insert(id, Arc::clone(&thread));
        thread
    };
    static ACTIVE_GREEN_THREAD: RefCell<Option<Arc<GreenThread>>> = const { RefCell::new(None) };
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
    pub(crate) fn set_state(&self, new_state: ThreadState) {
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

/// One native worker's run queue: a work-stealing deque (bliss-jtc.14.1). The
/// owning worker pushes/pops at the **back** (LIFO — good locality and depth-
/// first evaluation); other idle workers steal from the **front** (FIFO — the
/// oldest, most likely independent work). Each deque has its own lock, so normal
/// scheduling never contends on a single global run-queue mutex (spec §2.3/§13).
struct Worker {
    local: Mutex<VecDeque<WorkerTask>>,
}

thread_local! {
    /// The pool-worker index of this OS thread, if it is a pool worker. Lets a
    /// worker submit follow-on work to its own deque for locality.
    static WORKER_INDEX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The global worker pool that multiplexes green threads onto OS worker threads
/// via per-worker work-stealing deques.
struct WorkerPool {
    workers: Vec<Worker>,
    /// Parking coordination for idle workers (separate from the run queues).
    park_mutex: Mutex<()>,
    park_cv: Condvar,
    /// Flag to signal shutdown to workers.
    shutdown: AtomicBool,
    /// Whether the pool's OS threads have been spawned.
    initialized: AtomicBool,
    /// Round-robin cursor for submissions from non-worker (external) threads.
    next: AtomicUsize,
}

impl WorkerPool {
    fn new() -> Self {
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 64);
        let workers = (0..num_workers)
            .map(|_| Worker {
                local: Mutex::new(VecDeque::new()),
            })
            .collect();
        WorkerPool {
            workers,
            park_mutex: Mutex::new(()),
            park_cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            next: AtomicUsize::new(0),
        }
    }

    /// Ensure the worker pool OS threads are running (one per deque).
    fn ensure_initialized(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            for i in 0..self.workers.len() {
                std::thread::Builder::new()
                    .name(format!("bliss-worker-{i}"))
                    .spawn(move || worker_loop(i))
                    .expect("failed to spawn worker thread");
            }
        }
    }

    /// Submit a green thread task. A worker submits to its own deque (locality);
    /// an external thread round-robins across workers. Wakes one parked worker.
    fn submit(&self, task: WorkerTask) {
        self.ensure_initialized();
        let idx = WORKER_INDEX
            .with(|w| w.get())
            .filter(|&i| i < self.workers.len())
            .unwrap_or_else(|| self.next.fetch_add(1, Ordering::Relaxed) % self.workers.len());
        self.workers[idx].local.lock().unwrap().push_back(task);
        self.park_cv.notify_one();
    }

    /// Pop this worker's own task (LIFO), else steal one (FIFO) from another
    /// worker's deque. Returns `None` only when every deque is empty.
    fn pop_or_steal(&self, idx: usize) -> Option<WorkerTask> {
        if let Some(t) = self.workers[idx].local.lock().unwrap().pop_back() {
            return Some(t);
        }
        let n = self.workers.len();
        for k in 1..n {
            let victim = (idx + k) % n;
            if let Some(t) = self.workers[victim].local.lock().unwrap().pop_front() {
                return Some(t);
            }
        }
        None
    }

    fn any_work(&self) -> bool {
        self.workers
            .iter()
            .any(|w| !w.local.lock().unwrap().is_empty())
    }

    /// Signal all workers to exit at their next scheduling point.
    #[allow(dead_code)]
    fn shutdown_now(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.park_cv.notify_all();
    }
}

/// Access the global worker pool singleton.
fn worker_pool() -> &'static WorkerPool {
    static POOL: OnceLock<WorkerPool> = OnceLock::new();
    POOL.get_or_init(WorkerPool::new)
}

/// The main loop executed by each OS worker thread: run local work LIFO, steal
/// FIFO when idle, and park when every deque is empty (bliss-jtc.14.1).
fn worker_loop(idx: usize) {
    WORKER_INDEX.with(|w| w.set(Some(idx)));
    let pool = worker_pool();
    loop {
        if pool.shutdown.load(Ordering::Acquire) {
            return;
        }
        if let Some(task) = pool.pop_or_steal(idx) {
            run_worker_task(task);
            continue;
        }
        // Nothing runnable: park until woken by a submission or a short timeout.
        // The timeout bounds any wakeup lost in the window between the empty scan
        // above and the wait below, so a worker can never sleep through work.
        let guard = pool.park_mutex.lock().unwrap();
        if pool.shutdown.load(Ordering::Acquire) {
            return;
        }
        if pool.any_work() {
            continue;
        }
        let _ = pool
            .park_cv
            .wait_timeout(guard, std::time::Duration::from_millis(5));
    }
}

/// Run one green thread to completion on the current worker.
fn run_worker_task(task: WorkerTask) {
    task.thread.set_state(ThreadState::Runnable);
    let thread = Arc::clone(&task.thread);
    ACTIVE_GREEN_THREAD.with(|slot| {
        *slot.borrow_mut() = Some(Arc::clone(&thread));
    });

    let result = run_green_thread_entry(&thread);

    ACTIVE_GREEN_THREAD.with(|slot| {
        *slot.borrow_mut() = None;
    });

    task.thread.set_state(ThreadState::Dead);
    task.result_cell.complete(result);
}

fn run_green_thread_entry(thread: &GreenThread) -> Result<BlissVal, BlissError> {
    let mut result = if thread.entry.is_function() {
        let fn_addr = thread.entry.0 & !crate::value::TAG_MASK;
        let func: fn() -> BlissVal = unsafe { std::mem::transmute(fn_addr) };
        Ok(func())
    } else if matches!(
        thread.entry.0,
        crate::value::NIL_BITS | crate::value::T_BITS
    ) {
        Ok(thread.entry)
    } else {
        Err(BlissError::TypeError {
            datum: thread.entry,
            expected: "function".to_string(),
        })
    };

    if thread.has_interrupt() {
        result = Ok(thread.take_interrupt().unwrap_or(NIL));
    }

    result
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

impl Default for WorkerThread {
    fn default() -> Self {
        Self::new()
    }
}

// ── Thread creation and management ─────────────────────────────────

/// Create a new green thread that will execute `entry`.
/// The thread starts in `Runnable` state and is submitted to the global
/// worker pool for M:N scheduling onto OS worker threads. The entry value
/// is stored on the GreenThread descriptor and invoked as a zero-argument
/// CL function when scheduled.
pub fn make_thread(entry: BlissVal) -> Result<GreenThreadId, BlissError> {
    let id = GreenThreadId(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let result_cell = Arc::new(ThreadResult::new());
    let thread = Arc::new(GreenThread {
        id,
        entry,
        state: Mutex::new(ThreadState::Runnable),
        stack: BlissStack::new(default_stack_size()),
        tls: Mutex::new(vec![NIL; MAX_TLS]),
        yield_requested: AtomicBool::new(false),
        result: Arc::clone(&result_cell),
        interrupt_pending: AtomicBool::new(false),
        interrupt_value: Mutex::new(NIL),
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
                return Err(BlissError::Internal(format!("no thread with id {}", id.0)));
            }
        }
    };

    // Block until the thread completes, then return the result.
    let val = result_cell.wait()?;

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
    if let Some(active) = ACTIVE_GREEN_THREAD.with(|slot| slot.borrow().clone()) {
        unsafe { &*(Arc::as_ptr(&active)) }
    } else {
        CURRENT_THREAD.with(|t| unsafe { &*(Arc::as_ptr(t)) })
    }
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
        None => Err(BlissError::Internal(format!("no thread with id {}", id.0))),
    }
}

/// List all live green thread IDs.
pub fn all_thread_ids() -> Vec<GreenThreadId> {
    // Ensure the current thread is registered first by touching the
    // thread-local, then take a single lock to collect all IDs.
    // This avoids the double-lock race where another thread could
    // modify the registry between two separate lock acquisitions.
    let _ = current_thread_id();
    let mut registry = thread_registry().lock().unwrap();
    prune_orphaned_threads(&mut registry, None);
    registry.keys().copied().collect()
}

/// The frame pointer a green thread published at its last safepoint, for
/// precise CL-stack scanning by the GC (nmq.3). Returns `None` if the thread is
/// not registered.
pub fn thread_published_fp(id: GreenThreadId) -> Option<*const crate::stack::Frame> {
    let registry = thread_registry().lock().ok()?;
    registry.get(&id).map(|t| t.stack().published_fp())
}

/// Count threads that must participate in a safepoint handshake.
///
/// Native threads are intentionally excluded: while they are inside foreign
/// code they cannot poll, and the safepoint protocol must not wait for them.
pub fn safepoint_participant_count_excluding(current: GreenThreadId) -> usize {
    let _ = current_thread_id();
    let mut registry = thread_registry().lock().unwrap();
    prune_orphaned_threads(&mut registry, Some(current));
    registry
        .iter()
        .filter(|(id, thread)| {
            **id != current
                && thread.state() != ThreadState::Dead
                && thread.state() != ThreadState::Native
        })
        .count()
}

fn prune_orphaned_threads(
    registry: &mut HashMap<GreenThreadId, Arc<GreenThread>>,
    current: Option<GreenThreadId>,
) {
    registry.retain(|id, thread| {
        if Some(*id) == current {
            return true;
        }

        // Thread-local CURRENT_THREAD entries can outlive a runtime instance:
        // once the owning OS thread exits, the registry may be the last owner.
        // Those orphaned entries are not runnable work and must not block
        // shutdown or appear as live threads in subsequent runtimes/tests.
        Arc::strong_count(thread) > 1 || thread.state() == ThreadState::Dead
    });
}

/// Wait until every other registered green thread has finished executing.
///
/// Unlike `join_thread`, this preserves each thread's result so callers may
/// still join later and observe the completed value.
pub fn wait_for_other_threads() {
    let current = current_thread_id();
    loop {
        let pending = {
            let mut registry = thread_registry().lock().unwrap();
            prune_orphaned_threads(&mut registry, Some(current));
            registry
                .iter()
                .filter(|(id, thread)| {
                    // Only wait for green threads this runtime actually spawned
                    // (those created by `make_thread`, which carry a real entry
                    // function). Bootstrap CURRENT_THREAD entries created lazily
                    // by *other* OS threads have a NIL entry and are never driven
                    // to Dead by this runtime, so waiting on them would livelock
                    // shutdown whenever another runtime/test thread coexists.
                    **id != current && !thread.entry().is_nil()
                })
                .any(|(_, thread)| thread.state() != ThreadState::Dead)
        };
        if !pending {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
