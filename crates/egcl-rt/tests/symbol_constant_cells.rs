// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclError, EgclVal, symbols};

#[test]
fn constant_cells_preserve_identity_dynamic_bindings_and_heap_images() {
    egcl_rt::rooted!(constant = symbols::make_uninterned("SAME-NAME"));
    egcl_rt::rooted!(ordinary = symbols::make_uninterned("SAME-NAME"));
    let idx = constant.as_symbol_index();
    let other = ordinary.as_symbol_index();
    let seven = EgclVal::from_fixnum(7);
    let nine = EgclVal::from_fixnum(9);

    symbols::set_symbol_value_checked(idx, nine).unwrap();
    assert!(!symbols::symbol_is_constant(idx));
    let previous = symbols::bind_symbol_value(idx, nine);
    symbols::define_symbol_constant(idx, seven);
    assert!(symbols::symbol_is_constant(idx));
    assert_eq!(symbols::symbol_value(idx), Some(nine));
    assert!(matches!(
        symbols::set_symbol_value_checked(idx, seven),
        Err(EgclError::ProgramError(_))
    ));
    assert_eq!(symbols::symbol_value(idx), Some(nine));
    symbols::restore_symbol_binding(idx, previous);
    assert_eq!(symbols::symbol_value(idx), Some(seven));
    symbols::define_symbol_constant(idx, seven);

    assert!(!symbols::symbol_is_constant(other));
    symbols::set_symbol_value_checked(other, seven).unwrap();
    let previous = symbols::bind_symbol_value(other, seven);
    symbols::set_symbol_value_checked(other, nine).unwrap();
    assert_eq!(symbols::symbol_value(other), Some(nine));
    symbols::restore_symbol_binding(other, previous);
    assert_eq!(symbols::symbol_value(other), Some(seven));
    symbols::mark_symbol_constant(other);
    assert!(symbols::set_symbol_value_checked(other, nine).is_err());

    egcl_rt::gc::full_gc().unwrap();
    let heap = egcl_rt::gc::serialize_heap_objects();
    let registry = egcl_rt::gc::serialize_symbols();
    egcl_rt::gc::restore_heap(&heap).unwrap();
    egcl_rt::gc::restore_symbols(&registry).unwrap();
    egcl_rt::gc::full_gc().unwrap();
    for index in [idx, other] {
        assert!(symbols::symbol_is_constant(index));
        assert!(matches!(
            symbols::set_symbol_value_checked(index, nine),
            Err(EgclError::ProgramError(_))
        ));
        assert_eq!(symbols::symbol_value(index), Some(seven));
    }
}
