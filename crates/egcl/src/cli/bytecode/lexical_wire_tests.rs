// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn parsed(pool: &BbuConstPool) -> Vec<BbuConstant> {
    pool.entries
        .iter()
        .map(|bytes| {
            let mut cursor = BbuCursor::new(bytes);
            let value = parse_bbu_constant(&mut cursor, BBU_BYTECODE_VERSION).unwrap();
            assert!(cursor.done());
            value
        })
        .collect()
}

#[test]
fn portable_lexical_symbols_remap_once_and_image_keys_keep_saved_identity() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(first = egcl_rt::symbols::make_uninterned("G"));
    egcl_rt::rooted!(second = egcl_rt::symbols::make_uninterned("G"));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool_root = &mut pool);
    let first_ref = pool.lexical_name(&var_name(*first)).unwrap();
    assert_eq!(pool.lexical_name(&var_name(*first)), Some(first_ref));
    let second_ref = pool.lexical_name(&var_name(*second)).unwrap();
    assert_ne!(first_ref, second_ref);
    let encoded = parsed(&pool);
    assert!(matches!(
        encoded[first_ref as usize],
        BbuConstant::UninternedSymbol(_)
    ));
    egcl_rt::rooted!(values = materialize_bbu_constants(&encoded).unwrap());
    let restored =
        lexical_identity::symbol(&bbu_lexical_key_from_values(&values, first_ref, 0x0301).unwrap())
            .unwrap();
    assert_ne!(restored, *first);
    assert_ne!(values[first_ref as usize], values[second_ref as usize]);
    assert_eq!(
        bbu_lexical_key_from_values(&values, first_ref, 0x0301).unwrap(),
        var_name(restored)
    );
    assert!(bbu_lexical_key_from_values(&values, first_ref, 0x0300).is_err());

    let mut image = BbuConstPool {
        saved_heap_refs: true,
        ..Default::default()
    };
    egcl_rt::rooted_ref!(_image_root = &mut image);
    let image_ref = image.lexical_name(&var_name(*first)).unwrap();
    assert_eq!(image.value(*first), Some(image_ref));
    egcl_rt::rooted!(params = arena_cons(*first, NIL));
    let params_ref = image.value(*params).unwrap();
    match &parsed(&image)[image_ref as usize] {
        BbuConstant::SavedHeapRef(raw) => assert_eq!(*raw, first.to_raw()),
        other => panic!("image changed lexical registry identity: {other:?}"),
    }
    let image_constants = parsed(&image);
    egcl_rt::rooted!(image_values = materialize_bbu_constants(&image_constants).unwrap());
    assert_eq!(cp(image_values[params_ref as usize]).0, *first);
    assert_eq!(
        bbu_lexical_key_from_values(&image_values, image_ref, 0x0301).unwrap(),
        var_name(*first)
    );
    assert!(validate_bbu_lexical_name(&image_constants, image_ref, 0x0301).is_ok());
    assert!(validate_bbu_lexical_name(&image_constants, image_ref, 0x0300).is_err());
}

#[test]
fn lexical_wire_prevalidation_rejects_bad_kinds_versions_and_truncation() {
    let constants = vec![
        BbuConstant::String("G".into()),
        BbuConstant::UninternedSymbol(0),
        BbuConstant::Nil,
    ];
    for opcode in [0x3c, 0x3d, 0x3e] {
        let mut code = vec![opcode];
        put_u32(&mut code, 0);
        assert!(validate_bbu_lexical_operands(&code, &constants, 0x0300).is_ok());
        code[1..].copy_from_slice(&1u32.to_le_bytes());
        assert!(validate_bbu_lexical_operands(&code, &constants, 0x0301).is_ok());
        assert!(validate_bbu_lexical_operands(&code, &constants, 0x0300).is_err());
        for reference in [2u32, 3, u32::MAX] {
            code[1..].copy_from_slice(&reference.to_le_bytes());
            assert!(validate_bbu_lexical_operands(&code, &constants, 0x0301).is_err());
        }
        for length in 1..5 {
            assert!(validate_bbu_lexical_operands(&code[..length], &constants, 0x0301).is_err());
        }
    }
    assert!(validate_bbu_lexical_name(&[BbuConstant::UninternedSymbol(1)], 0, 0x0301).is_err());
    assert!(
        validate_bbu_lexical_name(&[BbuConstant::SavedHeapRef(NIL.to_raw())], 0, 0x0301).is_err()
    );
}

