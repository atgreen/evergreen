// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_tests::ordinary_t2;
use super::*;
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::{Collector, HeapCollector};
use std::sync::atomic::Ordering;

fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("actual mapped native owner")
    };
    code
}

fn install_native(
    env: &mut Env,
    name: &str,
    params: &str,
    forms: &str,
    tier: u8,
) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {forms})"), env).unwrap();
    let symbol = egcl_rt::symbols::intern(name);
    if tier == 2 {
        return ordinary_t2::install(name, params, &format!("({forms})"), env);
    }
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert!(lazy_compile_defun(
        symbol,
        name,
        egcl_rt::function::lambda_list(*function),
        egcl_rt::function::body(*function),
        env,
    ));
    let native = super::super::install_t1(symbol, registry_get(symbol).unwrap(), false)
        .expect("real installed T1 body");
    assert!(!native.is_t2);
    publish_native(symbol, Some(*function), &native);
    (symbol, native)
}

#[derive(Clone, Copy)]
enum Route {
    Object,
    BytecodeNamed,
}

struct Fixture {
    target: (u32, Rc<NativeCode>),
    driver: (u32, Rc<NativeCode>),
    function: EgclVal,
    _pins: ordinary_t2::RecursiveFixturePins,
}
impl TraceHostRoots for Fixture {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
    }
}
impl Fixture {
    fn new(env: &mut Env, name: &str, params: &str, forms: &str, tier: u8) -> Self {
        let _ = reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST");
        crate::cli::read_eval_all_env(
            "(setq *canonical-root-effects* 0)
             (defun canonical-root-driver-observe () (%ordinary-t2-entry-for-test))
             (defun canonical-root-observe () (%ordinary-t2-entry-for-test))
             (defun canonical-root-values (x)
               (setq *canonical-root-effects* (+ *canonical-root-effects* 1)) (values x 77))
             (defun canonical-root-wide-values (a b c d)
               (setq *canonical-root-effects* (+ *canonical-root-effects* 1)) (values a b c d))",
            env,
        )
        .unwrap();
        // Preserve the real interpreter activation above the driver, and
        // prevent helper tiering from changing the observation/collection site.
        let pins = ordinary_t2::RecursiveFixturePins::new(&[
            "CANONICAL-ROOT-DRIVER-OBSERVE",
            "CANONICAL-ROOT-OBSERVE",
            "CANONICAL-ROOT-VALUES",
            "CANONICAL-ROOT-WIDE-VALUES",
        ]);
        let target = install_native(env, name, params, forms, tier);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(target.0).unwrap());
        let driver = install_native(
            env,
            "CANONICAL-ROOT-DRIVER",
            "()",
            "(progn (canonical-root-driver-observe) 42)",
            2,
        );
        Self {
            target,
            driver,
            function: *function,
            _pins: pins,
        }
    }
    fn run(&self, env: &mut Env, probe: *mut Probe) {
        unsafe {
            (*probe).seen = false;
            (*probe).result = None;
            (*probe).error = None;
        }
        ordinary_t2::expect_entry(&self.driver.1);
        let function = egcl_rt::symbols::symbol_function(self.driver.0).unwrap();
        assert_eq!(
            owner(&self.driver.1)
                .run_callable(function, &[], env)
                .unwrap(),
            EgclVal::from_fixnum(42)
        );
        assert!(
            unsafe { (*probe).seen },
            "selected body must publish its physical native owner"
        );
    }
}

struct Probe {
    function: EgclVal,
    symbol: u32,
    native: Rc<NativeCode>,
    driver: Rc<NativeCode>,
    args: Vec<EgclVal>,
    values: Vec<EgclVal>,
    result: Option<EgclVal>,
    error: Option<EgclError>,
    route: Route,
    in_target: bool,
    seen: bool,
    owner: usize,
    tier: u8,
    anchor: usize,
    sp: usize,
    body_entries: usize,
    collect: bool,
    moved: bool,
    loop_body: bool,
    changing_tier: bool,
    prepares_before: (usize, usize),
}
impl TraceHostRoots for Probe {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
        self.args.trace_host_roots(visit);
        self.values.trace_host_roots(visit);
        self.result.trace_host_roots(visit);
        self.error.trace_host_roots(visit);
    }
}
impl Probe {
    fn new(fixture: &Fixture, route: Route, args: Vec<EgclVal>) -> Self {
        Self {
            function: fixture.function,
            symbol: fixture.target.0,
            native: Rc::clone(&fixture.target.1),
            driver: Rc::clone(&fixture.driver.1),
            args,
            values: Vec::new(),
            result: None,
            error: None,
            route,
            in_target: false,
            seen: false,
            owner: 0,
            tier: 0,
            anchor: 0,
            sp: 0,
            body_entries: 0,
            collect: false,
            moved: false,
            loop_body: false,
            changing_tier: false,
            prepares_before: (0, 0),
        }
    }
}
thread_local! {
    static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) };
}

