#![cfg(egcl_fibers)]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::execution_local::ExecutionLocal;
use egcl_rt::scheduler::{SchedulerConfig, SchedulerGroup};
use egcl_rt::thread::{current_fiber_id, current_thread_id, fiber_yield, make_fiber};
use egcl_rt::value::{EgclVal, T};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicUsize = AtomicUsize::new(0);
struct OwnedValue(Cell<u64>);
impl Drop for OwnedValue {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}
static VALUE: ExecutionLocal<OwnedValue> =
    unsafe { ExecutionLocal::new(|| OwnedValue(Cell::new(0))) };
static OTHER: ExecutionLocal<Cell<u64>> = unsafe { ExecutionLocal::new(|| Cell::new(0)) };

fn migrating_fiber() -> EgclVal {
    let identity = current_fiber_id().unwrap().0;
    assert!(VALUE.try_with(|_| ()).is_none());
    VALUE.with(|value| value.0.set(identity));
    OTHER.with(|value| value.set(identity + 1000));
    let mut carrier = current_thread_id();
    // Keep the owning box borrowed while migrating: neither the cache borrow
    // nor a cached carrier TLS address may survive across the callback's yield.
    VALUE.with(|value| {
        for _ in 0..200 {
            assert_eq!(value.0.get(), identity);
            fiber_yield().unwrap();
            let next = current_thread_id();
            if next != carrier {
                MIGRATIONS.fetch_add(1, Ordering::Relaxed);
                carrier = next;
            }
            assert_eq!(value.0.get(), identity);
            assert_eq!(VALUE.try_with(|v| v.0.get()), Some(identity));
            VALUE.with(|v| assert!(std::ptr::eq(v, value)));
            OTHER.with(|v| assert_eq!(v.get(), identity + 1000));
        }
    });
    T
}

#[test]
fn cached_execution_slots_follow_fibers_and_retire_after_migration() {
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 4 }).unwrap();
    for _ in 0..64 {
        let entry = native_entry::entry(migrating_fiber);
        group.submit(make_fiber(entry).unwrap()).unwrap();
    }
    assert_eq!(group.finish().unwrap(), vec![T; 64]);
    assert!(
        MIGRATIONS.load(Ordering::Relaxed) > 0,
        "must exercise migration"
    );
    assert_eq!(DROPS.load(Ordering::Relaxed), 64);
    // A native execution on this thread must not see a retired fiber's box.
    assert!(VALUE.try_with(|_| ()).is_none());
}

#[path = "support/native_entry.rs"]
mod native_entry;
