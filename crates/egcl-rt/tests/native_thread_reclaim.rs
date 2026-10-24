// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A dedicated thread that finishes without being joined is reclaimed (#591):
//! its OS thread is detached and its full per-thread record dropped, while its
//! name, liveness and still-rooted result stay observable until one JOIN.
//!
//! One test in its own binary: the registry count it reads is process-wide.
use std::time::{Duration, Instant};
use egcl_rt::thread::{
    all_thread_ids, current_thread_id, dedicated_native_thread_record_count, interrupt_thread,
    join_thread, join_thread_timeout, live_thread_ids, make_thread, thread_alive,
    thread_is_carrier, thread_name,
};
use egcl_rt::value::{EgclVal, T};

const WORKERS: usize = 64;

/// Each result's address at allocation, to prove the collection moved it.
static ORIGINALS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

fn boxed_float_entry() -> EgclVal {
    let body = egcl_rt::gc::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe { *(body as *mut f64) = 42.0 };
    let value = unsafe { EgclVal::from_heap_ptr(body.sub(8)) };
    ORIGINALS.lock().unwrap().push(value.to_raw());
    value
}

fn native_entry(function: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(function as *const () as *mut u8) }
}

fn float_value(value: EgclVal) -> f64 {
    unsafe { *(value.as_ptr().add(8) as *const f64) }
}

#[test]
fn finished_unjoined_threads_are_reclaimed_and_join_exactly_once() {
    egcl_rt::gc::ensure_heap_initialized();
    // Register this thread as a Running participant: only such a thread may
    // reclaim others, exactly as a Lisp caller of MAKE-THREAD is.
    let me = current_thread_id();
    let baseline = dedicated_native_thread_record_count();

    let ids: Vec<_> = (0..WORKERS)
        .map(|_| make_thread(native_entry(boxed_float_entry)).unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(20);
    while ids.iter().any(|id| thread_alive(*id) == Some(true)) {
        assert!(Instant::now() < deadline, "workers did not finish");
        std::thread::yield_now();
    }

    // Each creation reclaims what has finished. Keep creating (and joining)
    // a trigger until every worker's OS thread has been seen to exit.
    let mut reclaimed = false;
    while Instant::now() < deadline {
        let trigger = make_thread(T).unwrap();
        assert_eq!(join_thread(trigger).unwrap(), T);
        if dedicated_native_thread_record_count() <= baseline {
            reclaimed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        reclaimed,
        "finished, unjoined threads kept their records: {} > {baseline}",
        dedicated_native_thread_record_count()
    );

    // The unconsumed results must stay rooted, and be relocated, across a
    // moving collection now that only the reclaimed record refers to them.
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert!(egcl_rt::gc::heap_stats().minor_gc_count > 0);

    let all = all_thread_ids();
    let live = live_thread_ids();
    assert!(all.contains(&me) && live.contains(&me));
    for id in &ids {
        assert_eq!(thread_alive(*id), Some(false));
        assert_eq!(thread_name(*id), Some(format!("egcl-thread-{}", id.0)));
        assert_eq!(thread_is_carrier(*id), Some(false));
        assert!(!all.contains(id) && !live.contains(id));
        interrupt_thread(*id, T).expect("interrupting a finished thread is a no-op");
    }
    let originals = ORIGINALS.lock().unwrap().clone();
    assert_eq!(originals.len(), WORKERS);
    let mut moved = 0;
    for (index, id) in ids.iter().enumerate() {
        let value = if index % 2 == 0 {
            join_thread(*id).unwrap()
        } else {
            join_thread_timeout(*id, Some(Duration::ZERO))
                .unwrap()
                .expect("a finished thread joins within any timeout")
        };
        assert_eq!(float_value(value), 42.0);
        if !originals.contains(&value.to_raw()) {
            moved += 1;
        }
        assert!(join_thread(*id).is_err(), "a second join must fail");
        assert_eq!(thread_alive(*id), None, "a joined handle leaves the registry");
        assert_eq!(thread_name(*id), None);
    }
    assert!(moved > 0, "no result was relocated, so rooting went untested");
}
