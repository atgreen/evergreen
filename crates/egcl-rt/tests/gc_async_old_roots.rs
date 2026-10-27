// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Unbarriered old-to-young stores, including a newly allocated large object's
//! continuation pages. Run once per process under Auto/legacy/off and verify.
use egcl_rt::object::type_id;
use egcl_rt::value::{EgclVal, NIL};
use egcl_rt::{GcConfig, gc};

fn cons(marker: i64) -> EgclVal {
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(marker));
        body.cast::<EgclVal>().add(1).write(NIL);
        EgclVal::from_cons_ptr(body)
    }
}

#[test]
fn async_old_pages_retain_moving_young_values_without_barriers() {
    const CHILD: &str = "EGCL_ASYNC_OLD_POSITIVE_CHILD";
    if std::env::var_os("EGCL_TEST_ASYNC_OLD_POSITIVE").is_some()
        && std::env::var_os(CHILD).is_none()
    {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "async_old_pages_retain_moving_young_values_without_barriers",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_GC_ASYNC_WP_LOG", "1")
            .output()
            .expect("run isolated positive async cohort");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            stderr
        );
        assert!(
            stderr
                .matches("[gc-async-wp] snapshot older ranges")
                .count()
                >= 5,
            "old-cons, new large-tail, armed tail and post-major snapshots must all execute asynchronously: {stderr}"
        );
        assert!(
            !stderr.contains("fallback") && !stderr.contains("read failed"),
            "positive cohort must not silently pass through legacy: {stderr}"
        );
        for phase in [801, 802, 803] {
            assert!(
                stderr.contains(&format!("ASYNC-LARGE-PHASE {phase}")),
                "{stderr}"
            );
        }
        return;
    }
    egcl_rt::init_heap(&GcConfig {
        heap_size: 2 * 1024 * 1024,
        heap_max: 2 * 1024 * 1024,
        nursery_size: 16 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    egcl_rt::rooted!(older = Vec::<EgclVal>::new());
    for i in 0..200 {
        older.push(cons(i));
    }
    gc::collect_t0_minor().unwrap();
    // Live older bytes now exceed Auto's nursery/4 eligibility threshold.
    let before = cons(701).to_raw();
    unsafe {
        older[0]
            .as_ptr()
            .cast::<EgclVal>()
            .write(EgclVal::from_raw(before));
    }
    gc::collect_t0_minor().unwrap();
    let moved = unsafe { older[0].as_ptr().cast::<EgclVal>().read() };
    assert_ne!(
        moved.to_raw(),
        before,
        "old cons's sole young referent must move"
    );
    assert_eq!(
        unsafe { moved.as_ptr().cast::<EgclVal>().read().as_fixnum() },
        701
    );

    // This new LargeObject was absent from the last armed coverage. Its last
    // element lies beyond the head region's alloc_limit and must default dirty.
    let length = 1600usize;
    let bytes = (length + 1) * 8;
    let body = egcl_rt::alloc_typed(bytes, type_id::SIMPLE_VECTOR).unwrap();
    unsafe {
        body.cast::<usize>().write(length);
        for i in 0..length {
            body.cast::<EgclVal>().add(i + 1).write(NIL);
        }
    }
    egcl_rt::rooted!(
        large = unsafe { EgclVal::from_heap_ptr(body.sub(gc::body_header_offset(bytes))) }
    );
    for marker in [801, 802, 803] {
        let before = cons(marker).to_raw();
        // No Lisp allocation occurs between taking this slot and storing.
        let slot = unsafe {
            (*large)
                .as_ptr()
                .add(gc::body_header_offset(bytes))
                .cast::<EgclVal>()
                .add(length)
        };
        unsafe {
            slot.write(EgclVal::from_raw(before));
        }
        gc::collect_t0_minor().unwrap();
        let slot = unsafe {
            (*large)
                .as_ptr()
                .add(gc::body_header_offset(bytes))
                .cast::<EgclVal>()
                .add(length)
        };
        let value = unsafe { slot.read() };
        assert_ne!(
            value.to_raw(),
            before,
            "large tail's sole young referent must move"
        );
        assert_eq!(
            unsafe { value.as_ptr().cast::<EgclVal>().read().as_fixnum() },
            marker
        );
        eprintln!("ASYNC-LARGE-PHASE {marker}");
        if marker == 802 {
            gc::full_gc().unwrap();
        }
    }
}
