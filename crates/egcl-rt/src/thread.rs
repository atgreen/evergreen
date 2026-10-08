// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! JVM-style native thread and fiber model.
//!
//! See §2.3 of the spec.

use crate::debug_stack::{FrameOrigin, RecordedCalls};
use crate::error::EgclError;
use crate::gc::{TraceHostRoots, register_root_scanner};
use crate::lock_order::{LockLevel, OrderedMutex};
use crate::stack::EgclStack;
use crate::value::{EgclVal, NIL};

use std::cell::RefCell;
// Unix fibers own a saved stack pointer updated at each context switch.
#[cfg(egcl_unix_fibers)]
use std::cell::UnsafeCell;
use std::collections::{HashMap, VecDeque};
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicIsize, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering, fence,
};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

#[cfg(all(target_arch = "x86_64", windows))]
#[path = "thread/windows.rs"]
mod windows;
#[cfg(all(target_arch = "x86_64", windows))]
use windows::FiberExecutionContext;

/// Unique identifier for a lightweight managed fiber.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FiberId(pub u64);

/// Whether this target has a real stackful fiber backend.
pub const FIBERS_SUPPORTED: bool = cfg!(egcl_fibers);

/// Managed fiber lifecycle states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FiberState {
    /// Allocated but not submitted to a scheduler group.
    Created = 0,
    /// On a carrier's run queue.
    Runnable = 1,
    /// Mounted and executing on exactly one carrier.
    Running = 2,
    /// Cooperatively yielded with a saved continuation.
    Suspended = 3,
    /// Waiting on a mutex, condition variable, or channel.
    Blocked = 4,
    /// Executing a C FFI call; GC scans published roots without waiting.
    Native = 5,
    /// Blocked on async I/O.
    Waiting = 6,
    /// Entry function returned or the fiber was terminated.
    Dead = 7,
}

const SUSPEND_NONE: u8 = 0;
const SUSPEND_YIELD: u8 = 1;
const SUSPEND_PARK: u8 = 2;
const SUSPEND_DEAD: u8 = 3;

/// Resume state saved when a fiber unmounts from a carrier.
#[derive(Debug, Default)]
pub struct FiberContinuation {
    saved_sp: AtomicUsize,
    saved_fp: AtomicUsize,
    resume_token: AtomicUsize,
}

#[cfg(egcl_unix_fibers)]
struct FiberExecutionContext {
    // The fiber's saved stack pointer. Updated in place each time the fiber
    // suspends (crate::context::swap writes through this cell).
    context: Box<UnsafeCell<crate::context::Context>>,
    _native_stack: EgclStack,
}

#[cfg(egcl_unix_fibers)]
impl FiberExecutionContext {
    fn new(stack_size: usize) -> Result<Self, EgclError> {
        let native_stack = EgclStack::new(stack_size);
        let page = crate::syscall::page_size();
        let guard = unsafe { native_stack.base().sub(page) as *mut u8 };
        // EgclStack protects the high end for upward-growing Lisp frames.
        // A host stack grows downward, so protect the low reservation too.
        unsafe { crate::syscall::mprotect(guard, page, crate::syscall::PROT_NONE) }
            .map_err(|error| EgclError::Internal(format!("fiber stack guard: {error}")))?;
        crate::runtime::register_sigsegv_stack_guard_range(guard as usize, page);
        // Portable context switch (no libc ucontext): lay down an initial frame
        // on the native stack that enters the trampoline on first swap-in.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(native_stack.base() as *mut u8, stack_size) };
        let sp = crate::context::make(bytes, fiber_context_trampoline);
        Ok(Self {
            context: Box::new(UnsafeCell::new(sp)),
            _native_stack: native_stack,
        })
    }

    fn as_ptr(&self) -> *mut crate::context::Context {
        self.context.get()
    }
}

#[cfg(egcl_unix_fibers)]
impl Drop for FiberExecutionContext {
    fn drop(&mut self) {
        let low = self._native_stack.base() as usize - crate::syscall::page_size();
        crate::runtime::unregister_sigsegv_stack_guard_range(low);
    }
}

#[cfg(not(egcl_fibers))]
struct FiberExecutionContext;

#[cfg(not(egcl_fibers))]
impl FiberExecutionContext {
    fn new(_stack_size: usize) -> Result<Self, EgclError> {
        Ok(Self)
    }
}

impl FiberContinuation {
    /// Publish the managed stack position and interpreter/native resume token.
    pub fn save(&self, sp: usize, fp: usize, resume_token: usize) {
        self.saved_sp.store(sp, Ordering::Release);
        self.saved_fp.store(fp, Ordering::Release);
        self.resume_token.store(resume_token, Ordering::Release);
    }

    /// Return `(sp, fp, resume-token)` from the last suspension point.
    pub fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.saved_sp.load(Ordering::Acquire),
            self.saved_fp.load(Ordering::Acquire),
            self.resume_token.load(Ordering::Acquire),
        )
    }
}

/// Maximum number of TLS slots per fiber.
pub const MAX_TLS: usize = 4096;

/// Default stack size for fibers (512 KiB).
const DEFAULT_STACK_SIZE: usize = 512 * 1024;

/// Usable `EgclStack` size for a fiber, honouring `EGCL_STACK_SIZE`
/// (accepts a raw byte count or a `k`/`m`/`g` suffix), defaulting to 512 KiB.
///
/// Deep interpreted recursion is bounded by this size once CL activations live
/// on the `EgclStack` (bliss-nmq): overflow raises `STORAGE-CONDITION` (R2.20).
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
    std::env::var("EGCL_STACK_SIZE")
        .ok()
        .and_then(|v| parse_size(&v))
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_STACK_SIZE)
}

/// Unique identifier for an exposed one-to-one OS native thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeThreadId(pub u64);

/// Lifecycle state of an exposed native/platform thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NativeThreadState {
    Born = 0,
    Running = 1,
    Blocked = 2,
    Native = 3,
    Dead = 4,
    Aborted = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingSignal {
    Interrupt,
    Shutdown,
    NullGuard,
    StackGuard,
    Arithmetic,
    Pipe,
    Timeout,
}

impl PendingSignal {
    fn bit(self) -> u8 {
        match self {
            PendingSignal::Interrupt => 1 << 0,
            PendingSignal::Shutdown => 1 << 1,
            PendingSignal::NullGuard => 1 << 2,
            PendingSignal::StackGuard => 1 << 3,
            PendingSignal::Arithmetic => 1 << 4,
            PendingSignal::Pipe => 1 << 5,
            PendingSignal::Timeout => 1 << 6,
        }
    }

    fn from_bits(bits: u8) -> Option<Self> {
        if bits & PendingSignal::Shutdown.bit() != 0 {
            Some(PendingSignal::Shutdown)
        } else if bits & PendingSignal::Timeout.bit() != 0 {
            Some(PendingSignal::Timeout)
        } else if bits & PendingSignal::NullGuard.bit() != 0 {
            Some(PendingSignal::NullGuard)
        } else if bits & PendingSignal::StackGuard.bit() != 0 {
            Some(PendingSignal::StackGuard)
        } else if bits & PendingSignal::Arithmetic.bit() != 0 {
            Some(PendingSignal::Arithmetic)
        } else if bits & PendingSignal::Pipe.bit() != 0 {
            Some(PendingSignal::Pipe)
        } else if bits & PendingSignal::Interrupt.bit() != 0 {
            Some(PendingSignal::Interrupt)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionHandlerCluster {
    pub frame: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionRestartCluster {
    pub frame: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadConditionState {
    pub handler_stack: Vec<ConditionHandlerCluster>,
    pub restart_stack: Vec<ConditionRestartCluster>,
    pub debugger_hook: Option<EgclVal>,
    pub break_on_signals: Option<EgclVal>,
    pub debugger_invoked: bool,
    pub handler_case_clauses: HashMap<u64, EgclVal>,
    pub pending_handler_case: Option<(EgclVal, EgclVal)>,
    pub next_handler_case_id: i64,
}

impl ThreadConditionState {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TraceHostRoots for ThreadConditionState {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(value) = self.debugger_hook.as_mut() {
            visit(value);
        }
        if let Some(value) = self.break_on_signals.as_mut() {
            visit(value);
        }
        for value in self.handler_case_clauses.values_mut() {
            visit(value);
        }
        if let Some((condition, handler)) = self.pending_handler_case.as_mut() {
            visit(condition);
            visit(handler);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionStateSnapshot {
    pub handler_depth: usize,
    pub restart_depth: usize,
    pub debugger_hook: Option<EgclVal>,
    pub break_on_signals: Option<EgclVal>,
    pub handler_case_clause_count: usize,
    pub pending_handler_case: bool,
}

/// Atomic counters for the two deliberately distinct identity spaces.
static NEXT_NATIVE_THREAD_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_FIBER_ID: AtomicU64 = AtomicU64::new(1);
static FOREGROUND_EXECUTION_KIND: AtomicU8 = AtomicU8::new(0);
static FOREGROUND_EXECUTION_ID: AtomicU64 = AtomicU64::new(0);

const FOREGROUND_NATIVE_THREAD: u8 = 1;
const FOREGROUND_FIBER: u8 = 2;

fn native_object_order(id: NativeThreadId, field: u64) -> u64 {
    id.0.saturating_mul(32).saturating_add(field).max(1)
}

fn fiber_object_order(id: FiberId, field: u64) -> u64 {
    (1_u64 << 62) | id.0.saturating_mul(32).saturating_add(field)
}

/// Completion remains observable after one joiner consumes the result.
struct ThreadCompletion {
    completed: bool,
    value: Option<Result<EgclVal, EgclError>>,
    fiber_waiters: Vec<(FiberId, u64)>,
}

/// Holds a thread's GC-visible result and native/managed completion waiters.
pub(crate) struct ThreadResult {
    completion: Mutex<ThreadCompletion>,
    done: Condvar,
}

impl ThreadResult {
    fn new() -> Self {
        ThreadResult {
            completion: Mutex::new(ThreadCompletion {
                completed: false,
                value: None,
                fiber_waiters: Vec::new(),
            }),
            done: Condvar::new(),
        }
    }

    /// Publish completion before waking either kind of waiter. Wake fibers
    /// without retaining the result mutex: their scheduler uses registry locks.
    fn complete(&self, val: Result<EgclVal, EgclError>) {
        let waiters = {
            let mut completion = self.completion.lock().unwrap();
            completion.value = Some(val);
            completion.completed = true;
            self.done.notify_all();
            std::mem::take(&mut completion.fiber_waiters)
        };
        for (fiber, token) in waiters {
            wake_fiber_wait(fiber, token);
        }
    }

    /// Wait without removing the GC-visible result. A blocked native caller
    /// must resume through the safepoint protocol before moving root slots.
    fn wait_until_ready(&self) {
        let mut completion = self.completion.lock().unwrap();
        while !completion.completed {
            completion = self.done.wait(completion).unwrap();
        }
    }

    /// A consumed result is still completed, so a concurrent join reports
    /// "already joined" rather than mistaking consumption for a timeout.
    fn wait_until_ready_for(&self, timeout: std::time::Duration) -> bool {
        let completion = self.completion.lock().unwrap();
        let (completion, _) = self
            .done
            .wait_timeout_while(completion, timeout, |state| !state.completed)
            .unwrap();
        completion.completed
    }

    fn wait_for_fiber_join(&self) -> Result<(), EgclError> {
        let mut completion = self.completion.lock().unwrap();
        if completion.completed {
            return Ok(());
        }
        match crate::sync::blocking_mode("JOIN-FIBER")? {
            crate::sync::BlockingMode::Native => {
                while !completion.completed {
                    completion = self.done.wait(completion).unwrap();
                }
                Ok(())
            }
            crate::sync::BlockingMode::Fiber => loop {
                // Register and publish Blocked under the completion lock so a
                // concurrent completion cannot pass between the two actions.
                let waiter = prepare_current_fiber_park(FiberState::Blocked)?;
                completion.fiber_waiters.push(waiter);
                drop(completion);
                #[cfg(test)]
                join_completion_tests::before_park();
                let parked = park_prepared_current_fiber();
                completion = self.completion.lock().unwrap();
                completion.fiber_waiters.retain(|entry| *entry != waiter);
                if parked.is_err() {
                    cancel_prepared_current_fiber_park();
                }
                parked?;
                if completion.completed {
                    return Ok(());
                }
                // An explicit unpark can resume us before completion. Recheck
                // and register a fresh generation rather than claiming success.
            },
        }
    }

    fn is_done(&self) -> bool {
        self.completion.lock().unwrap().completed
    }
}

#[cfg(test)]
mod join_completion_tests {
    mod native_entry {
        use crate as egcl_rt;
        include!("../tests/support/native_entry.rs");
    }
    use super::*;
    use crate::value::T;

    thread_local! {
        static RETAINED: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
        static BEFORE_PARK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) fn retained_result() {
        let hook = RETAINED.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
            hook();
        }
    }

    pub(super) fn before_park() {
        let hook = BEFORE_PARK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    fn isolated(name: &str, body: impl FnOnce()) {
        const CHILD: &str = "EGCL_TEST_CONCURRENT_JOIN_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("thread::join_completion_tests::{name}"),
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(10));
            eprintln!("a concurrent joiner treated a consumed result as unfinished");
            std::process::exit(124);
        });
        current_thread_id();
        body();
    }

    fn concurrent_join(fiber: bool) {
        let name = if fiber {
            "consumed_fiber_result_remains_completed"
        } else {
            "consumed_native_result_remains_completed"
        };
        isolated(name, || {
            let pool = fiber.then(|| CarrierPool::new(1));
            let fiber_id = pool.as_ref().map(|pool| {
                let id = make_fiber(T).unwrap();
                pool.submit(id).unwrap();
                id
            });
            let native_id = (!fiber).then(|| make_thread(T).unwrap());
            let (ready, retained) = std::sync::mpsc::channel();
            let (release, proceed) = std::sync::mpsc::channel();
            let second = std::thread::spawn(move || {
                RETAINED.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || {
                        ready.send(()).unwrap();
                        proceed.recv().unwrap();
                    }))
                });
                if let Some(id) = native_id {
                    join_thread_timeout(id, Some(std::time::Duration::from_millis(100)))
                } else {
                    join_fiber(fiber_id.unwrap()).map(Some)
                }
            });
            {
                let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
                retained
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            let first = if let Some(id) = native_id {
                join_thread(id)
            } else {
                join_fiber(fiber_id.unwrap())
            };
            assert_eq!(first.unwrap(), T);
            release.send(()).unwrap();
            let second = {
                let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
                second.join().unwrap()
            };
            if let Some(pool) = pool {
                pool.shutdown_and_join().unwrap();
            }
            assert!(
                matches!(second, Err(EgclError::ProgramError(_))),
                "consumption must be reported as already joined, not pending: {second:?}"
            );
        });
    }

    #[cfg(egcl_fibers)]
    mod managed_races {
        use super::*;
        static RACE_POOL: OnceLock<CarrierPool> = OnceLock::new();
        static CHILD_GATE: OnceLock<crate::sync::EgclSemaphore> = OnceLock::new();
        static EARLY_WAKE: AtomicBool = AtomicBool::new(false);

        fn race_child() -> EgclVal {
            CHILD_GATE.get().unwrap().wait(None).unwrap();
            T
        }

        fn race_parent() -> EgclVal {
            if EARLY_WAKE.load(Ordering::Acquire) {
                BEFORE_PARK.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(|| {
                        CHILD_GATE.get().unwrap().signal(1).unwrap();
                        // Force completion's wake to finish while this parent is still
                        // mounted, before the actual context switch.
                        while !current_fiber()
                            .unwrap()
                            .wake_pending
                            .load(Ordering::Acquire)
                        {
                            std::thread::yield_now();
                        }
                    }))
                });
            }
            let entry = native_entry::entry(race_child);
            let child = make_fiber(entry).unwrap();
            RACE_POOL.get().unwrap().submit(child).unwrap();
            join_fiber(child).unwrap()
        }

