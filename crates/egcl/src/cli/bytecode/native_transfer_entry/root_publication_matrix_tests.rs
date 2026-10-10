// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::tests::{force_minor_gc, native_only_slot_for};
use super::*;

struct Probe {
    env: *mut Env,
    markers: &'static [i64],
    osr_after: Option<(u32, u32)>,
    observed: bool,
    moved: usize,
}
thread_local! { static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) }; }

fn observe() {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    if probe.observed
        || probe
            .osr_after
            .is_some_and(|(symbol, before)| osr_entry_count(symbol) <= before)
    {
        return;
    }
    if probe.osr_after.is_some() {
        assert!(
            NATIVE_OSR_ACTIVE.with(Cell::get),
            "collect while OSR machine code is still active"
        );
    }
    let published = unsafe { &*ACTIVE.with(Cell::get) };
    let slots: Vec<_> = probe
        .markers
        .iter()
        .map(|marker| {
            native_only_slot_for(published, *marker)
                .expect("every private object needs a sole native home")
        })
        .collect();
    // Slot addresses and diagnostic bits do not root the objects in Rust.
    let before: Vec<_> = slots
        .iter()
        .map(|slot| unsafe { (**slot).to_raw() })
        .collect();
    assert!(
        !unsafe { &*probe.env }
            .mv
            .iter()
            .any(|value| before.contains(&value.to_raw())),
        "the outer environment must not keep a backup copy"
    );
    force_minor_gc();
    probe.moved = slots
        .iter()
        .zip(before)
        .filter(|(slot, bits)| unsafe { (***slot).to_raw() != *bits })
        .count();
    probe.observed = true;
}

fn compile(name: &str, params: &str, forms: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap())
}

fn install(name: &str, params: &str, forms: &str, env: &mut Env) {
    crate::cli::read_eval_all_env(
        &format!("(defun {name} {params} {})", &forms[1..forms.len() - 1]),
        env,
    )
    .unwrap();
    let body = compile(name, params, forms, env);
    let symbol = crate::cli::resolve_sym(name).unwrap().as_symbol_index();
    registry_put(symbol, Arc::clone(&body));
    let installed = install_baseline(symbol, Arc::clone(&body)).unwrap_or_else(|| {
        panic!(
            "{name}: matrix callee must be mapped native code: {:?}",
            body.code
        )
    });
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &installed,
    );
}

fn install_collector(env: &mut Env) {
    install(
        "PUBLICATION-MATRIX-COLLECT",
        "()",
        "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 2) (go again)))) nil)",
        env,
    );
}

fn run_probe(
    code: &TransferCode,
    env: &mut Env,
    markers: &'static [i64],
    osr: Option<u32>,
) -> EgclVal {
    code.run(&[], env).unwrap();
    env.mv.clear();
    force_minor_gc();
    take_nested_entries();
    let mut probe = Probe {
        env,
        markers,
        osr_after: osr.map(|symbol| (symbol, osr_entry_count(symbol))),
        observed: false,
        moved: 0,
    };
    PROBE.with(|slot| slot.set(&mut probe));
    let previous = OBSERVE.with(|slot| slot.replace(Some(observe)));
    let result = code.run(&[], env);
    OBSERVE.with(|slot| slot.set(previous));
    PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    let value = result.unwrap();
    assert!(
        probe.observed,
        "the required native/OSR route must reach the observer"
    );
    assert_eq!(
        probe.moved,
        markers.len(),
        "every private native root must actually move"
    );
    if let Some((symbol, before)) = probe.osr_after {
        assert!(
            osr_entry_count(symbol) > before,
            "the measured invocation must really enter OSR"
        );
    }
    value
}

#[test]
fn native_private_root_survives_short_and_wide_calls() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install_collector(&mut env);
    for (name, params, bindings, call, sum, expected) in [
        (
            "PUBLICATION-MATRIX-SHORT",
            "(a b)",
            "((a (cons 10 nil)) (b (cons 20 nil)))",
            "(publication-matrix-short a b)",
            "(+ (car a) (car b))",
            30,
        ),
        (
            "PUBLICATION-MATRIX-WIDE",
            "(a b c d e f g h)",
            "((a (cons 1 nil)) (b (cons 2 nil)) (c (cons 3 nil)) (d (cons 4 nil))
              (e (cons 5 nil)) (f (cons 6 nil)) (g (cons 7 nil)) (h (cons 8 nil)))",
            "(publication-matrix-wide a b c d e f g h)",
            "(+ (car a) (car b) (car c) (car d) (car e) (car f) (car g) (car h))",
            36,
        ),
    ] {
        install(
            name,
            params,
            &format!("((publication-matrix-collect) {sum})"),
            &mut env,
        );
        let body = compile(
            "PUBLICATION-MATRIX-CALLER",
            "()",
            &format!(
                "((let {bindings} (let ((x (cons 314159 nil))) (values nil)
                (let ((sum {call})) (values x sum)))))"
            ),
            &env,
        );
        let code =
            TransferCode::compile_nested_protected(body).expect("private-root caller must compile");
        let value = run_probe(&code, &mut env, &[314159], None);
        assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
        assert_eq!(
            take_nested_entries(),
            2,
            "measured call enters both callee and collector"
        );
        assert_eq!(
            env.mv,
            vec![value, EgclVal::from_fixnum(expected)],
            "read every heap argument after collection"
        );
    }
}

