use super::{BlockingMode, FiberWaiter, TorclMutex, blocking_mode, timer};
use crate::error::TorclError;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

struct CondState {
    generation: u64,
    fiber_waiters: VecDeque<FiberWaiter>,
}

/// Fiber-aware condition variable paired with [`TorclMutex`].
pub struct TorclCondVar {
    name: Option<String>,
    state: Mutex<CondState>,
    native_waiters: Condvar,
}

impl TorclCondVar {
    pub fn new(name: Option<String>) -> Self {
        Self {
            name,
            state: Mutex::new(CondState {
                generation: 0,
                fiber_waiters: VecDeque::new(),
            }),
            native_waiters: Condvar::new(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Atomically enqueue on this wait queue and release `mutex`.  The mutex is
    /// reacquired before returning, including on timeout.
    pub fn wait(&self, mutex: &TorclMutex, timeout: Option<Duration>) -> Result<bool, TorclError> {
        if !mutex.owned_by_current() {
            return Err(TorclError::ProgramError(
                "CONDITION-WAIT requires the current execution to own the mutex".into(),
            ));
        }
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        match blocking_mode("CONDITION-WAIT")? {
            BlockingMode::Fiber => {
                let mut state = self.state.lock().unwrap();
                let (fiber, token) =
                    crate::thread::prepare_current_fiber_park(crate::thread::FiberState::Blocked)?;
                let waiter = FiberWaiter::new(fiber, token);
                state.fiber_waiters.push_back(waiter.clone());
                mutex.release()?;
                if let Some(deadline) = deadline {
                    if let Err(error) = timer::schedule(fiber, token, deadline) {
                        state
                            .fiber_waiters
                            .retain(|entry| !(entry.fiber == fiber && entry.token == token));
                        crate::thread::cancel_prepared_current_fiber_park();
                        drop(state);
                        mutex.grab(true, None)?;
                        return Err(error);
                    }
                }
                drop(state);
                crate::thread::park_prepared_current_fiber()?;
                let notified = waiter.notified.load(Ordering::Acquire);
                let mut state = self.state.lock().unwrap();
                state
                    .fiber_waiters
                    .retain(|entry| !(entry.fiber == fiber && entry.token == token));
                drop(state);
                mutex.grab(true, None)?;
                Ok(notified)
            }
            BlockingMode::Native => {
                let mut state = self.state.lock().unwrap();
                let generation = state.generation;
                mutex.release()?;
                let notified = loop {
                    if state.generation != generation {
                        break true;
                    }
                    state = match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            let (new_state, result) =
                                self.native_waiters.wait_timeout(state, remaining).unwrap();
                            state = new_state;
                            if result.timed_out() && state.generation == generation {
                                break false;
                            }
                            continue;
                        }
                        None => self.native_waiters.wait(state).unwrap(),
                    };
                };
                drop(state);
                mutex.grab(true, None)?;
                Ok(notified)
            }
        }
    }

    pub fn notify(&self, count: usize) {
        if count == 0 {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        let mut remaining = count;
        while remaining > 0 {
            match state.fiber_waiters.pop_front() {
                Some(waiter) => {
                    if waiter.wake() {
                        remaining -= 1;
                    }
                }
                None => break,
            }
        }
        for _ in 0..remaining {
            self.native_waiters.notify_one();
        }
    }

    pub fn broadcast(&self) {
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        for waiter in state.fiber_waiters.drain(..) {
            waiter.wake();
        }
        self.native_waiters.notify_all();
    }
}

impl Default for TorclCondVar {
    fn default() -> Self {
        Self::new(None)
    }
}
