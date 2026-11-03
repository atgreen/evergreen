// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_tests::ordinary_t2;
use super::super::mapped_ordinary_tests::{install, owner, run_mapped};
use super::*;

thread_local! {
    // Exact code, current Env MV flag, stable context address, actual entries.
    static OBSERVED: Cell<(usize, usize, usize, usize)> = const { Cell::new((0, 0, 0, 0)) };
}
fn observe_entry() {
    let (owner, mv, address, entries) = OBSERVED.with(Cell::get);
    let context = CAPTURE.with(Cell::get);
    assert!(!context.is_null());
    unsafe {
        assert_eq!((*context).owner as usize, owner);
        assert!((*context).ordinary_bridge && (*context).nested.is_null());
        assert!(!(*context).frame.is_null());
        let control = (*context).native_recursive.as_ref().unwrap();
        assert_eq!(
            control.mv_active as usize, mv,
            "current Env, including alternating Env objects"
        );
        assert_eq!(
            control.execution,
            egcl_rt::thread::current_execution_context_for_native().unwrap()
        );
        assert_eq!(
            control.stack_limit,
            egcl_rt::stack::current_native_stack_limit()
        );
        assert_eq!(
            (*context).native_bindings,
            egcl_rt::symbols::native_binding_depth()
        );
    }
    if address != 0 {
        assert_eq!(context as usize, address);
    }
    OBSERVED.with(|slot| slot.set((owner, mv, context as usize, entries + 1)));
}
struct Observing;
impl Drop for Observing {
    fn drop(&mut self) {
        ORDINARY_ENTRY_HOOK.with(|hook| hook.set(None));
        OBSERVED.with(|slot| slot.set((0, 0, 0, 0)));
        super::super::take_activation_counts_for_test();
    }
}

#[test]
#[ignore = "requires actual native bridges; also run with context reuse OFF"]
fn hundred_separate_parents_reuse_one_initialized_context_and_refresh_current_env() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut first = Env::new(false);
    let mut second = Env::new(false);
    egcl_rt::rooted_ref!(_first = &mut first);
    egcl_rt::rooted_ref!(_second = &mut second);
    crate::cli::read_eval_all_env(
        "(defun context-reuse-observe () (%ordinary-t2-entry-for-test))",
        &mut first,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["CONTEXT-REUSE-OBSERVE"]);
    let (symbol, child) = install(
        &mut first,
        "CONTEXT-REUSE-CHILD",
        "(x)",
        "(progn (context-reuse-observe) x)",
    );
    let (parent_symbol, parent) = install(
        &mut first,
        "CONTEXT-REUSE-PARENT",
        "(f x &key (seed (1+ 0)) &aux (u 0))",
        "(funcall f x)",
    );
    assert!(owner(&parent).ordinary_entry.is_none());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    // Settle callable publication outside the measured bridge window, as in
    // the existing separate-parent pool fixture.
    ordinary_t2::expect_entry(&child);
    funcall_fast::apply(*function, &[NIL], &mut first, funcall_fast::Admit::Native)
        .unwrap()
        .unwrap();
    owner(&child).bridge_pool.idle.borrow_mut().clear();
    take_context_constructions_for_test();
    take_context_drops_for_test();
    take_prepares_for_test();
    take_entries_for_test();
    take_storage_births_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::start_activation_counts_for_test(symbol);
    OBSERVED.with(|slot| slot.set((Rc::as_ptr(owner(&child)) as usize, 0, 0, 0)));
    let _observing = Observing;
    for index in 0..100 {
        ordinary_t2::expect_entry(&child);
        let env = if index % 2 == 0 {
            &mut first
        } else {
            &mut second
        };
        OBSERVED.with(|slot| {
            let (owner, _, address, entries) = slot.get();
            slot.set((
                owner,
                std::ptr::from_ref(&env.mv_active) as usize,
                address,
                entries,
            ));
        });
        ORDINARY_ENTRY_HOOK.with(|hook| hook.set(Some(observe_entry)));
        let value = EgclVal::from_fixnum(index);
        assert_eq!(
            run_mapped(parent_symbol, &parent, &[*function, value], env).unwrap(),
            value
        );
        assert!(
            ordinary_t2::observed_entry(),
            "actual child machine body entry {index}"
        );
        let pool = owner(&child).bridge_pool.idle.borrow();
        assert_eq!(pool.len(), 1);
        let bridge = pool[0].as_ref().get_ref();
        assert_eq!(bridge.context.live, context_reuse_enabled());
        if bridge.context.live {
            assert_eq!(
                bridge.context.slot.as_ptr() as usize,
                OBSERVED.with(|slot| slot.get().2)
            );
            assert!(bridge.context.frame.is_null());
            assert!(
                bridge.context.native_recursive.is_none() && bridge.context.native_poll.is_none()
            );
        }
        assert_eq!(super::super::activation_counts_for_test(), (0, 0));
    }
    assert_eq!(OBSERVED.with(|slot| slot.get().3), 100);
    assert_eq!(take_entries_for_test(), 100);
    assert_eq!(
        take_prepares_for_test(),
        100,
        "active ownership checkouts remain per parent"
    );
    assert_eq!(
        take_context_constructions_for_test(),
        if context_reuse_enabled() { 1 } else { 100 }
    );
    assert_eq!(take_storage_births_for_test(), 1);
    assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    assert_eq!(
        take_context_drops_for_test(),
        if context_reuse_enabled() { 0 } else { 100 }
    );
    owner(&child).retire_private_entries();
    assert_eq!(
        take_context_drops_for_test(),
        usize::from(context_reuse_enabled()),
        "retirement immediately drops the retained initialized context"
    );
    assert!(
        owner(&child).bridge_pool.idle.borrow().is_empty(),
        "retirement destroys idle contexts immediately"
    );
}

#[test]
fn initialized_idle_context_drops_with_code_without_a_strong_backedge() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, installed) = install(&mut env, "CONTEXT-NO-CYCLE", "(x)", "x");
    // A private physical version isolates the pool from registry/directory Rc owners.
    let code = Rc::new(
        TransferCode::compile_variant(Arc::clone(&owner(&installed).body), true, false, None)
            .unwrap(),
    );
    let weak = Rc::downgrade(&code);
    let frame = egcl_rt::current_stack()
        .push_frame(NIL, std::ptr::null(), 0, 0)
        .unwrap();
    let _frame = FrameGuard(Some(frame));
    take_context_constructions_for_test();
    take_context_drops_for_test();
    let mut lease = Lease::new(Rc::clone(&code), &mut env, frame);
    lease.bridge_mut().clean_return = true; // Structural lifetime fixture; no native-entry claim.
    drop(lease);
    assert_eq!(code.bridge_pool.idle.borrow().len(), 1);
    assert_eq!(take_context_constructions_for_test(), 1);
    assert_eq!(
        take_context_drops_for_test(),
        usize::from(!context_reuse_enabled())
    );
    drop(code);
    assert!(
        weak.upgrade().is_none(),
        "initialized idle context owns no strong code edge"
    );
    assert_eq!(
        take_context_drops_for_test(),
        usize::from(context_reuse_enabled()),
        "code destruction drops its idle initialized context without dereferencing code pointers"
    );
}
