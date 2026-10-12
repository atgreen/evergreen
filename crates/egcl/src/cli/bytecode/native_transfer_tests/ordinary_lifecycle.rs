// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Real installed register entries must retain asynchronous and depth semantics.
use super::*;
use std::cell::Cell;

thread_local! {
    static SIGNAL_TO_POST: Cell<Option<egcl_rt::PendingSignal>> = const { Cell::new(None) };
    static INJECTIONS: Cell<usize> = const { Cell::new(0) };
    static LEAF_DEPTH: Cell<Option<(u32, usize)>> = const { Cell::new(None) };
}
fn post_signal() {
    let signal = SIGNAL_TO_POST.with(|slot| slot.take()).expect("one pending test request");
    INJECTIONS.with(|count| count.set(count.get() + 1));
    egcl_rt::post_current_pending_signal(signal);
}
fn observe_depth() {
    LEAF_DEPTH.with(|slot| slot.set(Some((NATIVE_DEPTH.with(Cell::get),
        egcl_rt::current_stack().frame_depth()))));
}
struct RestoreObserver(Option<fn()>);
impl Drop for RestoreObserver {
    fn drop(&mut self) {
        super::ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
        LEAF_DEPTH.with(|slot| slot.set(None));
    }
}

fn install(name: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        super::ordinary_t2::install(name, "(a b c d)", forms, env)
    })
}

fn fixture(env: &mut Env) -> (u32, Rc<NativeCode>, Rc<NativeCode>, u32) {
    crate::cli::read_eval_all_env(
        "(setq *ordinary-lifecycle-effects* 0)
         (defun ordinary-lifecycle-terminal (a b c d)
           (%ordinary-t2-entry-for-test)
           (setq *ordinary-lifecycle-effects* (+ *ordinary-lifecycle-effects* 1))
           (values a b c d))
         (defun ordinary-lifecycle-middle (a b c d)
           (ordinary-lifecycle-terminal b d a c))", env).unwrap();
    let (_, middle) = install("ORDINARY-LIFECYCLE-MIDDLE",
        "((ordinary-lifecycle-terminal b d a c))", env);
    let (symbol, outer) = install("ORDINARY-LIFECYCLE-OUTER",
        "((ordinary-lifecycle-middle d c b a))", env);
    for code in [&outer, &middle] {
        let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("mapped owner") };
        assert!(native_transfer_entry::installed_listing(mapped).contains("Ordinary x86-64 Lisp ABI"));
        assert_eq!(mapped.value_maps_for_test().iter().count(), 2,
            "non-tail CALL retains direct and cold maps");
    }
    let args = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for iteration in 0..3 {
        super::ordinary_t2::expect_entry(if iteration == 0 { &middle } else { &outer });
        assert_eq!(run_native(&outer, symbol, &args, env).unwrap(), args[2]);
    }
    let effects = egcl_rt::symbols::intern("*ORDINARY-LIFECYCLE-EFFECTS*");
    (symbol, outer, middle, effects)
}

fn effects(symbol: u32) -> EgclVal {
    egcl_rt::symbols::symbol_value(symbol).unwrap()
}

fn reset_effects(symbol: u32) {
    egcl_rt::symbols::set_symbol_value_checked(symbol, EgclVal::from_fixnum(0)).unwrap();
}

