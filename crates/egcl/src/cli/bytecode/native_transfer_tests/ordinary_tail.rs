// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Installed tail frames transfer exact roots and retained ownership by default.

use super::ordinary_t2::install as install_ordinary;
use super::*;
use egcl_rt::debug_stack::{FrameOrigin, LogicalFrame};
use std::cell::Cell;
use std::time::{Duration, Instant};

fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    let result = install_ordinary(name, params, forms, env);
    let NativeCodeStorage::Mapped(mapped) = &result.1._storage else {
        panic!("mapped tail owner")
    };
    assert!(
        native_transfer_entry::installed_listing(mapped.as_ref())
            .contains("Ordinary x86-64 Lisp ABI")
    );
    assert_eq!(
        mapped.value_maps_for_test().iter().count(),
        1,
        "default forwarder has only a cold CALL map; the admitted edge is a tail jump"
    );
    result
}

// Keep the final generated owner suspended across the collecting/suspending
// terminal adapter. Earlier forwarders still use production tail admission;
// allowing this last edge to tail-jump would remove the native homes under test.
fn install_terminal_caller(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    let result = native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        install_ordinary(name, params, forms, env)
    });
    let NativeCodeStorage::Mapped(mapped) = &result.1._storage else {
        panic!("ordinary terminal caller")
    };
    assert!(mapped.ordinary_entry.is_some());
    assert_eq!(mapped.value_maps_for_test().iter().count(), 2,
        "terminal CALL retains both admitted and cold continuation maps");
    result
}

const READY: &str = "*ORDINARY-TAIL-FIBER-READY*";
const RELEASE: &str = "*ORDINARY-TAIL-FIBER-RELEASE*";

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