#[test]
fn lexical_prevalidation_walks_control_names_without_interpreting_them_as_variables() {
    let constants = vec![BbuConstant::String("\0egcl-lexical:symbol:123".into())];
    let mut code = vec![0x3f]; // RETURN-FROM-NAMED: name
    put_u32(&mut code, 0);
    code.push(0x40); // GO-NAMED: name
    put_u32(&mut code, 0);
    code.push(0x41); // NAMED-TAG: name, bytecode target
    put_u32(&mut code, 0);
    put_u32(&mut code, 0);
    code.push(0x3c);
    put_u32(&mut code, 0);
    assert!(validate_bbu_lexical_operands(&code, &constants, 0x0301).is_ok());
    let last_operand = code.len() - 4;
    code[last_operand..].copy_from_slice(&1u32.to_le_bytes());
    assert!(validate_bbu_lexical_operands(&code, &constants, 0x0301).is_err());
}

#[test]
fn macro_optional_and_keyword_supplied_variables_keep_exact_identity() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    egcl_rt::rooted!(variables = Vec::<EgclVal>::new());
    for _ in 0..4 {
        variables.push(egcl_rt::symbols::make_uninterned("G"));
    }
    egcl_rt::rooted!(optional = form_list(&[variables[0], NIL, variables[1]]));
    egcl_rt::rooted!(keyword = resolve_sym(":VALUE").unwrap());
    egcl_rt::rooted!(key_pair = form_list(&[*keyword, variables[2]]));
    egcl_rt::rooted!(key_spec = form_list(&[*key_pair, NIL, variables[3]]));
    egcl_rt::rooted!(optional_marker = resolve_sym("&OPTIONAL").unwrap());
    egcl_rt::rooted!(key_marker = resolve_sym("&KEY").unwrap());
    egcl_rt::rooted!(params = form_list(&[*optional_marker, *optional, *key_marker, *key_spec]));
    super::super::bind_macro_lambda_list(
        *params,
        &[EgclVal::from_fixnum(41), *keyword, EgclVal::from_fixnum(42)],
        &mut env,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        env.lookup_var_symbol(variables[0]),
        Some(EgclVal::from_fixnum(41))
    );
    assert_eq!(env.lookup_var_symbol(variables[1]), Some(T));
    assert_eq!(
        env.lookup_var_symbol(variables[2]),
        Some(EgclVal::from_fixnum(42))
    );
    assert_eq!(env.lookup_var_symbol(variables[3]), Some(T));
    let (names, ..) = parse_macro_lambda_list(*params).unwrap();
    assert_eq!(
        names,
        variables.iter().copied().map(var_name).collect::<Vec<_>>()
    );
}

#[test]
fn portable_control_names_require_strings_without_lexical_key_decoding() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // Control names retain their original string namespace, even when a legal
    // name resembles an encoded lexical variable. Symbols are not accepted as
    // an alternate wire representation in this namespace.
    let name = "\0egcl-lexical:symbol:2147483648";
    egcl_rt::rooted!(constants = vec![arena_str(name)]);
    egcl_rt::rooted!(symbol = egcl_rt::symbols::make_uninterned(name));
    constants.push(*symbol);
    constants.push(EgclVal::from_fixnum(42));
    constants.push(NIL);
    egcl_rt::rooted!(cons = arena_cons(NIL, NIL));
    constants.push(*cons);
    for opcode in [0x3f, 0x40, 0x41] {
        let mut encoded = EncodedBbuFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            name_ref: BBU_NO_INDEX,
            lambda_list_ref: BBU_NO_INDEX,
            flags: 0,
            arity_min: 0,
            arity_max: 0,
            n_locals: 0,
            max_stack: 0,
            code: vec![opcode],
            literal_refs: vec![],
            metadata: None,
        };
        put_u32(&mut encoded.code, 0);
        if opcode == 0x41 {
            put_u32(&mut encoded.code, 0); // Valid instruction-boundary target.
        }
        let decoded = decode_bbu_function(&encoded, &constants, &[], &[], 0x0301)
            .expect("a string control name remains supported");
        assert_eq!(decoded.names, vec![name.to_owned()]);
        for reference in 1..constants.len() as u32 {
            encoded.code[1..5].copy_from_slice(&reference.to_le_bytes());
            assert!(
                decode_bbu_function(&encoded, &constants, &[], &[], 0x0301).is_err(),
                "opcode={opcode:#x}, non-string reference={reference}"
            );
        }
    }
}

