// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

struct ChildProbe {
    owner: usize,
    root_name: String,
    child_name: String,
    seen: bool,
}
thread_local! {
    static CHILD: Cell<*mut ChildProbe> = const { Cell::new(std::ptr::null_mut()) };
}
fn observe_both_native_frames() {
    // Keep the existing exact Rust-root owner, anchor, SP and control witness.
    observe_profile();
    let child = CHILD.with(Cell::get);
    if child.is_null() || unsafe { (*child).seen } {
        return;
    }
    let boundary = root_publication::ACTIVE.with(Cell::get);
    if boundary.is_null() || unsafe { (*boundary).owner() as usize != (*child).owner } {
        return;
    }
    let root = PROBE.with(Cell::get);
    assert!(!root.is_null());
    unsafe {
        assert_eq!(
            egcl_rt::current_stack().fp() as usize,
            (*root).anchor,
            "neither native frame creates a callee VM activation before its guard"
        );
        assert_eq!(egcl_rt::current_stack().sp() as usize, (*root).sp);
        assert!(
            (*root).native_seen && (*root).moved,
            "root marker really ran and relocated the actual first"
        );
        let rows = egcl_rt::debug_stack::capture_current(32);
        for name in [&(*child).root_name, &(*child).child_name] {
            assert!(
                rows.iter().any(
                    |row| row.origin == egcl_rt::debug_stack::FrameOrigin::Native
                        && row
                            .function
                            .as_deref()
                            .is_some_and(|function| function.ends_with(name.as_str()))
                ),
                "both exact physical native owners must be walkable before cold child recovery: {rows:?}"
            );
        }
        (*child).seen = true;
    }
}
struct ObserveChild {
    previous: Option<fn()>,
}
impl Drop for ObserveChild {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.previous));
        CHILD.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

#[test]
#[ignore = "requires Rust-owned ordinary root and two physical native frames"]
fn rust_owned_root_materializes_a_distinct_native_child_guard_once() {
    assert!(
        enabled(),
        "this acceptance case requires EGCL_RUST_ORDINARY_ROOT=1"
    );
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun rr-distinct-guard-child (x) (rust-root-effect x) (values (+ x 1) x))",
        &mut env,
    )
    .unwrap();
    // Preserve BOTH native frames: the root must not tail-replace itself with
    // the child whose ordinary arithmetic guard this case deliberately fails.
    egcl_rt::rooted!(
        fixture = with_ordinary_non_tail_emission_for_test(|| Fixture::new(
            &mut env,
            "RR-DISTINCT-GUARD-ROOT",
            "(x)",
            "(locally (declare (notinline rr-distinct-guard-child)) (progn (rust-root-observe) (rr-distinct-guard-child x)))",
            false
        ))
    );
    let (symbol, child) = with_ordinary_non_tail_emission_for_test(|| {
        install(
            &mut env,
            "RR-DISTINCT-GUARD-CHILD",
            "(x)",
            "(progn (rust-root-effect x) (values (+ x 1) x))",
        )
    });
    assert!(owner(&child).has_deopt);
    assert!(owner(&child).lean_root_entry().is_some());
    // Warm the actual named cell after BOTH code owners exist, not a hand-made
    // direct child pointer that bypasses normal dispatch/admission.
    struct RestoreEnv(*mut Env);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            NATIVE_ENV.with(|slot| slot.set(self.0));
        }
    }
    let saved = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
    assert_eq!(
        super::super::super::super::call_table::ordinary_call(symbol, &[EgclVal::from_fixnum(7)])
            .unwrap(),
        EgclVal::from_fixnum(8)
    );
    drop(saved);
    crate::cli::read_eval_all_env("(setq *rust-root-effects* 0)", &mut env).unwrap();
    let child_before = owner(&child).deopt_count();
    let root_before = owner(&fixture.target.1).deopt_count();
    let mut child_probe = ChildProbe {
        owner: Rc::as_ptr(owner(&child)) as usize,
        root_name: owner(&fixture.target.1).body.name.clone(),
        child_name: owner(&child).body.name.clone(),
        seen: false,
    };
    egcl_rt::rooted!(probe = Probe::new(&fixture, vec![NIL]));
    probe.fresh_args = true;
    probe.collect = true;
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    CHILD.with(|slot| slot.set(&mut child_probe));
    let _child_hook = ObserveChild {
        previous: root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(observe_both_native_frames))),
    };
    super::super::super::start_activation_counts_for_test(fixture.target.0);
    take_entries_for_test();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    fixture.run(&mut env, raw);
    assert!(child_probe.seen && probe.native_seen && probe.moved);
    assert_eq!(
        probe.body_entries, 1,
        "root marker is not replayed by child cold recovery"
    );
    assert_eq!(owner(&child).deopt_count() - child_before, 1);
    assert_eq!(
        owner(&fixture.target.1).deopt_count(),
        root_before,
        "root's own guard never failed"
    );
    assert_eq!(
        env.lookup_var("*RUST-ROOT-EFFECTS*"),
        Some(EgclVal::from_fixnum(1))
    );
    assert!(probe.result.is_none());
    egcl_rt::rooted!(
        error = probe
            .error
            .take()
            .expect("generic child arithmetic rejects its cons")
    );
    match error.without_backtrace() {
        EgclError::TypeError { datum, .. } => assert_eq!(*datum, probe.args[0]),
        EgclError::Signalled(details) => {
            egcl_rt::rooted!(condition = details.condition);
            assert!(crate::cli::condition_matches_handler(
                &env,
                *condition,
                "TYPE-ERROR"
            ));
            let slot = crate::cli::resolve_sym("DATUM").expect("condition datum slot");
            assert_eq!(
                egcl_stdlib::slot_value(*condition, slot).unwrap(),
                probe.args[0],
                "cold child condition retains its exact relocated actual"
            );
        }
        error => panic!("expected TYPE-ERROR, got {error:?}"),
    }
    assert_eq!(take_entries_for_test(), 1);
    assert_eq!(
        super::super::super::take_activation_counts_for_test(),
        (0, 0),
        "cold child materialization creates no root callee VM frame"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert!(!native_error_pending());
    // A normal call after the propagated condition checks error/MV cleanup;
    // it is not counted as proof of a second child-native guard execution.
    probe.fresh_args = false;
    probe.collect = false;
    probe.args[0] = EgclVal::from_fixnum(9);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert_eq!(probe.result, Some(EgclVal::from_fixnum(10)));
    assert_eq!(probe.body_entries, 2);
    assert_eq!(
        probe.values,
        vec![EgclVal::from_fixnum(10), EgclVal::from_fixnum(9)]
    );
    assert_eq!(
        env.lookup_var("*RUST-ROOT-EFFECTS*"),
        Some(EgclVal::from_fixnum(2))
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert!(!native_error_pending());
}
