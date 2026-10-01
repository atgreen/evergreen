// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::scheduler::*;
use egcl_rt::thread::{
    FiberId, FiberState, all_thread_ids, fiber_state, fiber_yield, make_fiber, park_current_fiber,
    thread_is_carrier,
};
use egcl_rt::value::{EgclVal, T};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Serializes the tests that stand up a real scheduler group (bliss-z11t).
///
/// Each of these creates carrier OS threads and drives fibers, and both the
/// safepoint handshake and the thread registry are per PROCESS. Run
/// concurrently they perturb each other: `time_slice_expiry_...` requests
/// preemption process-wide, carriers from one test compete for CPU with
/// another's, and `scheduler_group_exposes_real_native_carrier_threads`
/// observes `all_thread_ids()` including the other test's carriers. The
/// observed failure was `yield_unmounts_and_later_resumes_after_the_call_site`
/// asserting a yield ORDER of [1,2,3,4], which another test's carriers can
/// reorder by delaying a resume.
///
/// Poisoning is recovered rather than propagated so one panic does not cascade.
fn scheduler_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

macro_rules! serialize_scheduler {
    () => {
        let _scheduler_guard = scheduler_lock().lock().unwrap_or_else(|e| e.into_inner());
    };
}

static YIELD_TRACE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static PARK_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_HOG_STARTED: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_HOG_DONE: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_PEER_RAN_EARLY: AtomicUsize = AtomicUsize::new(0);

fn yielding_fiber_a() -> EgclVal {
    YIELD_TRACE.lock().unwrap().push(1);
    fiber_yield().unwrap();
    YIELD_TRACE.lock().unwrap().push(3);
    T
}

fn yielding_fiber_b() -> EgclVal {
    YIELD_TRACE.lock().unwrap().push(2);
    fiber_yield().unwrap();
    YIELD_TRACE.lock().unwrap().push(4);
    T
}

fn parking_fiber() -> EgclVal {
    PARK_PROGRESS.store(1, Ordering::Release);
    park_current_fiber().unwrap();
    PARK_PROGRESS.store(2, Ordering::Release);
    T
}

fn preemption_hog() -> EgclVal {
    PREEMPT_HOG_STARTED.store(1, Ordering::Release);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);
    while std::time::Instant::now() < deadline {
        // A compiler back-edge/allocation poll reaches this same runtime path.
        egcl_rt::poll_safepoint();
        std::hint::spin_loop();
    }
    PREEMPT_HOG_DONE.store(1, Ordering::Release);
    T
}

fn preemption_peer() -> EgclVal {
    if PREEMPT_HOG_DONE.load(Ordering::Acquire) == 0 {
        PREEMPT_PEER_RAN_EARLY.store(1, Ordering::Release);
    }
    T
}

#[test]
fn scheduler_config_clone_debug() {
    let cfg = SchedulerConfig { num_workers: 4 };
    assert_eq!(cfg.clone().num_workers, 4);
    assert!(format!("{cfg:?}").contains('4'));
}

#[test]
fn scheduler_group_requires_at_least_one_carrier() {
    serialize_scheduler!();
    assert!(SchedulerGroup::init(&SchedulerConfig { num_workers: 0 }).is_err());
}

#[test]
fn scheduler_group_exposes_real_native_carrier_threads() {
    serialize_scheduler!();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 3 }).unwrap();
    assert_eq!(group.requested_carrier_count(), 3);
    assert_eq!(group.carrier_thread_ids().len(), 3);
    let all_threads = all_thread_ids();
    assert!(
        group
            .carrier_thread_ids()
            .iter()
            .all(|id| all_threads.contains(id))
    );
    assert!(
        group
            .carrier_thread_ids()
            .iter()
            .all(|&id| thread_is_carrier(id) == Some(true))
    );
    group.shutdown().unwrap();
}

#[test]
fn submit_runs_a_created_fiber_and_finish_preserves_order() {
    serialize_scheduler!();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let first = make_fiber(T).unwrap();
    let second = make_fiber(T).unwrap();
    group.submit(first).unwrap();
    group.submit(second).unwrap();
    assert_ne!(fiber_state(first), Some(FiberState::Created));
    assert_eq!(group.finish().unwrap(), vec![T, T]);
}

#[test]
fn scheduler_rejects_unknown_or_duplicate_fibers() {
    serialize_scheduler!();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    assert!(group.submit(FiberId(u64::MAX)).is_err());
    let fiber = make_fiber(T).unwrap();
    group.submit(fiber).unwrap();
    assert!(group.submit(fiber).is_err());
    assert_eq!(group.finish().unwrap(), vec![T]);
}

#[test]
fn submission_after_shutdown_is_rejected() {
    serialize_scheduler!();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    group.shutdown().unwrap();
    assert!(group.submit(make_fiber(T).unwrap()).is_err());
}

#[test]
fn many_fibers_complete_over_a_smaller_carrier_set() {
    serialize_scheduler!();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let fibers: Vec<_> = (0..200).map(|_| make_fiber(T).unwrap()).collect();
    for &fiber in &fibers {
        group.submit(fiber).unwrap();
    }
    let values = group.finish().unwrap();
    assert_eq!(values.len(), 200);
    assert!(values.iter().all(|&value| value == T));
}

#[test]
fn yield_unmounts_and_later_resumes_after_the_call_site() {
    serialize_scheduler!();
    YIELD_TRACE.lock().unwrap().clear();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let a = make_fiber(native_entry::entry(yielding_fiber_a)).unwrap();
    let b = make_fiber(native_entry::entry(yielding_fiber_b)).unwrap();
    group.submit(a).unwrap();
    group.submit(b).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    let trace = YIELD_TRACE.lock().unwrap().clone();
    assert_eq!(trace.len(), 4);
    assert!(trace.iter().position(|&v| v == 1) < trace.iter().position(|&v| v == 3));
    assert!(trace.iter().position(|&v| v == 2) < trace.iter().position(|&v| v == 4));
    assert_ne!(trace[0] % 2, trace[1] % 2, "another fiber ran after yield");
}

#[test]
fn parked_fiber_unmounts_until_explicit_unpark() {
    serialize_scheduler!();
    PARK_PROGRESS.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let fiber = make_fiber(native_entry::entry(parking_fiber)).unwrap();
    group.submit(fiber).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while fiber_state(fiber) != Some(FiberState::Blocked) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(PARK_PROGRESS.load(Ordering::Acquire), 1);
    assert_eq!(fiber_state(fiber), Some(FiberState::Blocked));
    group.unpark(fiber).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T]);
    assert_eq!(PARK_PROGRESS.load(Ordering::Acquire), 2);
}

#[test]
fn time_slice_expiry_preempts_at_safepoints_on_one_carrier() {
    serialize_scheduler!();
    PREEMPT_HOG_STARTED.store(0, Ordering::Release);
    PREEMPT_HOG_DONE.store(0, Ordering::Release);
    PREEMPT_PEER_RAN_EARLY.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let hog = make_fiber(native_entry::entry(preemption_hog)).unwrap();
    group.submit(hog).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while PREEMPT_HOG_STARTED.load(Ordering::Acquire) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(PREEMPT_HOG_STARTED.load(Ordering::Acquire), 1);

    let peer = make_fiber(native_entry::entry(preemption_peer)).unwrap();
    group.submit(peer).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(
        PREEMPT_PEER_RAN_EARLY.load(Ordering::Acquire),
        1,
        "the peer fiber should run before the CPU-bound fiber completes"
    );
}

#[path = "support/native_entry.rs"]
mod native_entry;
