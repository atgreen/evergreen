// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A function T1 has declined runs as bytecode without revisiting the tier
//! state machine on every later call (bliss-nkzx6, GitHub #474).

use super::*;

#[test]
fn a_declined_function_skips_tier_dispatch_after_its_decline() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(name = resolve_sym("DECLINED-DISPATCH-PROBE").unwrap());
    let symbol = name.as_symbol_index();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
    egcl_rt::rooted!(function = egcl_rt::function::alloc_interpreted(*params, *forms, NIL, *name));
    let body = Arc::new(
        compile_function(
            "DECLINED-DISPATCH-PROBE",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    egcl_rt::symbols::set_symbol_function(symbol, *function);
    assert!(publish_bytecode(symbol, body, None, *function));
    // A body T1 refuses: the blacklist makes `try_promote_to_t1` decline it.
    DEOPT_BLACKLIST.with(|blacklist| blacklist.borrow_mut().insert(symbol));
    let calls = u64::from(t1_threshold()) + 200;
    let before = TIER_DISPATCHES.with(std::cell::Cell::get);
    for i in 0..calls {
        // The tree-walker counts a named function's invocation before
        // dispatching it; do the same so the probe crosses the T1 threshold.
        egcl_rt::function::record_invocation(*function);
        let result = call_registered(
            symbol,
            &[EgclVal::from_fixnum(i as i64)],
            *function,
            &mut env,
        )
        .expect("registered bytecode")
        .unwrap();
        assert_eq!(result, EgclVal::from_fixnum(i as i64 + 1));
    }
    let dispatches = TIER_DISPATCHES.with(std::cell::Cell::get) - before;
    let declined = t1_declined(symbol);
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert!(declined, "the probe must actually be declined by T1");
    assert!(
        dispatches <= u64::from(t1_threshold()),
        "{dispatches} of {calls} calls re-entered tier dispatch after the decline"
    );
}
