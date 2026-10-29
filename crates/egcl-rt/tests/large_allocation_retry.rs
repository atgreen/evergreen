// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclVal, GcConfig, gc, object::type_id, value::NIL};

fn bytes(size: usize) -> Option<EgclVal> {
    let body = gc::alloc_typed(size, type_id::SIMPLE_BASE_STRING)?;
    unsafe {
        std::ptr::write_bytes(body, b'x', size);
        body.cast::<u64>().write((size - 8) as u64);
        Some(EgclVal::from_heap_ptr(
            body.sub(gc::body_header_offset(size)),
        ))
    }
}

fn assert_bytes(value: EgclVal, size: usize) {
    unsafe {
        let body = value.as_ptr().add(gc::body_header_offset(size));
        assert_eq!(body.cast::<u64>().read(), (size - 8) as u64);
        assert_eq!(*body.add(8), b'x');
        assert_eq!(*body.add(size - 1), b'x');
    }
}

#[test]
fn discarded_large_allocations_collect_once_and_preserve_live_objects() {
    egcl_rt::init_heap(&GcConfig {
        heap_size: 2 * 1024 * 1024,
        heap_max: 2 * 1024 * 1024,
        nursery_size: 64 * 1024,
        tlab_size: 256,
        region_size: 16 * 1024,
        promotion_threshold: 2,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    // Extended header, multiple regions, and stable large-object identity.
    egcl_rt::rooted!(large = bytes(80_000).unwrap());
    let large_before = *large;
    // Leave dead padding before the live cons. Even if the full cycle reuses
    // this nursery region as old space, compaction must change its offset.
    let padding = gc::alloc_typed(8, type_id::DOUBLE_FLOAT).unwrap();
    unsafe { padding.cast::<f64>().write(0.0) };
    let body = gc::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(42));
        body.cast::<EgclVal>().add(1).write(NIL);
    }
    egcl_rt::rooted!(nursery = unsafe { EgclVal::from_cons_ptr(body) });
    let nursery_before = *nursery;
    let collections_before = gc::heap_stats().minor_gc_count;
    let mut saw_movement = false;
    // Medium objects exceed a TLAB but fit in one region. Discard enough to
    // exhaust the region budget without filling the tiny nursery TLAB.
    for _ in 0..512 {
        let value = bytes(600).expect("discarded medium objects must be reclaimed");
        assert_bytes(value, 600);
        if !saw_movement && gc::heap_stats().minor_gc_count > collections_before {
            assert_ne!(
                *nursery, nursery_before,
                "retry must actually relocate nursery roots"
            );
            saw_movement = true;
        }
    }
    assert!(saw_movement, "the allocation slow path must collect");
    // Also reclaim discarded extended-header objects spanning several regions.
    for _ in 0..128 {
        let value = bytes(80_000).expect("discarded multi-region objects must be reclaimed");
        assert_bytes(value, 80_000);
        assert_eq!(
            *large, large_before,
            "live large objects must remain nonmoving"
        );
        assert_bytes(*large, 80_000);
        assert_eq!(
            unsafe { nursery.as_ptr().cast::<EgclVal>().read() },
            EgclVal::from_fixnum(42)
        );
    }
    // The invalidated TLAB must refill safely after the successful large retry.
    let small = gc::alloc_typed(8, type_id::DOUBLE_FLOAT).unwrap();
    unsafe { small.cast::<f64>().write(7.0) };

    // A request larger than the whole heap is rejected without any collection.
    let before = gc::heap_stats();
    assert!(bytes(3 * 1024 * 1024).is_none());
    let after = gc::heap_stats();
    assert_eq!(after.minor_gc_count, before.minor_gc_count);
    assert_eq!(after.major_gc_count, before.major_gc_count);
    assert!(gc::alloc_typed(usize::MAX, type_id::SIMPLE_BASE_STRING).is_none());

    // Retained large objects cannot be reclaimed. The slow path terminates with
    // OOM after a bounded collection cycle, while preserving every live payload.
    egcl_rt::rooted!(retained = Vec::<EgclVal>::new());
    let mut failed = false;
    for _ in 0..128 {
        let before = gc::heap_stats();
        match bytes(80_000) {
            Some(value) => retained.push(value),
            None => {
                let after = gc::heap_stats();
                assert!(after.minor_gc_count - before.minor_gc_count <= 3);
                assert!(after.major_gc_count - before.major_gc_count <= 3);
                failed = true;
                break;
            }
        }
    }
    assert!(failed, "retained heap must still report OOM");
    for &value in retained.iter() {
        assert_bytes(value, 80_000);
    }
    assert_eq!(*large, large_before);
    assert_bytes(*large, 80_000);
    assert_eq!(
        unsafe { nursery.as_ptr().cast::<EgclVal>().read() },
        EgclVal::from_fixnum(42)
    );
    retained.clear();
    assert!(
        bytes(80_000).is_some(),
        "dropping retained data permits recovery"
    );
}
