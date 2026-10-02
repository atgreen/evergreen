// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::{EgclVal, NIL};
use egcl_rt::{GcConfig, init_heap};

#[test]
fn function_allocation_collects_before_reporting_exhaustion() {
    init_heap(&GcConfig {
        heap_size: 64 * 1024,
        heap_max: 64 * 1024,
        nursery_size: 8 * 1024,
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
    let body = unsafe { EgclVal::from_heap_ptr(payload.sub(8)) };
    let original_body = body.to_raw();
    egcl_rt::rooted!(survivor = egcl_rt::function::alloc_interpreted(NIL, body, NIL, NIL));
    let survivor_address = survivor.to_raw();
    for _ in 0..100 {
        for _ in 0..100 {
            let function = egcl_rt::function::alloc_interpreted(
                NIL,
                egcl_rt::function::body(*survivor),
                NIL,
                NIL,
            );
            assert_eq!(
                egcl_rt::function::body(function),
                egcl_rt::function::body(*survivor)
            );
        }
        assert_eq!(survivor.to_raw(), survivor_address);
        let body = egcl_rt::function::body(*survivor);
        assert_eq!(unsafe { *(body.as_ptr().add(8) as *const f64) }, 42.0);
    }
    assert!(egcl_rt::heap_stats().major_gc_count > 0);
    assert_ne!(egcl_rt::function::body(*survivor).to_raw(), original_body);
}
