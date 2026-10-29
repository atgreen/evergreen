// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native code reads and assigns a special's value cell inline (bliss-csc8v).

use super::*;

fn eval(source: &str, env: &mut Env) -> Result<EgclVal, EgclError> {
    super::super::super::read_eval_all_env(source, env)
}

fn helper_calls() -> usize {
    GLOBAL_ACCESS_HELPER_CALLS.with(|calls| calls.replace(0))
}

#[test]
fn native_special_access_uses_the_value_cell_and_survives_relocation() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    eval(
        "(setq *inline-cell* nil)
         (defun inline-cell-store (n)
           (let ((i 0))
             (tagbody
              top
                (setq *inline-cell* (cons i (cons i nil)))
                (setq i (+ i 1))
                (if (< i n) (go top))))
           *inline-cell*)
         (defun inline-cell-car () (car *inline-cell*))",
        &mut env,
    )
    .unwrap();
    let cell = egcl_rt::symbols::find_index("*INLINE-CELL*").unwrap();
    let store = egcl_rt::symbols::find_index("INLINE-CELL-STORE").unwrap();
    let read = egcl_rt::symbols::find_index("INLINE-CELL-CAR").unwrap();
    // Invocation heat installs native code for both functions.
    for _ in 0..60 {
        eval("(inline-cell-store 3)", &mut env).unwrap();
        eval("(inline-cell-car)", &mut env).unwrap();
    }
    for symbol in [store, read] {
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        assert!(
            egcl_rt::function::tier(*function) >= 1,
            "{}: the probe must run native code",
            egcl_rt::symbols::symbol_name(symbol).unwrap()
        );
    }
    assert_eq!(egcl_rt::symbols::binding_guard(cell), Some(0));

    // Unbound by any execution: every access stays in native code.
    helper_calls();
    eval("(inline-cell-store 50)", &mut env).unwrap();
    assert_eq!(
        helper_calls(),
        0,
        "unbound-by-binding accesses must not call the runtime"
    );
    // The list the native store left in the value cell is young; a minor
    // collection moves it and updates the cell, which is a root.
    let stored = egcl_rt::symbols::symbol_value(cell).unwrap();
    egcl_rt::collect_t0_minor().unwrap();
    let relocated = egcl_rt::symbols::symbol_value(cell).unwrap();
    assert_ne!(
        stored.to_raw(),
        relocated.to_raw(),
        "the stored list must actually move"
    );
    assert_eq!(
        eval("(inline-cell-car)", &mut env).unwrap(),
        EgclVal::from_fixnum(49)
    );
    assert_eq!(
        helper_calls(),
        0,
        "the inline read must see the relocated list"
    );
    assert_eq!(
        eval("(car (cdr *inline-cell*))", &mut env).unwrap(),
        EgclVal::from_fixnum(49)
    );

    // A dynamic binding sends the same code to the runtime, which sees it.
    helper_calls();
    assert_eq!(
        eval(
            "(let ((*inline-cell* (list 7))) (inline-cell-car))",
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(7)
    );
    assert!(
        helper_calls() > 0,
        "a bound special must take the runtime path"
    );
    assert_eq!(
        egcl_rt::symbols::binding_guard(cell),
        Some(0),
        "the binding released its guard"
    );
    assert_eq!(
        eval("(inline-cell-car)", &mut env).unwrap(),
        EgclVal::from_fixnum(49)
    );

    // An unbound cell falls through to the runtime, which signals.
    egcl_rt::symbols::set_symbol_value(cell, egcl_rt::value::UNBOUND);
    let unbound = format!("{:?}", eval("(inline-cell-car)", &mut env));
    assert!(
        unbound.contains("*INLINE-CELL*") && unbound.to_ascii_lowercase().contains("unbound"),
        "an unbound read must signal: {unbound}"
    );
}

#[test]
fn binding_guard_counts_executions_and_constants() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let symbol = egcl_rt::symbols::intern("*INLINE-GUARD-PROBE*");
    assert_eq!(egcl_rt::symbols::binding_guard(symbol), Some(0));
    let outer = egcl_rt::symbols::bind_symbol_value(symbol, EgclVal::from_fixnum(1));
    assert_eq!(egcl_rt::symbols::binding_guard(symbol), Some(1));
    // A nested binding in the same execution does not count again.
    let inner = egcl_rt::symbols::bind_symbol_value(symbol, EgclVal::from_fixnum(2));
    assert_eq!(egcl_rt::symbols::binding_guard(symbol), Some(1));
    let other = std::thread::spawn(move || {
        let previous = egcl_rt::symbols::bind_symbol_value(symbol, EgclVal::from_fixnum(3));
        let seen = egcl_rt::symbols::binding_guard(symbol);
        egcl_rt::symbols::restore_symbol_binding(symbol, previous);
        seen
    })
    .join()
    .unwrap();
    assert_eq!(
        other,
        Some(2),
        "a second execution's binding counts separately"
    );
    egcl_rt::symbols::restore_symbol_binding(symbol, inner);
    assert_eq!(egcl_rt::symbols::binding_guard(symbol), Some(1));
    egcl_rt::symbols::restore_symbol_binding(symbol, outer);
    assert_eq!(egcl_rt::symbols::binding_guard(symbol), Some(0));
    // A constant keeps reads inline but sends assignments to the runtime.
    egcl_rt::symbols::define_symbol_constant(symbol, EgclVal::from_fixnum(4));
    assert_eq!(
        egcl_rt::symbols::binding_guard(symbol),
        Some(egcl_rt::symbols::BINDING_GUARD_CONSTANT)
    );
}
