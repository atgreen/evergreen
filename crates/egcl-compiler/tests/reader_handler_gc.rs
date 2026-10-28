// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::reader;
use egcl_rt::value::NIL;
use egcl_rt::{Collector, HeapCollector};

#[test]
fn reader_tables_keep_anonymous_handlers_alive_across_collection() {
    egcl_rt::gc::ensure_heap_initialized();
    egcl_rt::rooted!(table = reader::make_readtable(None).unwrap());
    egcl_rt::rooted!(body = reader::read_from_string("(12345)").unwrap().0);
    let original_body = body.to_raw();
    // These are the pinned interpreted-function objects installed by Lisp's
    // SET-MACRO-CHARACTER, rather than a synthetic movable function layout.
    let (ordinary, dispatch, unregistered) = {
        egcl_rt::rooted!(ordinary = egcl_rt::function::alloc_interpreted(NIL, *body, NIL, NIL));
        egcl_rt::rooted!(dispatch = egcl_rt::function::alloc_interpreted(NIL, *body, NIL, NIL));
        let unregistered = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, NIL);
        reader::set_macro_character(*table, '!', *ordinary, false).unwrap();
        reader::set_dispatch_macro_character(*table, '#', '!', *dispatch).unwrap();
        (*ordinary, *dispatch, unregistered)
    };

    HeapCollector::new().minor_gc().unwrap();
    eprintln!(
        "handler body relocation: {original_body:#x} -> {:#x}",
        body.to_raw()
    );
    assert_ne!(
        body.to_raw(),
        original_body,
        "the handler body really moved"
    );
    assert_eq!(egcl_rt::function::body(ordinary), *body);
    assert_eq!(egcl_rt::function::body(dispatch), *body);

    // Only the readtable registrations retain these function objects. Rust
    // locals are deliberately not roots.
    HeapCollector::new().full_gc().unwrap();
    assert_ne!(
        egcl_rt::gc::heap_object_type_id(unregistered),
        Some(egcl_rt::object::type_id::FUNCTION_INTERPRETED),
        "major GC must reclaim the unregistered control"
    );
    let ordinary = reader::get_macro_character(*table, '!').unwrap().0.unwrap();
    let dispatch = reader::get_dispatch_macro_character(*table, '#', '!')
        .unwrap()
        .unwrap();
    assert!(
        egcl_rt::function::is_interpreted_function(ordinary),
        "ordinary reader handler was reclaimed"
    );
    assert!(
        egcl_rt::function::is_interpreted_function(dispatch),
        "dispatch reader handler was reclaimed"
    );
    assert_eq!(egcl_rt::function::body(ordinary), *body);
    assert_eq!(egcl_rt::function::body(dispatch), *body);
}
