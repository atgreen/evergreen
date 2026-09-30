use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use egcl_rt::WriteBarrier;
use egcl_rt::gc::{
    Allocator, Collector, GcConfig, HeapAllocator, HeapCollector, SatbCardBarrier, WeakPointer,
    full_gc, gc_marking_in_progress, get_entry_continuation, heap_stats, init_heap, record_object,
    register_finalizer, register_weak_pointer, set_entry_continuation, set_finalizer_dispatch,
    set_gc_marking_in_progress, walk_heap,
};
use egcl_rt::value::EgclVal;

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

fn init_test_heap() {
    init_heap(&gc_config()).expect("init_heap");
}

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn finalizer_log() -> &'static Mutex<Vec<(u64, u64)>> {
    static LOG: OnceLock<Mutex<Vec<(u64, u64)>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

static PANIC_IN_FINALIZER: AtomicBool = AtomicBool::new(false);

fn finalizer_dispatch(finalizer: EgclVal, object: EgclVal) {
    finalizer_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((finalizer.to_raw(), object.to_raw()));
    assert!(
        !PANIC_IN_FINALIZER.load(Ordering::SeqCst),
        "intentional finalizer panic"
    );
}

fn install_finalizer_dispatch() {
    set_finalizer_dispatch(finalizer_dispatch);
    finalizer_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    PANIC_IN_FINALIZER.store(false, Ordering::SeqCst);
}

fn heap_objects() -> Vec<(usize, u8, usize)> {
    let mut out = Vec::new();
    walk_heap(|ptr, type_id, size| {
        out.push((ptr as usize, type_id, size));
        true
    })
    .expect("walk_heap");
    out
}

#[test]
fn spec_gc_allocator_paths_and_stats_follow_configured_heap() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.01, R3.02, R3.03, R3.04, and R3.15, the runtime must expose
    // a generational heap with configurable TLAB-backed bump allocation and
    // a refill path when the TLAB is exhausted.
    init_test_heap();
    let mut allocator = HeapAllocator::new().expect("allocator");
    let first = allocator.alloc_fast(24).expect("fast alloc");
    let second = allocator.alloc_fast(24).expect("second fast alloc");
    assert_eq!(second as usize - first as usize, 32);

    while allocator.alloc_fast(24).is_some() {}
    let slow = allocator
        .alloc_slow(24)
        .expect("slow alloc after exhaustion");
    assert!(!slow.is_null());

    let stats = heap_stats();
    assert_eq!(stats.nursery_capacity, gc_config().nursery_size as u64);
    assert_eq!(
        stats.old_gen_capacity,
        (gc_config().heap_size - gc_config().nursery_size) as u64
    );
    assert!(stats.bytes_allocated >= 96);
    assert!(stats.nursery_used > 0);
}

#[test]
fn spec_gc_large_objects_minor_gc_and_full_gc_use_real_collector_paths() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.05, R3.06, R3.18, and R10.07, large objects must allocate in
    // dedicated old-gen regions, minor GC must collect nursery state, and
    // full_gc must drive the real collector entrypoints.
    init_test_heap();
    let mut allocator = HeapAllocator::new().expect("allocator");
    let _small = allocator.alloc_fast(24).expect("nursery object");
    let large = allocator.alloc_large(700).expect("large object");
    // Keep the large object reachable so it survives full_gc (unreferenced large
    // objects are now reclaimed, bliss-jg6g); this test checks it stays walkable
    // and in a dedicated region, not that it leaks.
    let saved = get_entry_continuation();
    set_entry_continuation(unsafe { EgclVal::from_heap_ptr(large.sub(8)) });

    let before = heap_stats();
    assert!(before.large_object_bytes >= 720);

    let mut collector = HeapCollector::new();
    collector.minor_gc().expect("minor_gc");
    let after_minor = heap_stats();
    assert_eq!(after_minor.minor_gc_count, before.minor_gc_count + 1);
    assert_eq!(after_minor.nursery_used, 0);

    full_gc().expect("full_gc");
    let after_full = heap_stats();
    assert!(after_full.minor_gc_count > after_minor.minor_gc_count);
    assert!(after_full.major_gc_count > after_minor.major_gc_count);

    let objects = heap_objects();
    assert!(
        objects
            .iter()
            .any(|(ptr, _, size)| *ptr == large as usize && *size == 700),
        "large object should remain walkable after collection"
    );
    set_entry_continuation(saved);
}

