// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Fiber-aware synchronization and waiting primitives (spec §13.9).
//!
//! An unpinned managed fiber publishes its roots and unmounts from the carrier.
//! Native threads, and pinned fibers under the configured policy, use the
//! corresponding OS-blocking path.

pub mod atomic;
mod condvar;
mod io;
mod mutex;
mod semaphore;
#[cfg(windows)]
mod socket_windows;
mod timer;

pub use condvar::EgclCondVar;
pub use io::{IoInterest, wait_fd, wait_socket};
pub use mutex::EgclMutex;
pub use semaphore::EgclSemaphore;

use crate::error::EgclError;
use crate::thread::{current_fiber, fiber_yield};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockingMode {
    Fiber,
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PinnedBlockingAction {
    Warn = 1,
    Error = 2,
    Native = 3,
}

static PINNED_BLOCKING_ACTION: AtomicU8 = AtomicU8::new(0);

pub fn set_pinned_blocking_action(action: PinnedBlockingAction) {
    PINNED_BLOCKING_ACTION.store(action as u8, Ordering::Release);
}

fn pinned_blocking_action() -> PinnedBlockingAction {
    if let Some(value) = crate::symbols::find_index("EGCL-FIBER::*PINNED-BLOCKING-ACTION*")
        .or_else(|| crate::symbols::find_index("EGCL-FIBER:*PINNED-BLOCKING-ACTION*"))
        .and_then(crate::symbols::symbol_value)
    {
        if value == crate::value::NIL {
            return PinnedBlockingAction::Native;
        }
        match crate::symbols::symbol_name_of(value).as_deref() {
            Some("KEYWORD:WARN") => return PinnedBlockingAction::Warn,
            Some("KEYWORD:ERROR") => return PinnedBlockingAction::Error,
            _ => {}
        }
    }

    match PINNED_BLOCKING_ACTION.load(Ordering::Acquire) {
        1 => PinnedBlockingAction::Warn,
        2 => PinnedBlockingAction::Error,
        3 => PinnedBlockingAction::Native,
        _ => {
            let action = match std::env::var("EGCL_PINNED_BLOCKING_ACTION")
                .unwrap_or_else(|_| "WARN".into())
                .to_ascii_uppercase()
                .as_str()
            {
                "ERROR" => PinnedBlockingAction::Error,
                "NIL" | "SILENT" | "NATIVE" => PinnedBlockingAction::Native,
                _ => PinnedBlockingAction::Warn,
            };
            PINNED_BLOCKING_ACTION.store(action as u8, Ordering::Release);
            action
        }
    }
}

/// Select cooperative or native blocking for the current execution.
/// This only applies the pinned-blocking policy; native I/O must additionally
/// publish roots with `NativeBlockingScope` for the duration of the operation.
pub fn blocking_mode(operation: &str) -> Result<BlockingMode, EgclError> {
    let Some(fiber) = current_fiber() else {
        return Ok(BlockingMode::Native);
    };
    if fiber.can_yield() {
        return Ok(BlockingMode::Fiber);
    }

    match pinned_blocking_action() {
        PinnedBlockingAction::Error => Err(EgclError::ProgramError(format!(
            "pinned fiber cannot cooperatively block in {operation}"
        ))),
        PinnedBlockingAction::Native => Ok(BlockingMode::Native),
        PinnedBlockingAction::Warn => {
            eprintln!("warning: pinned fiber uses native blocking in {operation}");
            Ok(BlockingMode::Native)
        }
    }
}

/// Cooperatively sleep without occupying a carrier.  Outside a fiber (or in a
/// pinned fiber under the configured fallback policy), this blocks the native
/// thread in the ordinary way.
pub fn fiber_sleep(duration: Duration) -> Result<(), EgclError> {
    if duration.is_zero() {
        if current_fiber().is_some() {
            return fiber_yield();
        }
        std::thread::yield_now();
        return Ok(());
    }
    match blocking_mode("FIBER-SLEEP")? {
        BlockingMode::Native => {
            std::thread::sleep(duration);
            Ok(())
        }
        BlockingMode::Fiber => {
            let (fiber, token) =
                crate::thread::prepare_current_fiber_park(crate::thread::FiberState::Blocked)?;
            if let Err(error) = timer::schedule(fiber, token, std::time::Instant::now() + duration)
            {
                crate::thread::cancel_prepared_current_fiber_park();
                return Err(error);
            }
            crate::thread::park_prepared_current_fiber()
        }
    }
}

#[derive(Clone)]
struct FiberWaiter {
    fiber: crate::thread::FiberId,
    token: u64,
    notified: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl FiberWaiter {
    fn new(fiber: crate::thread::FiberId, token: u64) -> Self {
        Self {
            fiber,
            token,
            notified: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn wake(&self) -> bool {
        self.notified
            .store(true, std::sync::atomic::Ordering::Release);
        crate::thread::wake_fiber_wait(self.fiber, self.token)
    }
}
