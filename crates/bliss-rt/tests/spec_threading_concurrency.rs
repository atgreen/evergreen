use bliss_rt::error::BlissError;
use bliss_rt::safepoint::{enter_safepoint, poll_safepoint, resume_all_threads, wait_for_all_threads};
use bliss_rt::scheduler::{Scheduler, SchedulerConfig};
use bliss_rt::thread::{
    GreenThreadId, all_thread_ids, current_thread, current_thread_id, interrupt_thread, join_thread,
    make_thread,
};
use bliss_rt::value::{BlissVal, T};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

static SLOW_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static RELEASE_SLOW_THREAD: AtomicBool = AtomicBool::new(false);
static SAFETY_LOOP_EXIT: AtomicBool = AtomicBool::new(false);
static POLL_ITERATIONS: AtomicUsize = AtomicUsize::new(0);

fn value_returning_entry() -> BlissVal {
    BlissVal::from_fixnum(1234)
}

fn tls_isolated_entry() -> BlissVal {
    let thread = current_thread();
    thread.tls_set(0, BlissVal::from_fixnum(99));
    thread.tls_get(0)
}

fn slow_interruptible_entry() -> BlissVal {
    SLOW_THREAD_STARTED.store(true, Ordering::Release);
    while !RELEASE_SLOW_THREAD.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    T
}

fn polling_entry() -> BlissVal {
    while !SAFETY_LOOP_EXIT.load(Ordering::Acquire) {
        POLL_ITERATIONS.fetch_add(1, Ordering::AcqRel);
        poll_safepoint();
        std::thread::yield_now();
    }
    BlissVal::from_fixnum(POLL_ITERATIONS.load(Ordering::Acquire) as i64)
}

unsafe fn fn_entry(function: fn() -> BlissVal) -> BlissVal {
    unsafe { BlissVal::from_function_ptr(function as usize as *mut u8) }
}

#[test]
fn make_thread_join_and_registry_cleanup_follow_the_public_thread_api() {
    // Per R2.04 and R13.18, user-visible green threads run via the public make/join entrypoints.
    let before = all_thread_ids();
    let id = make_thread(T).expect("thread creation must succeed");
    assert!(all_thread_ids().contains(&id));

    let result = join_thread(id).expect("join must return the thread result");
    assert_eq!(result, T);
    assert!(!all_thread_ids().contains(&id));
    assert!(all_thread_ids().len() <= before.len() + 1);
}

#[test]
fn function_entries_execute_on_worker_threads_and_return_values() {
    // Per R2.04, green threads are scheduled onto the worker pool.
    // Per R13.18, MAKE-THREAD and JOIN-THREAD expose observable thread execution.
    let id = make_thread(unsafe { fn_entry(value_returning_entry) }).expect("thread creation");
    let value = join_thread(id).expect("join must succeed");
    assert_eq!(value, BlissVal::from_fixnum(1234));
}

#[test]
fn invalid_thread_entry_surfaces_a_result_error_not_a_panic() {
    // Per R2.18, runtime failures propagate through Result rather than panicking.
    let id = make_thread(BlissVal::from_fixnum(17)).expect("thread creation");
    let err = join_thread(id).expect_err("non-function, non-special thread entry must fail");
    assert!(matches!(err, BlissError::TypeError { expected, .. } if expected == "function"));
}

#[test]
fn thread_local_storage_is_isolated_between_green_threads() {
    // Per R2.05, each green thread has its own control/value stack and thread-local state.
    // Per R13.17, bindings in one thread must not affect another thread.
    current_thread().tls_set(0, BlissVal::from_fixnum(7));
    let id = make_thread(unsafe { fn_entry(tls_isolated_entry) }).expect("thread creation");
    let child_value = join_thread(id).expect("join must succeed");

    assert_eq!(child_value, BlissVal::from_fixnum(99));
    assert_eq!(current_thread().tls_get(0), BlissVal::from_fixnum(7));
}

