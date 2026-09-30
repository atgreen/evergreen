// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#[cfg(unix)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
#[cfg(unix)]
use egcl_rt::sync::{IoInterest, wait_fd};
use egcl_rt::sync::{
    PinnedBlockingAction, EgclCondVar, EgclMutex, EgclSemaphore, fiber_sleep,
    set_pinned_blocking_action,
};
use egcl_rt::thread::{FiberState, current_fiber, fiber_state, make_fiber};
use egcl_rt::value::{T, EgclVal};
use egcl_rt::{SchedulerConfig, SchedulerGroup};

fn serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn function(entry: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(entry as *const () as *mut u8) }
}

fn wait_until(predicate: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !predicate() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(predicate(), "timed out waiting for fiber test state");
}

static MUTEX: OnceLock<EgclMutex> = OnceLock::new();
static MUTEX_PROGRESS: AtomicUsize = AtomicUsize::new(0);

fn mutex_holder() -> EgclVal {
    let mutex = MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    MUTEX_PROGRESS.store(1, Ordering::Release);
    fiber_sleep(Duration::from_millis(20)).unwrap();
    mutex.release().unwrap();
    T
}

fn mutex_waiter() -> EgclVal {
    let mutex = MUTEX.get().unwrap();
    mutex.grab(true, Some(Duration::from_secs(1))).unwrap();
    MUTEX_PROGRESS.store(2, Ordering::Release);
    mutex.release().unwrap();
    T
}

#[test]
fn contended_mutex_parks_fiber_without_blocking_its_only_carrier() {
    let _guard = serial_lock().lock().unwrap();
    MUTEX.get_or_init(EgclMutex::default);
    MUTEX_PROGRESS.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let holder = make_fiber(function(mutex_holder)).unwrap();
    group.submit(holder).unwrap();
    wait_until(|| MUTEX_PROGRESS.load(Ordering::Acquire) == 1);
    let waiter = make_fiber(function(mutex_waiter)).unwrap();
    group.submit(waiter).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(MUTEX_PROGRESS.load(Ordering::Acquire), 2);
}

static COND_MUTEX: OnceLock<EgclMutex> = OnceLock::new();
static CONDVAR: OnceLock<EgclCondVar> = OnceLock::new();
static COND_READY: AtomicUsize = AtomicUsize::new(0);
static COND_DONE: AtomicUsize = AtomicUsize::new(0);

fn condition_waiter() -> EgclVal {
    let mutex = COND_MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    mutex.grab(true, None).unwrap();
    COND_READY.fetch_add(1, Ordering::AcqRel);
    assert!(
        CONDVAR
            .get()
            .unwrap()
            .wait(mutex, Some(Duration::from_secs(1)))
            .unwrap()
    );
    COND_DONE.fetch_add(1, Ordering::AcqRel);
    mutex.release().unwrap();
    mutex.release().unwrap();
    assert!(mutex.release().is_err());
    T
}

#[test]
fn condition_notify_and_broadcast_wake_parked_fibers() {
    let _guard = serial_lock().lock().unwrap();
    COND_MUTEX.get_or_init(|| EgclMutex::new(None, true));
    CONDVAR.get_or_init(EgclCondVar::default);
    COND_READY.store(0, Ordering::Release);
    COND_DONE.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let first = make_fiber(function(condition_waiter)).unwrap();
    let second = make_fiber(function(condition_waiter)).unwrap();
    group.submit(first).unwrap();
    group.submit(second).unwrap();
    wait_until(|| COND_READY.load(Ordering::Acquire) == 2);
    // READY is incremented while holding the mutex. Taking it proves both
    // waits have enqueued, so the notification-count assertions are not races.
    let mutex = COND_MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    assert_eq!(CONDVAR.get().unwrap().notify(1), 1);
    mutex.release().unwrap();
    wait_until(|| COND_DONE.load(Ordering::Acquire) == 1);
    assert_eq!(CONDVAR.get().unwrap().broadcast(), 1);
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(COND_DONE.load(Ordering::Acquire), 2);
    assert_eq!(CONDVAR.get().unwrap().broadcast(), 0);
}

static SEMAPHORE: OnceLock<EgclSemaphore> = OnceLock::new();
static SEM_DONE: AtomicUsize = AtomicUsize::new(0);

