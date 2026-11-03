// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Requires the coordinated mapped-child no-bind/activation-counter patch.
use super::*;
use std::sync::atomic::Ordering;

fn install_child(env: &mut Env, name: &str) -> (u32, Rc<NativeCode>, Arc<CallCell>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} (x) x)"), env).unwrap();
    let symbol = egcl_rt::symbols::intern(name);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert!(lazy_compile_defun(
        symbol,
        name,
        egcl_rt::function::lambda_list(*function),
        egcl_rt::function::body(*function),
        env
    ));
    let body = registry_get(symbol).expect("canonical lowered DEFUN");
    let native = native_transfer_entry::install_t1(symbol, body, false).expect("actual T1");
    assert!(!native.is_t2);
    publish_native(symbol, Some(*function), &native);
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped T1 storage");
    };
    assert!(code.lean_root_entry().is_some());
    assert!(
        code.mapped_entry().is_some(),
        "production baseline advertises both entries; ON must prefer ordinary child"
    );
    let cell = resolve(symbol).unwrap();
    warm_target(&cell);
    (symbol, native, cell)
}
fn warm_target(cell: &Arc<CallCell>) {
    let state = unsafe { state(Arc::as_ptr(cell) as u64) };
    publish_target(
        state,
        linkage::revision(cell.ordinal()),
        select_target(state.symbol, 1),
    );
    assert!(matches!(
        state.target.borrow().as_ref(),
        Some(Target::Native { .. })
    ));
    assert!(!cell.is_cold());
}
fn set_invocations(function: EgclVal, value: u32) {
    // The fixture retains its real callable; the API exposes pinned metadata.
    unsafe {
        (*egcl_rt::function::invocation_counter_address(function).unwrap())
            .store(value, Ordering::Relaxed);
    }
}
fn set_backedges(function: EgclVal, value: u32) {
    unsafe {
        (*egcl_rt::function::back_edge_counter_address(function).unwrap())
            .store(value, Ordering::Relaxed);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_t1_repeated_child_calls_preserve_heat_without_per_call_activations() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!profiling_disabled());
    assert!(
        t2_invoke_threshold() > 101,
        "run with normal invocation threshold, not force-T2"
    );
    let _mapped_values = native_transfer_entry::MappedValuesForTest::new();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (child_symbol, child, cell) = install_child(&mut env, "MAPPED-T1-COUNTED-CHILD");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(child_symbol).unwrap());
    let calls = vec!["(mapped-t1-counted-child x)"; 100].join(" ");
    let forms = format!(
        "((declare (notinline mapped-t1-counted-child)) (values (progn {calls}) :secondary))"
    );
    crate::cli::read_eval_all_env(
        &format!(
            "(defun mapped-t1-counted-parent (x) {} )",
            &forms[1..forms.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let (parent_symbol, parent) = native_transfer_tests::ordinary_t2::install(
        "MAPPED-T1-COUNTED-PARENT",
        "(x)",
        &forms,
        &mut env,
    );
    let NativeCodeStorage::Mapped(parent_code) = &parent._storage else {
        panic!("mapped parent");
    };
    assert!(
        parent_code.ordinary_entry.is_none(),
        "actual mapped caller required"
    );
    assert!(parent.is_t2 && !child.is_t2);
    // Installing a caller can invalidate linkage. Re-select the exact T1
    // target before resetting heat, without executing a priming invocation.
    warm_target(&cell);
    set_invocations(*function, 0);
    set_backedges(*function, 0);
    native_transfer_entry::mapped_ordinary::take_entries_for_test();
    native_transfer_entry::mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_lean_root_entries_for_test();
    native_transfer_entry::take_nested_entries();
    native_transfer_entry::start_activation_counts_for_test(child_symbol);
    let result = parent_code
        .run_callable(
            egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
            &[EgclVal::from_fixnum(47)],
            &mut env,
        )
        .unwrap();
    assert_eq!(result, EgclVal::from_fixnum(47));
    assert!(
        env.mv_active && env.mv.len() == 2,
        "mapped caller preserves observable MV"
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        100,
        "each T1 call credits exactly once; no generated binding may bypass heat"
    );
    assert_eq!(egcl_rt::function::back_edge_count(*function), 0);
    assert!(!child.is_t2, "this evidence exercises T1 machine code");
    if t1_ordinary_child_enabled() {
        assert_eq!(native_transfer_entry::take_nested_entries(), 0);
        assert_eq!(
            native_transfer_entry::mapped_ordinary::take_entries_for_test(),
            100
        );
        assert_eq!(
            native_transfer_entry::mapped_ordinary::take_prepares_for_test(),
            1
        );
        assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
        assert_eq!(
            native_transfer_entry::take_activation_counts_for_test(),
            (0, 0),
            "no managed preparation or general native-child recycling, including parent teardown"
        );
    } else {
        assert_eq!(
            native_transfer_entry::mapped_ordinary::take_entries_for_test(),
            0
        );
        assert_eq!(native_transfer_entry::take_nested_entries(), 100);
        assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
        assert_eq!(
            native_transfer_entry::take_activation_counts_for_test(),
            (100, 100)
        );
    }
}

#[test]
fn t1_ordinary_selection_credits_only_after_all_guards_and_declines_promotion_boundary() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!profiling_disabled());
    assert!(
        t2_invoke_threshold() > 2 && t2_enabled(),
        "normal tiering fixture required"
    );
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, _native, cell) = install_child(&mut env, "MAPPED-T1-SELECTION-CHILD");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let state = unsafe { state(Arc::as_ptr(&cell) as u64) };
    assert!(matches!(
        state.target.borrow().as_ref(),
        Some(Target::Native { promote: true, .. })
    ));
    let address = Arc::as_ptr(&cell) as u64;
    set_invocations(*function, 0);
    set_backedges(*function, 0);
    egcl_rt::call_table::invalidate(symbol);
    assert!(cell.is_cold());
    assert!(unsafe { lean_native_target(address, 1) }.is_none());
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        0,
        "cold target credits nothing"
    );
    warm_target(&cell);
    assert_eq!(
        unsafe { mapped_callee_declines(address) },
        t1_ordinary_child_enabled(),
        "dual-entry preparation preference follows the same comparison control"
    );
    assert_eq!(
        unsafe { mapped_callee(address) }.is_none(),
        t1_ordinary_child_enabled()
    );
    assert!(
        mapped_callable(*function, 1).is_some(),
        "T1 function objects retain their managed child until FUNCALL heat admission exists"
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        0,
        "route preference itself never credits heat"
    );
    assert!(unsafe { lean_native_target(address, 0) }.is_none());
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        0,
        "arity miss credits nothing"
    );
    let old_depth = NATIVE_DEPTH.with(|depth| depth.replace(native_depth_cap()));
    let selected = unsafe { lean_native_target(address, 1) };
    NATIVE_DEPTH.with(|depth| depth.set(old_depth));
    assert!(selected.is_none());
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        0,
        "depth miss credits nothing"
    );
    if t1_ordinary_child_enabled() {
        set_invocations(*function, t2_invoke_threshold() - 2);
        assert!(unsafe { lean_native_target(address, 1) }.is_some());
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            t2_invoke_threshold() - 1
        );
        assert!(
            unsafe { lean_native_target(address, 1) }.is_none(),
            "threshold invocation must take cold promotion path"
        );
        assert!(
            !unsafe { mapped_callee_declines(address) },
            "promotion boundary retains the original preparation poll"
        );
        assert!(unsafe { mapped_callee(address) }.is_none());
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            t2_invoke_threshold() - 1,
            "cold caller will credit once, selector must not pre-credit"
        );
        set_invocations(*function, 0);
        set_backedges(*function, t2_backedge_threshold());
        assert!(
            unsafe { lean_native_target(address, 1) }.is_none(),
            "loop-hot promotion retains original path"
        );
        assert_eq!(egcl_rt::function::invoke_count(*function), 0);
        set_backedges(*function, t2_backedge_threshold() - 1);
        assert!(unsafe { lean_native_target(address, 1) }.is_some());
        assert_eq!(egcl_rt::function::invoke_count(*function), 1);
    }
    if t1_ordinary_child_enabled() {
        T2_DECLINED.with(|declined| declined.borrow_mut().insert(symbol));
        warm_target(&cell);
        assert!(matches!(
            state.target.borrow().as_ref(),
            Some(Target::Native { promote: false, .. })
        ));
        set_invocations(*function, t2_invoke_threshold());
        set_backedges(*function, t2_backedge_threshold());
        assert!(
            unsafe { lean_native_target(address, 1) }.is_some(),
            "declined T2 target stays eligible T1 even beyond thresholds"
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            t2_invoke_threshold() + 1
        );
        T2_DECLINED.with(|declined| declined.borrow_mut().remove(&symbol));
    }
    let before = egcl_rt::function::invoke_count(*function);
    crate::cli::read_eval_all_env(
        "(defun mapped-t1-selection-child (x) :replacement)",
        &mut env,
    )
    .unwrap();
    assert!(cell.is_cold(), "redefinition invalidates old publication");
    assert!(unsafe { lean_native_target(address, 1) }.is_none());
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        before,
        "retained old target must not be counted after replacement"
    );
}

