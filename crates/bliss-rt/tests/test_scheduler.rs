//! Tests for bliss-rt scheduler module: SchedulerConfig, Scheduler lifecycle,
//! submit, park/unpark, yield, shutdown, active_thread_count.

use bliss_rt::scheduler::*;
use bliss_rt::thread::GreenThreadId;

// ── SchedulerConfig ───────────────────────────────────────────────

#[test]
fn scheduler_config_clone_debug() {
    let cfg = SchedulerConfig { num_workers: 4 };
    let c2 = cfg.clone();
    assert_eq!(c2.num_workers, 4);
    assert!(format!("{:?}", c2).contains("4"));
}

// ── Scheduler::init ───────────────────────────────────────────────

#[test]
fn scheduler_init_succeeds() {
    assert!(Scheduler::init(&SchedulerConfig { num_workers: 2 }).is_ok());
}

#[test]
fn scheduler_init_single_worker() {
    assert!(Scheduler::init(&SchedulerConfig { num_workers: 1 }).is_ok());
}

// ── submit / unpark / yield / active_thread_count ─────────────────

#[test]
fn submit_valid_thread() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    assert!(s.submit(GreenThreadId(1)).is_ok());
}

#[test]
fn unpark_unknown_thread_errors() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    assert!(s.unpark(GreenThreadId(999_999)).is_err());
}

#[test]
fn request_yield_does_not_panic() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    s.request_yield(GreenThreadId(1)); // should not panic
}

#[test]
fn active_thread_count_starts_zero() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    assert_eq!(s.active_thread_count(), 0);
}

#[test]
fn active_count_increments_after_submit() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    s.submit(GreenThreadId(1)).unwrap();
    assert!(s.active_thread_count() >= 1);
}

// ── shutdown ──────────────────────────────────────────────────────

#[test]
fn shutdown_succeeds_and_clears_threads() {
    let s = Scheduler::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let _ = s.submit(GreenThreadId(1));
    assert!(s.shutdown().is_ok());
    assert_eq!(s.active_thread_count(), 0);
}
