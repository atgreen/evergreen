// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! An uncached fifth target retains the ordinary ABI without extending the
//! parent's four cached/generated lanes. Every native claim observes its body.
use super::super::super::native_transfer_tests::ordinary_t2;
use super::super::mapped_ordinary_tests::{install, owner, run_mapped};
use super::*;
use egcl_rt::{Collector, HeapCollector};

struct Fixture {
    parent: (u32, Rc<NativeCode>),
    children: Vec<(u32, Rc<NativeCode>)>,
    functions: Vec<EgclVal>,
    object: bool,
}
struct NativeEnv(*mut Env);
impl Drop for NativeEnv {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}
impl TraceHostRoots for Fixture {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.functions.trace_host_roots(visit);
    }
}
impl Fixture {
    fn new(env: &mut Env, tier: u8, object: bool, fifth: &str, cleanup: bool) -> Self {
        egcl_rt::rooted!(functions = Vec::new());
        let mut children = Vec::new();
        for index in 0..5 {
            let name = format!("OVERFLOW-CHILD-{index}");
            let forms = if index == 4 { fifth } else { "x" };
            let (symbol, mut native) = install(env, &name, "(x)", forms);
            if tier == 1 {
                native =
                    install_t1(symbol, Arc::clone(native.body.as_ref().unwrap()), false).unwrap();
                publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &native);
            }
            assert_eq!(native.is_t2, tier == 2);
            assert!(owner(&native).lean_root_entry().is_some());
            functions.push(egcl_rt::symbols::symbol_function(symbol).unwrap());
            children.push((symbol, native));
        }
        let call = |i| {
            if object {
                format!("(funcall f{i} x)")
            } else {
                format!("(overflow-child-{i} x)")
            }
        };
        let prefix = (0..4).map(call).collect::<Vec<_>>().join(" ");
        let last = call(4);
        let work = format!("(progn {prefix} (dotimes (i n) {last}) {last})");
        let work = if cleanup {
            format!("(unwind-protect {work} (setq *overflow-cleanups* (+ *overflow-cleanups* 1)))")
        } else {
            work
        };
        let params = if object {
            "(f0 f1 f2 f3 f4 x n &key (k (1+ 0)) &aux (u 0))"
        } else {
            "(x n &key (k (1+ 0)) &aux (u 0))"
        };
        let forms = format!(
            "(declare (notinline overflow-child-0 overflow-child-1 overflow-child-2 overflow-child-3 overflow-child-4)) {work}"
        );
        let parent = install(env, "OVERFLOW-PARENT", params, &forms);
        assert!(owner(&parent.1).ordinary_entry.is_none());
        if !object {
            // Parent compilation can invalidate named linkage. Cold slots must
            // settle before the measured call: this is an overflow test, not
            // a request to change first-call cold dispatch semantics.
            let _native_env = NativeEnv(NATIVE_ENV.with(|slot| slot.replace(env)));
            ordinary_t2::expect_entry(&children[4].1);
            for (symbol, _) in &children {
                super::super::super::call_table::ordinary_call(*symbol, &[NIL]).unwrap();
            }
        }
        // T1 heat assertions begin after fixture setup and remain below either
        // promotion threshold. T2 invocations retain their no-credit policy.
        for function in functions.iter() {
            unsafe {
                (*egcl_rt::function::invocation_counter_address(*function).unwrap())
                    .store(0, std::sync::atomic::Ordering::Relaxed)
            };
        }
        Self {
            parent,
            children,
            functions: std::mem::take(&mut *functions),
            object,
        }
    }
    fn run(&self, x: EgclVal, repeats: i64, env: &mut Env) -> Result<EgclVal, EgclError> {
        let mut args = if self.object {
            self.functions.clone()
        } else {
            Vec::new()
        };
        args.extend([x, EgclVal::from_fixnum(repeats)]);
        egcl_rt::rooted!(args = args);
        run_mapped(self.parent.0, &self.parent.1, &args, env)
    }
    fn fifth(&self) -> &Rc<TransferCode> {
        owner(&self.children[4].1)
    }
    fn reset(&self) {
        take_entries_for_test();
        take_prepares_for_test();
        take_uncached_calls_for_test();
        super::super::take_lean_root_entries_for_test();
        super::super::start_activation_counts_for_test(self.children[4].0);
        ordinary_t2::expect_entry(&self.children[4].1);
        PROBE.with(|slot| {
            slot.set(Probe {
                owner: Rc::as_ptr(self.fifth()) as usize,
                ..Probe::empty()
            })
        });
    }
    fn assert_route(&self, calls: usize) {
        assert!(ordinary_t2::observed_entry());
        assert_eq!(PROBE.with(|slot| slot.get().calls), calls);
        assert_eq!(take_uncached_calls_for_test(), (calls, 4));
        assert_eq!(take_entries_for_test(), calls + 4);
        assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
        assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    }
}

