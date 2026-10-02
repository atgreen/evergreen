// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::NIL;
use egcl_rt::{init_heap, GcConfig};

#[test]
fn discarded_symbols_reuse_storage_and_surviving_names_move() {
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
    egcl_rt::rooted!(held = egcl_rt::symbols::make_uninterned("SURVIVOR"));
    let index = held.as_symbol_index();
    let object = egcl_rt::symbols::symbol_object_ptr(index).unwrap();
    // The symbol is pinned, but its name must be traced and relocated.
    let original_name = unsafe { (*(object as *const egcl_rt::object::SymbolData)).name };
    for _ in 0..100 {
        for _ in 0..100 {
            egcl_rt::symbols::make_uninterned("DISCARDED");
        }
        egcl_rt::gc::full_gc().unwrap();
        assert_eq!(egcl_rt::symbols::symbol_object_ptr(index), Some(object));
        assert_eq!(
            egcl_rt::symbols::symbol_name(index).as_deref(),
            Some("SURVIVOR")
        );
        assert_eq!(egcl_rt::symbols::all_symbol_indices(), vec![index]);
    }
    egcl_rt::rooted!(name = unsafe { (*(object as *const egcl_rt::object::SymbolData)).name });
    assert_ne!(*name, original_name, "the retained name must actually move");
    *held = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(egcl_rt::symbols::symbol_name(index).is_none());
    assert_eq!(name.as_string(), "SURVIVOR");
}