        fn completion_race(early: bool) {
            let name = if early {
                "managed_races::completion_before_unmount_does_not_lose_join_wake"
            } else {
                "managed_races::spurious_unpark_rechecks_completion_and_replaces_waiter"
            };
            isolated(name, || {
                EARLY_WAKE.store(early, Ordering::Release);
                assert!(
                    CHILD_GATE
                        .set(crate::sync::EgclSemaphore::new(None, 0).unwrap())
                        .is_ok()
                );
                assert!(RACE_POOL.set(CarrierPool::new(2)).is_ok());
                let entry = native_entry::entry(race_parent);
                let parent = make_fiber(entry).unwrap();
                let descriptor = fiber_registry()
                    .lock()
                    .unwrap()
                    .get(&parent)
                    .unwrap()
                    .clone();
                RACE_POOL.get().unwrap().submit(parent).unwrap();
                if !early {
                    while descriptor.state() != FiberState::Blocked {
                        crate::poll_safepoint();
                        std::thread::yield_now();
                    }
                    let generation = descriptor.wait_generation.load(Ordering::Acquire);
                    assert!(wake_fiber_wait(parent, generation));
                    while descriptor.wait_generation.load(Ordering::Acquire) == generation {
                        crate::poll_safepoint();
                        std::thread::yield_now();
                    }
                    assert!(!descriptor.result.is_done(), "spurious wake completed JOIN");
                    CHILD_GATE.get().unwrap().signal(1).unwrap();
                }
                assert_eq!(join_fiber(parent).unwrap(), T);
                RACE_POOL.get().unwrap().shutdown_and_join().unwrap();
            });
        }

        #[test]
        fn completion_before_unmount_does_not_lose_join_wake() {
            completion_race(true);
        }

        #[test]
        fn spurious_unpark_rechecks_completion_and_replaces_waiter() {
            completion_race(false);
        }
    }

    #[test]
    fn consumed_native_result_remains_completed() {
        concurrent_join(false);
    }

    #[cfg(egcl_fibers)]
    #[test]
    fn consumed_fiber_result_remains_completed() {
        concurrent_join(true);
    }
}

/// An exposed OS-backed platform thread. Scheduler-owned carrier threads use
/// this same type and are distinguished only by `is_carrier()`.
pub struct NativeThread {
    id: NativeThreadId,
    name: Option<String>,
    carrier: bool,
    auto_registered: bool,
    entry: AtomicU64,
    // Read during TLS retirement, when the debug lock-stack TLS may already
    // be destroyed. This scalar state needs atomic publication, not a mutex.
    state: AtomicU8,
    stack: EgclStack,
    tls: OrderedMutex<Vec<EgclVal>>,
    lisp_frames: Mutex<RecordedCalls>,
    condition_state: OrderedMutex<ThreadConditionState>,
    result: Arc<ThreadResult>,
    join_handle: OrderedMutex<Option<std::thread::JoinHandle<()>>>,
    interrupt_pending: AtomicBool,
    interrupt_value: OrderedMutex<EgclVal>,
    /// Lisp execution context (§2.3.1, R4.72) — the state generated code will
    /// reach through the reserved register. It OWNS the yield flag; there is
    /// deliberately no second copy on this struct, because generated code and
    /// the runtime must not be able to disagree about which is current.
    execution: crate::exec_context::LispExecutionContext,
    gc_participates: AtomicBool,
    published_sp: AtomicUsize,
    published_fp: AtomicUsize,
    /// `pthread_t` for directed SIGUSR1 delivery on Unix (zero until mounted).
    #[cfg(unix)]
    os_thread_id: AtomicUsize,
    pending_signals: AtomicU8,
}

unsafe impl Send for NativeThread {}
unsafe impl Sync for NativeThread {}

impl NativeThread {
    fn new(
        id: NativeThreadId,
        name: Option<String>,
        carrier: bool,
        auto_registered: bool,
        entry: EgclVal,
        result: Arc<ThreadResult>,
    ) -> Self {
        Self {
            id,
            name,
            carrier,
            auto_registered,
            entry: AtomicU64::new(entry.0),
            state: AtomicU8::new(NativeThreadState::Born as u8),
            stack: EgclStack::new(default_stack_size()),
            lisp_frames: Mutex::new(RecordedCalls::default()),
            tls: OrderedMutex::new(
                LockLevel::ExecutionObject,
                native_object_order(id, 2),
                "native thread TLS",
                vec![NIL; MAX_TLS],
            ),
            condition_state: OrderedMutex::new(
                LockLevel::ExecutionObject,
                native_object_order(id, 9),
                "native thread condition state",
                ThreadConditionState::new(),
            ),
            result,
            join_handle: OrderedMutex::new(
                LockLevel::ExecutionObject,
                native_object_order(id, 3),
                "native thread join handle",
                None,
            ),
            interrupt_pending: AtomicBool::new(false),
            interrupt_value: OrderedMutex::new(
                LockLevel::ExecutionObject,
                native_object_order(id, 4),
                "native thread interrupt",
                NIL,
            ),
            // A bare native thread is its own carrier.
            execution: crate::exec_context::LispExecutionContext::new(
                crate::exec_context::ExecutionKind::NativeThread,
                id.0,
                id.0,
            ),
            gc_participates: AtomicBool::new(false),
            published_sp: AtomicUsize::new(0),
            published_fp: AtomicUsize::new(0),
            #[cfg(unix)]
            os_thread_id: AtomicUsize::new(0),
            pending_signals: AtomicU8::new(0),
        }
    }

    pub fn id(&self) -> NativeThreadId {
        self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn is_carrier(&self) -> bool {
        self.carrier
    }

    pub fn is_alive(&self) -> bool {
        !self.result.is_done()
            && !matches!(
                self.state(),
                NativeThreadState::Dead | NativeThreadState::Aborted
            )
    }

    fn is_inactive_auto_registered(&self) -> bool {
        self.auto_registered && !self.gc_participates()
    }

    pub fn state(&self) -> NativeThreadState {
        match self.state.load(Ordering::Acquire) {
            0 => NativeThreadState::Born,
            1 => NativeThreadState::Running,
            2 => NativeThreadState::Blocked,
            3 => NativeThreadState::Native,
            4 => NativeThreadState::Dead,
            5 => NativeThreadState::Aborted,
            _ => unreachable!("invalid native thread state"),
        }
    }

    pub(crate) fn set_state(&self, state: NativeThreadState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub fn stack(&self) -> &EgclStack {
        &self.stack
    }

    pub fn entry(&self) -> EgclVal {
        EgclVal(self.entry.load(Ordering::Acquire))
    }

    pub fn tls_get(&self, index: u32) -> EgclVal {
        self.tls
            .lock()
            .unwrap()
            .get(index as usize)
            .copied()
            .unwrap_or(NIL)
    }

    pub fn with_condition_state_mut<T>(&self, f: impl FnOnce(&mut ThreadConditionState) -> T) -> T {
        let mut state = self.condition_state.lock().unwrap();
        f(&mut state)
    }

    pub fn condition_state_snapshot(&self) -> ConditionStateSnapshot {
        let state = self.condition_state.lock().unwrap();
        ConditionStateSnapshot {
            handler_depth: state.handler_stack.len(),
            restart_depth: state.restart_stack.len(),
            debugger_hook: state.debugger_hook,
            break_on_signals: state.break_on_signals,
            handler_case_clause_count: state.handler_case_clauses.len(),
            pending_handler_case: state.pending_handler_case.is_some(),
        }
    }

    pub fn tls_set(&self, index: u32, value: EgclVal) {
        if let Some(slot) = self.tls.lock().unwrap().get_mut(index as usize) {
            *slot = value;
        }
    }

    pub fn check_and_clear_yield(&self) -> bool {
        self.execution.take_yield_request()
    }

    /// The Lisp execution context for this thread (§2.3.1). Its address is what
    /// generated code will eventually hold in the reserved register, so it is
    /// stable for the life of the thread object.
    pub fn execution(&self) -> &crate::exec_context::LispExecutionContext {
        &self.execution
    }

    pub(crate) fn set_gc_participates(&self, participates: bool) {
        self.gc_participates.store(participates, Ordering::Release);
    }

    pub(crate) fn gc_participates(&self) -> bool {
        self.gc_participates.load(Ordering::Acquire)
    }

    pub fn publish_stack(&self, sp: usize, fp: usize) {
        self.published_sp.store(sp, Ordering::Release);
        self.published_fp.store(fp, Ordering::Release);
    }

    pub fn published_stack(&self) -> (usize, usize) {
        (
            self.published_sp.load(Ordering::Acquire),
            self.published_fp.load(Ordering::Acquire),
        )
    }

    fn post_interrupt(&self, condition: EgclVal) {
        *self.interrupt_value.lock().unwrap() = condition;
        self.interrupt_pending.store(true, Ordering::Release);
    }

    fn take_interrupt(&self) -> Option<EgclVal> {
        self.interrupt_pending
            .swap(false, Ordering::AcqRel)
            .then(|| *self.interrupt_value.lock().unwrap())
    }

    fn post_pending_signal(&self, signal: PendingSignal) {
        let previous = self
            .pending_signals
            .fetch_or(signal.bit(), Ordering::Release);
        // Only a bit that was not already set adds an untaken signal, or the
        // count would drift above the number `take_pending_signal` can remove.
        if previous & signal.bit() == 0 {
            POSTED_PENDING_SIGNALS.fetch_add(1, Ordering::Release);
        }
    }

    fn take_pending_signal(&self) -> Option<PendingSignal> {
        loop {
            let bits = self.pending_signals.load(Ordering::Acquire);
            let signal = PendingSignal::from_bits(bits)?;
            let new_bits = bits & !signal.bit();
            if self
                .pending_signals
                .compare_exchange(bits, new_bits, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                POSTED_PENDING_SIGNALS.fetch_sub(1, Ordering::Release);
                return Some(signal);
            }
        }
    }

    fn trace_execution_roots(&self, visit: &mut dyn FnMut(*mut EgclVal)) {
        trace_atomic_egcl_val(&self.entry, visit);
        self.lisp_frames.lock().unwrap().trace_host_roots(visit);
        self.tls.lock().unwrap().trace_host_roots(visit);
        self.condition_state.lock().unwrap().trace_host_roots(visit);
        self.interrupt_value.lock().unwrap().trace_host_roots(visit);
        self.result
            .completion
            .lock()
            .unwrap()
            .value
            .trace_host_roots(visit);
    }
}

fn trace_atomic_egcl_val(slot: &AtomicU64, visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut value = EgclVal(slot.load(Ordering::Acquire));
    value.trace_host_roots(visit);
    slot.store(value.0, Ordering::Release);
}

fn install_execution_root_scanner() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| register_root_scanner(scan_execution_roots));
}

fn scan_execution_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let threads = native_threads_snapshot();
    for thread in threads {
        thread.trace_execution_roots(visit);
    }

    let fibers: Vec<Arc<Fiber>> = fiber_registry().lock().unwrap().values().cloned().collect();
    for fiber in fibers {
        fiber.trace_execution_roots(visit);
    }
}