#[test]
fn spec_gc_barrier_logs_satb_and_remembered_set_state() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.09 and R3.11, old-gen marking uses a SATB barrier and
    // cross-generation pointers are tracked by a remembered-set/card barrier.
    init_test_heap();
    let barrier = SatbCardBarrier::new().expect("barrier");
    let mut slot = EgclVal::from_fixnum(0);

    set_gc_marking_in_progress(false);
    barrier.write_barrier(
        &mut slot,
        EgclVal::from_raw(0x1000),
        EgclVal::from_raw(0x2000),
    );
    assert!(barrier.drain_satb_buffer().is_empty());
    assert!(barrier.is_card_dirty(&mut slot as *mut _ as usize));

    barrier.clear_cards();
    set_gc_marking_in_progress(true);
    barrier.write_barrier(
        &mut slot,
        EgclVal::from_raw(0x3000),
        EgclVal::from_raw(0x4000),
    );
    let satb = barrier.drain_satb_buffer();
    assert_eq!(satb, vec![EgclVal::from_raw(0x3000)]);
    assert!(gc_marking_in_progress());
    set_gc_marking_in_progress(false);
}

#[test]
fn spec_gc_major_gc_runs_finalizers_and_survives_finalizer_panics() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.12, R3.16, and R10.07, unreachable objects with finalizers must
    // be finalized during GC and finalizer errors must not corrupt GC state.
    init_test_heap();
    install_finalizer_dispatch();

    record_object(0x41, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    let object_ptr = heap_objects()
        .into_iter()
        .find(|(_, type_id, _)| *type_id == 0x41)
        .map(|(ptr, _, _)| ptr)
        .expect("recorded object");
    let object = EgclVal::from_raw(object_ptr as u64);

    register_finalizer(object, EgclVal::from_fixnum(7)).expect("register_finalizer");
    let mut collector = HeapCollector::new();
    collector.major_gc().expect("major_gc with finalizer");

    let log = finalizer_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    // The object is small, so `record_object` placed it in the nursery; the
    // minor collection inside `major_gc` promotes (evacuates) it before it is
    // found dead. Its finalizer therefore fires exactly once, keyed on the
    // forwarded address rather than the stale pre-promotion one (egcl-jtc.7f).
    // Assert on the firing and the finalizer value, not the (moved) address.
    assert_eq!(log.len(), 1, "finalizer must fire exactly once");
    assert_eq!(
        log[0].0,
        EgclVal::from_fixnum(7).to_raw(),
        "the registered finalizer (7) must be the one invoked"
    );

    record_object(0x42, vec![9, 9, 9, 9, 9, 9, 9, 9]);
    let panicking_ptr = heap_objects()
        .into_iter()
        .find(|(_, type_id, _)| *type_id == 0x42)
        .map(|(ptr, _, _)| ptr)
        .expect("panicking object");
    let panicking_object = EgclVal::from_raw(panicking_ptr as u64);
    register_finalizer(panicking_object, EgclVal::from_fixnum(8)).expect("register_finalizer");

    PANIC_IN_FINALIZER.store(true, Ordering::SeqCst);
    collector
        .major_gc()
        .expect("major_gc should survive finalizer panic");
    PANIC_IN_FINALIZER.store(false, Ordering::SeqCst);
    assert!(heap_stats().major_gc_count >= 2);
}

#[test]
fn spec_gc_weak_references_break_atomically_for_dead_objects() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.13 and R10.07, weak references to unreachable objects are cleared
    // during GC, and multiple weak references to the same dead object break in
    // the same collection.
    init_test_heap();

    record_object(0x51, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    let object_ptr = heap_objects()
        .into_iter()
        .find(|(_, type_id, _)| *type_id == 0x51)
        .map(|(ptr, _, _)| ptr)
        .expect("weak target");
    let referent = EgclVal::from_raw(object_ptr as u64);

    let mut weak_a = WeakPointer::new(referent);
    let mut weak_b = WeakPointer::new(referent);
    register_weak_pointer(&mut weak_a);
    register_weak_pointer(&mut weak_b);

    HeapCollector::new().major_gc().expect("major_gc");

    assert_eq!(weak_a.value(), (egcl_rt::value::NIL, true));
    assert_eq!(weak_b.value(), (egcl_rt::value::NIL, true));
}

#[test]
fn spec_gc_large_object_is_not_moved_by_major_gc() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R3.19, large-object regions are never moved even when they survive
    // a major collection.
    init_test_heap();
    let mut allocator = HeapAllocator::new().expect("allocator");
    let ptr = allocator.alloc_large(700).expect("large object");
    unsafe {
        std::ptr::write_unaligned(ptr as *mut u64, ptr as u64);
    }
    // Root the object so it legitimately SURVIVES the collection (an
    // unreferenced large object is now reclaimed, bliss-jg6g); the point of this
    // test is that a surviving large object is not MOVED (R3.19). The value is
    // the tagged header pointer, body - OBJECT_HEADER_SIZE.
    let saved = get_entry_continuation();
    set_entry_continuation(unsafe { EgclVal::from_heap_ptr(ptr.sub(8)) });

    HeapCollector::new().major_gc().expect("major_gc");

    let objects = heap_objects();
    assert!(
        objects
            .iter()
            .any(|(obj_ptr, _, size)| *obj_ptr == ptr as usize && *size == 700),
        "surviving large object address changed across major GC"
    );
    set_entry_continuation(saved);
}
