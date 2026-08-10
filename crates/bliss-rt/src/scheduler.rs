//! Green-thread scheduler — work-stealing deque per worker.
//!
//! See §2.3.4 of the spec.

use crate::error::BlissError;
use crate::thread::GreenThreadId;

/// Scheduler configuration.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Number of OS worker threads (default: hardware thread count).
    pub num_workers: usize,
}

/// The global scheduler instance.
pub struct Scheduler {
    _private: (),
}

impl Scheduler {
    /// Initialize the scheduler and spawn worker threads.
    pub fn init(config: &SchedulerConfig) -> Result<Self, BlissError> {
        unimplemented!("Scheduler::init")
    }

    /// Submit a runnable green thread to the current worker's deque.
    pub fn submit(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        unimplemented!("Scheduler::submit")
    }

    /// Park the current green thread (transition to Blocked).
    pub fn park_current(&self) {
        unimplemented!("Scheduler::park_current")
    }

    /// Unpark a blocked green thread (transition to Runnable).
    pub fn unpark(&self, thread_id: GreenThreadId) -> Result<(), BlissError> {
        unimplemented!("Scheduler::unpark")
    }

    /// Request preemption of the given green thread at the next safepoint.
    pub fn request_yield(&self, thread_id: GreenThreadId) {
        unimplemented!("Scheduler::request_yield")
    }

    /// Shut down the scheduler: interrupt all green threads, join workers.
    pub fn shutdown(&self) -> Result<(), BlissError> {
        unimplemented!("Scheduler::shutdown")
    }

    /// Return the number of live (non-Dead) green threads.
    pub fn active_thread_count(&self) -> usize {
        unimplemented!("Scheduler::active_thread_count")
    }
}
