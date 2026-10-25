// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ownership of a fiber wait token when a deadline and a notifier race
//! (bliss-xown). Each test forces one interleaving with a hook instead of
//! hoping a timer lands inside a few-instruction window, so the outcome is
//! the same on every run.

use super::*;
use crate::sync::{EgclCondVar, EgclMutex, EgclSemaphore};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod native_entry {
    use crate as egcl_rt;
    include!("../../tests/support/native_entry.rs");
}

type Hook = Box<dyn FnOnce() + Send>;

/// Runs in a resumed semaphore or condition wait before it reads why it woke.
static AFTER_WAIT_PARK: Mutex<Option<Hook>> = Mutex::new(None);

thread_local! {
    /// Runs in `wake_fiber_wait` before the waker commits to its token.
    static BEFORE_WAKE_CLAIM: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(crate) fn after_wait_park() {
    let hook = AFTER_WAIT_PARK.lock().unwrap().take();
    if let Some(hook) = hook {
        hook();
    }
}

/// Runs in SIGNAL after the permits are added to the count and before any is
/// handed to a waiter.
static AFTER_SEMAPHORE_COUNT_RAISED: Mutex<Option<Hook>> = Mutex::new(None);

pub(crate) fn after_semaphore_count_raised() {
    let hook = AFTER_SEMAPHORE_COUNT_RAISED.lock().unwrap().take();
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn before_wake_claim() {
    let hook = BEFORE_WAKE_CLAIM.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

static ARRIVED: AtomicBool = AtomicBool::new(false);
static GO: AtomicBool = AtomicBool::new(false);

/// Stop the hooked execution until the test releases it.
fn gate() {
    ARRIVED.store(true, Ordering::Release);
    while !GO.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}

fn wait_for(predicate: impl Fn() -> bool) {
    while !predicate() {
        crate::poll_safepoint();
        std::thread::yield_now();
    }
}

fn parked_unmounted(fiber: &Fiber) -> bool {
    let state = fiber.state.lock().unwrap();
    *state == FiberState::Blocked && !fiber.mounted.load(Ordering::Acquire)
}

fn isolated(name: &str, body: impl FnOnce()) {
    const CHILD: &str = "EGCL_TEST_WAIT_TOKEN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("thread::wait_token_tests::{name}"),
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(20));
        eprintln!("wait-token race test hung");
        std::process::exit(124);
    });
    current_thread_id();
    body();
}

/// Park `entry`'s fiber on a two-carrier pool and return it with its token.
fn start_parked(entry: fn() -> EgclVal) -> (CarrierPool, FiberId, Arc<Fiber>, u64) {
    let pool = CarrierPool::new(2);
    let fiber = make_fiber(native_entry::entry(entry)).unwrap();
    let descriptor = lookup_fiber(fiber).unwrap();
    pool.submit(fiber).unwrap();
    wait_for(|| parked_unmounted(&descriptor));
    let token = descriptor.wait_generation.load(Ordering::Acquire);
    (pool, fiber, descriptor, token)
}

static SEMAPHORE: OnceLock<EgclSemaphore> = OnceLock::new();

fn semaphore_entry() -> EgclVal {
    let acquired = SEMAPHORE
        .get()
        .unwrap()
        .wait(Some(Duration::from_secs(60)))
        .unwrap();
    EgclVal::from_fixnum(i64::from(acquired))
}

/// The deadline wakes the waiter first; SIGNAL then pops the same waiter
/// before it has read its wake reason. The signal lost the token, so its
/// permit must stay in the count, and the waiter must not also report it.
#[test]
fn semaphore_signal_losing_to_deadline_keeps_its_permit() {
    isolated("semaphore_signal_losing_to_deadline_keeps_its_permit", || {
        assert!(SEMAPHORE.set(EgclSemaphore::new(None, 0).unwrap()).is_ok());
        let (pool, fiber, _descriptor, token) = start_parked(semaphore_entry);
        *AFTER_WAIT_PARK.lock().unwrap() = Some(Box::new(gate));
        // The deadline service completes the wait.
        assert!(wake_fiber_wait(fiber, token));
        wait_for(|| ARRIVED.load(Ordering::Acquire));
        SEMAPHORE.get().unwrap().signal(1).unwrap();
        GO.store(true, Ordering::Release);
        let acquired = join_fiber(fiber).unwrap().as_fixnum();
        let remaining = SEMAPHORE.get().unwrap().count();
        assert_eq!(
            acquired + remaining,
            1,
            "one permit became {acquired} acquisition(s) plus {remaining} left in the count"
        );
        pool.shutdown_and_join().unwrap();
    });
}

static COND_MUTEX: OnceLock<EgclMutex> = OnceLock::new();
static CONDVAR: OnceLock<EgclCondVar> = OnceLock::new();

fn condition_entry() -> EgclVal {
    let mutex = COND_MUTEX.get().unwrap();
    mutex.grab(true, None).unwrap();
    let notified = CONDVAR
        .get()
        .unwrap()
        .wait(mutex, Some(Duration::from_secs(60)))
        .unwrap();
    mutex.release().unwrap();
    EgclVal::from_fixnum(i64::from(notified))
}

