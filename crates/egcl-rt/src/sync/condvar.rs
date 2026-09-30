// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::{BlockingMode, FiberWaiter, EgclMutex, blocking_mode, timer};
use crate::error::EgclError;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct CondState {
    fiber_waiters: VecDeque<FiberWaiter>,
    native_waiters: VecDeque<Arc<NativeWaiter>>,
}

/// Each native wait has its own wake predicate and condvar. A shared generation
/// cannot distinguish a selected waiter from another wait that merely times out
/// after NOTIFY. The queue lock serializes selection with timeout removal.
struct NativeWaiter {
    notified: AtomicBool,
    wake: Condvar,
}

/// Fiber-aware condition variable paired with [`EgclMutex`].
pub struct EgclCondVar {
    name: Option<String>,
    state: Mutex<CondState>,
}

impl EgclCondVar {
    pub fn new(name: Option<String>) -> Self {
        Self {
            name,
            state: Mutex::new(CondState {
                fiber_waiters: VecDeque::new(),
                native_waiters: VecDeque::new(),
            }),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Atomically enqueue on this wait queue and release `mutex`.  The mutex is
    /// reacquired before returning, including on timeout.
    pub fn wait(&self, mutex: &EgclMutex, timeout: Option<Duration>) -> Result<bool, EgclError> {
        // The native wait and mutex reacquisition use no moving heap values.
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        if !mutex.owned_by_current() {
            return Err(EgclError::ProgramError(
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
                let depth = mutex.release_for_wait()?;
                if let Some(deadline) = deadline {
                    if let Err(error) = timer::schedule(fiber, token, deadline) {
                        state
                            .fiber_waiters
                            .retain(|entry| !(entry.fiber == fiber && entry.token == token));
                        crate::thread::cancel_prepared_current_fiber_park();
                        drop(state);
                        mutex.reacquire_after_wait(depth)?;
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
                mutex.reacquire_after_wait(depth)?;
                Ok(notified)
            }
            BlockingMode::Native => {
                let mut state = self.state.lock().unwrap();
                let waiter = Arc::new(NativeWaiter {
                    notified: AtomicBool::new(false),
                    wake: Condvar::new(),
                });
                let depth = mutex.release_for_wait()?;
                state.native_waiters.push_back(Arc::clone(&waiter));
                let notified = loop {
                    if waiter.notified.load(Ordering::Acquire) {
                        break true;
                    }
                    state = match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            let (new_state, result) =
                                waiter.wake.wait_timeout(state, remaining).unwrap();
                            state = new_state;
                            if result.timed_out() && !waiter.notified.load(Ordering::Acquire) {
                                break false;
                            }
                            continue;
                        }
                        None => waiter.wake.wait(state).unwrap(),
                    };
                };
                state
                    .native_waiters
                    .retain(|entry| !Arc::ptr_eq(entry, &waiter));
                drop(state);
                mutex.reacquire_after_wait(depth)?;
                Ok(notified)
            }
        }
    }

    /// Select at most `count` waiters and return how many were notified. Pending
    /// notifications are not permits: a call with no waiters returns zero.
    pub fn notify(&self, count: usize) -> usize {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        if count == 0 {
            return 0;
        }
        let mut state = self.state.lock().unwrap();
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
        while remaining > 0 {
            let Some(waiter) = state.native_waiters.pop_front() else {
                break;
            };
            waiter.notified.store(true, Ordering::Release);
            waiter.wake.notify_one();
            remaining -= 1;
        }
        count - remaining
    }

    /// Notify every queued waiter, returning the number selected.
    pub fn broadcast(&self) -> usize {
        self.notify(usize::MAX)
    }
}

impl Default for EgclCondVar {
    fn default() -> Self {
        Self::new(None)
    }
}