fn observe_publication() {
    let pointer = PROBE.with(Cell::get);
    if pointer.is_null() || !unsafe { (*pointer).in_target } || unsafe { (*pointer).seen } {
        return;
    }
    let Some(boundary) = (unsafe { root_publication::ACTIVE.with(Cell::get).as_ref() }) else {
        return;
    };
    unsafe {
        let probe = &mut *pointer;
        let physical = &*boundary.owner();
        if physical.body.name != owner(&probe.native).body.name {
            return;
        }
        if !probe.changing_tier && !probe.loop_body {
            assert_eq!(boundary.owner(), Rc::as_ptr(owner(&probe.native)));
        }
        if probe.changing_tier {
            // Selection may have installed T2 before this call. Name/tier
            // metadata alone is not the marker's authority: match the actual
            // published owner to the retained T1 or current native version.
            let selected = if boundary.owner() == Rc::as_ptr(owner(&probe.native)) {
                Rc::clone(&probe.native)
            } else {
                NATIVE_REGISTRY
                    .with(|registry| registry.borrow().get(&probe.symbol).cloned())
                    .expect("selected physical version remains installed")
            };
            assert_eq!(boundary.owner(), Rc::as_ptr(owner(&selected)));
            ordinary_t2::expect_entry(&selected);
        }
        let context = &*boundary.context;
        let expected_new = enabled() && (physical.is_t2 || t1_enabled()) && !probe.loop_body;
        if expected_new {
            assert_eq!(egcl_rt::current_stack().fp() as usize, probe.anchor);
            assert_eq!(egcl_rt::current_stack().sp() as usize, probe.sp);
            assert!(context.ordinary_bridge && context.activation.is_null());
            assert_eq!(context.frame as usize, probe.anchor);
            assert_eq!(
                super::super::activation_counts_for_test(),
                probe.prepares_before
            );
        } else {
            assert_ne!(egcl_rt::current_stack().fp() as usize, probe.anchor);
            assert!(!context.ordinary_bridge);
        }
        probe.seen = true;
        probe.owner = boundary.owner() as usize;
        probe.tier = if physical.is_t2 { 2 } else { 1 };
    }
}