#[derive(Clone, Copy)]
struct Probe {
    owner: usize,
    calls: usize,
    argument: *const EgclVal,
    receiver: *const EgclVal,
    moved: bool,
    retire: bool,
    panic: bool,
}
impl Probe {
    const fn empty() -> Self {
        Self {
            owner: 0,
            calls: 0,
            argument: std::ptr::null(),
            receiver: std::ptr::null(),
            moved: false,
            retire: false,
            panic: false,
        }
    }
}
thread_local! { static PROBE: Cell<Probe> = const { Cell::new(Probe::empty()) }; }
fn observe() {
    let mut probe = PROBE.with(Cell::get);
    // The pinned T0 marker may have entered through a Rust callable boundary
    // which deliberately masks CAPTURE. Follow its registered segment links,
    // as ordinary_t2_entry_for_test does, to the actual suspended native owner.
    let capture = super::super::child_capture_for_test().or_else(|| {
        super::super::super::native_callable::outer::suspended_callers_for_test()?
            .last()?
            .1
    });
    assert_eq!(
        capture,
        Some((probe.owner, false)),
        "the marker must suspend this exact uncached native owner, not a managed child"
    );
    assert_eq!(super::super::activation_counts_for_test(), (0, 0));
    probe.calls += 1;
    if !probe.argument.is_null() && !probe.moved {
        let before = unsafe { (*probe.argument).to_raw() };
        let receiver_before =
            (!probe.receiver.is_null()).then(|| unsafe { (*probe.receiver).to_raw() });
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(
            unsafe { (*probe.argument).to_raw() },
            before,
            "live uncached argument relocates"
        );
        if let Some(before) = receiver_before {
            assert_ne!(
                unsafe { (*probe.receiver).to_raw() },
                before,
                "uncached receiver really relocates"
            );
        }
        probe.moved = true;
    }
    if probe.retire {
        unsafe { (&*(probe.owner as *const TransferCode)).retire_private_entries() };
    }
    PROBE.with(|slot| slot.set(probe));
    if probe.panic {
        panic!("uncached bridge guarded panic");
    }
}
struct Observing(Option<fn()>);
impl Observing {
    fn new() -> Self {
        Self(ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(observe))))
    }
}
impl Drop for Observing {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        PROBE.with(|slot| slot.set(Probe::empty()));
        super::super::take_activation_counts_for_test();
    }
}
fn bootstrap(env: &mut Env) -> ordinary_t2::RecursiveFixturePins {
    assert!(overflow_enabled() && pooling_enabled());
    crate::cli::read_eval_all_env(
        "(setq *overflow-cleanups* 0) (defun overflow-observe () (%ordinary-t2-entry-for-test))",
        env,
    )
    .unwrap();
    ordinary_t2::RecursiveFixturePins::new(&["OVERFLOW-OBSERVE"])
}

#[test]
#[ignore = "requires native entry and normal promotion thresholds"]
fn fifth_named_and_object_targets_execute_native_without_general_activations() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t2_invoke_threshold() > 105);
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pin = bootstrap(&mut env);
    for (tier, object) in [(1, false), (1, true), (2, false), (2, true)] {
        egcl_rt::rooted!(
            fixture = Fixture::new(
                &mut env,
                tier,
                object,
                "(progn (overflow-observe) x)",
                false
            )
        );
        fixture.reset();
        let _observing = Observing::new();
        take_storage_births_for_test();
        assert_eq!(fixture.run(T, 99, &mut env).unwrap(), T);
        fixture.assert_route(100);
        assert_eq!(
            take_storage_births_for_test(),
            5,
            "four cached plus one reused uncached backing"
        );
        assert_eq!(fixture.fifth().bridge_pool.idle.borrow().len(), 1);
        for (index, function) in fixture.functions.iter().enumerate() {
            assert_eq!(
                egcl_rt::function::invoke_count(*function),
                if tier == 2 {
                    0
                } else if index == 4 {
                    100
                } else {
                    1
                }
            );
        }
    }
}

