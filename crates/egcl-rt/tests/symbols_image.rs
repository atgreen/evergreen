// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::NIL;

#[test]
fn restored_symbol_registry_preserves_liveness_and_reclaims_dead_entries() {
    egcl_rt::rooted!(held = egcl_rt::symbols::make_uninterned("IMAGE-SURVIVOR"));
    let index = held.as_symbol_index();
    egcl_rt::symbols::set_symbol_value(index, *held);
    for _ in 0..100 {
        egcl_rt::symbols::make_uninterned("BEFORE-RESTORE");
    }
    egcl_rt::gc::full_gc().unwrap();
    let heap = egcl_rt::gc::serialize_heap_objects();
    let symbols = egcl_rt::gc::serialize_symbols();
    egcl_rt::gc::restore_heap(&heap).unwrap();
    egcl_rt::gc::restore_symbols(&symbols).unwrap();
    for _ in 0..100 {
        egcl_rt::symbols::make_uninterned("AFTER-RESTORE");
    }
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(egcl_rt::symbols::symbol_value(index), Some(*held));
    assert_eq!(
        egcl_rt::symbols::symbol_name(index).as_deref(),
        Some("IMAGE-SURVIVOR")
    );
    assert_eq!(egcl_rt::symbols::all_symbol_indices(), vec![index]);
    *held = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(egcl_rt::symbols::all_symbol_indices().is_empty());
}
