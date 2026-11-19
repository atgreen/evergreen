// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Retain an executing Rust-owned definition through replacement, without a
//! test-owned strong old-code lease covering either collection.
use super::*;

struct RetainedProbe {
    definition_env: *mut Env,
    symbol: u32,
    old_function: EgclVal,
    new_function: EgclVal,
    payload: EgclVal,
    old_pending: Option<Rc<NativeCode>>,
    old_code: std::rc::Weak<TransferCode>,
    old_owner: usize,
    new_native: Option<Rc<NativeCode>>,
    driver: Rc<NativeCode>,
    driver_capture: *mut CaptureContext,
    phase: usize,
    native_seen: [bool; 4],
    old_calls: usize,
    new_calls: usize,
    anchor: usize,
    sp: usize,
    primary: Vec<EgclVal>,
    tuples: Vec<Vec<EgclVal>>,
    before_minor: u64,
    after_minor: u64,
    after_full: u64,
}
impl TraceHostRoots for RetainedProbe {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.old_function);
        visit(&mut self.new_function);
        visit(&mut self.payload);
        self.primary.trace_host_roots(visit);
        self.tuples.trace_host_roots(visit);
    }
}
thread_local! {
    static RETAINED: Cell<*mut RetainedProbe> = const { Cell::new(std::ptr::null_mut()) };
}
fn save_driver_capture() {
    let probe = RETAINED.with(Cell::get);
    assert!(!probe.is_null());
    unsafe {
        let capture = CAPTURE.with(Cell::get);
        assert!(!capture.is_null());
        assert_eq!((*capture).owner, Rc::as_ptr(owner(&(*probe).driver)));
        (*probe).driver_capture = capture;
    }
}
fn observe_retained_native() {
    let probe = RETAINED.with(Cell::get);
    if probe.is_null() {
        return;
    }
    unsafe {
        let phase = (*probe).phase;
        if phase == 0 || (&(*probe).native_seen)[phase] {
            return;
        }
        let expected = if phase == 1 {
            (*probe).old_owner
        } else {
            Rc::as_ptr(owner(Option::as_ref(&(*probe).new_native).unwrap())) as usize
        };
        let boundary = root_publication::ACTIVE.with(Cell::get);
        if boundary.is_null() || (*boundary).owner() as usize != expected {
            return;
        }
        let context = &*(*boundary).context;
        let stack = egcl_rt::current_stack();
        if enabled() {
            assert_eq!(
                (stack.fp() as usize, stack.sp() as usize),
                ((*probe).anchor, (*probe).sp),
                "replacement and original both use the independent frame-free Root entry"
            );
            assert!(context.ordinary_bridge && context.activation.is_null());
            assert_eq!(context.frame as usize, (*probe).anchor);
        } else {
            assert_ne!(
                stack.fp() as usize,
                (*probe).anchor,
                "OFF keeps its actual managed callee frame"
            );
            assert!(!context.ordinary_bridge);
        }
        assert_ne!((*boundary).context, (*probe).driver_capture);
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = context.native_recursive.as_ref().unwrap();
        assert_eq!(control.execution, execution);
        assert_eq!(
            control.mv_active,
            std::ptr::addr_of!((*NATIVE_ENV.with(Cell::get)).mv_active)
        );
        (&mut (*probe).native_seen)[phase] = true;
    }
}
fn retained_marker() {
    let probe = RETAINED.with(Cell::get);
    assert!(!probe.is_null());
    unsafe {
        if (*probe).phase == 0 {
            drive_retained_calls(probe);
            return;
        }
        assert!(
            (&(*probe).native_seen)[(*probe).phase],
            "actual body marker follows exact native-owner publication"
        );
        if (*probe).phase != 1 {
            (*probe).new_calls += 1;
            return;
        }
        assert_eq!((*probe).old_calls, 0);
        (*probe).old_calls += 1;
        assert!(
            Option::is_none(&(*probe).old_pending),
            "drop test-owned old NativeCode before actual entry"
        );
        assert!(std::rc::Weak::upgrade(&(*probe).old_code).is_some()); // temporary drops before GC
        assert!(root_publication::published_frame_has_owner_for_test(
            (*probe).old_owner
        ));
        (*probe).before_minor = (*probe).payload.to_raw();
        HeapCollector::new().minor_gc().unwrap();
        (*probe).after_minor = (*probe).payload.to_raw();
        assert_ne!(
            (*probe).before_minor,
            (*probe).after_minor,
            "actual old native argument must really move before replacement allocates pinned objects"
        );
        let (_, new) = install(
            &mut *(*probe).definition_env,
            "RR-RETAINED-CALLEE",
            "(x payload)",
            "(progn (rr-retained-observe) (rr-retained-values payload :new (+ x 100) \"new-root-constant\"))",
        );
        assert!(owner(&new).lean_root_entry().is_some());
        assert_ne!(Rc::as_ptr(owner(&new)) as usize, (*probe).old_owner);
        (*probe).new_function = egcl_rt::symbols::symbol_function((*probe).symbol).unwrap();
        assert_ne!((*probe).new_function, (*probe).old_function);
        assert!(NATIVE_REGISTRY.with(|map| {
            map.borrow()
                .get(&(*probe).symbol)
                .is_some_and(|installed| Rc::ptr_eq(installed, &new))
        }));
        (*probe).new_native = Some(new);
        // No upgrade is held over the full collection. The old symbol cell and
        // registry installation are gone; runtime leases/maps retain old code.
        assert!(std::rc::Weak::upgrade(&(*probe).old_code).is_some());
        HeapCollector::new().full_gc().unwrap();
        (*probe).after_full = (*probe).payload.to_raw();
        assert!(
            std::rc::Weak::upgrade(&(*probe).old_code).is_some(),
            "active old code survives both collections"
        );
        assert!(root_publication::published_frame_has_owner_for_test(
            (*probe).old_owner
        ));
        assert!(!native_error_pending());
    }
}
unsafe fn drive_retained_calls(probe: *mut RetainedProbe) {
    unsafe {
        let env = NATIVE_ENV.with(Cell::get);
        assert!(!env.is_null());
        let stack = egcl_rt::current_stack();
        assert!(
            !stack.fp().is_null(),
            "a genuine T0 observer anchors the Rust calls"
        );
        (*probe).anchor = stack.fp() as usize;
        (*probe).sp = stack.sp() as usize;
        let info = (*stack.fp()).code_info;
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = (&*execution).active_native_control();
        let segment = native_transfer::current_segment();
        HeapCollector::new().minor_gc().unwrap();
        (*probe).payload = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL);
        let old = Option::take(&mut (*probe).old_pending).unwrap();
        ordinary_t2::expect_entry(&old);
        drop(old);
        let _capture = CaptureRestore(CAPTURE.with(|slot| slot.replace((*probe).driver_capture)));
        for phase in 1..=3 {
            (*probe).phase = phase;
            let function = match phase {
                1 => (*probe).old_function,
                2 => {
                    ordinary_t2::expect_entry(Option::as_ref(&(*probe).new_native).unwrap());
                    // Fresh named selection must resolve to the replacement.
                    let current = egcl_rt::symbols::symbol_function((*probe).symbol).unwrap();
                    assert_eq!(current, (*probe).new_function);
                    current
                }
                _ => {
                    ordinary_t2::expect_entry(Option::as_ref(&(*probe).new_native).unwrap());
                    let (_, code, current, _) =
                        super::super::super::funcall_fast::native_target((*probe).new_function, 2)
                            .expect("replacement is the function-object cache selection");
                    assert!(Rc::ptr_eq(
                        &code,
                        owner(Option::as_ref(&(*probe).new_native).unwrap())
                    ));
                    assert_eq!(current, (*probe).new_function);
                    current
                }
            };
            let arguments = [EgclVal::from_fixnum(7), (*probe).payload];
            let result = super::super::super::super::native_callable::invoke(
                function, &arguments, &mut *env,
            )
            .unwrap();
            assert!(
                ordinary_t2::observed_entry(),
                "exact selected native body executes"
            );
            Vec::push(&mut (*probe).primary, result);
            Vec::push(&mut (*probe).tuples, (&*env).mv.clone());
            assert_eq!(
                (stack.fp() as usize, stack.sp() as usize),
                ((*probe).anchor, (*probe).sp)
            );
            assert_eq!((*stack.fp()).code_info, info);
            assert_eq!((&*execution).active_native_control(), control);
            assert_eq!(native_transfer::current_segment(), segment);
            assert_eq!(CAPTURE.with(Cell::get), (*probe).driver_capture);
            assert!(!native_error_pending());
        }
        ordinary_t2::expect_entry(&(*probe).driver);
        (*probe).phase = 0;
    }
}
struct RetainedHooks {
    body: Option<fn()>,
    profile: Option<fn()>,
}
impl Drop for RetainedHooks {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.body));
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.profile));
        RETAINED.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

