// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! End-to-end roots and exceptional retirement across ordinary code owners.

use super::ordinary_t2::install;
use super::*;
use egcl_rt::debug_stack::FrameOrigin;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_register_foreign_arguments_survive_gc_and_exceptional_retirement() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun ordinary-register-roots-leaf (a b c d) d)
         (defun ordinary-register-roots-middle (a b c d)
           (ordinary-register-roots-leaf b d a c))",
        &mut env,
    )
    .unwrap();
    let (_, middle) = install(
        "ORDINARY-REGISTER-ROOTS-MIDDLE",
        "(a b c d)",
        "((ordinary-register-roots-leaf b d a c))",
        &mut env,
    );
    let (symbol, outer) = install(
        "ORDINARY-REGISTER-ROOTS-OUTER",
        "(a b c d)",
        "((ordinary-register-roots-middle d c b a))",
        &mut env,
    );
    for code in [&middle, &outer] {
        let NativeCodeStorage::Mapped(code) = &code._storage else {
            panic!("mapped owner")
        };
        assert!(native_transfer_entry::installed_listing(code).contains("Ordinary x86-64 Lisp ABI"));
    }
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for _ in 0..2 {
        assert_eq!(
            run_native(&outer, symbol, &warm, &mut env).unwrap(),
            warm[1]
        );
    }
    crate::cli::read_eval_all_env(
        "(defun ordinary-register-roots-leaf (a b c d)
           (%force-minor-gc-for-test) (error \"ordinary roots failure\"))",
        &mut env,
    )
    .unwrap();
    // Allocate after warming/redefinition so the failing call must cross a
    // moving collection. Stress may promote early values during construction.
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(arguments = Vec::new());
    for index in 0..4 {
        arguments.push(crate::cli::arena_cons(
            EgclVal::from_fixnum(100 + index),
            NIL,
        ));
    }
    let original: Vec<_> = arguments.iter().map(|value| value.to_raw()).collect();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    egcl_rt::rooted!(error = run_native(&outer, symbol, &arguments, &mut env).unwrap_err());
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        1,
        "only the terminal call is cold: the middle frame entered through generated code"
    );
    assert!(
        arguments
            .iter()
            .zip(original)
            .any(|(value, bits)| value.to_raw() != bits),
        "the error path must actually relocate heap arguments"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before,
        "exception retires generated child depth and its root frame exactly once"
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(
        unsafe { &*execution }.active_native_control(),
        control,
        "the outer scope restores the previous execution control pointer"
    );

    // The owned backtrace must remain traceable after all native frames retire.
    HeapCollector::new().minor_gc().unwrap();
    let trace = error.backtrace().expect("captured ordinary error");
    let frames: Vec<_> = trace
        .iter()
        .filter(|frame| {
            frame.function.as_deref().is_some_and(|name| {
                name.ends_with("ORDINARY-REGISTER-ROOTS-MIDDLE")
                    || name.ends_with("ORDINARY-REGISTER-ROOTS-OUTER")
            })
        })
        .collect();
    assert_eq!(frames.len(), 2, "{trace:?}");
    for (frame, (name, indices)) in frames.iter().zip([
        ("ORDINARY-REGISTER-ROOTS-MIDDLE", [3, 2, 1, 0]),
        ("ORDINARY-REGISTER-ROOTS-OUTER", [0, 1, 2, 3]),
    ]) {
        assert!(
            frame.function.as_deref().unwrap().ends_with(name),
            "{trace:?}"
        );
        assert_eq!(frame.origin, FrameOrigin::Native);
        let actual = frame
            .arguments
            .as_ref()
            .expect("all original argument homes are available");
        assert_eq!(actual.len(), 4);
        for (value, index) in actual.iter().zip(indices) {
            assert_eq!(
                *value, arguments[index],
                "{name} argument {index} has its relocated home"
            );
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(100 + index as i64), NIL)
            );
        }
    }
}
