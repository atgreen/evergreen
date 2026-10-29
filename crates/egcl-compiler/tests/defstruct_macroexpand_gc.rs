// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::{macroexpand::*, reader};
use egcl_rt::{
    EgclError, EgclVal,
    object::{ConsCell, type_id},
    value::NIL,
};
use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);
const SOURCE: &str = "(DEFSTRUCT (DATA-STRUCT (:CONC-NAME DATA-) (:PRINT-OBJECT PRINTER-DATA)) \
    \"documented\" (SLOT-DATA (INIT-GC) :TYPE (OR INTEGER NULL) :READ-ONLY T))";

fn nth(mut value: EgclVal, index: usize) -> EgclVal {
    for _ in 0..index {
        assert!(value.is_cons());
        value = unsafe { (*(value.as_ptr() as *const ConsCell)).cdr };
    }
    assert!(value.is_cons());
    unsafe { (*(value.as_ptr() as *const ConsCell)).car }
}

fn collecting_expander(
    expander: EgclVal,
    _: EgclVal,
    _: &Environment,
) -> Result<EgclVal, EgclError> {
    assert_eq!(
        expander,
        EgclVal::from_fixnum(42),
        "a DEFSTRUCT data operand was expanded"
    );
    CALLS.fetch_add(1, Ordering::SeqCst);
    let body = egcl_rt::gc::alloc_typed(16, type_id::CONS).expect("allocate during expansion");
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(1));
        body.cast::<EgclVal>().add(1).write(NIL);
    }
    egcl_rt::gc::collect_t0_minor()?;
    Ok(EgclVal::from_fixnum(42))
}

#[test]
fn defstruct_preserves_slot_metadata_across_a_collecting_initializer_expansion() {
    egcl_rt::gc::ensure_heap_initialized();
    let initializer = EgclVal::from_symbol_index(reader::intern_symbol("INIT-GC"));
    let type_operator = EgclVal::from_symbol_index(reader::intern_symbol("OR"));
    let slot_name = EgclVal::from_symbol_index(reader::intern_symbol("SLOT-DATA"));
    define_global_macro(initializer, EgclVal::from_fixnum(42));
    // If the walker mistakes type syntax for executable code, this distinct
    // expander marker makes the error observable before any result assertions.
    define_global_macro(type_operator, EgclVal::from_fixnum(99));
    set_macroexpand_hook(collecting_expander);
    let environment = Environment::null().augment_variable(
        slot_name,
        VariableInfo::SymbolMacro(EgclVal::from_fixnum(999)),
    );
    egcl_rt::rooted!(environment = environment);
    let control: u32 =
        egcl_rt::symbols::make_uninterned("UNOWNED-DEFSTRUCT-CONTROL").as_symbol_index();
    // Intern every source symbol/readtable setup before the movement probe,
    // so pinned symbol allocations cannot retain the type's nursery region.
    reader::read_from_string(SOURCE).unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(form = reader::read_from_string(SOURCE).unwrap().0);
    egcl_rt::rooted!(options = nth(*form, 1));
    egcl_rt::rooted!(documentation = nth(*form, 2));
    egcl_rt::rooted!(slot = nth(*form, 3));
    egcl_rt::rooted!(type_form = nth(*slot, 3));
    let before_type: u64 = type_form.to_raw();
    CALLS.store(0, Ordering::SeqCst);
    egcl_rt::rooted!(expanded = macroexpand_all(*form, &environment).unwrap());
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    assert_ne!(
        type_form.to_raw(),
        before_type,
        "the protected type cons must actually relocate"
    );
    assert_eq!(
        nth(*expanded, 1),
        *options,
        "definition options retain exact data identity"
    );
    assert_eq!(nth(*expanded, 2), *documentation);
    let expanded_slot = nth(*expanded, 3);
    assert_eq!(
        nth(expanded_slot, 0),
        slot_name,
        "slot name is not a symbol-macro reference"
    );
    assert_eq!(nth(expanded_slot, 1), EgclVal::from_fixnum(42));
    assert_eq!(
        nth(expanded_slot, 3),
        *type_form,
        "compound slot type stays exact data"
    );
    assert_eq!(nth(expanded_slot, 2), nth(*slot, 2));
    assert_eq!(nth(expanded_slot, 4), nth(*slot, 4));
    assert_eq!(nth(expanded_slot, 5), nth(*slot, 5));
    assert_eq!(nth(*type_form, 0), type_operator);
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        egcl_rt::symbols::symbol_name(control).is_none(),
        "unowned control must be reclaimed"
    );
    undefine_global_macro(initializer);
    undefine_global_macro(type_operator);
}
