// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{
    DynBind, Env, arena_cons, arena_str, gensym_symbol, heap_test_lock, resolve_sym, typep_matches,
    vec_to_list,
};
use egcl_rt::Collector;

fn fixnum(value: i64) -> EgclVal {
    EgclVal::from_fixnum(value)
}

fn entry(name: EgclVal, value: EgclVal) -> EgclVal {
    egcl_rt::rooted!(tail = arena_cons(value, NIL));
    arena_cons(name, *tail)
}

#[test]
fn first_entry_wins_and_prepended_entries_shadow_older_ones() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(a = gensym_symbol("TRI-A"));
    egcl_rt::rooted!(b = gensym_symbol("TRI-B"));
    egcl_rt::rooted!(a_new = entry(*a, fixnum(1)));
    egcl_rt::rooted!(a_old = entry(*a, fixnum(2)));
    egcl_rt::rooted!(b_entry = entry(*b, fixnum(3)));
    egcl_rt::rooted!(list = vec_to_list(&[*a_new, *b_entry, *a_old]));
    let registry = TypeRegistry::Expanders;
    assert_eq!(lookup(registry, *list, *a), Some(fixnum(1)));
    assert_eq!(lookup(registry, *list, *b), Some(fixnum(3)));

    // The head moves; the index must follow it rather than rebuild or go stale.
    let before = list.to_raw();
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        list.to_raw(),
        before,
        "the registry head must actually move"
    );
    assert_eq!(lookup(registry, *list, *a), Some(fixnum(1)));

    // A redefinition conses onto the front, as DEFTYPE does.
    egcl_rt::rooted!(redefined = entry(*b, fixnum(4)));
    egcl_rt::rooted!(longer = arena_cons(*redefined, *list));
    assert_eq!(lookup(registry, *longer, *b), Some(fixnum(4)));
    assert_eq!(lookup(registry, *longer, *a), Some(fixnum(1)));

    // An unrelated list, such as a dynamic rebinding, shares nothing.
    egcl_rt::rooted!(other_entry = entry(*a, fixnum(5)));
    egcl_rt::rooted!(other = vec_to_list(&[*other_entry]));
    assert_eq!(lookup(registry, *other, *a), Some(fixnum(5)));
    assert_eq!(lookup(registry, *other, *b), None);
    assert_eq!(lookup(registry, NIL, *a), None);

    // Restoring the longer list sees all of it again.
    assert_eq!(lookup(registry, *longer, *b), Some(fixnum(4)));
}

#[test]
fn typep_sees_a_redefinition_consed_onto_the_registry() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    egcl_rt::rooted!(alias = gensym_symbol("TRI-ALIAS"));
    let integer = resolve_sym("INTEGER").unwrap();
    let string = resolve_sym("STRING").unwrap();
    egcl_rt::rooted!(text = arena_str("x"));
    egcl_rt::rooted!(as_integer = entry(*alias, integer));
    egcl_rt::rooted!(definitions = vec_to_list(&[*as_integer]));
    let definitions_symbol = resolve_sym("*TYPE-DEFINITIONS*").unwrap();
    let expanders_symbol = resolve_sym("*TYPE-EXPANDERS*").unwrap();
    egcl_rt::rooted!(_expanders = DynBind::establish(expanders_symbol, NIL));
    {
        egcl_rt::rooted!(_binding = DynBind::establish(definitions_symbol, *definitions));
        assert!(typep_matches(&mut env, EgclVal::from_fixnum(1), *alias).unwrap());
        assert!(!typep_matches(&mut env, *text, *alias).unwrap());
    }
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(as_string = entry(*alias, string));
    egcl_rt::rooted!(redefined = arena_cons(*as_string, *definitions));
    egcl_rt::rooted!(_binding = DynBind::establish(definitions_symbol, *redefined));
    assert!(!typep_matches(&mut env, EgclVal::from_fixnum(1), *alias).unwrap());
    assert!(typep_matches(&mut env, *text, *alias).unwrap());
}