fn native_thread_registry() -> &'static OrderedMutex<HashMap<NativeThreadId, Arc<NativeThread>>> {
    static REGISTRY: OnceLock<OrderedMutex<HashMap<NativeThreadId, Arc<NativeThread>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::ExecutionRegistry,
            1,
            "native thread registry",
            HashMap::new(),
        )
    })
}

fn prune_inactive_auto_registered_threads(
    registry: &mut HashMap<NativeThreadId, Arc<NativeThread>>,
) {
    registry.retain(|_, thread| !thread.is_inactive_auto_registered());
}

fn native_threads_snapshot() -> Vec<Arc<NativeThread>> {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry.values().cloned().collect()
}

thread_local! {
    static CURRENT_NATIVE_THREAD: RefCell<Option<CurrentNativeThread>> =
        const { RefCell::new(None) };
    static SANDBOX_CPU_DEADLINE_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count of pending signals posted to a thread or fiber and not yet taken.
///
/// `take_current_pending_signal` has to consult per-thread/per-fiber state, and
/// reaching either costs a thread-local lookup, a `RefCell` borrow and an `Arc`
/// clone/drop (two more atomic RMWs) — ~5% of every native call on top of the
/// flag checks (bliss-htff). This count lets the overwhelmingly common "nobody
/// has a pending signal" case skip all of it with one relaxed load.
///
/// It counts posts across ALL threads and fibers, so observing zero is safe for
/// every thread to act on: a zero means no execution anywhere holds an untaken
/// signal. That is what makes a process-global word correct here, where a
/// global *boolean* would not be — one thread clearing it could hide another
/// thread's pending signal.
static POSTED_PENDING_SIGNALS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// True if any thread or fiber holds a posted, untaken signal.
pub fn any_posted_pending_signal() -> bool {
    POSTED_PENDING_SIGNALS.load(Ordering::Relaxed) != 0
}

/// True if this thread has an armed sandbox CPU deadline that must keep being
/// polled. Arming raises the process signal summary; the drain re-raises it
/// while this holds, so polling continues.
pub fn sandbox_cpu_deadline_armed() -> bool {
    SANDBOX_CPU_DEADLINE_NS.with(|deadline| deadline.get()) != 0
}

struct CurrentNativeThread {
    thread: Arc<NativeThread>,
}

impl CurrentNativeThread {
    fn new(thread: Arc<NativeThread>) -> Self {
        thread.set_gc_participates(true);
        Self { thread }
    }
}

impl Drop for CurrentNativeThread {
    fn drop(&mut self) {
        crate::execution_local::retire_native(std::thread::current().id());
        crate::safepoint::retire_native_thread(&self.thread);
    }
}

fn install_current_native_thread(thread: Arc<NativeThread>) {
    install_execution_root_scanner();
    #[cfg(unix)]
    thread
        .os_thread_id
        .store(crate::syscall::gettid() as usize, Ordering::Release);
    CURRENT_NATIVE_THREAD.with(|slot| {
        *slot.borrow_mut() = Some(CurrentNativeThread::new(thread));
    });
    #[cfg(test)]
    registration_tests::installed();
}

fn ensure_current_native_thread() -> Arc<NativeThread> {
    ensure_current_native_thread_in_state(NativeThreadState::Running)
}

fn current_native_thread_if_registered() -> Option<Arc<NativeThread>> {
    CURRENT_NATIVE_THREAD.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|current| Arc::clone(&current.thread))
    })
}

fn ensure_current_native_thread_in_state(initial_state: NativeThreadState) -> Arc<NativeThread> {
    install_execution_root_scanner();
    if let Some(thread) = current_native_thread_if_registered() {
        return thread;
    }
    let id = NativeThreadId(NEXT_NATIVE_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let result = Arc::new(ThreadResult::new());
    let thread = Arc::new(NativeThread::new(
        id,
        std::thread::current().name().map(str::to_owned),
        false,
        true,
        NIL,
        result,
    ));
    // Publish a fully initialized, quiescent participant. A collector may have
    // already taken its Running-thread snapshot, and registry enumeration must
    // not mistake this entrant for an inactive auto registration and prune it.
    thread.set_state(NativeThreadState::Native);
    let current = CurrentNativeThread::new(Arc::clone(&thread));
    #[cfg(unix)]
    thread
        .os_thread_id
        .store(crate::syscall::gettid() as usize, Ordering::Release);
    native_thread_registry()
        .lock()
        .unwrap()
        .insert(id, Arc::clone(&thread));
    #[cfg(test)]
    registration_tests::published(&thread);
    CURRENT_NATIVE_THREAD.with(|slot| *slot.borrow_mut() = Some(current));
    if initial_state == NativeThreadState::Running {
        // Wait out an active pause without retaining a registry lock or TLS
        // borrow. Foreign callbacks instead remain Native until Lisp entry.
        crate::safepoint::transition_native_state(&thread, initial_state);
    }
    thread
}

#[cfg(test)]
pub(crate) mod registration_tests {
    use super::*;

    type PublicationHook = Box<dyn FnOnce(&NativeThread)>;
    static STARTUP_WAIT: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
        std::sync::Mutex::new(None);
    thread_local! {
        static PUBLICATION_HOOK: RefCell<Option<PublicationHook>> = const { RefCell::new(None) };
        static ADMISSION_WAIT: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
    }

    pub(super) fn installed() {
        let sender = STARTUP_WAIT.lock().unwrap().take();
        if let Some(sender) = sender {
            ADMISSION_WAIT.with(|slot| *slot.borrow_mut() = Some(sender));
        }
    }

    pub(crate) fn waiting_for_admission() {
        ADMISSION_WAIT.with(|slot| {
            if let Some(sender) = slot.borrow_mut().take() {
                sender.send(()).unwrap();
            }
        });
    }

    pub(super) fn published(thread: &NativeThread) {
        let hook = PUBLICATION_HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook(thread);
        }
    }

    #[test]
    fn registration_during_gc_publishes_a_quiescent_participant() {
        const CHILD: &str = "EGCL_TEST_REGISTRATION_DURING_GC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "thread::registration_tests::registration_during_gc_publishes_a_quiescent_participant",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        current_thread_id();
        crate::safepoint::wait_for_all_threads().unwrap();
        let (published, observed) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let (waiting, admission) = std::sync::mpsc::channel();
        let newcomer = std::thread::spawn(move || {
            ADMISSION_WAIT.with(|slot| *slot.borrow_mut() = Some(waiting));
            PUBLICATION_HOOK.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |thread| {
                    published
                        .send((thread.id, thread.state(), thread.gc_participates()))
                        .unwrap();
                    proceed.recv().unwrap();
                }));
            });
            current_thread_id()
        });
        let observation = observed.recv_timeout(std::time::Duration::from_secs(10));
        // Enumeration prunes inactive auto registrations. A published entrant
        // must already participate even while it cannot yet run Lisp.
        let ids = all_thread_ids();
        release.send(()).unwrap();
        // Prove that registration reaches the active-pause wait. Removing the
        // admission transition must fail even if publication remains Native.
        let waited = admission.recv_timeout(std::time::Duration::from_secs(10));
        crate::safepoint::resume_all_threads().unwrap();
        let joined = newcomer.join().unwrap();
        let (id, state, participates) = observation.unwrap();
        assert_eq!(joined, id);
        assert_eq!(state, NativeThreadState::Native);
        assert!(participates);
        assert!(waited.is_ok(), "entrant did not wait for GC admission");
        assert!(
            ids.contains(&id),
            "entrant was pruned before completing registration"
        );
    }

    fn startup_waits_for_gc(carrier: bool) {
        const CHILD: &str = "EGCL_TEST_STARTUP_DURING_GC_CHILD";
        let name = if carrier {
            "carrier_startup_waits_for_active_gc"
        } else {
            "dedicated_startup_waits_for_active_gc"
        };
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("thread::registration_tests::{name}"),
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        current_thread_id();
        crate::safepoint::wait_for_all_threads().unwrap();
        let (waiting, admission) = std::sync::mpsc::channel();
        *STARTUP_WAIT.lock().unwrap() = Some(waiting);
        let group = carrier.then(|| {
            crate::SchedulerGroup::init(&crate::SchedulerConfig { num_workers: 1 }).unwrap()
        });
        let dedicated = (!carrier).then(|| make_thread(NIL).unwrap());
        // No new mutator was included in the collector's snapshot. Its first
        // transition to Running must wait rather than contribute an arrival.
        let waited = admission.recv_timeout(std::time::Duration::from_secs(10));
        crate::safepoint::resume_all_threads().unwrap();
        if let Some(group) = group {
            group.finish().unwrap();
        }
        if let Some(id) = dedicated {
            assert_eq!(join_thread(id).unwrap(), NIL);
        }
        assert!(waited.is_ok(), "{name} bypassed GC admission");
    }

    #[test]
    fn carrier_startup_waits_for_active_gc() {
        startup_waits_for_gc(true);
    }

    #[test]
    fn dedicated_startup_waits_for_active_gc() {
        startup_waits_for_gc(false);
    }
}

/// Global thread registry mapping IDs to thread descriptors.
fn fiber_registry() -> &'static OrderedMutex<HashMap<FiberId, Arc<Fiber>>> {
    static REGISTRY: OnceLock<OrderedMutex<HashMap<FiberId, Arc<Fiber>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::ExecutionRegistry,
            2,
            "fiber registry",
            HashMap::new(),
        )
    })
}

// The fiber mounted on this carrier, if any.
thread_local! {
    static ACTIVE_FIBER: RefCell<Option<Arc<Fiber>>> = const { RefCell::new(None) };
}

/// Green thread descriptor. D2.01.
pub struct Fiber {
    id: FiberId,
    name: OrderedMutex<Option<String>>,
    /// The CL function (entry point) this fiber was created to execute.
    entry: AtomicU64,
    state: OrderedMutex<FiberState>,
    lisp_frames: Mutex<RecordedCalls>,
    // Unregister roots before either stack is destroyed (field drop order).
    host_roots: crate::gc::FiberRoots,
    native_faults: crate::runtime::FiberFaultState,
    stack: EgclStack,
    continuation: FiberContinuation,
    // Ports without a native context switch neither mount nor resume.
    #[cfg_attr(not(egcl_fibers), allow(dead_code))]
    execution_context: FiberExecutionContext,
    native_stack_size: usize,
    /// Native return context: a pointer to the carrier's saved-SP cell on Unix,
    /// or its Windows fiber handle. Set on each mount and read on suspension.
    /// Stored on the fiber
    /// — NOT in a thread-local — because a fiber can be preempted on one carrier
    /// and resumed on another, and a compiler-cached thread-local address would
    /// then be stale (reads the wrong/cleared carrier slot). See bliss-bca.5.
    #[cfg_attr(not(egcl_fibers), allow(dead_code))]
    scheduler_return: AtomicUsize,
    /// Carrier pool chosen by the first scheduler-group submission.  Wakeups
    /// from timers, synchronization primitives, and I/O always return to this
    /// pool; a fiber cannot migrate between scheduler groups.
    scheduler_pool: OrderedMutex<Option<Weak<WorkerPool>>>,
    /// True from the carrier's mount handshake until it has consumed the
    /// reason for the fiber's return to the scheduler.  A wake arriving while
    /// this is true is recorded, not independently enqueued.
    mounted: AtomicBool,
    /// An unpark that raced with the carrier-side unmount handshake.
    wake_pending: AtomicBool,
    /// Why the continuation most recently returned to its carrier.
    suspend_reason: AtomicU8,
    /// Monotonic token distinguishing successive blocking operations.  Stale
    /// timeout/I/O completions cannot wake a later wait by the same fiber.
    wait_generation: AtomicU64,
    tls: OrderedMutex<Vec<EgclVal>>,
    dynamic_bindings: OrderedMutex<Vec<(EgclVal, EgclVal)>>,
    handler_stack: OrderedMutex<Vec<EgclVal>>,
    restart_stack: OrderedMutex<Vec<EgclVal>>,
    condition_state: OrderedMutex<ThreadConditionState>,
    pin_count: AtomicUsize,
    /// Lisp execution context (§2.3.1, R4.72). It OWNS both the carrier
    /// association — refreshed on each mount, since a fiber can resume on a
    /// different carrier — and the cooperative-preemption yield flag (§2.5.3
    /// step 4). Neither is duplicated on this struct: a cached register value
    /// and a struct field disagreeing is the failure the register design exists
    /// to prevent.
    execution: crate::exec_context::LispExecutionContext,
    /// Shared result cell — written by the executing thread, read by joiners.
    result: Arc<ThreadResult>,
    /// Flag indicating an interrupt has been requested.
    interrupt_pending: AtomicBool,
    /// The condition value to deliver on interrupt.
    interrupt_value: OrderedMutex<EgclVal>,
    /// Stack pointer / frame pointer this managed fiber published at its last
    /// safepoint before parking or entering Native (bliss-jtc.14.2). While a
    /// fiber is suspended it does not run its own handshake, so it publishes its
    /// stack roots here for the collector to scan on its behalf (spec §2.5).
    /// `0` means "running normally, not published".
    published_sp: AtomicUsize,
    published_fp: AtomicUsize,
    pending_signals: AtomicU8,
}

impl Fiber {
    pub fn name(&self) -> Option<String> {
        self.name.lock().unwrap().clone()
    }

    pub fn continuation(&self) -> &FiberContinuation {
        &self.continuation
    }

    pub fn native_stack_size(&self) -> usize {
        self.native_stack_size
    }
    pub fn native_stack_bounds(&self) -> Option<(usize, usize)> {
        #[cfg(egcl_unix_fibers)]
        {
            let low = self.execution_context._native_stack.base() as usize;
            Some((low, low + self.native_stack_size))
        }
        #[cfg(not(egcl_unix_fibers))]
        {
            None
        }
    }