#[test]
#[ignore = "requires moving native roots and transfer recovery"]
fn uncached_native_call_moves_arguments_preserves_values_and_unwinds_errors_once() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pin = bootstrap(&mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            2,
            true,
            "(progn (overflow-observe) (values x (list x)))",
            false
        )
    );
    fixture.reset();
    let observing = Observing::new();
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(173), NIL));
    PROBE.with(|slot| {
        let mut p = slot.get();
        p.argument = &*argument;
        slot.set(p);
    });
    egcl_rt::rooted!(result = fixture.run(*argument, 0, &mut env).unwrap());
    assert_eq!(*result, *argument);
    assert_eq!(crate::cli::cp(*result), (EgclVal::from_fixnum(173), NIL));
    assert!(PROBE.with(|slot| slot.get().moved));
    assert!(env.mv_active && env.mv.len() == 2);
    assert_eq!(env.mv[0], *argument);
    assert_eq!(crate::cli::cp(env.mv[1]), (*argument, NIL));
    fixture.assert_route(1);
    drop(observing);
    egcl_rt::rooted!(
        errors = Fixture::new(
            &mut env,
            2,
            false,
            "(progn (overflow-observe) (car x))",
            true
        )
    );
    errors.reset();
    let _observing = Observing::new();
    let depth = (
        egcl_rt::current_stack().fp(),
        egcl_rt::current_stack().sp(),
        NATIVE_DEPTH.with(Cell::get),
    );
    let error = errors
        .run(EgclVal::from_fixnum(9), 0, &mut env)
        .expect_err("CAR must reject a fixnum");
    assert!(
        format!("{error:?}").to_ascii_lowercase().contains("list"),
        "{error:?}"
    );
    errors.assert_route(1);
    assert_eq!(
        crate::cli::read_eval_all_env("*overflow-cleanups*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
    assert_eq!(
        (
            egcl_rt::current_stack().fp(),
            egcl_rt::current_stack().sp(),
            NATIVE_DEPTH.with(Cell::get)
        ),
        depth
    );
    assert!(errors.fifth().bridge_pool.idle.borrow().is_empty());
}

#[test]
#[ignore = "requires movable heap closure and native call maps"]
fn uncached_fifth_heap_closure_keeps_its_instance_when_receiver_and_argument_move() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pin = bootstrap(&mut env);
    egcl_rt::rooted!(fixture = Fixture::new(&mut env, 2, true, "x", false));
    egcl_rt::rooted!(params = reader::read_from_string("(seed)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((let ((cell seed)) (lambda (x) (overflow-observe) (cons cell x))))"
        )
        .unwrap()
        .0
    );
    let maker =
        Arc::new(compile_function("OVERFLOW-MAKER", *params, *forms, &env, true, false).unwrap());
    egcl_rt::rooted!(_maker = super::super::super::ActiveBytecodeRoot::new(&maker));
    egcl_rt::rooted!(
        first = super::super::super::run(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(-1)],
            NIL,
            &mut env
        )
        .unwrap()
    );
    let symbol = egcl_rt::function::definition_index(*first);
    let (_, native) = ordinary_t2::install_body(symbol, Arc::clone(&maker.nested_functions[0]));
    assert!(owner(&native).native_callable_slot.is_some());
    assert!(owner(&native).lean_root_entry().is_some());
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(
        receiver = super::super::super::run(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(41)],
            NIL,
            &mut env
        )
        .unwrap()
    );
    fixture.functions[4] = *receiver;
    fixture.children[4] = (symbol, native);
    fixture.reset();
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(82), NIL));
    PROBE.with(|slot| {
        let mut p = slot.get();
        p.argument = &*argument;
        p.receiver = &*receiver;
        slot.set(p);
    });
    let _observing = Observing::new();
    egcl_rt::rooted!(result = fixture.run(*argument, 0, &mut env).unwrap());
    assert_eq!(
        crate::cli::cp(*result),
        (EgclVal::from_fixnum(41), *argument)
    );
    assert_eq!(crate::cli::cp(*argument), (EgclVal::from_fixnum(82), NIL));
    assert!(PROBE.with(|slot| slot.get().moved));
    assert_eq!(fixture.functions[4], *receiver);
    fixture.assert_route(1);
}

#[test]
#[ignore = "requires native retirement and guarded panic recovery"]
fn uncached_retirement_and_guarded_panic_never_pool_dirty_storage_and_retry_reuses() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let _pin = bootstrap(&mut env);
    egcl_rt::rooted!(
        retiring = Fixture::new(&mut env, 2, true, "(progn (overflow-observe) x)", false)
    );
    retiring.reset();
    let observing = Observing::new();
    PROBE.with(|slot| {
        let mut p = slot.get();
        p.retire = true;
        slot.set(p);
    });
    assert_eq!(retiring.run(T, 0, &mut env).unwrap(), T);
    retiring.assert_route(1);
    assert!(retiring.fifth().bridge_pool.retired.get());
    assert!(retiring.fifth().bridge_pool.idle.borrow().is_empty());
    drop(observing);
    egcl_rt::rooted!(
        fixture = Fixture::new(&mut env, 2, true, "(progn (overflow-observe) x)", false)
    );
    fixture.reset();
    let observing = Observing::new();
    PROBE.with(|slot| {
        let mut p = slot.get();
        p.panic = true;
        slot.set(p);
    });
    let stack = egcl_rt::current_stack();
    let before = (
        stack.fp(),
        stack.sp(),
        NATIVE_DEPTH.with(Cell::get),
        CAPTURE.with(Cell::get),
        egcl_rt::native_transfer::current_segment(),
    );
    let error = fixture.run(T, 0, &mut env).unwrap_err();
    assert_eq!(
        (
            stack.fp(),
            stack.sp(),
            NATIVE_DEPTH.with(Cell::get),
            CAPTURE.with(Cell::get),
            egcl_rt::native_transfer::current_segment()
        ),
        before
    );
    assert!(format!("{error:?}").contains("uncached bridge guarded panic"));
    fixture.assert_route(1);
    assert!(fixture.fifth().bridge_pool.idle.borrow().is_empty());
    assert!(!native_error_pending());
    drop(observing);
    for births in [1, 0] {
        fixture.reset();
        let _observing = Observing::new();
        take_storage_births_for_test();
        assert_eq!(fixture.run(T, 0, &mut env).unwrap(), T);
        fixture.assert_route(1);
        assert_eq!(take_storage_births_for_test(), births);
    }
}
