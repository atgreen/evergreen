// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::mapped_ordinary_tests::{install, owner, run_mapped};
use super::*;
use egcl_rt::Collector;

struct Probe {
    owner: usize,
    observed: usize,
    payload: *mut EgclVal,
    before: u64,
    after: u64,
}
thread_local! {
    static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) };
}
fn observe_hot_child() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null()
        || mapped_generated::fast_entries_for_test() == 0
        || !root_publication::published_frame_has_owner_for_test(unsafe { (*probe).owner })
    {
        return;
    }
    unsafe {
        (*probe).observed += 1;
    }
    if unsafe { !(*probe).payload.is_null() && (*probe).after == 0 } {
        unsafe {
            (*probe).before = (*(*probe).payload).to_raw();
        }
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
        unsafe {
            (*probe).after = (*(*probe).payload).to_raw();
        }
    }
    let rows = egcl_rt::debug_stack::capture_current(32);
    assert!(
        rows.iter().any(
            |row| row.origin == egcl_rt::debug_stack::FrameOrigin::Native
                && row
                    .function
                    .as_deref()
                    .is_some_and(|name| name.ends_with("GENERATED-CHILD"))
        ),
        "{rows:?}"
    );
}
struct Observing(Option<fn()>);
impl Drop for Observing {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(self.0));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    }
}
fn observing(probe: &mut Probe) -> Observing {
    PROBE.with(|slot| slot.set(probe));
    Observing(root_publication::PROFILE_OBSERVE.with(|hook| hook.replace(Some(observe_hot_child))))
}

#[test]
fn generated_adapter_runs_actual_native_child_without_per_call_rust_dispatch() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun generated-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _observer_pin =
        super::super::native_transfer_tests::ordinary_t2::RecursiveFixturePins::new(&[
            "GENERATED-OBSERVE",
        ]);
    let (_, child) = install(
        &mut env,
        "GENERATED-CHILD",
        "(x)",
        "(progn (generated-observe) x)",
    );
    super::super::native_transfer_tests::ordinary_t2::expect_entry(&child);
    let (symbol, parent) = install(
        &mut env,
        "GENERATED-PARENT",
        "(n &key (seed (1+ 0)))",
        "(let ((sum 0)) (dotimes (i n sum) (setq sum (+ sum (generated-child i)))))",
    );
    run_mapped(symbol, &parent, &[EgclVal::from_fixnum(1)], &mut env).unwrap();
    mapped_generated::take_fast_entries_for_test();
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    take_published_checked_calls_for_test();
    take_lean_root_entries_for_test();
    let mut probe = Probe {
        owner: Rc::as_ptr(owner(&child)) as usize,
        observed: 0,
        payload: std::ptr::null_mut(),
        before: 0,
        after: 0,
    };
    let _observing = observing(&mut probe);
    assert_eq!(
        run_mapped(symbol, &parent, &[EgclVal::from_fixnum(64)], &mut env).unwrap(),
        EgclVal::from_fixnum(2016)
    );
    let fast = mapped_generated::take_fast_entries_for_test();
    let all = mapped_ordinary::take_entries_for_test();
    assert_eq!(fast, 63, "only initial activation lane fill may use R6");
    assert_eq!(all - fast, 1, "every later call bypasses general Rust run");
    assert_eq!(take_published_checked_calls_for_test(), 1);
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 1);
    assert_eq!(take_lean_root_entries_for_test(), 0);
    assert!(
        probe.observed >= 63,
        "physical callee must execute, not interpreted fallback"
    );
}

#[test]
fn generated_adapter_preserves_heap_actual_after_real_child_collection() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, child) = install(
        &mut env,
        "GENERATED-CHILD",
        "(x)",
        "(progn (logxor 7 7 7) x)",
    );
    let (symbol, parent) = install(
        &mut env,
        "GENERATED-ARG-PARENT",
        "(x &key (seed (1+ 0)))",
        "(progn (generated-child nil) (generated-child x))",
    );
    run_mapped(symbol, &parent, &[NIL], &mut env).unwrap();
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    for _ in 0..32 {
        crate::cli::arena_cons(NIL, NIL);
    }
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(761919), NIL));
    let mut probe = Probe {
        owner: Rc::as_ptr(owner(&child)) as usize,
        observed: 0,
        payload: std::ptr::from_mut(&mut *payload),
        before: 0,
        after: 0,
    };
    mapped_generated::take_fast_entries_for_test();
    let _observing = observing(&mut probe);
    egcl_rt::rooted!(result = run_mapped(symbol, &parent, &[*payload], &mut env).unwrap());
    assert!(mapped_generated::take_fast_entries_for_test() > 0);
    assert!(probe.observed > 0);
    assert_ne!(probe.before, 0);
    assert_ne!(
        probe.before, probe.after,
        "fresh heap argument must really relocate"
    );
    assert_eq!(
        *result, *payload,
        "physical callee must reload the moved argument"
    );
}