#[test]
#[ignore = "actual Rust-owned retained definition and moving native arguments; run ON/OFF separately"]
fn rust_owned_root_retains_active_definition_across_replacement_and_moving_gc() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *rr-retained-effects* 0 *rr-retained-tags* nil)
         (defun rr-retained-observe () (%ordinary-t2-entry-for-test))
         (defun rr-retained-values (payload tag n literal)
           (setq *rr-retained-effects* (+ *rr-retained-effects* 1))
           (setq *rr-retained-tags* (cons tag *rr-retained-tags*))
           (values payload tag n literal))",
        &mut env,
    )
    .unwrap();
    let _pins =
        ordinary_t2::RecursiveFixturePins::new(&["RR-RETAINED-OBSERVE", "RR-RETAINED-VALUES"]);
    let (symbol, old) = install(
        &mut env,
        "RR-RETAINED-CALLEE",
        "(x payload)",
        "(progn (rr-retained-observe) (rr-retained-values payload :old (+ x 1) \"old-root-constant\"))",
    );
    assert!(owner(&old).lean_root_entry().is_some());
    let (driver_symbol, driver) = install(
        &mut env,
        "RR-RETAINED-DRIVER",
        "()",
        "(progn (rr-retained-observe) 42)",
    );
    egcl_rt::rooted!(
        probe = RetainedProbe {
            definition_env: &mut env,
            symbol,
            old_function: egcl_rt::symbols::symbol_function(symbol).unwrap(),
            new_function: NIL,
            payload: NIL,
            old_code: Rc::downgrade(owner(&old)),
            old_owner: Rc::as_ptr(owner(&old)) as usize,
            old_pending: Some(old),
            new_native: None,
            driver,
            driver_capture: std::ptr::null_mut(),
            phase: 0,
            native_seen: [false; 4],
            old_calls: 0,
            new_calls: 0,
            anchor: 0,
            sp: 0,
            primary: Vec::new(),
            tuples: Vec::new(),
            before_minor: 0,
            after_minor: 0,
            after_full: 0,
        }
    );
    RETAINED.with(|slot| slot.set(&mut *probe));
    let _hooks = RetainedHooks {
        body: ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(retained_marker))),
        profile: root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(observe_retained_native))),
    };
    take_entries_for_test();
    super::super::super::mapped_ordinary::take_entries_for_test();
    super::super::super::start_activation_counts_for_test(symbol);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    ordinary_t2::expect_entry(&probe.driver);
    let driver_function = egcl_rt::symbols::symbol_function(driver_symbol).unwrap();
    let driver_code = Rc::clone(owner(&probe.driver));
    assert_eq!(
        with_ordinary_entry_hook_for_test(save_driver_capture, || driver_code.run_callable(
            driver_function,
            &[],
            &mut env
        ))
        .unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert!(probe.old_pending.is_none());
    assert_eq!((probe.old_calls, probe.new_calls), (1, 2));
    assert_eq!(&probe.native_seen[1..], &[true; 3]);
    assert_ne!(probe.before_minor, probe.after_minor);
    assert_ne!(probe.after_full, 0);
    assert_eq!(take_entries_for_test(), if enabled() { 3 } else { 0 });
    assert_eq!(
        super::super::super::mapped_ordinary::take_entries_for_test(),
        0
    );
    assert_eq!(
        super::super::super::take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (3, 3) }
    );
    let old_tag = reader::intern_keyword_symbol("OLD");
    let new_tag = reader::intern_keyword_symbol("NEW");
    assert_eq!(probe.primary, vec![probe.payload; 3]);
    assert_eq!(probe.tuples.len(), 3);
    for (index, values) in probe.tuples.iter().enumerate() {
        assert_eq!(values.len(), 4);
        assert_eq!(values[0], probe.payload);
        assert_eq!(values[1], if index == 0 { old_tag } else { new_tag });
        assert_eq!(
            values[2],
            EgclVal::from_fixnum(if index == 0 { 8 } else { 107 })
        );
        assert_eq!(
            values[3].as_string(),
            if index == 0 {
                "old-root-constant"
            } else {
                "new-root-constant"
            }
        );
    }
    assert_eq!(
        crate::cli::cp(probe.payload),
        (EgclVal::from_fixnum(73), NIL)
    );
    assert_eq!(
        env.lookup_var("*RR-RETAINED-EFFECTS*"),
        Some(EgclVal::from_fixnum(3))
    );
    assert_eq!(
        crate::cli::list_to_vec(env.lookup_var("*RR-RETAINED-TAGS*").unwrap()),
        vec![new_tag, new_tag, old_tag]
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert!(!native_error_pending());
}
