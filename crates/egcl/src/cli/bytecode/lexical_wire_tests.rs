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
