//! Public fiber scheduler-group lifecycle.
//!
//! Scheduling mechanics live in `thread.rs`: one internal deque per exposed
//! native carrier thread, local LIFO execution, cross-carrier FIFO stealing,
//! and carrier park/wakeup. This module owns only the public group lifecycle;
//! it never maintains a second or phantom run queue.

use crate::error::EgclError;
use crate::lock_order::{LockLevel, OrderedMutex};
use crate::thread::{
    CarrierPool, FiberId, FiberState, NativeThreadId, fiber_state, join_fiber, request_fiber_yield,
};
use crate::value::EgclVal;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Requested number of exposed OS carrier threads.
    pub num_workers: usize,
}

/// A public lifecycle handle for fibers scheduled over exposed carrier threads.
pub struct SchedulerGroup {
    requested_carriers: usize,
    pool: CarrierPool,
    carriers: Vec<NativeThreadId>,
    submitted: OrderedMutex<Vec<FiberId>>,
    closed: AtomicBool,
}

impl SchedulerGroup {
    pub fn init(config: &SchedulerConfig) -> Result<Self, EgclError> {
        if config.num_workers == 0 {
            return Err(EgclError::ProgramError(
                "scheduler group requires at least one carrier".into(),
            ));
        }
        let pool = CarrierPool::new(config.num_workers);
        let carriers = pool.carrier_thread_ids();
        static NEXT_GROUP_ORDER: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            requested_carriers: config.num_workers,
            pool,
            carriers,
            submitted: OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                (1_u64 << 63) | NEXT_GROUP_ORDER.fetch_add(1, Ordering::Relaxed),
                "scheduler group submissions",
                Vec::new(),
            ),
            closed: AtomicBool::new(false),
        })
    }

    /// Called on an idle native carrier, outside all scheduler locks.
    pub fn set_idle_hook(&self, hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) {
        self.pool.set_idle_hook(hook);
    }

    pub fn submit(&self, fiber: FiberId) -> Result<(), EgclError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(EgclError::ProgramError(
                "scheduler group is closed to submission".into(),
            ));
        }
        self.pool.submit(fiber)?;
        self.submitted.lock().unwrap().push(fiber);
        Ok(())
    }

    pub fn request_yield(&self, fiber: FiberId) {
        let _ = request_fiber_yield(fiber);
    }

    pub fn unpark(&self, fiber: FiberId) -> Result<(), EgclError> {
        match fiber_state(fiber) {
            Some(FiberState::Blocked | FiberState::Waiting) => self.pool.submit(fiber),
            Some(_) => Err(EgclError::ProgramError(format!(
                "fiber {} is not parked",
                fiber.0
            ))),
            None => Err(EgclError::Internal(format!(
                "fiber {} is unknown",
                fiber.0
            ))),
        }
    }

    pub fn shutdown(&self) -> Result<(), EgclError> {
        self.closed.store(true, Ordering::Release);
        if self.active_fiber_count() == 0 {
            self.pool.shutdown_and_join()
        } else {
            Err(EgclError::ProgramError(
                "cannot shut down a scheduler group with active fibers; use finish".into(),
            ))
        }
    }

    pub fn finish(&self) -> Result<Vec<EgclVal>, EgclError> {
        // The accumulator is scanned while this caller waits for later fibers.
        // Register the caller before publishing or mutating its host roots.
        crate::thread::current_thread_id();
        self.closed.store(true, Ordering::Release);
        let ids = self.submitted.lock().unwrap().clone();
        let mut results: Result<Vec<EgclVal>, EgclError> = Ok(Vec::with_capacity(ids.len()));
        crate::rooted_ref!(_results_root = &mut results);
        for id in ids {
            match join_fiber(id) {
                Ok(value) => results.as_mut().unwrap().push(value),
                Err(error) => {
                    results = Err(error);
                    break;
                }
            }
        }
        self.pool.shutdown_and_join()?;
        drop(_results_root);
        results
    }

    pub fn active_fiber_count(&self) -> usize {
        let submitted = self.submitted.lock().unwrap().clone();
        submitted
            .iter()
            .filter(|&&id| !matches!(fiber_state(id), None | Some(FiberState::Dead)))
            .count()
    }

    /// Compatibility spelling for older Rust callers.
    pub fn active_thread_count(&self) -> usize {
        self.active_fiber_count()
    }

    pub fn carrier_thread_ids(&self) -> &[NativeThreadId] {
        &self.carriers
    }

    pub fn requested_carrier_count(&self) -> usize {
        self.requested_carriers
    }
}

/// Historical Rust facade name. It is a real scheduler group, not a second
/// scheduler or a thread-ID bookkeeping set.
pub type Scheduler = SchedulerGroup;

pub fn run_fibers(
    fibers: &[FiberId],
    config: &SchedulerConfig,
) -> Result<Vec<EgclVal>, EgclError> {
    let group = SchedulerGroup::init(config)?;
    for &fiber in fibers {
        group.submit(fiber)?;
    }
    group.finish()
}
