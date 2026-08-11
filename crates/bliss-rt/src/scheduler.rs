//! Green-thread scheduler — work-stealing deque per worker.
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

    /// Park the current green thread (transition to Blocked).
    pub fn park_current(&self) {
        // In a full implementation this would remove the current green
        // thread from the run queue and switch to the next runnable
        // thread. In the bootstrap runtime we simply record the
        // transition — there is only one OS thread running.
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

    /// Request preemption of the given green thread at the next safepoint.
    pub fn request_yield(&self, _thread_id: GreenThreadId) {
        // Sets a per-thread yield flag that will be checked at the next
        // safepoint poll. In the bootstrap runtime this is a no-op since
        // safepoint polling is cooperative.
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