struct RestoreDepth(u32);
impl Drop for RestoreDepth {
    fn drop(&mut self) { NATIVE_DEPTH.with(|depth| depth.set(self.0)); }
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn ordinary_register_pending_signals_use_real_cold_delivery_and_restore_state() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, outer, _middle, counter) = fixture(&mut env);
    let args = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for signal in [egcl_rt::PendingSignal::Interrupt, egcl_rt::PendingSignal::Shutdown,
        egcl_rt::PendingSignal::Timeout] {
        reset_effects(counter);
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = unsafe { &*execution }.active_native_control();
        let segment = egcl_rt::native_transfer::current_segment();
        SIGNAL_TO_POST.with(|slot| slot.set(Some(signal)));
        INJECTIONS.with(|count| count.set(0));
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
        native_transfer_entry::ordinary_calls::take_named_entries_for_test();
        let error = native_transfer_entry::with_ordinary_entry_hook_for_test(
            post_signal,
            || run_native(&outer, symbol, &args, &mut env),
        ).unwrap_err();
        assert_eq!(INJECTIONS.with(Cell::get), 1, "request posted after the root entry poll");
        assert!(matches!((signal, error.without_backtrace()),
            (egcl_rt::PendingSignal::Interrupt, EgclError::Interrupt)
                | (egcl_rt::PendingSignal::Shutdown, EgclError::Shutdown)
                | (egcl_rt::PendingSignal::Timeout, EgclError::Timeout)), "{error:?}");
        assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 1,
            "the generated ordinary edge must invoke the production cold handler");
        assert_eq!(native_transfer_entry::ordinary_calls::take_named_entries_for_test(), 0,
            "pending transfer rejects the edge before entering any named adapter");
        if !matches!(signal, egcl_rt::PendingSignal::Shutdown) {
            let frames = error.backtrace().expect("interrupt/timeout retain their native trace");
            assert!(frames.iter().any(|frame| frame.function.as_deref()
                .is_some_and(|name| name.ends_with("ORDINARY-LIFECYCLE-OUTER"))));
            assert!(!frames.iter().any(|frame| frame.function.as_deref()
                .is_some_and(|name| name.ends_with("ORDINARY-LIFECYCLE-MIDDLE"))),
                "pending request must reject the first edge, before entering the middle owner");
        }
        assert_eq!(effects(counter), EgclVal::from_fixnum(0), "pending transfer must precede callee effects");
        assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
        assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
        assert_eq!(unsafe { &*execution }.active_native_control(), control);
        assert!(egcl_rt::take_current_pending_signal().is_none(), "signal consumed once");
        assert_eq!(run_native(&outer, symbol, &args, &mut env).unwrap(), args[2]);
        assert_eq!(effects(counter), EgclVal::from_fixnum(1), "later invocation remains usable");
    }
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn ordinary_register_depth_cap_uses_cold_fallback_once_and_preserves_values() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, outer, _middle, counter) = fixture(&mut env);
    let args = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    let cap = native_depth_cap();
    assert!(cap >= 4);
    let _observer = RestoreObserver(super::ordinary_t2::RECURSIVE_LEAF_OBSERVER
        .with(|hook| hook.replace(Some(observe_depth))));
    let mut observations = Vec::new();
    for initial in [0, cap - 1] {
        let _depth = RestoreDepth(NATIVE_DEPTH.with(|depth| depth.replace(initial)));
        LEAF_DEPTH.with(|slot| slot.set(None));
        reset_effects(counter);
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = unsafe { &*execution }.active_native_control();
        let segment = egcl_rt::native_transfer::current_segment();
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
        native_transfer_entry::ordinary_calls::take_named_entries_for_test();
        assert_eq!(run_native(&outer, symbol, &args, &mut env).unwrap(), args[2]);
        assert!(env.mv_active);
        assert_eq!(env.mv, [args[2], args[0], args[3], args[1]]);
        assert_eq!(effects(counter), EgclVal::from_fixnum(1), "fallback must not replay the callee");
        assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
            usize::from(initial != 0), "only the depth cap rejects a generated edge");
        assert_eq!(native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
            usize::from(initial == 0),
            "normal entry uses the terminal adapter; depth fallback interprets the middle");
        let observed = LEAF_DEPTH.with(Cell::get).expect("actual terminal execution");
        if initial != 0 {
            assert_eq!(observed.0, cap, "rejected direct child must not exceed native depth cap");
        }
        observations.push(observed);
        assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
        assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
        assert_eq!(unsafe { &*execution }.active_native_control(), control);
    }
    assert!(observations[1].1 > observations[0].1,
        "depth fallback must execute the middle function on the managed stack");
}
