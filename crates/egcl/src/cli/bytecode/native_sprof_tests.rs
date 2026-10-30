// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use native_transfer_entry::root_publication;
use std::cell::Cell;

thread_local! {
    static EXPECTED_OWNER: Cell<usize> = const { Cell::new(0) };
    static EXPECTED_CALLER: Cell<usize> = const { Cell::new(0) };
    static OWNER_VISITS: Cell<usize> = const { Cell::new(0) };
}

struct Recording;
impl Recording {
    fn begin() -> Self {
        crate::cli::sprof::begin_for_test();
        OWNER_VISITS.with(|count| count.set(0));
        native_transfer_entry::reset_poll_paths_for_test();
        Self
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        crate::cli::sprof::stop();
        root_publication::OBSERVE.with(|hook| hook.set(None));
        root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(None));
    }
}

fn sample_leaf() {
    let expected = EXPECTED_OWNER.with(Cell::get);
    if root_publication::published_frame_has_owner_for_test(expected) {
        let caller = EXPECTED_CALLER.with(Cell::get);
        if caller != 0 {
            assert!(
                root_publication::published_frame_has_owner_for_test(caller),
                "native caller and callee must share the same physical segment walk"
            );
        }
        OWNER_VISITS.with(|count| count.set(count.get() + 1));
        crate::cli::sprof::request_for_test();
        // This is an actual helper publication with its physical native
        // callers present. No synthetic profiler frame supplies their names.
        crate::cli::sprof::maybe_sample();
    }
}

fn compile_body(name: &str, body: &str, env: &mut Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = crate::cli::read_from_string_in_env("(n)", env).unwrap().0);
    egcl_rt::rooted!(forms = crate::cli::read_from_string_in_env(body, env).unwrap().0);
    Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap())
}

// Match the established publication fixture: a conditional backward GO,
// with the result outside the loop, rather than a nested early-return loop.
const LOOP_BODY: &str = "((let ((left 0))
  (tagbody again (setq left (+ left 1))
    (if (< left n) (go again)))
  42))";
const ITERATIONS: i64 = 4096;
// Three-argument LOGXOR retains a real helper call in T2. Its result is used,
// so the fixture cannot drop that publication as dead arithmetic.
const PUBLICATION_BODY: &str = "((if (= (logxor n n n) n) 42 0))";