fn observe_body() {
    let pointer = PROBE.with(Cell::get);
    assert!(!pointer.is_null());
    unsafe {
        if (*pointer).in_target {
            (*pointer).body_entries += 1;
            if (*pointer).collect && !(*pointer).moved {
                let before: Vec<u64> = Vec::as_slice(&(*pointer).args)
                    .iter()
                    .map(|value| value.to_raw())
                    .collect();
                HeapCollector::new().minor_gc().unwrap();
                for (value, bits) in Vec::as_slice(&(*pointer).args).iter().zip(before) {
                    assert_ne!(
                        value.to_raw(),
                        bits,
                        "each live argument actually relocates"
                    );
                }
                (*pointer).moved = true;
            }
            return;
        }
        let env = NATIVE_ENV.with(Cell::get);
        assert!(!env.is_null());
        let stack = egcl_rt::current_stack();
        assert!(
            !stack.fp().is_null(),
            "genuine interpreted driver helper anchor"
        );
        if (*pointer).collect {
            HeapCollector::new().minor_gc().unwrap();
            for index in 0..Vec::len(&(*pointer).args) {
                let value = crate::cli::arena_cons(EgclVal::from_fixnum(700 + index as i64), NIL);
                (&mut (*pointer).args)[index] = value;
            }
        }
        (*pointer).anchor = stack.fp() as usize;
        (*pointer).sp = stack.sp() as usize;
        let capture = CAPTURE.with(Cell::get);
        let segment = native_transfer::current_segment();
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = (&*execution).active_native_control();
        (*pointer).prepares_before = super::super::activation_counts_for_test();
        (*pointer).in_target = true;
        if !(*pointer).loop_body && !(*pointer).changing_tier {
            ordinary_t2::expect_entry(&(*pointer).native);
        }
        let arguments = {
            let values = &(*pointer).args;
            &*std::ptr::from_ref(values.as_slice())
        };
        let result = match (*pointer).route {
            Route::Object => super::super::super::native_callable::invoke(
                (*pointer).function,
                arguments,
                &mut *env,
            ),
            // This is the actual canonical named adapter. Its own invocation
            // accounting/native_for_dispatch must precede the new Root hook.
            Route::BytecodeNamed => c2i_call_result(u64::from((*pointer).symbol), arguments, 0),
        };
        (*pointer).values = (&*env).mv.clone();
        match result {
            Ok(value) => (*pointer).result = Some(value),
            Err(error) => (*pointer).error = Some(error),
        }
        assert_eq!(
            (stack.fp() as usize, stack.sp() as usize),
            ((*pointer).anchor, (*pointer).sp)
        );
        assert_eq!(CAPTURE.with(Cell::get), capture);
        assert_eq!(native_transfer::current_segment(), segment);
        assert_eq!((&*execution).active_native_control(), control);
        (*pointer).in_target = false;
        ordinary_t2::expect_entry(&(*pointer).driver);
    }
}

struct Hooks {
    probe: *mut Probe,
    body: Option<fn()>,
    profile: Option<fn()>,
}
impl Hooks {
    fn install(probe: &mut Probe) -> Self {
        Self {
            probe: PROBE.with(|slot| slot.replace(probe)),
            body: ordinary_t2::RECURSIVE_LEAF_OBSERVER
                .with(|slot| slot.replace(Some(observe_body))),
            profile: root_publication::PROFILE_OBSERVE
                .with(|slot| slot.replace(Some(observe_publication))),
        }
    }
}
impl Drop for Hooks {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.profile));
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.body));
        PROBE.with(|slot| slot.set(self.probe));
    }
}

fn heat(function: EgclVal, invokes: u32, edges: u32) {
    unsafe {
        (*egcl_rt::function::invocation_counter_address(function).unwrap())
            .store(invokes, Ordering::Relaxed);
        (*egcl_rt::function::back_edge_counter_address(function).unwrap())
            .store(edges, Ordering::Relaxed);
    }
}
fn reset(symbol: u32) {
    take_entries_for_test();
    super::super::mapped_ordinary::take_entries_for_test();
    super::super::start_activation_counts_for_test(symbol);
}

#[test]
#[ignore = "actual canonical T1 entry; run global Root/T1 Root flags ON and OFF separately"]
fn canonical_t1_object_and_named_calls_credit_once_without_callee_vm_frames() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(!profiling_disabled() && t2_invoke_threshold() > 101 && !egcl_t2_forced());
    for route in [Route::Object, Route::BytecodeNamed] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            fixture = Fixture::new(
                &mut env,
                "CANONICAL-T1-HUNDRED",
                "(x)",
                "(canonical-root-observe) (canonical-root-values x)",
                1
            )
        );
        assert!(owner(&fixture.target.1).lean_root_entry().is_some());
        heat(fixture.function, 0, 0);
        egcl_rt::rooted!(probe = Probe::new(&fixture, route, vec![EgclVal::from_fixnum(71)]));
        let _hooks = Hooks::install(&mut probe);
        let raw = &mut *probe as *mut Probe;
        reset(fixture.target.0);
        for _ in 0..100 {
            fixture.run(&mut env, raw);
            assert!(probe.error.is_none());
            assert_eq!(probe.tier, 1);
            assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
            assert_eq!(
                probe.values,
                vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(77)]
            );
        }
        assert_eq!(probe.body_entries, 100);
        assert_eq!(egcl_rt::function::invoke_count(fixture.function), 100);
        assert_eq!(egcl_rt::function::back_edge_count(fixture.function), 0);
        let new = enabled() && t1_enabled();
        assert_eq!(take_entries_for_test(), if new { 100 } else { 0 });
        assert_eq!(
            super::super::take_activation_counts_for_test(),
            if new { (0, 0) } else { (100, 100) }
        );
        assert_eq!(super::super::mapped_ordinary::take_entries_for_test(), 0);
        assert_eq!(
            env.lookup_var("*CANONICAL-ROOT-EFFECTS*"),
            Some(EgclVal::from_fixnum(100))
        );
    }
}

