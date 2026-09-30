#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use egcl_rt::thread::{FiberId, fiber_state, make_fiber};
use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclError, EgclVal};

static FIRST: AtomicU64 = AtomicU64::new(0);
static READY: AtomicBool = AtomicBool::new(false);
static COLLECTED: AtomicBool = AtomicBool::new(false);

fn collecting() -> EgclVal {
    READY.store(true, Ordering::Release);
    while fiber_state(FiberId(FIRST.load(Ordering::Acquire))).is_some() {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    COLLECTED.store(egcl_rt::collect_t0_minor().is_ok(), Ordering::Release);
    EgclVal::from_fixnum(17)
}

#[test]
fn finish_preserves_an_error_datum_while_shutting_down_carriers() {
    let invalid_entry = egcl_rt::gc::alloc_double_float(42.0);
    let original = invalid_entry.to_raw();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let id = make_fiber(invalid_entry).unwrap();
    FIRST.store(id.0, Ordering::Release);
    group.submit(id).unwrap();
    let entry = unsafe { EgclVal::from_function_ptr(collecting as *const () as *mut u8) };
    group.submit(make_fiber(entry).unwrap()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !READY.load(Ordering::Acquire) {
        egcl_rt::poll_safepoint();
        assert!(
            std::time::Instant::now() < deadline,
            "collector did not start"
        );
        std::thread::yield_now();
    }
    let EgclError::TypeError { datum, .. } = group.finish().unwrap_err() else {
        panic!("invalid entry must return its type error");
    };
    assert!(
        COLLECTED.load(Ordering::Acquire),
        "collector handshake failed"
    );
    assert!(
        fiber_state(id).is_none(),
        "failed join must remove its registry root"
    );
    assert_ne!(datum.to_raw(), original, "error datum must be relocated");
    assert_eq!(unsafe { datum.as_ptr().add(8).cast::<f64>().read() }, 42.0);
}
