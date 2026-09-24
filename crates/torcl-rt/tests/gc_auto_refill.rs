//! Automatic nursery collection on the allocator slow path (bliss-5tb).

use std::sync::{Mutex, OnceLock};
use torcl_rt::value::{TAG_HEAP_OBJECT, TAG_MASK, TorclVal};
use torcl_rt::{
    Allocator, GcConfig, HeapAllocator, ShadowRootScope, TorclStack, alloc_typed, current_thread,
    heap_stats, init_heap,
};

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn config() -> GcConfig {
    GcConfig {
        heap_size: 64 * 1024,
        heap_max: 128 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

fn allocate(allocator: &mut HeapAllocator) -> *mut u8 {
    allocator
        .alloc_fast(16)
        .or_else(|| allocator.alloc_slow(16).ok())
        .expect("allocation should collect and retry instead of exhausting the nursery")
}

#[test]
fn slow_path_collects_and_reuses_the_configured_nursery() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    // This is several times the configured 8 KiB nursery. Every object is
    // unreachable, so each automatic minor collection can reuse the nursery.
    for _ in 0..1_000 {
        let _ = allocate(&mut allocator);
    }

    let stats = heap_stats();
    assert!(
        stats.minor_gc_count >= 3,
        "allocation beyond the nursery budget must trigger repeated minor collections"
    );
    assert!(
        stats.regions_free >= 12,
        "free old-generation reserve must not be silently converted into nursery regions"
    );
}

#[test]
fn automatic_collection_relocates_a_stack_root() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    let rooted_body = allocate(&mut allocator);
    const MARKER: u64 = 0x0055_AA11_22BB_33CC;
    unsafe { *(rooted_body as *mut u64) = MARKER };
    let rooted = TorclVal(((rooted_body as u64) - 8) | TAG_HEAP_OBJECT);

    let stack = current_thread().stack();
    let frame = stack
        .push_frame(TorclVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .expect("root frame");
    unsafe { TorclStack::frame_slots_mut(frame)[0] = rooted };

    for _ in 0..600 {
        let _ = allocate(&mut allocator);
    }

    let relocated = unsafe { TorclStack::frame_slots_mut(frame)[0] };
    let relocated_header = (relocated.0 & !TAG_MASK) as usize;
    assert_ne!(
        relocated_header,
        (rooted.0 & !TAG_MASK) as usize,
        "the rooted nursery object should have moved"
    );
    assert_eq!(
        unsafe { *((relocated_header + 8) as *const u64) },
        MARKER,
        "automatic collection must preserve rooted contents"
    );

    stack.pop_frame();
}

#[test]
fn collection_walks_past_retired_tlab_filler() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");

    // Leave an 80-byte tail in the first 256-byte TLAB, then request an object
    // whose 96-byte footprint forces a refill. The retired tail must become a
    // filler; otherwise the collector stops at its zero header and never sees
    // this rooted object in the following TLAB.
    allocator.alloc_fast(160).expect("first TLAB allocation");
    let rooted_body = allocator.alloc_slow(80).expect("refilled allocation");
    const MARKER: u64 = 0x0033_4455_6677_8899;
    unsafe { *(rooted_body as *mut u64) = MARKER };
    let roots = ShadowRootScope::new();
    let root = roots.root(TorclVal(
        ((rooted_body as u64) - std::mem::size_of::<u64>() as u64) | TAG_HEAP_OBJECT,
    ));

    for _ in 0..600 {
        let _ = allocate(&mut allocator);
    }

    let relocated_header = (root.get().0 & !TAG_MASK) as usize;
    assert_eq!(
        unsafe { *((relocated_header + 8) as *const u64) },
        MARKER,
        "a live object beyond a retired TLAB tail must be marked and relocated"
    );
}

#[test]
fn t0_alloc_typed_collects_and_preserves_shadow_roots() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");

    let body = alloc_typed(16, torcl_rt::object::type_id::BIGNUM).expect("root allocation");
    const MARKER: u64 = 0x0011_2233_4455_6677;
    unsafe { *(body as *mut u64) = MARKER };
    let roots = ShadowRootScope::new();
    let root = roots.root(unsafe { TorclVal::from_heap_ptr(body.sub(8)) });

    for _ in 0..1_000 {
        alloc_typed(48, torcl_rt::object::type_id::BIGNUM)
            .expect("T0 allocation should collect and retry");
    }

    let stats = heap_stats();
    assert!(
        stats.minor_gc_count >= 3,
        "alloc_typed must reuse the nursery through repeated minor collections"
    );
    assert_eq!(
        unsafe { *((root.get().as_ptr().add(8)) as *const u64) },
        MARKER,
        "a T0 shadow root must retain its contents across automatic collections"
    );
}
