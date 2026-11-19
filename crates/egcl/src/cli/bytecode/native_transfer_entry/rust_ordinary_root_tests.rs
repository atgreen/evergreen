// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_tests::ordinary_t2;
use super::super::mapped_ordinary_tests::{install, owner};
use super::*;
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::{Collector, HeapCollector};

#[path = "rust_ordinary_root_panic_tests.rs"]
mod panic_tests;

struct Fixture {
    parent: (u32, Rc<NativeCode>),
    target: (u32, Rc<NativeCode>),
    function: EgclVal,
    prototype: EgclVal,
    _pins: ordinary_t2::RecursiveFixturePins,
}
impl TraceHostRoots for Fixture {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
        visit(&mut self.prototype);
    }
}
impl Fixture {
    fn new(env: &mut Env, name: &str, params: &str, body: &str, movable: bool) -> Self {
        crate::cli::read_eval_all_env(
            "(setq *rust-root-effects* 0 *rust-root-input* 2.5d0)
             (defun rust-root-observe () (%ordinary-t2-entry-for-test))
             (defun rust-root-effect (x) (setq *rust-root-effects* (+ *rust-root-effects* 1)) x)
             (defun rust-root-zero-effect () (setq *rust-root-effects* (+ *rust-root-effects* 1)) *rust-root-input*)",
            env,
        ).unwrap();
        let mut target = install(env, name, params, body);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(target.0).unwrap());
        egcl_rt::rooted!(prototype = NIL);
        if movable {
            let body = Arc::clone(target.1.body.as_ref().unwrap());
            *function = super::super::super::heap_env::make_closure(&body, NIL, &[], env).unwrap();
            *prototype = egcl_rt::function::closure_prototype(*function).unwrap();
            target =
                ordinary_t2::install_body(egcl_rt::function::definition_index(*function), body);
        }
        let parent = install(
            env,
            "RUST-ROOT-DRIVER",
            "()",
            "(progn (rust-root-observe) 42)",
        );
        let pins = ordinary_t2::RecursiveFixturePins::new(&[
            "RUST-ROOT-OBSERVE",
            "RUST-ROOT-EFFECT",
            "RUST-ROOT-ZERO-EFFECT",
        ]);
        assert!(owner(&target.1).lean_root_entry().is_some());
        assert!(ordinary_native_body(&owner(&target.1).body));
        assert!(!owner(&target.1).body.has_env);
        Self {
            parent,
            target,
            function: *function,
            prototype: *prototype,
            _pins: pins,
        }
    }
    fn run(&self, env: &mut Env, probe: *mut Probe) {
        unsafe {
            (*probe).parent_owner = Rc::as_ptr(owner(&self.parent.1)) as usize;
            (*probe).target_owner = Rc::as_ptr(owner(&self.target.1)) as usize;
            (*probe).native_seen = false;
            (*probe).result = None;
            (*probe).error = None;
            ordinary_t2::expect_entry(&self.parent.1);
            let function = egcl_rt::symbols::symbol_function(self.parent.0).unwrap();
            let result = with_ordinary_entry_hook_for_test(save_parent_capture, || {
                owner(&self.parent.1).run_callable(function, &[], env)
            })
            .unwrap();
            assert_eq!(result, EgclVal::from_fixnum(42));
            assert!(
                (*probe).native_seen,
                "the selected target must publish its actual native body"
            );
        }
    }
}

