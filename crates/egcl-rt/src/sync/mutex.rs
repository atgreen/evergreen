use super::{BlockingMode, FiberWaiter, blocking_mode, timer};
use crate::error::EgclError;
use crate::thread::{NativeThreadId, current_fiber_id, current_thread_id};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MutexOwner {
    Fiber(crate::thread::FiberId),
    Native(NativeThreadId),
}

fn current_owner() -> MutexOwner {
    current_fiber_id()
        .map(MutexOwner::Fiber)
        .unwrap_or_else(|| MutexOwner::Native(current_thread_id()))
}

struct MutexState {
    owner: Option<MutexOwner>,
    depth: usize,
    fiber_waiters: VecDeque<FiberWaiter>,
}

/// Non-recursive by default, fiber-aware mutex.  Native waiters use a condvar;
/// unpinned fibers enqueue a waker and unmount from their carrier.
pub struct EgclMutex {
    name: Option<String>,
    recursive: bool,
    state: Mutex<MutexState>,
    native_waiters: Condvar,
}

impl EgclMutex {
    pub fn new(name: Option<String>, recursive: bool) -> Self {
        Self {
            name,
            recursive,
            state: Mutex::new(MutexState {
                owner: None,
                depth: 0,
                fiber_waiters: VecDeque::new(),
            }),
            native_waiters: Condvar::new(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn grab(&self, wait: bool, timeout: Option<Duration>) -> Result<bool, EgclError> {
        // Only Rust-owned queue/owner state is touched until this scope drops.
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        let owner = current_owner();
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        loop {
            let mut state = self.state.lock().unwrap();
            match state.owner {
                None => {
                    state.owner = Some(owner);
                    state.depth = 1;
                    return Ok(true);
                }
                Some(existing) if existing == owner => {
                    if self.recursive {
                        state.depth += 1;
                        return Ok(true);
                    }
                    return Err(EgclError::ProgramError(
                        "attempt to recursively acquire a non-recursive mutex".into(),
                    ));
                }
                Some(_) if !wait => return Ok(false),
                Some(_) => {}
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(false);
            }

            match blocking_mode("GRAB-MUTEX")? {
                BlockingMode::Native => {
                    state = match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            let (state, wait_result) =
                                self.native_waiters.wait_timeout(state, remaining).unwrap();
                            if wait_result.timed_out() && state.owner.is_some() {
                                return Ok(false);
                            }
                            state
                        }
                        None => self.native_waiters.wait(state).unwrap(),
                    };
                    drop(state);
                }
                BlockingMode::Fiber => {
                    let (fiber, token) = crate::thread::prepare_current_fiber_park(
                        crate::thread::FiberState::Blocked,
                    )?;
                    let waiter = FiberWaiter::new(fiber, token);
                    state.fiber_waiters.push_back(waiter.clone());
                    if let Some(deadline) = deadline {
                        if let Err(error) = timer::schedule(fiber, token, deadline) {
                            state
                                .fiber_waiters
                                .retain(|entry| !(entry.fiber == fiber && entry.token == token));
                            crate::thread::cancel_prepared_current_fiber_park();
                            return Err(error);
                        }
                    }
                    drop(state);
                    crate::thread::park_prepared_current_fiber()?;
                    let mut state = self.state.lock().unwrap();
                    state
                        .fiber_waiters
                        .retain(|entry| !(entry.fiber == fiber && entry.token == token));
                    drop(state);
                }
            }
        }
    }

    pub fn release(&self) -> Result<(), EgclError> {
        self.release_depth(false).map(|_| ())
    }

    /// A condition wait releases every recursive acquisition atomically.
    pub(crate) fn release_for_wait(&self) -> Result<usize, EgclError> {
        self.release_depth(true)
    }

    pub(crate) fn reacquire_after_wait(&self, depth: usize) -> Result<(), EgclError> {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        self.grab(true, None)?;
        self.state.lock().unwrap().depth = depth;
        Ok(())
    }

    fn release_depth(&self, all: bool) -> Result<usize, EgclError> {
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        let owner = current_owner();
        let mut state = self.state.lock().unwrap();
        if state.owner != Some(owner) {
            return Err(EgclError::ProgramError(
                "attempt to release a mutex not owned by the current execution".into(),
            ));
        }
        let depth = state.depth;
        if !all && depth > 1 {
            state.depth -= 1;
            return Ok(1);
        }
        state.owner = None;
        state.depth = 0;
        while let Some(waiter) = state.fiber_waiters.pop_front() {
            if waiter.wake() {
                return Ok(depth);
            }
        }
        self.native_waiters.notify_one();
        Ok(depth)
    }

    pub(crate) fn owned_by_current(&self) -> bool {
        self.state.lock().unwrap().owner == Some(current_owner())
    }
}

impl Default for EgclMutex {
    fn default() -> Self {
        Self::new(None, false)
    }
}
