// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn lexical_keys_distinguish_same_names_and_escape_reserved_ordinary_names() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(first = egcl_rt::symbols::make_uninterned("G"));
    egcl_rt::rooted!(second = egcl_rt::symbols::make_uninterned("G"));
    let first_key = lexical_identity::key(*first);
    let second_key = lexical_identity::key(*second);
    assert_ne!(first_key, second_key);
    assert_eq!(lexical_identity::symbol(&first_key), Some(*first));
    assert_eq!(lexical_identity::symbol(&second_key), Some(*second));
    assert_eq!(lexical_identity::resolve(&first_key), Some(*first));
    assert_eq!(lexical_identity::index(&second_key), second.symbol_index());
    // Legal names may contain NUL and look exactly like either private tag.
    for name in [first_key.as_str(), "\0egcl-lexical:name:G", "G"] {
        let escaped = lexical_identity::ordinary(name);
        assert_eq!(lexical_identity::name(&escaped), name);
        assert!(lexical_identity::symbol_index(&escaped).is_none());
        assert_ne!(escaped, first_key);
        assert_ne!(escaped, second_key);
    }
    let lookalike = EgclVal::from_symbol_index(egcl_rt::symbols::intern(&first_key));
    let lookalike_key = lexical_identity::key(lookalike);
    assert_ne!(lookalike_key, first_key);
    assert_eq!(lexical_identity::resolve(&lookalike_key), Some(lookalike));
    assert_eq!(
        lexical_identity::index(&lookalike_key),
        lookalike.symbol_index()
    );
}

fn symbol_name_value(index: u32) -> EgclVal {
    let object = egcl_rt::symbols::symbol_object_ptr(index).expect("live symbol object");
    unsafe { (*(object as *const egcl_rt::object::SymbolData)).name }
}

#[test]
fn bytecode_lexical_keys_alone_retain_symbols_and_relocate_their_names() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(forms = arena_cons(NIL, NIL));
    let mut body = compile_function("LEXICAL-KEY-ROOT-TEST", NIL, *forms, &env, false, false)
        .expect("compile empty fixture");
    // Drain setup/pinned allocations before the symbols' fresh name strings.
    egcl_rt::gc::collect_t0_minor().unwrap();
    // Dead prefix prevents a two-phase collection from coincidentally reusing
    // the names' original offsets when it reuses the original nursery region.
    let _ = arena_cons(NIL, NIL);
    let (name_index, parameter_index, control_index, before_name, before_parameter) = {
        egcl_rt::rooted!(name = egcl_rt::symbols::make_uninterned("KEY-ONLY-NAME"));
        egcl_rt::rooted!(parameter = egcl_rt::symbols::make_uninterned("KEY-ONLY-PARAMETER"));
        let control = egcl_rt::symbols::make_uninterned("UNOWNED-KEY-CONTROL");
        body.names = vec![lexical_identity::key(*name)];
        body.code = vec![Instr::LoadEnvVar(0), Instr::Return];
        body.param_layout = vec![(lexical_identity::key(*parameter), VarLoc::Boxed)];
        body.params_form = NIL;
        body.constants.clear();
        body.load_time_values.clear();
        body.nested_functions.clear();
        body.restart_cases.clear();
        let name_index = name.as_symbol_index();
        let parameter_index = parameter.as_symbol_index();
        (
            name_index,
            parameter_index,
            control.as_symbol_index(),
            symbol_name_value(name_index),
            symbol_name_value(parameter_index),
        )
    };
    let body = Arc::new(body);
    // No symbol-valued parameter/source/constant survives. The only live edges
    // are the encoded names used by an instruction and the parameter layout.
    let roots = ActiveBytecodeRoot::new(&body);
    egcl_rt::rooted!(roots = roots);
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(
        egcl_rt::symbols::symbol_name(name_index).as_deref(),
        Some("KEY-ONLY-NAME")
    );
    assert_eq!(
        egcl_rt::symbols::symbol_name(parameter_index).as_deref(),
        Some("KEY-ONLY-PARAMETER")
    );
    assert_ne!(
        symbol_name_value(name_index),
        before_name,
        "key-only instruction symbol name must actually relocate"
    );
    assert_ne!(
        symbol_name_value(parameter_index),
        before_parameter,
        "key-only parameter symbol name must actually relocate"
    );
    assert!(
        egcl_rt::symbols::symbol_name(control_index).is_none(),
        "an unrelated unowned symbol must be reclaimed"
    );
    assert_eq!(
        lexical_identity::symbol_index(&body.names[0]),
        Some(name_index)
    );
    assert_eq!(
        lexical_identity::symbol_index(&body.param_layout[0].0),
        Some(parameter_index)
    );
    drop(roots);
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        egcl_rt::symbols::symbol_name(name_index).is_none(),
        "a Rust Arc without an active bytecode owner is not a Lisp root"
    );
    assert!(egcl_rt::symbols::symbol_name(parameter_index).is_none());
}