    pub fn dynamic_bindings(&self) -> Vec<(EgclVal, EgclVal)> {
        self.dynamic_bindings.lock().unwrap().clone()
    }

    pub fn handler_stack(&self) -> Vec<EgclVal> {
        self.handler_stack.lock().unwrap().clone()
    }

    pub fn restart_stack(&self) -> Vec<EgclVal> {
        self.restart_stack.lock().unwrap().clone()
    }

    pub fn with_condition_state_mut<T>(&self, f: impl FnOnce(&mut ThreadConditionState) -> T) -> T {
        let mut state = self.condition_state.lock().unwrap();
        f(&mut state)
    }

    pub fn condition_state_snapshot(&self) -> ConditionStateSnapshot {
        let state = self.condition_state.lock().unwrap();
        ConditionStateSnapshot {
            handler_depth: state.handler_stack.len(),
            restart_depth: state.restart_stack.len(),
            debugger_hook: state.debugger_hook,
            break_on_signals: state.break_on_signals,
            handler_case_clause_count: state.handler_case_clauses.len(),
            pending_handler_case: state.pending_handler_case.is_some(),
        }
    }

    pub fn pin(&self) {
        self.pin_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn unpin(&self) -> Result<(), EgclError> {
        self.pin_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .map(|_| ())
            .map_err(|_| EgclError::ProgramError("fiber pin count underflow".into()))
    }

    pub fn can_yield(&self) -> bool {
        self.pin_count.load(Ordering::Acquire) == 0
    }

    pub fn carrier_id(&self) -> Option<NativeThreadId> {
        match self.execution.carrier_id() {
            0 => None,
            id => Some(NativeThreadId(id)),
        }
    }

    /// The Lisp execution context for this fiber (§2.3.1). Its address is stable
    /// across migration — only the carrier field inside it changes — which is
    /// what lets a pointer cached in the reserved register survive a fiber
    /// switch.
    pub fn execution(&self) -> &crate::exec_context::LispExecutionContext {
        &self.execution
    }

    /// Publish this fiber's stack roots (SP/FP) at a safepoint, before it parks,
    /// blocks, or enters Native, so the collector can scan them while it is
    /// suspended (bliss-jtc.14.2).
    pub fn publish_stack(&self, sp: usize, fp: usize) {
        self.published_sp.store(sp, Ordering::Release);
        self.published_fp.store(fp, Ordering::Release);
    }

    /// The stack roots this fiber last published, or `(0, 0)` if it is running
    /// normally and scanning its own stack.
    pub fn published_stack(&self) -> (usize, usize) {
        (
            self.published_sp.load(Ordering::Acquire),
            self.published_fp.load(Ordering::Acquire),
        )
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        crate::execution_local::retire_fiber(self.id);
    }
}

/// Prevent suspension while a carrier-bound resource is borrowed. This does
/// not suppress safepoint handshakes, and composes with existing foreign pins.
pub(crate) struct FiberPin {
    fiber: Option<&'static Fiber>,
    _carrier_bound: std::marker::PhantomData<*mut ()>,
}

impl FiberPin {
    pub(crate) fn current() -> Self {
        let fiber = current_fiber();
        if let Some(fiber) = fiber {
            fiber.pin();
        }
        Self {
            fiber,
            _carrier_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for FiberPin {
    fn drop(&mut self) {
        if let Some(fiber) = self.fiber {
            // This scope owns exactly one pin, including on error exits.
            let _ = fiber.unpin();
        }
    }
}

/// Number of live (non-`Dead`) managed fibers in the registry — the M in the
/// N-native-workers × M-managed-fibers model (bliss-jtc.14.2).
pub fn live_fiber_count() -> usize {
    fiber_registry()
        .lock()
        .unwrap()
        .values()
        .filter(|t| t.state() != FiberState::Dead)
        .count()
}

/// Request that fiber `id` yield at its next safepoint. Returns `false` if no
/// such fiber exists (bliss-jtc.14.2).
pub fn request_fiber_yield(id: FiberId) -> bool {
    let reg = fiber_registry().lock().unwrap();
    match reg.get(&id) {
        Some(t) => {
            // Was a Release store; the context publishes SeqCst, matching the
            // swap on the consuming side rather than being weaker than it.
            t.execution.request_yield();
            true
        }
        None => false,
    }
}

// Safety: Fiber access is controlled by the scheduler and thread registry.
unsafe impl Send for Fiber {}
unsafe impl Sync for Fiber {}

impl Fiber {
    /// Get this thread's unique ID.
    pub fn id(&self) -> FiberId {
        self.id
    }

    /// Get the entry value this thread was created to execute.
    pub fn entry(&self) -> EgclVal {
        EgclVal(self.entry.load(Ordering::Acquire))
    }

    /// Get the current state of this thread.
    pub fn state(&self) -> FiberState {
        *self.state.lock().unwrap()
    }

    /// Set the thread state.
    pub(crate) fn set_state(&self, new_state: FiberState) {
        *self.state.lock().unwrap() = new_state;
    }

    /// Get a reference to this thread's CL stack.
    pub fn stack(&self) -> &EgclStack {
        &self.stack
    }

    /// Get this thread's TLS slot at the given index.
    pub fn tls_get(&self, index: u32) -> EgclVal {
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
        self.execution.take_yield_request()
    }

    /// Request this thread to yield at its next safepoint.
    pub fn request_yield(&self) {
        self.execution.request_yield();
    }

    /// Set this thread's TLS slot at the given index.
    pub fn tls_set(&self, index: u32, value: EgclVal) {
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
    pub fn take_interrupt(&self) -> Option<EgclVal> {
        if self.interrupt_pending.swap(false, Ordering::AcqRel) {
            let val = *self.interrupt_value.lock().unwrap();
            Some(val)
        } else {
            None
        }
    }

    /// Deliver an interrupt condition to this thread.
    fn post_interrupt(&self, condition: EgclVal) {
        *self.interrupt_value.lock().unwrap() = condition;
        self.interrupt_pending.store(true, Ordering::Release);
    }

    fn post_pending_signal(&self, signal: PendingSignal) {
        let previous = self
            .pending_signals
            .fetch_or(signal.bit(), Ordering::Release);
        // Only a bit that was not already set adds an untaken signal, or the
        // count would drift above the number `take_pending_signal` can remove.
        if previous & signal.bit() == 0 {
            POSTED_PENDING_SIGNALS.fetch_add(1, Ordering::Release);
        }
    }

    fn take_pending_signal(&self) -> Option<PendingSignal> {
        loop {
            let bits = self.pending_signals.load(Ordering::Acquire);
            let signal = PendingSignal::from_bits(bits)?;
            let new_bits = bits & !signal.bit();
            if self
                .pending_signals
                .compare_exchange(bits, new_bits, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                POSTED_PENDING_SIGNALS.fetch_sub(1, Ordering::Release);
                return Some(signal);
            }
        }
    }

    fn trace_execution_roots(&self, visit: &mut dyn FnMut(*mut EgclVal)) {
        trace_atomic_egcl_val(&self.entry, visit);
        self.lisp_frames.lock().unwrap().trace_host_roots(visit);
        self.tls.lock().unwrap().trace_host_roots(visit);
        self.dynamic_bindings
            .lock()
            .unwrap()
            .trace_host_roots(visit);
        self.handler_stack.lock().unwrap().trace_host_roots(visit);
        self.restart_stack.lock().unwrap().trace_host_roots(visit);
        self.condition_state.lock().unwrap().trace_host_roots(visit);
        self.interrupt_value.lock().unwrap().trace_host_roots(visit);
        self.result
            .completion
            .lock()
            .unwrap()
            .value
            .trace_host_roots(visit);
    }
}

// ── Worker pool for M:N fibering ──────────────────────────────

/// A task submitted to the carrier pool: a fiber to execute.
struct WorkerTask {
    thread: Arc<Fiber>,
    result_cell: Arc<ThreadResult>,
}

const DEQUE_CAPACITY: usize = 1 << 16;

/// Bounded Chase–Lev work-stealing deque. Exactly one carrier owns bottom
/// push/pop; any carrier may CAS the top to steal the oldest task. Slots contain
/// owned task pointers and are reclaimed only by the operation that wins the
/// bottom-vs-top race.
struct ChaseLevDeque {
    top: AtomicIsize,
    bottom: AtomicIsize,
    slots: Box<[AtomicPtr<WorkerTask>]>,
}

impl ChaseLevDeque {
    fn new() -> Self {
        let slots = (0..DEQUE_CAPACITY)
            .map(|_| AtomicPtr::new(ptr::null_mut()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            top: AtomicIsize::new(0),
            bottom: AtomicIsize::new(0),
            slots,
        }
    }

    #[inline]
    fn slot(&self, index: isize) -> &AtomicPtr<WorkerTask> {
        &self.slots[(index as usize) & (DEQUE_CAPACITY - 1)]
    }

    /// Owner-only bottom push.
    fn push(&self, task: WorkerTask) -> Result<(), WorkerTask> {
        let bottom = self.bottom.load(Ordering::Relaxed);
        let top = self.top.load(Ordering::Acquire);
        if bottom - top >= DEQUE_CAPACITY as isize {
            return Err(task);
        }
        let raw = Box::into_raw(Box::new(task));
        let previous = self.slot(bottom).swap(raw, Ordering::Relaxed);
        debug_assert!(previous.is_null(), "Chase-Lev slot reused while occupied");
        fence(Ordering::Release);
        self.bottom.store(bottom + 1, Ordering::Release);
        Ok(())
    }

    /// Owner-only bottom pop (LIFO).
    fn pop(&self) -> Option<WorkerTask> {
        let bottom = self.bottom.load(Ordering::Relaxed) - 1;
        self.bottom.store(bottom, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        let top = self.top.load(Ordering::Relaxed);
        if top > bottom {
            self.bottom.store(top, Ordering::Relaxed);
            return None;
        }

        if top == bottom {
            if self
                .top
                .compare_exchange(top, top + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_err()
            {
                self.bottom.store(top + 1, Ordering::Relaxed);
                return None;
            }
            self.bottom.store(top + 1, Ordering::Relaxed);
        }

        let raw = self.slot(bottom).swap(ptr::null_mut(), Ordering::AcqRel);
        debug_assert!(!raw.is_null(), "claimed Chase-Lev slot was empty");
        (!raw.is_null()).then(|| unsafe { *Box::from_raw(raw) })
    }

    /// Multi-thief top steal (FIFO).
    fn steal(&self) -> Option<WorkerTask> {
        let top = self.top.load(Ordering::Acquire);
        fence(Ordering::SeqCst);
        let bottom = self.bottom.load(Ordering::Acquire);
        if top >= bottom {
            return None;
        }
        let raw = self.slot(top).load(Ordering::Acquire);
        if raw.is_null()
            || self
                .top
                .compare_exchange(top, top + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_err()
        {
            return None;
        }
        let claimed = self.slot(top).swap(ptr::null_mut(), Ordering::AcqRel);
        debug_assert_eq!(
            claimed, raw,
            "Chase-Lev slot changed after successful steal"
        );
        (!claimed.is_null()).then(|| unsafe { *Box::from_raw(claimed) })
    }

    fn is_empty(&self) -> bool {
        self.top.load(Ordering::Acquire) >= self.bottom.load(Ordering::Acquire)
    }
}

impl Drop for ChaseLevDeque {
    fn drop(&mut self) {
        for slot in &self.slots {
            let raw = slot.swap(ptr::null_mut(), Ordering::AcqRel);
            if !raw.is_null() {
                unsafe { drop(Box::from_raw(raw)) };
            }
        }
    }
}

/// One carrier's owner deque plus an MPSC staging queue for submissions made by
/// threads that do not own this Chase–Lev bottom.
struct Worker {
    local: ChaseLevDeque,
    pending: OrderedMutex<VecDeque<WorkerTask>>,
    deferred: OrderedMutex<VecDeque<WorkerTask>>,
    /// Fiber mounted on this carrier, or zero while the carrier is idle.
    current_fiber: AtomicU64,
}

thread_local! {
    /// `(pool-id, carrier-index)` for owner-only bottom operations.
    static WORKER_CONTEXT: std::cell::Cell<Option<(u64, usize)>> = const { std::cell::Cell::new(None) };
}

static NEXT_POOL_ID: AtomicU64 = AtomicU64::new(1);

/// The global carrier pool that multiplexes fibers onto OS carrier threads
/// via per-worker work-stealing deques.
struct WorkerPool {
    id: u64,
    workers: Vec<Worker>,
    idle_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Parking coordination for idle workers (separate from the run queues).
    park_mutex: Mutex<()>,
    park_cv: Condvar,
    /// Flag to signal shutdown to workers.
    shutdown: AtomicBool,
    /// Whether the pool's OS threads have been spawned.
    initialized: AtomicBool,
    /// Round-robin cursor for submissions from non-worker (external) threads.
    next: AtomicUsize,
    /// Public identities of the OS threads carrying this pool's fibers.
    carrier_ids: OrderedMutex<Vec<NativeThreadId>>,
}

impl WorkerPool {
    fn new(num_workers: usize) -> Self {
        let num_workers = num_workers.max(1);
        let id = NEXT_POOL_ID.fetch_add(1, Ordering::Relaxed);
        let workers = (0..num_workers)
            .map(|index| Worker {
                local: ChaseLevDeque::new(),
                pending: OrderedMutex::new(
                    LockLevel::ExecutionRegistry,
                    (id << 32) | ((index as u64) << 2) | 1,
                    "carrier pending queue",
                    VecDeque::new(),
                ),
                deferred: OrderedMutex::new(
                    LockLevel::ExecutionRegistry,
                    (id << 32) | ((index as u64) << 2) | 2,
                    "carrier deferred queue",
                    VecDeque::new(),
                ),
                current_fiber: AtomicU64::new(0),
            })
            .collect();
        WorkerPool {
            id,
            workers,
            idle_hook: Mutex::new(None),
            park_mutex: Mutex::new(()),
            park_cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            carrier_ids: OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                (id << 32) | u32::MAX as u64,
                "carrier identity list",
                Vec::new(),
            ),
        }
    }

    /// Ensure the carrier pool OS threads are running (one per deque).
    fn ensure_initialized(self: &Arc<Self>) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }
        if self
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            for i in 0..self.workers.len() {
                let id = NativeThreadId(NEXT_NATIVE_THREAD_ID.fetch_add(1, Ordering::Relaxed));
                let name = format!("egcl-carrier-{i}");
                let result = Arc::new(ThreadResult::new());
                let carrier = Arc::new(NativeThread::new(
                    id,
                    Some(name.clone()),
                    true,
                    false,
                    NIL,
                    Arc::clone(&result),
                ));
                native_thread_registry()
                    .lock()
                    .unwrap()
                    .insert(id, Arc::clone(&carrier));
                self.carrier_ids.lock().unwrap().push(id);
                let running_carrier = Arc::clone(&carrier);
                let running_pool = Arc::clone(self);
                let handle = std::thread::Builder::new()
                    .name(name)
                    .spawn(move || {
                        install_current_native_thread(Arc::clone(&running_carrier));
                        crate::safepoint::transition_native_state(
                            &running_carrier,
                            NativeThreadState::Running,
                        );
                        worker_loop(running_pool, i);
                        result.complete(Ok(NIL));
                        crate::safepoint::retire_native_thread(&running_carrier);
                    })
                    .expect("failed to spawn carrier thread");
                *carrier.join_handle.lock().unwrap() = Some(handle);
            }

            // HotSpot-style cooperative preemption: a service timer marks the
            // fibers currently mounted on this pool's carriers. Generated and
            // interpreted safepoint polls perform the context switch; the timer
            // thread never suspends a carrier asynchronously.
            let weak = Arc::downgrade(self);
            let quantum = std::env::var("EGCL_TIME_SLICE_US")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1_000);
            let _ = std::thread::Builder::new()
                .name(format!("egcl-preemption-{}", self.id))
                .spawn(move || {
                    let interval = std::time::Duration::from_micros(quantum);
                    loop {
                        std::thread::sleep(interval);
                        let Some(pool) = weak.upgrade() else {
                            return;
                        };
                        if pool.shutdown.load(Ordering::Acquire) {
                            return;
                        }
                        for worker in &pool.workers {
                            let id = worker.current_fiber.load(Ordering::Acquire);
                            if id != 0 {
                                request_fiber_yield(FiberId(id));
                            }
                        }
                    }
                });
        }
    }

    fn carrier_ids(self: &Arc<Self>) -> Vec<NativeThreadId> {
        self.ensure_initialized();
        self.carrier_ids.lock().unwrap().clone()
    }

    /// Submit a fiber task. A worker submits to its own deque (locality);
    /// an external thread round-robins across workers. Wakes one parked worker.
    fn submit(self: &Arc<Self>, task: WorkerTask) {
        self.ensure_initialized();
        let local = WORKER_CONTEXT.with(|context| context.get());
        let idx = local
            .filter(|&(pool, index)| pool == self.id && index < self.workers.len())
            .map(|(_, index)| index)
            .unwrap_or_else(|| self.next.fetch_add(1, Ordering::Relaxed) % self.workers.len());
        if local == Some((self.id, idx)) {
            if let Err(task) = self.workers[idx].local.push(task) {
                self.workers[idx].pending.lock().unwrap().push_back(task);
            }
        } else {
            self.workers[idx].pending.lock().unwrap().push_back(task);
        }
        self.park_cv.notify_one();
    }

    fn drain_pending(&self, idx: usize) {
        let worker = &self.workers[idx];
        let mut pending = worker.pending.lock().unwrap();
        while let Some(task) = pending.pop_front() {
            if let Err(task) = worker.local.push(task) {
                pending.push_front(task);
                break;
            }
        }
    }

    /// Pop this worker's own task (LIFO), else steal one (FIFO) from another
    /// worker's deque. Returns `None` only when every deque is empty.
    fn pop_or_steal(&self, idx: usize) -> Option<WorkerTask> {
        self.drain_pending(idx);
        if let Some(t) = self.workers[idx].local.pop() {
            return Some(t);
        }
        if let Some(t) = self.workers[idx].deferred.lock().unwrap().pop_front() {
            return Some(t);
        }
        let n = self.workers.len();
        for k in 1..n {
            let victim = (idx + k) % n;
            if let Some(t) = self.workers[victim].local.steal() {
                return Some(t);
            }
            if let Some(t) = self.workers[victim].deferred.lock().unwrap().pop_front() {
                return Some(t);
            }
            // External submissions (including timer wakes) go to an inbox.
            // Its owner may be pinned in native I/O and unable to drain it.
            if let Some(t) = self.workers[victim].pending.lock().unwrap().pop_front() {
                return Some(t);
            }
        }
        None
    }

    fn any_work(&self) -> bool {
        self.workers.iter().any(|w| {
            !w.local.is_empty()
                || !w.pending.lock().unwrap().is_empty()
                || !w.deferred.lock().unwrap().is_empty()
        })
    }

    /// Signal all workers to exit at their next scheduling point.
    #[allow(dead_code)]
    fn shutdown_now(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.park_cv.notify_all();
    }

    fn shutdown_and_join(&self) -> Result<(), EgclError> {
        self.shutdown_now();
        let ids = self.carrier_ids.lock().unwrap().clone();
        let carriers: Vec<_> = {
            let registry = native_thread_registry().lock().unwrap();
            ids.iter()
                .filter_map(|id| registry.get(id).cloned())
                .collect()
        };
        for carrier in carriers {
            let handle = carrier.join_handle.lock().unwrap().take();
            if let Some(handle) = handle {
                let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
                handle.join().map_err(|_| {
                    EgclError::Internal(format!("carrier thread {} panicked", carrier.id.0))
                })?;
            }
        }
        let mut registry = native_thread_registry().lock().unwrap();
        for id in ids {
            registry.remove(&id);
        }
        Ok(())
    }
}

/// Access the VM-wide default carrier pool used by the low-level convenience
/// `submit_fiber` API. Explicit scheduler groups own independent pools.
fn worker_pool() -> &'static Arc<WorkerPool> {
    static POOL: OnceLock<Arc<WorkerPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let carriers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 64);
        Arc::new(WorkerPool::new(carriers))
    })
}

