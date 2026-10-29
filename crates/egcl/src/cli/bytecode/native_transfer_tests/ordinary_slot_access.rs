// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn read(env: &mut Env, source: &str) -> EgclVal {
    crate::cli::read_eval_all_env(source, env).unwrap()
}

// These fixtures load the public prelude, so read their source with the same
// package-aware environment as their definitions instead of the bare reader.
pub(super) fn install_in_environment(
    name: &str,
    params: &str,
    forms: &str,
    env: &mut Env,
) -> (u32, Rc<NativeCode>) {
    egcl_rt::rooted!(
        params = crate::cli::read_next_form_at(&params.chars().collect::<Vec<_>>(), 0, env)
            .unwrap()
            .0
    );
    egcl_rt::rooted!(
        forms = crate::cli::read_next_form_at(&forms.chars().collect::<Vec<_>>(), 0, env)
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern(name);
    let body = compile_function(name, *params, *forms, env, false, false).unwrap_or_else(|| {
        panic!(
            "{name}: package-aware fixture compilation failed: {:?}",
            last_bail_reason()
        )
    });
    install_body(symbol, Arc::new(body))
}

pub(super) fn define_entry_observer(name: &str, env: &mut Env) -> RecursiveFixturePins {
    egcl_rt::rooted!(
        name = crate::cli::read_next_form_at(&name.chars().collect::<Vec<_>>(), 0, env)
            .unwrap()
            .0
    );
    // This is an implementation test operator, not a public package function.
    // Preserve its exact bare identity without package-local reader homonyms.
    let hook = EgclVal::from_symbol_index(reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST"));
    egcl_rt::rooted!(body = crate::cli::vec_to_list(&[hook]));
    egcl_rt::rooted!(defun = crate::cli::resolve_sym("DEFUN").unwrap());
    egcl_rt::rooted!(definition = crate::cli::vec_to_list(&[*defun, *name, NIL, *body]));
    crate::cli::eval_form(*definition, env).unwrap();
    // Keep only this instrumentation callback interpreted so it observes the
    // target's suspended native frame rather than becoming the top native owner.
    RecursiveFixturePins::new(&[&crate::cli::sym_name(*name)])
}

pub(super) fn is_undefined_function(error: &EgclError, expected: EgclVal, env: &Env) -> bool {
    egcl_rt::rooted!(expected = expected);
    match error.without_backtrace() {
        EgclError::UndefinedFunction(name) => egcl_stdlib::equality::equal(*name, *expected),
        EgclError::Signalled(details) => {
            egcl_rt::rooted!(condition = details.condition);
            if !crate::cli::condition_matches_handler(env, *condition, "UNDEFINED-FUNCTION") {
                return false;
            }
            egcl_rt::rooted!(name_slot = crate::cli::resolve_sym("NAME").unwrap());
            // CELL-ERROR-NAME's standard implementation reads this inherited
            // slot. Check the actual public name, including a proper SETF name.
            matches!(egcl_stdlib::clos::slot_value_if_present(*condition, *name_slot),
                Some(Ok(name)) if egcl_stdlib::equality::equal(name, *expected))
        }
        _ => false,
    }
}

fn assert_call(code: &NativeCode, symbol: u32, object: EgclVal, expected: i64, env: &mut Env) {
    expect_entry(code);
    assert_eq!(
        run_native(code, symbol, &[object], env).unwrap(),
        EgclVal::from_fixnum(expected)
    );
    assert!(
        observed_entry(),
        "the exact installed mapped T2 caller must execute"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_slot_access_observes_using_class_mutation_and_bindings() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(false));
    read(&mut env, crate::cli::EMBEDDED_BOOT_LISP);
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(true));
    read(&mut env, "(in-package :cl-user)");
    let _observer_pin = define_entry_observer("native-slot-observe-entry", &mut env);
    read(
        &mut env,
        "
        (defclass native-slot-probe () ((value :initform 9 :accessor native-slot-reader)))
        (defparameter *native-slot-object* (make-instance 'native-slot-probe))
        (defclass native-slot-meta (standard-class) ())
        (defmethod egcl-mop:slot-value-using-class :around
            ((class native-slot-meta) object slot)
          (call-next-method))
    ",
    );
    egcl_rt::rooted!(object = read(&mut env, "*native-slot-object*"));
    assert!(
        crate::cli::slot_access::pristine(*object, crate::cli::slot_access::Operation::Read, &env),
        "a custom-metaclass-only override must retain standard-class fast access"
    );
    let mut readers = Vec::new();
    for (name, forms) in [
        (
            "NATIVE-SLOT-ACCESSOR",
            "((progn (native-slot-observe-entry) (native-slot-reader object)))",
        ),
        (
            "NATIVE-SLOT-DIRECT",
            "((progn (native-slot-observe-entry) (slot-value object 'value)))",
        ),
    ] {
        let (symbol, code) = install_in_environment(name, "(object)", forms, &mut env);
        assert_mapped_t2(&code);
        for _ in 0..3 {
            assert_call(&code, symbol, *object, 9, &mut env);
        }
        readers.push((symbol, code));
    }
    for (offset, expected) in [(100, 109), (200, 209)] {
        read(
            &mut env,
            &format!(
                "
            (defmethod egcl-mop:slot-value-using-class :around
                ((class standard-class) (object native-slot-probe) slot)
              (+ {offset} (call-next-method)))"
            ),
        );
        assert!(!crate::cli::slot_access::pristine(
            *object,
            crate::cli::slot_access::Operation::Read,
            &env
        ));
        for (symbol, code) in &readers {
            assert_call(code, *symbol, *object, expected, &mut env);
        }
    }
    read(
        &mut env,
        "
        (remove-method #'egcl-mop:slot-value-using-class
          (find-method #'egcl-mop:slot-value-using-class '(:around)
            (list (find-class 'standard-class) (find-class 'native-slot-probe) (find-class 't))))
    ",
    );
    assert!(crate::cli::slot_access::pristine(
        *object,
        crate::cli::slot_access::Operation::Read,
        &env
    ));
    for (symbol, code) in &readers {
        assert_call(code, *symbol, *object, 9, &mut env);
    }
    let mut writers = Vec::new();
    for (name, forms) in [
        (
            "NATIVE-SLOT-ACCESSOR-WRITER",
            "((progn (native-slot-observe-entry) (setf (native-slot-reader object) 20)))",
        ),
        (
            "NATIVE-SLOT-DIRECT-WRITER",
            "((progn (native-slot-observe-entry) (setf (slot-value object 'value) 20)))",
        ),
    ] {
        let (symbol, code) = install_in_environment(name, "(object)", forms, &mut env);
        assert_mapped_t2(&code);
        for _ in 0..3 {
            assert_call(&code, symbol, *object, 20, &mut env);
        }
        writers.push((symbol, code));
    }
    for offset in [1, 2] {
        read(
            &mut env,
            &format!(
                "
            (defmethod (setf egcl-mop:slot-value-using-class) :around
                (new (class standard-class) (object native-slot-probe) slot)
              (call-next-method (+ new {offset}) class object slot))"
            ),
        );
        assert!(!crate::cli::slot_access::pristine(
            *object,
            crate::cli::slot_access::Operation::Write,
            &env
        ));
        for (symbol, code) in &writers {
            assert_call(code, *symbol, *object, 20, &mut env);
            assert_eq!(
                read(
                    &mut env,
                    "(egcl-internal::%raw-slot-value *native-slot-object* 'value)"
                ),
                EgclVal::from_fixnum(20 + offset)
            );
        }
    }
    read(
        &mut env,
        "
        (remove-method #'(setf egcl-mop:slot-value-using-class)
          (find-method #'(setf egcl-mop:slot-value-using-class) '(:around)
            (list (find-class 't) (find-class 'standard-class)
                  (find-class 'native-slot-probe) (find-class 't))))
    ",
    );
    assert!(crate::cli::slot_access::pristine(
        *object,
        crate::cli::slot_access::Operation::Write,
        &env
    ));
    for (symbol, code) in &writers {
        assert_call(code, *symbol, *object, 20, &mut env);
    }
    read(
        &mut env,
        "(setf (slot-value *native-slot-object* 'value) 9)",
    );
    // The exact pure-delegate body is insufficient proof when its helper's
    // actual binding changes. Both an old cache hit and a fresh fill must miss.
    read(
        &mut env,
        "
        (setf (symbol-function 'egcl-internal::%standard-slot-value-using-class)
              (lambda (class object slot) (declare (ignore class object slot)) 501))
    ",
    );
    assert!(!crate::cli::slot_access::pristine(
        *object,
        crate::cli::slot_access::Operation::Read,
        &env
    ));
    for (symbol, code) in &readers {
        assert_call(code, *symbol, *object, 501, &mut env);
    }
    read(
        &mut env,
        "
        (setf (symbol-function 'egcl-mop:slot-value-using-class)
              (lambda (class object slot) (declare (ignore class object slot)) 601))
    ",
    );
    for (symbol, code) in &readers {
        assert_call(code, *symbol, *object, 601, &mut env);
    }
    read(&mut env, "(fmakunbound 'egcl-mop:slot-value-using-class)");
    assert!(!crate::cli::slot_access::pristine(
        *object,
        crate::cli::slot_access::Operation::Read,
        &env
    ));
    egcl_rt::rooted!(missing_name = read(&mut env, "'egcl-mop:slot-value-using-class"));
    for (symbol, code) in &readers {
        expect_entry(code);
        egcl_rt::rooted!(result = run_native(code, *symbol, &[*object], &mut env));
        assert!(
            matches!(&*result, Err(error) if is_undefined_function(error, *missing_name, &env)),
            "a missing using-class function must report its public name: {:?}",
            &*result
        );
        assert!(observed_entry());
    }
}