#[test]
fn function_cell_keys_alone_retain_symbols_and_relocate_their_names() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(forms = arena_cons(NIL, NIL));
    let mut body = compile_function("LEXICAL-KEY-ROOT-TEST", NIL, *forms, &env, false, false)
        .expect("compile empty fixture");
    // Drain setup/pinned allocations before the symbols' fresh name strings.
    egcl_rt::gc::collect_t0_minor().unwrap();
    // Dead prefix prevents a two-phase collection from coincidentally reusing
    // the names' original offsets when it reuses the original nursery region.
    let _ = arena_cons(NIL, NIL);
    let (name_index, parameter_index, control_index, before_name, before_parameter) = {
        egcl_rt::rooted!(name = egcl_rt::symbols::make_uninterned("KEY-ONLY-NAME"));
        egcl_rt::rooted!(parameter = egcl_rt::symbols::make_uninterned("KEY-ONLY-PARAMETER"));
        let control = egcl_rt::symbols::make_uninterned("UNOWNED-KEY-CONTROL");
        body.names = vec![lexical_identity::function(&lexical_identity::key(*name))];
        body.code = vec![Instr::LoadEnvVar(0), Instr::Return];
        body.param_layout = vec![(lexical_identity::key(*parameter), VarLoc::Boxed)];
        body.params_form = NIL;
        body.constants.clear();
        body.load_time_values.clear();
        body.nested_functions.clear();
        body.restart_cases.clear();
        let name_index = name.as_symbol_index();
        let parameter_index = parameter.as_symbol_index();
        (
            name_index,
            parameter_index,
            control.as_symbol_index(),
            symbol_name_value(name_index),
            symbol_name_value(parameter_index),
        )
    };
    let body = Arc::new(body);
    // No symbol-valued parameter/source/constant survives. The only live edges
    // are the encoded names used by an instruction and the parameter layout.
    let roots = ActiveBytecodeRoot::new(&body);
    egcl_rt::rooted!(roots = roots);
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(
        egcl_rt::symbols::symbol_name(name_index).as_deref(),
        Some("KEY-ONLY-NAME")
    );
    assert_eq!(
        egcl_rt::symbols::symbol_name(parameter_index).as_deref(),
        Some("KEY-ONLY-PARAMETER")
    );
    assert_ne!(
        symbol_name_value(name_index),
        before_name,
        "key-only instruction symbol name must actually relocate"
    );
    assert_ne!(
        symbol_name_value(parameter_index),
        before_parameter,
        "key-only parameter symbol name must actually relocate"
    );
    assert!(
        egcl_rt::symbols::symbol_name(control_index).is_none(),
        "an unrelated unowned symbol must be reclaimed"
    );
    assert_eq!(
        lexical_identity::owned_symbol(&body.names[0]).and_then(EgclVal::symbol_index),
        Some(name_index)
    );
    assert_eq!(
        lexical_identity::symbol_index(&body.param_layout[0].0),
        Some(parameter_index)
    );
    drop(roots);
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        egcl_rt::symbols::symbol_name(name_index).is_none(),
        "a Rust Arc without an active bytecode owner is not a Lisp root"
    );
    assert!(egcl_rt::symbols::symbol_name(parameter_index).is_none());
}

#[test]
fn captured_function_cell_value_moves_without_aliasing_same_symbol_variable() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(symbol = egcl_rt::symbols::make_uninterned("MATCH-BODY"));
    let variable = lexical_identity::key(*symbol);
    let function = lexical_identity::function(&variable);
    env.define_lexical_local(&variable, EgclVal::from_fixnum(3));
    egcl_rt::gc::collect_t0_minor().unwrap();
    let before = arena_cons(EgclVal::from_fixnum(4), NIL);
    env.define_lexical_local(&function, before);
    egcl_rt::gc::collect_t0_minor().unwrap();
    let after = Env::lookup_lexical_frame(&env.frame, &function).unwrap();
    assert_ne!(
        before, after,
        "captured FUNCTION-cell payload must actually relocate"
    );
    assert_eq!(cp(after).0, EgclVal::from_fixnum(4));
    assert_eq!(
        Env::lookup_lexical_frame(&env.frame, &variable),
        Some(EgclVal::from_fixnum(3))
    );
}
