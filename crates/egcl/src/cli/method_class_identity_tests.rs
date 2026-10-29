// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn host_method_records_keep_class_values_and_reject_name_only_records() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defclass identity-codec-class () ())
         (defmethod identity-codec-method ((object identity-codec-class)) :ok)",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(
        class = egcl_stdlib::find_class(resolve_sym("IDENTITY-CODEC-CLASS").unwrap()).unwrap()
    );
    let previous_methods = SAVE_METHODS.with(|cell| cell.replace(Some(env.methods)));
    let previous_generics = SAVE_GENERICS.with(|cell| cell.replace(Some(env.generics)));
    let bytes = host_serialize_registries();
    SAVE_METHODS.with(|cell| *cell.borrow_mut() = previous_methods);
    SAVE_GENERICS.with(|cell| *cell.borrow_mut() = previous_generics);
    host_restore_registries(&bytes).unwrap();
    let value = PENDING_HOST_METHODS.with(|cell| {
        let records = cell.borrow();
        let method = &records
            .iter()
            .find(|(name, _)| name == "IDENTITY-CODEC-METHOD")
            .unwrap()
            .1[0];
        assert_eq!(method.5[0].0, 1);
        method.5[0].2
    });
    assert_eq!(value, class.to_raw());
    drain_pending_host_generics(&env);
    let definitions = env.methods.borrow();
    assert!(
        matches!(definitions["IDENTITY-CODEC-METHOD"][0].specializers[0],
        MethodSpecializer::Class { class: actual, .. } if actual == *class)
    );
    drop(definitions);

    let marker = bytes
        .windows(4)
        .position(|window| window == b"GFSI")
        .unwrap();
    let mut legacy = bytes.clone();
    legacy[marker..marker + 4].copy_from_slice(b"GFNS");
    let error = host_restore_registries(&legacy).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("obsolete name-based method specializers")
    );
    assert!(host_restore_registries(&bytes[..marker + 4]).is_err());
}

#[test]
fn next_method_context_traces_class_identity_in_cloned_chains() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _env = Env::new(false);
    let class = egcl_stdlib::find_class(resolve_sym("STANDARD-OBJECT").unwrap()).unwrap();
    let replacement = egcl_stdlib::find_class(resolve_sym("INTEGER").unwrap()).unwrap();
    let definition = MethodDef {
        method_id: next_stdlib_class_id(),
        captured_frame: None,
        specializers: vec![MethodSpecializer::Class {
            source_name: "STANDARD-OBJECT".into(),
            class,
        }],
        lambda_list: NIL,
        qualifier: egcl_stdlib::MethodQualifier::Primary,
        combination: None,
        body: NIL,
    };
    let mut next = NextMethod::Primary {
        primary: vec![definition],
    };
    // Class objects are pinned, so this tests the host visitor's exact slot
    // contract rather than claiming that a class moved during a minor GC.
    egcl_rt::gc::TraceHostRoots::trace_host_roots(&mut next, &mut |slot| unsafe {
        if *slot == class {
            *slot = replacement;
        }
    });
    let NextMethod::Primary { primary } = next else {
        unreachable!()
    };
    assert!(matches!(primary[0].specializers[0],
        MethodSpecializer::Class { class: actual, .. } if actual == replacement));
}

#[test]
fn existing_improper_specializer_fails_instead_of_emitting_a_fresh_name_recipe() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defclass identity-improper-class () ())
         (defmethod identity-improper-method ((object identity-improper-class)) :ok)
         (setf (find-class 'identity-improper-class) nil)",
        &mut env,
    )
    .unwrap();
    let error = bytecode::build_image_from_runtime(&env).unwrap_err();
    assert!(error.to_string().contains("without MAKE-LOAD-FORM"));
}

#[test]
fn builtin_dispatch_and_metaclass_metadata_use_stored_class_identity() {
    let _guard = heap_test_lock().lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(integer = egcl_stdlib::find_class(resolve_sym("INTEGER").unwrap()).unwrap());
    egcl_rt::rooted!(standard_class = egcl_stdlib::find_class(resolve_sym("STANDARD-CLASS").unwrap()).unwrap());
    egcl_rt::rooted!(text = arena_str("not an integer"));
    let definition = |source: &str, class| MethodDef {
        method_id: next_stdlib_class_id(),
        captured_frame: None,
        specializers: vec![MethodSpecializer::Class { source_name: source.into(), class }],
        lambda_list: NIL,
        qualifier: egcl_stdlib::MethodQualifier::Primary,
        combination: None,
        body: NIL,
    };
    egcl_rt::rooted!(method = definition("FOREIGN::STRING", *integer));
    assert!(method_specificity_vector(&env, &method, &[EgclVal::from_fixnum(7)]).is_some());
    assert!(method_specificity_vector(&env, &method, &[*text]).is_none());
    env.methods.borrow_mut().insert("IDENTITY-METACLASS-FLAG".into(),
        vec![definition("FOREIGN::STANDARD-CLASS", *integer)]);
    assert!(!generic_dispatches_on_a_metaclass(&env, "IDENTITY-METACLASS-FLAG"));
    env.methods.borrow_mut().insert("IDENTITY-METACLASS-FLAG".into(),
        vec![definition("FOREIGN::ORDINARY-NAME", *standard_class)]);
    assert!(generic_dispatches_on_a_metaclass(&env, "IDENTITY-METACLASS-FLAG"));
}