#[test]
#[ignore = "actual canonical T2 post-dispatch entry; run Root flag ON/OFF"]
fn canonical_t2_named_dispatch_uses_the_existing_root_executor_without_heat_credit() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "CANONICAL-T2-HUNDRED",
            "(x)",
            "(canonical-root-observe) (canonical-root-values x)",
            2
        )
    );
    heat(fixture.function, 0, 0);
    egcl_rt::rooted!(
        probe = Probe::new(
            &fixture,
            Route::BytecodeNamed,
            vec![EgclVal::from_fixnum(71)]
        )
    );
    let _hooks = Hooks::install(&mut probe);
    let raw = &mut *probe as *mut Probe;
    reset(fixture.target.0);
    for _ in 0..100 {
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none());
        assert_eq!(probe.tier, 2);
        assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
    }
    assert_eq!(probe.body_entries, 100);
    assert_eq!(egcl_rt::function::invoke_count(fixture.function), 0);
    assert_eq!(take_entries_for_test(), if enabled() { 100 } else { 0 });
    assert_eq!(
        super::super::take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (100, 100) }
    );
}

#[test]
#[ignore = "actual T1 native overflow argument relocation; no per-allocation stress"]
fn canonical_t1_and_t2_roots_preserve_relocated_register_and_overflow_arguments_and_mv() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for tier in [1, 2] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            fixture = Fixture::new(
                &mut env,
                "CANONICAL-MOVING",
                "(a b c d)",
                "(canonical-root-observe) (canonical-root-wide-values a b c d)",
                tier
            )
        );
        heat(fixture.function, 0, 0);
        egcl_rt::rooted!(
            probe = Probe::new(
                &fixture,
                if tier == 1 {
                    Route::Object
                } else {
                    Route::BytecodeNamed
                },
                vec![NIL; 4],
            )
        );
        probe.collect = true;
        let _hooks = Hooks::install(&mut probe);
        let raw = &mut *probe as *mut Probe;
        reset(fixture.target.0);
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none() && probe.moved);
        assert_eq!(probe.tier, tier);
        assert_eq!(probe.result, Some(probe.args[0]));
        assert_eq!(probe.values, probe.args);
        assert_eq!(probe.body_entries, 1);
        assert_eq!(
            egcl_rt::function::invoke_count(fixture.function),
            u32::from(tier == 1)
        );
        let new = enabled() && (tier == 2 || t1_enabled());
        assert_eq!(
            super::super::take_activation_counts_for_test(),
            if new { (0, 0) } else { (1, 1) }
        );
    }
}