struct Probe {
    function: EgclVal,
    prototype: EgclVal,
    args: Vec<EgclVal>,
    target_native: Rc<NativeCode>,
    parent_native: Rc<NativeCode>,
    values: Vec<EgclVal>,
    result: Option<EgclVal>,
    error: Option<EgclError>,
    parent_owner: usize,
    target_owner: usize,
    parent_capture: *mut CaptureContext,
    anchor: usize,
    sp: usize,
    control: u64,
    in_target: bool,
    native_seen: bool,
    expect_new: bool,
    body_entries: usize,
    fresh_args: bool,
    collect: bool,
    moved: bool,
    receiver_moved: bool,
    captured_moved: bool,
    maker: Option<Arc<BytecodeFunction>>,
    seed: EgclVal,
}
impl TraceHostRoots for Probe {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
        visit(&mut self.prototype);
        visit(&mut self.seed);
        self.args.trace_host_roots(visit);
        self.values.trace_host_roots(visit);
        self.result.trace_host_roots(visit);
        self.error.trace_host_roots(visit);
    }
}
impl Probe {
    fn new(fixture: &Fixture, args: Vec<EgclVal>) -> Self {
        Self {
            function: fixture.function,
            prototype: fixture.prototype,
            args,
            target_native: Rc::clone(&fixture.target.1),
            parent_native: Rc::clone(&fixture.parent.1),
            values: Vec::new(),
            result: None,
            error: None,
            parent_owner: 0,
            target_owner: 0,
            parent_capture: std::ptr::null_mut(),
            anchor: 0,
            sp: 0,
            control: 0,
            in_target: false,
            native_seen: false,
            expect_new: enabled(),
            body_entries: 0,
            fresh_args: false,
            collect: false,
            moved: false,
            receiver_moved: false,
            captured_moved: false,
            maker: None,
            seed: NIL,
        }
    }
}
thread_local! { static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) }; }
fn save_parent_capture() {
    let probe = PROBE.with(Cell::get);
    assert!(!probe.is_null());
    unsafe {
        let capture = CAPTURE.with(Cell::get);
        if !capture.is_null() && (*capture).owner as usize == (*probe).parent_owner {
            (*probe).parent_capture = capture;
        }
    }
}
fn observe_profile() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null() || !unsafe { (*probe).in_target } || unsafe { (*probe).native_seen } {
        return;
    }
    let Some(boundary) = (unsafe { root_publication::ACTIVE.with(Cell::get).as_ref() }) else {
        return;
    };
    if boundary.owner() as usize != unsafe { (*probe).target_owner } {
        return;
    }
    unsafe {
        let context = &*boundary.context;
        assert_eq!(context.owner as usize, (*probe).target_owner);
        let stack = egcl_rt::current_stack();
        if (*probe).expect_new {
            assert_eq!(
                stack.fp() as usize,
                (*probe).anchor,
                "no callee VM frame during native body"
            );
            assert_eq!(stack.sp() as usize, (*probe).sp);
            assert!(context.ordinary_bridge && context.activation.is_null());
            assert_eq!(context.frame as usize, (*probe).anchor);
            assert_ne!(
                boundary.context,
                (*probe).parent_capture,
                "independent Rust-owned capture"
            );
        } else {
            assert_ne!(
                stack.fp() as usize,
                (*probe).anchor,
                "OFF retains real callee VM frame"
            );
            assert!(!context.ordinary_bridge);
        }
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = context.native_recursive.as_ref().unwrap();
        assert_eq!(control.execution, execution);
        assert_eq!(
            control.mv_active,
            std::ptr::addr_of!((*NATIVE_ENV.with(Cell::get)).mv_active)
        );
        assert_ne!(std::ptr::from_ref(control) as u64, (*probe).control);
        let rows = egcl_rt::debug_stack::capture_current(32);
        let owner = &*context.owner;
        let name = &owner.body.name;
        assert!(
            rows.iter().any(|row| row.origin
                == if (*probe).expect_new {
                    egcl_rt::debug_stack::FrameOrigin::Native
                } else {
                    egcl_rt::debug_stack::FrameOrigin::Managed
                }
                && row
                    .function
                    .as_deref()
                    .is_some_and(|function| function.ends_with(name.as_str()))),
            "{rows:?}"
        );
        (*probe).native_seen = true;
    }
}
struct CaptureRestore(*mut CaptureContext);
impl Drop for CaptureRestore {
    fn drop(&mut self) {
        CAPTURE.with(|slot| slot.set(self.0));
    }
}
fn observe_body() {
    let probe = PROBE.with(Cell::get);
    assert!(!probe.is_null());
    if unsafe { (*probe).in_target } {
        unsafe {
            (*probe).body_entries += 1;
            if (*probe).collect && !(*probe).moved {
                let last = if (*probe).fresh_args {
                    let args = &(*probe).args;
                    args.last().map(|value| value.to_raw())
                } else {
                    None
                };
                let captured = egcl_rt::function::closure_env((*probe).function).to_raw();
                let receiver = (*probe).function.to_raw();
                HeapCollector::new().minor_gc().unwrap();
                if let Some(before) = last {
                    assert_ne!(
                        {
                            let args = &(*probe).args;
                            args.last().unwrap().to_raw()
                        },
                        before,
                        "last actual, including the overflow home, must really relocate"
                    );
                    (*probe).moved = true;
                }
                if captured != NIL.to_raw() {
                    assert_ne!(
                        egcl_rt::function::closure_env((*probe).function).to_raw(),
                        captured,
                        "actual captured environment must relocate"
                    );
                    (*probe).captured_moved = true;
                }
                if !(*probe).prototype.is_nil() {
                    assert_ne!(
                        (*probe).function.to_raw(),
                        receiver,
                        "fresh selected receiver must relocate"
                    );
                    (*probe).receiver_moved = true;
                }
            }
        }
        return;
    }
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    let stack = egcl_rt::current_stack();
    // This callback runs in an actual Lisp observer activation above the real
    // native driver, not a test-manufactured VM anchor.
    assert!(!stack.fp().is_null());
    unsafe {
        if (*probe).fresh_args || !(*probe).prototype.is_nil() {
            HeapCollector::new().minor_gc().unwrap();
        }
        let maker = {
            let slot = &(*probe).maker;
            slot.as_ref().map(Arc::clone)
        };
        if let Some(maker) = maker {
            (*probe).function = super::super::super::run_with_sym(
                maker,
                &[(*probe).seed],
                NIL,
                u32::MAX,
                &mut *env,
            )
            .unwrap();
        } else if !(*probe).prototype.is_nil() {
            (*probe).function =
                egcl_rt::function::alloc_movable_closure_instance((*probe).prototype).unwrap();
        }
        if (*probe).fresh_args {
            let count = {
                let args = &(*probe).args;
                args.len()
            };
            for index in 0..count {
                let value = crate::cli::arena_cons(EgclVal::from_fixnum(700 + index as i64), NIL);
                (&mut (*probe).args)[index] = value;
            }
        }
        (*probe).anchor = stack.fp() as usize;
        (*probe).sp = stack.sp() as usize;
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        (*probe).control = (&*execution).active_native_control();
        (*probe).in_target = true;
        // Deliberately expose a real older capture below Rust frames. The new
        // seam must mask it, not authorize mapped/generated borrowing from it.
        let _capture = CaptureRestore(CAPTURE.with(|slot| slot.replace((*probe).parent_capture)));
        let parent_segment = native_transfer::current_segment();
        ordinary_t2::expect_entry(&(*probe).target_native);
        let function = (*probe).function;
        let args = {
            let args = &(*probe).args;
            &*std::ptr::from_ref(args.as_slice())
        };
        let result = super::super::super::native_callable::invoke(function, args, &mut *env);
        assert!(
            ordinary_t2::observed_entry(),
            "target body must actually execute"
        );
        (*probe).values = (&*env).mv.clone();
        ordinary_t2::expect_entry(&(*probe).parent_native);
        match result {
            Ok(value) => (*probe).result = Some(value),
            Err(error) => (*probe).error = Some(error),
        }
        assert_eq!(
            (stack.fp() as usize, stack.sp() as usize),
            ((*probe).anchor, (*probe).sp)
        );
        assert_eq!((&*execution).active_native_control(), (*probe).control);
        assert_eq!(native_transfer::current_segment(), parent_segment);
        assert_eq!(CAPTURE.with(Cell::get), (*probe).parent_capture);
        (*probe).in_target = false;
    }
}
struct Hooks {
    body: Option<fn()>,
    profile: Option<fn()>,
}
impl Hooks {
    fn new(probe: &mut Probe) -> Self {
        PROBE.with(|slot| slot.set(probe));
        Self {
            body: ordinary_t2::RECURSIVE_LEAF_OBSERVER
                .with(|slot| slot.replace(Some(observe_body))),
            profile: root_publication::PROFILE_OBSERVE
                .with(|slot| slot.replace(Some(observe_profile))),
        }
    }
}
impl Drop for Hooks {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.body));
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.profile));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

