// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Installed ordinary owners remain walkable while their fiber is unmounted.

use super::ordinary_t2::install as install_ordinary;
use super::*;
use egcl_rt::debug_stack::{FrameOrigin, LogicalFrame};
use std::cell::Cell;
use std::time::{Duration, Instant};

// This fixture specifically needs two simultaneous physical ordinary owners.
fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        install_ordinary(name, params, forms, env)
    })
}

const READY: &str = "*ORDINARY-REGISTER-FIBER-READY*";
const RELEASE: &str = "*ORDINARY-REGISTER-FIBER-RELEASE*";

fn release() {
    egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(RELEASE), T).unwrap();
}

struct Cohort<'a> {
    group: &'a egcl_rt::SchedulerGroup,
    finished: bool,
}

impl Cohort<'_> {
    fn finish(&mut self) -> Vec<EgclVal> {
        release();
        let values = self.group.finish().unwrap();
        self.finished = true;
        values
    }
}

impl Drop for Cohort<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.finish()));
        }
    }
}

fn ordinary_fiber() -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, middle) = install(
        "ORDINARY-REGISTER-FIBER-MIDDLE",
        "(a b c d)",
        "((ordinary-register-fiber-terminal b d a c))",
        &mut env,
    );
    let (symbol, outer) = install(
        "ORDINARY-REGISTER-FIBER-OUTER",
        "(a b c d)",
        "((ordinary-register-fiber-middle d c b a))",
        &mut env,
    );
    for code in [&middle, &outer] {
        let NativeCodeStorage::Mapped(code) = &code._storage else {
            panic!("mapped owner")
        };
        assert!(
            native_transfer_entry::installed_listing(code).contains("Ordinary x86-64 Lisp ABI")
        );
    }
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for _ in 0..2 {
        assert_eq!(
            run_native(&outer, symbol, &warm, &mut env).unwrap(),
            warm[1]
        );
    }
    // Do not keep the foreign owner alive with a test-owned Rc after its cell
    // is invalidated. The suspended segment's directory must retain it.
    drop(middle);
    egcl_rt::rooted!(arguments = Vec::new());
    for index in 0..4 {
        arguments.push(crate::cli::arena_cons(
            EgclVal::from_fixnum(200 + index),
            NIL,
        ));
    }
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    egcl_rt::poll_safepoint();
    assert_eq!(super::super::native_loop_should_exit(), 0);
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    let result = run_native(&outer, symbol, &arguments, &mut env).unwrap();
    assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 0,
        "all generated edges are admitted");
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
        1,
        "only the terminal suspension uses the named adapter; middle enters its generated body"
    );
    assert_eq!(result, arguments[2]);
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 4);
    for (actual, index) in env.mv.iter().zip([2, 0, 3, 1]) {
        assert_eq!(*actual, arguments[index]);
        assert_eq!(
            crate::cli::cp(*actual),
            (EgclVal::from_fixnum(200 + index as i64), NIL)
        );
    }
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    // An already active old owner completed above; the next call must use the
    // new definition published by the other execution while this fiber slept.
    assert_eq!(
        run_native(&outer, symbol, &arguments, &mut env).unwrap(),
        EgclVal::from_fixnum(909)
    );
    EgclVal::from_fixnum(1)
}

fn stopped_rows(fiber: egcl_rt::FiberId, deadline: Instant) -> Vec<LogicalFrame> {
    loop {
        // This API holds mount exclusion. Merely observing Waiting does not
        // authorize reading another execution's native stack or directory.
        if let Some(rows) = egcl_rt::debug_stack::capture_fiber(fiber, 64).unwrap() {
            if rows.iter().any(|row| {
                row.function
                    .as_deref()
                    .is_some_and(|name| name.ends_with("ORDINARY-REGISTER-FIBER-MIDDLE"))
            }) {
                return rows;
            }
        }
        egcl_rt::poll_safepoint();
        assert!(Instant::now() < deadline, "ordinary chain did not suspend");
        std::thread::yield_now();
    }
}

