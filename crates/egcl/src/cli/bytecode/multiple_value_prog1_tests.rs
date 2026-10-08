// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn check_saved_values_move(native: bool) {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((multiple-value-prog1 (values x x) (egcl-ext:gc)))")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("MV-PROG1-MOVING-VALUES");
    let body = Arc::new(
        compile_function("MV-PROG1-MOVING-VALUES", *params, *forms, &env, true, false)
            .expect("MULTIPLE-VALUE-PROG1 must lower"),
    );
    registry_put(symbol, Arc::clone(&body));
    let code = native
        .then(|| try_promote_to_t1_with_speculation(symbol, false).expect("must compile at T1"));
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(args = vec![arena_cons(EgclVal::from_fixnum(37), NIL)]);
    let before = args[0];
    let result = if let Some(code) = code {
        run_native(&code, symbol, &args, &mut env).unwrap()
    } else {
        run_with_sym(body, &args, NIL, symbol, &mut env).unwrap()
    };
    assert_ne!(args[0], before, "the saved value must actually relocate");
    assert_eq!(result, args[0]);
    assert!(env.mv_active);
    assert_eq!(env.mv, vec![args[0], args[0]]);
    assert_eq!(cp(result), (EgclVal::from_fixnum(37), NIL));
    registry_remove(symbol);
}

#[test]
fn bytecode_saved_values_survive_relocation() {
    check_saved_values_move(false);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn native_saved_values_survive_relocation() {
    check_saved_values_move(true);
}
