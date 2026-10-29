// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::{EgclError, EgclVal, value::NIL};
use egcl_stdlib::clos::*;

fn symbol(name: &str) -> EgclVal {
    EgclVal::from_symbol_index(egcl_rt::symbols::intern(name))
}

#[test]
fn primary_name_mutation_keeps_registry_layout_and_class_graph_unchanged() {
    ensure_clos_bootstrapped().unwrap();
    let original = symbol("NAME-WRITER-ORIGINAL");
    let alias = symbol("NAME-WRITER-ALIAS");
    let moved = symbol("NAME-WRITER-MOVED");
    let slot = symbol("NAME-WRITER-SLOT");
    let metaclass = find_class(symbol("STANDARD-CLASS")).unwrap();
    egcl_rt::rooted!(class = new_class_metaobject(metaclass).unwrap());
    define_class(original, *class, &[], &[slot]).unwrap();
    bind_class_name(alias, *class).unwrap();
    egcl_rt::rooted!(instance = allocate_instance(*class).unwrap());
    set_slot_value(*instance, slot, EgclVal::from_fixnum(19)).unwrap();
    let layout = instance_layout_key(*instance);
    let cpl = compute_class_precedence_list(*class).unwrap();
    assert_eq!(set_class_name(*class, moved).unwrap(), moved);
    assert!(!update_class_name(*class, moved).unwrap());
    assert_eq!(find_class(original), Some(*class));
    assert_eq!(find_class(alias), Some(*class));
    assert_eq!(find_class(moved), None);
    assert_eq!(class_name(*class), moved);
    assert_eq!(compute_class_precedence_list(*class).unwrap(), cpl);
    assert_eq!(instance_layout_key(*instance), layout);
    assert_eq!(
        slot_value(*instance, slot).unwrap(),
        EgclVal::from_fixnum(19)
    );
    assert_eq!(set_class_name(*class, NIL).unwrap(), NIL);
    assert_eq!(class_name(*class), NIL);
    assert_eq!(find_class(original), Some(*class));
    assert!(matches!(
        set_class_name(*class, EgclVal::from_fixnum(3)),
        Err(EgclError::TypeError { .. })
    ));
    assert!(matches!(
        set_class_name(EgclVal::from_fixnum(3), moved),
        Err(EgclError::TypeError { .. })
    ));
    assert_eq!(class_name(*class), NIL);
    let integer = find_class(symbol("INTEGER")).unwrap();
    assert_eq!(
        set_class_name(integer, symbol("INTEGER")).unwrap(),
        symbol("INTEGER")
    );
    assert!(matches!(
        set_class_name(integer, moved),
        Err(EgclError::ProgramError(_))
    ));
    assert_eq!(class_name(integer), symbol("INTEGER"));
    // The class parameter is not restricted to instances of STANDARD-CLASS.
    // User structure-class objects remain supported without touching a layout.
    set_structure_class(*class);
    assert_eq!(set_class_name(*class, moved).unwrap(), moved);
    assert_eq!(instance_layout_key(*instance), layout);
}
