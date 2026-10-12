// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A callable advertises each ABI independently; an ordinary entry does not
//! imply either presence or absence of a managed activation entry.

use super::ordinary_t2::install;
use super::*;
use native_transfer_entry::{take_nested_entries, TransferCode};

fn legacy_caller(name: &str, forms: &str, env: &Env) -> TransferCode {
    egcl_rt::rooted!(params = reader::read_from_string("(a b c d)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    let code = TransferCode::compile(body).expect("explicit managed activation caller");
    assert!(code.ordinary_entry.is_none());
    assert!(code.mapped_entry().is_some());
    code
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mixed_entries_preserve_legacy_leaf_and_register_bridge_paths() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun mixed-entry-leaf (a b c d) d)
         (defun mixed-entry-bridge (a b c d) (mixed-entry-leaf a b c d))
         (defun mixed-entry-collect () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let (_, leaf) = install("MIXED-ENTRY-LEAF", "(a b c d)", "(d)", &mut env);
    let (bridge_symbol, bridge) = install(
        "MIXED-ENTRY-BRIDGE",
        "(a b c d)",
        "((mixed-entry-leaf a b c d))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(leaf_owner) = &leaf._storage else {
        panic!("installed leaf owner")
    };
    let NativeCodeStorage::Mapped(bridge_owner) = &bridge._storage else {
        panic!("installed bridge owner")
    };
    assert!(leaf.is_t2 && bridge.is_t2);
    assert!(leaf_owner.ordinary_entry.is_some());
    assert!(bridge_owner.ordinary_entry.is_some());
    assert!(
        leaf_owner.mapped_entry().is_some(),
        "leaf advertises both ABIs"
    );
    assert!(
        bridge_owner.mapped_entry().is_none(),
        "ordinary-only forwarder must never advertise its UD2 legacy placeholder"
    );

    let direct = legacy_caller(
        "MIXED-ENTRY-MANAGED-DIRECT",
        "((let ((result (mixed-entry-leaf a b c d))) (mixed-entry-collect) result))",
        &env,
    );
    let forwarded = legacy_caller(
        "MIXED-ENTRY-MANAGED-FORWARD",
        "((mixed-entry-bridge a b c d))",
        &env,
    );
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    // Resolve cells through actual calls with an active Env. No cached state
    // or dispatch preparation is borrowed from another test.
    for _ in 0..2 {
        assert_eq!(direct.run(&warm, &mut env).unwrap(), warm[3]);
        assert_eq!(forwarded.run(&warm, &mut env).unwrap(), warm[3]);
        assert_eq!(
            run_native(&bridge, bridge_symbol, &warm, &mut env).unwrap(),
            warm[3]
        );
    }
    let leaf_cell =
        call_table::resolve(egcl_rt::symbols::find_index("MIXED-ENTRY-LEAF").unwrap()).unwrap();
    let bridge_cell = call_table::resolve(bridge_symbol).unwrap();
    assert!(unsafe { call_table::mapped_callee(Arc::as_ptr(&leaf_cell) as u64) }.is_some());
    assert!(unsafe { call_table::mapped_callee(Arc::as_ptr(&bridge_cell) as u64) }.is_none());

    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        arguments = vec![
            EgclVal::from_fixnum(11),
            EgclVal::from_fixnum(22),
            EgclVal::from_fixnum(33),
            crate::cli::arena_cons(EgclVal::from_fixnum(74), NIL),
        ]
    );
    let before = arguments[3].to_raw();
    let stack = egcl_rt::current_stack();
    let frame = stack.fp();
    let sp = stack.sp();
    env.set_mv(vec![T, NIL, EgclVal::from_fixnum(99)]);
    take_nested_entries();
    egcl_rt::rooted!(result = direct.run(&arguments, &mut env).unwrap());
    assert_eq!(
        take_nested_entries(),
        1,
        "leaf must use the warmed same-segment managed entry"
    );
    assert_ne!(
        arguments[3].to_raw(),
        before,
        "the caller's collecting continuation must relocate the heap argument"
    );
    assert_eq!(
        *result, arguments[3],
        "native result home must follow relocation"
    );
    assert_eq!(crate::cli::cp(*result), (EgclVal::from_fixnum(74), NIL));
    assert!(
        !env.mv_active,
        "single leaf result clears stale multiple values"
    );
    assert_eq!((stack.fp(), stack.sp()), (frame, sp));

    // The same legacy caller must fall back for an ordinary-only bridge,
    // rather than accidentally jumping to its non-entry byte zero.
    env.set_mv(vec![T, NIL]);
    take_nested_entries();
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    assert_eq!(forwarded.run(&arguments, &mut env).unwrap(), arguments[3]);
    assert_eq!(
        take_nested_entries(),
        0,
        "ordinary-only bridge cannot become a legacy child"
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0
    );
    assert!(!env.mv_active);

    // Admitting the leaf's managed entry must not displace its register entry
    // when an ordinary caller already has a compatible warmed descriptor.
    take_nested_entries();
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    assert_eq!(
        run_native(&bridge, bridge_symbol, &arguments, &mut env).unwrap(),
        arguments[3]
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0
    );
    assert_eq!(take_nested_entries(), 0);
    assert!(!env.mv_active);
    assert_eq!((stack.fp(), stack.sp()), (frame, sp));
}
