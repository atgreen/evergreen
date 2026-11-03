// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::super::*;
use super::enabled;
use crate::cli::bytecode::{call_table, native_transfer_tests::ordinary_t2};
use egcl_rt::Collector;

#[path = "typed_mapped_retained_tests.rs"]
mod retained;

struct NativeEnv(*mut Env);
impl NativeEnv {
    fn install(env: &mut Env) -> Self {
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnv {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}

struct Probe {
    owner: usize,
    calls: usize,
    payloads: *mut Vec<EgclVal>,
    before: Vec<u64>,
    after: Vec<u64>,
}
thread_local! {
    static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) };
}
fn observe() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null() {
        return;
    }
    assert!(root_publication::published_frame_has_owner_for_test(
        unsafe { (*probe).owner }
    ));
    unsafe { (*probe).calls += 1 };
    if unsafe { !(*probe).payloads.is_null() && (*probe).before.is_empty() } {
        unsafe {
            (*probe).before = (&*(*probe).payloads).iter().map(|v| v.to_raw()).collect();
        }
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
        unsafe {
            (*probe).after = (&*(*probe).payloads).iter().map(|v| v.to_raw()).collect();
        }
    }
}
struct Observing(Option<fn()>);
impl Observing {
    fn install(probe: &mut Probe) -> Self {
        PROBE.with(|slot| slot.set(probe));
        Self(ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(observe))))
    }
}
impl Drop for Observing {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    }
}
fn install(env: &mut Env, name: &str, params: &str, forms: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {forms})"), env).unwrap();
    ordinary_t2::install(name, params, &format!("({forms})"), env)
}
fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("real installed native owner")
    };
    code
}
fn run_parent(
    symbol: u32,
    native: &NativeCode,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    assert!(
        owner(native).ordinary_entry.is_none(),
        "actual mapped caller required"
    );
    owner(native).run_callable(
        egcl_rt::symbols::symbol_function(symbol).unwrap(),
        args,
        env,
    )
}
fn observer_fixture(env: &mut Env) -> ordinary_t2::RecursiveFixturePins {
    let _ = reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST");
    crate::cli::read_eval_all_env(
        "(defun typed-entry-observe () (%ordinary-t2-entry-for-test))
         (defun typed-entry-values (x) (values x 77))
         (defun typed-wide-values (a b c d) (values a b c d))",
        env,
    )
    .unwrap();
    ordinary_t2::RecursiveFixturePins::new(&[
        "TYPED-ENTRY-OBSERVE",
        "TYPED-ENTRY-VALUES",
        "TYPED-WIDE-VALUES",
    ])
}
fn warm(symbol: u32, child: &NativeCode, args: &[EgclVal]) {
    ordinary_t2::expect_entry(child);
    call_table::ordinary_call(symbol, args).unwrap();
    assert!(ordinary_t2::observed_entry());
}
fn assert_type_error(error: EgclError, expected: EgclVal, env: &Env) {
    // Mapped propagation can signal a raw error before returning it. Preserve
    // the semantic class and exact datum checks for both representations.
    egcl_rt::rooted!(error = error);
    egcl_rt::rooted!(expected = expected);
    match error.without_backtrace() {
        EgclError::TypeError { datum, .. } => assert_eq!(*datum, *expected),
        EgclError::Signalled(details) => {
            egcl_rt::rooted!(condition = details.condition);
            assert!(
                crate::cli::condition_matches_handler(env, *condition, "TYPE-ERROR"),
                "expected TYPE-ERROR, got {:?}",
                &*error
            );
            let slot = crate::cli::resolve_sym("DATUM").expect("condition DATUM symbol");
            let datum = egcl_stdlib::slot_value(*condition, slot)
                .unwrap_or_else(|failure| panic!("TYPE-ERROR datum: {failure:?}; {:?}", &*error));
            assert_eq!(
                datum, *expected,
                "signalled TYPE-ERROR retains the exact datum"
            );
        }
        other => panic!("expected TYPE-ERROR, got {other:?}"),
    }
}

