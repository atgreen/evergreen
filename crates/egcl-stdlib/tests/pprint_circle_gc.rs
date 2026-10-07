// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::object::{type_id, ConsCell};
use egcl_rt::value::{EgclVal, T};
use egcl_stdlib::format::{circle_active, circle_enter, circle_exit, circle_visit, CircleMark};

fn cycle() -> EgclVal {
    let body = egcl_rt::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        let value = EgclVal::from_cons_ptr(body);
        let cell = body as *mut ConsCell;
        (*cell).car = EgclVal::from_fixnum(1);
        (*cell).cdr = value;
        value
    }
}

#[test]
fn nested_circle_tables_relocate_and_restore_the_enclosing_graph() {
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 16 * 1024 * 1024,
        heap_max: 64 * 1024 * 1024,
        nursery_size: 64 * 1024,
        tlab_size: 4096,
        region_size: 4096,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    let control = egcl_rt::symbols::intern("*PRINT-CIRCLE*");
    egcl_rt::symbols::set_symbol_value(control, T);
    egcl_rt::rooted!(outer = cycle());
    egcl_rt::rooted!(inner = cycle());
    let outer_before = *outer;
    let inner_before = *inner;
    circle_enter(*outer);
    assert!(matches!(circle_visit(*outer), CircleMark::First(1)));
    circle_enter(*outer);
    assert!(matches!(circle_visit(*outer), CircleMark::Repeat(1)));
    circle_exit();
    assert!(circle_active());
    circle_enter(*inner);
    assert!(matches!(circle_visit(*inner), CircleMark::First(2)));
    egcl_rt::collect_t0_minor().unwrap();
    assert_ne!(*outer, outer_before, "outer graph must actually relocate");
    assert_ne!(*inner, inner_before, "inner graph must actually relocate");
    assert!(matches!(circle_visit(*inner), CircleMark::Repeat(2)));
    circle_exit();
    assert!(matches!(circle_visit(*outer), CircleMark::Repeat(1)));
    circle_exit();
    assert!(!circle_active());
    circle_exit();
    assert!(!circle_active());
}
