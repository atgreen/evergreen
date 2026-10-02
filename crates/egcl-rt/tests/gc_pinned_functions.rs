// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::{EgclVal, NIL};
use egcl_rt::{GcConfig, init_heap};

#[test]
fn discarded_functions_reuse_storage_beside_live_pinned_functions() {
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
            egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL);
        }
        egcl_rt::gc::full_gc().unwrap();
        assert_eq!(survivor.to_raw(), survivor_address);
        let body = egcl_rt::function::body(*survivor);
        assert_ne!(
            body.to_raw(),
            original_body,
            "the function must trace a moving body"
        );
        assert_eq!(unsafe { *(body.as_ptr().add(8) as *const f64) }, 42.0);
    }
    // Restore into the same heap after building a free-slot pool. No address
    // from that pool may overwrite an object in the restored layout.
    let saved = egcl_rt::gc::serialize_heap_objects();
    let old_survivor = survivor.to_raw();
    egcl_rt::gc::restore_heap(&saved).unwrap();
    *survivor = EgclVal::from_raw(egcl_rt::gc::remap_saved_pointer(old_survivor));
    for _ in 0..100 {
        egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL);
    }
    egcl_rt::gc::full_gc().unwrap();
    let body = egcl_rt::function::body(*survivor);
    assert_eq!(unsafe { *(body.as_ptr().add(8) as *const f64) }, 42.0);
}
