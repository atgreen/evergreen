// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::tests::{force_minor_gc, native_only_slot_for};
use super::*;

struct Probe {
    env: *mut Env,
    markers: &'static [i64],
    osr: Option<OsrRoute>,
    observed: bool,
    moved: usize,
    ordinary_owner: Option<usize>,
    ordinary_entries: usize,
}
thread_local! { static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) }; }

#[derive(Default)]
struct OsrRoute {
    symbol: u32,
    replacement: Option<Rc<NativeCode>>,
    first_owner: usize,
    next_owner: usize,
    parent_owner: usize,
    parent_frame: usize,
    child_frame: usize,
    before: u32,
    steps_at_handoff: i64,
}

fn observe() {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    if probe.observed {
        return;
    }
    if let Some(route) = &mut probe.osr {
        let Some((owner, nested)) = child_capture_for_test() else {
            return;
        };
        let frame = egcl_rt::current_stack().fp();
        if owner == route.parent_owner {
            assert!(!nested, "the caller owns the outer mapped activation");
            route.parent_frame = frame as usize;
            return;
        }
        if owner != route.first_owner && owner != route.next_owner {
            return;
        }
        assert_ne!(route.parent_frame, 0, "observe the mapped parent first");
        assert_eq!(
            unsafe { (*frame).prev_fp } as usize,
            route.parent_frame,
            "OSR must retain the suspended caller"
        );
        if owner == route.first_owner {
            assert!(nested, "T1 child must use the actual mapped call record");
            if route.child_frame == 0 {
                route.child_frame = frame as usize;
            }
            assert_eq!(frame as usize, route.child_frame);
            let marker = unsafe { slot_get(frame.cast_mut(), 0) };
            let steps = crate::cli::cp(marker).1.as_fixnum();
            if steps == 4 {
                if let Some(code) = route.replacement.take() {
                    route.steps_at_handoff = steps;
                    NATIVE_REGISTRY.with(|registry| {
                        registry.borrow_mut().insert(route.symbol, Rc::clone(&code))
                    });
                    publish_native(
                        route.symbol,
                        egcl_rt::symbols::symbol_function(route.symbol),
                        &code,
                    );
                }
            }
            return;
        }
        assert!(
            route.replacement.is_none(),
            "publish T2 from inside live T1"
        );
        assert_ne!(
            route.child_frame, 0,
            "observe the original child activation"
        );
        assert_eq!(
            frame as usize, route.child_frame,
            "T2 must replace the running child rather than start a new invocation"
        );
        assert_eq!(osr_entry_count(route.symbol), route.before + 1);
        assert!(
            !egcl_rt::native_transfer::current_segment().is_null(),
            "collect while the exact worker-built mapped T2 owner is active"
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
    install_with_protocol(name, params, forms, env, false);
}

fn install_with_protocol(name: &str, params: &str, forms: &str, env: &mut Env, mapped: bool) {
    crate::cli::read_eval_all_env(
        &format!("(defun {name} {params} {})", &forms[1..forms.len() - 1]),
        env,
    )
    .unwrap();
    let body = compile(name, params, forms, env);
    let symbol = crate::cli::resolve_sym(name).unwrap().as_symbol_index();
    registry_put(symbol, Arc::clone(&body));
    let installed = if mapped {
        // Keep explicit coverage of the remaining mapped child protocol.
        let code = TransferCode::compile_mapped_for_test(Arc::clone(&body)).unwrap();
        assert!(code.ordinary_entry.is_none() && code.mapped_entry().is_some());
        install_baseline_code(symbol, code)
    } else {
        install_baseline(symbol, Arc::clone(&body))
    }.unwrap_or_else(|| {
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

fn observe_ordinary_producer_entry() {
    let probe = unsafe { &mut *PROBE.with(Cell::get) };
    let context = CAPTURE.with(Cell::get);
    assert!(!context.is_null(), "ordinary producer has a retained context");
    assert_eq!(Some(unsafe { (*context).owner } as usize), probe.ordinary_owner,
        "the measured ordinary entry must belong to the installed producer");
    probe.ordinary_entries += 1;
}

fn run_probe(
    code: &TransferCode,
    env: &mut Env,
    markers: &'static [i64],
    osr: Option<OsrRoute>,
) -> EgclVal {
    run_probe_with_ordinary_owner(code, env, markers, osr, None)
}

fn run_probe_with_ordinary_owner(
    code: &TransferCode,
    env: &mut Env,
    markers: &'static [i64],
    mut osr: Option<OsrRoute>,
    ordinary_owner: Option<usize>,
) -> EgclVal {
    code.run(&[], env).unwrap();
    env.mv.clear();
    force_minor_gc();
    take_nested_entries();
    if let Some(route) = &mut osr {
        route.parent_owner = code as *const TransferCode as usize;
        route.before = osr_entry_count(route.symbol);
    }
    let mut probe = Probe {
        env,
        markers,
        osr,
        observed: false,
        moved: 0,
        ordinary_owner,
        ordinary_entries: 0,
    };
    PROBE.with(|slot| slot.set(&mut probe));
    let previous = OBSERVE.with(|slot| slot.replace(Some(observe)));
    if ordinary_owner.is_some() { take_native_fallback_count(); }
    // Install after warmup so only the measured invocation can satisfy it.
    let result = if ordinary_owner.is_some() {
        with_ordinary_entry_hook_for_test(observe_ordinary_producer_entry,
            || code.run(&[], env))
    } else {
        code.run(&[], env)
    };
    OBSERVE.with(|slot| slot.set(previous));
    PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    let value = result.unwrap();
    if ordinary_owner.is_some() {
        assert_eq!(take_native_fallback_count(), 0, "measured native owners must not fall back");
        assert_eq!(probe.ordinary_entries, 1,
            "the measured producer must execute its ordinary native entry");
    }
    assert!(
        probe.observed,
        "the required native/OSR route must reach the observer"
    );
    assert_eq!(
        probe.moved,
        markers.len(),
        "every private native root must actually move"
    );
    if let Some(route) = probe.osr {
        assert_eq!(route.steps_at_handoff, 4);
        assert_eq!(
            osr_entry_count(route.symbol),
            route.before + 1,
            "the measured invocation must enter mapped OSR exactly once"
        );
        assert_eq!(
            take_nested_entries(),
            1,
            "enter the baseline child exactly once"
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
        install_with_protocol(
            name,
            params,
            &format!("((publication-matrix-collect) {sum})"),
            &mut env,
            true,
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
    // The producer now uses ordinary entry, while the lexical-loop collector
    // retains a mapped child activation. Check both owners explicitly: the
    // nested-entry counter records only the latter physical protocol.
    let mut ordinary_producer = None;
    for (name, ordinary) in [
        ("PUBLICATION-MATRIX-PRIMARY", true),
        ("PUBLICATION-MATRIX-COLLECT", false),
    ] {
        let symbol = crate::cli::resolve_sym(name).unwrap().as_symbol_index();
        NATIVE_REGISTRY.with(|registry| {
            let registry = registry.borrow();
            let installed = registry.get(&symbol).expect("installed matrix callee");
            let NativeCodeStorage::Mapped(owner) = &installed._storage else {
                panic!("{name}: expected native transfer owner");
            };
            if ordinary {
                ordinary_producer = Some(std::ptr::from_ref(owner.as_ref()) as usize);
            }
            assert_eq!(owner.ordinary_entry.is_some(), ordinary, "{name}");
            assert_eq!(owner.mapped_entry().is_some(), !ordinary, "{name}");
        });
    }
    let code = TransferCode::compile_nested_protected(compile(
        "PUBLICATION-MATRIX-RETURNS",
        "()",
        "((let ((x (publication-matrix-primary))) (values nil) (publication-matrix-collect) x))",
        &env,
    ))
    .expect("returned primary must stay in native code");
    let value = run_probe_with_ordinary_owner(&code, &mut env, &[314159], None,
        Some(ordinary_producer.expect("installed ordinary producer owner")));
    assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
    assert_eq!(
        take_nested_entries(),
        1,
        "the collector alone enters a mapped child; the producer uses ordinary entry"
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
            .env(CHILD, "1")
            .env("EGCL_T0_T1_THRESHOLD", "4000000000")
            .env("EGCL_T1_T2_THRESHOLD", "4000000000")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "4000000000")
            .env_remove("EGCL_FORCE_TIER")
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
    let source = "((setf (car marker) (1+ (car marker)))
        (let ((sum 0))
          (dotimes (i 40)
            (setq sum (+ sum i))
            (setf (cdr marker) (1+ (cdr marker))))
          (values marker sum)))";
    crate::cli::read_eval_all_env(
        &format!("(defun publication-matrix-osr (marker) (progn . {source}))"),
        &mut env,
    )
    .unwrap();
    let symbol = crate::cli::resolve_sym("PUBLICATION-MATRIX-OSR")
        .unwrap()
        .as_symbol_index();
    let body = compile("PUBLICATION-MATRIX-OSR", "(marker)", source, &env);
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary mapped T1 child");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("child must install mapped T1");
    };
    assert!(!first.is_t2);
    let input = snapshot_t2_input(symbol, 0).expect("ordinary worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("ordinary worker artifact");
    let next = install_t2_completion(T2Completion {
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary mapped T2 child");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("child must install mapped T2");
    };
    assert!(next.is_t2 && !next.osr_entries.is_empty());
    assert!(Arc::ptr_eq(next.body.as_ref().unwrap(), &body));
    let route = OsrRoute {
        symbol,
        first_owner: Rc::as_ptr(first_mapped) as usize,
        next_owner: Rc::as_ptr(next_mapped) as usize,
        replacement: Some(Rc::clone(&next)),
        ..OsrRoute::default()
    };
    // Build through the real worker pipeline, then defer publication until
    // the measured T1 child has already committed four loop iterations.
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&first)));
    mark_fresh_promotion(symbol);
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &first);
    let code = TransferCode::compile_nested_protected(compile(
        "PUBLICATION-MATRIX-OSR-CALLER",
        "()",
        "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 2) (go again))))
          (let ((x (cons 314159 nil)) (marker (cons 0 0)))
            (values nil)
            (multiple-value-bind (answer sum) (publication-matrix-osr marker)
              (values x answer sum))))",
        &env,
    ))
    .expect("private-root mapped caller must compile");
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let value = run_probe(&code, &mut env, &[314159], Some(route));
    assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 3);
    assert_eq!(env.mv[0], value);
    assert_eq!(
        crate::cli::cp(env.mv[1]),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40)),
        "callee prefix and loop effects must not replay across OSR"
    );
    assert_eq!(env.mv[2], EgclVal::from_fixnum(780));
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before,
        "both frame owners must retire exactly once"
    );
    eprintln!("native-root OSR movement verified");
}
