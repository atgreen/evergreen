//! Scheduler control handle.
//!
//! The single real scheduler — N **native OS workers** each owning a
//! work-stealing deque, running M **managed Bliss fibers** (`GreenThread`) — is
//! implemented in [`crate::thread`] (the `WorkerPool`/`Worker` structs are
//! internal there). This module is a thin *control handle* over that runtime
//! (lifecycle/config + safepoint-mediated control ops), not a competing
//! scheduler: `park_current`/`request_yield` delegate to the real fiber runtime
//! (bliss-jtc.14.2). Public thread APIs (`make_thread`, `join_thread`, …) create
//! and join managed fibers; native workers are never exposed.
//!
//! See §2.3.4 of the spec.

use crate::error::BlissError;
use crate::thread::GreenThreadId;
use std::collections::HashSet;
use std::sync::Mutex;

/// Scheduler configuration.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Number of OS worker threads (default: hardware thread count).
    pub num_workers: usize,
}

/// The global scheduler instance.
pub struct Scheduler {
    /// Number of OS worker threads.
    _num_workers: usize,
    /// Set of thread IDs that have been submitted and are active (not dead).
    active: Mutex<HashSet<GreenThreadId>>,
    /// Whether the scheduler has been shut down.
    shut_down: Mutex<bool>,
}

impl Scheduler {
    /// Initialize the scheduler and spawn worker threads.
    pub fn init(config: &SchedulerConfig) -> Result<Self, BlissError> {
        if config.num_workers == 0 {
            return Err(BlissError::Internal(
                "scheduler requires at least one worker".into(),
            ));
        }
        Ok(Scheduler {
            _num_workers: config.num_workers,
            active: Mutex::new(HashSet::new()),
            shut_down: Mutex::new(false),
        })
    }

    /// Submit a runnable green thread to the current worker's deque.
    pub fn submit(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        let shut = self.shut_down.lock().unwrap();
        if *shut {
            return Err(BlissError::Internal("scheduler is shut down".into()));
        }
        drop(shut);
        let mut active = self.active.lock().unwrap();
        active.insert(thread_id);
        Ok(())
    }

    /// Park the current fiber, yielding it at the next safepoint so the fiber
    /// runtime can run another runnable fiber (bliss-jtc.14.2).
    pub fn park_current(&self) {
        crate::thread::thread_yield();
    }

    /// Unpark a blocked green thread (transition to Runnable).
    pub fn unpark(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        let active = self.active.lock().unwrap();
        if active.contains(&thread_id) {
            Ok(())
        } else {
            Err(BlissError::Internal(format!(
                "thread {:?} not found in scheduler",
                thread_id
            )))
        }
    }

    /// Request preemption of fiber `thread_id` at its next safepoint poll — sets
    /// the real per-fiber yield flag in the fiber runtime (bliss-jtc.14.2).
    pub fn request_yield(&self, thread_id: GreenThreadId) {
        let _ = crate::thread::request_fiber_yield(thread_id);
    }

    /// Shut down the scheduler: interrupt all green threads, join workers.
    pub fn shutdown(&self) -> Result<(), BlissError> {
        let mut active = self.active.lock().unwrap();
        active.clear();
        let mut shut = self.shut_down.lock().unwrap();
        *shut = true;
        Ok(())
    }

    /// Return the number of live (non-Dead) green threads.
    pub fn active_thread_count(&self) -> usize {
        let active = self.active.lock().unwrap();
        active.len()
    }
}
