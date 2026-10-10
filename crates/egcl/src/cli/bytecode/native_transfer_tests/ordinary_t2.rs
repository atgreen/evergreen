// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

thread_local! {
    // An address for comparison only: this deliberately is not a GC root.
    static ALLOCATED_ROOT: Cell<u64> = const { Cell::new(0) };
    static EXPECTED_OWNER: Cell<usize> = const { Cell::new(0) };
    static OBSERVED_ENTRY: Cell<bool> = const { Cell::new(false) };
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
    NIL
}

pub(super) fn expect_entry(code: &NativeCode) {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped caller")
    };
    EXPECTED_OWNER.with(|entry| entry.set(Rc::as_ptr(mapped) as usize));
    OBSERVED_ENTRY.with(|observed| observed.set(false));
}

pub(super) fn observed_entry() -> bool {
    OBSERVED_ENTRY.with(Cell::get)
}

fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
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

struct RecursiveFixturePins(Vec<u32>);
impl RecursiveFixturePins {
    fn new(names: &[&str]) -> Self {
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
    use native_transfer_entry::take_recursive_entries;
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
        egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        take_recursive_entries();
        egcl_rt::rooted!(result = run_native(&code, symbol, &[EgclVal::from_fixnum(1)], &mut env));
        assert_ne!(egcl_rt::symbols::symbol_function(symbol), Some(*original));
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
