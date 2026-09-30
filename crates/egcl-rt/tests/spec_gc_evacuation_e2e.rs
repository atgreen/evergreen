// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! End-to-end moving-GC evacuation: interconnected objects rooted on a CL frame
//! survive BOTH a minor (nursery→old) and a major (old-gen compaction) collection
//! with their structure and identity intact — every internal reference and root
//! is relocated before any region is reclaimed (bliss-jtc.17).

use std::sync::{Mutex, OnceLock};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{NIL, TAG_CONS, TAG_MASK, EgclVal};
use egcl_rt::{Allocator, Collector, GcConfig, EgclStack, current_thread, init_heap, walk_heap};
use egcl_rt::{HeapAllocator, HeapCollector};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 4 * 1024 * 1024,
        tlab_size: 4096,
        region_size: 8192,
        promotion_threshold: 0, // promote nursery → old-gen on the first minor GC
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}

/// Allocate a CONS `(car . cdr)` on the GC heap (type_id CONS so trace_object
/// relocates its car/cdr). The body (car@0, cdr@8) is what a TAG_CONS value
/// points at.
fn make_cons(alloc: &mut HeapAllocator, car: EgclVal, cdr: EgclVal) -> EgclVal {
    let body = alloc
        .alloc_fast(16)
        .or_else(|| alloc.alloc_slow(16).ok())
        .expect("cons alloc");
    unsafe {
        let hdr = body.sub(8) as *mut ObjectHeader;
        (*hdr).0 = ((*hdr).0 & 0x00FF_FFFF_FFFF_FFFF) | ((type_id::CONS as u64) << 56);
        *(body as *mut EgclVal) = car;
        *((body as *mut EgclVal).add(1)) = cdr;
    }
    EgclVal((body as u64) | TAG_CONS)
}

fn car(v: EgclVal) -> EgclVal {
    unsafe { *((v.0 & !TAG_MASK) as *const EgclVal) }
}
fn cdr(v: EgclVal) -> EgclVal {
    unsafe { *(((v.0 & !TAG_MASK) as *const EgclVal).add(1)) }
}

/// Verify the list reads back as the given fixnum sequence and terminates in NIL.
fn check_list(mut v: EgclVal, expected: &[i64]) {
    for &e in expected {
        assert_eq!(car(v).as_fixnum(), e, "list element mismatch");
        v = cdr(v);
    }
    assert!(v.is_nil(), "list must terminate in NIL");
}

#[test]
fn persistent_forwarding_chain_is_marked_before_nursery_evacuation() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");
    let first = make_cons(&mut alloc, EgclVal::from_fixnum(1), NIL);
    let second = make_cons(&mut alloc, EgclVal::from_fixnum(2), NIL);
    let target = make_cons(&mut alloc, EgclVal::from_fixnum(42), NIL);
    // Only the first stub is rooted. Neither stub is indexed as a nursery
    // object, so marking must follow the entire chain to retain the target.
    unsafe {
        egcl_rt::gc::forward_object_to(first, second);
        egcl_rt::gc::forward_object_to(second, target);
    }
    let stack = current_thread().stack();
    let frame = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(frame)[0] = first };
    HeapCollector::new().minor_gc().expect("minor_gc");
    let relocated = unsafe { EgclStack::frame_slots_mut(frame)[0] };
    assert_ne!(relocated, target, "the target must actually evacuate");
    check_list(relocated, &[42]);
    HeapCollector::new().major_gc().expect("major_gc");
    check_list(unsafe { EgclStack::frame_slots_mut(frame)[0] }, &[42]);
    stack.pop_frame();
}

#[test]
fn interconnected_objects_survive_minor_and_major_evacuation_with_identity() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // A rooted linked list (100 200 300), plus many objects that survive the
    // minor collection and then become unreachable. That leaves the promoted
    // old-gen region mostly dead and forces major evacuation.
    let c3 = make_cons(&mut alloc, EgclVal::from_fixnum(300), NIL);
    let c2 = make_cons(&mut alloc, EgclVal::from_fixnum(200), c3);
    let c1 = make_cons(&mut alloc, EgclVal::from_fixnum(100), c2);
    let garbage: Vec<EgclVal> = (0..400)
        .map(|_| make_cons(&mut alloc, EgclVal::from_fixnum(0), NIL))
        .collect();

    // Root the list head and temporary objects on the current thread's CL stack.
    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 401, 0)
        .unwrap();
    unsafe {
        let slots = EgclStack::frame_slots_mut(f);
        slots[0] = c1;
        slots[1..].copy_from_slice(&garbage);
    }
    let orig = c1;

    // MINOR GC: promotes the rooted objects to old-gen (threshold 0); relocates the
    // frame root and the list's cdr links to the promoted copies.
    HeapCollector::new().minor_gc().expect("minor_gc");
    let after_minor = unsafe { EgclStack::frame_slots_mut(f)[0] };
    assert_ne!(after_minor.0, orig.0, "list evacuated by minor GC");
    check_list(after_minor, &[100, 200, 300]);

    // Drop the temporary roots, then MAJOR GC: the mostly-dead old-gen region is
    // evacuated, moving the live list again and rewriting its internal links +
    // the frame root before the region is freed.
    unsafe {
        EgclStack::frame_slots_mut(f)[1..].fill(NIL);
    }
    HeapCollector::new().full_gc().expect("full_gc");
    let after_major = unsafe { EgclStack::frame_slots_mut(f)[0] };
    assert_ne!(
        after_major.0, after_minor.0,
        "list evacuated again by major GC (old-gen compaction)"
    );
    check_list(after_major, &[100, 200, 300]);

    stack.pop_frame();
}

#[test]
fn minor_gc_does_not_promote_unreachable_nursery_objects() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    let live = make_cons(&mut alloc, EgclVal::from_fixnum(42), NIL);
    for _ in 0..400 {
        let _ = make_cons(&mut alloc, EgclVal::from_fixnum(0), NIL);
    }

    let stack = current_thread().stack();
    let frame = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(frame)[0] = live };

    HeapCollector::new().minor_gc().expect("minor_gc");

    let relocated = unsafe { EgclStack::frame_slots_mut(frame)[0] };
    assert_eq!(car(relocated).as_fixnum(), 42);
    let mut cons_count = 0;
    walk_heap(|_, tid, _| {
        if tid == type_id::CONS {
            cons_count += 1;
        }
        true
    })
    .expect("walk_heap");
    assert_eq!(
        cons_count, 1,
        "only the rooted cons should survive the nursery collection"
    );

    stack.pop_frame();
}
