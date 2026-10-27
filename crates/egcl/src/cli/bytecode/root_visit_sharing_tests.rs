// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use crate::cli::{EnvFrame, SharedCell};
use std::collections::HashMap;

fn body(env: &Env) -> BytecodeFunction {
    egcl_rt::rooted!(form = super::super::vec_to_list(&[NIL]));
    compile_function("ROOT-VISIT-FIXTURE", NIL, *form, env, false, false).unwrap()
}

#[test]
fn shared_nested_body_slots_once_distinct_copies_both_and_public_walks_fresh() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let mut leaf = body(&env);
    leaf.constants = vec![EgclVal::from_fixnum(71)];
    let leaf = Arc::new(leaf);
    let mut first = body(&env);
    first.nested_functions = vec![Arc::clone(&leaf)];
    let mut second = first.clone();
    let mut copy = (*leaf).clone();
    copy.constants = leaf.constants.clone();
    second.nested_functions.push(Arc::new(copy));
    let first = Arc::new(first);
    let second = Arc::new(second);
    let shared_slot = leaf.constants.as_ptr() as usize;
    let distinct_slot = second.nested_functions[1].constants.as_ptr() as usize;
    let mut visits = HashMap::new();
    register_bytecode_roots(&first);
    register_bytecode_roots(&second);
    scan_bytecode_roots(&mut |slot| {
        *visits.entry(slot as usize).or_insert(0) += 1;
        if slot as usize == shared_slot || slot as usize == distinct_slot {
            unsafe {
                *slot = EgclVal::from_fixnum(72);
            }
        }
    });
    assert_eq!(
        visits[&shared_slot],
        if root_visit_sharing_enabled() { 1 } else { 2 }
    );
    assert_eq!(visits[&distinct_slot], 1);
    assert_eq!(leaf.constants[0], EgclVal::from_fixnum(72));
    assert_eq!(
        second.nested_functions[1].constants[0],
        EgclVal::from_fixnum(72)
    );
    for replacement in [73, 74] {
        let mut count = 0;
        unsafe {
            trace_bytecode_function(Arc::as_ptr(&first) as *mut _, &mut |slot| {
                if slot as usize == shared_slot {
                    count += 1;
                    *slot = EgclVal::from_fixnum(replacement);
                }
            })
        };
        assert_eq!(count, 1, "a fresh public walk must not inherit suppression");
        assert_eq!(leaf.constants[0], EgclVal::from_fixnum(replacement));
    }
}

fn frame(parent: Option<Arc<SharedCell<EnvFrame>>>, value: i64) -> Arc<SharedCell<EnvFrame>> {
    let vars = Default::default();
    // Spell out the frame before inserting to preserve the repository's VecMap type.
    let mut frame = EnvFrame {
        vars,
        symbol_vars: Default::default(),
        parent,
    };
    frame
        .vars
        .insert("CAPTURE".into(), EgclVal::from_fixnum(value));
    Arc::new(SharedCell::new(frame))
}

#[test]
fn private_owners_share_code_but_keep_distinct_frames_and_shared_parent() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let code = Arc::new(body(&env));
    let parent = frame(None, 70);
    let first = frame(Some(Arc::clone(&parent)), 71);
    let second = frame(Some(Arc::clone(&parent)), 72);
    let mut symbols = Vec::new();
    let mut symbol_values = Vec::new();
    egcl_rt::rooted_ref!(_symbols = &mut symbol_values);
    for captured in [&first, &second] {
        let value = egcl_rt::symbols::make_uninterned("ROOT-VISIT-OWNER");
        let symbol = value.as_symbol_index();
        symbol_values.push(value);
        closure_bodies()
            .borrow_mut()
            .insert(symbol, Arc::clone(&code));
        closure_envs()
            .borrow_mut()
            .insert(symbol, Arc::clone(captured));
        symbols.push(symbol);
    }
    let slots: Vec<_> = [&parent, &first, &second]
        .into_iter()
        .map(|f| f.borrow_mut().vars.get_mut("CAPTURE").unwrap() as *mut EgclVal as usize)
        .collect();
    let mut counts = HashMap::new();
    let mut state = CodeRootVisitState::default();
    for &symbol in &symbols {
        trace_private_function_roots(
            symbol,
            &mut |slot| {
                let address = slot as usize;
                if slots.contains(&address) {
                    *counts.entry(address).or_insert(0) += 1;
                    unsafe {
                        *slot = EgclVal::from_fixnum(99);
                    }
                }
            },
            &mut state,
        );
    }
    for (i, slot) in slots.into_iter().enumerate() {
        let expected = if i == 0 && !root_visit_sharing_enabled() {
            2
        } else {
            1
        };
        assert_eq!(counts[&slot], expected);
    }
    for f in [&parent, &first, &second] {
        assert_eq!(
            f.borrow().vars.get("CAPTURE"),
            Some(&EgclVal::from_fixnum(99))
        );
    }
    with_traced_callables(|traced| {
        for symbol in &symbols {
            assert!(traced.contains(symbol));
        }
    });
    for symbol in symbols {
        closure_bodies().borrow_mut().remove(&symbol);
        closure_envs().borrow_mut().remove(&symbol);
    }
}

#[test]
fn private_call_only_capture_moves_and_survives_major_without_global_function_owner() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let callee = Arc::new(body(&env));
    let captured = frame(None, 0);
    egcl_rt::rooted!(closure = make_bytecode_closure(&callee, Some(Arc::clone(&captured))));
    let symbol = egcl_rt::function::name(*closure).as_symbol_index();
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    let mut caller = body(&env);
    caller.code = vec![
        Instr::CallNamed {
            sym: symbol,
            nargs: 0,
        },
        Instr::Return,
    ];
    let caller = Arc::new(caller);
    egcl_rt::rooted!(_caller = ActiveBytecodeRoot::new(&caller));
    drop(closure);
    env.mv.clear();
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(young = super::super::arena_cons(EgclVal::from_fixnum(81), NIL));
    let before = young.to_raw();
    captured.borrow_mut().vars.insert("CAPTURE".into(), *young);
    drop(young);
    egcl_rt::gc::collect_t0_minor().unwrap();
    let after = *captured.borrow().vars.get("CAPTURE").unwrap();
    assert_ne!(
        before,
        after.to_raw(),
        "private-call-only captured cons must actually relocate"
    );
    assert_eq!(super::super::cp(after), (EgclVal::from_fixnum(81), NIL));
    egcl_rt::gc::full_gc().unwrap();
    assert!(closure_envs().borrow().contains_key(&symbol));
    let after_major = *captured.borrow().vars.get("CAPTURE").unwrap();
    assert_eq!(
        super::super::cp(after_major),
        (EgclVal::from_fixnum(81), NIL)
    );
    assert!(with_traced_callables(|traced| traced.contains(&symbol)));
}
