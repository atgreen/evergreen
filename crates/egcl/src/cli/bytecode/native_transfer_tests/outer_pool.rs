// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Outer entries from the interpreter reuse a finished activation's buffers.

use super::*;

/// Each call in the probe is a transfer site with its own capture snapshot. An
/// outer entry used to build all of them on every call; it now takes the state
/// a finished call left in the code's pool (bliss-t9tzf). The reused state must
/// not change a result, including after an error left the activation and after
/// a collection moved the arguments.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn outer_activation_reuses_its_pooled_state() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let source = "(if (minusp b) (error \"outer pool\") (cons a (cons (cons b a) (list b))))";
    super::super::super::read_eval_all_env(
        &format!("(defun outer-pool-probe (a b) {source})"),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(a b)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&format!("({source})")).unwrap().0);
    let symbol = egcl_rt::symbols::intern("OUTER-POOL-PROBE");
    let body = Arc::new(
        compile_function("OUTER-POOL-PROBE", *params, *forms, &env, false, false).unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    let code = native_transfer_entry::install_t1(symbol, body, false).expect("mapped T1");
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped T1 owner")
    };
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    native_transfer_entry::take_fresh_prepares_for_test();
    let mut moved = 0;
    for i in 0..40 {
        egcl_rt::rooted!(a = super::super::super::arena_cons(EgclVal::from_fixnum(i), NIL));
        let b = EgclVal::from_fixnum(i * 3);
        if i % 4 == 1 {
            let before = *a;
            egcl_rt::collect_t0_minor().unwrap();
            moved += usize::from(*a != before);
        }
        egcl_rt::rooted!(args = vec![*a, b]);
        if i % 5 == 2 {
            args[1] = EgclVal::from_fixnum(-1);
            assert!(mapped.run_callable(*function, &args, &mut env).is_err());
            continue;
        }
        egcl_rt::rooted!(got = mapped.run_callable(*function, &args, &mut env).unwrap());
        egcl_rt::rooted!(tail = super::super::super::arena_cons(b, NIL));
        egcl_rt::rooted!(pair = super::super::super::arena_cons(b, *a));
        egcl_rt::rooted!(tail = super::super::super::arena_cons(*pair, *tail));
        egcl_rt::rooted!(expected = super::super::super::arena_cons(*a, *tail));
        assert!(
            egcl_stdlib::equality::equal(*got, *expected),
            "call {i} returned {}",
            super::super::super::format_val(*got)
        );
        assert_eq!(super::super::super::cp(*got).0, *a, "call {i} must return its own argument");
    }
    assert!(moved > 0, "a collection between calls must relocate an argument");
    assert_eq!(
        native_transfer_entry::take_fresh_prepares_for_test(),
        1,
        "only the first outer call may build an activation state"
    );
    assert_eq!(mapped.pooled_states_for_test(), 1);
}