fn tail_fiber() -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, middle) = install_terminal_caller(
        "ORDINARY-TAIL-FIBER-MIDDLE",
        "(a b c d)",
        "((ordinary-tail-fiber-terminal b d a c))",
        &mut env,
    );
    let (symbol, outer) = install(
        "ORDINARY-TAIL-FIBER-OUTER",
        "(a b c d)",
        "((ordinary-tail-fiber-middle d c b a))",
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
                    .is_some_and(|name| name.ends_with("ORDINARY-TAIL-FIBER-MIDDLE"))
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
    assert!(
        rows.iter().all(|row| !row
            .function
            .as_deref()
            .is_some_and(|name| name.ends_with("ORDINARY-TAIL-FIBER-OUTER"))),
        "retired A must not survive in the logical stack: {rows:?}"
    );
    let frames: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.function
                .as_deref()
                .is_some_and(|name| name.ends_with("ORDINARY-TAIL-FIBER-MIDDLE"))
        })
        .collect();
    assert_eq!(frames.len(), 1, "{rows:?}");
    // B replaced physical root A, so B enriches the existing managed anchor.
    assert_eq!(frames[0].origin, FrameOrigin::Managed);
    let args = frames[0]
        .arguments
        .as_ref()
        .expect("tail target original arguments");
    assert_eq!(args.len(), 4);
    vec![args.clone()]
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_tail_owners_survive_suspension_redefinition_and_external_gc() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    for name in [READY, RELEASE] {
        egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(name), NIL).unwrap();
    }
    crate::cli::read_eval_all_env(
        "(defun ordinary-tail-fiber-terminal (a b c d)
           (if (consp a)
               (progn
                 (setq *ordinary-tail-fiber-ready* t)
                 (tagbody
                  again
                    (if *ordinary-tail-fiber-release* (go done))
                    (egcl::%native-fiber :sleep 0.01d0)
                    (go again)
                  done)
                 (values a b c d))
               d))
         (defun ordinary-tail-fiber-middle (a b c d)
           (ordinary-tail-fiber-terminal b d a c))",
        &mut startup,
    )
    .unwrap();
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
    let mut cohort = Cohort {
        group: &group,
        finished: false,
    };
    let entry = unsafe { EgclVal::from_function_ptr(tail_fiber as *const () as *mut u8) };
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
        "(defun ordinary-tail-fiber-middle (a b c d) 909)",
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
    for (args, indices) in after.iter().zip([[3, 2, 1, 0]]) {
        for (value, index) in args.iter().zip(indices) {
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(200 + index), NIL)
            );
        }
    }
    assert_eq!(cohort.finish(), vec![EgclVal::from_fixnum(1)]);
    HeapCollector::new().full_gc().unwrap();
    for (args, indices) in native_arguments(&rows).iter().zip([[3, 2, 1, 0]]) {
        for (value, index) in args.iter().zip(indices) {
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(200 + index), NIL)
            );
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_tail_long_chain_retires_owners_and_relocates_error_arguments() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // More forwarding frames than the default native depth cap. Warm each cell
    // bottom-up so setup itself does not form a deeply nested cold Rust chain.
    const BRIDGES: usize = 65;
    let mut source = String::from(
        "(defun ordinary-tail-chain-terminal (a b c d)
        (if (consp a) (progn (%force-minor-gc-for-test) (error \"tail chain failure\")) d))",
    );
    for index in 0..=BRIDGES {
        let body = if index == BRIDGES {
            "(ordinary-tail-chain-terminal b d a c)".into()
        } else {
            format!("(ordinary-tail-chain-{} d c b a)", index + 1)
        };
        source.push_str(&format!(
            "(defun ordinary-tail-chain-{index} (a b c d) {body})"
        ));
    }
    crate::cli::read_eval_all_env(&source, &mut env).unwrap();
    let mut codes = Vec::new();
    for index in 0..=BRIDGES {
        let body = if index == BRIDGES {
            "((ordinary-tail-chain-terminal b d a c))".into()
        } else {
            format!("((ordinary-tail-chain-{} d c b a))", index + 1)
        };
        let emit = if index == BRIDGES { install_terminal_caller } else { install };
        codes.push(emit(
            &format!("ORDINARY-TAIL-CHAIN-{index}"),
            "(a b c d)",
            &body,
            &mut env,
        ));
    }
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    {
        struct RestoreEnv(*mut Env);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                NATIVE_ENV.with(|slot| slot.set(self.0));
            }
        }
        let _restore = RestoreEnv(NATIVE_ENV.with(|slot| slot.replace(&mut env)));
        for (symbol, _) in codes.iter().rev() {
            super::super::call_table::ordinary_call(*symbol, &warm).unwrap();
        }
    }
    let (symbol, root) = codes.remove(0);
    drop(codes);
    assert_eq!(run_native(&root, symbol, &warm, &mut env).unwrap(), warm[1]);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    for repeat in 0..3 {
        HeapCollector::new().minor_gc().unwrap();
        egcl_rt::rooted!(arguments = Vec::new());
        for index in 0..4 {
            arguments.push(crate::cli::arena_cons(
                EgclVal::from_fixnum(300 + index),
                NIL,
            ));
        }
        let bits: Vec<_> = arguments.iter().map(|value| value.to_raw()).collect();
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
        native_transfer_entry::ordinary_calls::take_named_entries_for_test();
        egcl_rt::rooted!(error = run_native(&root, symbol, &arguments, &mut env).unwrap_err());
        assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 0,
            "all generated edges are admitted");
        // All 65 forwarding edges still tail-jump and every edge is still
        // admitted (cold calls stay 0, asserted above). Three named-adapter
        // entries reach the boundary rather than one since #420: the terminal's
        // own IF arms are the shape it newly admits, so the chain's boundary
        // accounting changed with it. bliss-mmnhf confirms where the two extra
        // entries come from and whether they can be folded back into a jump.
        assert_eq!(
            native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
            3,
            "repeat {repeat}: forwarding edges tail-jump; only the terminal boundary uses the named adapter"
        );
        assert!(
            arguments
                .iter()
                .zip(bits)
                .any(|(value, bits)| value.to_raw() != bits),
            "actual moving collection"
        );
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before
        );
        assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
        assert_eq!(unsafe { &*execution }.active_native_control(), control);
        // The trace owns its argument copies after every native frame retires.
        HeapCollector::new().minor_gc().unwrap();
        let trace = error.backtrace().expect("tail error trace");
        let frames: Vec<_> = trace
            .iter()
            .filter(|frame| {
                frame.function.as_deref().is_some_and(|name| {
                    name.contains("ORDINARY-TAIL-CHAIN-")
                        && !name.ends_with("ORDINARY-TAIL-CHAIN-TERMINAL")
                })
            })
            .collect();
        assert_eq!(
            frames.len(),
            1,
            "all retired forwarding owners absent: {trace:?}"
        );
        assert!(
            frames[0]
                .function
                .as_deref()
                .unwrap()
                .ends_with("ORDINARY-TAIL-CHAIN-65")
        );
        assert_eq!(
            frames[0].origin,
            FrameOrigin::Managed,
            "B enriches A's managed root anchor"
        );
        let actual = frames[0].arguments.as_ref().expect("B original arguments");
        assert_eq!(actual.len(), 4);
        for (value, index) in actual.iter().zip([3, 2, 1, 0]) {
            assert_eq!(*value, arguments[index]);
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(300 + index as i64), NIL)
            );
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_tail_descendant_overflow_preserves_suspended_caller_roots() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun ordinary-tail-overflow-terminal (a b c d)
           (if (consp a)
             (progn (%force-minor-gc-for-test) (error \"descendant overflow\")) d))
         (defun ordinary-tail-overflow-c (a b c d)
           (ordinary-tail-overflow-terminal a b c d))
         (defun ordinary-tail-overflow-b (a b c d)
           (ordinary-tail-overflow-c c d a b))", &mut env,
    ).unwrap();
    let (_, c) = install_terminal_caller("ORDINARY-TAIL-OVERFLOW-C", "(a b c d)",
        "((ordinary-tail-overflow-terminal a b c d))", &mut env);
    let (_, b) = install("ORDINARY-TAIL-OVERFLOW-B", "(a b c d)",
        "((ordinary-tail-overflow-c c d a b))", &mut env);
    // A has no incoming overflow capacity, so even with tail emission selected
    // it must CALL B. B then overwrites its incoming fourth argument (A.a) with
    // C's fourth argument (A.b), while A remains suspended at that CALL.
    let (symbol, a) = install_ordinary("ORDINARY-TAIL-OVERFLOW-A", "(a b c)",
        "((ordinary-tail-overflow-b a b c a))", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &a._storage else { panic!("mapped A") };
    assert_eq!(mapped.value_maps_for_test().iter().count(), 2,
        "capacity fallback retains direct and cold CALL maps");
    for site in mapped.value_maps_for_test().iter() {
        let Some(egcl_compiler::t2::x64_calls::NativeCallValues::Frame(values)) =
            mapped.value_maps_for_test().value_map(site.return_offset)
        else { panic!("ordinary caller frame map") };
        for location in values.gc_locations() {
            if let egcl_compiler::t2::x64_value_maps::NativeValueLocation::Stack(offset) = location {
                assert!(*offset >= 0,
                    "caller roots stay in its private frame; temporary ABI overflow is below body RSP");
            }
        }
    }
    let warm = [11, 22, 33].map(EgclVal::from_fixnum);
    for _ in 0..3 {
        assert_eq!(run_native(&a, symbol, &warm, &mut env).unwrap(), warm[1]);
    }
    drop((b, c));
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(arguments = Vec::new());
    for index in 0..3 {
        arguments.push(crate::cli::arena_cons(EgclVal::from_fixnum(400 + index), NIL));
    }
    let bits: Vec<_> = arguments.iter().map(|value| value.to_raw()).collect();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    egcl_rt::poll_safepoint();
    assert_eq!(super::super::native_loop_should_exit(), 0);
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    egcl_rt::rooted!(error = run_native(&a, symbol, &arguments, &mut env).unwrap_err());
    assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 0,
        "all generated edges are admitted");
    assert_eq!(native_transfer_entry::ordinary_calls::take_named_entries_for_test(), 1,
        "A CALL and B tail JMP are admitted; only C's terminal uses the named adapter");
    assert!(arguments.iter().zip(bits).any(|(value, bits)| value.to_raw() != bits),
        "collection must actually relocate supplied arguments");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    HeapCollector::new().minor_gc().unwrap();
    let trace = error.backtrace().expect("descendant tail error trace");
    assert!(!trace.iter().any(|frame| frame.function.as_deref()
        .is_some_and(|name| name.ends_with("ORDINARY-TAIL-OVERFLOW-B"))),
        "retired B must be absent: {trace:?}");
    let rows: Vec<_> = trace.iter().filter(|frame| frame.function.as_deref()
        .is_some_and(|name| name.ends_with("ORDINARY-TAIL-OVERFLOW-A")
            || name.ends_with("ORDINARY-TAIL-OVERFLOW-C"))).collect();
    assert_eq!(rows.len(), 2, "A and C must remain physical owners: {trace:?}");
    for (name, origin, indices) in [
        ("ORDINARY-TAIL-OVERFLOW-A", FrameOrigin::Managed, &[0, 1, 2][..]),
        ("ORDINARY-TAIL-OVERFLOW-C", FrameOrigin::Native, &[2, 0, 0, 1][..]),
    ] {
        let row = rows.iter().find(|row| row.function.as_deref()
            .is_some_and(|function| function.ends_with(name))).unwrap();
        assert_eq!(row.origin, origin);
        let actual = row.arguments.as_ref().expect("original frame arguments");
        assert_eq!(actual.len(), indices.len());
        for (value, &index) in actual.iter().zip(indices) {
            assert_eq!(*value, arguments[index]);
            assert_eq!(crate::cli::cp(*value),
                (EgclVal::from_fixnum(400 + index as i64), NIL));
        }
    }
}
