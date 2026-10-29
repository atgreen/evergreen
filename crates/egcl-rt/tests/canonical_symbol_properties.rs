// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{
    EgclVal, gc, symbols,
    value::{NIL, T},
};

fn cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
    egcl_rt::rooted!(car = car);
    egcl_rt::rooted!(cdr = cdr);
    let body = egcl_rt::alloc_typed(16, egcl_rt::object::type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(*car);
        body.cast::<EgclVal>().add(1).write(*cdr);
        EgclVal::from_cons_ptr(body)
    }
}

fn car(value: EgclVal) -> EgclVal {
    assert!(value.is_cons());
    unsafe { value.as_ptr().cast::<EgclVal>().read() }
}

#[test]
fn canonical_property_cells_relocate_and_survive_registry_image_restoration() {
    assert_eq!(symbols::symbol_property_index(NIL), None);
    assert_eq!(symbols::symbol_property_index(T), None);
    let nil_index = symbols::intern("NIL");
    let t_index = symbols::intern("T");
    let ordinary_index = symbols::intern("OTHER::T");
    assert_eq!(symbols::symbol_property_index(NIL), Some(nil_index));
    assert_eq!(symbols::symbol_property_index(T), Some(t_index));
    assert_eq!(
        symbols::symbol_property_index(EgclVal::from_symbol_index(ordinary_index)),
        Some(ordinary_index)
    );
    assert_eq!(
        symbols::symbol_property_index(EgclVal::from_fixnum(0)),
        None
    );
    // No pinned allocations share the nursery regions containing our values.
    gc::collect_t0_minor().unwrap();
    let mut before = Vec::new();
    for (index, datum) in [(nil_index, 41), (t_index, 42)] {
        let plist = {
            egcl_rt::rooted!(value = cons(EgclVal::from_fixnum(datum), NIL));
            egcl_rt::rooted!(tail = cons(*value, NIL));
            cons(T, *tail)
        };
        symbols::set_symbol_plist(index, plist);
        before.push((
            plist,
            car(unsafe { plist.as_ptr().cast::<EgclVal>().add(1).read() }),
        ));
    }
    // Only the registry's property cells own these lists and their values.
    gc::collect_t0_minor().unwrap();
    for ((symbol, datum), (old_plist, old_value)) in [(NIL, 41), (T, 42)].into_iter().zip(before) {
        let index = symbols::symbol_property_index(symbol).unwrap();
        let plist = symbols::symbol_plist(index).unwrap();
        let value = car(unsafe { plist.as_ptr().cast::<EgclVal>().add(1).read() });
        assert_ne!(plist, old_plist, "property list must actually relocate");
        assert_ne!(value, old_value, "property value must actually relocate");
        assert_eq!(car(value), EgclVal::from_fixnum(datum));
    }
    let heap = gc::serialize_heap_objects();
    let registry = gc::serialize_symbols();
    symbols::set_symbol_plist(nil_index, NIL);
    symbols::set_symbol_plist(t_index, NIL);
    gc::restore_heap(&heap).unwrap();
    gc::restore_symbols(&registry).unwrap();
    gc::full_gc().unwrap();
    for (symbol, datum) in [(NIL, 41), (T, 42)] {
        let plist = symbols::symbol_plist(symbols::symbol_property_index(symbol).unwrap()).unwrap();
        assert_eq!(car(plist), T);
        let value = car(unsafe { plist.as_ptr().cast::<EgclVal>().add(1).read() });
        assert_eq!(car(value), EgclVal::from_fixnum(datum));
    }
}
