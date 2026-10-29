// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_generic_calls_observe_unbinding_rebinding_and_restoration() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let eval = |source: &str, env: &mut Env| crate::cli::read_eval_all_env(source, env).unwrap();
    eval(
        "(defmethod native-bound-gf ((x integer)) (+ x 1))
          (setq *native-bound-saved* #'native-bound-gf)
          (defmethod (setf native-bound-value) (new (object cons)) (setf (car object) new) new)
          (setq *native-bound-saved-writer* #'(setf native-bound-value))",
        &mut env,
    );
    let _observer_pin =
        super::slot_access::define_entry_observer("native-bound-observe-entry", &mut env);
    let (reader, read_code) = super::slot_access::install_in_environment(
        "NATIVE-BOUND-CALL",
        "(x)",
        "((progn (native-bound-observe-entry) (native-bound-gf x)))",
        &mut env,
    );
    assert_mapped_t2(&read_code);
    let (writer, write_code) = super::slot_access::install_in_environment(
        "NATIVE-BOUND-WRITE",
        "(object)",
        "((progn (native-bound-observe-entry) (setf (native-bound-value object) 7)))",
        &mut env,
    );
    assert_mapped_t2(&write_code);
    egcl_rt::rooted!(object = crate::cli::arena_cons(EgclVal::from_fixnum(0), NIL));
    for _ in 0..3 {
        expect_entry(&read_code);
        assert_eq!(
            run_native(&read_code, reader, &[EgclVal::from_fixnum(1)], &mut env).unwrap(),
            EgclVal::from_fixnum(2)
        );
        assert!(observed_entry());
        expect_entry(&write_code);
        assert_eq!(
            run_native(&write_code, writer, &[*object], &mut env).unwrap(),
            EgclVal::from_fixnum(7)
        );
        assert!(observed_entry());
    }
    eval(
        "(setf (symbol-function 'native-bound-gf) (lambda (x) (+ x 100)))
          (setf (fdefinition '(setf native-bound-value))
                (lambda (new object) (setf (car object) (+ new 100)) new))",
        &mut env,
    );
    expect_entry(&read_code);
    assert_eq!(
        run_native(&read_code, reader, &[EgclVal::from_fixnum(1)], &mut env).unwrap(),
        EgclVal::from_fixnum(101)
    );
    assert!(observed_entry());
    expect_entry(&write_code);
    assert_eq!(
        run_native(&write_code, writer, &[*object], &mut env).unwrap(),
        EgclVal::from_fixnum(7)
    );
    assert_eq!(crate::cli::cp(*object).0, EgclVal::from_fixnum(107));
    assert!(observed_entry());
    eval(
        "(fmakunbound 'native-bound-gf) (fmakunbound '(setf native-bound-value))",
        &mut env,
    );
    assert_eq!(eval("(fboundp 'native-bound-gf)", &mut env), NIL);
    assert_eq!(eval("(fboundp '(setf native-bound-value))", &mut env), NIL);
    egcl_rt::rooted!(missing_reader = eval("'native-bound-gf", &mut env));
    egcl_rt::rooted!(missing_writer = eval("'(setf native-bound-value)", &mut env));
    for (symbol, code, reading) in [(reader, &read_code, true), (writer, &write_code, false)] {
        let value = if reading {
            EgclVal::from_fixnum(1)
        } else {
            *object
        };
        expect_entry(code);
        egcl_rt::rooted!(result = run_native(code, symbol, &[value], &mut env));
        assert!(
            matches!(&*result, Err(error) if super::slot_access::is_undefined_function(
                error, if reading { *missing_reader } else { *missing_writer }, &env)),
            "{:?}",
            &*result
        );
        assert!(observed_entry());
    }
    assert_eq!(
        eval("(funcall *native-bound-saved* 1)", &mut env),
        EgclVal::from_fixnum(2)
    );
    eval(
        "(setf (symbol-function 'native-bound-gf) *native-bound-saved*)
          (setf (fdefinition '(setf native-bound-value)) *native-bound-saved-writer*)",
        &mut env,
    );
    expect_entry(&read_code);
    assert_eq!(
        run_native(&read_code, reader, &[EgclVal::from_fixnum(1)], &mut env).unwrap(),
        EgclVal::from_fixnum(2)
    );
    assert!(observed_entry());
    expect_entry(&write_code);
    assert_eq!(
        run_native(&write_code, writer, &[*object], &mut env).unwrap(),
        EgclVal::from_fixnum(7)
    );
    assert_eq!(crate::cli::cp(*object).0, EgclVal::from_fixnum(7));
    assert!(observed_entry());
}
