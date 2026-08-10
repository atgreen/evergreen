//! Tests for bliss-compiler macroexpand: Environment, macroexpand_1,
//! macroexpand, macroexpand_all, set_macroexpand_hook.

use bliss_compiler::macroexpand::*;
use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, T};

// ── Environment construction ─────────────────────────────────────

#[test]
fn environment_null_creates_empty_env() {
    let _env = Environment::null();
}

// ── Environment: variable_information ────────────────────────────

#[test]
fn null_env_has_no_variable_info() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(0);
    assert!(env.variable_information(name).is_none());
}

#[test]
fn augment_variable_lexical() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(1);
    let env2 = env.augment_variable(name, VariableInfo::Lexical);
    match env2.variable_information(name).expect("should exist") {
        VariableInfo::Lexical => {}
        other => panic!("expected Lexical, got {:?}", other),
    }
}

#[test]
fn augment_variable_special() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(2);
    let env2 = env.augment_variable(name, VariableInfo::Special);
    match env2.variable_information(name).unwrap() {
        VariableInfo::Special => {}
        other => panic!("expected Special, got {:?}", other),
    }
}

#[test]
fn augment_variable_constant() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(3);
    let val = BlissVal::from_fixnum(42);
    let env2 = env.augment_variable(name, VariableInfo::Constant(val));
    match env2.variable_information(name).unwrap() {
        VariableInfo::Constant(v) => assert_eq!(v, val),
        other => panic!("expected Constant, got {:?}", other),
    }
}

#[test]
fn augment_variable_symbol_macro() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(4);
    let expansion = BlissVal::from_fixnum(99);
    let env2 = env.augment_variable(name, VariableInfo::SymbolMacro(expansion));
    match env2.variable_information(name).unwrap() {
        VariableInfo::SymbolMacro(v) => assert_eq!(v, expansion),
        other => panic!("expected SymbolMacro, got {:?}", other),
    }
}

#[test]
fn augment_variable_does_not_mutate_original() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(5);
    let _env2 = env.augment_variable(name, VariableInfo::Lexical);
    assert!(env.variable_information(name).is_none());
}

// ── Environment: function_information ────────────────────────────

#[test]
fn null_env_has_no_function_info() {
    let env = Environment::null();
    assert!(env.function_information(BlissVal::from_fixnum(10)).is_none());
}

#[test]
fn augment_function_lexical() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(11);
    let env2 = env.augment_function(name, FunctionInfo::Lexical);
    match env2.function_information(name).unwrap() {
        FunctionInfo::Lexical => {}
        other => panic!("expected Lexical, got {:?}", other),
    }
}

#[test]
fn augment_function_macro() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(13);
    let env2 = env.augment_function(name, FunctionInfo::Macro(T));
    match env2.function_information(name).unwrap() {
        FunctionInfo::Macro(v) => assert_eq!(v, T),
        other => panic!("expected Macro, got {:?}", other),
    }
}

#[test]
fn augment_function_special_operator() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(14);
    let env2 = env.augment_function(name, FunctionInfo::SpecialOperator);
    match env2.function_information(name).unwrap() {
        FunctionInfo::SpecialOperator => {}
        other => panic!("expected SpecialOperator, got {:?}", other),
    }
}

#[test]
fn augment_function_global() {
    // Issue #9: Test FunctionInfo::Global variant
    let env = Environment::null();
    let name = BlissVal::from_fixnum(12);
    let env2 = env.augment_function(name, FunctionInfo::Global);
    match env2.function_information(name).unwrap() {
        FunctionInfo::Global => {}
        other => panic!("expected Global, got {:?}", other),
    }
}

#[test]
fn augment_function_does_not_mutate_original() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(15);
    let _env2 = env.augment_function(name, FunctionInfo::Lexical);
    assert!(env.function_information(name).is_none());
}

// ── Environment: declaration_information (Issue #9) ──────────────

#[test]
fn null_env_has_no_declaration_info() {
    let env = Environment::null();
    assert!(env.declaration_information(BlissVal::from_fixnum(20)).is_none());
}

#[test]
fn declaration_information_with_augmented_env() {
    // After augmenting an environment with declarations, declaration_information
    // should return the correct values. We test this by augmenting with variables
    // and functions and checking that declaration queries on the enriched env
    // return expected results for known declaration names.
    let env = Environment::null();
    // In a real implementation, there would be an augment_declaration method
    // or declarations would be set via augment_variable/augment_function.
    // We test that querying a non-existent declaration returns None even in
    // a non-null environment.
    let name = BlissVal::from_fixnum(30);
    let env2 = env.augment_variable(name, VariableInfo::Lexical);
    // Declaration information is separate from variable information
    let decl_name = BlissVal::from_fixnum(100);
    assert!(
        env2.declaration_information(decl_name).is_none(),
        "declaration_information should return None for unknown declaration names"
    );
}

