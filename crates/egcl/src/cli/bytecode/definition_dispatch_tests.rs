// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

fn body(name: &str, forms: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    Arc::new(compile_function(name, NIL, *forms, env, false, false).unwrap())
}

#[test]
fn native_publication_does_not_assign_an_old_tier_to_a_replacement_object() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(name = resolve_sym("DISPATCH-TIER-PUBLICATION").unwrap());
    let symbol = name.as_symbol_index();
    egcl_rt::rooted!(old = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    egcl_rt::rooted!(new = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    let old_body = body("DISPATCH-TIER-PUBLICATION", "(11)", &env);
    egcl_rt::symbols::set_symbol_function(symbol, *old);
    assert!(publish_bytecode(symbol, old_body, None, *old));
    let old_code = try_promote_to_t1(symbol).expect("the probe must compile actual native code");
    let new_body = body("DISPATCH-TIER-PUBLICATION", "(29)", &env);
    egcl_rt::symbols::set_symbol_function(symbol, *new);
    assert!(publish_bytecode(symbol, new_body, None, *new));
    // A completed compilation must not attach its tier to a fresh cell read
    // after another setter has replaced the selected definition.
    publish_native(symbol, Some(*new), &old_code);
    let tier = egcl_rt::function::tier(*new);
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(tier, 0, "replacement has no native code of its own yet");
}

#[test]
fn named_dispatch_during_paused_function_cell_publication_uses_one_definition() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(name = resolve_sym("DISPATCH-PUBLICATION-SNAPSHOT").unwrap());
    let symbol = name.as_symbol_index();
    egcl_rt::rooted!(old = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    egcl_rt::rooted!(new = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    let old_body = body("DISPATCH-PUBLICATION-SNAPSHOT", "(11)", &env);
    egcl_rt::rooted!(_old_body = ActiveBytecodeRoot::new(&old_body));
    let new_body = body("DISPATCH-PUBLICATION-SNAPSHOT", "(29)", &env);
    egcl_rt::rooted!(_new_body = ActiveBytecodeRoot::new(&new_body));
    // Give each source-free object its immutable body, then reproduce the
    // setter's real pause between the function-cell store and invalidation.
    egcl_rt::symbols::set_symbol_function(symbol, *new);
    assert!(publish_bytecode(symbol, Arc::clone(&new_body), None, *new));
    egcl_rt::symbols::set_symbol_function(symbol, *old);
    assert!(publish_bytecode(symbol, old_body, None, *old));
    egcl_rt::symbols::set_symbol_function(symbol, *new);

    let caller = body(
        "DISPATCH-SNAPSHOT-CALLER",
        "((dispatch-publication-snapshot))",
        &env,
    );
    let t0 = run(caller, &[], NIL, &mut env).unwrap();
    let previous = NATIVE_ENV.with(|slot| slot.replace(&mut env));
    let checked = c2i_call_result(u64::from(symbol), &[], 0);
    let cell = call_table::resolve(symbol).unwrap();
    let mut cached = Vec::new();
    for _ in 0..2 {
        let entry = unsafe { &*cell.checked_entry_address(false) }
            .load(std::sync::atomic::Ordering::Acquire);
        let call: extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            unsafe { std::mem::transmute(entry) };
        cached.push(EgclVal(call(Arc::as_ptr(&cell) as u64, 0, 0, 0, 0, 0)));
    }
    NATIVE_ENV.with(|slot| slot.set(previous));
    let saved = call_registered(symbol, &[], *old, &mut env)
        .unwrap()
        .unwrap();
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(
        t0,
        EgclVal::from_fixnum(29),
        "T0 must execute the selected new object"
    );
    assert_eq!(
        checked.unwrap(),
        EgclVal::from_fixnum(29),
        "native adapter must use the same snapshot"
    );
    assert_eq!(
        cached,
        vec![EgclVal::from_fixnum(29); 2],
        "cold and warm cells must retain one definition"
    );
    assert_eq!(
        saved,
        EgclVal::from_fixnum(11),
        "saved old objects retain their own body"
    );
}

#[test]
fn promotion_cannot_replace_the_selected_definition_with_a_newer_body() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(name = resolve_sym("DISPATCH-PROMOTION-SNAPSHOT").unwrap());
    let symbol = name.as_symbol_index();
    egcl_rt::rooted!(old = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    egcl_rt::rooted!(new = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *name));
    let old_body = body("DISPATCH-PROMOTION-SNAPSHOT", "(11)", &env);
    egcl_rt::rooted!(_old_body = ActiveBytecodeRoot::new(&old_body));
    egcl_rt::symbols::set_symbol_function(symbol, *old);
    assert!(publish_bytecode(symbol, Arc::clone(&old_body), None, *old));
    // Dispatch selected OLD above. Another setter finishes before the tier
    // state machine refreshes the registry and compiles its current body.
    let new_body = body("DISPATCH-PROMOTION-SNAPSHOT", "(29)", &env);
    egcl_rt::symbols::set_symbol_function(symbol, *new);
    assert!(publish_bytecode(symbol, new_body, None, *new));
    let selected = native_for_dispatch(symbol, Some(*old), t1_threshold(), &old_body);
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert!(
        selected.as_ref().is_none_or(|code| code
            .body
            .as_ref()
            .is_some_and(|body| Arc::ptr_eq(body, &old_body))),
        "promotion must not return another definition's machine code"
    );
    if selected.is_none() {
        assert_eq!(
            egcl_rt::function::tier(*old),
            0,
            "do not publish a replacement's tier on OLD"
        );
    }
}
