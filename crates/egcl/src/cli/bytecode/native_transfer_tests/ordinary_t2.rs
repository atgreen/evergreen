// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

#[path = "ordinary_t2_poll_latency.rs"]
mod poll_latency;

thread_local! {
    // An address for comparison only: this deliberately is not a GC root.
    static ALLOCATED_ROOT: Cell<u64> = const { Cell::new(0) };
    static EXPECTED_OWNER: Cell<usize> = const { Cell::new(0) };
    static OBSERVED_ENTRY: Cell<bool> = const { Cell::new(false) };
    pub(in crate::cli::bytecode) static RECURSIVE_LEAF_OBSERVER: Cell<Option<fn()>> = const { Cell::new(None) };
    static LEAF_MANAGED_DEPTH: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(crate) fn ordinary_t2_root_for_test() -> EgclVal {
    ordinary_t2_entry_for_test();
    egcl_rt::rooted!(value = super::super::super::arena_cons(EgclVal::from_fixnum(71), NIL));
    ALLOCATED_ROOT.with(|slot| slot.set(value.to_raw()));
    *value
}

pub(crate) fn ordinary_t2_entry_for_test() -> EgclVal {
    let entry = EXPECTED_OWNER.with(Cell::get);
    assert_ne!(entry, 0);
    assert!(!egcl_rt::native_transfer::current_segment().is_null());
    let capture = native_transfer_entry::child_capture_for_test().or_else(|| {
        native_callable::outer::suspended_callers_for_test()?
            .last()?
            .1
    });
    assert_eq!(
        capture,
        Some((entry, false)),
        "helper must observe this installed mapped caller through the real segment links"
    );
    OBSERVED_ENTRY.with(|observed| observed.set(true));
    if let Some(observe) = RECURSIVE_LEAF_OBSERVER.with(Cell::get) {
        observe();
    }
    LEAF_MANAGED_DEPTH.with(|depth| {
        if depth.get().is_some() {
            depth.set(Some(egcl_rt::current_stack().frame_depth()));
        }
    });
    NIL
}

pub(in crate::cli::bytecode) fn expect_entry(code: &NativeCode) {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped caller")
    };
    EXPECTED_OWNER.with(|entry| entry.set(Rc::as_ptr(mapped) as usize));
    OBSERVED_ENTRY.with(|observed| observed.set(false));
}

pub(super) fn observed_entry() -> bool {
    OBSERVED_ENTRY.with(Cell::get)
}

pub(in crate::cli::bytecode) fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    install_body(symbol, body)
}

