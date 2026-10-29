// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn mapped_typep(type_name: &str, env: &mut Env) -> native_transfer_entry::TransferCode {
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
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    let code = native_transfer_entry::TransferCode::compile_mapped_for_test(body)
        .expect("admit and emit actual mapped T1 TYPEP checker");
    assert!(
        code.mapped_entry().is_some(),
        "fixture must have generated native code"
    );
    code
}

#[test]
fn mapped_native_typep_calls_public_fallback_for_foreign_type_symbols() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        r#"
      (defpackage "NATIVE-TYPEP-IDENTITY" (:use "COMMON-LISP")
        (:shadow "FIXNUM" "SYMBOL" "INTEGER"))
      (deftype native-typep-identity::fixnum () '(signed-byte 63))
      (deftype native-typep-identity::symbol () 'integer)
      (deftype native-typep-identity::integer () 'symbol)
    "#,
        &mut env,
    )
    .unwrap();
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
        let code = mapped_typep(type_name, &mut env);
        for alternative in 0..2 {
            let value = values[index][alternative];
            let expected = if alternative == 0 { T } else { NIL };
            native_transfer_entry::take_published_checked_calls_for_test();
            assert_eq!(
                code.run(&[value], &mut env).unwrap(),
                expected,
                "{type_name}"
            );
            assert!(
                native_transfer_entry::take_published_checked_calls_for_test() > 0,
                "foreign TYPEP must execute the generated-code checked-call boundary: {type_name}"
            );
        }
    }
    // Canonical CL:FIXNUM still uses the native intrinsic. This also proves
    // that the observed call counter is not merely native activation setup.
    let canonical = mapped_typep("cl:fixnum", &mut env);
    for alternative in 0..2 {
        let value = if alternative == 0 { small } else { *big };
        let expected = if alternative == 0 { T } else { NIL };
        native_transfer_entry::take_published_checked_calls_for_test();
        assert_eq!(canonical.run(&[value], &mut env).unwrap(), expected);
        assert_eq!(
            native_transfer_entry::take_published_checked_calls_for_test(),
            0,
            "canonical FIXNUM must retain its inline native type test"
        );
    }
}
