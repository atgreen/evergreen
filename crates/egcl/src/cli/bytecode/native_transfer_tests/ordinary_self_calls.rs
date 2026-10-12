// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

thread_local! {
    static REPLACEMENT: RefCell<String> = const { RefCell::new(String::new()) };
    static MARKER: Cell<*mut EgclVal> = const { Cell::new(std::ptr::null_mut()) };
    static REPLACED: Cell<bool> = const { Cell::new(false) };
    static MOVED: Cell<bool> = const { Cell::new(false) };
    static SELF_COLD: Cell<bool> = const { Cell::new(false) };
    static DYNAMIC: Cell<bool> = const { Cell::new(false) };
}

fn replace_and_collect() {
    if REPLACED.with(|flag| flag.replace(true)) { return; }
    let source = REPLACEMENT.with(|source| source.borrow().clone());
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    unsafe { super::super::super::read_eval_all_env(&source, &mut *env).unwrap(); }
    let marker = MARKER.with(Cell::get);
    let before = unsafe { (*marker).to_raw() };
    egcl_rt::gc::full_gc().unwrap();
    MOVED.with(|flag| flag.set(unsafe { (*marker).to_raw() } != before));
    assert!(MOVED.with(Cell::get), "argument must relocate while caller is published");
}

fn at_self_cold() {
    SELF_COLD.with(|flag| flag.set(true));
    replace_and_collect();
}

fn before_self_call() {
    if DYNAMIC.with(Cell::get) { replace_and_collect(); }
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    // The wrapper's refresh leaves this pending; the next self admission must
    // publish its caller and take the cold poll, rather than the direct edge.
    unsafe { &*execution }.request_yield();
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(None));
        native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER.with(|hook| hook.set(None));
        MARKER.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

fn exercise(notinline: bool) {
    let _lock = super::super::super::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = if notinline { "ORDINARY-DYNAMIC-SELF-GC" } else { "ORDINARY-DIRECT-SELF-GC" };
    let declaration = if notinline { format!("(declare (notinline {name}))") } else { String::new() };
    let forms = format!("({declaration} (if (= n 0) (car marker) (progn (self-cold-observer) (+ ({name} 0 marker) 0))))");
    super::super::super::read_eval_all_env(&format!(
        "(defun self-cold-observer () (%ordinary-t2-entry-for-test))
         (defun {name} (n marker) {declaration}
           (if (= n 0) (car marker) (progn (self-cold-observer) (+ ({name} 0 marker) 0))))"), &mut env).unwrap();
    let (symbol, code) = ordinary_t2::install(name, "(n marker)", &forms, &mut env);
    assert!(code.is_t2);
    let body = code.body.as_ref().expect("installed bytecode body");
    let self_sites: Vec<_> = body.code.iter().enumerate().filter_map(|(pc, instruction)| {
        matches!(instruction, Instr::CallNamed { sym, .. } if *sym == symbol)
            .then_some(pc as u32)
    }).collect();
    assert_eq!(self_sites.len(), 1, "fixture must retain a real non-tail self call");
    if notinline {
        assert!(body.self_call_sites.is_empty(), "NOTINLINE must prohibit direct self binding");
    } else {
        assert_eq!(body.self_call_sites, self_sites, "source whitelist must select the actual self call");
    }
    let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("ordinary code owner") };
    assert!(mapped.ordinary_entry.is_some());
    ordinary_t2::expect_entry(&code);
    egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(args = vec![EgclVal::from_fixnum(1), *marker]);
    MARKER.with(|slot| slot.set(&mut *marker));
    REPLACEMENT.with(|source| *source.borrow_mut() = format!("(defun {name} (n marker) 99)"));
    REPLACED.with(|flag| flag.set(false));
    MOVED.with(|flag| flag.set(false));
    SELF_COLD.with(|flag| flag.set(false));
    DYNAMIC.with(|flag| flag.set(notinline));
    let _reset = Reset;
    ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(Some(before_self_call)));
    native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER.with(|hook| hook.set(Some(at_self_cold)));
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    egcl_rt::rooted!(result = run_native(&code, symbol, &args, &mut env).unwrap());
    assert!(ordinary_t2::observed_entry(), "fixture must execute the installed ordinary owner");
    assert!(REPLACED.with(Cell::get));
    assert!(MOVED.with(Cell::get));
    assert_eq!(SELF_COLD.with(Cell::get), !notinline);
    if notinline {
        assert_eq!(*result, EgclVal::from_fixnum(99));
    } else {
        assert!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test() > 0);
        assert_eq!(*result, EgclVal::from_fixnum(71), "cold self-call must read the moved marker in the retained old definition");
    }
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(super::super::super::apply_function(*replacement, &args, &mut env).unwrap(),
        EgclVal::from_fixnum(99), "fresh external calls must see replacement");
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_self_cold_keeps_exact_definition_across_replacement_and_gc() { exercise(false); }

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_notinline_self_observes_replacement_across_gc() { exercise(true); }