#[test]
fn hundred_rust_owned_calls_execute_body_without_callee_vm_frames() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-HUNDRED",
            "()",
            "(progn (rust-root-observe) (values 71 77))",
            false
        )
    );
    egcl_rt::rooted!(probe = Probe::new(&fixture, Vec::new()));
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    take_entries_for_test();
    super::super::mapped_ordinary::take_entries_for_test();
    super::super::start_activation_counts_for_test(fixture.target.0);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp());
    for _ in 0..100 {
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none());
        assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
        assert_eq!(
            probe.values,
            vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(77)]
        );
        assert_eq!((stack.fp(), stack.sp()), before);
    }
    assert_eq!(probe.body_entries, 100);
    assert_eq!(take_entries_for_test(), if enabled() { 100 } else { 0 });
    assert_eq!(super::super::mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(
        super::super::take_activation_counts_for_test(),
        if probe.expect_new { (0, 0) } else { (100, 100) }
    );
}

#[test]
fn four_argument_rust_owned_entry_relocates_native_overflow_home() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-FOUR",
            "(a b c d)",
            "(progn (rust-root-observe) (values d a b c))",
            false
        )
    );
    egcl_rt::rooted!(probe = Probe::new(&fixture, vec![NIL; 4]));
    probe.fresh_args = true;
    probe.collect = true;
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert!(
        probe.moved,
        "actual fourth argument must move during the native call"
    );
    assert_eq!(probe.body_entries, 1);
    assert_eq!(probe.result, Some(probe.args[3]));
    assert_eq!(
        probe.values,
        vec![probe.args[3], probe.args[0], probe.args[1], probe.args[2]]
    );
}