#[test]
#[ignore = "requires native entry and the normal asynchronous T2 broker"]
fn mapped_t1_cold_threshold_call_credits_once_and_publishes_t2_for_both_heat_sources() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!profiling_disabled() && t2_enabled());
    assert!(
        std::env::var("EGCL_T2_NO_QUEUE").as_deref() != Ok("1"),
        "broker must be enabled"
    );
    let _mapped_values = native_transfer_entry::MappedValuesForTest::new();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for loop_hot in [false, true] {
        let suffix = if loop_hot { "BACKEDGE" } else { "INVOKE" };
        let child_name = format!("MAPPED-T1-PROMOTE-{suffix}-CHILD");
        let parent_name = format!("MAPPED-T1-PROMOTE-{suffix}-PARENT");
        let (symbol, old_native, cell) = install_child(&mut env, &child_name);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let forms =
            format!("((declare (notinline {child_name})) (values ({child_name} x) :secondary))");
        crate::cli::read_eval_all_env(
            &format!("(defun {parent_name} (x) {} )", &forms[1..forms.len() - 1]),
            &mut env,
        )
        .unwrap();
        let (parent_symbol, parent) =
            native_transfer_tests::ordinary_t2::install(&parent_name, "(x)", &forms, &mut env);
        let NativeCodeStorage::Mapped(parent_code) = &parent._storage else {
            panic!("mapped parent");
        };
        assert!(parent_code.ordinary_entry.is_none());
        warm_target(&cell);
        let initial = if loop_hot {
            0
        } else {
            t2_invoke_threshold() - 1
        };
        set_invocations(*function, initial);
        set_backedges(
            *function,
            if loop_hot { t2_backedge_threshold() } else { 0 },
        );
        assert!(unsafe { lean_native_target(Arc::as_ptr(&cell) as u64, 1) }.is_none());
        assert!(
            unsafe { mapped_callee(Arc::as_ptr(&cell) as u64) }.is_none(),
            "promotion boundary remains cold even with a mapped entry"
        );
        assert_eq!(egcl_rt::function::invoke_count(*function), initial);
        native_transfer_entry::mapped_ordinary::take_entries_for_test();
        assert_eq!(
            parent_code
                .run_callable(
                    egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                    &[T],
                    &mut env
                )
                .unwrap(),
            T
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            initial + 1,
            "threshold call declines selector without pre-credit, then cold dispatch credits once"
        );
        assert_eq!(
            native_transfer_entry::mapped_ordinary::take_entries_for_test(),
            0,
            "threshold invocation must use the original cold transition"
        );
        let replacement = wait_for_t2(symbol).expect("real queued T2 publication");
        assert!(replacement.is_t2);
        assert!(!Rc::ptr_eq(&replacement, &old_native));
        assert_eq!(egcl_rt::function::tier(*function), 2);
        warm_target(&cell);
        assert!(
            matches!(unsafe { state(Arc::as_ptr(&cell) as u64) }.target.borrow().as_ref(), Some(Target::Native { code, .. }) if code.is_t2)
        );
        let count = egcl_rt::function::invoke_count(*function);
        assert_eq!(
            parent_code
                .run_callable(
                    egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                    &[NIL],
                    &mut env
                )
                .unwrap(),
            NIL
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            count,
            "installed T2 does not acquire T1 credits"
        );
        assert_eq!(
            native_transfer_entry::mapped_ordinary::take_entries_for_test(),
            1,
            "replacement enters actual ordinary native child"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_t2_dual_entry_child_avoids_managed_preparation_and_does_not_count_t1_heat() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!profiling_disabled());
    let _mapped_values = native_transfer_entry::MappedValuesForTest::new();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (child_symbol, _baseline, cell) = install_child(&mut env, "MAPPED-T2-DUAL-COUNTED-CHILD");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(child_symbol).unwrap());
    let (_, child) = native_transfer_tests::ordinary_t2::install_body(
        child_symbol,
        registry_get(child_symbol).unwrap(),
    );
    assert!(child.is_t2);
    assert_eq!(egcl_rt::function::tier(*function), 2);
    let NativeCodeStorage::Mapped(child_code) = &child._storage else {
        panic!("T2 owner");
    };
    assert!(
        child_code.ordinary_entry.is_some() && child_code.mapped_entry().is_some(),
        "this test must exercise real dual-entry T2 production code"
    );
    let calls = vec!["(mapped-t2-dual-counted-child x)"; 100].join(" ");
    let forms = format!(
        "((declare (notinline mapped-t2-dual-counted-child)) (values (progn {calls}) :secondary))"
    );
    crate::cli::read_eval_all_env(
        &format!(
            "(defun mapped-t2-dual-counted-parent (x) {})",
            &forms[1..forms.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let (parent_symbol, parent) = native_transfer_tests::ordinary_t2::install(
        "MAPPED-T2-DUAL-COUNTED-PARENT",
        "(x)",
        &forms,
        &mut env,
    );
    let NativeCodeStorage::Mapped(parent_code) = &parent._storage else {
        panic!("mapped parent");
    };
    assert!(parent_code.ordinary_entry.is_none());
    warm_target(&cell);
    assert!(
        prefer_ordinary_child(&child, *function, false),
        "T2 preference is independent of the T1 comparison switch"
    );
    assert!(
        !prefer_ordinary_child(&child, *function, true),
        "retain the promote:false T2 contract"
    );
    assert!(unsafe { mapped_callee_declines(Arc::as_ptr(&cell) as u64) });
    assert!(unsafe { mapped_callee(Arc::as_ptr(&cell) as u64) }.is_none());
    assert!(
        mapped_callable(*function, 1).is_none(),
        "T2 object calls share ordinary preference with the mirrored FUNCALL child flag"
    );
    set_invocations(*function, 17);
    set_backedges(*function, 0);
    native_transfer_entry::mapped_ordinary::take_entries_for_test();
    native_transfer_entry::mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_nested_entries();
    native_transfer_entry::take_lean_root_entries_for_test();
    native_transfer_entry::start_activation_counts_for_test(child_symbol);
    let result = parent_code
        .run_callable(
            egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
            &[EgclVal::from_fixnum(61)],
            &mut env,
        )
        .unwrap();
    assert_eq!(result, EgclVal::from_fixnum(61));
    assert!(env.mv_active && env.mv.len() == 2);
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        17,
        "T2 never acquires T1 invocation heat"
    );
    assert_eq!(egcl_rt::function::back_edge_count(*function), 0);
    assert_eq!(native_transfer_entry::take_nested_entries(), 0);
    assert_eq!(
        native_transfer_entry::mapped_ordinary::take_entries_for_test(),
        100
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
