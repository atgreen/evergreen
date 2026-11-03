// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A pre-entry Rust panic is caught above every machine invocation. The retry
//! must publish the real selected native body, not merely reach the entry hook.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

const PANIC_TAG: &str = "bounded Rust-owned pre-entry panic";

fn panic_before_root_machine_entry() {
    let probe = PROBE.with(Cell::get);
    assert!(!probe.is_null());
    let context = CAPTURE.with(Cell::get);
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!context.is_null() && !env.is_null());
    unsafe {
        assert_ne!(context, (*probe).parent_capture);
        assert_eq!((*context).owner as usize, (*probe).target_owner);
        assert_eq!((*context).ordinary_bridge, enabled());
        assert!((*env).block_stack.is_empty() && (*env).tag_stack.is_empty());
        assert!(!(*env).mv_active && (*env).mv.is_empty());
        let control = (*context).native_recursive.as_ref().unwrap();
        assert_eq!(control.mv_active, std::ptr::addr_of!((*env).mv_active));
    }
    assert_eq!(
        egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
        0
    );
    assert_eq!(
        egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        0
    );
    std::panic::panic_any(PANIC_TAG);
}

fn observe_pre_entry_panic_then_retry() {
    let probe = PROBE.with(Cell::get);
    assert!(!probe.is_null());
    if unsafe { (*probe).in_target } {
        observe_body();
        return;
    }
    let outer_env = NATIVE_ENV.with(Cell::get);
    assert!(!outer_env.is_null());
    // A separate rooted Env makes restoration of NATIVE_ENV observable rather
    // than comparing two pointers that happened to denote the same Env.
    unsafe { (*outer_env).set_mv(vec![EgclVal::from_fixnum(900)]) };
    let mut child_env = unsafe { (&*outer_env).clone() };
    egcl_rt::rooted_ref!(_child_env = &mut child_env);
    child_env.block_stack = vec![("RR-PANIC-BLOCK".into(), "rr-panic-block".into())];
    child_env.tag_stack = vec![("RR-PANIC-TAG".into(), "rr-panic-tag".into())];
    child_env.set_mv(vec![EgclVal::from_fixnum(74), EgclVal::from_fixnum(75)]);
    let blocks = child_env.block_stack.clone();
    let tags = child_env.tag_stack.clone();
    let catch_base = child_env.catch_stack.len();
    let stack = egcl_rt::current_stack();
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    assert!(!stack.fp().is_null());
    unsafe {
        (*probe).anchor = stack.fp() as usize;
        (*probe).sp = stack.sp() as usize;
        (*probe).control = (&*execution).active_native_control();
        let code_info = (*stack.fp()).code_info;
        let _capture = CaptureRestore(CAPTURE.with(|slot| slot.replace((*probe).parent_capture)));
        let segment = native_transfer::current_segment();
        let depth = NATIVE_DEPTH.with(Cell::get);
        let recovery = (
            egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        );
        let native = Rc::clone(&(*probe).target_native);
        let code = owner(&native);
        (*probe).in_target = true;
        ordinary_t2::expect_entry(&native);
        // Direct helper/old-root calls keep this catch inside the real pinned
        // T0 observer callback. The hook runs before invoke_ordinary_native_segment;
        // no panic unwinds through any newly entered machine frame.
        let caught = catch_unwind(AssertUnwindSafe(|| {
            with_ordinary_entry_hook_for_test(panic_before_root_machine_entry, || {
                if enabled() {
                    try_run(code, (*probe).function, &[], &mut child_env)
                        .expect("eligible Rust-owned entry must reach its pre-entry hook")
                } else {
                    code.run_callable((*probe).function, &[], &mut child_env)
                }
            })
        }))
        .expect_err("the one-shot pre-entry hook must panic");
        assert_eq!(caught.downcast_ref::<&str>().copied(), Some(PANIC_TAG));
        assert_eq!((*probe).body_entries, 0);
        assert!(!(*probe).native_seen && !ordinary_t2::observed_entry());
        assert_eq!(NATIVE_ENV.with(Cell::get), outer_env);
        assert_eq!(CAPTURE.with(Cell::get), (*probe).parent_capture);
        assert_eq!((&*execution).active_native_control(), (*probe).control);
        assert_eq!(native_transfer::current_segment(), segment);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
        assert_eq!(
            (stack.fp() as usize, stack.sp() as usize),
            ((*probe).anchor, (*probe).sp)
        );
        assert_eq!((*stack.fp()).code_info, code_info);
        assert_eq!(child_env.block_stack, blocks);
        assert_eq!(child_env.tag_stack, tags);
        assert_eq!(child_env.catch_stack.len(), catch_base);
        assert!(!child_env.mv_active && child_env.mv.is_empty());
        assert!((*outer_env).mv_active);
        assert_eq!((*outer_env).mv, vec![EgclVal::from_fixnum(900)]);
        assert_eq!(
            (
                egcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
                egcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip()
            ),
            recovery
        );
        assert!(!native_error_pending());
        // with_ordinary_entry_hook_for_test must have retired its hook while
        // unwinding. This second entry must run and publish the actual body.
        ordinary_t2::expect_entry(&native);
        let result = if enabled() {
            try_run(code, (*probe).function, &[], &mut child_env).expect("retry stays eligible")
        } else {
            code.run_callable((*probe).function, &[], &mut child_env)
        };
        (*probe).result = Some(result.unwrap());
        (*probe).values = child_env.mv.clone();
        assert!(ordinary_t2::observed_entry());
        ordinary_t2::expect_entry(&(*probe).parent_native);
        assert_eq!(NATIVE_ENV.with(Cell::get), outer_env);
        assert_eq!(CAPTURE.with(Cell::get), (*probe).parent_capture);
        assert_eq!((&*execution).active_native_control(), (*probe).control);
        assert_eq!(native_transfer::current_segment(), segment);
        assert_eq!(child_env.block_stack, blocks);
        assert_eq!(child_env.tag_stack, tags);
        assert!((*outer_env).mv_active);
        assert_eq!((*outer_env).mv, vec![EgclVal::from_fixnum(900)]);
        (*probe).in_target = false;
    }
}

