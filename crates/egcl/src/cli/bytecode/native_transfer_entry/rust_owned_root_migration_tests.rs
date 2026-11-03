// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reuse the established parked-fiber controller, external moving collector,
//! native argument decoder and forced different-carrier completion protocol.
use super::super::super::native_transfer_tests::ordinary_t2;
use super::*;
use egcl_rt::gc::TraceHostRoots;

struct RootProbe {
    middle: Rc<NativeCode>,
    function: EgclVal,
    arguments: Vec<EgclVal>,
    values: Vec<EgclVal>,
    result: EgclVal,
    anchor: usize,
    sp: usize,
    call: usize,
    seen: usize,
    post_resume: bool,
    leaf_previous: Option<fn()>,
    leaf_detached: bool,
}
impl TraceHostRoots for RootProbe {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
        self.arguments.trace_host_roots(visit);
        self.values.trace_host_roots(visit);
        visit(&mut self.result);
    }
}
static ROOT_PROBE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut RootProbe>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };

fn observe_root_publication() {
    let probe = ROOT_PROBE.with(Cell::get);
    if probe.is_null() {
        return;
    }
    unsafe {
        if (*probe).call == 0 || (*probe).seen == (*probe).call {
            return;
        }
        let boundary = root_publication::ACTIVE.with(Cell::get);
        if boundary.is_null() || (*boundary).owner() != Rc::as_ptr(owner(&(*probe).middle)) {
            return;
        }
        let context = &*(*boundary).context;
        let stack = egcl_rt::current_stack();
        assert_eq!(
            stack.fp() as usize,
            (*probe).anchor,
            "Rust-owned native body has no callee VM activation"
        );
        assert_eq!(stack.sp() as usize, (*probe).sp);
        assert!(context.ordinary_bridge && context.activation.is_null());
        assert_eq!(context.frame as usize, (*probe).anchor);
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let control = context.native_recursive.as_ref().unwrap();
        assert_eq!(control.execution, execution);
        assert_eq!(
            (&*execution).active_native_control(),
            std::ptr::from_ref(control) as u64
        );
        assert_eq!(activation_counts_for_test(), (0, 0));
        if (*probe).call == 3 {
            let origin = PARKED_ORIGIN.load(Ordering::Acquire);
            assert_ne!(origin, 0);
            assert_ne!(egcl_rt::current_thread_id().0, origin);
            assert_eq!(control.seen_carrier, (&*execution).carrier_id());
            assert_eq!(
                Some(control.seen_signal_epoch),
                egcl_rt::runtime::current_seen_signal_activity()
            );
            (*probe).post_resume = true;
        }
        (*probe).seen = (*probe).call;
    }
}

fn invoke_root_from_driver() {
    let probe = ROOT_PROBE.with(Cell::get);
    assert!(!probe.is_null());
    // Detach the one-shot thread-local observer BEFORE any suspension. The
    // execution-local publication observer, probe and roots follow migration.
    unsafe {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set((*probe).leaf_previous));
        (*probe).leaf_detached = true;
        let env = NATIVE_ENV.with(Cell::get);
        assert!(!env.is_null());
        let stack = egcl_rt::current_stack();
        assert!(
            !stack.fp().is_null(),
            "genuine T0 helper is the Rust caller's anchor"
        );
        (*probe).anchor = stack.fp() as usize;
        (*probe).sp = stack.sp() as usize;
        let segment = egcl_rt::native_transfer::current_segment();
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let prior_control = (&*execution).active_native_control();
        let first = [1, 2, 3, 4].map(EgclVal::from_fixnum);
        let last = [5, 6, 7, 8].map(EgclVal::from_fixnum);
        for call in 1..=3 {
            (*probe).call = call;
            let actuals = match call {
                1 => first,
                2 => [
                    (&(*probe).arguments)[3],
                    (&(*probe).arguments)[2],
                    (&(*probe).arguments)[1],
                    (&(*probe).arguments)[0],
                ],
                _ => last,
            };
            let value = super::super::super::native_callable::invoke(
                (*probe).function,
                &actuals,
                &mut *env,
            )
            .unwrap();
            assert_eq!(
                (*probe).seen,
                call,
                "exact selected physical native owner published"
            );
            assert_eq!(
                (stack.fp() as usize, stack.sp() as usize),
                ((*probe).anchor, (*probe).sp)
            );
            assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
            assert_eq!((&*execution).active_native_control(), prior_control);
            if call == 2 {
                (*probe).result = value;
                (*probe).values = (&*env).mv.clone();
            } else {
                assert_eq!(value, EgclVal::from_fixnum(if call == 1 { 3 } else { 7 }));
            }
        }
        let values = {
            let values = &(*probe).values;
            values.clone()
        };
        (&mut *env).set_mv(values);
    }
}

