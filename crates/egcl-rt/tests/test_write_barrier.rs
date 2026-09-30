//! Authoritative generational write barrier + remembered set (bliss-jtc.21).
//!
//! Reference stores route through the shared `store_ref`/`write_barrier`
//! helpers, which record old→young references in the remembered set and log
//! SATB pre-write values. A minor GC consumes the remembered set to keep young
//! referents alive and relocate the old slots after the nursery moves.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use egcl_rt::gc::{
    Allocator, Collector, GcConfig, HeapAllocator, HeapCollector, drain_satb_log, init_heap,
    remembered_set_len, set_gc_marking_in_progress, store_ref,
};
use egcl_rt::value::{T, TAG_HEAP_OBJECT, EgclVal};
use egcl_rt::{
    FiberState, SchedulerConfig, SchedulerGroup, EgclStack, current_stack, fiber_state,
    make_fiber, park_current_fiber,
};

static PARKED_ROOT_INPUT: AtomicU64 = AtomicU64::new(0);
static PARKED_ROOT_OUTPUT: AtomicU64 = AtomicU64::new(0);
static PARKED_ROOT_READY: AtomicUsize = AtomicUsize::new(0);

fn fiber_with_parked_stack_root() -> EgclVal {
    let stack = current_stack();
    let frame = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .expect("fiber CL stack frame");
    unsafe {
        EgclStack::frame_slots_mut(frame)[0] = EgclVal(PARKED_ROOT_INPUT.load(Ordering::Acquire));
    }
    PARKED_ROOT_READY.store(1, Ordering::Release);
    park_current_fiber().expect("park fiber with published CL stack root");
    let relocated = unsafe { EgclStack::frame_slots_mut(frame)[0] };
    PARKED_ROOT_OUTPUT.store(relocated.0, Ordering::Release);
    stack.pop_frame();
    T
}

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
fn heap_ref(body: *mut u8) -> EgclVal {
    EgclVal(((body as u64) - 8) | TAG_HEAP_OBJECT)
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
    let slot = holder as *mut EgclVal;
    unsafe { store_ref(slot, heap_ref(young)) };
    assert_eq!(
        remembered_set_len(),
        1,
        "barrier recorded the old→young slot"
    );

    // Minor GC moves the young object; the remembered set must keep it alive and
    // relocate the holder's slot to the new location.
    HeapCollector::new().minor_gc().unwrap();

    let relocated = unsafe { *slot };
    let new_header = (relocated.0 & !0b111) as usize;
    assert_ne!(
        new_header,
        young as usize - 8,
        "young object was evacuated (moved)"
    );
    assert_eq!(
        unsafe { *((new_header + 8) as *const u64) },
        MARKER,
        "young referent survived with intact contents at its new location"
    );
    assert_eq!(
        remembered_set_len(),
        0,
        "remembered set cleared after minor GC"
    );
}

#[test]
fn immediate_stores_do_not_grow_the_remembered_set() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();
    let holder = alloc.alloc_large(16).unwrap();
    let slot = holder as *mut EgclVal;

    unsafe { store_ref(slot, EgclVal::from_fixnum(42)) };
    assert_eq!(
        remembered_set_len(),
        0,
        "storing a fixnum records no reference"
    );
    assert_eq!(unsafe { *slot }, EgclVal::from_fixnum(42));
}

#[test]
fn satb_pre_write_logging_captures_overwritten_references_during_marking() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();
    let holder = alloc.alloc_large(16).unwrap();
    let slot = holder as *mut EgclVal;
    let old = heap_ref(alloc.alloc_fast(16).unwrap());

    // Not marking: the barrier logs nothing to the SATB buffer.
    unsafe { *slot = old };
    set_gc_marking_in_progress(false);
    unsafe { store_ref(slot, EgclVal::from_fixnum(1)) };
    assert!(
        drain_satb_log().is_empty(),
        "no SATB logging outside marking"
    );

    // Marking active: the overwritten (pre-write) reference is logged.
    unsafe { *slot = old };
    set_gc_marking_in_progress(true);
    unsafe { store_ref(slot, EgclVal::from_fixnum(2)) };
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
    use egcl_rt::{EgclStack, current_thread};
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();

    let young = alloc.alloc_fast(24).unwrap();
    const MARKER: u64 = 0x0055_1122_3344;
    unsafe { *(young as *mut u64) = MARKER };

    // Hold `young` only from a frame on the current thread's CL stack.
    let stack = current_thread().stack();
    let f = stack
        .push_frame(EgclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    unsafe { EgclStack::frame_slots_mut(f)[0] = heap_ref(young) };

    HeapCollector::new().minor_gc().unwrap();

    let relocated = unsafe { EgclStack::frame_slots_mut(f)[0] };
    let new_header = (relocated.0 & !0b111) as usize;
    assert_ne!(
        new_header,
        young as usize - 8,
        "young object was moved by minor GC"
    );
    assert_eq!(
        unsafe { *((new_header + 8) as *const u64) },
        MARKER,
        "frame-held young object survived with intact contents at its new location"
    );

    stack.pop_frame();
}

/// A parked fiber is not participating in the GC handshake, so its published
/// managed stack must keep roots alive and receive evacuation updates.  This
/// also proves that the resumed continuation observes the rewritten frame.
#[test]
fn nursery_root_relocates_across_fiber_park_and_resume() {
    let _g = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&gc_config()).unwrap();
    let mut alloc = HeapAllocator::new().unwrap();

    let young = alloc.alloc_fast(24).unwrap();
    const MARKER: u64 = 0x0066_7788_99AA;
    unsafe { *(young as *mut u64) = MARKER };
    let original = heap_ref(young);
    PARKED_ROOT_INPUT.store(original.0, Ordering::Release);
    PARKED_ROOT_OUTPUT.store(0, Ordering::Release);
    PARKED_ROOT_READY.store(0, Ordering::Release);

    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let fiber = make_fiber(unsafe {
        EgclVal::from_function_ptr(fiber_with_parked_stack_root as *const () as *mut u8)
    })
    .unwrap();
    group.submit(fiber).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while (PARKED_ROOT_READY.load(Ordering::Acquire) == 0
        || fiber_state(fiber) != Some(FiberState::Blocked))
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(PARKED_ROOT_READY.load(Ordering::Acquire), 1);
    assert_eq!(fiber_state(fiber), Some(FiberState::Blocked));

    HeapCollector::new().minor_gc().unwrap();
    group.unpark(fiber).unwrap();
    assert_eq!(group.finish().unwrap(), vec![T]);

    let relocated = EgclVal(PARKED_ROOT_OUTPUT.load(Ordering::Acquire));
    let new_header = (relocated.0 & !0b111) as usize;
    assert_ne!(
        new_header,
        original.0 as usize & !0b111,
        "parked fiber root was relocated"
    );
    assert_eq!(
        unsafe { *((new_header + 8) as *const u64) },
        MARKER,
        "resumed fiber observed the relocated object with intact contents"
    );
}
