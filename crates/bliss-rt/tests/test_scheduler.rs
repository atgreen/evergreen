use bliss_rt::scheduler::*;
use bliss_rt::thread::{
    FiberId, FiberState, all_thread_ids, fiber_state, fiber_yield, make_fiber, park_current_fiber,
    thread_is_carrier,
};
use bliss_rt::value::{BlissVal, T};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static YIELD_TRACE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static PARK_PROGRESS: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_HOG_STARTED: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_HOG_DONE: AtomicUsize = AtomicUsize::new(0);
static PREEMPT_PEER_RAN_EARLY: AtomicUsize = AtomicUsize::new(0);

fn yielding_fiber_a() -> BlissVal {
    YIELD_TRACE.lock().unwrap().push(1);
    fiber_yield().unwrap();
    YIELD_TRACE.lock().unwrap().push(3);
    T
}

fn yielding_fiber_b() -> BlissVal {
    YIELD_TRACE.lock().unwrap().push(2);
    fiber_yield().unwrap();
    YIELD_TRACE.lock().unwrap().push(4);
    T
}

fn parking_fiber() -> BlissVal {
    PARK_PROGRESS.store(1, Ordering::Release);
    park_current_fiber().unwrap();
    PARK_PROGRESS.store(2, Ordering::Release);
    T
}

fn preemption_hog() -> BlissVal {
    PREEMPT_HOG_STARTED.store(1, Ordering::Release);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);
    while std::time::Instant::now() < deadline {
        // A compiler back-edge/allocation poll reaches this same runtime path.
        bliss_rt::poll_safepoint();
        std::hint::spin_loop();
    }
    PREEMPT_HOG_DONE.store(1, Ordering::Release);
    T
}

fn preemption_peer() -> BlissVal {
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
    assert!(SchedulerGroup::init(&SchedulerConfig { num_workers: 0 }).is_err());
}

#[test]
fn scheduler_group_exposes_real_native_carrier_threads() {
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
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    assert!(group.submit(FiberId(u64::MAX)).is_err());
    let fiber = make_fiber(T).unwrap();
    group.submit(fiber).unwrap();
    assert!(group.submit(fiber).is_err());
    assert_eq!(group.finish().unwrap(), vec![T]);
}

#[test]
fn submission_after_shutdown_is_rejected() {
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    group.shutdown().unwrap();
    assert!(group.submit(make_fiber(T).unwrap()).is_err());
}

#[test]
fn many_fibers_complete_over_a_smaller_carrier_set() {
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
    YIELD_TRACE.lock().unwrap().clear();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let a = make_fiber(unsafe {
        BlissVal::from_function_ptr(yielding_fiber_a as *const () as *mut u8)
    })
    .unwrap();
    let b = make_fiber(unsafe {
        BlissVal::from_function_ptr(yielding_fiber_b as *const () as *mut u8)
    })
    .unwrap();
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
    PARK_PROGRESS.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let fiber =
        make_fiber(unsafe { BlissVal::from_function_ptr(parking_fiber as *const () as *mut u8) })
            .unwrap();
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
    PREEMPT_HOG_STARTED.store(0, Ordering::Release);
    PREEMPT_HOG_DONE.store(0, Ordering::Release);
    PREEMPT_PEER_RAN_EARLY.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let hog =
        make_fiber(unsafe { BlissVal::from_function_ptr(preemption_hog as *const () as *mut u8) })
            .unwrap();
    group.submit(hog).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while PREEMPT_HOG_STARTED.load(Ordering::Acquire) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(PREEMPT_HOG_STARTED.load(Ordering::Acquire), 1);

    let peer =
        make_fiber(unsafe { BlissVal::from_function_ptr(preemption_peer as *const () as *mut u8) })
            .unwrap();
    group.submit(peer).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(
        PREEMPT_PEER_RAN_EARLY.load(Ordering::Acquire),
        1,
        "the peer fiber should run before the CPU-bound fiber completes"
    );
}
