#![cfg(egcl_fibers)]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::lock_order::{LockLevel, OrderedMutex, OrderedRwLock};
use egcl_rt::thread::{current_fiber, make_fiber};
use egcl_rt::value::{NIL, T};
use egcl_rt::{EgclVal, SchedulerConfig, SchedulerGroup};

fn native_guard_probe() -> EgclVal {
    let fiber = current_fiber().unwrap();
    let mutex = OrderedMutex::new(LockLevel::Stream, 1, "native mutex", ());
    let rwlock = OrderedRwLock::new(LockLevel::Stream, 2, "native rwlock", ());
    let mut correct = fiber.can_yield();
    {
        let _guard = mutex.lock().unwrap();
        correct &= !fiber.can_yield();
        {
            let _nested = rwlock.read().unwrap();
            correct &= !fiber.can_yield();
        }
        correct &= !fiber.can_yield();
    }
    correct &= fiber.can_yield();
    {
        let _guard = rwlock.write().unwrap();
        correct &= !fiber.can_yield();
    }
    correct &= fiber.can_yield();
    if correct { T } else { NIL }
}

#[test]
fn native_ordered_guards_prevent_carrier_migration_and_restore_eligibility() {
    const CHILD: &str = "EGCL_ORDERED_LOCK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_ordered_guards_prevent_carrier_migration_and_restore_eligibility",
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
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let function = native_entry::entry(native_guard_probe);
    group.submit(make_fiber(function).unwrap()).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T]);
}

#[test]
fn execution_lock_parks_contenders_without_blocking_its_owner() {
    use egcl_rt::lock_order::OrderedExecutionMutex;
    use egcl_rt::sync::EgclSemaphore;
    use egcl_rt::thread::{FiberState, fiber_state};
    use std::sync::OnceLock;
    use std::time::Duration;

    static LOCK: OnceLock<OrderedExecutionMutex<u32>> = OnceLock::new();
    static RELEASE: OnceLock<EgclSemaphore> = OnceLock::new();
    fn owner() -> EgclVal {
        let mut value = LOCK.get().unwrap().lock().unwrap();
        assert!(current_fiber().unwrap().can_yield());
        *value = 1;
        RELEASE.get().unwrap().wait(None).unwrap();
        assert_eq!(*value, 1);
        *value = 2;
        NIL
    }
    fn contender() -> EgclVal {
        let mut value = LOCK.get().unwrap().lock().unwrap();
        assert_eq!(*value, 2);
        *value = 3;
        NIL
    }
    fn release() -> EgclVal {
        RELEASE.get().unwrap().signal(1).unwrap();
        NIL
    }
    fn entry(f: fn() -> EgclVal) -> EgclVal {
        native_entry::entry(f)
    }

    const CHILD: &str = "EGCL_EXECUTION_LOCK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution_lock_parks_contenders_without_blocking_its_owner",
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
        std::thread::sleep(Duration::from_secs(10));
        eprintln!("execution lock blocked its carrier");
        std::process::exit(124);
    });
    assert!(
        LOCK.set(OrderedExecutionMutex::new(
            LockLevel::Stream,
            1,
            "execution lock",
            0
        ))
        .is_ok()
    );
    assert!(RELEASE.set(EgclSemaphore::new(None, 0).unwrap()).is_ok());
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let first = make_fiber(entry(owner)).unwrap();
    group.submit(first).unwrap();
    while fiber_state(first) != Some(FiberState::Blocked) {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    let second = make_fiber(entry(contender)).unwrap();
    group.submit(second).unwrap();
    while fiber_state(second) != Some(FiberState::Blocked) {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    group.submit(make_fiber(entry(release)).unwrap()).unwrap();
    group.finish().unwrap();
    assert_eq!(*LOCK.get().unwrap().lock().unwrap(), 3);
}

#[test]
fn execution_lock_ownership_survives_carrier_migration() {
    use egcl_rt::lock_order::OrderedExecutionMutex;
    use egcl_rt::thread::{current_thread_id, fiber_yield};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
    fn probe() -> EgclVal {
        let lock = OrderedExecutionMutex::new(LockLevel::Stream, 1, "migrating", 0);
        let nested = OrderedMutex::new(LockLevel::Stream, 2, "nested native", ());
        let mut guard = lock.lock().unwrap();
        let mut carrier = current_thread_id();
        for expected in 0..200 {
            assert_eq!(*guard, expected);
            *guard += 1;
            fiber_yield().unwrap();
            let next = current_thread_id();
            if next != carrier {
                MIGRATIONS.fetch_add(1, Ordering::Relaxed);
                carrier = next;
            }
            let native = nested.lock().unwrap();
            assert!(!current_fiber().unwrap().can_yield());
            drop(native);
            assert!(current_fiber().unwrap().can_yield());
        }
        drop(guard);
        assert_eq!(*lock.lock().unwrap(), 200);
        T
    }
    const CHILD: &str = "EGCL_MIGRATING_LOCK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "execution_lock_ownership_survives_carrier_migration",
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
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 4 }).unwrap();
    let entry = native_entry::entry(probe);
    for _ in 0..64 {
        group.submit(make_fiber(entry).unwrap()).unwrap();
    }
    assert_eq!(group.finish().unwrap(), vec![T; 64]);
    assert!(
        MIGRATIONS.load(Ordering::Relaxed) > 0,
        "must exercise migration with a live execution guard"
    );
}

#[path = "support/native_entry.rs"]
mod native_entry;
