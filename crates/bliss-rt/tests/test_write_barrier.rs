//! Authoritative generational write barrier + remembered set (bliss-jtc.21).
//!
//! Reference stores route through the shared `store_ref`/`write_barrier`
//! helpers, which record old→young references in the remembered set and log
//! SATB pre-write values. A minor GC consumes the remembered set to keep young
//! referents alive and relocate the old slots after the nursery moves.

use bliss_rt::gc::{
    drain_satb_log, init_heap, remembered_set_len, set_gc_marking_in_progress, store_ref,
    Allocator, Collector, GcConfig, HeapAllocator, HeapCollector,
};
use bliss_rt::value::{BlissVal, TAG_HEAP_OBJECT};
use std::sync::{Mutex, OnceLock};

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 256,
        region_size: 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

fn test_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// A heap-object reference in the runtime convention: the value points at the
/// object *header* (body − 8), matching `from_heap_ptr`; the first body word
/// (header + 8, i.e. the allocator's returned pointer) holds the test marker.
fn heap_ref(body: *mut u8) -> BlissVal {
    BlissVal(((body as u64) - 8) | TAG_HEAP_OBJECT)
}

#[test]
fn old_to_young_reference_survives_and_relocates_across_minor_gc() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();

    // A stable holder (large object — never moved by a minor GC) with one
    // reference slot at body[0]; and a young nursery object carrying a marker.
    let holder = alloc.alloc_large(64).unwrap();
    let young = alloc.alloc_fast(24).unwrap();
    const MARKER: u64 = 0x00A5_1234_5678_9ABC;
    unsafe { *(young as *mut u64) = MARKER };

    // Store the young pointer into the holder's slot THROUGH the barrier.
    let slot = holder as *mut BlissVal;
    unsafe { store_ref(slot, heap_ref(young)) };
    assert_eq!(remembered_set_len(), 1, "barrier recorded the old→young slot");

    // Minor GC moves the young object; the remembered set must keep it alive and
    // relocate the holder's slot to the new location.
    HeapCollector::new().minor_gc().unwrap();

    let relocated = unsafe { *slot };
    let new_header = (relocated.0 & !0b111) as usize;
    assert_ne!(new_header, young as usize - 8, "young object was evacuated (moved)");
    assert_eq!(
        unsafe { *((new_header + 8) as *const u64) },
        MARKER,
        "young referent survived with intact contents at its new location"
    );
    assert_eq!(remembered_set_len(), 0, "remembered set cleared after minor GC");
}

#[test]
fn immediate_stores_do_not_grow_the_remembered_set() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();
    let holder = alloc.alloc_large(16).unwrap();
    let slot = holder as *mut BlissVal;

    unsafe { store_ref(slot, BlissVal::from_fixnum(42)) };
    assert_eq!(remembered_set_len(), 0, "storing a fixnum records no reference");
    assert_eq!(unsafe { *slot }, BlissVal::from_fixnum(42));
}

#[test]
fn satb_pre_write_logging_captures_overwritten_references_during_marking() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();
    let holder = alloc.alloc_large(16).unwrap();
    let slot = holder as *mut BlissVal;
    let old = heap_ref(alloc.alloc_fast(16).unwrap());

    // Not marking: the barrier logs nothing to the SATB buffer.
    unsafe { *slot = old };
    set_gc_marking_in_progress(false);
    unsafe { store_ref(slot, BlissVal::from_fixnum(1)) };
    assert!(drain_satb_log().is_empty(), "no SATB logging outside marking");

    // Marking active: the overwritten (pre-write) reference is logged.
    unsafe { *slot = old };
    set_gc_marking_in_progress(true);
    unsafe { store_ref(slot, BlissVal::from_fixnum(2)) };
    let logged = drain_satb_log();
    set_gc_marking_in_progress(false);
    assert_eq!(logged, vec![old], "SATB logs the overwritten reference");
}

/// A nursery object reachable ONLY from a CL-stack frame survives a minor GC and
/// its frame slot is relocated to the moved object (bliss-jtc.17 completes the
/// young-object root set for minor GC — previously only major GC relocated
/// frame refs).
#[test]
fn nursery_object_held_only_by_cl_frame_relocates_across_minor_gc() {
    use bliss_rt::{current_thread, BlissStack};
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();

    let young = alloc.alloc_fast(24).unwrap();
    const MARKER: u64 = 0x0055_1122_3344;
    unsafe { *(young as *mut u64) = MARKER };

    // Hold `young` only from a frame on the current thread's CL stack.
    let stack = current_thread().stack();
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { BlissStack::frame_slots_mut(f)[0] = heap_ref(young) };

    HeapCollector::new().minor_gc().unwrap();

    let relocated = unsafe { BlissStack::frame_slots_mut(f)[0] };
    let new_header = (relocated.0 & !0b111) as usize;
    assert_ne!(new_header, young as usize - 8, "young object was moved by minor GC");
    assert_eq!(
        unsafe { *((new_header + 8) as *const u64) },
        MARKER,
        "frame-held young object survived with intact contents at its new location"
    );

    stack.pop_frame();
}