struct RootHooks {
    profile: Option<fn()>,
    probe: *mut RootProbe,
}
impl Drop for RootHooks {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.profile));
        ROOT_PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        unsafe {
            if !(*self.probe).leaf_detached {
                ordinary_t2::RECURSIVE_LEAF_OBSERVER
                    .with(|slot| slot.set((*self.probe).leaf_previous));
            }
        }
    }
}

fn root_fiber() -> EgclVal {
    assert!(super::super::rust_ordinary_root::enabled_for_test());
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *rr-migration-body-count* 0)
         (defun rr-migration-drive () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin =
        ordinary_t2::RecursiveFixturePins::new(&["RR-MIGRATION-DRIVE", "MF-OBJECT-FIBER-TERMINAL"]);
    let (symbol, middle) = with_ordinary_non_tail_emission_for_test(|| {
        install_mapped(
            &mut env,
            "MF-OBJECT-FIBER-MIDDLE",
            "(a b c d)",
            "(progn (setq *rr-migration-body-count* (+ *rr-migration-body-count* 1)) (mf-object-fiber-terminal b d a c))",
        )
    });
    assert!(owner(&middle).lean_root_entry().is_some());
    let (driver_symbol, driver) = install_mapped(
        &mut env,
        "MF-OBJECT-FIBER-OUTER",
        "()",
        "(progn (rr-migration-drive) 42)",
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    egcl_rt::rooted!(
        probe = RootProbe {
            middle,
            function: *function,
            arguments: Vec::new(),
            values: Vec::new(),
            result: NIL,
            anchor: 0,
            sp: 0,
            call: 0,
            seen: 0,
            post_resume: false,
            leaf_previous: None,
            leaf_detached: false,
        }
    );
    for index in 0..4 {
        probe.arguments.push(crate::cli::arena_cons(
            EgclVal::from_fixnum(200 + index),
            NIL,
        ));
    }
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let prior_control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    super::super::rust_ordinary_root::take_entries_for_test();
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    take_lean_root_entries_for_test();
    start_activation_counts_for_test(symbol);
    ordinary_t2::expect_entry(&driver);
    probe.leaf_previous = ordinary_t2::RECURSIVE_LEAF_OBSERVER
        .with(|slot| slot.replace(Some(invoke_root_from_driver)));
    ROOT_PROBE.with(|slot| slot.set(&mut *probe));
    let _hooks = RootHooks {
        profile: root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(observe_root_publication))),
        probe: &mut *probe,
    };
    let driver_function = egcl_rt::symbols::symbol_function(driver_symbol).unwrap();
    assert_eq!(
        owner(&driver)
            .run_callable(driver_function, &[], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert_eq!(super::super::rust_ordinary_root::take_entries_for_test(), 3);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 0);
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 0);
    // The explicit driver owns a managed root; the three selected middle calls do not.
    assert_eq!(take_lean_root_entries_for_test(), 1);
    assert_eq!(take_activation_counts_for_test(), (0, 0));
    assert_eq!(
        owner(&probe.middle).deopt_count(),
        0,
        "all three body effects remain native"
    );
    assert_eq!(
        env.lookup_var("*RR-MIGRATION-BODY-COUNT*"),
        Some(EgclVal::from_fixnum(3))
    );
    assert!(probe.post_resume && probe.leaf_detached);
    assert_eq!(probe.result, probe.arguments[2]);
    assert_eq!(probe.values.len(), 4);
    for (value, index) in probe.values.iter().zip([2, 0, 3, 1]) {
        assert_eq!(*value, probe.arguments[index]);
    }
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(
        unsafe { &*execution }.active_native_control(),
        prior_control
    );
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires actual carrier migration and external moving collection"]
fn rust_owned_ordinary_root_survives_migration_external_gc_and_post_resume_entry() {
    run_migration(root_fiber, 2);
}