/// One scheduler group's private carrier/deque implementation. The public
/// scheduler module holds this handle but cannot observe or mutate run queues.
pub(crate) struct CarrierPool {
    pool: Arc<WorkerPool>,
}

impl CarrierPool {
    pub(crate) fn new(carrier_count: usize) -> Self {
        let pool = Arc::new(WorkerPool::new(carrier_count));
        pool.ensure_initialized();
        Self { pool }
    }

    pub(crate) fn set_idle_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.pool.idle_hook.lock().unwrap() = hook;
        self.pool.park_cv.notify_all();
    }

    pub(crate) fn carrier_thread_ids(&self) -> Vec<NativeThreadId> {
        self.pool.carrier_ids()
    }

    pub(crate) fn submit(&self, fiber: FiberId) -> Result<(), EgclError> {
        submit_fiber_to_pool(fiber, &self.pool)
    }

    pub(crate) fn shutdown_and_join(&self) -> Result<(), EgclError> {
        self.pool.shutdown_and_join()
    }
}

/// The main loop executed by each OS worker thread: run local work LIFO, steal
/// FIFO when idle, and park when every deque is empty (bliss-jtc.14.1).
fn worker_loop(pool: Arc<WorkerPool>, idx: usize) {
    WORKER_CONTEXT.with(|context| context.set(Some((pool.id, idx))));
    loop {
        crate::safepoint::poll_safepoint();
        if pool.shutdown.load(Ordering::Acquire) {
            return;
        }
        if let Some(task) = pool.pop_or_steal(idx) {
            run_worker_task(&pool, idx, task);
            continue;
        }
        let hook = pool.idle_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
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
        // Coordinate both entry and wakeup with an in-progress collection.
        // Merely storing Blocked/Running can miss the collector's snapshot or
        // let a waking carrier mutate a root list while GC scans it.
        let blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        let _ = pool
            .park_cv
            .wait_timeout(guard, std::time::Duration::from_millis(5));
        drop(blocked);
    }
}

/// Mount one fiber on the current carrier until it yields, parks, or dies.
fn run_worker_task(pool: &Arc<WorkerPool>, carrier_index: usize, task: WorkerTask) {
    {
        let mut state = task.thread.state.lock().unwrap();
        if *state != FiberState::Runnable || task.thread.mounted.swap(true, Ordering::AcqRel) {
            let old_state = *state;
            drop(state);
            task.result_cell.complete(Err(EgclError::Internal(format!(
                "attempted to mount fiber in invalid state {old_state:?}"
            ))));
            return;
        }
        *state = FiberState::Running;
    }
    task.thread.publish_stack(0, 0);
    // Refresh the carrier association before Lisp resumes on this OS thread
    // (§2.3.1). The context's ADDRESS does not change here — only this field —
    // which is why a cached context pointer stays valid across migration.
    task.thread
        .execution()
        .set_carrier_id(current_thread_id().0);
    let thread = Arc::clone(&task.thread);
    pool.workers[carrier_index]
        .current_fiber
        .store(thread.id().0, Ordering::Release);
    ACTIVE_FIBER.with(|slot| {
        *slot.borrow_mut() = Some(Arc::clone(&thread));
    });

    // This scope stays on the carrier stack. A suspended fiber's registered
    // root chain remains live independently of whichever carrier resumes it.
    let mounted_roots = unsafe { thread.host_roots.mount() };

    match thread.native_faults.mount() {
        Ok(mounted_faults) => {
            #[cfg(egcl_unix_fibers)]
            unsafe {
                // Save the scheduler (carrier) context and switch to the fiber. The
                // fiber resumes at its trampoline (first mount) or where it last
                // suspended; control returns here when it swaps back. The return context
                // is recorded on the FIBER (not a thread-local) so it survives the fiber
                // migrating to a different carrier between suspend and resume.
                let mut scheduler_context: crate::context::Context = crate::context::NULL;
                thread
                    .scheduler_return
                    .store(&mut scheduler_context as *mut _ as usize, Ordering::Release);
                let fiber_sp = *thread.execution_context.as_ptr();
                crate::context::swap(&mut scheduler_context, fiber_sp);
                thread.scheduler_return.store(0, Ordering::Release);
            }

            #[cfg(all(target_arch = "x86_64", windows))]
            if let Err(error) = unsafe { thread.execution_context.resume(&thread.scheduler_return) }
            {
                thread.suspend_reason.store(SUSPEND_DEAD, Ordering::Release);
                thread.result.complete(Err(error));
            }

            #[cfg(not(egcl_fibers))]
            {
                let result = run_fiber_entry(&thread);
                thread.stack.publish_top();
                thread.continuation.save(
                    thread.stack.published_sp(),
                    thread.stack.published_fp() as usize,
                    0,
                );
                thread.set_state(FiberState::Dead);
                thread.result.complete(result);
            }

            drop(mounted_faults);
        }
        Err(error) => {
            thread.suspend_reason.store(SUSPEND_DEAD, Ordering::Release);
            thread.result.complete(Err(error));
        }
    }

    drop(mounted_roots);
    ACTIVE_FIBER.with(|slot| {
        *slot.borrow_mut() = None;
    });
    pool.workers[carrier_index]
        .current_fiber
        .store(0, Ordering::Release);

    let reason = thread.suspend_reason.swap(SUSPEND_NONE, Ordering::AcqRel);
    let mut state = thread.state.lock().unwrap();
    thread.mounted.store(false, Ordering::Release);
    match reason {
        SUSPEND_YIELD => {
            *state = FiberState::Runnable;
            drop(state);
            pool.workers[carrier_index]
                .deferred
                .lock()
                .unwrap()
                .push_back(task);
            pool.park_cv.notify_one();
        }
        SUSPEND_PARK => {
            if thread.wake_pending.swap(false, Ordering::AcqRel) {
                *state = FiberState::Runnable;
                drop(state);
                pool.submit(task);
            }
        }
        SUSPEND_DEAD => {
            *state = FiberState::Dead;
        }
        _ => {
            let old_state = *state;
            *state = FiberState::Dead;
            drop(state);
            task.result_cell.complete(Err(EgclError::Internal(format!(
                "fiber returned to scheduler without a suspension reason (state {old_state:?})"
            ))));
        }
    }
}

#[cfg(egcl_fibers)]
extern "C" fn fiber_context_trampoline() {
    let Some(fiber) = current_fiber() else {
        crate::syscall::abort()
    };
    let result = run_fiber_entry(fiber);
    fiber.stack.publish_top();
    fiber.continuation.save(
        fiber.stack.published_sp(),
        fiber.stack.published_fp() as usize,
        0,
    );
    fiber.set_state(FiberState::Dead);
    fiber.suspend_reason.store(SUSPEND_DEAD, Ordering::Release);
    fiber.result.complete(result);
    // SAFETY: called on the fiber's own stack at the trampoline tail, exactly
    // where a completed fiber must hand control back to its scheduler.
    if unsafe { swap_fiber_to_scheduler(fiber) }.is_err() {
        crate::syscall::abort();
    }
    crate::syscall::abort();
}

