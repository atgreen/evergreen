use super::{blocking_mode, timer, BlockingMode, FiberWaiter};
use crate::error::BlissError;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Counting semaphore whose waits unmount unpinned fibers.
pub struct BlissSemaphore {
    name: Option<String>,
    count: AtomicI64,
    fiber_waiters: Mutex<VecDeque<FiberWaiter>>,
    native_waiters: Condvar,
}

impl BlissSemaphore {
    pub fn new(name: Option<String>, count: i64) -> Result<Self, BlissError> {
        if count < 0 {
            return Err(BlissError::ProgramError(
                "semaphore count must be non-negative".into(),
            ));
        }
        Ok(Self {
            name,
            count: AtomicI64::new(count),
            fiber_waiters: Mutex::new(VecDeque::new()),
            native_waiters: Condvar::new(),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn count(&self) -> i64 {
        self.count.load(Ordering::Acquire)
    }

    pub fn try_wait(&self, permits: i64) -> Result<bool, BlissError> {
        if permits <= 0 {
            return Err(BlissError::ProgramError(
                "semaphore wait count must be positive".into(),
            ));
        }
        Ok(self
            .count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count >= permits).then_some(count - permits)
            })
            .is_ok())
    }

    pub fn wait(&self, timeout: Option<Duration>) -> Result<bool, BlissError> {
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        loop {
            if self.try_wait(1)? {
                return Ok(true);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(false);
            }

            let mut waiters = self.fiber_waiters.lock().unwrap();
            // Recheck while holding the wait-queue lock so SIGNAL cannot add a
            // permit between the failed check and enqueue.
            if self.try_wait(1)? {
                return Ok(true);
            }
            match blocking_mode("WAIT-ON-SEMAPHORE")? {
                BlockingMode::Fiber => {
                    let (fiber, token) = crate::thread::prepare_current_fiber_park(
                        crate::thread::FiberState::Blocked,
                    )?;
                    let waiter = FiberWaiter::new(fiber, token);
                    waiters.push_back(waiter.clone());
                    if let Some(deadline) = deadline {
                        if let Err(error) = timer::schedule(fiber, token, deadline) {
                            waiters.retain(|entry| !(entry.fiber == fiber && entry.token == token));
                            crate::thread::cancel_prepared_current_fiber_park();
                            return Err(error);
                        }
                    }
                    drop(waiters);
                    crate::thread::park_prepared_current_fiber()?;
                    if waiter.notified.load(Ordering::Acquire) {
                        return Ok(true);
                    }
                    let mut waiters = self.fiber_waiters.lock().unwrap();
                    waiters.retain(|entry| !(entry.fiber == fiber && entry.token == token));
                    drop(waiters);
                }
                BlockingMode::Native => {
                    waiters = match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            let (waiters, result) = self
                                .native_waiters
                                .wait_timeout(waiters, remaining)
                                .unwrap();
                            if result.timed_out() && self.count() == 0 {
                                return Ok(false);
                            }
                            waiters
                        }
                        None => self.native_waiters.wait(waiters).unwrap(),
                    };
                    drop(waiters);
                }
            }
        }
    }

    pub fn signal(&self, permits: i64) -> Result<(), BlissError> {
        if permits <= 0 {
            return Err(BlissError::ProgramError(
                "semaphore signal count must be positive".into(),
            ));
        }
        self.count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(permits)
            })
            .map_err(|_| BlissError::ProgramError("semaphore count overflow".into()))?;

        let mut waiters = self.fiber_waiters.lock().unwrap();
        let mut remaining = permits;
        while remaining > 0 {
            match waiters.pop_front() {
                Some(waiter) => {
                    if waiter.wake() {
                        self.count.fetch_sub(1, Ordering::AcqRel);
                        remaining -= 1;
                    }
                }
                None => break,
            }
        }
        for _ in 0..remaining {
            self.native_waiters.notify_one();
        }
        Ok(())
    }
}
