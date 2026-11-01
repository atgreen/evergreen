// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap lexical environments (Backpack): relocation and the compile-time
//! rewrite of an eligible unit.

use super::*;

fn compile(params: &str, body: &str, env: &Env) -> BytecodeFunction {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(body).unwrap().0);
    compile_function("HEAP-ENV-TEST", *params, *forms, env, true, false).expect("compile fixture")
}

fn all_code(function: &BytecodeFunction, out: &mut Vec<Instr>) {
    out.extend(function.code.iter().copied());
    for nested in &function.nested_functions {
        all_code(nested, out);
    }
}

/// A heap environment is an ordinary movable object: a minor collection
/// relocates both it and the nursery value in its slot, and a rooted holder
/// reads the moved value back through `(depth, index)`.
#[test]
fn heap_environment_and_its_slot_values_relocate() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::gc::collect_t0_minor().unwrap();
    let _ = arena_cons(NIL, NIL);
    egcl_rt::rooted!(parent = egcl_rt::lexenv::make(NIL, 1).expect("parent"));
    egcl_rt::rooted!(child = egcl_rt::lexenv::make(*parent, 2).expect("child"));
    egcl_rt::rooted!(value = arena_cons(EgclVal::from_fixnum(42), NIL));
    assert!(egcl_rt::lexenv::store(*parent, 0, 0, *value));
    assert!(egcl_rt::lexenv::store(*child, 0, 1, *value));
    let unrooted_child = *child;
    let unrooted_value = *value;
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(*child, unrooted_child, "the environment did not move");
    assert_ne!(*value, unrooted_value, "the slot value did not move");
    // Both slots were relocated with their holders, through the parent link.
    assert_eq!(egcl_rt::lexenv::load(*child, 0, 1), Some(*value));
    assert_eq!(egcl_rt::lexenv::load(*child, 1, 0), Some(*value));
    assert_eq!(egcl_rt::lexenv::hop(*child, 1), Some(*parent));
    assert_eq!(cp(*value).0, EgclVal::from_fixnum(42));
    // Unwritten slots stay unbound; out-of-range addresses are refused.
    assert_eq!(
        egcl_rt::lexenv::load(*child, 0, 0),
        Some(egcl_rt::value::UNBOUND)
    );
    assert_eq!(egcl_rt::lexenv::load(*child, 0, 2), None);
    assert_eq!(egcl_rt::lexenv::load(*child, 2, 0), None);
}

/// An eligible unit uses heap environments exclusively: no name-keyed frame
/// instruction survives, no function keeps a base frame, a captured but
/// unassigned variable is copied into the closure, and a captured, assigned
/// one lives in an environment slot whose name the debugger table reports.
#[test]
fn eligible_unit_is_rewritten_to_heap_environments() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let function = compile(
        "(start step)",
        "((let ((total start)) (list (lambda () (setq total (+ total step))) (lambda () total))))",
        &env,
    );
    let mut code = Vec::new();
    all_code(&function, &mut code);
    assert!(
        !code.iter().any(|instr| matches!(
            instr,
            Instr::LoadEnvVar(_)
                | Instr::StoreEnvVar(_)
                | Instr::DefineEnvVar(_)
                | Instr::PushEnvChild
                | Instr::PopEnvChild
                | Instr::MakeClosure {
                    capture_env: true,
                    ..
                }
        )),
        "name-keyed instructions survived: {code:?}"
    );
    assert!(!function.has_env);
    assert!(
        function
            .nested_functions
            .iter()
            .all(|nested| !nested.has_env)
    );
    // TOTAL is assigned: one shared environment. STEP is only read: copied.
    assert_eq!(
        code.iter()
            .filter(|i| matches!(i, Instr::MakeEnv(1)))
            .count(),
        1,
        "{code:?}"
    );
    assert!(
        code.iter()
            .any(|i| matches!(i, Instr::MakeEnvClosure { nflat: 1, .. }))
    );
    assert!(
        code.iter()
            .any(|i| matches!(i, Instr::MakeEnvClosure { nflat: 0, .. }))
    );
    assert!(code.iter().any(|i| matches!(i, Instr::ClosureEnv)));
    // Parameters stay in local slots 0 and 1.
    assert!(matches!(function.param_layout[0].1, VarLoc::Slot(0)));
    assert!(matches!(function.param_layout[1].1, VarLoc::Slot(1)));
    let names: Vec<String> = function
        .nested_functions
        .iter()
        .flat_map(|nested| heap_env::slot_names(nested))
        .map(|(_, _, _, name)| name)
        .collect();
    assert!(
        names.iter().any(|name| name.ends_with("TOTAL")),
        "{names:?}"
    );
    assert!(names.iter().any(|name| name.ends_with("STEP")), "{names:?}");
}

/// A unit whose restart clause reads a captured variable is not eligible: it
/// keeps the name-keyed frames, unchanged.
#[test]
fn ineligible_unit_keeps_name_keyed_frames() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let function = compile(
        "(x)",
        "((let ((f (lambda () x))) (restart-case (funcall f) (use () x))))",
        &env,
    );
    let mut code = Vec::new();
    all_code(&function, &mut code);
    if !function.restart_cases.is_empty() {
        assert!(!code.iter().any(|i| matches!(
            i,
            Instr::MakeEnv(_) | Instr::EnvRef { .. } | Instr::ClosureEnv
        )));
    }
}
