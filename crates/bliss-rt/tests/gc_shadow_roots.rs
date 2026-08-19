//! Precise host-language shadow roots for the tree-walker (bliss-6b2.1).

use bliss_rt::value::{BlissVal, TAG_MASK};
use bliss_rt::{
    Allocator, Collector, GcConfig, HeapAllocator, HeapCollector, ShadowRootScope, alloc_typed,
    full_gc, init_heap, walk_heap,
};
use std::sync::{Mutex, OnceLock};

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

fn allocate(allocator: &mut HeapAllocator, marker: u64) -> BlissVal {
    let body = allocator
        .alloc_fast(16)
        .or_else(|| allocator.alloc_slow(16).ok())
        .expect("allocate probe");
    unsafe {
        *(body as *mut u64) = marker;
        BlissVal::from_heap_ptr(body.sub(8))
    }
}

fn marker(value: BlissVal) -> u64 {
    let header = (value.to_raw() & !TAG_MASK) as *const u8;
    unsafe { *(header.add(8) as *const u64) }
}

fn allocate_t0(marker: u64) -> BlissVal {
    let body = alloc_typed(16, bliss_rt::object::type_id::BIGNUM).expect("allocate T0 probe");
    unsafe {
        *(body as *mut u64) = marker;
        BlissVal::from_heap_ptr(body.sub(8))
    }
}

#[test]
fn scalar_and_vector_temporaries_are_rewritten_after_minor_gc() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");
    let mut allocator = HeapAllocator::new().expect("allocator");
    let scope = ShadowRootScope::new();

    let original_scalar = allocate(&mut allocator, 0xA11C_E001);
    let scalar = scope.root(original_scalar);
    let originals = [
        allocate(&mut allocator, 0xA11C_E002),
        allocate(&mut allocator, 0xA11C_E003),
        allocate(&mut allocator, 0xA11C_E004),
    ];
    let values = scope.root_values(originals);

    HeapCollector::new().minor_gc().expect("minor_gc");

    assert_ne!(
        scalar.get(),
        original_scalar,
        "scalar root should be relocated"
    );
    assert_eq!(marker(scalar.get()), 0xA11C_E001);
    for (index, root) in values.iter().enumerate() {
        assert_ne!(
            root.get(),
            originals[index],
            "vector root should be relocated"
        );
        assert_eq!(marker(root.get()), 0xA11C_E002 + index as u64);
    }
}

#[test]
fn t0_reacquires_a_walkable_tlab_after_explicit_full_gc() {
    let _guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    init_heap(&config()).expect("init_heap");

    let _dead = allocate_t0(0xA11C_F001);
    full_gc().expect("first full_gc");

    let second = allocate_t0(0xA11C_F002);
    let second_body = unsafe { second.as_ptr() as usize + 8 };
    let mut found = false;
    walk_heap(|body, type_id, _| {
        found |= body as usize == second_body && type_id == bliss_rt::object::type_id::BIGNUM;
        true
    })
    .expect("walk post-GC T0 allocation");
    assert!(found, "post-GC T0 allocation must be on the walkable heap frontier");

    let roots = ShadowRootScope::new();
    let second = roots.root(second);
    full_gc().expect("second full_gc");
    assert_eq!(marker(second.get()), 0xA11C_F002);
}