fn reset(symbol: u32) {
    start_activation_counts_for_test(symbol);
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    mapped_generated::take_fast_entries_for_test();
    take_lean_target_calls_for_test();
}
fn probe(child: &NativeCode) -> Probe {
    Probe {
        owner: Rc::as_ptr(owner(child)) as usize,
        calls: 0,
        payloads: std::ptr::null_mut(),
        before: Vec::new(),
        after: Vec::new(),
    }
}

#[test]
#[ignore = "requires actual native entry; run both EGCL_MAPPED_TYPED_ORDINARY=1 and0"]
fn fixed_typed_named_calls_use_the_checking_entry_and_generated_revisit() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = observer_fixture(&mut env);
    let (symbol, child) = install(
        &mut env,
        "TYPED-NAMED-CHILD",
        "(x)",
        "(declare (type fixnum x)) (typed-entry-observe) (typed-entry-values x)",
    );
    let code = owner(&child);
    assert!(child.is_t2 && code.ordinary_checks_params);
    assert!(code.ordinary_descriptor(0).is_some());
    assert!(
        code.lean_root_entry().is_none(),
        "root admission remains untyped"
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(
        code.checked_typed_mapped_entry(*function).is_some(),
        enabled()
    );
    let (parent_symbol, parent) = install(
        &mut env,
        "TYPED-NAMED-PARENT",
        "(x &key (seed (1+ 0)) &aux (u 0))",
        "(declare (notinline typed-named-child)) (typed-named-child x) (typed-named-child x)",
    );
    let _env = NativeEnv::install(&mut env);
    warm(symbol, &child, &[EgclVal::from_fixnum(3)]);
    let cell = call_table::resolve(symbol).unwrap();
    assert!(
        unsafe { call_table::lean_native_target(Arc::as_ptr(&cell) as u64, 1) }.is_none(),
        "Rust-owned selector cannot authorize a typed lean root"
    );
    let before = egcl_rt::function::invoke_count(*function);
    reset(symbol);
    let mut probe = probe(&child);
    let _observing = Observing::install(&mut probe);
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[EgclVal::from_fixnum(31)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(31)
    );
    assert_eq!(probe.calls, 2, "exact native child body executes twice");
    assert!(env.mv_active);
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(31), EgclVal::from_fixnum(77)]
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        before,
        "T2 policy adds no invocation credit"
    );
    assert_eq!(
        take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (2, 2) }
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        if enabled() { 2 } else { 0 }
    );
    assert_eq!(
        mapped_generated::take_fast_entries_for_test(),
        usize::from(enabled()),
        "second named call reuses the existing checking descriptor"
    );
    let (reject_symbol, reject_parent) = install(
        &mut env,
        "TYPED-REVISIT-PARENT",
        "(bad &key (seed (1+ 0)) &aux (u 0))",
        "(declare (notinline typed-named-child)) (typed-named-child 31) (typed-named-child bad)",
    );
    warm(symbol, &child, &[EgclVal::from_fixnum(3)]);
    probe.calls = 0;
    reset(symbol);
    egcl_rt::rooted!(bad = crate::cli::arena_cons(EgclVal::from_fixnum(9), NIL));
    let fp = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    let error = run_parent(reject_symbol, &reject_parent, &[*bad], &mut env).unwrap_err();
    assert_type_error(error, *bad, &env);
    assert_eq!(
        probe.calls, 1,
        "the generated revisit repeats declaration checks before body effects"
    );
    assert_eq!(
        mapped_generated::take_fast_entries_for_test(),
        usize::from(enabled())
    );
    assert_eq!(
        take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (1, 1) }
    );
    assert_eq!(egcl_rt::current_stack().fp(), fp);
    assert_eq!(egcl_rt::current_stack().sp(), sp);
    assert!(!native_error_pending());
    probe.calls = 0;
    assert_eq!(
        run_parent(
            reject_symbol,
            &reject_parent,
            &[EgclVal::from_fixnum(32)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(32)
    );
    assert_eq!(probe.calls, 2, "same parent succeeds after typed rejection");
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(32), EgclVal::from_fixnum(77)]
    );
}

