// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Precise pinning + non-moving large-object policy (bliss-jtc.18): a pinned
//! object keeps its address across collections, references to it from moved
//! objects stay valid, and large objects are never copied.

use std::sync::{Mutex, OnceLock};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{NIL, TAG_CONS, TAG_MASK, EgclVal};
use egcl_rt::{Allocator, Collector, GcConfig, EgclStack, current_thread, init_heap, pin};
use egcl_rt::{HeapAllocator, HeapCollector, full_gc, walk_heap};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 8 * 1024 * 1024,
        nursery_size: 4 * 1024 * 1024,
        tlab_size: 512,
        region_size: 512, // small regions so distinct objects land in distinct regions
        promotion_threshold: 0,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}

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
fn fill_region(alloc: &mut HeapAllocator) {
    for _ in 0..40 {
        let _ = make_cons(alloc, EgclVal::from_fixnum(0), NIL);
    }
}

#[test]
fn pinned_object_keeps_its_address_while_others_are_evacuated() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // P (pinned) and Q (not) in separate regions; both rooted.
    let p = make_cons(&mut alloc, EgclVal::from_fixnum(999), NIL);
    pin(p);
    fill_region(&mut alloc);
    let q = make_cons(&mut alloc, EgclVal::from_fixnum(111), NIL);

    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 2, 0)
        .unwrap();
    unsafe {
        EgclStack::frame_slots_mut(f)[0] = p;
        EgclStack::frame_slots_mut(f)[1] = q;
    }
    let p_addr = p.0 & !TAG_MASK;

    HeapCollector::new().minor_gc().expect("minor_gc");

    let p_after = unsafe { EgclStack::frame_slots_mut(f)[0] };
    let q_after = unsafe { EgclStack::frame_slots_mut(f)[1] };
    assert_eq!(p_after.0 & !TAG_MASK, p_addr, "pinned object was NOT moved");
    assert_eq!(car(p_after).as_fixnum(), 999, "pinned contents intact");
    assert_ne!(
        q_after.0 & !TAG_MASK,
        q.0 & !TAG_MASK,
        "unpinned object WAS moved"
    );
    assert_eq!(
        car(q_after).as_fixnum(),
        111,
        "moved object contents intact"
    );

    stack.pop_frame();
}

#[test]
fn reference_to_a_pinned_object_from_a_moved_object_stays_valid() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // P pinned; O (in a later region) references P via its cdr and will move.
    let p = make_cons(&mut alloc, EgclVal::from_fixnum(555), NIL);
    pin(p);
    let p_addr = p.0 & !TAG_MASK;
    fill_region(&mut alloc);
    let o = make_cons(&mut alloc, EgclVal::from_fixnum(42), p);

    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(f)[0] = o };

    HeapCollector::new().minor_gc().expect("minor_gc");

    let o_after = unsafe { EgclStack::frame_slots_mut(f)[0] };
    assert_ne!(
        o_after.0 & !TAG_MASK,
        o.0 & !TAG_MASK,
        "referencing object moved"
    );
    // O's cdr still points at the un-moved pinned object, whose contents are intact.
    assert_eq!(
        cdr(o_after).0 & !TAG_MASK,
        p_addr,
        "cdr still points at pinned object"
    );
    assert_eq!(car(cdr(o_after)).as_fixnum(), 555, "pinned referent intact");

    stack.pop_frame();
}

