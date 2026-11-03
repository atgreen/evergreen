// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Moving-root evidence for the T1 callee metadata of the mapped child route.
use super::*;
use egcl_rt::Collector;
use std::cell::Cell;
use std::sync::atomic::Ordering;

struct Probe {
    owner: usize,
    // Points to an independently registered root, never an unrooted Lisp word.
    argument: *mut EgclVal,
    before: u64,
    after: u64,
    observed: bool,
}
thread_local! {
    static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) };
}
fn collect_at_t1_publication() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null() || unsafe { (*probe).observed } {
        return;
    }
    let Some((_, owner)) = native_transfer_entry::root_publication::active_anchor_for_test() else {
        return;
    };
    if owner != unsafe { (*probe).owner } {
        return; // Ignore the outer mapped caller's earlier publication.
    }
    assert_eq!(
        native_transfer_entry::child_capture_for_test(),
        Some((owner, false)),
        "the exact T1 bridge owner is executing, not a managed child or T0 fallback"
    );
    assert_eq!(native_transfer_entry::activation_counts_for_test(), (0, 0));
    unsafe {
        (*probe).before = (*(*probe).argument).to_raw();
        (*probe).observed = true;
    }
    // The innermost published PC resolves to the T1 owner and its precise
    // native value map. No scanner borrow is retained across collection.
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    unsafe {
        (*probe).after = (*(*probe).argument).to_raw();
    }
    assert_ne!(
        unsafe { (*probe).before },
        unsafe { (*probe).after },
        "the live nursery argument must actually relocate inside T1"
    );
}
struct Observing(Option<fn()>);
impl Drop for Observing {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::PROFILE_OBSERVE.with(|hook| hook.set(self.0));
        PROBE.with(|probe| probe.set(std::ptr::null_mut()));
    }
}
fn observing(probe: &mut Probe) -> Observing {
    PROBE.with(|slot| slot.set(probe));
    Observing(
        native_transfer_entry::root_publication::PROFILE_OBSERVE
            .with(|hook| hook.replace(Some(collect_at_t1_publication))),
    )
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_t1_child_relocates_nursery_argument_at_its_exact_native_publication() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_ordinary_child_enabled() && !profiling_disabled());
    let _mapped_values = native_transfer_entry::MappedValuesForTest::new();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Keep the existing private test operator in a pinned T0 wrapper. The
    // compiled callee sees a real named function, not an unsupported operator.
    let _ = reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST");
    crate::cli::read_eval_all_env(
        "(defun t1-moving-observer () (%ordinary-t2-entry-for-test))
         (defun t1-moving-child (x) (t1-moving-observer) x)
         (defun t1-moving-parent (x)
           (declare (notinline t1-moving-child))
           (values (t1-moving-child x) :secondary))",
        &mut env,
    )
    .unwrap();
    let _observer_pin =
        native_transfer_tests::ordinary_t2::RecursiveFixturePins::new(&["T1-MOVING-OBSERVER"]);
    let child_symbol = egcl_rt::symbols::intern("T1-MOVING-CHILD");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(child_symbol).unwrap());
    assert!(lazy_compile_defun(
        child_symbol,
        "T1-MOVING-CHILD",
        egcl_rt::function::lambda_list(*function),
        egcl_rt::function::body(*function),
        &env
    ));
    let child =
        native_transfer_entry::install_t1(child_symbol, registry_get(child_symbol).unwrap(), false)
            .expect("real fixed ordinary T1 child");
    assert!(!child.is_t2);
    publish_native(child_symbol, Some(*function), &child);
    let NativeCodeStorage::Mapped(child_code) = &child._storage else {
        panic!("T1 metadata owner");
    };
    assert!(child_code.lean_root_entry().is_some());
    assert!(
        child_code.mapped_entry().is_none(),
        "this T1 runtime-calling body is ordinary-only; its real ordinary entry must run"
    );
    native_transfer_tests::ordinary_t2::expect_entry(&child);
    let (parent_symbol, parent) = native_transfer_tests::ordinary_t2::install(
        "T1-MOVING-PARENT",
        "(x)",
        "((declare (notinline t1-moving-child)) (values (t1-moving-child x) :secondary))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(parent_code) = &parent._storage else {
        panic!("mapped parent");
    };
    assert!(parent_code.ordinary_entry.is_none());
    let cell = resolve(child_symbol).unwrap();
    let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
    publish_target(
        state,
        linkage::revision(cell.ordinal()),
        select_target(child_symbol, 1),
    );
    assert!(
        matches!(state.target.borrow().as_ref(), Some(Target::Native { code, .. }) if !code.is_t2)
    );
    unsafe {
        (*egcl_rt::function::invocation_counter_address(*function).unwrap())
            .store(0, Ordering::Relaxed);
    }
    // Finish fixture allocation before creating the nursery object to move.
    // Bridge setup is Rust-only; publication will force its first collection.
    let _ = crate::cli::arena_cons(NIL, NIL);
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(173), NIL));
    let mut probe = Probe {
        owner: Rc::as_ptr(child_code) as usize,
        argument: std::ptr::from_mut(&mut *argument),
        before: 0,
        after: 0,
        observed: false,
    };
    native_transfer_entry::mapped_ordinary::take_entries_for_test();
    native_transfer_entry::mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_lean_root_entries_for_test();
    native_transfer_entry::start_activation_counts_for_test(child_symbol);
    let observer = observing(&mut probe);
    egcl_rt::rooted!(
        result = parent_code
            .run_callable(
                egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                &[*argument],
                &mut env
            )
            .unwrap()
    );
    drop(observer);
    assert!(
        probe.observed,
        "must collect at the actual T1-owned machine publication"
    );
    assert_ne!(probe.before, probe.after);
    assert_eq!(
        *result, *argument,
        "native callee returns the relocated argument after its runtime call"
    );
    assert_eq!(crate::cli::cp(*result), (EgclVal::from_fixnum(173), NIL));
    assert!(env.mv_active && env.mv.len() == 2);
    assert_eq!(egcl_rt::function::invoke_count(*function), 1);
    assert_eq!(
        native_transfer_entry::mapped_ordinary::take_entries_for_test(),
        1
    );
    assert_eq!(
        native_transfer_entry::mapped_ordinary::take_prepares_for_test(),
        1
    );
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
    assert_eq!(
        native_transfer_entry::take_activation_counts_for_test(),
        (0, 0)
    );
    assert!(egcl_rt::native_transfer::current_segment().is_null());
}