/// As above for CONDITION-NOTIFY: a waiter the deadline already completed is
/// not selected, so NOTIFY's count and the waiter's result must agree.
#[test]
fn condition_notify_losing_to_deadline_is_not_reported_by_the_waiter() {
    isolated(
        "condition_notify_losing_to_deadline_is_not_reported_by_the_waiter",
        || {
            assert!(COND_MUTEX.set(EgclMutex::default()).is_ok());
            assert!(CONDVAR.set(EgclCondVar::default()).is_ok());
            let (pool, fiber, _descriptor, token) = start_parked(condition_entry);
            *AFTER_WAIT_PARK.lock().unwrap() = Some(Box::new(gate));
            assert!(wake_fiber_wait(fiber, token));
            wait_for(|| ARRIVED.load(Ordering::Acquire));
            let selected = CONDVAR.get().unwrap().notify(1) as i64;
            GO.store(true, Ordering::Release);
            let reported = join_fiber(fiber).unwrap().as_fixnum();
            assert_eq!(
                reported, selected,
                "NOTIFY selected {selected} waiter(s) but the waiter reported notified={reported}"
            );
            pool.shutdown_and_join().unwrap();
        },
    );
}

static SECOND_WAKE_ISSUED: AtomicBool = AtomicBool::new(false);

fn two_waits_entry() -> EgclVal {
    prepare_current_fiber_park(FiberState::Blocked).unwrap();
    park_prepared_current_fiber().unwrap();
    prepare_current_fiber_park(FiberState::Blocked).unwrap();
    park_prepared_current_fiber().unwrap();
    EgclVal::from_fixnum(i64::from(SECOND_WAKE_ISSUED.load(Ordering::Acquire)))
}

/// A waker that validated token N, then stalled while another waker completed
/// wait N and the fiber entered wait N+1, must not complete wait N+1.
#[test]
fn stale_waker_cannot_complete_the_next_wait() {
    isolated("stale_waker_cannot_complete_the_next_wait", || {
        let (pool, fiber, descriptor, first) = start_parked(two_waits_entry);
        let stale = std::thread::spawn(move || {
            BEFORE_WAKE_CLAIM.with(|slot| *slot.borrow_mut() = Some(Box::new(gate)));
            wake_fiber_wait(fiber, first)
        });
        wait_for(|| ARRIVED.load(Ordering::Acquire));
        assert!(wake_fiber_wait(fiber, first));
        wait_for(|| {
            descriptor.wait_generation.load(Ordering::Acquire) == first + 1
                && parked_unmounted(&descriptor)
        });
        GO.store(true, Ordering::Release);
        let stale_won = stale.join().unwrap();
        assert!(
            !stale_won,
            "a waker holding token {first} completed wait {}",
            first + 1
        );
        assert!(parked_unmounted(&descriptor), "the second wait was woken");
        SECOND_WAKE_ISSUED.store(true, Ordering::Release);
        assert!(wake_fiber_wait(fiber, first + 1));
        assert_eq!(join_fiber(fiber).unwrap().as_fixnum(), 1);
        pool.shutdown_and_join().unwrap();
    });
}

fn self_wake_entry() -> EgclVal {
    let (fiber, token) = prepare_current_fiber_park(FiberState::Blocked).unwrap();
    // Still mounted: both wakes take the recorded wake_pending path.
    let first = wake_fiber_wait(fiber, token);
    let second = wake_fiber_wait(fiber, token);
    park_prepared_current_fiber().unwrap();
    EgclVal::from_fixnum(i64::from(first) * 2 + i64::from(second))
}

/// Before the carrier has unmounted a prepared wait, only one waker owns it.
#[test]
fn only_one_waker_claims_a_wait_that_is_still_mounted() {
    isolated("only_one_waker_claims_a_wait_that_is_still_mounted", || {
        let pool = CarrierPool::new(1);
        let fiber = make_fiber(native_entry::entry(self_wake_entry)).unwrap();
        pool.submit(fiber).unwrap();
        let claims = join_fiber(fiber).unwrap().as_fixnum();
        assert_eq!(claims, 2, "expected first=true second=false, got code {claims}");
        pool.shutdown_and_join().unwrap();
    });
}

/// A TRY-WAIT that takes SIGNAL's permit between SIGNAL raising the count and
/// handing that permit to a parked waiter: the permit goes to exactly one of
/// them and the count never goes negative (bliss-8f1c1).
#[test]
fn signal_does_not_hand_off_a_permit_try_wait_already_took() {
    isolated("signal_does_not_hand_off_a_permit_try_wait_already_took", || {
        assert!(SEMAPHORE.set(EgclSemaphore::new(None, 0).unwrap()).is_ok());
        let (pool, fiber, descriptor, _token) = start_parked(semaphore_entry);
        let stolen = Arc::new(AtomicBool::new(false));
        let thief = Arc::clone(&stolen);
        *AFTER_SEMAPHORE_COUNT_RAISED.lock().unwrap() = Some(Box::new(move || {
            thief.store(SEMAPHORE.get().unwrap().try_wait(1).unwrap(), Ordering::Release);
        }));
        SEMAPHORE.get().unwrap().signal(1).unwrap();
        assert!(stolen.load(Ordering::Acquire), "the hook's TRY-WAIT took the permit");
        let count = SEMAPHORE.get().unwrap().count();
        assert_eq!(count, 0, "one permit, taken once, left the count at {count}");
        assert!(parked_unmounted(&descriptor), "the waiter was also given the permit");
        // A second permit is the waiter's.
        SEMAPHORE.get().unwrap().signal(1).unwrap();
        assert_eq!(join_fiber(fiber).unwrap().as_fixnum(), 1);
        assert_eq!(SEMAPHORE.get().unwrap().count(), 0);
        pool.shutdown_and_join().unwrap();
    });
}