#[test]
fn large_object_is_never_moved_across_repeated_major_collections() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");
    let mut alloc = HeapAllocator::new().expect("allocator");

    // A large object (bigger than half a region) lands in a dedicated
    // non-moving large-object region.
    let large = alloc.alloc_large(2048).expect("large object");
    unsafe { *(large as *mut u64) = 0x00BA_DA55 };
    let large_addr = large as u64;
    // Root the object so it legitimately SURVIVES the collections — an
    // unreferenced large object (single- or multi-region) is now reclaimed
    // (bliss-jg6g/bliss-hy5v). This test asserts a surviving large object is
    // never MOVED (R3.19), not the old leak. 2048 bytes fits the inline size
    // field, so the value is the small-header tagged pointer, body − 8.
    let saved = egcl_rt::gc::get_entry_continuation();
    egcl_rt::gc::set_entry_continuation(unsafe { EgclVal::from_heap_ptr(large.sub(8)) });

    // Churn the heap and force repeated major collections.
    for _ in 0..3 {
        for _ in 0..200 {
            let _ = make_cons(&mut alloc, EgclVal::from_fixnum(0), NIL);
        }
        full_gc().expect("full_gc");
    }

    // The large object is still at its original address, walkable, contents intact.
    assert_eq!(
        unsafe { *(large_addr as *const u64) },
        0x00BA_DA55,
        "large object intact"
    );
    let mut found = false;
    walk_heap(|ptr, _tid, _size| {
        if ptr as u64 == large_addr {
            found = true;
        }
        true
    })
    .expect("walk_heap");
    assert!(found, "large object still walkable at its original address");
    egcl_rt::gc::set_entry_continuation(saved);
}

/// A PINNED large object must be retained across a major GC even with no
/// references to it — pinning promises a stable address, so an addressed buffer
/// (e.g. handed to FFI) unreachable from the heap must not be freed under the
/// caller (bliss-7puh). NOTE: today large objects are unconditionally marked
/// live in the major GC (never reclaimed at all — see the follow-up bead), so
/// this retention holds by that rule; the PINNED guard added to the LargeObject
/// sweep arm is defense-in-depth that becomes load-bearing once large objects
/// become collectible. This test pins the RETENTION GUARANTEE regardless of
/// which mechanism provides it.
#[test]
fn dead_single_region_large_object_is_reclaimed_by_a_major_gc() {
    // bliss-jg6g: a single-region large object that becomes unreachable must be
    // reclaimed by the major GC. Previously the sweep force-marked every
    // LargeObject region live, so a dead large object leaked forever.
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    // Regions large enough that the ~524 KB object occupies a SINGLE region;
    // the shared gc_config() uses 512-byte regions, which would make it a
    // multi-region large object (intentionally kept force-live by the fix).
    let mut cfg = gc_config();
    cfg.region_size = 2 * 1024 * 1024;
    cfg.heap_size = 16 * 1024 * 1024;
    cfg.heap_max = 32 * 1024 * 1024;
    cfg.nursery_size = 4 * 1024 * 1024;
    init_heap(&cfg).expect("init_heap");

    let before = egcl_rt::heap_stats().large_object_bytes;

    // A large (single-region) reference-free leaf, kept only in a raw pointer —
    // no EgclVal root — so the collector sees it as garbage.
    // Body > region_size/2 forces a dedicated LargeObject region (alloc_large);
    // < region_size keeps it to ONE region. 1.2 MiB in a 2 MiB region does both.
    let body_size = 1_200_000;
    let body = egcl_rt::gc::alloc_typed(body_size, type_id::SIMPLE_BASE_STRING)
        .expect("alloc large object");
    unsafe {
        *body = 0xCD;
    }
    let during = egcl_rt::heap_stats().large_object_bytes;
    assert!(
        during > before,
        "the large object should be accounted before GC (before={before}, during={during})"
    );

    // Force a full collection. Nothing references the object.
    full_gc().expect("full_gc");

    let after = egcl_rt::heap_stats().large_object_bytes;
    assert!(
        after < during,
        "a dead single-region large object was not reclaimed \
         (before={before}, during={during}, after={after})"
    );
    assert_eq!(
        after, before,
        "large-object accounting should return to baseline after the dead object is freed"
    );
}

