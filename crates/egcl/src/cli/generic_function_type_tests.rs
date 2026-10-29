// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn raw_and_reified_generic_wrappers_have_the_same_semantic_types() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defgeneric gf801-unit (object))", &mut env).unwrap();
    let name = resolve_sym("GF801-UNIT").unwrap();
    egcl_rt::rooted!(source = generic_fn_wrapper(&mut env, name, "GF801-UNIT"));
    egcl_rt::rooted!(function = coerce_installed_function(&env, *source));
    assert!(reified_closure(*function).is_some());
    egcl_rt::rooted!(metadata = env.generics.borrow()["GF801-UNIT"].generic_function);
    assert!(
        metadata.is_meta_handle(),
        "exercise the real stdlib GF representation"
    );
    egcl_rt::rooted!(values = vec![*source, *function, *metadata]);
    read_eval_all_env(
        "(setq *type-expanders*
           (cons (list 'gf801-unit-standard-alias
                       (function (lambda () 'standard-object)))
             (cons (list 'gf801-unit-compiled-alias
                         (function (lambda () 'compiled-function)))
                   *type-expanders*)))",
        &mut env,
    )
    .unwrap();
    let standard_alias = resolve_sym("GF801-UNIT-STANDARD-ALIAS").unwrap();
    let compiled_alias = resolve_sym("GF801-UNIT-COMPILED-ALIAS").unwrap();
    let compiled_type = resolve_sym("COMPILED-FUNCTION").unwrap();
    egcl_rt::rooted!(ordinary = builtin_fn_wrapper(&mut env, resolve_sym("CAR").unwrap(), "CAR"));
    assert!(value_satisfies_declared_type(compiled_type, *ordinary).unwrap());
    for index in 0..values.len() {
        assert!(is_generic_function_value(values[index]));
        assert!(!is_compiled_function_value(values[index]));
        assert!(!value_satisfies_declared_type(compiled_type, values[index]).unwrap());
        assert!(typep_matches(&mut env, values[index], standard_alias).unwrap());
        assert!(!typep_matches(&mut env, values[index], compiled_alias).unwrap());
        assert!(callable_type_matches(values[index], "STANDARD-OBJECT").unwrap());
        assert!(!callable_type_matches(values[index], "COMPILED-FUNCTION").unwrap());
        assert_eq!(
            class_of_value(values[index]),
            egcl_stdlib::find_class(resolve_sym("STANDARD-GENERIC-FUNCTION").unwrap()).unwrap()
        );
        for name in [
            "STANDARD-OBJECT",
            "GENERIC-FUNCTION",
            "STANDARD-GENERIC-FUNCTION",
        ] {
            egcl_rt::rooted!(class = egcl_stdlib::find_class(resolve_sym(name).unwrap()).unwrap());
            assert!(typep_matches(&mut env, values[index], *class).unwrap());
        }
    }
}
