// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
static REFUSALS: AtomicUsize = AtomicUsize::new(0);
static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
static REFUSE_FIBERS: Mutex<Vec<crate::thread::FiberId>> = Mutex::new(Vec::new());
static REJECTED: Mutex<Option<(crate::thread::FiberId, crate::thread::NativeThreadId)>> =
    Mutex::new(None);

// Simulate a destination whose hardening policy rejects native stack transfer.
// Restrict injection to this test's fibers; no environment or global platform
// policy is changed, and another test's native entries are unaffected.
pub(super) fn destination_supported(actual: bool) -> bool {
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
    *REJECTED.lock().unwrap() = Some((fiber, carrier));
    false
}

pub(crate) fn before_requeue(fiber: crate::thread::FiberId) {
    let rejected = REJECTED
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(id, _)| *id == fiber);
    if rejected && crate::gc::collect_t0_minor().is_ok() {
        COLLECTIONS.fetch_add(1, Ordering::Relaxed);
    }
}

// These assembly entries call Rust helpers normally. The inner entry transfers
// only AFTER its helper has returned, proving the migrated landing state as
// well as observing ownership before a later poll could repair stale anchors.
#[unsafe(naked)]
unsafe extern "C" fn outer_entry() -> u64 {
    core::arch::naked_asm!("endbr64", "jmp {helper}", helper = sym outer_helper);
}

#[unsafe(naked)]
unsafe extern "C" fn inner_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64", "push rdx", "call {helper}", "pop rdi",
        "mov rsi, rax", "mov edx, 1", "jmp {leave}",
        helper = sym inner_helper, leave = sym leave_native_segment,
    );
}

extern "C" fn inner_helper() -> u64 {
    let inner = current_segment();
    let outer = unsafe { (*inner).previous() };
    if outer.is_null() {
        return EgclVal::from_fixnum(2).0;
    }
    let Some(body) = crate::gc::alloc_typed(8, crate::object::type_id::DOUBLE_FLOAT) else {
        return EgclVal::from_fixnum(256).0;
    };
    unsafe {
        *(body as *mut f64) = 42.0;
    }
    crate::rooted!(value = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
    let mut failures = 0;
    for _ in 0..200 {
        let before = crate::thread::current_thread_id();
        let before_value = value.to_raw();
        if crate::thread::fiber_yield().is_err() {
            return EgclVal::from_fixnum(4).0;
        }
        let after = crate::thread::current_thread_id();
        let mut rejected = REJECTED.lock().unwrap();
        if let Some((fiber, required_carrier)) = *rejected
            && Some(fiber) == crate::thread::current_fiber_id()
        {
            if after != required_carrier {
                failures |= 32;
            }
            if value.to_raw() == before_value {
                failures |= 64;
            }
            *rejected = None;
        }
        drop(rejected);
        if unsafe { *(value.as_ptr().add(8) as *const f64) } != 42.0 {
            failures |= 128;
        }
        if before != after {
            MIGRATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // Both anchors must be ready BEFORE the helper returns to generated
        // code, including the outer segment suspended below this Rust helper.
        if current_segment() != inner
            || unsafe { (*inner).carrier() != after || (*outer).carrier() != after }
        {
            failures |= 1;
        }
    }
    EgclVal::from_fixnum(failures).0
}

extern "C" fn outer_helper() -> u64 {
    let outer = current_segment();
    let stack = crate::current_stack();
    let outcome =
        unsafe { invoke_native_segment(inner_entry as *const u8, std::ptr::null_mut(), stack) };
    match outcome {
        Ok(outcome) if outcome.exit == NativeExit::Transfer && current_segment() == outer => {
            outcome.value.0
        }
        _ => EgclVal::from_fixnum(8).0,
    }
}

fn migrating_fiber() -> EgclVal {
    let stack = crate::current_stack();
    let fp = stack.fp();
    let sp = stack.sp();
    let outcome =
        unsafe { invoke_native_segment(outer_entry as *const u8, std::ptr::null_mut(), stack) };
    match outcome {
        Ok(outcome)
            if outcome.exit == NativeExit::Returned
                && current_segment().is_null()
                && stack.fp() == fp
                && stack.sp() == sp =>
        {
            outcome.value
        }
        _ => EgclVal::from_fixnum(16),
    }
}

#[test]
fn scheduler_revalidates_all_native_segments_before_resuming_a_fiber() {
    crate::gc::ensure_heap_initialized();
    crate::thread::current_thread_id();
    assert!(
        is_supported(),
        "requires the supported Linux segment transition"
    );
    for (num_workers, refuse) in [(1, false), (4, false), (4, true)] {
        MIGRATIONS.store(0, Ordering::Relaxed);
        REFUSALS.store(0, Ordering::Relaxed);
        COLLECTIONS.store(0, Ordering::Relaxed);
        let group = crate::SchedulerGroup::init(&crate::SchedulerConfig { num_workers }).unwrap();
        let mut fibers = Vec::new();
        for _ in 0..12 {
            let entry =
                unsafe { EgclVal::from_function_ptr(migrating_fiber as *const () as *mut u8) };
            fibers.push(crate::thread::make_fiber(entry).unwrap());
        }
        if refuse {
            *REFUSE_FIBERS.lock().unwrap() = fibers.clone();
        }
        for fiber in fibers {
            group.submit(fiber).unwrap();
        }
        let results = group.finish().unwrap();
        if num_workers == 4 {
            assert!(
                MIGRATIONS.load(Ordering::Relaxed) > 0,
                "probe must actually migrate"
            );
        }
        assert_eq!(
            results,
            vec![EgclVal::from_fixnum(0); 12],
            "every resumed segment must already belong to its current carrier"
        );
        assert_eq!(REFUSALS.load(Ordering::Relaxed), usize::from(refuse));
        assert_eq!(COLLECTIONS.load(Ordering::Relaxed), usize::from(refuse));
        assert!(
            REJECTED.lock().unwrap().is_none(),
            "refused fiber must resume exactly once"
        );
    }
}

#[test]
fn rejected_destination_does_not_partially_update_nested_anchors() {
    let current = crate::thread::current_thread_id();
    let previous_carrier = crate::thread::NativeThreadId(current.0 + 1000);
    let make = |previous| NativeSegment {
        saved_sp: 0,
        landing_pc: 0,
        previous,
        owner: SegmentOwner::Thread(current),
        carrier: previous_carrier,
        stack_sp: std::ptr::null(),
        stack_fp: std::ptr::null(),
        _pinned: std::marker::PhantomPinned,
    };
    let mut outer = std::pin::pin!(make(std::ptr::null_mut()));
    let outer = unsafe { outer.as_mut().get_unchecked_mut() as *mut NativeSegment };
    let mut inner = std::pin::pin!(make(outer));
    let inner = unsafe { inner.as_mut().get_unchecked_mut() as *mut NativeSegment };
    let old = ACTIVE.with(|active| active.replace(inner));
    let _restore = ActiveSegment(old);
    assert_eq!(prepare_carrier_resume_with(|| false), Err(previous_carrier));
    assert_eq!(unsafe { (*inner).carrier }, previous_carrier);
    assert_eq!(unsafe { (*outer).carrier }, previous_carrier);
    assert_eq!(prepare_carrier_resume_with(|| true), Ok(()));
    assert_eq!(unsafe { (*inner).carrier }, current);
    assert_eq!(unsafe { (*outer).carrier }, current);
    assert_eq!(
        prepare_carrier_resume_with(|| panic!("unchanged carrier must not query")),
        Ok(())
    );
}