#[test]
fn native_returned_primary_moves_after_becoming_a_caller_private_root() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install_collector(&mut env);
    install(
        "PUBLICATION-MATRIX-PRIMARY",
        "()",
        "((cons 314159 nil))",
        &mut env,
    );
    let code = TransferCode::compile_nested_protected(compile(
        "PUBLICATION-MATRIX-RETURNS",
        "()",
        "((let ((x (publication-matrix-primary))) (values nil) (publication-matrix-collect) x))",
        &env,
    ))
    .expect("returned primary must stay in native code");
    let value = run_probe(&code, &mut env, &[314159], None);
    assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
    assert_eq!(
        take_nested_entries(),
        2,
        "measured producer and collector must both run natively"
    );
}

thread_local! { static RETURNS_MOVED: Cell<bool> = const { Cell::new(false) }; }

fn observe_saved_returns() {
    if RETURNS_MOVED.with(Cell::get) {
        return;
    }
    let context = CAPTURE.with(Cell::get);
    // Saved multiple values intentionally belong to the rooted continuation.
    // Take writable slot addresses, then end every borrow before collection.
    let slots: [*mut EgclVal; 3] = unsafe {
        let saved = (*(*context).cleanups)
            .last_mut()
            .expect("inside native cleanup");
        let CleanupAction::Normal {
            value,
            values: Some(values),
            ..
        } = &mut saved.continuation.action
        else {
            panic!("normal cleanup must retain multiple return values");
        };
        assert_eq!(values.len(), 2);
        [value, &mut values[0], &mut values[1]]
    };
    let before = slots.map(|slot| unsafe { slot.read().to_raw() });
    assert_eq!(before[0], before[1]);
    assert_eq!(
        before[1], before[2],
        "all three copies describe one returned object"
    );
    force_minor_gc();
    let after = slots.map(|slot| unsafe { slot.read().to_raw() });
    assert!(
        before
            .iter()
            .zip(after)
            .all(|(before, after)| *before != after),
        "the saved primary and both returned-value copies must move"
    );
    assert_eq!(after[0], after[1]);
    assert_eq!(after[1], after[2]);
    RETURNS_MOVED.with(|moved| moved.set(true));
}

#[test]
fn native_multiple_returns_move_during_native_cleanup() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install(
        "PUBLICATION-MATRIX-VALUES",
        "()",
        "((let ((x (cons 314159 nil))) (values x x)))",
        &mut env,
    );
    let code = TransferCode::compile_nested_protected(compile(
        "PUBLICATION-MATRIX-MV-CALLER",
        "()",
        "((unwind-protect (publication-matrix-values)
            (let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 2) (go again))))))",
        &env,
    ))
    .expect("multiple returns and cleanup must compile natively");
    code.run(&[], &mut env).unwrap();
    env.mv.clear();
    force_minor_gc();
    take_nested_entries();
    take_native_fallback_count();
    RETURNS_MOVED.with(|moved| moved.set(false));
    let previous = OBSERVE.with(|slot| slot.replace(Some(observe_saved_returns)));
    let result = code.run(&[], &mut env);
    OBSERVE.with(|slot| slot.set(previous));
    let value = result.unwrap();
    assert!(RETURNS_MOVED.with(Cell::get));
    assert_eq!(
        take_nested_entries(),
        1,
        "producer must return through the native adapter"
    );
    assert_eq!(take_native_fallback_count(), 0, "cleanup must stay native");
    assert_eq!(env.mv, vec![value, value]);
    assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
}

#[test]
fn native_private_root_survives_real_callee_osr() {
    const CHILD: &str = "EGCL_TEST_NATIVE_ROOT_OSR";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::bytecode::native_transfer_entry::root_publication::matrix_tests::native_private_root_survives_real_callee_osr", "--nocapture"])
            .env(CHILD, "1").env("EGCL_OSR_THRESHOLD", "1")
            .env("EGCL_T0_T1_THRESHOLD", "4000000000")
            .env_remove("EGCL_NATIVE_TRANSFER").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("native-root OSR movement verified"),
            "the subprocess must execute the intended probe, not an empty test filter"
        );
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install_collector(&mut env);
    crate::cli::read_eval_all_env(
        "(defun publication-matrix-osr ()
        (let ((n 0)) (tagbody again (setq n (+ n 1)) (publication-matrix-collect)
            (if (< n 3) (go again)))))",
        &mut env,
    )
    .unwrap();
    let symbol = crate::cli::resolve_sym("PUBLICATION-MATRIX-OSR")
        .unwrap()
        .as_symbol_index();
    let code = TransferCode::compile(compile(
        "PUBLICATION-MATRIX-OSR-CALLER",
        "()",
        "((let ((x (cons 314159 nil))) (values nil) (publication-matrix-osr) x))",
        &env,
    ))
    .expect("private-root OSR caller must compile");
    let value = run_probe(&code, &mut env, &[314159], Some(symbol));
    assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
    assert!(
        !NATIVE_OSR_ACTIVE.with(Cell::get),
        "retire the OSR machine-call extent"
    );
    eprintln!("native-root OSR movement verified");
}
