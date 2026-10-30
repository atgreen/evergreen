// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

// Native exceptional exits can already have signalled the public condition.
// Check its type and datum rather than assuming the pre-signal Rust variant.
fn assert_type_error_datum(error: &EgclError, expected: EgclVal, env: &Env) {
    egcl_rt::rooted!(expected = expected);
    egcl_rt::rooted!(_type_error = resolve_sym("TYPE-ERROR").unwrap());
    egcl_rt::rooted!(datum_slot = resolve_sym("DATUM").unwrap());
    match error.without_backtrace() {
        EgclError::TypeError { datum, .. } => assert_eq!(*datum, *expected),
        EgclError::Signalled(details) => {
            egcl_rt::rooted!(condition = details.condition);
            assert!(
                crate::cli::condition_matches_handler(env, *condition, "TYPE-ERROR"),
                "expected a signalled TYPE-ERROR, got {error:?}"
            );
            let datum = egcl_stdlib::slot_value(*condition, *datum_slot).unwrap();
            assert_eq!(
                datum, *expected,
                "signalled condition must retain the exact original datum"
            );
        }
        other => panic!("expected TYPE-ERROR with original datum, got {other:?}"),
    }
}

#[test]
fn string_aref_executes_in_ordinary_and_mapped_t2_without_deopt() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(base = crate::cli::arena_str("A\0Z"));
    egcl_rt::rooted!(wide = crate::cli::arena_str("Aλ😀"));
    egcl_rt::rooted!(vector = egcl_stdlib::sequences::build_simple_vector(&[NIL, T]));
    egcl_rt::rooted!(octets = egcl_rt::packed_u8::make(2, 255).unwrap());
    for mapped in [false, true] {
        let _mapped = mapped.then(native_transfer_entry::MappedOnlyForTest::new);
        let (symbol, code) = native_transfer_tests::ordinary_t2::install(
            "NATIVE-STRING-AREF",
            "(array index)",
            "((aref array index))",
            &mut env,
        );
        assert!(code.is_t2);
        let NativeCodeStorage::Mapped(native) = &code._storage else {
            panic!("native T2 owner");
        };
        assert_eq!(native.ordinary_entry.is_none(), mapped);
        // Direct execution of the emitted entry, with no interpreter deopt,
        // proves the layout operation actually ran in both native ABIs.
        egcl_rt::rooted!(cases = [*base, *wide, *vector, *octets]);
        egcl_rt::rooted!(
            expected = [
                EgclVal::from_char('\0'),
                EgclVal::from_char('😀'),
                NIL,
                EgclVal::from_fixnum(255)
            ]
        );
        for (index, offset) in [1, 2, 0, 1].into_iter().enumerate() {
            egcl_rt::rooted!(
                value = run_native(
                    &code,
                    symbol,
                    &[cases[index], EgclVal::from_fixnum(offset)],
                    &mut env
                )
                .unwrap()
            );
            assert_eq!(*value, expected[index]);
        }
        egcl_stdlib::sequences::string_set_char(*base, 2, EgclVal::from_char('X')).unwrap();
        egcl_stdlib::sequences::string_set_char(*wide, 1, EgclVal::from_char('β')).unwrap();
        for (which, offset, expected) in [(0, 2, 'X'), (1, 1, 'β')] {
            egcl_rt::rooted!(array = if which == 0 { *base } else { *wide });
            assert_eq!(
                run_native(
                    &code,
                    symbol,
                    &[*array, EgclVal::from_fixnum(offset)],
                    &mut env
                )
                .unwrap(),
                EgclVal::from_char(expected)
            );
        }
        assert_eq!(
            native.deopt_count(),
            0,
            "all supported layouts must stay native"
        );
        for index in [-1, 3] {
            assert!(
                run_native(
                    &code,
                    symbol,
                    &[*base, EgclVal::from_fixnum(index)],
                    &mut env
                )
                .is_err()
            );
        }
        egcl_rt::rooted!(
            error = run_native(
                &code,
                symbol,
                &[EgclVal::from_fixnum(42), EgclVal::from_fixnum(0)],
                &mut env
            )
            .unwrap_err()
        );
        assert_type_error_datum(&error, EgclVal::from_fixnum(42), &env);
        let (svref_symbol, svref_code) = native_transfer_tests::ordinary_t2::install(
            "NATIVE-STRING-SVREF",
            "(array index)",
            "((svref array index))",
            &mut env,
        );
        for which in 0..2 {
            let string = if which == 0 { *base } else { *wide };
            egcl_rt::rooted!(
                error = run_native(
                    &svref_code,
                    svref_symbol,
                    &[string, EgclVal::from_fixnum(0)],
                    &mut env
                )
                .unwrap_err()
            );
            assert_type_error_datum(&error, if which == 0 { *base } else { *wide }, &env);
        }
    }
}
