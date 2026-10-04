// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_rt::gc::{Collector, HeapCollector};

fn take_original(datum: EgclVal) {
    egcl_rt::rooted!(
        error = NATIVE_ERROR
            .with(|slot| slot.take())
            .expect("pending failure")
    );
    assert!(
        matches!(error.without_backtrace(), EgclError::TypeError { datum: value, .. } if *value == datum)
    );
    let frames = error.backtrace().expect("pre-unwind snapshot");
    let original = frames
        .iter()
        .find(|frame| frame.function.as_deref() == Some("ORIGINAL-NATIVE-OWNER"))
        .expect("original call survives guard teardown");
    assert_eq!(original.arguments.as_deref(), Some(&[datum][..]));
    assert!(!native_error_pending());
}

#[test]
fn pending_native_snapshot_roots_arguments_and_preserves_the_first_error() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install_bytecode_root_scanner();
    assert!(!native_error_pending());
    egcl_rt::rooted!(datum = super::super::arena_str("original native datum"));
    let before = datum.to_raw();
    let call = egcl_rt::debug_stack::CallFrame::enter_with_args("ORIGINAL-NATIVE-OWNER", &[*datum]);
    stash_native_error(EgclError::TypeError {
        datum: *datum,
        expected: "list".into(),
    });
    drop(call);
    stash_native_error(EgclError::ProgramError(
        "later error must not replace the first".into(),
    ));
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        datum.to_raw(),
        before,
        "pending snapshot argument must actually move"
    );
    take_original(*datum);
    assert_eq!(super::super::val_as_str(*datum), "original native datum");
}

#[cfg(all(any(target_arch = "x86_64", target_arch = "s390x"), unix))]
#[test]
fn nested_native_call_roots_a_saved_outer_snapshot() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        form = reader::read_from_string("((eval '(%force-minor-gc-for-test)) 73)")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("NATIVE-SNAPSHOT-COLLECTOR");
    let body =
        compile_function("NATIVE-SNAPSHOT-COLLECTOR", NIL, *form, &env, false, false).unwrap();
    registry_put(symbol, Arc::new(body));
    let native =
        try_promote_to_t1_with_speculation(symbol, false).expect("real native collecting callee");
    assert!(!native_error_pending());
    egcl_rt::rooted!(datum = super::super::arena_str("original native datum"));
    let before = datum.to_raw();
    let call = egcl_rt::debug_stack::CallFrame::enter_with_args("ORIGINAL-NATIVE-OWNER", &[*datum]);
    stash_native_error(EgclError::TypeError {
        datum: *datum,
        expected: "list".into(),
    });
    drop(call);
    let result = run_native(&native, symbol, &[], &mut env);
    registry_remove(symbol);
    assert_eq!(result.unwrap(), EgclVal::from_fixnum(73));
    assert_ne!(
        datum.to_raw(),
        before,
        "the nested native call must relocate the saved snapshot argument"
    );
    take_original(*datum);
    assert_eq!(super::super::val_as_str(*datum), "original native datum");
}
