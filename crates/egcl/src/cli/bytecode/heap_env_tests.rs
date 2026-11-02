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
    let _ = arena_cons(NIL, NIL);
    egcl_rt::gc::collect_t0_minor().unwrap();
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
// Only x86-64 Linux rewrites (`heap_env::enabled`).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
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

/// The production heap-environment producer makes a nursery callable whose
/// precise root and traced environment are both rewritten by a minor GC.
#[test]
fn control_free_heap_closure_and_its_capture_relocate() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    let body = Arc::new(compile("()", "(nil)", &env));
    egcl_rt::rooted!(_body_root = ActiveBytecodeRoot::new(&body));
    // Mint the pinned prototype and finish incidental startup allocation first.
    let (prototype, _) = closure_templates::template_for(&body);
    egcl_rt::rooted!(prototype = prototype);
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(capture = egcl_rt::lexenv::make(NIL, 1).unwrap());
    egcl_rt::rooted!(payload = arena_cons(EgclVal::from_fixnum(73), NIL));
    assert!(egcl_rt::lexenv::store(*capture, 0, 0, *payload));
    egcl_rt::rooted!(closure = heap_env::make_closure(&body, *capture, &[], &env).unwrap());
    assert!(!egcl_rt::function::is_pinned(*closure));
    assert_eq!(unsafe { egcl_rt::function::closure_state(*closure) }, 0);
    let counter = egcl_rt::function::invocation_counter_address(*closure);
    let before = (*closure, *capture, *payload);
    // A second, unrooted instance must disappear at this minor collection,
    // without scheduling a major or retaining a per-instance host identity.
    let _ = heap_env::make_closure(&body, *capture, &[], &env).unwrap();
    fn instance_count(prototype: EgclVal) -> usize {
        let mut count = 0;
        egcl_rt::gc::walk_heap(|body, kind, _| {
            if kind == egcl_rt::object::type_id::FUNCTION_INTERPRETED {
                let function = unsafe {
                    EgclVal::from_heap_ptr(
                        body.sub(std::mem::size_of::<egcl_rt::object::ObjectHeader>())
                            .cast_mut(),
                    )
                };
                count +=
                    usize::from(egcl_rt::function::closure_prototype(function) == Some(prototype));
            }
            true
        })
        .unwrap();
        count
    }
    assert_eq!(instance_count(*prototype), 2);
    let major_count = egcl_rt::heap_stats().major_gc_count;
    // Only the callable root keeps the environment and its payload alive.
    drop(capture);
    drop(payload);
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(*closure, before.0, "the callable itself must move");
    assert_eq!(egcl_rt::heap_stats().major_gc_count, major_count);
    assert_eq!(
        instance_count(*prototype),
        1,
        "minor GC must reclaim the dead instance"
    );
    let captured = egcl_rt::function::closure_env(*closure);
    assert_ne!(captured, before.1, "the captured environment must move");
    let held = egcl_rt::lexenv::load(captured, 0, 0).unwrap();
    assert_ne!(held, before.2, "the captured payload must move");
    assert_eq!(cp(held), (EgclVal::from_fixnum(73), NIL));
    assert_eq!(
        egcl_rt::function::closure_prototype(*closure),
        Some(*prototype)
    );
    assert!(egcl_rt::function::is_pinned(*prototype));
    assert_eq!(
        egcl_rt::function::invocation_counter_address(*closure),
        counter
    );
    let count = egcl_rt::function::invoke_count(*prototype);
    egcl_rt::function::record_invocation(*closure);
    assert_eq!(egcl_rt::function::invoke_count(*prototype), count + 1);
    egcl_rt::gc::full_gc().unwrap();
    let captured = egcl_rt::function::closure_env(*closure);
    assert_eq!(
        cp(egcl_rt::lexenv::load(captured, 0, 0).unwrap()).0,
        EgclVal::from_fixnum(73)
    );
}

/// A movable callable cannot use the stable-address shortcut. A retained
/// native identity handle follows its precise root across actual relocation.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn movable_closure_native_identity_cache_reads_the_relocated_root() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    let body = Arc::new(compile("()", "(nil)", &env));
    egcl_rt::rooted!(_body_root = ActiveBytecodeRoot::new(&body));
    let code = native_transfer_entry::TransferCode::compile(Arc::clone(&body))
        .expect("constant body has a native entry");
    let _ = closure_templates::template_for(&body);
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(closure = heap_env::make_closure(&body, NIL, &[], &env).unwrap());
    assert!(!egcl_rt::function::is_pinned(*closure));
    let identity = code.callable_root(*closure);
    assert!(code.with_pinned_callable_root(*closure, |_| ()).is_none());
    assert!(Arc::ptr_eq(
        &identity,
        &code
            .cached_callable_root(&closure)
            .expect("current identity")
    ));
    let before = *closure;
    egcl_rt::gc::collect_t0_minor().unwrap();
    assert_ne!(*closure, before, "cached callable must actually relocate");
    assert!(identity.with_gc_stable_mutator(|held| *held == *closure));
    // An uninstalled code owner has no private-cache preparer: its cache
    // stays populated, so this explicitly exercises the traced-root fallback.
    let cached = code.cached_callable_root(&closure).expect("moved identity");
    assert!(Arc::ptr_eq(&identity, &cached));
    assert!(code.with_pinned_callable_root(*closure, |_| ()).is_none());
    assert!(Arc::ptr_eq(&identity, &code.callable_root(*closure)));
}