#[test]
fn pre_entry_rust_panic_restores_owned_root_state_before_actual_native_retry() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        fixture = Fixture::new(
            &mut env,
            "RR-PRE-ENTRY-PANIC",
            "()",
            "(progn (rust-root-observe) (values 71 77))",
            false
        )
    );
    egcl_rt::rooted!(probe = Probe::new(&fixture, Vec::new()));
    let raw = &mut *probe as *mut Probe;
    let _hooks = Hooks::new(&mut probe);
    ordinary_t2::RECURSIVE_LEAF_OBSERVER
        .with(|slot| slot.set(Some(observe_pre_entry_panic_then_retry)));
    take_entries_for_test();
    super::super::super::mapped_ordinary::take_entries_for_test();
    super::super::super::start_activation_counts_for_test(fixture.target.0);
    fixture.run(&mut env, raw);
    assert!(probe.error.is_none());
    assert_eq!(probe.result, Some(EgclVal::from_fixnum(71)));
    assert_eq!(
        probe.values,
        vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(77)]
    );
    assert_eq!(probe.body_entries, 1);
    // This counter increments before the hook: two boundary attempts are not
    // two machine/body entries. The publication observer above proves the retry.
    assert_eq!(take_entries_for_test(), if enabled() { 2 } else { 0 });
    assert_eq!(
        super::super::super::mapped_ordinary::take_entries_for_test(),
        0
    );
    // OFF prepares twice, but PooledActivation::drop deliberately discards
    // the panicking first state without recycle; only the successful retry
    // contributes a recycle attempt. ON still performs neither operation.
    assert_eq!(
        super::super::super::take_activation_counts_for_test(),
        if enabled() { (0, 0) } else { (2, 1) }
    );
}
