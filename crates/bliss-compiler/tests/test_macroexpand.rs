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
fn augment_function_does_not_mutate_original() {
    let env = Environment::null();
    let name = BlissVal::from_fixnum(15);
    let _env2 = env.augment_function(name, FunctionInfo::Lexical);
    assert!(env.function_information(name).is_none());
}

// ── Environment: declaration_information ─────────────────────────

#[test]
fn null_env_has_no_declaration_info() {
    let env = Environment::null();
    assert!(env.declaration_information(BlissVal::from_fixnum(20)).is_none());
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

// ── macroexpand ──────────────────────────────────────────────────

#[test]
fn macroexpand_non_macro_unchanged() {
    let env = Environment::null();
    let form = BlissVal::from_fixnum(100);
    let (expanded, did) = macroexpand(form, &env).unwrap();
    assert!(!did);
    assert_eq!(expanded, form);
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

// ── set_macroexpand_hook ─────────────────────────────────────────

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