#[cfg(egcl_unix_fibers)]
unsafe fn swap_fiber_to_scheduler(fiber: &Fiber) -> Result<(), EgclError> {
    // Read the scheduler-return context from the fiber (migration-safe; see the
    // `scheduler_return` field). A thread-local would be unsound here.
    let scheduler = fiber.scheduler_return.load(Ordering::Acquire) as *mut crate::context::Context;
    if scheduler.is_null() {
        return Err(EgclError::Internal(
            "fiber has no mounted scheduler context".into(),
        ));
    }
    // Save the fiber's context into its cell and switch back to the carrier.
    // SAFETY: `scheduler` points at the carrier's live on-stack context.
    unsafe { crate::context::swap(fiber.execution_context.as_ptr(), *scheduler) };
    Ok(())
}

#[cfg(all(target_arch = "x86_64", windows))]
unsafe fn swap_fiber_to_scheduler(fiber: &Fiber) -> Result<(), EgclError> {
    unsafe { windows::suspend(&fiber.scheduler_return) }
}

fn run_fiber_entry(thread: &Fiber) -> Result<EgclVal, EgclError> {
    let entry = thread.entry();
    let mut result = run_entry(entry);

    if thread.has_interrupt() {
        result = Ok(thread.take_interrupt().unwrap_or(NIL));
    }

    result
}

/// A callback, installed by the interpreter/runtime host, that runs a Lisp
/// function value (interpreted closure, bytecode function, or symbol naming a
/// function) to completion on the *current* thread and returns its primary
/// value. `egcl-rt` cannot call the interpreter directly (it is the lower
/// layer), so the host registers this hook at startup — mirroring
/// `install_gc_hooks` (egcl-rt/lib.rs) and `set_runtime_init_hook`. Without it,
/// only bare native `fn() -> EgclVal` entry points can run on a spawned thread
/// (the pre-existing behaviour, used by the Rust-level threading tests).
pub type ThreadEntryRunner = fn(EgclVal) -> Result<EgclVal, EgclError>;

static THREAD_ENTRY_RUNNER: OnceLock<ThreadEntryRunner> = OnceLock::new();

/// Install the host's Lisp thread-entry runner (see [`ThreadEntryRunner`]).
/// Idempotent; a second call is ignored.
pub fn set_thread_entry_runner(runner: ThreadEntryRunner) {
    let _ = THREAD_ENTRY_RUNNER.set(runner);
}

pub fn run_entry(entry: EgclVal) -> Result<EgclVal, EgclError> {
    if entry.is_function() {
        // A bare native code entry point (`TAG_FUNCTION` = an untagged code
        // address). Interpreted/bytecode function objects are heap objects, not
        // `TAG_FUNCTION`, so they fall through to the host runner below.
        let fn_addr = entry.0 & !crate::value::TAG_MASK;
        let function: fn() -> EgclVal = unsafe { std::mem::transmute(fn_addr) };
        Ok(function())
    } else if matches!(entry.0, crate::value::NIL_BITS | crate::value::T_BITS) {
        Ok(entry)
    } else if let Some(runner) = THREAD_ENTRY_RUNNER.get() {
        // A Lisp function value: hand it to the interpreter host, which builds a
        // fresh per-thread environment and applies it (bliss-q9i1).
        runner(entry)
    } else {
        Err(EgclError::TypeError {
            datum: entry,
            expected: "function".to_string(),
        })
    }
}

// ── Exposed native/platform thread API ─────────────────────────────

/// Create a dedicated one-to-one OS-backed native thread.
pub fn make_thread(entry: EgclVal) -> Result<NativeThreadId, EgclError> {
    make_thread_named(entry, None)
}

/// Create a named dedicated one-to-one OS-backed native thread.
///
/// When no name is supplied the generated name remains stable for the
/// lifetime of the thread descriptor and is also used as the host thread's
/// diagnostic name.
pub fn make_thread_named(
    entry: EgclVal,
    name: Option<String>,
) -> Result<NativeThreadId, EgclError> {
    make_thread_named_with_setup(entry, name, || ())
}

/// Create a native thread with host state installed before its Lisp entry runs.
/// The setup result remains alive throughout entry execution and is dropped on
/// the worker before completion is published, including during unwinding.
/// Only the setup closure crosses threads; its returned guard need not be Send.
pub fn make_thread_named_with_setup<F, G>(
    entry: EgclVal,
    name: Option<String>,
    setup: F,
) -> Result<NativeThreadId, EgclError>
where
    F: FnOnce() -> G + Send + 'static,
{
    let id = NativeThreadId(NEXT_NATIVE_THREAD_ID.fetch_add(1, Ordering::Relaxed));
    let name = name.unwrap_or_else(|| format!("egcl-thread-{}", id.0));
    let result = Arc::new(ThreadResult::new());
    let thread = Arc::new(NativeThread::new(
        id,
        Some(name.clone()),
        false,
        false,
        entry,
        Arc::clone(&result),
    ));
    native_thread_registry()
        .lock()
        .unwrap()
        .insert(id, Arc::clone(&thread));

    let running = Arc::clone(&thread);
    let handle = match std::thread::Builder::new().name(name).spawn(move || {
        install_current_native_thread(Arc::clone(&running));
        crate::safepoint::transition_native_state(&running, NativeThreadState::Running);
        // An unwinding entry must still publish completion: JOIN waits on
        // this result before it can inspect the OS join handle. Catch at
        // the worker boundary; after a panic this worker retires without
        // evaluating any more Lisp or hiding the failure with an interrupt.
        let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _host_state = setup();
            let mut value = run_entry(running.entry());
            if let Some(interrupt) = running.take_interrupt() {
                value = Ok(interrupt);
            }
            value
        }))
        .unwrap_or_else(|_| {
            Err(EgclError::Internal(format!(
                "native thread {} panicked",
                id.0
            )))
        });
        running.stack.publish_top();
        result.complete(value);
        crate::safepoint::retire_native_thread(&running);
    }) {
        Ok(handle) => handle,
        Err(error) => {
            native_thread_registry().lock().unwrap().remove(&id);
            return Err(EgclError::Internal(format!(
                "failed to create native thread: {error}"
            )));
        }
    };
    *thread.join_handle.lock().unwrap() = Some(handle);
    Ok(id)
}

/// Join a dedicated native thread and return its entry value.
pub fn join_thread(id: NativeThreadId) -> Result<EgclVal, EgclError> {
    match join_thread_timeout(id, None)? {
        Some(value) => Ok(value),
        None => unreachable!("an unbounded native-thread join cannot time out"),
    }
}

/// Join a dedicated native thread, optionally bounded by `timeout`.
///
/// `Ok(None)` means that the deadline expired. In that case neither the
/// result nor the OS join handle is consumed, so a later join remains valid.
/// `Ok(Some(value))` consumes the thread exactly like [`join_thread`].
pub fn join_thread_timeout(
    id: NativeThreadId,
    timeout: Option<std::time::Duration>,
) -> Result<Option<EgclVal>, EgclError> {
    if id == current_thread_id() {
        return Err(EgclError::ProgramError(
            "a thread cannot join itself".into(),
        ));
    }
    let thread = {
        let mut registry = native_thread_registry().lock().unwrap();
        prune_inactive_auto_registered_threads(&mut registry);
        registry
            .get(&id)
            .cloned()
            .ok_or_else(|| EgclError::Internal(format!("no native thread with id {}", id.0)))?
    };
    #[cfg(test)]
    join_completion_tests::retained_result();
    // Leave the result in its scanned cell throughout the native wait. The
    // scope's Drop waits out any active collection before we move the value.
    let ready = {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        match timeout {
            Some(timeout) => thread.result.wait_until_ready_for(timeout),
            None => {
                thread.result.wait_until_ready();
                true
            }
        }
    };
    if !ready {
        return Ok(None);
    }
    let mut value = thread
        .result
        .completion
        .lock()
        .unwrap()
        .value
        .take()
        .ok_or_else(|| {
            EgclError::ProgramError(format!("native thread {} was already joined", id.0))
        })?;
    crate::rooted_ref!(_value_root = &mut value);
    // OS teardown can outlast result publication. Keep the result rooted and
    // participate in GC while waiting, with no join-handle lock held.
    let handle = thread.join_handle.lock().unwrap().take();
    if let Some(handle) = handle {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        handle
            .join()
            .map_err(|_| EgclError::Internal(format!("native thread {} panicked", id.0)))?;
    }
    native_thread_registry().lock().unwrap().remove(&id);
    drop(_value_root);
    value.map(Some)
}

pub fn current_thread_id() -> NativeThreadId {
    ensure_current_native_thread().id()
}

pub fn current_thread() -> &'static NativeThread {
    let thread = ensure_current_native_thread();
    unsafe { &*Arc::as_ptr(&thread) }
}

/// A foreign-created thread starts quiescent. Its callback entry joins the
/// collector's Running participant set only under the safepoint transition lock.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub(crate) fn current_thread_for_foreign_entry() -> &'static NativeThread {
    let thread = ensure_current_native_thread_in_state(NativeThreadState::Native);
    unsafe { &*Arc::as_ptr(&thread) }
}

pub fn all_thread_ids() -> Vec<NativeThreadId> {
    let _ = current_thread_id();
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry.keys().copied().collect()
}

/// Return the stable diagnostic name for a registered native thread.
pub fn thread_name(id: NativeThreadId) -> Option<String> {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry.get(&id).and_then(|thread| thread.name.clone())
}

/// Whether a registered native thread has not reached its terminal state.
/// Missing (including already joined) thread handles return `None`.
pub fn thread_alive(id: NativeThreadId) -> Option<bool> {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry.get(&id).map(|thread| thread.is_alive())
}

/// Snapshot the registered threads that have not reached a terminal state.
pub fn live_thread_ids() -> Vec<NativeThreadId> {
    let _ = current_thread_id();
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry
        .iter()
        .filter_map(|(id, thread)| thread.is_alive().then_some(*id))
        .collect()
}

pub fn thread_is_carrier(id: NativeThreadId) -> Option<bool> {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry.get(&id).map(|thread| thread.is_carrier())
}

pub fn carrier_thread_ids() -> Vec<NativeThreadId> {
    worker_pool().carrier_ids()
}

pub fn interrupt_thread(id: NativeThreadId, condition: EgclVal) -> Result<(), EgclError> {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    let thread = registry
        .get(&id)
        .ok_or_else(|| EgclError::Internal(format!("no native thread with id {}", id.0)))?;
    if thread.state() != NativeThreadState::Dead {
        thread.post_interrupt(condition);
    }
    Ok(())
}

pub fn post_current_pending_signal(signal: PendingSignal) {
    if let Some(fiber) = current_fiber() {
        fiber.post_pending_signal(signal);
    } else {
        current_thread().post_pending_signal(signal);
    }
}

/// Mark the mounted fiber, or otherwise the current native thread, as the
/// foreground execution target for process-directed deferred signals.
pub fn set_current_execution_foreground() {
    if let Some(fiber) = current_fiber() {
        FOREGROUND_EXECUTION_ID.store(fiber.id().0, Ordering::Release);
        FOREGROUND_EXECUTION_KIND.store(FOREGROUND_FIBER, Ordering::Release);
    } else {
        let id = current_thread_id();
        FOREGROUND_EXECUTION_ID.store(id.0, Ordering::Release);
        FOREGROUND_EXECUTION_KIND.store(FOREGROUND_NATIVE_THREAD, Ordering::Release);
    }
}

pub fn post_foreground_pending_signal(signal: PendingSignal) -> Result<(), EgclError> {
    let kind = FOREGROUND_EXECUTION_KIND.load(Ordering::Acquire);
    let id = FOREGROUND_EXECUTION_ID.load(Ordering::Acquire);
    match kind {
        FOREGROUND_NATIVE_THREAD => {
            let mut registry = native_thread_registry().lock().unwrap();
            prune_inactive_auto_registered_threads(&mut registry);
            let thread = registry.get(&NativeThreadId(id)).ok_or_else(|| {
                EgclError::Internal(format!("no foreground native thread with id {id}"))
            })?;
            thread.post_pending_signal(signal);
            Ok(())
        }
        FOREGROUND_FIBER => {
            let registry = fiber_registry().lock().unwrap();
            let fiber = registry
                .get(&FiberId(id))
                .ok_or_else(|| EgclError::Internal(format!("no foreground fiber with id {id}")))?;
            fiber.post_pending_signal(signal);
            Ok(())
        }
        _ => Err(EgclError::Internal(
            "no foreground execution is registered".into(),
        )),
    }
}

pub fn take_current_pending_signal() -> Option<PendingSignal> {
    current_fiber()
        .and_then(Fiber::take_pending_signal)
        .or_else(|| current_thread().take_pending_signal())
}

pub fn with_current_condition_state_mut<T>(f: impl FnOnce(&mut ThreadConditionState) -> T) -> T {
    if let Some(fiber) = current_fiber() {
        fiber.with_condition_state_mut(f)
    } else {
        current_thread().with_condition_state_mut(f)
    }
}

pub fn current_condition_state_snapshot() -> ConditionStateSnapshot {
    if let Some(fiber) = current_fiber() {
        fiber.condition_state_snapshot()
    } else {
        current_thread().condition_state_snapshot()
    }
}

pub fn start_current_sandbox_cpu_deadline(limit_ms: u64) -> Result<(), EgclError> {
    if limit_ms == 0 {
        clear_current_sandbox_cpu_deadline();
        return Ok(());
    }
    let now = crate::syscall::thread_cpu_time_ns()
        .map_err(|errno| EgclError::Internal(format!("clock_gettime failed: {errno}")))?;
    let limit_ns = limit_ms.saturating_mul(1_000_000);
    SANDBOX_CPU_DEADLINE_NS.with(|deadline| deadline.set(now.saturating_add(limit_ns).max(1)));
    crate::runtime::mark_process_signal_activity();
    Ok(())
}