#[test]
fn interrupt_delivery_changes_the_join_result_of_a_live_thread() {
    // Per R9.04 and R13.18, INTERRUPT-THREAD is delivered to the target thread.
    SLOW_THREAD_STARTED.store(false, Ordering::Release);
    RELEASE_SLOW_THREAD.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(slow_interruptible_entry) }).expect("thread creation");
    while !SLOW_THREAD_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    interrupt_thread(id, BlissVal::from_fixnum(444)).expect("interrupt must be accepted");
    RELEASE_SLOW_THREAD.store(true, Ordering::Release);

    let result = join_thread(id).expect("join must succeed");
    assert_eq!(result, BlissVal::from_fixnum(444));
}

#[test]
fn all_threads_reports_a_live_thread_until_join_completes() {
    // Per R9.04 and R13.18, the runtime exposes the set of live thread handles.
    SLOW_THREAD_STARTED.store(false, Ordering::Release);
    RELEASE_SLOW_THREAD.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(slow_interruptible_entry) }).expect("thread creation");
    while !SLOW_THREAD_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    assert!(all_thread_ids().contains(&id));
    RELEASE_SLOW_THREAD.store(true, Ordering::Release);
    assert_eq!(join_thread(id).expect("join must succeed"), T);
    assert!(!all_thread_ids().contains(&id));
}

#[test]
fn scheduler_configuration_and_shutdown_are_observable_through_public_api() {
    // Per R2.03, the runtime supports a configurable worker-thread pool.
    // Per R13.08, scheduling is exposed through the runtime scheduler surface.
    let scheduler = Scheduler::init(&SchedulerConfig { num_workers: 3 })
        .expect("scheduler with explicit worker count must initialize");
    let thread_id = GreenThreadId(0xCAFE);
    scheduler.submit(thread_id).expect("submit must succeed");
    assert_eq!(scheduler.active_thread_count(), 1);
    scheduler.shutdown().expect("shutdown must succeed");
    assert_eq!(scheduler.active_thread_count(), 0);
    assert!(scheduler.submit(thread_id).is_err());
}

#[test]
fn enter_safepoint_publishes_the_current_stack_top() {
    // Per R2.07 and R13.10, safepoints use cooperative polling rather than async suspension.
    let stack = current_thread().stack();
    assert_eq!(stack.published_sp(), 0);
    assert!(stack.published_fp().is_null());

    enter_safepoint();

    assert_eq!(stack.published_sp(), stack.used());
    assert_eq!(stack.published_fp(), stack.fp());
}

#[test]
fn stop_the_world_waits_for_polling_threads_and_resumes_them() {
    // Per R2.07, safepoint polls must be observed by running threads.
    // Per R13.10, stop-the-world uses the safepoint handshake rather than async suspension.
    SAFETY_LOOP_EXIT.store(false, Ordering::Release);
    POLL_ITERATIONS.store(0, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(polling_entry) }).expect("thread creation");
    while POLL_ITERATIONS.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }

    wait_for_all_threads().expect("safepoint rendezvous must succeed");
    let iterations_during_stop = POLL_ITERATIONS.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        POLL_ITERATIONS.load(Ordering::Acquire),
        iterations_during_stop,
        "the polling thread should be parked while the world is stopped"
    );

    resume_all_threads().expect("resume must succeed");
    while POLL_ITERATIONS.load(Ordering::Acquire) == iterations_during_stop {
        std::thread::yield_now();
    }

    SAFETY_LOOP_EXIT.store(true, Ordering::Release);
    let result = join_thread(id).expect("polling thread must finish");
    assert!(result.as_fixnum() >= iterations_during_stop as i64);
}

#[test]
fn current_thread_identity_is_stable_within_the_calling_thread() {
    // Per R13.18, CURRENT-THREAD is a stable handle for the running thread.
    let thread = current_thread();
    assert_eq!(thread.id(), current_thread_id());
    assert_eq!(thread.id(), current_thread_id());
}