fn semaphore_waiter() -> EgclVal {
    assert!(
        SEMAPHORE
            .get()
            .unwrap()
            .wait(Some(Duration::from_secs(1)))
            .unwrap()
    );
    SEM_DONE.fetch_add(1, Ordering::AcqRel);
    T
}

#[test]
fn semaphore_signal_assigns_permits_to_waiting_fibers() {
    let _guard = serial_lock().lock().unwrap();
    SEMAPHORE.get_or_init(|| EgclSemaphore::new(None, 0).unwrap());
    SEM_DONE.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let first = make_fiber(function(semaphore_waiter)).unwrap();
    let second = make_fiber(function(semaphore_waiter)).unwrap();
    group.submit(first).unwrap();
    group.submit(second).unwrap();
    wait_until(|| {
        fiber_state(first) == Some(FiberState::Blocked)
            && fiber_state(second) == Some(FiberState::Blocked)
    });
    SEMAPHORE.get().unwrap().signal(2).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(SEM_DONE.load(Ordering::Acquire), 2);
    assert_eq!(SEMAPHORE.get().unwrap().count(), 0);
}

static SLEEP_ORDER: AtomicUsize = AtomicUsize::new(0);

fn sleeping_fiber() -> EgclVal {
    fiber_sleep(Duration::from_millis(20)).unwrap();
    assert_eq!(SLEEP_ORDER.fetch_add(1, Ordering::AcqRel), 1);
    T
}

fn runs_while_other_fiber_sleeps() -> EgclVal {
    assert_eq!(SLEEP_ORDER.fetch_add(1, Ordering::AcqRel), 0);
    T
}

#[test]
fn fiber_sleep_uses_deadline_service_and_releases_carrier() {
    let _guard = serial_lock().lock().unwrap();
    SLEEP_ORDER.store(0, Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let sleeper = make_fiber(function(sleeping_fiber)).unwrap();
    group.submit(sleeper).unwrap();
    wait_until(|| fiber_state(sleeper) == Some(FiberState::Blocked));
    group
        .submit(make_fiber(function(runs_while_other_fiber_sleeps)).unwrap())
        .unwrap();
    assert_eq!(group.finish().unwrap(), vec![T, T]);
    assert_eq!(SLEEP_ORDER.load(Ordering::Acquire), 2);
}

#[cfg(unix)]
static PIPE_READ: AtomicI32 = AtomicI32::new(-1);
#[cfg(unix)]
static PIPE_READY: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
fn fd_waiter() -> EgclVal {
    let ready = wait_fd(
        PIPE_READ.load(Ordering::Acquire),
        IoInterest::Read,
        Some(Duration::from_secs(1)),
    )
    .unwrap();
    PIPE_READY.store(usize::from(ready), Ordering::Release);
    T
}

#[cfg(unix)]
#[test]
fn epoll_readiness_and_timeout_resume_waiting_fibers() {
    let _guard = serial_lock().lock().unwrap();
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    PIPE_READ.store(fds[0], Ordering::Release);
    PIPE_READY.store(0, Ordering::Release);

    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let ready_fiber = make_fiber(function(fd_waiter)).unwrap();
    group.submit(ready_fiber).unwrap();
    wait_until(|| fiber_state(ready_fiber) == Some(FiberState::Waiting));
    let byte = [b'x'];
    assert_eq!(unsafe { libc::write(fds[1], byte.as_ptr().cast(), 1) }, 1);
    assert_eq!(group.finish().unwrap(), vec![T]);
    assert_eq!(PIPE_READY.load(Ordering::Acquire), 1);

    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let timeout_fiber = make_fiber(function(|| {
        let ready = wait_fd(
            PIPE_READ.load(Ordering::Acquire),
            IoInterest::Read,
            Some(Duration::from_millis(15)),
        )
        .unwrap();
        EgclVal::from_fixnum(i64::from(ready))
    }))
    .unwrap();
    // Drain the byte before testing timeout.
    let mut byte = [0_u8; 1];
    assert_eq!(
        unsafe { libc::read(fds[0], byte.as_mut_ptr().cast(), 1) },
        1
    );
    group.submit(timeout_fiber).unwrap();
    assert_eq!(group.finish().unwrap()[0].as_fixnum(), 0);
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

static PINNED_SEM: OnceLock<EgclSemaphore> = OnceLock::new();
static PINNED_READY: AtomicUsize = AtomicUsize::new(0);

fn pinned_native_waiter() -> EgclVal {
    let fiber = current_fiber().unwrap();
    fiber.pin();
    PINNED_READY.store(1, Ordering::Release);
    let result = PINNED_SEM.get().unwrap().wait(None).unwrap();
    fiber.unpin().unwrap();
    assert!(result);
    T
}

#[test]
fn pinned_fiber_uses_configured_native_blocking_fallback() {
    let _guard = serial_lock().lock().unwrap();
    PINNED_SEM.get_or_init(|| EgclSemaphore::new(None, 0).unwrap());
    PINNED_READY.store(0, Ordering::Release);
    set_pinned_blocking_action(PinnedBlockingAction::Native);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    group
        .submit(make_fiber(function(pinned_native_waiter)).unwrap())
        .unwrap();
    wait_until(|| PINNED_READY.load(Ordering::Acquire) == 1);
    PINNED_SEM.get().unwrap().signal(1).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T]);
    set_pinned_blocking_action(PinnedBlockingAction::Warn);
}