pub fn clear_current_sandbox_cpu_deadline() {
    SANDBOX_CPU_DEADLINE_NS.with(|deadline| deadline.set(0));
}

pub fn poll_current_sandbox_cpu_deadline() -> Result<(), EgclError> {
    let deadline = SANDBOX_CPU_DEADLINE_NS.with(|deadline| deadline.get());
    if deadline == 0 {
        return Ok(());
    }
    let now = crate::syscall::thread_cpu_time_ns()
        .map_err(|errno| EgclError::Internal(format!("clock_gettime failed: {errno}")))?;
    if now >= deadline {
        clear_current_sandbox_cpu_deadline();
        post_current_pending_signal(PendingSignal::Timeout);
    }
    Ok(())
}

pub fn thread_yield() {
    std::thread::yield_now();
}

// ── Lightweight fiber API ─────────────────────────────────────────

/// Allocate a fiber in `Created` state. It does not run until submitted.
pub fn make_fiber(entry: EgclVal) -> Result<FiberId, EgclError> {
    make_fiber_with_stack_size(entry, 512 * 1024)
}

/// Allocate a fiber with an explicit native stack reservation in bytes.
pub fn make_fiber_with_stack_size(entry: EgclVal, stack_size: usize) -> Result<FiberId, EgclError> {
    if !(64 * 1024..=1024 * 1024 * 1024).contains(&stack_size) {
        return Err(EgclError::ProgramError(
            "fiber stack size must be between 64 KiB and 1 GiB".into(),
        ));
    }

    let id = FiberId(NEXT_FIBER_ID.fetch_add(1, Ordering::Relaxed));
    let result = Arc::new(ThreadResult::new());
    let fiber = Arc::new(Fiber {
        id,
        name: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 1),
            "fiber name",
            None,
        ),
        entry: AtomicU64::new(entry.0),
        state: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 2),
            "fiber state",
            FiberState::Created,
        ),
        lisp_frames: Mutex::new(RecordedCalls::default()),
        stack: EgclStack::new(default_stack_size()),
        continuation: FiberContinuation::default(),
        execution_context: FiberExecutionContext::new(stack_size)?,
        native_stack_size: stack_size,
        host_roots: crate::gc::FiberRoots::new(),
        native_faults: crate::runtime::FiberFaultState::default(),
        scheduler_return: AtomicUsize::new(0),
        scheduler_pool: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 3),
            "fiber scheduler owner",
            None,
        ),
        mounted: AtomicBool::new(false),
        wake_pending: AtomicBool::new(false),
        suspend_reason: AtomicU8::new(SUSPEND_NONE),
        wait_generation: AtomicU64::new(0),
        tls: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 4),
            "fiber TLS",
            vec![NIL; MAX_TLS],
        ),
        dynamic_bindings: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 5),
            "fiber dynamic bindings",
            Vec::new(),
        ),
        handler_stack: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 6),
            "fiber handler stack",
            Vec::new(),
        ),
        restart_stack: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 7),
            "fiber restart stack",
            Vec::new(),
        ),
        condition_state: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 9),
            "fiber condition state",
            ThreadConditionState::new(),
        ),
        pin_count: AtomicUsize::new(0),
        // Carrier 0 = not yet mounted; set by the carrier's mount handshake.
        execution: crate::exec_context::LispExecutionContext::new(
            crate::exec_context::ExecutionKind::Fiber,
            id.0,
            0,
        ),
        result,
        interrupt_pending: AtomicBool::new(false),
        interrupt_value: OrderedMutex::new(
            LockLevel::ExecutionObject,
            fiber_object_order(id, 8),
            "fiber interrupt",
            NIL,
        ),
        published_sp: AtomicUsize::new(0),
        published_fp: AtomicUsize::new(0),
        pending_signals: AtomicU8::new(0),
    });
    fiber_registry()
        .lock()
        .unwrap()
        .insert(id, Arc::clone(&fiber));
    Ok(id)
}

pub fn submit_fiber(id: FiberId) -> Result<(), EgclError> {
    submit_fiber_to_pool(id, worker_pool())
}

fn submit_fiber_to_pool(id: FiberId, pool: &Arc<WorkerPool>) -> Result<(), EgclError> {
    let fiber = fiber_registry()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| EgclError::Internal(format!("no fiber with id {}", id.0)))?;
    {
        let mut owner = fiber.scheduler_pool.lock().unwrap();
        match owner.as_ref().and_then(Weak::upgrade) {
            Some(existing) if !Arc::ptr_eq(&existing, pool) => {
                return Err(EgclError::ProgramError(format!(
                    "fiber {} already belongs to another scheduler group",
                    id.0
                )));
            }
            Some(_) => {}
            None => *owner = Some(Arc::downgrade(pool)),
        }
    }
    let enqueue = {
        let mut state = fiber.state.lock().unwrap();
        match *state {
            FiberState::Created => {
                *state = FiberState::Runnable;
                true
            }
            FiberState::Blocked | FiberState::Waiting => {
                if fiber.mounted.load(Ordering::Acquire) {
                    // The carrier will observe this while holding the same
                    // state lock after swapcontext returns.  That lock closes
                    // the otherwise-lost-wakeup window around mounted=false.
                    fiber.wake_pending.store(true, Ordering::Release);
                    false
                } else {
                    *state = FiberState::Runnable;
                    true
                }
            }
            _ => {
                return Err(EgclError::ProgramError(format!(
                    "fiber {} is already submitted",
                    id.0
                )));
            }
        }
    };
    if enqueue {
        pool.submit(WorkerTask {
            result_cell: Arc::clone(&fiber.result),
            thread: fiber,
        });
    }
    Ok(())
}

pub fn join_fiber(id: FiberId) -> Result<EgclVal, EgclError> {
    if current_fiber_id() == Some(id) {
        return Err(EgclError::ProgramError("a fiber cannot join itself".into()));
    }
    let result = fiber_registry()
        .lock()
        .unwrap()
        .get(&id)
        .map(|fiber| Arc::clone(&fiber.result))
        .ok_or_else(|| EgclError::Internal(format!("no fiber with id {}", id.0)))?;
    #[cfg(test)]
    join_completion_tests::retained_result();
    // Keep the result in its registry-scanned cell until any active GC has
    // finished. Waiting must not leave the native caller marked Running.
    {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        result.wait_for_fiber_join()?;
    }
    let mut value = result
        .completion
        .lock()
        .unwrap()
        .value
        .take()
        .ok_or_else(|| EgclError::ProgramError(format!("fiber {} was already joined", id.0)))?;
    crate::rooted_ref!(_value_root = &mut value);
    // Dropping the final owner unregisters host roots (GcWorld). Release the
    // higher-level execution registry lock before running that destructor.
    let removed = fiber_registry().lock().unwrap().remove(&id);
    drop(removed);
    drop(_value_root);
    value
}

// A fiber can resume on another OS thread. Keep the TLS address computation
// inside a fresh call so LLVM cannot hoist it across a yielding caller.
#[inline(never)]
pub fn current_fiber() -> Option<&'static Fiber> {
    ACTIVE_FIBER
        .try_with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|fiber| unsafe { &*Arc::as_ptr(fiber) })
        })
        .ok()
        .flatten()
}

pub fn current_fiber_id() -> Option<FiberId> {
    current_fiber().map(Fiber::id)
}

pub fn all_fiber_ids() -> Vec<FiberId> {
    fiber_registry().lock().unwrap().keys().copied().collect()
}

pub fn fiber_state(id: FiberId) -> Option<FiberState> {
    fiber_registry().lock().unwrap().get(&id).map(|f| f.state())
}

pub fn fiber_carrier_thread(id: FiberId) -> Option<NativeThreadId> {
    fiber_registry()
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|fiber| fiber.carrier_id())
}

fn lookup_fiber(id: FiberId) -> Result<Arc<Fiber>, EgclError> {
    fiber_registry()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| EgclError::ProgramError("unknown fiber".into()))
}
pub fn fiber_pin(id: FiberId) -> Result<(), EgclError> {
    lookup_fiber(id)?.pin();
    Ok(())
}
pub fn fiber_unpin(id: FiberId) -> Result<(), EgclError> {
    lookup_fiber(id)?.unpin()
}
pub fn fiber_can_yield(id: FiberId) -> Result<bool, EgclError> {
    Ok(lookup_fiber(id)?.can_yield())
}

enum CallFrameOwner {
    Fiber(Arc<Fiber>),
    Native(Arc<NativeThread>),
}

impl CallFrameOwner {
    fn frames(&self) -> &Mutex<RecordedCalls> {
        match self {
            Self::Fiber(fiber) => &fiber.lisp_frames,
            Self::Native(thread) => &thread.lisp_frames,
        }
    }
}

