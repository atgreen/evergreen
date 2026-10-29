// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn native_typep(type_name: &str, env: &mut Env) -> native_transfer_entry::TransferCode {
    egcl_rt::rooted!(
        params = super::super::read_from_string_in_env("(value)", env)
            .unwrap()
            .0
    );
    egcl_rt::rooted!(
        forms =
            super::super::read_from_string_in_env(&format!("((typep value '{type_name}))"), env,)
                .unwrap()
                .0
    );
    let body = Arc::new(
        compile_function("NATIVE-TYPEP-IDENTITY", *params, *forms, env, false, false)
            .expect("compile TYPEP bytecode"),
    );
    assert!(body.code.iter().any(|instruction| matches!(instruction,
        Instr::CallNamed { sym, nargs: 2 }
            if egcl_rt::symbols::symbol_name(*sym).as_deref() == Some("TYPEP"))));
    if type_name == "FIXNUM" {
        let canonical_index = egcl_rt::symbols::find_index("FIXNUM").unwrap();
        assert_eq!(
            egcl_rt::symbols::registry_key(canonical_index).as_deref(),
            Some("FIXNUM")
        );
        let call = body
            .code
            .iter()
            .position(|instruction| {
                matches!(instruction,
            Instr::CallNamed { sym, nargs: 2 }
                if egcl_rt::symbols::symbol_name(*sym).as_deref() == Some("TYPEP"))
            })
            .unwrap();
        let Instr::Const(index) = body.code[call - 1] else {
            panic!("canonical TYPEP fixture must have an exact constant type operand");
        };
        assert_eq!(
            body.constants[index as usize],
            EgclVal::from_symbol_index(canonical_index),
            "the minimal-environment fixture must use the canonical symbol, not a qualified lookalike"
        );
    }
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    // The cleanup-protocol test builder deliberately disables all intrinsics;
    // use production scope-free compilation so this exercises TYPEP selection.
    let code = native_transfer_entry::TransferCode::compile(body)
        .expect("admit and emit intrinsic-enabled native T1 TYPEP checker");
    assert!(
        code.mapped_entry().is_some() || code.ordinary_entry.is_some(),
        "fixture must have an actual generated entry"
    );
    code
}

// Production emission may choose ordinary or mapped entry ABI. Both counters
// observe calls leaving actual generated code, not activation preparation.
fn take_native_outcalls() -> usize {
    native_transfer_entry::take_published_checked_calls_for_test()
        + native_transfer_entry::ordinary_calls::take_named_entries_for_test()
        + native_transfer_entry::ordinary_calls::take_cold_calls_for_test()
}

#[test]
fn native_typep_calls_public_fallback_for_foreign_type_symbols() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        r#"
      (defpackage "NATIVE-TYPEP-IDENTITY" (:use "COMMON-LISP")
        (:shadow "FIXNUM" "SYMBOL" "INTEGER"))
    "#,
        &mut env,
    )
    .unwrap();
    // Env::new(false) deliberately omits boot Lisp, including DEFTYPE's macro.
    // Install exact-name zero-argument aliases through the supported legacy
    // registry fixture. Dynamic guards restore both registries on every exit;
    // public TYPEP still performs its normal alias lookup at runtime.
    egcl_rt::rooted!(
        definitions = super::super::read_from_string_in_env(
            "((native-typep-identity::fixnum (signed-byte 63)) \
          (native-typep-identity::symbol integer) \
          (native-typep-identity::integer symbol))",
            &mut env,
        )
        .unwrap()
        .0
    );
    let definitions_symbol = resolve_sym("*TYPE-DEFINITIONS*").unwrap();
    let expanders_symbol = resolve_sym("*TYPE-EXPANDERS*").unwrap();
    egcl_rt::rooted!(_definitions_binding = DynBind::establish(definitions_symbol, *definitions));
    egcl_rt::rooted!(_expanders_binding = DynBind::establish(expanders_symbol, NIL));
    egcl_rt::rooted!(big = egcl_rt::bignum::alloc_bignum_cli(1, &[1u64 << 61]));
    assert!(
        !big.is_fixnum(),
        "foreign FIXNUM alias must accept a real bignum"
    );
    let small = EgclVal::from_fixnum(42);
    egcl_rt::rooted!(values = [[*big, T], [small, T], [T, small]]);
    for (index, type_name) in [
        "native-typep-identity::fixnum",
        "native-typep-identity::symbol",
        "native-typep-identity::integer",
    ]
    .into_iter()
    .enumerate()
    {
        let type_symbol = super::super::read_from_string_in_env(type_name, &mut env)
            .unwrap()
            .0;
        assert!(
            super::super::typep_matches(&mut env, values[index][0], type_symbol).unwrap(),
            "cold public TYPEP must resolve the fixture alias: {type_name}"
        );
        assert!(!super::super::typep_matches(&mut env, values[index][1], type_symbol).unwrap());
        let code = native_typep(type_name, &mut env);
        for alternative in 0..2 {
            let value = values[index][alternative];
            let expected = if alternative == 0 { T } else { NIL };
            take_native_outcalls();
            assert_eq!(
                code.run(&[value], &mut env).unwrap(),
                expected,
                "{type_name}"
            );
            assert_eq!(
                code.deopt_count(),
                0,
                "foreign checker must not deopt to the interpreter"
            );
            assert!(
                take_native_outcalls() > 0,
                "foreign TYPEP must execute a generated-code public-call boundary: {type_name}"
            );
        }
    }
    // Canonical CL:FIXNUM still uses the native intrinsic. This also proves
    // that the observed call counter is not merely native activation setup.
    // Verify the exact canonical constant in native_typep instead of relying
    // on qualified-name resolution in this minimal environment fixture.
    let canonical = native_typep("FIXNUM", &mut env);
    for alternative in 0..2 {
        let value = if alternative == 0 { small } else { *big };
        let expected = if alternative == 0 { T } else { NIL };
        take_native_outcalls();
        assert_eq!(canonical.run(&[value], &mut env).unwrap(), expected);
        assert_eq!(
            canonical.deopt_count(),
            0,
            "canonical checker must execute without interpreter deoptimization"
        );
        assert_eq!(
            take_native_outcalls(),
            0,
            "canonical FIXNUM must retain its inline native type test"
        );
    }
}