static TIMEOUT_MUTEX: OnceLock<EgclMutex> = OnceLock::new();
static TIMEOUT_MUTEX_HELD: AtomicUsize = AtomicUsize::new(0);
static TIMEOUT_COND_MUTEX: OnceLock<EgclMutex> = OnceLock::new();
static TIMEOUT_COND: OnceLock<EgclCondVar> = OnceLock::new();
static TIMEOUT_SEM: OnceLock<EgclSemaphore> = OnceLock::new();

fn timeout_mutex_holder() -> EgclVal {
    let mutex = TIMEOUT_MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    TIMEOUT_MUTEX_HELD.store(1, Ordering::Release);
    fiber_sleep(Duration::from_millis(30)).unwrap();
    mutex.release().unwrap();
    T
}

fn timeout_mutex_waiter() -> EgclVal {
    EgclVal::from_fixnum(i64::from(
        TIMEOUT_MUTEX
            .get()
            .unwrap()
            .grab(true, Some(Duration::from_millis(5)))
            .unwrap(),
    ))
}

fn timeout_condition_waiter() -> EgclVal {
    let mutex = TIMEOUT_COND_MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    let notified = TIMEOUT_COND
        .get()
        .unwrap()
        .wait(mutex, Some(Duration::from_millis(5)))
        .unwrap();
    mutex.release().unwrap();
    EgclVal::from_fixnum(i64::from(notified))
}

fn timeout_semaphore_waiter() -> EgclVal {
    EgclVal::from_fixnum(i64::from(
        TIMEOUT_SEM
            .get()
            .unwrap()
            .wait(Some(Duration::from_millis(5)))
            .unwrap(),
    ))
}

#[test]
fn mutex_condition_and_semaphore_timeouts_return_without_stale_wakes() {
    let _guard = serial_lock().lock().unwrap();
    TIMEOUT_MUTEX.get_or_init(EgclMutex::default);
    TIMEOUT_COND_MUTEX.get_or_init(EgclMutex::default);
    TIMEOUT_COND.get_or_init(EgclCondVar::default);
    TIMEOUT_SEM.get_or_init(|| EgclSemaphore::new(None, 0).unwrap());
    TIMEOUT_MUTEX_HELD.store(0, Ordering::Release);

    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let holder = make_fiber(function(timeout_mutex_holder)).unwrap();
    group.submit(holder).unwrap();
    wait_until(|| TIMEOUT_MUTEX_HELD.load(Ordering::Acquire) == 1);
    group
        .submit(make_fiber(function(timeout_mutex_waiter)).unwrap())
        .unwrap();
    group
        .submit(make_fiber(function(timeout_condition_waiter)).unwrap())
        .unwrap();
    group
        .submit(make_fiber(function(timeout_semaphore_waiter)).unwrap())
        .unwrap();
    let values = group.finish().unwrap();
    assert_eq!(values[0], T);
    assert!(values[1..].iter().all(|value| value.as_fixnum() == 0));

    // A timed-out semaphore waiter was removed; a later permit remains
    // available instead of being assigned to stale queue state.
    TIMEOUT_SEM.get().unwrap().signal(1).unwrap();
    assert!(TIMEOUT_SEM.get().unwrap().try_wait(1).unwrap());
}
