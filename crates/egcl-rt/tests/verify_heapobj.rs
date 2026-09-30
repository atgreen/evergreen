// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Verify whether a TAG_HEAP_OBJECT value (EgclVal → header) survives a moving GC.
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{TAG_HEAP_OBJECT, TAG_MASK, EgclVal};
use egcl_rt::{Allocator, Collector, GcConfig, EgclStack, current_thread, init_heap};
use egcl_rt::{HeapAllocator, HeapCollector};

fn cfg() -> GcConfig {
    GcConfig {
        heap_size: 4 << 20,
        heap_max: 8 << 20,
        nursery_size: 4 << 20,
        tlab_size: 4096,
        region_size: 8192,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}

#[test]
fn heap_object_via_from_heap_ptr_survives_minor_gc() {
    init_heap(&cfg()).unwrap();
    let mut a = HeapAllocator::new().unwrap();
    // A RATIO-shaped heap object: body [numerator, denominator]. EgclVal points
    // at the object header (like strings/instances/ratios do).
    let body = a.alloc_fast(16).or_else(|| a.alloc_slow(16).ok()).unwrap();
    unsafe {
        let hdr = body.sub(8) as *mut ObjectHeader;
        (*hdr).0 = ((*hdr).0 & 0x00FF_FFFF_FFFF_FFFF) | ((type_id::RATIO as u64) << 56);
        *(body as *mut u64) = 0xABCD; // marker in first body word
    }
    let header_ptr = unsafe { body.sub(8) } as u64;
    let v = EgclVal(header_ptr | TAG_HEAP_OBJECT); // v → header (heap-obj convention)

    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe {
        EgclStack::frame_slots_mut(f)[0] = v;
    }

    HeapCollector::new().minor_gc().unwrap();

    let after = unsafe { EgclStack::frame_slots_mut(f)[0] };
    let moved = (after.0 & !TAG_MASK) != (v.0 & !TAG_MASK);
    // If relocation is correct, `after` points at the moved object's header, and
    // header+8 holds the marker.
    let content = unsafe { *(((after.0 & !TAG_MASK) + 8) as *const u64) };
    eprintln!(
        "HEAPOBJ: moved={moved} after=0x{:x} content=0x{content:x}",
        after.0 & !TAG_MASK
    );
    assert_eq!(
        content, 0xABCD,
        "heap object content must survive + relocate correctly"
    );
    stack.pop_frame();
}