#[test]
#[ignore = "requires actual native entry; run both EGCL_MAPPED_TYPED_ORDINARY=1 and0"]
fn fixed_typed_funcall_keeps_published_and_rust_ingresses_distinct() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = observer_fixture(&mut env);
    let (symbol, child) = install(
        &mut env,
        "TYPED-OBJECT-CHILD",
        "(x)",
        "(declare (type fixnum x)) (typed-entry-observe) (typed-entry-values x)",
    );
    let (parent_symbol, parent) = install(
        &mut env,
        "TYPED-OBJECT-PARENT",
        "(f x &key (seed (1+ 0)) &aux (u 0))",
        "(funcall f x) (funcall f x)",
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let _env = NativeEnv::install(&mut env);
    warm(symbol, &child, &[EgclVal::from_fixnum(3)]);
    ordinary_t2::expect_entry(&child);
    super::super::super::native_callable::invoke(*function, &[EgclVal::from_fixnum(4)], &mut env)
        .unwrap();
    assert!(ordinary_t2::observed_entry());
    reset(symbol);
    let mut probe = probe(&child);
    let _observing = Observing::install(&mut probe);
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[*function, EgclVal::from_fixnum(43)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(43)
    );
    assert_eq!(probe.calls, 2);
    assert!(env.mv_active);
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(43), EgclVal::from_fixnum(77)]
    );
    assert_eq!(
        take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (2, 2) }
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        if enabled() { 2 } else { 0 }
    );
    assert_eq!(
        mapped_generated::take_fast_entries_for_test(),
        0,
        "object calls never bind a named lane"
    );
    reset(symbol);
    assert!(matches!(
        super::super::super::native_callable::invoke(*function, &[NIL], &mut env),
        Err(EgclError::TypeError { .. })
    ));
    assert_eq!(
        probe.calls, 2,
        "Rust wrong-type ingress never executes the body"
    );
    assert_eq!(mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(
        super::super::super::native_callable::invoke(
            *function,
            &[EgclVal::from_fixnum(44)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(44)
    );
    assert_eq!(probe.calls, 3);
    assert_eq!(
        take_activation_counts_for_test(),
        (1, 1),
        "valid typed Rust root keeps its managed validation path"
    );
}

#[test]
#[ignore = "requires native typed entries and actual moving collection"]
fn fixed_typed_overflow_entry_preserves_moved_register_arguments_and_type_errors() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = observer_fixture(&mut env);
    let (symbol, child) = install(
        &mut env,
        "TYPED-WIDE-CHILD",
        "(a b c d)",
        "(declare (type fixnum d)) (typed-entry-observe) (typed-wide-values a b c d)",
    );
    assert!(owner(&child).ordinary_checks_params);
    let (parent_symbol, parent) = install(
        &mut env,
        "TYPED-WIDE-PARENT",
        "(a b c d &key (seed (1+ 0)) &aux (u 0))",
        "(declare (notinline typed-wide-child)) (typed-wide-child a b c d) (typed-wide-child a b c d)",
    );
    let _env = NativeEnv::install(&mut env);
    warm(symbol, &child, &[NIL, NIL, NIL, EgclVal::from_fixnum(7)]);
    // Promote fixture/bootstrap allocations before creating the nursery values
    // whose movement at the actual child publication is the test witness.
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(payloads = Vec::new());
    for n in 0..3 {
        payloads.push(crate::cli::arena_cons(EgclVal::from_fixnum(n), NIL));
    }
    let mut probe = probe(&child);
    probe.payloads = &mut *payloads;
    let _observing = Observing::install(&mut probe);
    reset(symbol);
    egcl_rt::rooted!(
        result = run_parent(
            parent_symbol,
            &parent,
            &[
                payloads[0],
                payloads[1],
                payloads[2],
                EgclVal::from_fixnum(7)
            ],
            &mut env
        )
        .unwrap()
    );
    assert_eq!(probe.calls, 2);
    assert_eq!(probe.before.len(), 3);
    for (before, after) in probe.before.iter().zip(&probe.after) {
        assert_ne!(before, after, "each nursery argument really moves");
    }
    assert_eq!(*result, payloads[0]);
    assert!(env.mv_active);
    assert_eq!(
        env.mv,
        vec![
            payloads[0],
            payloads[1],
            payloads[2],
            EgclVal::from_fixnum(7)
        ]
    );
    assert_eq!(
        take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (2, 2) }
    );
    egcl_rt::rooted!(bad = crate::cli::arena_cons(EgclVal::from_fixnum(99), NIL));
    let fp = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    let depth = NATIVE_DEPTH.with(Cell::get);
    let capture = CAPTURE.with(Cell::get);
    let error = run_parent(
        parent_symbol,
        &parent,
        &[payloads[0], payloads[1], payloads[2], *bad],
        &mut env,
    )
    .unwrap_err();
    assert_type_error(error, *bad, &env);
    assert_eq!(probe.calls, 2, "overflow rejection precedes body effects");
    assert_eq!(egcl_rt::current_stack().fp(), fp);
    assert_eq!(egcl_rt::current_stack().sp(), sp);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
    assert_eq!(CAPTURE.with(Cell::get), capture);
    assert!(!native_error_pending());
    crate::cli::read_eval_all_env(
        "(setq *typed-cleanups* 0)
      (handler-case (unwind-protect (typed-wide-parent nil nil nil (cons 1 nil))
                     (setq *typed-cleanups* (+ *typed-cleanups* 1)))
        (type-error () nil))
      (assert (= *typed-cleanups* 1))",
        &mut env,
    )
    .unwrap();
    assert_eq!(probe.calls, 2);
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[
                payloads[0],
                payloads[1],
                payloads[2],
                EgclVal::from_fixnum(8)
            ],
            &mut env
        )
        .unwrap(),
        payloads[0]
    );
    assert_eq!(
        probe.calls, 4,
        "successful retry after typed error and Lisp unwind"
    );
}