#[test]
fn function_name_wire_remaps_symbols_and_preserves_namespace_separation() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(first = egcl_rt::symbols::make_uninterned("MATCH-BODY"));
    egcl_rt::rooted!(second = egcl_rt::symbols::make_uninterned("MATCH-BODY"));
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let variable_ref = pool.lexical_name(&var_name(*first)).unwrap();
    let first_ref = pool
        .lexical_name(&closure_fn_binding_key(&var_name(*first)))
        .unwrap();
    let second_ref = pool
        .lexical_name(&closure_fn_binding_key(&var_name(*second)))
        .unwrap();
    assert_eq!(first_ref & !BBU_FUNCTION_NAME_REF, variable_ref);
    assert_ne!(first_ref, second_ref);
    let constants = parsed(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    let variable = bbu_lexical_key_from_values(&values, variable_ref, 0x0303).unwrap();
    let function = bbu_lexical_key_from_values(&values, first_ref, 0x0303).unwrap();
    assert_eq!(
        lexical_identity::function_base(&function),
        Some(variable.as_str())
    );
    assert_ne!(lexical_identity::owned_symbol(&function), Some(*first));
    assert!(lexical_identity::symbol_index(&function).is_none());
    assert!(validate_bbu_lexical_name(&constants, first_ref, 0x0302).is_err());
    assert!(validate_bbu_parameter_name(&constants, first_ref, 0x0303).is_err());
    assert!(validate_bbu_parameter_name(&constants, variable_ref, 0x0303).is_ok());
    for bad in [BBU_FUNCTION_NAME_REF | constants.len() as u32, u32::MAX] {
        assert!(validate_bbu_lexical_name(&constants, bad, 0x0303).is_err());
    }
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    env.define_lexical_local(&variable, EgclVal::from_fixnum(3));
    env.define_lexical_local(&function, EgclVal::from_fixnum(4));
    assert!(env.funs.borrow().is_empty());
    let macro_env = super::super::macroexpand_environment_from_cli(&env);
    assert!(matches!(
        macro_env.function_information(lexical_identity::owned_symbol(&function).unwrap()),
        Some(compiler_macroexpand::FunctionInfo::Lexical)
    ));
    assert!(
        macro_env
            .function_information(values[(second_ref & !BBU_FUNCTION_NAME_REF) as usize])
            .is_none()
    );
    assert_eq!(
        Env::lookup_lexical_frame(&env.frame, &variable),
        Some(EgclVal::from_fixnum(3))
    );
    assert_eq!(
        Env::lookup_lexical_frame(&env.frame, &function),
        Some(EgclVal::from_fixnum(4))
    );
    assert!(Env::set_lexical_frame(
        &env.frame,
        &function,
        EgclVal::from_fixnum(5)
    ));
    assert_eq!(
        Env::lookup_lexical_frame(&env.frame, &variable),
        Some(EgclVal::from_fixnum(3))
    );
}

#[test]
fn reserved_function_namespace_spelling_is_an_ordinary_variable_on_wire() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let spelling = "\0egcl-lexical:function:G";
    let variable = lexical_identity::ordinary(spelling);
    let function = lexical_identity::function("G");
    assert_ne!(variable, function);
    let mut pool = BbuConstPool::default();
    egcl_rt::rooted_ref!(_pool = &mut pool);
    let reference = pool.lexical_name(&variable).unwrap();
    assert_eq!(reference & BBU_FUNCTION_NAME_REF, 0);
    let constants = parsed(&pool);
    egcl_rt::rooted!(values = materialize_bbu_constants(&constants).unwrap());
    assert_eq!(
        bbu_lexical_key_from_values(&values, reference, 0x0303).unwrap(),
        variable
    );
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    env.define_lexical_local(&variable, EgclVal::from_fixnum(11));
    env.define_lexical_local(&function, EgclVal::from_fixnum(22));
    let frozen = super::super::freeze_env_frame(&env.frame);
    let thawed = super::super::thaw_env_frame(&frozen);
    assert_eq!(
        Env::lookup_lexical_frame(&thawed, &variable),
        Some(EgclVal::from_fixnum(11))
    );
    assert_eq!(
        Env::lookup_lexical_frame(&thawed, &function),
        Some(EgclVal::from_fixnum(22))
    );
}