#[test]
fn publications_sample_actual_ordinary_and_mapped_native_caller_and_callee() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    for mapped in [false, true] {
        let _mapped = mapped.then(native_transfer_entry::MappedOnlyForTest::new);
        crate::cli::read_eval_all_env(
            "(defun sprof-native-leaf (n) n)
             (defun sprof-native-caller (n) (+ (sprof-native-leaf n) 1))",
            &mut env,
        )
        .unwrap();
        let (_, leaf) = native_transfer_tests::ordinary_t2::install(
            "SPROF-NATIVE-LEAF",
            "(n)",
            PUBLICATION_BODY,
            &mut env,
        );
        let (caller_symbol, caller) = native_transfer_tests::ordinary_t2::install(
            "SPROF-NATIVE-CALLER",
            "(n)",
            "((+ (sprof-native-leaf n) 1))",
            &mut env,
        );
        let NativeCodeStorage::Mapped(leaf_code) = &leaf._storage else {
            panic!("native leaf")
        };
        assert_eq!(leaf_code.ordinary_entry.is_none(), mapped);
        EXPECTED_OWNER.with(|owner| owner.set(Rc::as_ptr(leaf_code) as usize));
        let body = compile_body("SPROF-T0-CALLER", "((sprof-native-caller n))", &mut env);
        egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
        let symbol = egcl_rt::symbols::intern("SPROF-T0-CALLER");
        let NativeCodeStorage::Mapped(caller_code) = &caller._storage else {
            panic!("native caller")
        };
        assert_eq!(caller_code.ordinary_entry.is_none(), mapped);
        EXPECTED_CALLER.with(|owner| owner.set(Rc::as_ptr(caller_code) as usize));
        // Resolve cold linkage before the measured call. Both retained owners
        // must then be observed in the same physical native segment.
        assert_eq!(
            run_native(
                &caller,
                caller_symbol,
                &[EgclVal::from_fixnum(ITERATIONS)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(43)
        );
        let deopts = (caller_code.deopt_count(), leaf_code.deopt_count());
        let _recording = Recording::begin();
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(Some(sample_leaf)));
        let (_, publications_before) = native_transfer_entry::take_poll_paths_for_test();
        assert_eq!(publications_before, 0);
        assert_eq!(
            run_with_sym(
                body,
                &[EgclVal::from_fixnum(ITERATIONS)],
                NIL,
                symbol,
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(43)
        );
        assert!(
            OWNER_VISITS.with(Cell::get) > 0,
            "actual helper-publication counter with both physical native owners"
        );
        assert_eq!((caller_code.deopt_count(), leaf_code.deopt_count()), deopts);
        assert_eq!(
            crate::cli::sprof::shadow_depth(),
            0,
            "caller guards must retire"
        );
        let folded = crate::cli::sprof::fold();
        assert!(
            folded.lines().any(|line| {
                line.contains(
                    "SPROF-T0-CALLER [T0];SPROF-NATIVE-CALLER [T2];SPROF-NATIVE-LEAF [T2]",
                )
            }),
            "mapped={mapped}: {folded}"
        );
        assert_eq!(caller.is_t2, leaf.is_t2);
    }
}

fn release_forcing_observer() {
    // Remove the observer after the first requested poll. Later samples must
    // reach Rust through generated execution-slot checks, not an observer.
    root_publication::OBSERVE.with(|hook| hook.set(None));
}
fn post_after_poll() {
    if root_publication::OBSERVE.with(|hook| hook.get().is_some()) {
        return;
    }
    root_publication::OBSERVE_AFTER.with(|hook| hook.set(None));
    crate::cli::sprof::request_for_test();
}

#[test]
fn pending_samples_reach_call_free_native_polls_and_keep_actual_tier() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    for t2 in [false, true] {
        let body = compile_body("SPROF-CALL-FREE", LOOP_BODY, &mut env);
        egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
        let t1;
        let t2_owner;
        let code = if t2 {
            let (_, installed) = native_transfer_tests::ordinary_t2::install(
                "SPROF-CALL-FREE",
                "(n)",
                LOOP_BODY,
                &mut env,
            );
            t2_owner = installed;
            let NativeCodeStorage::Mapped(code) = &t2_owner._storage else {
                panic!("native T2")
            };
            code.as_ref()
        } else {
            t1 = native_transfer_entry::TransferCode::compile(body)
                .expect("production T1 compilation");
            &t1
        };
        let deopts = code.deopt_count();
        let _recording = Recording::begin();
        EXPECTED_OWNER.with(|owner| owner.set(std::ptr::from_ref(code) as usize));
        EXPECTED_CALLER.with(|owner| owner.set(0));
        if t2 {
            root_publication::OBSERVE.with(|hook| hook.set(Some(release_forcing_observer)));
            root_publication::OBSERVE_AFTER.with(|hook| hook.set(Some(post_after_poll)));
        } else {
            root_publication::OBSERVE.with(|hook| hook.set(Some(sample_leaf)));
        }
        assert_eq!(
            native_transfer_entry::with_ordinary_entry_hook_for_test(
                crate::cli::sprof::request_for_test,
                || code.run(&[EgclVal::from_fixnum(ITERATIONS)], &mut env),
            )
            .unwrap(),
            EgclVal::from_fixnum(42)
        );
        assert_eq!(code.deopt_count(), deopts, "loop must stay in native code");
        assert!(
            root_publication::OBSERVE_AFTER.with(|hook| hook.get().is_none()),
            "real native poll must consume the fixture hook"
        );
        if !t2 {
            assert!(OWNER_VISITS.with(Cell::get) > 0, "actual native T1 owner");
        }
        assert!(
            native_transfer_entry::take_poll_paths_for_test().1 >= 2,
            "later poll must honor the profiler request without a forcing observer"
        );
        let tier = if t2 { "T2" } else { "T1" };
        let folded = crate::cli::sprof::fold();
        assert!(
            folded.contains(&format!("SPROF-CALL-FREE [{tier}]")),
            "{folded}"
        );
        assert!(
            !folded.contains("SPROF-CALL-FREE [T0]"),
            "native sampling cannot be interpreter fallback"
        );
    }
}

#[test]
fn bytecode_backedges_sample_without_any_lisp_call() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    let body = compile_body(
        "SPROF-CALL-FREE-T0",
        "((let ((again t)) (tagbody top (if again (progn (setq again nil) (go top)))) 42))",
        &mut env,
    );
    assert!(
        !body
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::CallNamed { .. } | Instr::EvalHost(_)))
    );
    assert!(
        body.code
            .iter()
            .any(|instruction| matches!(instruction, Instr::Go { .. }))
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    let symbol = egcl_rt::symbols::intern("SPROF-CALL-FREE-T0");
    let _recording = Recording::begin();
    crate::cli::sprof::request_for_test();
    assert_eq!(
        run_with_sym(body, &[NIL], NIL, symbol, &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert!(crate::cli::sprof::fold().contains("SPROF-CALL-FREE-T0 [T0]"));
    assert_eq!(
        crate::cli::sprof::shadow_depth(),
        0,
        "call guards must be balanced"
    );
}