#[test]
#[ignore = "requires EGCL_MAPPED_TYPED_ORDINARY=1 EGCL_MAPPED_BRIDGE_OVERFLOW=0"]
fn typed_bridge_capacity_refusal_keeps_authoritative_root_type_validation() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(enabled() && !mapped_ordinary::overflow_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pins = observer_fixture(&mut env);
    let mut fillers = Vec::new();
    for n in 0..4 {
        fillers.push(install(&mut env, &format!("TYPED-FILL-{n}"), "(x)", "x"));
    }
    let (symbol, child) = install(
        &mut env,
        "TYPED-REFUSED-CHILD",
        "(x)",
        "(declare (type fixnum x)) (typed-entry-observe) (typed-entry-values x)",
    );
    let (parent_symbol, parent) = install(
        &mut env,
        "TYPED-REFUSED-PARENT",
        "(x &key (seed (1+ 0)) &aux (u 0))",
        "(declare (notinline typed-fill-0 typed-fill-1 typed-fill-2 typed-fill-3 typed-refused-child)) (typed-fill-0 0) (typed-fill-1 1) (typed-fill-2 2) (typed-fill-3 3) (typed-refused-child x)",
    );
    let _env = NativeEnv::install(&mut env);
    for (filler, _) in &fillers {
        call_table::ordinary_call(*filler, &[EgclVal::from_fixnum(0)]).unwrap();
    }
    warm(symbol, &child, &[EgclVal::from_fixnum(3)]);
    let mut probe = probe(&child);
    let _observing = Observing::install(&mut probe);
    egcl_rt::rooted!(bad = crate::cli::arena_cons(EgclVal::from_fixnum(9), NIL));
    reset(symbol);
    let fp = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    let error = run_parent(parent_symbol, &parent, &[*bad], &mut env).unwrap_err();
    assert_type_error(error, *bad, &env);
    assert_eq!(
        take_lean_target_calls_for_test(),
        5,
        "typed selector accepted after four real filler targets"
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        4,
        "typed bridge declined its fifth lane"
    );
    assert_eq!(
        probe.calls, 0,
        "root fallback validates before typed body effects"
    );
    assert_eq!(egcl_rt::current_stack().fp(), fp);
    assert_eq!(egcl_rt::current_stack().sp(), sp);
    assert!(!native_error_pending());
    reset(symbol);
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[EgclVal::from_fixnum(51)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(51)
    );
    assert_eq!(probe.calls, 1);
    assert_eq!(
        take_activation_counts_for_test(),
        (1, 1),
        "declined typed bridge uses the original checked managed root"
    );
    assert_eq!(mapped_ordinary::take_entries_for_test(), 4);
    assert!(env.mv_active);
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(51), EgclVal::from_fixnum(77)]
    );
}
