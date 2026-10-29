// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn legacy_generic_cache_is_normalized_before_exposure_and_preserves_bindings() {
    let _guard = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defgeneric gf800-cache (object)) (defmethod gf800-cache ((object t)) :retained)",
        &mut env,
    )
    .unwrap();
    let name = resolve_sym("GF800-CACHE").unwrap();
    egcl_rt::rooted!(initial = generic_fn_wrapper(&mut env, name, "GF800-CACHE"));
    assert!(egcl_rt::function::is_interpreted_function(*initial));
    egcl_rt::rooted!(legacy = reified_closure(*initial).expect("legacy source closure"));
    assert!(legacy.is_cons());
    // Model a legacy cache entry; this does not assume cross-build core compatibility.
    generic_wrapper_cache()
        .borrow_mut()
        .values
        .insert("GF800-CACHE".into(), *legacy);
    egcl_rt::rooted!(function = generic_fn_wrapper(&mut env, name, "GF800-CACHE"));
    assert!(egcl_rt::function::is_interpreted_function(*function));
    assert_eq!(generic_fn_wrapper(&mut env, name, "GF800-CACHE"), *function);
    assert_eq!(coerce_installed_function(&env, *function), *function);
    for value in [*legacy, *function] {
        assert!(is_generic_function_value(value));
        assert_eq!(generic_wrapper_name(value).as_deref(), Some("GF800-CACHE"));
    }
    egcl_rt::rooted!(representations = vec![*legacy, *function]);
    for index in 0..representations.len() {
        egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), representations[index]);
        assert!(generic_binding::current("GF800-CACHE"));
    }
    egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), egcl_rt::value::UNBOUND);
    assert!(generic_binding::undefined("GF800-CACHE"));
    assert!(!generic_binding::current("GF800-CACHE"));
    assert!(matches!(
        apply_function(name, &[NIL], &mut env),
        Err(EgclError::UndefinedFunction(_))
    ));
    assert_eq!(
        apply_function(*function, &[NIL], &mut env).unwrap(),
        resolve_sym(":RETAINED").unwrap()
    );
    egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), *function);
    assert!(generic_binding::current("GF800-CACHE"));
    assert_eq!(
        symbol_global_function_object(&mut env, name),
        Some(*function)
    );
    assert_eq!(
        generic_function_name_of(&env, *function).as_deref(),
        Some("GF800-CACHE")
    );
}
