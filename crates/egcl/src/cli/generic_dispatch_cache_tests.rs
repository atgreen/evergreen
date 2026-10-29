// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn reified_and_legacy_generic_arguments_do_not_alias_ordinary_callable_cache_keys() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    read_eval_all_env(
        "(defgeneric cache901-target (object))
         (defmethod cache901-target ((object standard-generic-function)) :generic)
         (defmethod cache901-target ((object function)) :function)
         (defmethod cache901-target ((object t)) :other)
         (defgeneric cache901-argument (object))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(name = resolve_sym("CACHE901-ARGUMENT").unwrap());
    egcl_rt::rooted!(generic = generic_fn_wrapper(&mut env, *name, "CACHE901-ARGUMENT"));
    egcl_rt::rooted!(source = reified_closure(*generic).unwrap());
    egcl_rt::rooted!(metadata = env.generics.borrow()["CACHE901-ARGUMENT"].generic_function);
    egcl_rt::rooted!(car = resolve_sym("CAR").unwrap());
    egcl_rt::rooted!(ordinary = builtin_fn_wrapper(&mut env, *car, "CAR"));
    assert_eq!(
        egcl_stdlib::class_of(*ordinary),
        egcl_stdlib::class_of(*generic),
        "exercise two semantic types with the same physical class"
    );
    assert_ne!(dispatch_key_entry(*ordinary), dispatch_key_entry(*generic));
    egcl_rt::rooted!(data = arena_cons(EgclVal::from_fixnum(1), NIL));
    egcl_rt::rooted!(text = arena_str("not a callable"));
    egcl_rt::rooted!(
        values = vec![
            *ordinary,
            *generic,
            *source,
            *metadata,
            NIL,
            T,
            EgclVal::from_fixnum(7),
            *data,
            *text
        ]
    );
    let generic_answer = resolve_sym(":GENERIC").unwrap();
    let function_answer = resolve_sym(":FUNCTION").unwrap();
    let other_answer = resolve_sym(":OTHER").unwrap();
    // Reverse the first-hit order, then retain all installed decisions while
    // alternating through function and ordinary-data representations.
    for reverse in [false, true] {
        GF_DISPATCH_CACHE.with(|cache| cache.borrow_mut().clear());
        for pass in 0..3 {
            for ordinal in 0..values.len() {
                let index = if reverse {
                    values.len() - ordinal - 1
                } else {
                    ordinal
                };
                let expected = if index == 0 {
                    function_answer
                } else if index <= 3 {
                    generic_answer
                } else {
                    other_answer
                };
                assert_eq!(
                    invoke_generic_function("CACHE901-TARGET", &[values[index]], &mut env).unwrap(),
                    expected,
                    "pass={pass}, index={index}, reverse={reverse}"
                );
            }
        }
        let target = egcl_rt::symbols::find_index("CACHE901-TARGET").unwrap();
        assert!(
            GF_DISPATCH_CACHE.with(|cache| cache
                .borrow()
                .keys()
                .filter(|key| key.gf == target)
                .count())
                >= 3,
            "the regression must exercise distinct populated cache entries"
        );
    }
}