#[test]
fn rust_owned_ordinary_guard_recovers_above_caller_once_after_movement() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-GUARD",
            "(x)",
            "(progn (rust-root-observe) (values (+ (rust-root-effect x) 1) x 77))",
            false
        )
    );
    assert!(owner(&fixture.target.1).has_deopt);
    let before = owner(&fixture.target.1).deopt_count();
    egcl_rt::rooted!(probe = Probe::new(&fixture, vec![NIL]));
    probe.fresh_args = true;
    probe.collect = true;
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    fixture.run(&mut env, raw);
    assert!(probe.moved);
    assert_eq!(probe.body_entries, 1);
    assert!(probe.result.is_none());
    let error = probe
        .error
        .as_ref()
        .expect("recovered generic arithmetic rejects a cons");
    match error.without_backtrace() {
        EgclError::TypeError { .. } => {}
        EgclError::Signalled(details) => assert!(crate::cli::condition_matches_handler(
            &env,
            details.condition,
            "TYPE-ERROR"
        )),
        other => panic!("expected TYPE-ERROR, got {other:?}"),
    }
    assert_eq!(owner(&fixture.target.1).deopt_count() - before, 1);
    assert_eq!(
        crate::cli::read_eval_all_env("*rust-root-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
}

#[test]
fn rust_owned_guard_relocates_actual_receiver_and_preserves_multiple_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-GUARD-MV",
            "()",
            "(progn (rust-root-observe) (values (+ (rust-root-zero-effect) 1) *rust-root-input* 77))",
            true
        )
    );
    assert!(owner(&fixture.target.1).has_deopt);
    let before = owner(&fixture.target.1).deopt_count();
    egcl_rt::rooted!(probe = Probe::new(&fixture, Vec::new()));
    probe.collect = true;
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none(), "{:?}", probe.error);
    assert!(probe.receiver_moved);
    assert_eq!(probe.body_entries, 1);
    assert_eq!(probe.result.unwrap().as_double_float(), 3.5);
    assert_eq!(probe.values.len(), 3);
    assert_eq!(probe.values[0].as_double_float(), 3.5);
    assert_eq!(probe.values[1].as_double_float(), 2.5);
    assert_eq!(probe.values[2], EgclVal::from_fixnum(77));
    assert_eq!(owner(&fixture.target.1).deopt_count() - before, 1);
    assert_eq!(
        crate::cli::read_eval_all_env("*rust-root-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
}

#[test]
fn rust_owned_entry_without_a_real_vm_anchor_uses_actual_native_root_fallback() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-NO-ANCHOR",
            "()",
            "(progn (rust-root-observe) (values 71 77))",
            false
        )
    );
    let stack = egcl_rt::current_stack();
    assert!(
        stack.fp().is_null(),
        "this control must start without a fabricated VM anchor"
    );
    let before = (stack.fp(), stack.sp());
    egcl_rt::rooted!(probe = Probe::new(&fixture, Vec::new()));
    probe.expect_new = false;
    probe.in_target = true;
    probe.target_owner = Rc::as_ptr(owner(&fixture.target.1)) as usize;
    let _hooks = Hooks::new(&mut probe);
    ordinary_t2::expect_entry(&fixture.target.1);
    take_entries_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::mapped_ordinary::take_entries_for_test();
    super::super::start_activation_counts_for_test(fixture.target.0);
    let result =
        super::super::super::native_callable::invoke(probe.function, &[], &mut env).unwrap();
    assert_eq!(result, EgclVal::from_fixnum(71));
    assert!(ordinary_t2::observed_entry() && probe.native_seen);
    assert_eq!(probe.body_entries, 1);
    assert_eq!(take_entries_for_test(), 0);
    assert_eq!(super::super::mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(super::super::take_lean_root_entries_for_test(), 1);
    assert_eq!(super::super::take_activation_counts_for_test(), (1, 1));
    assert_eq!((stack.fp(), stack.sp()), before);
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(77)]
    );
}