#[test]
fn generated_adapter_passes_named_captured_functions_their_actual_defining_environment() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let mut installed = Vec::new();
    for (name, captured) in [("GENERATED-CAPTURE-A", 11), ("GENERATED-CAPTURE-B", 22)] {
        // Evaluate a genuinely nested DEFUN in the root top-level world. A
        // hand-built child passed as a new top-level Env is not a lexical
        // binding form and intentionally does not capture under DEFUN rules.
        crate::cli::read_eval_all_env(
            &format!("(let ((generated-capture-cell {captured})) (defun {name} (x) (logxor 7 7 7) (+ generated-capture-cell x)))"),
            &mut env,
        ).unwrap();
        // Trigger the ordinary lazy bytecode path, preserving the definition's
        // real captured environment and its named compilation identity.
        crate::cli::read_eval_all_env(&format!("({name} 0)"), &mut env).unwrap();
        let symbol = egcl_rt::symbols::intern(name);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        assert!(
            super::super::closure_captured_env(*function).is_some(),
            "real nested definition must own its actual lexical frame"
        );
        // A single warm call need not reach the lazy compilation threshold.
        // Use the production coherent-object compiler rather than assuming
        // bytecode was already cached, or using a private closure owner.
        assert!(
            super::super::lazy_compile_defun(
                symbol,
                name,
                egcl_rt::function::lambda_list(*function),
                egcl_rt::function::body(*function),
                &env,
            ),
            "named captured definition must compile through its real identity"
        );
        let (compilation_owner, body) = super::super::bytecode_for_callable(symbol, *function)
            .expect("real captured global definition");
        assert_eq!(compilation_owner, symbol);
        assert!(
            body.has_env,
            "nested definition must retain its real lexical frame"
        );
        let (_, code) =
            super::super::native_transfer_tests::ordinary_t2::install_body(symbol, body);
        assert!(owner(&code).native_callable_slot.is_some());
        assert!(owner(&code).lean_root_entry().is_some());
        installed.push(code);
    }
    // A same-named caller variable must not replace either defining frame.
    env.define_lexical_local("GENERATED-CAPTURE-CELL", EgclVal::from_fixnum(999));
    let (symbol, parent) = install(
        &mut env,
        "GENERATED-CAPTURE-PARENT",
        "(n &key (seed (1+ 0)))",
        "(let ((answer nil)) (dotimes (i n answer) (setq answer (list (generated-capture-a 3) (generated-capture-b 3)))))",
    );
    run_mapped(symbol, &parent, &[EgclVal::from_fixnum(1)], &mut env).unwrap();
    mapped_generated::take_fast_entries_for_test();
    ordinary_closure_context::HITS.with(|count| count.set(0));
    let deopts: Vec<_> = installed
        .iter()
        .map(|code| owner(code).deopt_count())
        .collect();
    egcl_rt::rooted!(
        result = run_mapped(symbol, &parent, &[EgclVal::from_fixnum(16)], &mut env).unwrap()
    );
    assert!(mapped_generated::take_fast_entries_for_test() >= 30);
    assert!(
        ordinary_closure_context::HITS.with(Cell::get) > 0,
        "physical native closure homes must serve lexical reads"
    );
    for (code, before) in installed.iter().zip(deopts) {
        assert_eq!(owner(code).deopt_count(), before);
    }
    egcl_rt::rooted!(a = cp(*result).0);
    egcl_rt::rooted!(tail = cp(*result).1);
    egcl_rt::rooted!(b = cp(*tail).0);
    egcl_rt::rooted!(end = cp(*tail).1);
    assert_eq!(
        (*a, *b, *end),
        (EgclVal::from_fixnum(14), EgclVal::from_fixnum(25), NIL)
    );
    assert_eq!(installed.len(), 2);
}
