// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use native_transfer_entry::root_publication;
use std::cell::Cell;

thread_local! {
    static OWNER: Cell<usize> = const { Cell::new(0) };
    static VISITS: Cell<usize> = const { Cell::new(0) };
}
fn native_entry_is_mapped(code: &NativeCode) -> bool {
    let NativeCodeStorage::Mapped(native) = &code._storage else {
        panic!("T2 owner");
    };
    native.ordinary_entry.is_none()
}
fn observe() {
    if root_publication::published_frame_has_owner_for_test(OWNER.with(Cell::get)) {
        VISITS.with(|count| count.set(count.get() + 1));
    }
}
struct Recording;
impl Drop for Recording {
    fn drop(&mut self) {
        root_publication::OBSERVE.with(|hook| hook.set(None));
        native_transfer_entry::ordinary_calls::YIELD_AFTER_RETURN_TO_OWNER
            .with(|target| target.set(0));
    }
}

#[test]
fn generic_variable_ash_executes_in_ordinary_and_mapped_t2_without_deopt() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(wide = egcl_rt::bignum::alloc_bignum_cli(1, &[0, 1 << 36]));
    egcl_rt::rooted!(marker = crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL));
    let cases = [(7, 2), (-7, -1), (7, -100), (-7, -100), (1, 100)];
    egcl_rt::rooted!(
        expected_values = [
            EgclVal::from_fixnum(28),
            EgclVal::from_fixnum(-4),
            EgclVal::from_fixnum(0),
            EgclVal::from_fixnum(-1),
            *wide
        ]
    );
    for mapped in [false, true] {
        let _mapped = mapped.then(native_transfer_entry::MappedOnlyForTest::new);
        let (symbol, code) = native_transfer_tests::ordinary_t2::install(
            "NATIVE-VARIABLE-ASH",
            "(value count marker)",
            "((declare (type fixnum value count))
              (let ((shifted (ash value count)) (n 0))
                (tagbody again
                  (setq n (+ n 1))
                  (if (< n 2) (go again)))
                (cons shifted marker)))",
            &mut env,
        );
        assert!(code.is_t2);
        assert_eq!(native_entry_is_mapped(&code), mapped);
        let NativeCodeStorage::Mapped(native) = &code._storage else {
            panic!("T2 owner");
        };
        OWNER.with(|owner| owner.set(Rc::as_ptr(native) as usize));
        VISITS.with(|count| count.set(0));
        let _recording = Recording;
        root_publication::OBSERVE.with(|hook| hook.set(Some(observe)));
        for (index, &(value, count)) in cases.iter().enumerate() {
            // A fixnum result is answered by ASH's leaf helper before the
            // adapter (bliss-gg3i1.7.3); only the bignum result crosses it.
            let crosses_adapter = !mapped && !expected_values[index].is_fixnum();
            if crosses_adapter {
                // The ASH adapter services an entry notification before its
                // call. Request the real yield after it returns to this exact
                // owner, so the following native poll publishes the frame.
                native_transfer_entry::ordinary_calls::YIELD_AFTER_RETURN_TO_OWNER
                    .with(|target| target.set(OWNER.with(Cell::get)));
            }
            egcl_rt::rooted!(
                result = run_native(
                    &code,
                    symbol,
                    &[
                        EgclVal::from_fixnum(value),
                        EgclVal::from_fixnum(count),
                        *marker
                    ],
                    &mut env,
                )
                .unwrap()
            );
            assert_eq!(
                native_transfer_entry::ordinary_calls::YIELD_AFTER_RETURN_TO_OWNER.with(Cell::get),
                0,
                "the exact ordinary owner must consume the return notification"
            );
            assert!(result.is_cons());
            let (shifted, saved_marker) = crate::cli::cp(*result);
            assert_eq!(
                saved_marker, *marker,
                "native call must retain the exact heap marker"
            );
            egcl_rt::rooted!(shifted = shifted);
            assert!(crate::cli::numeric_equal(*shifted, expected_values[index]).unwrap());
        }
        assert!(
            VISITS.with(Cell::get) > 0,
            "the post-ASH poll must publish this actual T2 owner (mapped={mapped})"
        );
        assert_eq!(native.deopt_count(), 0);
        for (name, form, large) in [
            ("NATIVE-CONSTANT-ASH", "((ash value 1))", false),
            ("NATIVE-LARGE-ASH", "((ash value 100))", true),
        ] {
            egcl_rt::rooted!(
                expected = if large {
                    *wide
                } else {
                    EgclVal::from_fixnum(2)
                }
            );
            let (symbol, code) =
                native_transfer_tests::ordinary_t2::install(name, "(value)", form, &mut env);
            let NativeCodeStorage::Mapped(native) = &code._storage else {
                panic!("T2 owner");
            };
            assert!(code.is_t2);
            egcl_rt::rooted!(
                result = run_native(&code, symbol, &[EgclVal::from_fixnum(1)], &mut env).unwrap()
            );
            assert!(crate::cli::numeric_equal(*result, *expected).unwrap());
            assert_eq!(native.deopt_count(), 0);
        }
    }
}