#[test]
fn rust_owned_canonical_heap_instances_keep_distinct_moving_capture_graphs() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(fixture = Fixture::new(&mut env, "RR-UNUSED-SETUP", "(x)", "x", false));
    egcl_rt::rooted!(params = reader::read_from_string("(start)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((let ((total start)) (lambda (next) (let ((old total)) (setq total next) (rust-root-observe) (values old total)))))").unwrap().0);
    let maker =
        Arc::new(compile_function("RR-CAPTURE-MAKER", *params, *forms, &env, true, false).unwrap());
    egcl_rt::rooted!(_maker = super::super::super::ActiveBytecodeRoot::new(&maker));
    let body = Arc::clone(&maker.nested_functions[0]);
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::EnvRef { .. }))
    );
    egcl_rt::rooted!(
        first = super::super::super::run_with_sym(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(11)],
            NIL,
            u32::MAX,
            &mut env
        )
        .unwrap()
    );
    egcl_rt::rooted!(
        second = super::super::super::run_with_sym(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(22)],
            NIL,
            u32::MAX,
            &mut env
        )
        .unwrap()
    );
    assert_ne!(
        egcl_rt::function::closure_env(*first),
        egcl_rt::function::closure_env(*second)
    );
    assert!(!egcl_rt::function::closure_env(*first).is_nil());
    let symbol = egcl_rt::function::definition_index(*first);
    assert_eq!(symbol, egcl_rt::function::definition_index(*second));
    fixture.target = ordinary_t2::install_body(symbol, Arc::clone(&body));
    fixture.function = *first;
    fixture.prototype = egcl_rt::function::closure_prototype(*first).unwrap();
    assert!(owner(&fixture.target.1).native_callable_slot.is_some());
    assert!(owner(&fixture.target.1).accepts_explicit_callable(*first));
    assert!(owner(&fixture.target.1).accepts_explicit_callable(*second));
    egcl_rt::rooted!(probe = Probe::new(&fixture, vec![EgclVal::from_fixnum(77)]));
    probe.maker = Some(maker);
    probe.collect = true;
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    for seed in [11, 22] {
        probe.seed = EgclVal::from_fixnum(seed);
        probe.moved = false;
        probe.receiver_moved = false;
        probe.captured_moved = false;
        fixture.run(&mut env, raw);
        assert!(probe.error.is_none(), "{:?}", probe.error);
        assert!(probe.receiver_moved && probe.captured_moved);
        assert_eq!(probe.result, Some(EgclVal::from_fixnum(seed)));
        assert_eq!(
            probe.values,
            vec![EgclVal::from_fixnum(seed), EgclVal::from_fixnum(77)]
        );
    }
    assert_eq!(probe.body_entries, 2);
}

