// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::EgclVal;
use egcl_rt::{GcConfig, init_heap};

#[test]
fn repeated_full_collections_do_not_consume_empty_regions() {
    init_heap(&GcConfig {
        heap_size: 64 * 1024,
        heap_max: 64 * 1024,
        nursery_size: 8192,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    })
    .unwrap();
    let payload = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe { *(payload as *mut f64) = 42.0 };
    egcl_rt::rooted!(value = unsafe { EgclVal::from_heap_ptr(payload.sub(8)) });
    egcl_rt::gc::full_gc().unwrap();
    let free = egcl_rt::heap_stats().regions_free;
    for _ in 0..30 {
        egcl_rt::gc::full_gc().unwrap();
        assert_eq!(egcl_rt::heap_stats().regions_free, free);
        assert_eq!(unsafe { *(value.as_ptr().add(8) as *const f64) }, 42.0);
    }
}
