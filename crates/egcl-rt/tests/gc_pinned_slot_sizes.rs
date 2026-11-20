// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::NIL;

#[test]
fn reclaimed_pinned_slots_match_the_requested_body_size() {
    use egcl_rt::{function, gc, symbols};

    egcl_rt::rooted!(doomed_symbol = symbols::make_uninterned("DISCARDED"));
    let old_symbol = doomed_symbol.as_symbol_index();
    let symbol_address = symbols::symbol_object_ptr(old_symbol).unwrap();
    egcl_rt::rooted!(doomed_function = function::alloc_interpreted(NIL, NIL, NIL, *doomed_symbol));
    let function_address = doomed_function.to_raw();

    // Keep storage above both dead objects live so a major collection leaves
    // their slots in place instead of releasing the region or trimming its end.
    egcl_rt::rooted!(anchor_name = symbols::make_uninterned("ANCHOR"));
    egcl_rt::rooted!(anchor = function::alloc_interpreted(NIL, NIL, NIL, *anchor_name));
    let anchor_address = anchor.to_raw();
    *doomed_function = NIL;
    *doomed_symbol = NIL;
    gc::full_gc().unwrap();
    assert!(symbols::symbol_object_ptr(old_symbol).is_none());

    // The most recently freed slot of either size can differ from the next
    // requested size. Both allocations must reuse their own exact footprint.
    egcl_rt::rooted!(new_symbol = symbols::make_uninterned("REPLACEMENT"));
    let index = new_symbol.as_symbol_index();
    assert_eq!(symbols::symbol_object_ptr(index), Some(symbol_address));
    egcl_rt::rooted!(new_function = function::alloc_interpreted(NIL, NIL, NIL, *new_symbol));
    assert_eq!(new_function.to_raw(), function_address);
    gc::full_gc().unwrap();
    assert_eq!(anchor.to_raw(), anchor_address);
    assert_eq!(new_function.to_raw(), function_address);
    assert_eq!(symbols::symbol_name(index).as_deref(), Some("REPLACEMENT"));
}