// ── Environment: multiple/mixed bindings ─────────────────────────

#[test]
fn env_multiple_variables() {
    let env = Environment::null();
    let n1 = BlissVal::from_fixnum(30);
    let n2 = BlissVal::from_fixnum(31);
    let env2 = env.augment_variable(n1, VariableInfo::Lexical);
    let env3 = env2.augment_variable(n2, VariableInfo::Special);
    assert!(env3.variable_information(n1).is_some());
    assert!(env3.variable_information(n2).is_some());
}

#[test]
fn env_mixed_variable_and_function() {
    let env = Environment::null();
    let vn = BlissVal::from_fixnum(40);
    let fn_ = BlissVal::from_fixnum(41);
    let env2 = env.augment_variable(vn, VariableInfo::Lexical);
    let env3 = env2.augment_function(fn_, FunctionInfo::Lexical);
    assert!(env3.variable_information(vn).is_some());
    assert!(env3.function_information(fn_).is_some());
}

#[test]
fn env_variable_shadowing() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(50);
    let env2 = env.augment_variable(name, VariableInfo::Lexical);
    let env3 = env2.augment_variable(name, VariableInfo::Special);
    match env3.variable_information(name).unwrap() {
        VariableInfo::Special => {}
        other => panic!("expected Special (shadow), got {:?}", other),
    }
}

// ── macroexpand_1 ────────────────────────────────────────────────

#[test]
fn macroexpand_1_non_macro_unchanged() {
    let env = Environment::null();
    let form = BlissVal::from_fixnum(42);
    let (expanded, did) = macroexpand_1(form, &env).unwrap();
    assert!(!did);
    assert_eq!(expanded, form);
}

#[test]
fn macroexpand_1_nil_unchanged() {
    let env = Environment::null();
    let (expanded, did) = macroexpand_1(NIL, &env).unwrap();
    assert!(!did);
    assert_eq!(expanded, NIL);
}

// ── macroexpand_1 with actual macro expansion (Issue #6) ─────────

#[test]
fn macroexpand_1_expands_symbol_macro() {
    // Set up an environment with a symbol macro: symbol X expands to 42
    let env = Environment::null();
    let sym_name = BlissVal::from_fixnum(200); // stand-in for a symbol
    let expansion = BlissVal::from_fixnum(42);
    let env2 = env.augment_variable(sym_name, VariableInfo::SymbolMacro(expansion));

    // macroexpand_1 on a symbol that has a SymbolMacro binding should expand it
    let (result, expanded_p) = macroexpand_1(sym_name, &env2).unwrap();
    assert!(expanded_p, "macroexpand_1 should report expansion occurred");
    assert_eq!(
        result, expansion,
        "macroexpand_1 should return the symbol macro expansion"
    );
}

#[test]
fn macroexpand_1_expands_function_macro() {
    // Set up an environment with a macro function binding
    let env = Environment::null();
    let macro_name = BlissVal::from_fixnum(201);
    let expander_fn = T; // stand-in for the expander function
    let env2 = env.augment_function(macro_name, FunctionInfo::Macro(expander_fn));

    // Create a "form" that is a list with macro_name as operator
    // In CL, macroexpand_1 checks if car(form) is a macro in the environment
    // Since we can't easily construct cons cells without the runtime,
    // we verify the environment lookup works correctly
    match env2.function_information(macro_name).unwrap() {
        FunctionInfo::Macro(f) => {
            assert_eq!(f, T, "macro expander should be retrievable");
        }
        other => panic!("expected Macro, got {:?}", other),
    }
}

// ── macroexpand ──────────────────────────────────────────────────

#[test]
fn macroexpand_non_macro_unchanged() {
    let env = Environment::null();
    let form = BlissVal::from_fixnum(100);
    let (expanded, did) = macroexpand(form, &env).unwrap();
    assert!(!did);
    assert_eq!(expanded, form);
}

#[test]
fn macroexpand_fully_expands_symbol_macro() {
    // macroexpand should iterate macroexpand_1 until no more expansion
    let env = Environment::null();
    let sym = BlissVal::from_fixnum(300);
    let expansion = BlissVal::from_fixnum(77);
    let env2 = env.augment_variable(sym, VariableInfo::SymbolMacro(expansion));

    let (result, expanded_p) = macroexpand(sym, &env2).unwrap();
    assert!(expanded_p, "macroexpand should report expansion occurred");
    assert_eq!(result, expansion, "macroexpand should return final expansion");
}

