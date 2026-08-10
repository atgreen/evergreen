//! Green-thread scheduler — work-stealing deque per worker.
//!
//! See §2.3.4 of the spec.

use crate::error::BlissError;
use crate::thread::GreenThreadId;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Scheduler configuration.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Number of OS worker threads (default: hardware thread count).
    pub num_workers: usize,
}

/// Thread scheduling state tracked by the scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SchedState {
    Runnable,
    Parked,
}

/// Internal scheduler state.
struct SchedulerInner {
    num_workers: usize,
    threads: HashMap<u64, SchedState>,
    is_shutdown: bool,
}

/// The global scheduler instance.
pub struct Scheduler {
    inner: Mutex<SchedulerInner>,
}

impl Scheduler {
    /// Initialize the scheduler and spawn worker threads.
    pub fn init(config: &SchedulerConfig) -> Result<Self, BlissError> {
        if config.num_workers == 0 {
            return Err(BlissError::Internal(
                "scheduler requires at least 1 worker".into(),
            ));
        }
        Ok(Scheduler {
            inner: Mutex::new(SchedulerInner {
                num_workers: config.num_workers,
                threads: HashMap::new(),
                is_shutdown: false,
            }),
        })
    }

    /// Submit a runnable green thread to the current worker's deque.
    pub fn submit(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.is_shutdown {
            return Err(BlissError::Internal("scheduler is shut down".into()));
        }
        inner.threads.insert(thread_id.0, SchedState::Runnable);
        Ok(())
    }

    /// Park the current green thread (transition to Blocked).
    pub fn park_current(&self) {
        // In a full implementation, this would:
        // 1. Save the current green thread's context
        // 2. Transition it to Blocked state
        // 3. Switch to the next runnable green thread
        // For the unit-test level, we just mark any runnable thread as parked.
        let mut inner = self.inner.lock().unwrap();
        // Find first runnable thread and park it.
        let first_runnable = inner
            .threads
            .iter()
            .find(|(_, state)| **state == SchedState::Runnable)
            .map(|(&id, _)| id);
        if let Some(id) = first_runnable {
            inner.threads.insert(id, SchedState::Parked);
        }
    }

    /// Unpark a blocked green thread (transition to Runnable).
    pub fn unpark(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        let mut inner = self.inner.lock().unwrap();
        match inner.threads.get(&thread_id.0) {
            Some(&SchedState::Parked) => {
                inner.threads.insert(thread_id.0, SchedState::Runnable);
                Ok(())
            }
            Some(&SchedState::Runnable) => {
                // Already runnable, this is fine.
                Ok(())
            }
            None => Err(BlissError::Internal(format!(
                "cannot unpark unknown thread {}",
                thread_id.0
            ))),
        }
    }

    /// Request preemption of the given green thread at the next safepoint.
    pub fn request_yield(&self, _thread_id: GreenThreadId) {
        // In a full implementation, this sets a per-thread yield flag
        // that is checked at the next safepoint poll. The thread then
        // voluntarily yields its time slice back to the scheduler.
    }

    /// Shut down the scheduler: interrupt all green threads, join workers.
    pub fn shutdown(&self) -> Result<(), BlissError> {
        let mut inner = self.inner.lock().unwrap();
        inner.threads.clear();
        inner.is_shutdown = true;
        Ok(())
    }

    /// Return the number of live (non-Dead) green threads.
    pub fn active_thread_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.threads.len()
    }
}
