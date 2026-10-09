// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in scheduler fault injection for native continuation integration tests.
//! Only explicitly armed fibers are affected; production capability is unchanged.

use super::current_segment;
use crate::thread::{FiberId, NativeThreadId};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static REFUSALS: AtomicUsize = AtomicUsize::new(0);
static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
static REFUSE_FIBERS: Mutex<Vec<crate::thread::FiberId>> = Mutex::new(Vec::new());
static REJECTED: Mutex<Option<(FiberId, NativeThreadId, NativeThreadId)>> = Mutex::new(None);

// Simulate a destination whose hardening policy rejects native stack transfer.
// Restrict injection to this test's fibers; no environment or global platform
// policy is changed, and another test's native entries are unaffected.
pub(crate) fn destination_supported(actual: bool) -> bool {
    let Some(fiber) = crate::thread::current_fiber_id() else {
        return actual;
    };
    let mut fibers = REFUSE_FIBERS.lock().unwrap();
    if !actual || !fibers.contains(&fiber) {
        return actual;
    }
    fibers.clear();
    REFUSALS.fetch_add(1, Ordering::Relaxed);
    let carrier = unsafe { (*current_segment()).carrier() };
    *REJECTED.lock().unwrap() = Some((fiber, carrier, crate::thread::current_thread_id()));
    false
}

pub(crate) fn before_requeue(fiber: crate::thread::FiberId) {
    let rejected = REJECTED
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(id, _, _)| *id == fiber);
    if rejected && crate::gc::collect_t0_minor().is_ok() {
        COLLECTIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arm a cohort: the first incompatible-carrier probe refuses exactly one fiber.
/// Call after the preceding cohort has finished; arming clears prior observations.
pub fn arm(fibers: Vec<FiberId>) {
    assert!(REJECTED.lock().unwrap().is_none());
    REFUSALS.store(0, Ordering::Relaxed);
    COLLECTIONS.store(0, Ordering::Relaxed);
    *REFUSE_FIBERS.lock().unwrap() = fibers;
}

/// Consume this fiber's refusal, returning its required and rejected carriers.
/// Called by the resumed continuation, never by the scheduler.
pub fn take_rejection(fiber: FiberId) -> Option<(NativeThreadId, NativeThreadId)> {
    let mut rejected = REJECTED.lock().unwrap();
    if rejected.as_ref().is_some_and(|(id, _, _)| *id == fiber) {
        rejected
            .take()
            .map(|(_, required, refused)| (required, refused))
    } else {
        None
    }
}

/// Refusals, collections before requeue, and whether a refusal awaits resumption.
pub fn observations() -> (usize, usize, bool) {
    (
        REFUSALS.load(Ordering::Relaxed),
        COLLECTIONS.load(Ordering::Relaxed),
        REJECTED.lock().unwrap().is_some(),
    )
}

/// Stop injection after all cohort fibers have finished.
pub fn disarm() {
    REFUSE_FIBERS.lock().unwrap().clear();
}
