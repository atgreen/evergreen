#![cfg(egcl_fibers)]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::thread::{FiberId, fiber_state, make_fiber};
use egcl_rt::{EgclVal, SchedulerConfig, SchedulerGroup};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static FIRST: AtomicU64 = AtomicU64::new(0);
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

fn first() -> EgclVal {
    let value = egcl_rt::gc::alloc_double_float(42.0);
    ORIGINAL.store(value.to_raw() as usize, Ordering::Release);
    value
}

fn second() -> EgclVal {
    // Wait until finish has consumed the first result and removed its registry
    // root. The only remaining owner should be finish's result accumulator.
    while fiber_state(FiberId(FIRST.load(Ordering::Acquire))).is_some() {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    if egcl_rt::collect_t0_minor().is_err() {
        return EgclVal::from_fixnum(-1);
    }
    EgclVal::from_fixnum(17)
}

#[test]
fn finish_roots_early_results_while_later_fibers_collect() {
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let entry = native_entry::entry(first);
    let id = make_fiber(entry).unwrap();
    FIRST.store(id.0, Ordering::Release);
    group.submit(id).unwrap();
    let entry = native_entry::entry(second);
    group.submit(make_fiber(entry).unwrap()).unwrap();
    let results = group.finish().unwrap();
    assert_eq!(
        results[1],
        EgclVal::from_fixnum(17),
        "collector handshake failed"
    );
    assert_ne!(
        results[0].to_raw() as usize,
        ORIGINAL.load(Ordering::Acquire),
        "the accumulated result must be relocated, not left pointing at freed nursery storage"
    );
    assert_eq!(
        unsafe { results[0].as_ptr().add(8).cast::<f64>().read() },
        42.0
    );
}

#[path = "support/native_entry.rs"]
mod native_entry;