fn native_arguments(rows: &[LogicalFrame]) -> Vec<Vec<EgclVal>> {
    [
        ("ORDINARY-REGISTER-FIBER-MIDDLE", FrameOrigin::Native),
        ("ORDINARY-REGISTER-FIBER-OUTER", FrameOrigin::Managed),
    ]
    .iter()
    .map(|(name, origin)| {
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.function
                    .as_deref()
                    .is_some_and(|actual| actual.ends_with(name))
            })
            .collect();
        assert_eq!(matches.len(), 1, "{rows:?}");
        // The root enriches its existing managed anchor; only the direct
        // child adds a Native row. Both carry exact native argument homes.
        assert_eq!(matches[0].origin, *origin);
        let arguments = matches[0]
            .arguments
            .as_ref()
            .expect("original native arguments");
        assert_eq!(arguments.len(), 4);
        arguments.clone()
    })
    .collect()
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_register_owners_survive_suspension_redefinition_and_external_gc() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    for name in [READY, RELEASE] {
        egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(name), NIL).unwrap();
    }
    crate::cli::read_eval_all_env(
        "(defun ordinary-register-fiber-terminal (a b c d)
           (if (consp a)
               (progn
                 (setq *ordinary-register-fiber-ready* t)
                 (tagbody
                  again
                    (if *ordinary-register-fiber-release* (go done))
                    (egcl::%native-fiber :sleep 0.01d0)
                    (go again)
                  done)
                 (values a b c d))
               d))
         (defun ordinary-register-fiber-middle (a b c d)
           (ordinary-register-fiber-terminal b d a c))",
        &mut startup,
    )
    .unwrap();
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
    let mut cohort = Cohort {
        group: &group,
        finished: false,
    };
    let entry = unsafe { EgclVal::from_function_ptr(ordinary_fiber as *const () as *mut u8) };
    let fiber = egcl_rt::thread::make_fiber(entry).unwrap();
    group.submit(fiber).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let ready = egcl_rt::symbols::intern(READY);
    while egcl_rt::symbols::symbol_value(ready) != Some(T) {
        egcl_rt::poll_safepoint();
        assert!(
            Instant::now() < deadline,
            "terminal did not reach its suspension gate"
        );
        std::thread::yield_now();
    }
    // Redefinition clears the cell while old middle/outer PCs are still live.
    crate::cli::read_eval_all_env(
        "(defun ordinary-register-fiber-middle (a b c d) 909)",
        &mut startup,
    )
    .unwrap();
    // Drop the captured rows before GC: a copied backtrace must not provide
    // extra roots for the suspended execution whose physical homes we test.
    let before: Vec<Vec<u64>> = native_arguments(&stopped_rows(fiber, deadline))
        .iter()
        .map(|args| args.iter().map(|value| value.to_raw()).collect())
        .collect();
    HeapCollector::new().full_gc().unwrap();
    egcl_rt::rooted!(rows = stopped_rows(fiber, deadline));
    let after = native_arguments(&rows);
    assert!(
        after
            .iter()
            .flatten()
            .zip(before.iter().flatten())
            .any(|(value, bits)| value.to_raw() != *bits),
        "external collection must actually relocate an original native argument"
    );
    for (args, indices) in after.iter().zip([[3, 2, 1, 0], [0, 1, 2, 3]]) {
        for (value, index) in args.iter().zip(indices) {
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(200 + index), NIL)
            );
        }
    }
    assert_eq!(cohort.finish(), vec![EgclVal::from_fixnum(1)]);
    HeapCollector::new().full_gc().unwrap();
    for (args, indices) in native_arguments(&rows)
        .iter()
        .zip([[3, 2, 1, 0], [0, 1, 2, 3]])
    {
        for (value, index) in args.iter().zip(indices) {
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(200 + index), NIL)
            );
        }
    }
}
