// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn formatter_descriptor_is_exact_collectible_and_relocates_its_control() {
    const CHILD: &str = "EGCL_FORMATTER_DESCRIPTOR_GC_CHILD";
    const TEST: &str = "cli::formatter_protocol_tests::formatter_descriptor_is_exact_collectible_and_relocates_its_control";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_FORCE_TIER", "interp")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    // Warm private operator/symbol setup before constructing the movable probe.
    egcl_rt::rooted!(
        warm = make_formatter(
            egcl_stdlib::streams::make_lisp_string_fresh("warm"),
            &mut env
        )
        .unwrap()
    );
    drop(warm);
    egcl_rt::gc::full_gc().unwrap();
    egcl_rt::rooted!(control = egcl_stdlib::streams::make_lisp_string_fresh("~A"));
    let before = *control;
    egcl_rt::rooted!(function = make_formatter(*control, &mut env).unwrap());
    let definition = egcl_rt::function::definition_index(*function);
    assert_ne!(
        egcl_rt::symbols::symbol_function(definition),
        Some(*function),
        "no globally rooted private self binding"
    );
    let mut weak_control = Box::new(egcl_rt::gc::WeakPointer::new(
        egcl_rt::gc::finalizer_key(*control).unwrap(),
    ));
    let mut weak_function = Box::new(egcl_rt::gc::WeakPointer::new(
        egcl_rt::gc::finalizer_key(*function).unwrap(),
    ));
    egcl_rt::gc::register_weak_pointer(&mut weak_control);
    egcl_rt::gc::register_weak_pointer(&mut weak_function);
    drop(control);
    egcl_rt::gc::collect_t0_minor().unwrap();
    let retained = stdlib_formatter_control_hook(*function).unwrap();
    assert_ne!(
        retained, before,
        "control owned only by function frame must physically move"
    );
    assert_eq!(retained.as_string(), "~A");
    egcl_rt::rooted!(compiled = compile_owned_formatter(*function, &env).unwrap().unwrap());
    let original_frame = bytecode::closure_captured_env(*function).unwrap();
    let compiled_frame = bytecode::closure_captured_env(*compiled).unwrap();
    assert!(
        !Arc::ptr_eq(&original_frame, &compiled_frame),
        "COMPILE copy must own a fresh direct frame"
    );
    assert_eq!(
        stdlib_formatter_control_hook(*function)
            .unwrap()
            .as_string(),
        "~A"
    );
    assert_eq!(
        stdlib_formatter_control_hook(*compiled)
            .unwrap()
            .as_string(),
        "~A"
    );
    // Both exact self witnesses remain independent and each ordinary callable
    // uses its own frame, including the supported forced-interpreter fallback.
    egcl_rt::rooted!(callables = vec![*function, *compiled]);
    for index in 0..callables.len() {
        let callable = callables[index];
        egcl_rt::rooted!(callable = callable);
        egcl_rt::rooted!(stream = egcl_stdlib::make_string_output_stream(NIL).unwrap());
        let tail =
            apply_function(*callable, &[*stream, EgclVal::from_fixnum(7)], &mut env).unwrap();
        assert!(tail.is_nil());
        egcl_rt::rooted!(text = egcl_stdlib::get_output_stream_string(*stream).unwrap());
        assert_eq!(text.as_string(), "7");
    }
    // Sharing a direct frame cannot authorize another heap function object.
    egcl_rt::rooted!(other_name = egcl_rt::symbols::make_uninterned("OTHER-CALLABLE"));
    egcl_rt::rooted!(other = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *other_name));
    let frame = bytecode::closure_captured_env(*function).unwrap();
    bytecode::register_private_closure_env(*other, Arc::clone(&frame));
    assert!(stdlib_formatter_control_hook(*other).is_none());
    // Neither can an ordinary child inherit protocol authority from a parent.
    egcl_rt::rooted!(child_name = egcl_rt::symbols::make_uninterned("CHILD-CALLABLE"));
    egcl_rt::rooted!(child = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *child_name));
    let child_frame = Arc::new(SharedCell::new(EnvFrame {
        parent: Some(frame),
        ..EnvFrame::default()
    }));
    bytecode::register_private_closure_env(*child, child_frame);
    assert!(stdlib_formatter_control_hook(*child).is_none());
    drop(other);
    drop(child);
    drop(callables);
    drop(compiled);
    drop(function);
    env.mv.clear();
    env.mv_active = false;
    egcl_rt::gc::full_gc().unwrap();
    let dead_function = weak_function.is_broken();
    let dead_control = weak_control.is_broken();
    egcl_rt::gc::unregister_weak_pointer(&weak_control);
    egcl_rt::gc::unregister_weak_pointer(&weak_function);
    assert!(
        dead_function,
        "descriptor self cycle must not root discarded callable"
    );
    assert!(
        dead_control,
        "discarded callable must release its only control owner"
    );
    assert!(
        bytecode::closure_captured_env(EgclVal::from_symbol_index(definition)).is_none(),
        "dead private callable must release its frame entry"
    );
}

#[test]
fn compile_roots_relocated_definition_forms_while_evaluating_its_name() {
    const CHILD: &str = "EGCL_FORMATTER_COMPILE_SOURCE_GC_CHILD";
    const TEST: &str = "cli::formatter_protocol_tests::compile_roots_relocated_definition_forms_while_evaluating_its_name";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_FORCE_TIER", "interp")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    const SOURCE: &str = "(compile (progn (egcl-ext:gc) nil) (egcl::%make-formatter \"~A\"))";
    // Intern reader tokens first, then build a fresh nursery form whose
    // definition expression must move during evaluation of its name expression.
    egcl_rt::rooted!(warm = read_from_string_in_env(SOURCE, &mut env).unwrap().0);
    drop(warm);
    egcl_rt::gc::full_gc().unwrap();
    egcl_rt::rooted!(form = read_from_string_in_env(SOURCE, &mut env).unwrap().0);
    let (_, arguments) = cp(*form);
    let (_, tail) = cp(arguments);
    egcl_rt::rooted!(definition = cp(tail).0);
    let before = *definition;
    egcl_rt::rooted!(function = eval_form(*form, &mut env).unwrap());
    assert_ne!(
        *definition, before,
        "COMPILE definition source must physically relocate during name evaluation"
    );
    assert!(egcl_rt::function::is_interpreted_function(*function));
    assert_eq!(
        stdlib_formatter_control_hook(*function)
            .unwrap()
            .as_string(),
        "~A"
    );
    egcl_rt::rooted!(stream = egcl_stdlib::make_string_output_stream(NIL).unwrap());
    let rest = apply_function(*function, &[*stream, EgclVal::from_fixnum(7)], &mut env).unwrap();
    assert!(rest.is_nil());
    egcl_rt::rooted!(text = egcl_stdlib::get_output_stream_string(*stream).unwrap());
    assert_eq!(text.as_string(), "7");
}
