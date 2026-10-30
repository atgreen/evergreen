// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::{macroexpand::*, reader};
use egcl_rt::{EgclVal, object::ConsCell};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn nth(mut value: EgclVal, index: usize) -> EgclVal {
    for _ in 0..index {
        assert!(value.is_cons());
        value = unsafe { (*value.as_ptr().cast::<ConsCell>()).cdr };
    }
    assert!(value.is_cons());
    unsafe { (*value.as_ptr().cast::<ConsCell>()).car }
}

struct Registration(EgclVal);
impl Drop for Registration {
    fn drop(&mut self) {
        unregister_macro_function(self.0);
    }
}

fn check_scope(source: &str, output_path: &[usize]) {
    egcl_rt::gc::ensure_heap_initialized();
    let collect = EgclVal::from_symbol_index(reader::intern_symbol("ENV-GC-COLLECT"));
    let payload_name = EgclVal::from_symbol_index(reader::intern_symbol("ENV-GC-PAYLOAD"));
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let key =
        EgclVal::from_macro_handle(0x454e564743 + NEXT.fetch_add(1, Ordering::Relaxed) as i64);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    register_macro_function(
        key,
        Arc::new(move |_, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            egcl_rt::gc::collect_t0_minor()?;
            Ok(EgclVal::from_fixnum(42))
        }),
    );
    let _registration = Registration(key);
    // Complete symbol/readtable setup before allocating a fresh payload.
    reader::read_from_string(source).unwrap();
    reader::read_from_string("(QUOTE (17))").unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(payload = reader::read_from_string("(QUOTE (17))").unwrap().0);
    let before = payload.to_raw();
    egcl_rt::rooted!(
        environment = Environment::null()
            .augment_function(collect, FunctionInfo::Macro(key))
            .augment_variable(payload_name, VariableInfo::SymbolMacro(*payload))
    );
    egcl_rt::rooted!(form = reader::read_from_string(source).unwrap().0);
    calls.store(0, Ordering::SeqCst);
    egcl_rt::rooted!(expanded = macroexpand_all(*form, &environment).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{source}");
    assert_ne!(payload.to_raw(), before, "payload must relocate: {source}");
    let mut output = *expanded;
    for &index in output_path {
        output = nth(output, index);
    }
    assert_eq!(
        output, *payload,
        "derived scope must retain the relocated expansion: {source}"
    );
    assert!(nth(*payload, 1).is_cons());
    assert_eq!(nth(nth(*payload, 1), 0), EgclVal::from_fixnum(17));
}

#[test]
fn block_preserves_inherited_symbol_macro_after_gc() {
    check_scope(
        "(BLOCK NIL (ENV-GC-COLLECT) (ENV-GC-USE ENV-GC-PAYLOAD))",
        &[3, 1],
    );
}
#[test]
fn tagbody_preserves_inherited_symbol_macro_after_gc() {
    check_scope(
        "(TAGBODY (ENV-GC-COLLECT) (ENV-GC-USE ENV-GC-PAYLOAD))",
        &[2, 1],
    );
}
#[test]
fn let_star_preserves_inherited_symbol_macro_after_gc() {
    check_scope(
        "(LET* ((ENV-GC-X 0)) (ENV-GC-COLLECT) (ENV-GC-USE ENV-GC-PAYLOAD))",
        &[3, 1],
    );
}
#[test]
fn let_preserves_inherited_symbol_macro_after_gc() {
    check_scope(
        "(LET ((ENV-GC-X 0)) (ENV-GC-COLLECT) (ENV-GC-USE ENV-GC-PAYLOAD))",
        &[3, 1],
    );
}

#[test]
fn let_star_initializers_preserve_inherited_symbol_macro_after_gc() {
    check_scope(
        "(LET* ((ENV-GC-X (ENV-GC-COLLECT)) (ENV-GC-Y ENV-GC-PAYLOAD)) (ENV-GC-USE ENV-GC-Y))",
        &[1, 1, 1],
    );
}