/// An interpreter call whose arguments are traced with its execution's roots.
/// The owner follows a fiber across carrier migration. Native calls belong to
/// their native execution, and are not inherited by fibers mounted there.
pub struct FiberCallFrame {
    owner: CallFrameOwner,
    // Dropping on another execution would violate stack ordering. A fiber's
    // suspended Rust stack can migrate without sending individual guards.
    _execution_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl FiberCallFrame {
    pub fn enter(name: &str) -> Self {
        Self::enter_recorded(Some(name), None, FrameOrigin::Interpreted)
    }

    pub fn enter_with_args(name: &str, arguments: &[EgclVal]) -> Self {
        Self::enter_recorded(Some(name), Some(arguments), FrameOrigin::Interpreted)
    }

    pub fn enter_anonymous_with_args(arguments: &[EgclVal]) -> Self {
        Self::enter_recorded(None, Some(arguments), FrameOrigin::Interpreted)
    }

    /// Record original arguments for the current, already-pushed managed call
    /// frame. The guard must be dropped when that activation leaves the stack.
    /// This augments the physical frame rather than adding a second Lisp call.
    pub fn enter_managed(name: &str, arguments: &[EgclVal]) -> Self {
        Self::enter_recorded(Some(name), Some(arguments), FrameOrigin::Managed)
    }

    fn enter_recorded(
        name: Option<&str>,
        arguments: Option<&[EgclVal]>,
        origin: FrameOrigin,
    ) -> Self {
        let owner = match current_fiber_id() {
            Some(id) => {
                CallFrameOwner::Fiber(lookup_fiber(id).expect("mounted fiber is registered"))
            }
            None => match current_native_thread_if_registered() {
                Some(thread) => CallFrameOwner::Native(thread),
                None => {
                    // First-time registration may wait at a safepoint. Preserve
                    // the old API contract by rooting a one-time copy before it;
                    // registered executions take the allocation-free path.
                    crate::rooted!(arguments = arguments.map(<[EgclVal]>::to_vec));
                    let owner = CallFrameOwner::Native(ensure_current_native_thread());
                    return Self::enter_on_owner(owner, name, arguments.as_deref(), origin);
                }
            },
        };
        Self::enter_on_owner(owner, name, arguments, origin)
    }

    fn enter_on_owner(
        owner: CallFrameOwner,
        name: Option<&str>,
        arguments: Option<&[EgclVal]>,
        origin: FrameOrigin,
    ) -> Self {
        let anchor = current_stack().fp() as usize;
        owner
            .frames()
            .lock()
            .unwrap()
            .push(anchor, name, arguments, origin);
        Self {
            owner,
            _execution_affine: std::marker::PhantomData,
        }
    }
}
impl Drop for FiberCallFrame {
    fn drop(&mut self) {
        self.owner.frames().lock().unwrap().pop();
    }
}

pub(crate) fn current_backtrace(count: usize) -> Vec<crate::debug_stack::LogicalFrame> {
    let mut snapshot = crate::debug_stack::PendingBacktrace::default();
    crate::rooted_ref!(_snapshot = &mut snapshot);
    snapshot.recorded = if let Some(fiber) = current_fiber() {
        fiber.lisp_frames.lock().unwrap().snapshot(count)
    } else {
        let thread = current_thread();
        thread.lisp_frames.lock().unwrap().snapshot(count)
    };
    // SAFETY: this execution owns its active stack; copying does not yield.
    unsafe { snapshot.copy_stack(current_stack().fp(), count) };
    snapshot.resolve(count)
}

/// Snapshot an unmounted continuation. None means its stack is still running.
/// The state lock prevents a worker from mounting the stack during the copy.
pub(crate) fn fiber_logical_backtrace(
    id: FiberId,
    count: usize,
) -> Result<Option<Vec<crate::debug_stack::LogicalFrame>>, EgclError> {
    let fiber = lookup_fiber(id)?;
    let mut snapshot = crate::debug_stack::PendingBacktrace::default();
    crate::rooted_ref!(_snapshot = &mut snapshot);
    {
        let state = fiber.state.lock().unwrap();
        if fiber.mounted.load(Ordering::Acquire) || *state == FiberState::Running {
            return Ok(None);
        }
        snapshot.recorded = {
            let frames = fiber.lisp_frames.lock().unwrap();
            frames.snapshot(count)
        };
        // SAFETY: Arc owns the stack; the mount/state lock excludes a mutator.
        unsafe { snapshot.copy_stack(fiber.stack.published_fp(), count) };
        if *state == FiberState::Created && count != 0 {
            snapshot.entry(fiber.entry());
        }
    }
    Ok(Some(snapshot.resolve(count)))
}

/// Compatibility string view of the shared logical-frame snapshot.
pub fn fiber_backtrace(id: FiberId, count: usize) -> Result<Option<Vec<String>>, EgclError> {
    Ok(fiber_logical_backtrace(id, count)?.map(|frames| {
        frames
            .into_iter()
            .map(|frame| frame.function.unwrap_or_else(|| "<anonymous function>".into()))
            .collect()
    }))
}

pub fn fiber_yield() -> Result<(), EgclError> {
    let fiber = current_fiber()
        .ok_or_else(|| EgclError::ProgramError("FIBER-YIELD outside a fiber".into()))?;
    if !fiber.can_yield() {
        return Err(EgclError::ProgramError(
            "cannot yield while fiber is pinned".into(),
        ));
    }
    fiber.stack.publish_top();
    fiber.continuation.save(
        fiber.stack.published_sp(),
        fiber.stack.published_fp() as usize,
        1,
    );
    fiber.publish_stack(
        fiber.stack.published_sp(),
        fiber.stack.published_fp() as usize,
    );

    #[cfg(egcl_fibers)]
    {
        fiber.suspend_reason.store(SUSPEND_YIELD, Ordering::Release);
        fiber.set_state(FiberState::Suspended);
        unsafe { swap_fiber_to_scheduler(fiber)? };
    }

    #[cfg(not(egcl_fibers))]
    std::thread::yield_now();

    Ok(())
}

/// Park the mounted fiber without blocking its carrier. `unpark` on the owning
/// scheduler group makes it runnable again.
pub fn park_current_fiber() -> Result<(), EgclError> {
    prepare_current_fiber_park(FiberState::Blocked)?;
    park_prepared_current_fiber()
}

/// Publish the mounted fiber's roots and enter a blocked/waiting state while
/// it is still running on the carrier.  Callers use this while holding their
/// wait-queue lock, then release that lock and call
/// [`park_prepared_current_fiber`].  A concurrent wake is recorded in
/// `wake_pending`, closing the classic enqueue-to-park lost-wakeup window.
pub(crate) fn prepare_current_fiber_park(state: FiberState) -> Result<(FiberId, u64), EgclError> {
    if !matches!(state, FiberState::Blocked | FiberState::Waiting) {
        return Err(EgclError::Internal(
            "fiber can only prepare a Blocked or Waiting park".into(),
        ));
    }
    let fiber = current_fiber()
        .ok_or_else(|| EgclError::ProgramError("fiber park outside a fiber".into()))?;
    if !fiber.can_yield() {
        return Err(EgclError::ProgramError(
            "cannot park while fiber is pinned".into(),
        ));
    }
    fiber.stack.publish_top();
    fiber.continuation.save(
        fiber.stack.published_sp(),
        fiber.stack.published_fp() as usize,
        2,
    );
    fiber.publish_stack(
        fiber.stack.published_sp(),
        fiber.stack.published_fp() as usize,
    );
    let token = fiber.wait_generation.fetch_add(1, Ordering::AcqRel) + 1;
    fiber.suspend_reason.store(SUSPEND_PARK, Ordering::Release);
    fiber.set_state(state);
    Ok((fiber.id(), token))
}

/// Finish a park prepared by [`prepare_current_fiber_park`].
pub(crate) fn park_prepared_current_fiber() -> Result<(), EgclError> {
    let fiber = current_fiber()
        .ok_or_else(|| EgclError::ProgramError("fiber park outside a fiber".into()))?;
    // Ports without the native context switch only yield below, so FIBER goes
    // unread there. The binding still earns its keep on every target: it is the
    // "called outside a fiber" check, and dropping it would turn a caller's bug
    // into a silent no-op (bliss-w2vp).
    #[cfg(not(egcl_fibers))]
    let _ = &fiber;

    #[cfg(egcl_fibers)]
    {
        if !matches!(fiber.state(), FiberState::Blocked | FiberState::Waiting) {
            return Err(EgclError::Internal(
                "fiber park was not prepared before unmount".into(),
            ));
        }
        unsafe { swap_fiber_to_scheduler(fiber)? };
    }

    #[cfg(not(egcl_fibers))]
    std::thread::yield_now();

    Ok(())
}

/// Undo a prepared park if registration with the blocking subsystem fails.
pub(crate) fn cancel_prepared_current_fiber_park() {
    if let Some(fiber) = current_fiber() {
        fiber.suspend_reason.store(SUSPEND_NONE, Ordering::Release);
        fiber.set_state(FiberState::Running);
    }
}

/// Wake a blocked fiber if `token` still names its current wait.  This is the
/// common completion path for synchronization, timers, and I/O readiness.
pub(crate) fn wake_fiber_wait(id: FiberId, token: u64) -> bool {
    let fiber = match fiber_registry().lock().unwrap().get(&id).cloned() {
        Some(fiber) => fiber,
        None => return false,
    };
    if fiber.wait_generation.load(Ordering::Acquire) != token {
        return false;
    }
    let pool = fiber
        .scheduler_pool
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Weak::upgrade);
    let Some(pool) = pool else {
        return false;
    };
    submit_fiber_to_pool(id, &pool).is_ok()
}

pub fn interrupt_fiber(id: FiberId, condition: EgclVal) -> Result<(), EgclError> {
    let registry = fiber_registry().lock().unwrap();
    let fiber = registry
        .get(&id)
        .ok_or_else(|| EgclError::Internal(format!("no fiber with id {}", id.0)))?;
    if fiber.state() != FiberState::Dead {
        fiber.post_interrupt(condition);
    }
    Ok(())
}

/// Stack currently executing CL code: mounted fiber first, native thread otherwise.
pub fn current_stack() -> &'static EgclStack {
    current_fiber()
        .map(Fiber::stack)
        .unwrap_or_else(|| current_thread().stack())
}

pub fn thread_published_fp(id: NativeThreadId) -> Option<*const crate::stack::Frame> {
    let mut registry = native_thread_registry().lock().ok()?;
    prune_inactive_auto_registered_threads(&mut registry);
    registry
        .get(&id)
        .map(|thread| thread.stack().published_fp())
}

pub fn fiber_published_fp(id: FiberId) -> Option<*const crate::stack::Frame> {
    fiber_registry()
        .lock()
        .ok()?
        .get(&id)
        .map(|fiber| fiber.stack().published_fp())
}

pub fn safepoint_participant_count_excluding(current: NativeThreadId) -> usize {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry
        .iter()
        .filter(|(id, thread)| {
            **id != current
                && thread.state() == NativeThreadState::Running
                && thread.gc_participates()
        })
        .count()
}

/// Interrupt running native mutators so a blocking syscall returns `EINTR` and
/// the thread can observe the pending stop-the-world request at its next poll.
/// The SIGUSR1 handler itself only sets a flag; it never suspends the thread.
#[cfg(unix)]
pub(crate) fn signal_safepoint_participants(current: NativeThreadId) -> usize {
    let mut registry = native_thread_registry().lock().unwrap();
    prune_inactive_auto_registered_threads(&mut registry);
    registry
        .iter()
        .filter(|(id, thread)| {
            **id != current
                && thread.state() == NativeThreadState::Running
                && thread.gc_participates()
        })
        .filter(|(_, thread)| {
            // Deliver the safepoint interrupt by kernel TID (tgkill), the
            // no-libc equivalent of pthread_kill(pthread_t, SIGUSR1).
            let tid = thread.os_thread_id.load(Ordering::Acquire);
            tid != 0
                && crate::syscall::tgkill(
                    crate::syscall::getpid(),
                    tid as i32,
                    crate::syscall::SIGUSR1,
                )
                .is_ok()
        })
        .count()
}

#[cfg(not(unix))]
pub(crate) fn signal_safepoint_participants(_current: NativeThreadId) -> usize {
    0
}

pub fn wait_for_other_threads() {
    let current = current_thread_id();
    loop {
        let native_pending = {
            let mut registry = native_thread_registry().lock().unwrap();
            prune_inactive_auto_registered_threads(&mut registry);
            registry.iter().any(|(id, thread)| {
                *id != current
                    && !thread.is_carrier()
                    && thread.gc_participates()
                    && thread.state() != NativeThreadState::Dead
            })
        };
        let fiber_pending =
            fiber_registry().lock().unwrap().values().any(|fiber| {
                fiber.state() != FiberState::Created && fiber.state() != FiberState::Dead
            });
        if !native_pending && !fiber_pending {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(test)]
mod native_join_gc_tests {
    use super::*;

    #[test]
    fn join_roots_result_while_waiting_for_native_exit() {
        const CHILD: &str = "EGCL_TEST_JOIN_GC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "thread::native_join_gc_tests::join_roots_result_while_waiting_for_native_exit",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        crate::gc::ensure_heap_initialized();
        current_thread_id();
        let id = NativeThreadId(NEXT_NATIVE_THREAD_ID.fetch_add(1, Ordering::Relaxed));
        let result = Arc::new(ThreadResult::new());
        let thread = Arc::new(NativeThread::new(
            id,
            Some("join-gc-worker".into()),
            false,
            false,
            NIL,
            Arc::clone(&result),
        ));
        native_thread_registry()
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&thread));
        let running = Arc::clone(&thread);
        let (observed, collection) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            install_current_native_thread(Arc::clone(&running));
            crate::safepoint::transition_native_state(&running, NativeThreadState::Running);
            let body = crate::gc::alloc_typed(8, crate::object::type_id::DOUBLE_FLOAT).unwrap();
            unsafe { *(body as *mut f64) = 42.0 };
            let original = unsafe { EgclVal::from_heap_ptr(body.sub(8)) }.to_raw();
            result.complete(Ok(EgclVal::from_raw(original)));
            // The Lisp result can be available before the OS thread exits.
            // Collect only after JOIN has taken it from the result cell: its
            // caller must keep that value rooted throughout the remaining wait.
            while result.completion.lock().unwrap().value.is_some() {
                std::thread::yield_now();
            }
            let collected = crate::gc::collect_t0_minor();
            observed.send((collected, original)).unwrap();
            crate::safepoint::retire_native_thread(&running);
        });
        *thread.join_handle.lock().unwrap() = Some(handle);
        let value = join_thread(id).unwrap();
        let (collected, original) = collection.recv().unwrap();
        collected.expect("worker GC during native join");
        assert!(crate::gc::heap_stats().minor_gc_count > 0);
        assert_ne!(value.to_raw(), original, "JOIN result must be relocated");
        assert_eq!(unsafe { *(value.as_ptr().add(8) as *const f64) }, 42.0);
    }
}

#[cfg(test)]
mod chase_lev_tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    fn task(number: i64) -> WorkerTask {
        let id = make_fiber(EgclVal::from_fixnum(number)).unwrap();
        let fiber = fiber_registry().lock().unwrap().remove(&id).unwrap();
        WorkerTask {
            result_cell: Arc::clone(&fiber.result),
            thread: fiber,
        }
    }

    fn number(task: &WorkerTask) -> i64 {
        task.thread.entry().as_fixnum()
    }

    #[test]
    fn owner_is_lifo_and_thief_is_fifo() {
        let deque = ChaseLevDeque::new();
        assert!(deque.push(task(1)).is_ok());
        assert!(deque.push(task(2)).is_ok());
        assert!(deque.push(task(3)).is_ok());

        assert_eq!(number(&deque.pop().unwrap()), 3);
        assert_eq!(number(&deque.steal().unwrap()), 1);
        assert_eq!(number(&deque.pop().unwrap()), 2);
        assert!(deque.pop().is_none());
        assert!(deque.steal().is_none());
    }

    #[test]
    fn idle_carrier_steals_external_work_from_blocked_peer() {
        let pool = WorkerPool::new(2);
        // External wakeups land in pending, not the owner-only local deque.
        // Carrier 0 may be in native I/O and unable to drain its inbox.
        let wakeup = task(41);
        pool.workers[0].pending.lock().unwrap().push_back(wakeup);
        assert_eq!(number(&pool.pop_or_steal(1).expect("stranded wakeup")), 41);
        assert!(pool.pop_or_steal(0).is_none());
        assert!(pool.pop_or_steal(1).is_none());
    }

    #[test]
    fn concurrent_thieves_claim_every_task_exactly_once() {
        const TASKS: usize = 10_000;
        const THIEVES: usize = 8;
        let deque = Arc::new(ChaseLevDeque::new());
        for id in 0..TASKS {
            assert!(deque.push(task(id as i64)).is_ok());
        }
        let start = Arc::new(Barrier::new(THIEVES));
        let claimed = Arc::new(AtomicUsize::new(0));
        let values = Arc::new(Mutex::new(Vec::with_capacity(TASKS)));
        let mut handles = Vec::new();
        for _ in 0..THIEVES {
            let deque = Arc::clone(&deque);
            let start = Arc::clone(&start);
            let claimed = Arc::clone(&claimed);
            let values = Arc::clone(&values);
            handles.push(std::thread::spawn(move || {
                start.wait();
                // Hang protection only; the test is about exactly-once claims,
                // not throughput. Eight contending thieves on an in-order
                // RISC-V core manage roughly 1,900 steals per second, so a
                // 5 s deadline left the last ~800 tasks unclaimed there.
                let deadline = Instant::now() + Duration::from_secs(120);
                while claimed.load(Ordering::Acquire) < TASKS && Instant::now() < deadline {
                    if let Some(task) = deque.steal() {
                        values.lock().unwrap().push(number(&task));
                        claimed.fetch_add(1, Ordering::AcqRel);
                    } else {
                        std::thread::yield_now();
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let mut values = values.lock().unwrap().clone();
        values.sort_unstable();
        assert_eq!(values, (0..TASKS as i64).collect::<Vec<_>>());
        assert!(deque.is_empty());
    }
}