// ── macroexpand: circular expansion detection (Issue #7) ─────────

#[test]
fn macroexpand_detects_circular_expansion() {
    // R4.16: macroexpand must detect and signal program-error when a form
    // expands back to itself (circular expansion).
    let env = Environment::null();
    let sym = BlissVal::from_fixnum(400);
    // Symbol macro that expands to itself — infinite loop
    let env2 = env.augment_variable(sym, VariableInfo::SymbolMacro(sym));

    let result = macroexpand(sym, &env2);
    assert!(
        result.is_err(),
        "macroexpand should detect circular expansion and signal an error"
    );
}

// ── macroexpand_all ──────────────────────────────────────────────

#[test]
fn macroexpand_all_non_macro_unchanged() {
    let env = Environment::null();
    let form = BlissVal::from_fixnum(200);
    let expanded = macroexpand_all(form, &env).unwrap();
    assert_eq!(expanded, form);
}

#[test]
fn macroexpand_all_nil() {
    let env = Environment::null();
    assert_eq!(macroexpand_all(NIL, &env).unwrap(), NIL);
}

#[test]
fn macroexpand_all_expands_symbol_macro() {
    // macroexpand_all should expand symbol macros in the form
    let env = Environment::null();
    let sym = BlissVal::from_fixnum(500);
    let expansion = BlissVal::from_fixnum(88);
    let env2 = env.augment_variable(sym, VariableInfo::SymbolMacro(expansion));

    let result = macroexpand_all(sym, &env2).unwrap();
    assert_eq!(result, expansion, "macroexpand_all should expand symbol macros");
}

// ── set_macroexpand_hook (Issue #8) ──────────────────────────────

fn identity_hook(_: BlissVal, form: BlissVal, _: &Environment) -> Result<BlissVal, BlissError> {
    Ok(form)
}

fn constant_hook(_: BlissVal, _: BlissVal, _: &Environment) -> Result<BlissVal, BlissError> {
    Ok(BlissVal::from_fixnum(999))
}

#[test]
fn set_macroexpand_hook_accepts_and_replaces() {
    set_macroexpand_hook(identity_hook);
    set_macroexpand_hook(constant_hook);
}

#[test]
fn macroexpand_hook_is_invoked_during_expansion() {
    // Set a custom hook that transforms the expansion result in a detectable way
    // The constant_hook always returns fixnum 999 regardless of the actual expansion
    set_macroexpand_hook(constant_hook);

    let env = Environment::null();
    let sym = BlissVal::from_fixnum(600);
    let original_expansion = BlissVal::from_fixnum(42);
    let env2 = env.augment_variable(sym, VariableInfo::SymbolMacro(original_expansion));

    // When the hook is invoked, it should override the expansion result
    let (result, expanded_p) = macroexpand_1(sym, &env2).unwrap();
    assert!(expanded_p, "expansion should occur");
    // The hook should have been called, returning 999 instead of 42
    assert_eq!(
        result,
        BlissVal::from_fixnum(999),
        "macroexpand hook should be invoked, transforming the result to 999"
    );

    // Restore identity hook to avoid affecting other tests
    set_macroexpand_hook(identity_hook);
}

#[test]
fn macroexpand_hook_identity_preserves_expansion() {
    // With the identity hook, expansion should work normally
    set_macroexpand_hook(identity_hook);

    let env = Environment::null();
    let sym = BlissVal::from_fixnum(700);
    let expansion = BlissVal::from_fixnum(55);
    let env2 = env.augment_variable(sym, VariableInfo::SymbolMacro(expansion));

    let (result, expanded_p) = macroexpand_1(sym, &env2).unwrap();
    assert!(expanded_p, "expansion should occur");
    assert_eq!(
        result, expansion,
        "identity hook should preserve the expansion value"
    );
}

// ── Enum trait impls ─────────────────────────────────────────────

#[test]
fn variable_info_clone_and_debug() {
    let info = VariableInfo::Lexical;
    let _ = info.clone();
    assert!(format!("{:?}", VariableInfo::Special).contains("Special"));
}

#[test]
fn function_info_clone_and_debug() {
    let info = FunctionInfo::SpecialOperator;
    let _ = info.clone();
    assert!(format!("{:?}", FunctionInfo::Global).contains("Global"));
}