#[test]
fn multi_region_large_object_is_swept_as_a_unit() {
    // bliss-hy5v: (a) a LIVE multi-region large object keeps its continuation
    // regions across a major GC — the old per-region free arm freed a
    // zero-live continuation out from under the live start, handing the
    // object's tail to the next allocation; and (b) once dead, the whole span
    // (start + continuations) is reclaimed together, closing the leak that the
    // interim force-live workaround left.
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = gc_config();
    cfg.region_size = 2 * 1024 * 1024;
    cfg.heap_size = 32 * 1024 * 1024;
    cfg.heap_max = 64 * 1024 * 1024;
    cfg.nursery_size = 4 * 1024 * 1024;
    init_heap(&cfg).expect("init_heap");

    let before = egcl_rt::heap_stats().large_object_bytes;
    // A 5 MiB body spans 3 consecutive 2 MiB regions: 1 start + 2 continuations.
    let body_size = 5 * 1024 * 1024;
    let body = egcl_rt::gc::alloc_typed(body_size, type_id::SIMPLE_BASE_STRING)
        .expect("alloc multi-region large object");
    // Stamp the first and LAST payload bytes; the last lives in the final
    // continuation region and detects tail reuse/zeroing.
    unsafe {
        *body.add(8) = 0xAB;
        *body.add(body_size - 1) = 0xCD;
    }
    let during = egcl_rt::heap_stats().large_object_bytes;
    assert!(during > before, "large object accounted after allocation");

    // (a) Rooted via the entry continuation (large payload offset is 16, so
    // the tagged value is body - 16), the object must survive a full GC with
    // its continuation-region tail intact.
    let saved = egcl_rt::gc::get_entry_continuation();
    egcl_rt::gc::set_entry_continuation(unsafe { EgclVal::from_heap_ptr(body.sub(16)) });
    full_gc().expect("full_gc (live)");
    let live_after = egcl_rt::heap_stats().large_object_bytes;
    assert_eq!(
        live_after, during,
        "a LIVE multi-region large object must stay fully accounted across a major GC"
    );
    unsafe {
        assert_eq!(*body.add(8), 0xAB, "head byte survived");
        assert_eq!(
            *body.add(body_size - 1),
            0xCD,
            "tail byte in the last continuation region survived"
        );
    }

    // (b) Unrooted, the whole span is reclaimed and accounting returns to
    // baseline (previously it force-lived forever).
    egcl_rt::gc::set_entry_continuation(saved);
    full_gc().expect("full_gc (dead)");
    let after = egcl_rt::heap_stats().large_object_bytes;
    assert_eq!(
        after, before,
        "a dead multi-region large object must be reclaimed whole \
         (before={before}, during={during}, after={after})"
    );
}

#[test]
fn pinned_large_object_survives_a_major_gc_with_no_references() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).expect("init_heap");

    // A "large" object exceeds the header's inline size field: body ≥ 0xFFFF
    // words. Allocate one PINNED and deliberately keep only a raw pointer to it
    // (no EgclVal root), so the collector marks it dead.
    let body_size = 0xFFFF * 8; // comfortably over the large-object threshold
    let body = egcl_rt::gc::alloc_pinned_typed(body_size, type_id::SIMPLE_BASE_STRING)
        .expect("alloc pinned large object");
    // Stamp a recognizable byte so we can confirm the payload is not zeroed.
    unsafe {
        *body = 0xAB;
    }
    // A large object's payload offset is 16 (LARGE_OBJECT_PAYLOAD_OFFSET), so
    // its header sits at body - 16, not body - 8.
    let header = unsafe { body.sub(16) };
    let type_before = unsafe { (*(header as *const ObjectHeader)).type_id() };
    assert_eq!(type_before, type_id::SIMPLE_BASE_STRING);
    assert!(
        egcl_rt::heap_stats().large_object_bytes > 0,
        "the pinned large object should be accounted before GC"
    );

    // Force a full (minor + major) collection. Nothing references the object.
    full_gc().expect("full_gc");

    // The region must be retained: header intact, payload not zeroed, and the
    // large-object accounting unchanged. Before the fix the region was freed
    // and zeroed here.
    let type_after = unsafe { (*(header as *const ObjectHeader)).type_id() };
    assert_eq!(
        type_after,
        type_id::SIMPLE_BASE_STRING,
        "a pinned large object's header was cleared — its region was wrongly freed"
    );
    assert_eq!(
        unsafe { *body },
        0xAB,
        "a pinned large object's payload was zeroed — its region was wrongly freed"
    );
    assert!(
        egcl_rt::heap_stats().large_object_bytes > 0,
        "a pinned large object was subtracted from large-object accounting — freed"
    );
}
