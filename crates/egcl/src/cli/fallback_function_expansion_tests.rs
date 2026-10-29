// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn fallback_function_freezes_local_scopes_and_preserves_moving_data() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    read_eval_all_env(
        "(defmacro moving-source () (%force-minor-gc-for-test) 42)
         (defmacro shadowed-call (value) (declare (ignore value)) :wrong)
         (defmacro unexpanded-load-form () :wrong)",
        &mut env,
    )
    .unwrap();
    const SOURCE: &str = "
      (defun frozen-scope-fixture (value)
        (declare (type (or integer null) value))
        (macrolet ((local-form (argument) (list 'list argument '(moving-source))))
          (symbol-macrolet ((alias value)) (local-form alias)))
        (flet ((shadowed-call (argument) argument)) (shadowed-call value))
        '(literal untouched)
        (load-time-value (unexpanded-load-form)))";
    // Intern all names and drain setup allocations before reading movable
    // source. A dead prefix avoids coincidental address reuse in two phases.
    let _ = read_from_string_in_env(SOURCE, &mut env).unwrap();
    egcl_rt::gc::collect_t0_minor().unwrap();
    let _ = arena_cons(NIL, NIL);
    egcl_rt::rooted!(source = read_from_string_in_env(SOURCE, &mut env).unwrap().0);
    egcl_rt::rooted!(parts = list_to_vec(*source));
    egcl_rt::rooted!(source_declaration = list_to_vec(parts[3]));
    let type_declaration = list_to_vec(source_declaration[1]);
    egcl_rt::rooted!(type_data = type_declaration[1]);
    egcl_rt::rooted!(quoted_data = cp(cp(parts[6]).1).0);
    egcl_rt::rooted!(load_form = parts[7]);
    let before_source = source.to_raw();
    let before_quote = quoted_data.to_raw();
    egcl_rt::rooted!(expanded = expand_load_source_function(*source, &env).unwrap());
    assert_ne!(
        source.to_raw(),
        before_source,
        "source must move during the macro callback"
    );
    assert_ne!(
        quoted_data.to_raw(),
        before_quote,
        "quoted data must really relocate"
    );
    egcl_rt::rooted!(output = list_to_vec(*expanded));
    egcl_rt::rooted!(output_declaration = list_to_vec(output[3]));
    assert_eq!(
        list_to_vec(output_declaration[1])[1],
        *type_data,
        "compound type syntax is data, retaining its exact identity"
    );
    egcl_rt::rooted!(value = cp(output[2]).0);
    assert_eq!(
        list_to_vec(output[4]),
        vec![
            EgclVal::from_symbol_index(reader::find_symbol_index("LIST").unwrap()),
            *value,
            EgclVal::from_fixnum(42)
        ],
        "both local macro scopes must be frozen, including symbol-macro arguments"
    );
    egcl_rt::rooted!(local_function = list_to_vec(output[5]));
    egcl_rt::rooted!(definition = cp(local_function[1]).0);
    egcl_rt::rooted!(local_name = cp(*definition).0);
    assert_eq!(
        list_to_vec(local_function[2]),
        vec![*local_name, *value],
        "lexical function binding must suppress the same-named global macro"
    );
    assert_eq!(
        cp(cp(output[6]).1).0,
        *quoted_data,
        "quoted values remain data"
    );
    assert_eq!(
        output[7], *load_form,
        "LOAD-TIME-VALUE remains a separate compilation boundary"
    );
}