fn install_body(symbol: u32, body: Arc<BytecodeFunction>) -> (u32, Rc<NativeCode>) {
    registry_put(symbol, body);
    let input = snapshot_t2_input(symbol, 0).expect("ordinary T2 snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("ordinary T2 artifact");
    let code = install_t2_completion(T2Completion {
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary T2 installation");
    (symbol, code)
}

fn assert_mapped_t2(code: &NativeCode) {
    assert!(
        code.is_t2,
        "ordinary optimized publication must retain its T2 tier"
    );
    assert!(
        matches!(code._storage, NativeCodeStorage::Mapped(_)),
        "ordinary T2 installation must own mapped transfer code"
    );
    assert_eq!(code.transfer_abi_version, MAPPED_TRANSFER_ABI_VERSION);
    assert_ne!(
        code.code_id, 0,
        "ordinary T2 must retain its code-version identity"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_installs_published_calls_without_success_return_poll() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Test hooks execute through ordinary installed source functions; the
    // compiler deliberately does not admit test-only operators themselves.
    super::super::super::read_eval_all_env(
        "(defun ordinary-t2-collector () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let (_, code) = install(
        "ORDINARY-T2-PUBLISHED-CALL",
        "()",
        "((ordinary-t2-collector))",
        &mut env,
    );
    let bytes = unsafe { std::slice::from_raw_parts(code.entry, code.code_len) };
    let pending = c2i_transfer_pending as extern "C" fn() -> u64 as usize as u64;
    let mut decoder =
        iced_x86::Decoder::with_ip(64, bytes, code.entry as u64, iced_x86::DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        assert!(
            !(0..instruction.op_count()).any(|operand| instruction.op_kind(operand)
                == iced_x86::OpKind::Immediate64
                && instruction.immediate64() == pending),
            "ordinary T2 still loads the successful-return transfer-poll helper at {:#x}",
            instruction.ip()
        );
    }
    assert_mapped_t2(&code);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_moves_a_root_owned_only_by_the_native_caller() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun ordinary-t2-allocator () (%ordinary-t2-root-for-test))
         (defun ordinary-t2-collector () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "ORDINARY-T2-CALLER-ROOT",
        "()",
        "((let ((held (ordinary-t2-allocator)))
            (ordinary-t2-collector)
            held))",
        &mut env,
    );
    assert_mapped_t2(&code);
    for _ in 0..2 {
        egcl_rt::collect_t0_minor().unwrap();
        ALLOCATED_ROOT.with(|slot| slot.set(0));
        expect_entry(&code);
        egcl_rt::rooted!(result = run_native(&code, symbol, &[], &mut env).unwrap());
        let before = ALLOCATED_ROOT.with(Cell::get);
        assert_ne!(before, 0, "the allocating helper must run");
        assert_ne!(
            result.to_raw(),
            before,
            "the caller-only root must actually relocate"
        );
        assert_eq!(
            super::super::super::cp(*result),
            (EgclVal::from_fixnum(71), NIL)
        );
        assert!(
            OBSERVED_ENTRY.with(Cell::get),
            "the installed mapped entry must execute"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_preserves_values_and_selected_transfer_effects() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *ordinary-t2-calls* 0 *ordinary-t2-after* 0 *ordinary-t2-cleanups* 0)
         (defun ordinary-t2-finish (fail)
           (%ordinary-t2-entry-for-test)
           (setq *ordinary-t2-calls* (+ *ordinary-t2-calls* 1))
           (unwind-protect
             (if fail (throw :ordinary-t2 (values 71 72)) (values 71 72))
             (setq *ordinary-t2-cleanups* (+ *ordinary-t2-cleanups* 1))))
         (defun ordinary-t2-after () (setq *ordinary-t2-after* (+ *ordinary-t2-after* 1)))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "ORDINARY-T2-VALUES",
        "(fail)",
        "((if fail (progn (ordinary-t2-finish fail) (ordinary-t2-after))
                   (ordinary-t2-finish fail)))",
        &mut env,
    );
    assert_mapped_t2(&code);
    let tag = reader::read_from_string(":ordinary-t2").unwrap().0;
    let token = super::super::super::next_control_token("ORDINARY-T2-OUTER");
    env.catch_stack.push((tag, token.clone()));
    for fail in [NIL, T, NIL, T] {
        let stack = egcl_rt::current_stack();
        let before = (
            stack.fp(),
            stack.sp(),
            NATIVE_DEPTH.with(|depth| depth.get()),
        );
        expect_entry(&code);
        egcl_rt::rooted!(result = run_native(&code, symbol, &[fail], &mut env));
        if fail == T {
            assert!(
                matches!(&*result, Err(EgclError::Internal(selected)) if selected == &token),
                "selected transfer must cross the installed T2 caller: {:?}",
                &*result
            );
            assert_eq!(
                super::super::super::take_control_mv(&token, &mut env),
                EgclVal::from_fixnum(71)
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(71));
        }
        assert_eq!(
            env.mv.as_slice(),
            &[EgclVal::from_fixnum(71), EgclVal::from_fixnum(72)]
        );
        assert!(OBSERVED_ENTRY.with(Cell::get));
        assert_eq!(
            (
                stack.fp(),
                stack.sp(),
                NATIVE_DEPTH.with(|depth| depth.get())
            ),
            before
        );
    }
    env.catch_stack.clear();
    for (variable, expected) in [
        ("*ordinary-t2-calls*", 4),
        ("*ordinary-t2-cleanups*", 4),
        ("*ordinary-t2-after*", 0),
    ] {
        assert_eq!(
            super::super::super::read_eval_all_env(variable, &mut env).unwrap(),
            EgclVal::from_fixnum(expected),
            "effects for {variable}"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_guard_retirement_preserves_effects_and_actual_tier() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *ordinary-t2-guard-ticks* 0)
         (defun ordinary-t2-guard-tick ()
           (setq *ordinary-t2-guard-ticks* (+ *ordinary-t2-guard-ticks* 1)))
         (defun ordinary-t2-guarded (x) (ordinary-t2-guard-tick) (+ x 1))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "ORDINARY-T2-GUARDED",
        "(x)",
        "((ordinary-t2-guard-tick) (+ x 1))",
        &mut env,
    );
    assert_mapped_t2(&code);
    assert!(
        code.has_deopt,
        "ordinary optimizing IR must retain its arithmetic guards"
    );
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    let threshold = deopt_blacklist_threshold();
    for _ in 0..threshold {
        egcl_rt::rooted!(
            result = run_native(
                &code,
                symbol,
                &[EgclVal::from_fixnum((1_i64 << 60) - 1)],
                &mut env
            )
            .unwrap()
        );
        assert_eq!(
            super::super::super::format_val(*result),
            "1152921504606846976"
        );
    }
    assert!(mapped.needs_recompile());
    assert_eq!(
        run_native(&code, symbol, &[EgclVal::from_fixnum(41)], &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
    let replacement =
        NATIVE_REGISTRY.with(|registry| registry.borrow().get(&symbol).unwrap().clone());
    assert!(!Rc::ptr_eq(&replacement, &code));
    assert!(matches!(replacement._storage, NativeCodeStorage::Mapped(_)));
    assert!(
        !replacement.is_t2,
        "local baseline fallback must not masquerade as optimizing worker output"
    );
    assert!(!replacement.has_deopt);
    // A retired activation can still finish, but must not retire its replacement.
    egcl_rt::rooted!(
        result = run_native(
            &code,
            symbol,
            &[EgclVal::from_fixnum((1_i64 << 60) - 1)],
            &mut env
        )
        .unwrap()
    );
    assert_eq!(
        super::super::super::format_val(*result),
        "1152921504606846976"
    );
    assert!(
        NATIVE_REGISTRY
            .with(|registry| Rc::ptr_eq(registry.borrow().get(&symbol).unwrap(), &replacement))
    );
    assert_eq!(
        super::super::super::read_eval_all_env("*ordinary-t2-guard-ticks*", &mut env).unwrap(),
        EgclVal::from_fixnum(i64::from(threshold) + 2),
        "pre-guard effects must not replay"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_missing_mapped_artifact_preserves_mapped_t1() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(
        forms =
            reader::read_from_string("((length (make-string n)))")
                .unwrap()
                .0
    );
    let symbol = egcl_rt::symbols::intern("POLICY-T2-STRING");
    let body =
        Arc::new(compile_function("POLICY-T2-STRING", *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, body);
    let baseline = try_promote_to_t1(symbol).expect("mapped T1 string call");
    assert!(matches!(baseline._storage, NativeCodeStorage::Mapped(_)));
    let input = snapshot_t2_input(symbol, 0).expect("ordinary T2 snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let mut artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("worker artifact");
    // Exercise the installer admission boundary independently of which source
    // shapes the mapped emitter currently supports. The worker does not emit
    // an unusable checked fallback; remove its mapped preparation as well.
    assert!(artifact.checked.is_none(), "Linux workers emit only mapped code");
    assert!(artifact.mapped.take().is_some(), "worker prepares mapped IR");
    assert!(
        install_t2_completion(T2Completion {
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .is_none(),
        "missing mapped preparation must not replace the mapped T1 owner"
    );
    let retained = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&symbol).cloned())
        .unwrap();
    assert!(
        Rc::ptr_eq(&baseline, &retained),
        "keep the exact running mapped T1 owner"
    );
    assert_eq!(
        run_native(&retained, symbol, &[EgclVal::from_fixnum(10)], &mut env).unwrap(),
        EgclVal::from_fixnum(10)
    );
}

fn install_mv_body(
    name: &str,
    body: Arc<BytecodeFunction>,
    optimized: bool,
) -> (u32, Rc<NativeCode>) {
    let symbol = egcl_rt::symbols::intern(name);
    registry_put(symbol, body);
    let code = if optimized {
        let input = snapshot_t2_input(symbol, 0).expect("MV-bind worker snapshot");
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input
            .with_gc_stable_mutator(compile_t2_artifact)
            .expect("MV-bind worker artifact");
        install_t2_completion(T2Completion {
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .expect("MV-bind mapped T2 installation")
    } else {
        try_promote_to_t1(symbol).expect("MV-bind mapped T1 installation")
    };
    assert_eq!(code.is_t2, optimized);
    assert!(matches!(code._storage, NativeCodeStorage::Mapped(_)),
        "MV binding must execute through the mapped ABI, not checked fallback");
    assert_eq!(code.transfer_abi_version, MAPPED_TRANSFER_ABI_VERSION);
    (symbol, code)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mv_bind_maps_both_tiers_and_preserves_bound_heap_values() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun b7iai-producer (mode)
           (if (= mode 0) (values)
             (if (= mode 1) (values 71)
               (if (= mode 2)
                 (let ((a (%ordinary-t2-root-for-test))
                       (b (%ordinary-t2-root-for-test)))
                   (setf (car b) 72)
                   (values a b))
                 (values 71 72 73 74)))))
         (defun b7iai-collect ()
           (%ordinary-t2-entry-for-test)
           (%force-minor-gc-for-test))",
        &mut env,
    ).unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(mode)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((multiple-value-bind (a b c) (b7iai-producer mode)
            (values)
            (b7iai-collect)
            (values a b c)))"
    ).unwrap().0);
    for optimized in [false, true] {
        let name = if optimized { "B7IAI-MV-T2" } else { "B7IAI-MV-T1" };
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        assert!(body.code.iter().any(|op| matches!(op,
            Instr::TakeValuesToLocals { nvars: 3, .. })),
            "standard MULTIPLE-VALUE-BIND must exercise the local-transfer opcode");
        let (symbol, code) = install_mv_body(name, body, optimized);
        for mode in [0, 1, 2, 3] {
            egcl_rt::collect_t0_minor().unwrap();
            ALLOCATED_ROOT.with(|slot| slot.set(0));
            expect_entry(&code);
            let stack = egcl_rt::current_stack();
            let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
            egcl_rt::rooted!(primary = run_native(
                &code, symbol, &[EgclVal::from_fixnum(mode)], &mut env
            ).unwrap());
            assert!(observed_entry(), "the installed mapped caller must actually execute");
            assert!(env.mv_active);
            assert_eq!(env.mv.len(), 3);
            assert_eq!(*primary, env.mv[0]);
            match mode {
                0 => assert_eq!(env.mv, vec![NIL, NIL, NIL]),
                1 => assert_eq!(env.mv, vec![EgclVal::from_fixnum(71), NIL, NIL]),
                2 => {
                    assert_eq!(super::super::super::cp(env.mv[0]),
                        (EgclVal::from_fixnum(71), NIL));
                    assert_eq!(super::super::super::cp(env.mv[1]),
                        (EgclVal::from_fixnum(72), NIL));
                    assert_eq!(env.mv[2], NIL, "missing secondary values require NIL padding");
                    let secondary_before = ALLOCATED_ROOT.with(Cell::get);
                    assert_ne!(secondary_before, 0, "the producer allocated the secondary value");
                    assert_ne!(env.mv[1].to_raw(), secondary_before,
                        "the bound secondary must really move after the MV buffer is overwritten");
                }
                _ => assert_eq!(env.mv, vec![EgclVal::from_fixnum(71),
                    EgclVal::from_fixnum(72), EgclVal::from_fixnum(73)]),
            }
            assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mv_zero_binding_preserves_the_existing_tuple() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun b7iai-zero-producer () (%ordinary-t2-entry-for-test) (values 71 72))",
        &mut env,
    ).unwrap();
    egcl_rt::rooted!(forms = reader::read_from_string("((b7iai-zero-producer))").unwrap().0);
    for optimized in [false, true] {
        let name = if optimized { "B7IAI-ZERO-T2" } else { "B7IAI-ZERO-T1" };
        let mut body = compile_function(name, NIL, *forms, &env, false, false).unwrap();
        // Exercise the zero-destination bytecode contract directly: ordinary
        // Lisp body evaluation can legitimately clear MV before observing it.
        body.code = vec![
            Instr::CallNamed {
                sym: egcl_rt::symbols::intern("B7IAI-ZERO-PRODUCER"),
                nargs: 0,
            },
            Instr::TakeValuesToLocals { nvars: 0, slot_base: 0 },
            Instr::Const(0),
            Instr::Return,
        ];
        body.constants = vec![EgclVal::from_fixnum(71)];
        body.n_locals = 0;
        body.max_stack = 1;
        let (symbol, code) = install_mv_body(name, Arc::new(body), optimized);
        expect_entry(&code);
        let primary = run_native(&code, symbol, &[], &mut env).unwrap();
        assert!(observed_entry());
        assert_eq!(primary, EgclVal::from_fixnum(71));
        assert!(env.mv_active, "zero binding must not invoke the ClearMv operation");
        assert_eq!(env.mv, vec![EgclVal::from_fixnum(71), EgclVal::from_fixnum(72)]);
    }
}

pub(in crate::cli::bytecode) struct RecursiveFixturePins(Vec<u32>);
impl RecursiveFixturePins {
    pub(in crate::cli::bytecode) fn new(names: &[&str]) -> Self {
        Self(
            names
                .iter()
                .map(|name| {
                    let symbol = egcl_rt::symbols::intern(name);
                    profile_pin(symbol);
                    symbol
                })
                .collect(),
        )
    }
}
impl Drop for RecursiveFixturePins {
    fn drop(&mut self) {
        for &symbol in &self.0 {
            profile_unpin(symbol);
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_calls_move_roots_and_recover_deepest_guard_once() {
    use native_transfer_entry::take_recursive_entries;
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for fail in [false, true] {
        let name = if fail {
            "ORDINARY-T2-RECURSIVE-ERROR"
        } else {
            "ORDINARY-T2-RECURSIVE-ROOT"
        };
        let finish = if fail { "(car 1)" } else { "x" };
        let form = format!(
            "(progn (ordinary-t2-recursive-tick)
               (if (= n 0)
                   (progn (+ v 1) (ordinary-t2-recursive-finish x))
                   (progn ({name} (- n 1) v x) (ordinary-t2-recursive-car x))))"
        );
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *ordinary-t2-recursive-ticks* 0)
                 (defun ordinary-t2-recursive-tick ()
                   (setq *ordinary-t2-recursive-ticks* (+ *ordinary-t2-recursive-ticks* 1)))
                 (defun ordinary-t2-recursive-finish (x) (%force-minor-gc-for-test) {finish})
                 (defun ordinary-t2-recursive-car (x) (values (car x) x))
                 (defun {name} (n v x) {form})"
            ),
            &mut env,
        )
        .unwrap();
        let _pins = RecursiveFixturePins::new(&[
            "ORDINARY-T2-RECURSIVE-TICK",
            "ORDINARY-T2-RECURSIVE-FINISH",
            "ORDINARY-T2-RECURSIVE-CAR",
        ]);
        let (symbol, code) = install(name, "(n v x)", &format!("({form})"), &mut env);
        assert_mapped_t2(&code);
        assert!(code.has_deopt, "worker T2 retains arithmetic guards");
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            unreachable!()
        };
        assert!(mapped.native_entry_kind_slot.is_some(),
            "recursive guard recovery must exercise actual native child frames");
        #[cfg(not(egcl_no_disassembly))]
        assert!(
            native_transfer_entry::installed_listing(mapped)
                .contains("native recursive entry veneer"),
            "ordinary worker T2 must emit the precise direct recursive path"
        );
        let plus = egcl_rt::symbols::intern("+");
        let body = code.body.as_ref().unwrap();
        let add_bcp = body
            .code
            .iter()
            .position(|op| matches!(op, Instr::CallNamed { sym, nargs: 2 } if *sym == plus))
            .expect("deepest overflow operation");
        assert!(
            code.t2_metadata
                .as_ref()
                .unwrap()
                .speculations
                .iter()
                .any(|(bcp, kind)| { *bcp == add_bcp as u32 && matches!(kind, SpecType::Fixnum) }),
            "real worker must speculate the operation whose guard will fail"
        );

        // Drain old parser/compiler allocations before creating the nursery
        // argument whose relocation must be observed through suspended callers.
        egcl_rt::collect_t0_minor().unwrap();
        egcl_rt::rooted!(
            args = vec![
                EgclVal::from_fixnum(8),
                EgclVal::from_fixnum((1_i64 << 60) - 1),
                super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL),
            ]
        );
        let address = args[2].to_raw();
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        take_recursive_entries();
        let deopts = mapped.deopt_count();
        egcl_rt::rooted!(result = run_native(&code, symbol, &args, &mut env));
        assert_eq!(
            take_recursive_entries(),
            8,
            "all eight selfcalls use native recursive entries"
        );
        assert_eq!(
            mapped.deopt_count() - deopts,
            1,
            "only the deepest guard resumes T0"
        );
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before
        );
        assert_ne!(
            args[2].to_raw(),
            address,
            "the suspended caller argument must actually move"
        );
        assert_eq!(
            super::super::super::cp(args[2]),
            (EgclVal::from_fixnum(42), NIL)
        );
        if fail {
            let Err(EgclError::Signalled(details)) = &*result else {
                panic!("expected selected type error, got {:?}", &*result);
            };
            assert!(super::super::super::condition_matches_handler(
                &env,
                details.condition,
                "TYPE-ERROR"
            ));
            assert_eq!(
                details
                    .backtrace
                    .iter()
                    .filter(|frame| {
                        frame
                            .function
                            .as_deref()
                            .is_some_and(|function| function.ends_with(name))
                    })
                    .count(),
                9,
                "one logical frame per recursive activation"
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(42));
            assert_eq!(env.mv.as_slice(), &[EgclVal::from_fixnum(42), args[2]]);
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*ordinary-t2-recursive-ticks*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(9),
            "entry effects must not replay during deepest guard recovery"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_calls_preserve_redefinition_and_unbinding() {
    use native_transfer_entry::{take_recursive_entries, take_recursive_helper_calls};
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for unbind in [false, true] {
        let name = if unbind {
            "ORDINARY-T2-RECURSIVE-UNBOUND"
        } else {
            "ORDINARY-T2-RECURSIVE-REPLACED"
        };
        let change = if unbind {
            format!("(fmakunbound '{name})")
        } else {
            format!("(defun {name} (n) (values 99 100))")
        };
        let form = format!(
            "(if (= n 0) 0
               (progn (ordinary-t2-recursive-change) ({name} (- n 1))))"
        );
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *ordinary-t2-recursive-changes* 0)
                 (defun ordinary-t2-recursive-change ()
                   (setq *ordinary-t2-recursive-changes* (+ *ordinary-t2-recursive-changes* 1))
                   {change})
                 (defun {name} (n) {form})"
            ),
            &mut env,
        )
        .unwrap();
        let _pins = RecursiveFixturePins::new(&["ORDINARY-T2-RECURSIVE-CHANGE"]);
        // Preserve the named selfcall: compiling under NAME would eliminate
        // this tail call before the native redefinition guard could see it.
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let symbol = egcl_rt::symbols::intern(name);
        let mut body = compile_function("<lambda>", *params, *forms, &env, false, false).unwrap();
        body.name = name.into();
        assert!(body.code.iter().any(|op| {
            matches!(op, Instr::CallNamed { sym, .. } if *sym == symbol)
        }), "fixture must retain the named recursive call");
        let (symbol, code) = install_body(symbol, Arc::new(body));
        assert_mapped_t2(&code);
        let NativeCodeStorage::Mapped(mapped) = &code._storage else { unreachable!() };
        assert!(mapped.native_entry_kind_slot.is_some(),
            "generation invalidation must exercise the physical native-child entry mode");
        egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        take_recursive_entries();
        take_recursive_helper_calls();
        egcl_rt::rooted!(result = run_native(&code, symbol, &[EgclVal::from_fixnum(1)], &mut env));
        assert_ne!(egcl_rt::symbols::symbol_function(symbol), Some(*original));
        assert_eq!(take_recursive_helper_calls(), (0, 0),
            "replacement/unbinding must use the native entry's cold fallback");
        assert_eq!(
            take_recursive_entries(),
            0,
            "never enter the replaced definition"
        );
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before
        );
        if unbind {
            assert!(
                matches!(&*result, Err(EgclError::UndefinedFunction(_)))
                    || matches!(&*result, Err(EgclError::Signalled(details))
                    if super::super::super::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")),
                "{:?}",
                &*result
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(99));
            assert_eq!(
                env.mv.as_slice(),
                &[EgclVal::from_fixnum(99), EgclVal::from_fixnum(100)]
            );
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*ordinary-t2-recursive-changes*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(1),
            "the definition-changing effect executes once"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_idle_polls_skip_publication_but_t1_retains_heat() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    assert!(
        !profiling_disabled(),
        "this regression must exercise real T1 heat"
    );
    assert!(
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.get().is_none()),
        "an observer would force publication and invalidate the idle-path measurement"
    );
    let name = "ORDINARY-IDLE-POLL-LOOP";
    // More than 64 nonconstant arithmetic operations make the compiler emit
    // straight-line polls as well as loop-header polls, which retain heat in T1.
    let additions = "(setq sum (+ sum i)) ".repeat(80);
    let source = format!("((let ((sum 0)) (dotimes (i n) {additions}) (values sum n)))");
    super::super::super::read_eval_all_env(
        &format!("(defun {name} (n) (progn . {source}))"),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let baseline = try_promote_to_t1(symbol).expect("ordinary mapped T1 loop");
    assert!(!baseline.is_t2);
    assert!(matches!(baseline._storage, NativeCodeStorage::Mapped(_)));
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let heat_before = egcl_rt::function::back_edge_count(*function);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    native_transfer_entry::reset_poll_paths_for_test();
    assert_eq!(
        run_native(&baseline, symbol, &[EgclVal::from_fixnum(8)], &mut env).unwrap(),
        EgclVal::from_fixnum(2240)
    );
    let (fast, slow) = native_transfer_entry::take_poll_paths_for_test();
    assert_eq!(
        fast, 0,
        "T1 must retain publication, heat accounting, and OSR checks"
    );
    assert!(slow >= 8, "every loop visit retains a real poll: {slow}");
    assert!(
        egcl_rt::function::back_edge_count(*function) > heat_before,
        "baseline loop heat must still reach the installed function"
    );
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(2240), EgclVal::from_fixnum(8)]
    );

    // This helper runs snapshot -> compile_t2_artifact -> install_t2_completion.
    // Never turn a baseline owner into T2 by changing its is_t2 flag.
    let (_, optimized) = install_body(symbol, body);
    assert_mapped_t2(&optimized);
    assert!(native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.get().is_none()));
    native_transfer_entry::reset_poll_paths_for_test();
    native_transfer_entry::native_poll::take_generated_skips();
    assert_eq!(
        run_native(&optimized, symbol, &[EgclVal::from_fixnum(8)], &mut env).unwrap(),
        EgclVal::from_fixnum(2240)
    );
    let (fast, slow) = native_transfer_entry::take_poll_paths_for_test();
    let skipped = native_transfer_entry::native_poll::take_generated_skips();
    assert!(skipped >= 8,
        "T2 must execute generated idle skips in the installed loop: {skipped}");
    assert_eq!(fast, 0, "generated idle polls must not enter the Rust preflight");
    assert!(slow < skipped, "idle loop must predominantly bypass publication");
    assert_eq!(
        env.mv,
        vec![EgclVal::from_fixnum(2240), EgclVal::from_fixnum(8)]
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_generated_poll_cold_edge_relocates_a_native_only_root() {
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun generated-poll-allocator () (%ordinary-t2-root-for-test))", &mut env,
    ).unwrap();
    let (symbol, code) = install("GENERATED-POLL-ROOT", "(n)",
        "((let ((held (generated-poll-allocator)))
            (values nil)
            (let ((sum 0)) (dotimes (i n) (setq sum (+ sum i))))
            held))", &mut env);
    assert_mapped_t2(&code);
    expect_entry(&code);
    egcl_rt::collect_t0_minor().unwrap();
    thread_local! { static MOVED: Cell<bool> = const { Cell::new(false) }; }
    MOVED.with(|moved| moved.set(false));
    fn observe() {
        if MOVED.with(Cell::get) { return; }
        let Some(root) = native_transfer_entry::root_publication::native_poll_root_for_test(71) else { return };
        let before = unsafe { root.read() }.to_raw();
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(unsafe { root.read() }.to_raw(), before,
            "the generated cold edge must publish and relocate the native-only root");
        MOVED.with(|moved| moved.set(true));
    }
    struct ClearObserver;
    impl Drop for ClearObserver {
        fn drop(&mut self) {
            native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        }
    }
    let _observer = ClearObserver;
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(Some(observe)));
    native_transfer_entry::reset_poll_paths_for_test();
    let result = run_native(&code, symbol, &[EgclVal::from_fixnum(8)], &mut env).unwrap();
    assert!(MOVED.with(Cell::get), "a real mapped T2 poll must expose the root");
    assert_eq!(super::super::super::cp(result), (EgclVal::from_fixnum(71), NIL));
    assert!(native_transfer_entry::take_poll_paths_for_test().1 > 0);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_diagnostics_report_sparse_speculation_and_actual_publication() {
    const CHILD: &str = "EGCL_TEST_T2_DIAGNOSTICS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // EGCL_T2_LOG is process-wide OnceLock state. A separate process also
        // keeps unrelated compiler messages out of this publication oracle.
        let path =
            std::env::temp_dir().join(format!("egcl-t2-diagnostics-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::bytecode::native_transfer_tests::ordinary_t2::ordinary_t2_diagnostics_report_sparse_speculation_and_actual_publication",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_T2_LOG", &path)
            .env("EGCL_T2_NO_QUEUE", "1")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_T2_DISCARD")
            .output()
            .unwrap();
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "diagnostics child failed: {:?}\n{}\n{}\n{log}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "T2-DIAGNOSTIC-SPARSE";
    super::super::super::read_eval_all_env("(defun t2-diagnostic-sparse (x) (+ x 1))", &mut env)
        .unwrap();
    let symbol = egcl_rt::symbols::intern(name);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
    let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    for _ in 0..2 {
        assert_eq!(
            run(
                Arc::clone(&body),
                &[EgclVal::from_fixnum(41)],
                *function,
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(42)
        );
    }
    let make_completion = || {
        let input = snapshot_t2_input(symbol, 0).expect("real profiled worker snapshot");
        assert_eq!(input.type_profiles.len(), 1);
        let (&bcp, profile) = input.type_profiles.iter().next().unwrap();
        assert_eq!(
            (
                profile.fixnum,
                profile.single_float,
                profile.bignum,
                profile.other
            ),
            (2, 0, 0, 0)
        );
        assert_eq!(profile.dominant(), None, "below the dominant-sample floor");
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input
            .with_gc_stable_mutator(compile_t2_artifact)
            .expect("real worker output");
        assert_eq!(artifact.speculations, vec![(bcp, SpecType::Fixnum)]);
        (
            bcp,
            T2Completion {
                sym: symbol,
                generation,
                artifact: Some(artifact),
                input,
            },
        )
    };
    let log_path = std::env::var_os("EGCL_T2_LOG").unwrap();
    let read_log = || std::fs::read_to_string(&log_path).unwrap();
    let success = format!("[T2] {name}: optimized worker result installed with mapped T2 ABI");
    let (bcp, mut unpublished) = make_completion();
    assert!(read_log().contains(&format!(
        "[T2] {name}: site bcp={bcp} T0 samples fix=2 float=0 bignum=0 other=0 profile_choice=Some(Fixnum)"
    )), "sparse monomorphic samples must report the actual worker choice");
    assert!(
        !read_log().contains(&success),
        "worker compilation is not publication"
    );
    // Mapped allocation/emission can succeed without entering either registry
    // or publishing a tier. This boundary must not announce an installed tier.
    let emitted = native_transfer_entry::install_ordinary_t2(
        symbol,
        &body,
        unpublished.artifact.as_mut().unwrap(),
    )
    .expect("unpublished mapped emission");
    assert_mapped_t2(&emitted);
    assert!(NATIVE_REGISTRY.with(|registry| !registry.borrow().contains_key(&symbol)));
    assert!(
        !read_log().contains(&success),
        "emission must not claim publication"
    );
    drop(emitted);
    drop(unpublished);

    let (_, completion) = make_completion();
    let (_, stale) = make_completion();
    let installed = install_t2_completion(completion).expect("current completion publishes");
    assert_mapped_t2(&installed);
    assert!(
        NATIVE_REGISTRY
            .with(|registry| Rc::ptr_eq(registry.borrow().get(&symbol).unwrap(), &installed))
    );
    assert_eq!(egcl_rt::function::tier(*function), 2);
    let expected = format!(
        "{success}; bytes={}, entry={:p}, activation_slots={}, osr_entries={}",
        installed.code_len,
        installed.entry,
        installed.num_slots,
        installed.osr_entries.len()
    );
    let published_log = read_log();
    assert_eq!(
        published_log
            .lines()
            .filter(|line| line.starts_with(&success))
            .collect::<Vec<_>>(),
        vec![expected.as_str()],
        "one success record must describe the actual published owner"
    );
    assert_eq!(
        run_native(&installed, symbol, &[EgclVal::from_fixnum(42)], &mut env).unwrap(),
        EgclVal::from_fixnum(43)
    );

    // Re-publishing the bytecode advances its real generation. The already
    // compiled completion must be rejected before announcing any new install.
    registry_put(symbol, Arc::clone(&body));
    assert!(install_t2_completion(stale).is_none());
    assert_eq!(
        read_log(),
        published_log,
        "stale completion must not announce success"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_fib_has_no_per_call_frame_helpers() {
    use native_transfer_entry::{
        take_native_fallback_count, take_recursive_entries, take_recursive_helper_calls,
    };
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "ORDINARY-T2-NATIVE-FRAME-FIB";
    let form = "(if (< n 2) n
        (+ (ordinary-t2-native-frame-fib (- n 1))
           (ordinary-t2-native-frame-fib (- n 2))))";
    super::super::super::read_eval_all_env(
        &format!("(defun {name} (n) {form})"),
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(name, "(n)", &format!("({form})"), &mut env);
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(mapped.native_entry_kind_slot.is_some(),
        "invocation accounting must exercise generated native-child admission");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(
        egcl_rt::function::tier(*function),
        2,
        "exercise an ordinarily published worker T2 definition"
    );
    // Warm the already installed code and its dispatch cells before measuring.
    assert_eq!(
        run_native(&code, symbol, &[EgclVal::from_fixnum(3)], &mut env).unwrap(),
        EgclVal::from_fixnum(2)
    );
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let deopts = mapped.deopt_count();
    // This internal run_native entry bypasses apply_function's outer invocation
    // bump. Only the 464 recursive calls belong to the measured delta.
    let before_count = egcl_rt::function::invoke_count(*function);
    take_recursive_entries();
    take_native_fallback_count();
    take_recursive_helper_calls();
    assert_eq!(
        run_native(&code, symbol, &[EgclVal::from_fixnum(12)], &mut env).unwrap(),
        EgclVal::from_fixnum(144)
    );
    let entries = take_recursive_entries();
    let helpers = take_recursive_helper_calls();
    assert_eq!(entries, 464, "fib(12) must execute all 464 native self calls");
    assert_eq!(egcl_rt::function::invoke_count(*function), before_count,
        "ordinary T2 children must not update invocation heat");
    assert_eq!(take_native_fallback_count(), 0);
    assert_eq!(mapped.deopt_count(), deopts, "measure successful native returns");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(
        helpers,
        (0, 0),
        "ordinary T2 recursion must not prepare or retire frames through Rust helpers"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_single_return_releases_secondary_roots_without_clear_calls() {
    use egcl_rt::gc::TraceHostRoots;
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install("MV-ABI-SINGLE", "(x)", "(x)", &mut env);
    assert_mapped_t2(&code);

    // These objects have no Lisp owner other than the pending value tuple.
    // Inspect the actual root visitor, not just the active flag: an inactive
    // Vec with a nonzero length still retains every secondary object.
    {
        egcl_rt::rooted!(primary = super::super::super::arena_cons(EgclVal::from_fixnum(81), NIL));
        env.set_mv(vec![*primary]);
    }
    let secondary_bits = {
        egcl_rt::rooted!(secondary = super::super::super::arena_cons(EgclVal::from_fixnum(82), NIL));
        env.mv.push(*secondary);
        secondary.to_raw()
    };
    let mut roots_before = 0;
    env.trace_host_roots(&mut |slot| {
        if unsafe { (*slot).to_raw() } == secondary_bits { roots_before += 1; }
    });
    assert!(roots_before > 0, "the stale-secondary probe must start as a real root");
    assert_eq!(run_native(&code, symbol, &[EgclVal::from_fixnum(42)], &mut env).unwrap(),
        EgclVal::from_fixnum(42));
    assert!(!env.mv_active);
    assert!(env.mv.is_empty());
    let mut roots_after = 0;
    env.trace_host_roots(&mut |slot| {
        if unsafe { (*slot).to_raw() } == secondary_bits { roots_after += 1; }
    });
    assert_eq!(roots_after, 0, "single return must release stale secondary roots");

    let bytes = unsafe { std::slice::from_raw_parts(code.entry, code.code_len) };
    let clear = c2i_t2_mv as *const () as u64;
    let mut decoder = iced_x86::Decoder::with_ip(
        64, bytes, code.entry as u64, iced_x86::DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        assert!(!(0..instruction.op_count()).any(|operand|
            instruction.op_kind(operand) == iced_x86::OpKind::Immediate64
                && instruction.immediate64() == clear),
            "proven single-valued body still calls the MV clear helper");
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_return_distinguishes_zero_one_and_many_after_stale_values() {
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for (name, form, expected) in [
        ("MV-ABI-ZERO", "((values))", vec![]),
        ("MV-ABI-ONE", "((values x))", vec![42]),
        ("MV-ABI-MANY", "((values x 43 44))", vec![42, 43, 44]),
        ("MV-ABI-AFTER-MANY", "((progn (values 10 11) x))", vec![42]),
    ] {
        let (symbol, code) = install(name, "(x)", form, &mut env);
        assert_mapped_t2(&code);
        for stale in [vec![], vec![70], vec![70, 71, 72, 73]] {
            env.set_mv(stale.into_iter().map(EgclVal::from_fixnum).collect());
            let result = run_native(&code, symbol, &[EgclVal::from_fixnum(42)], &mut env).unwrap();
            let actual = if env.mv_active { env.mv.clone() } else { vec![result] };
            assert_eq!(actual, expected.iter().copied().map(EgclVal::from_fixnum).collect::<Vec<_>>(),
                "{name}: zero, one and many must not inherit the previous tuple");
        }
    }
}

#[test]
fn native_return_snapshot_canonicalizes_one_and_borrows_many() {
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for count in [0, 1, 2, 19] {
        env.set_mv((0..count).map(EgclVal::from_fixnum).collect());
        let descriptor = env.native_return_values();
        assert_eq!(descriptor.count, count as usize);
        if count <= 1 {
            assert!(descriptor.payload.is_null());
        } else {
            assert_eq!(descriptor.payload, env.mv.as_ptr());
            assert_eq!(unsafe { std::slice::from_raw_parts(descriptor.payload, descriptor.count) },
                env.mv.as_slice());
        }
        assert_eq!(env.mv_active, count != 1);
        if count == 1 { assert!(env.mv.is_empty(), "single tuple must not retain duplicate roots"); }
    }
    env.clear_mv();
    let descriptor = env.native_return_values();
    assert_eq!(descriptor.count, 1);
    assert!(descriptor.payload.is_null());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_frames_do_not_grow_the_managed_stack() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "ORDINARY-T2-NATIVE-FRAME-DEPTH";
    let form = "(if (= n 0) (ordinary-t2-native-frame-leaf)
        (+ (ordinary-t2-native-frame-depth (- n 1)) 1))";
    super::super::super::read_eval_all_env(
        &format!(
            "(defun ordinary-t2-native-frame-leaf () (%ordinary-t2-entry-for-test) 0)
             (defun {name} (n) {form})"
        ),
        &mut env,
    )
    .unwrap();
    // Keep observer dispatch identical in the depth-zero and recursive calls.
    // Its constant adapter/managed frames must not masquerade as recursion.
    let _pins = RecursiveFixturePins::new(&["ORDINARY-T2-NATIVE-FRAME-LEAF"]);
    let (symbol, code) = install(name, "(n)", &format!("({form})"), &mut env);
    assert_mapped_t2(&code);
    expect_entry(&code);
    run_native(&code, symbol, &[EgclVal::from_fixnum(0)], &mut env).unwrap();
    struct ClearDepthProbe;
    impl Drop for ClearDepthProbe {
        fn drop(&mut self) {
            LEAF_MANAGED_DEPTH.with(|depth| depth.set(None));
        }
    }
    let _probe = ClearDepthProbe;
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let mut depths = Vec::new();
    for n in [0, 6] {
        expect_entry(&code);
        LEAF_MANAGED_DEPTH.with(|depth| depth.set(Some(0)));
        native_transfer_entry::take_recursive_entries();
        assert_eq!(
            run_native(&code, symbol, &[EgclVal::from_fixnum(n)], &mut env).unwrap(),
            EgclVal::from_fixnum(n),
            "each recursive caller must resume its non-tail addition"
        );
        assert!(observed_entry(), "observe the installed native caller at the leaf");
        assert_eq!(native_transfer_entry::take_recursive_entries(), n as usize);
        let depth = LEAF_MANAGED_DEPTH.with(Cell::get).expect("active leaf probe");
        assert!(depth > 0, "the callback must actually observe its managed boundary");
        depths.push(depth);
        assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    }
    assert_eq!(
        depths[1], depths[0],
        "six actual native self calls must add no managed frames above the constant leaf boundary"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_native_recursive_overflow_resumes_every_frame_without_replay() {
    use native_transfer_entry::{take_recursive_entries, take_recursive_helper_calls};
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "ORDINARY-T2-NATIVE-FRAME-SUM";
    let form = "(progn (ordinary-t2-native-frame-sum-tick)
        (if (= n 0) (+ x 1)
            (+ (ordinary-t2-native-frame-sum (- n 1) x) 1)))";
    super::super::super::read_eval_all_env(
        &format!(
            "(setq *ordinary-t2-native-frame-sum-ticks* 0)
             (defun ordinary-t2-native-frame-sum-tick ()
               (setq *ordinary-t2-native-frame-sum-ticks*
                     (+ *ordinary-t2-native-frame-sum-ticks* 1)))
             (defun {name} (n x) {form})"
        ),
        &mut env,
    )
    .unwrap();
    let _pins = RecursiveFixturePins::new(&["ORDINARY-T2-NATIVE-FRAME-SUM-TICK"]);
    let (symbol, code) = install(name, "(n x)", &format!("({form})"), &mut env);
    assert_mapped_t2(&code);
    assert!(code.has_deopt, "worker T2 must retain arithmetic guards");
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(
        mapped.native_entry_kind_slot.is_some(),
        "guard recovery must exercise actual native child frames"
    );
    for overflow in [false, true] {
        super::super::super::read_eval_all_env(
            "(setq *ordinary-t2-native-frame-sum-ticks* 0)",
            &mut env,
        )
        .unwrap();
        let x = if overflow { (1_i64 << 60) - 1 } else { 10 };
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        let deopts = mapped.deopt_count();
        take_recursive_entries();
        take_recursive_helper_calls();
        egcl_rt::rooted!(
            result = run_native(
                &code,
                symbol,
                &[EgclVal::from_fixnum(8), EgclVal::from_fixnum(x)],
                &mut env,
            )
            .unwrap()
        );
        assert_eq!(
            take_recursive_entries(),
            8,
            "all eight selfcalls must enter native child frames"
        );
        assert_eq!(
            take_recursive_helper_calls(),
            (0, 0),
            "neither entry nor guard recovery may prepare/retire managed child frames"
        );
        assert_eq!(
            mapped.deopt_count() - deopts,
            if overflow { 9 } else { 0 },
            "each overflowing activation must resume its own continuation"
        );
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before
        );
        assert_eq!(
            super::super::super::format_val(*result),
            (x + 9).to_string(),
            "native returns and all nine guard resumptions must agree exactly"
        );
        assert_eq!(
            super::super::super::read_eval_all_env(
                "*ordinary-t2-native-frame-sum-ticks*",
                &mut env,
            )
            .unwrap(),
            EgclVal::from_fixnum(9),
            "guard recovery must not replay any activation's prefix"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_native_recursion_falls_back_at_depth_cap_and_restores_anchor() {
    use native_transfer_entry::{take_recursive_entries, take_recursive_helper_calls};
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "ORDINARY-T2-NATIVE-CAP";
    let form = "(if (= n 0) (ordinary-t2-native-cap-leaf)
        (+ (ordinary-t2-native-cap (- n 1)) 1))";
    super::super::super::read_eval_all_env(&format!(
        "(defun ordinary-t2-native-cap-leaf () (%ordinary-t2-entry-for-test) 0)
         (defun {name} (n) {form})"), &mut env).unwrap();
    let _pins = RecursiveFixturePins::new(&["ORDINARY-T2-NATIVE-CAP-LEAF"]);
    let (symbol, code) = install(name, "(n)", &format!("({form})"), &mut env);
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else { unreachable!() };
    assert!(mapped.native_entry_kind_slot.is_some(),
        "the cap must exercise physical native children, not managed recursive entries");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let cap = native_depth_cap();
    assert!(cap >= 4, "fixture needs one fallback below two admitted native children");
    let recursion_depth = usize::try_from(cap - 1).unwrap().min(8);
    thread_local! {
        static CAP_LEAF_DEPTH: Cell<Option<(usize, u32)>> = const { Cell::new(None) };
    }
    fn observe_leaf() {
        CAP_LEAF_DEPTH.with(|slot| slot.set(Some((
            egcl_rt::current_stack().frame_depth(), NATIVE_DEPTH.with(Cell::get),
        ))));
    }
    struct RestoreObserver(Option<fn()>);
    impl Drop for RestoreObserver {
        fn drop(&mut self) {
            RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
            CAP_LEAF_DEPTH.with(|slot| slot.set(None));
        }
    }
    struct RestoreDepth(u32);
    impl Drop for RestoreDepth {
        fn drop(&mut self) { NATIVE_DEPTH.with(|depth| depth.set(self.0)); }
    }
    let _observer = RestoreObserver(RECURSIVE_LEAF_OBSERVER.with(|hook|
        hook.replace(Some(observe_leaf))));
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let deopts = mapped.deopt_count();
    let mut managed_depths = Vec::new();
    for (initial_depth, expected_children) in [(0, recursion_depth), (cap - 3, 2)] {
        let _depth = RestoreDepth(NATIVE_DEPTH.with(|depth| depth.replace(initial_depth)));
        CAP_LEAF_DEPTH.with(|slot| slot.set(None));
        expect_entry(&code);
        take_recursive_entries();
        take_recursive_helper_calls();
        let before_count = egcl_rt::function::invoke_count(*function);
        assert_eq!(run_native(&code, symbol, &[EgclVal::from_fixnum(recursion_depth as i64)], &mut env).unwrap(),
            EgclVal::from_fixnum(recursion_depth as i64), "every non-tail addition must execute exactly once");
        assert!(observed_entry(), "the terminal leaf must actually execute");
        // Depth fallback does not uninstall T2 or resume tiering heat.
        assert_eq!(egcl_rt::function::invoke_count(*function), before_count,
            "installed T2 remains uncounted through depth fallback");
        assert_eq!(take_recursive_entries(), expected_children,
            "the outer anchor consumes one depth slot before child admission");
        assert_eq!(take_recursive_helper_calls(), (0, 0),
            "native children never use managed-frame prepare/finish helpers");
        assert_eq!(mapped.deopt_count(), deopts, "cap fallback is ordinary dispatch, not deopt");
        let (managed, native) = CAP_LEAF_DEPTH.with(Cell::get).expect("observed terminal leaf");
        assert_eq!(native, initial_depth + 1 + expected_children as u32,
            "the leaf sees exactly the admitted native chain");
        managed_depths.push(managed);
        assert_eq!((stack.fp(), stack.sp()), (before.0, before.1));
        assert_eq!(NATIVE_DEPTH.with(Cell::get), initial_depth,
            "return retires all native children and their managed anchor exactly once");
    }
    assert!(managed_depths[1] > managed_depths[0],
        "after the cap, remaining recursion must execute on the flat managed T0 stack");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_shared_closures_pause_heat_until_native_code_is_removed() {
    let _lock = super::super::super::heap_test_lock()
        .lock().unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(forms = super::super::super::vec_to_list(&[EgclVal::from_fixnum(73)]));
    let body = Arc::new(compile_function("COUNTER-SHARED-BODY", NIL, *forms, &env, false, false).unwrap());
    egcl_rt::rooted!(first = make_bytecode_closure(&body, None));
    egcl_rt::rooted!(second = make_bytecode_closure(&body, None));
    let a = egcl_rt::function::name(*first).as_symbol_index();
    let b = egcl_rt::function::name(*second).as_symbol_index();
    assert_eq!(closure_compilation_owner(a, 1).0, a);
    let (_, code) = install_body(a, Arc::clone(&body));
    assert_mapped_t2(&code);
    assert_eq!(egcl_rt::function::tier(*second), 0, "sibling metadata is not installed-code authority");
    let aggregate = closure_compilation_owner(b, 0).1;
    let anonymous = dispatch_invoke_count(b, None);
    let before = egcl_rt::function::invoke_count(*second);
    for _ in 0..3 {
        record_callable_invocation(*second);
        assert_eq!(call_registered(b, &[], *second, &mut env).unwrap().unwrap(), EgclVal::from_fixnum(73));
    }
    assert_eq!(egcl_rt::function::invoke_count(*second), before);
    assert_eq!(closure_compilation_owner(b, 0).1, aggregate);
    assert_eq!(dispatch_invoke_count(b, None), anonymous);
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().remove(&a));
    record_callable_invocation(*second);
    assert_eq!(egcl_rt::function::invoke_count(*second), before + 1,
        "lower-tier heat resumes after optimized code is removed");
}