fn observe_root_poll_error_and_retry() {
    let probe = PROBE.with(Cell::get);
    assert!(!probe.is_null());
    if unsafe { (*probe).in_target } {
        observe_body();
        return;
    }
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    let stack = egcl_rt::current_stack();
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    unsafe {
        assert!(!stack.fp().is_null());
        (*probe).anchor = stack.fp() as usize;
        (*probe).sp = stack.sp() as usize;
        (*probe).control = (&*execution).active_native_control();
        let code_info = (*stack.fp()).code_info;
        let _scope = BlockScopeGuard {
            env,
            blocks: std::mem::take(&mut (*env).block_stack),
            tags: std::mem::take(&mut (*env).tag_stack),
        };
        (*env)
            .block_stack
            .push(("RR-POLL-BLOCK".into(), "rr-poll-block".into()));
        (*env)
            .tag_stack
            .push(("RR-POLL-TAG".into(), "rr-poll-tag".into()));
        let blocks = (*env).block_stack.clone();
        let tags = (*env).tag_stack.clone();
        let catch_base = (*env).catch_stack.len();
        (*env).set_mv(vec![EgclVal::from_fixnum(74), EgclVal::from_fixnum(75)]);
        let _capture = CaptureRestore(CAPTURE.with(|slot| slot.replace((*probe).parent_capture)));
        let segment = native_transfer::current_segment();
        let recovery = (
            egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        );
        let depth = NATIVE_DEPTH.with(Cell::get);
        (*probe).in_target = true;
        // Post after entering the real pinned observer activation. Calling the
        // helper directly avoids an outer callable's poll consuming the signal.
        request_interrupt_for_test();
        assert!(!entry_poll_idle(&*execution));
        let native = Rc::clone(&(*probe).target_native);
        let code = owner(&native);
        let result = try_run(code, (*probe).function, &[], &mut *env);
        let result = if enabled() {
            result.expect("polling commits the Rust-owned entry even when it returns an error")
        } else {
            assert!(
                result.is_none(),
                "OFF declines before touching the pending signal"
            );
            code.run_callable((*probe).function, &[], &mut *env)
        };
        assert!(matches!(result, Err(EgclError::Interrupt)));
        assert_eq!(
            take_entries_for_test(),
            0,
            "entry poll must stop before machine body"
        );
        assert_eq!((*probe).body_entries, 0);
        assert!(!(*probe).native_seen);
        assert_eq!(NATIVE_ENV.with(Cell::get), env);
        assert_eq!(CAPTURE.with(Cell::get), (*probe).parent_capture);
        assert_eq!((&*execution).active_native_control(), (*probe).control);
        assert_eq!(native_transfer::current_segment(), segment);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
        assert_eq!(
            (stack.fp() as usize, stack.sp() as usize),
            ((*probe).anchor, (*probe).sp)
        );
        assert_eq!((*stack.fp()).code_info, code_info);
        assert_eq!((*env).block_stack, blocks);
        assert_eq!((*env).tag_stack, tags);
        assert_eq!((*env).catch_stack.len(), catch_base);
        assert!(!(*env).mv_active && (*env).mv.is_empty());
        assert_eq!(
            (
                egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
                egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip()
            ),
            recovery
        );
        assert!(egcl_rt::thread::take_current_pending_signal().is_none());
        assert!(!native_error_pending());
        // The consumed interrupt cannot poison a subsequent real native entry.
        ordinary_t2::expect_entry(&(*probe).target_native);
        let result = if enabled() {
            try_run(code, (*probe).function, &[], &mut *env).expect("retry remains eligible")
        } else {
            code.run_callable((*probe).function, &[], &mut *env)
        };
        (*probe).result = Some(result.unwrap());
        (*probe).values = (*env).mv.clone();
        assert!(ordinary_t2::observed_entry());
        ordinary_t2::expect_entry(&(*probe).parent_native);
        assert_eq!(CAPTURE.with(Cell::get), (*probe).parent_capture);
        assert_eq!((&*execution).active_native_control(), (*probe).control);
        assert_eq!(native_transfer::current_segment(), segment);
        assert_eq!((*env).block_stack, blocks);
        assert_eq!((*env).tag_stack, tags);
        (*probe).in_target = false;
    }
}

#[test]
fn rust_owned_entry_poll_error_restores_rust_scope_before_native_retry() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-POLL-ERROR",
            "()",
            "(progn (rust-root-observe) (values 71 77))",
            false
        )
    );
    egcl_rt::rooted!(probe = Probe::new(&fixture, Vec::new()));
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    ordinary_t2::RECURSIVE_LEAF_OBSERVER
        .with(|slot| slot.set(Some(observe_root_poll_error_and_retry)));
    take_entries_for_test();
    super::super::mapped_ordinary::take_entries_for_test();
    super::super::start_activation_counts_for_test(fixture.target.0);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
    assert_eq!(
        probe.values,
        vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(77)]
    );
    assert_eq!(probe.body_entries, 1);
    assert_eq!(take_entries_for_test(), if enabled() { 1 } else { 0 });
    assert_eq!(super::super::mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(
        super::super::take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (2, 2) }
    );
}

#[path = "rust_ordinary_root_child_deopt_tests.rs"]
mod child_deopt;