#[test]
#[ignore = "normal broker; invoke and backedge promotion plus next-definition selection"]
fn canonical_root_retains_invocation_and_backedge_promotion_and_next_definition() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(t2_enabled() && t2_invoke_threshold() > 1 && !egcl_t2_forced());
    assert_ne!(std::env::var("EGCL_T2_NO_QUEUE").as_deref(), Ok("1"));
    for edges in [false, true] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(
            fixture = Fixture::new(
                &mut env,
                "CANONICAL-T1-PROMOTE",
                "(x)",
                "(canonical-root-observe) (canonical-root-values x)",
                1
            )
        );
        let invokes = if edges { 0 } else { t2_invoke_threshold() - 1 };
        heat(
            fixture.function,
            invokes,
            if edges { t2_backedge_threshold() } else { 0 },
        );
        egcl_rt::rooted!(
            probe = Probe::new(&fixture, Route::Object, vec![EgclVal::from_fixnum(71)])
        );
        probe.changing_tier = true;
        let _hooks = Hooks::install(&mut probe);
        let raw = &mut *probe as *mut Probe;
        reset(fixture.target.0);
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none());
        assert_eq!(
            egcl_rt::function::invoke_count(fixture.function),
            invokes + 1
        );
        let optimized =
            wait_for_t2(fixture.target.0).expect("canonical selection queued actual T2");
        assert!(optimized.is_t2);
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none());
        assert_eq!(probe.tier, 2);
        assert_eq!(probe.owner, Rc::as_ptr(owner(&optimized)) as usize);
        assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
        assert_eq!(
            egcl_rt::function::invoke_count(fixture.function),
            invokes + 1
        );
        let (_, replacement) = install_native(
            &mut env,
            "CANONICAL-T1-PROMOTE",
            "(x)",
            "(canonical-root-observe) (canonical-root-values (+ x 100))",
            1,
        );
        probe.function = egcl_rt::symbols::symbol_function(fixture.target.0).unwrap();
        assert_ne!(probe.function, fixture.function);
        probe.native = replacement;
        heat(probe.function, 0, 0);
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none());
        assert_eq!(probe.tier, 1);
        assert_eq!(probe.owner, Rc::as_ptr(owner(&probe.native)) as usize);
        assert_eq!(probe.result, Some(EgclVal::from_fixnum(171)));
        assert_eq!(egcl_rt::function::invoke_count(probe.function), 1);
        assert_eq!(probe.body_entries, 3, "one marker per selected native body");
        assert_eq!(
            env.lookup_var("*CANONICAL-ROOT-EFFECTS*"),
            Some(EgclVal::from_fixnum(3))
        );
    }
}

#[test]
#[ignore = "requires EGCL_FORCE_TIER=t2 and the normal enabled broker"]
fn canonical_forced_t2_selection_precedes_root_execution_and_credits_once() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(forced_tier() == Some(ForcedTier::T2));
    assert!(t2_enabled());
    assert_ne!(std::env::var("EGCL_T2_NO_QUEUE").as_deref(), Ok("1"));
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "CANONICAL-FORCED-T2",
            "(x)",
            "(canonical-root-observe) (canonical-root-values x)",
            1
        )
    );
    heat(fixture.function, 0, 0);
    egcl_rt::rooted!(
        probe = Probe::new(
            &fixture,
            Route::BytecodeNamed,
            vec![EgclVal::from_fixnum(71)]
        )
    );
    probe.changing_tier = true;
    let _hooks = Hooks::install(&mut probe);
    let raw = &mut *probe as *mut Probe;
    reset(fixture.target.0);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert_eq!(probe.tier, 2, "force selection completed before entry");
    let installed = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&fixture.target.0).cloned())
        .unwrap();
    assert!(installed.is_t2);
    assert_eq!(probe.owner, Rc::as_ptr(owner(&installed)) as usize);
    assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
    assert_eq!(egcl_rt::function::invoke_count(fixture.function), 1);
    assert_eq!(
        probe.body_entries, 1,
        "forced selected native body executes once"
    );
    assert_eq!(
        env.lookup_var("*CANONICAL-ROOT-EFFECTS*"),
        Some(EgclVal::from_fixnum(1))
    );
}

#[test]
#[ignore = "actual T1 mapped loop fallback, retain OSR companion protocol"]
fn canonical_t1_loop_keeps_the_existing_managed_root_and_heat_protocol() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "CANONICAL-T1-LOOP",
            "(x)",
            "(let ((sum 0)) (dotimes (i x) (setq sum (+ sum i))) (canonical-root-values sum))",
            1
        )
    );
    assert!(!owner(&fixture.target.1).ordinary_root_allowed());
    heat(fixture.function, 0, 0);
    egcl_rt::rooted!(probe = Probe::new(&fixture, Route::Object, vec![EgclVal::from_fixnum(1024)]));
    probe.loop_body = true;
    let _hooks = Hooks::install(&mut probe);
    let raw = &mut *probe as *mut Probe;
    reset(fixture.target.0);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert_eq!(probe.tier, 1);
    assert_eq!(probe.result, Some(EgclVal::from_fixnum(1023 * 1024 / 2)));
    assert_eq!(egcl_rt::function::invoke_count(fixture.function), 1);
    assert!(egcl_rt::function::back_edge_count(fixture.function) > 0);
    assert_eq!(take_entries_for_test(), 0);
    assert_eq!(super::super::take_activation_counts_for_test(), (1, 1));
}
