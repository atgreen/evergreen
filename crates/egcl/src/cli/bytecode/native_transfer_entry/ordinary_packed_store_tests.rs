// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Install the real source continuation with the existing packed specialization.
//! Feedback is supplied explicitly so a cold test cannot pass with tagged IR.
use super::super::native_transfer_tests::ordinary_t2;
use super::*;
use egcl_compiler::t2::ir::Opcode;

thread_local! {
    static ARGUMENTS: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static EFFECTS: Cell<usize> = const { Cell::new(0) };
    static COLLECT: Cell<bool> = const { Cell::new(false) };
    static ORDINARY_ATTEMPTS: Cell<usize> = const { Cell::new(0) };
}

fn observe_body() {
    EFFECTS.with(|count| count.set(count.get() + 1));
    if COLLECT.with(Cell::get) {
        let pointer = ARGUMENTS.with(Cell::get);
        assert!(!pointer.is_null());
        let before = unsafe { [(*pointer).to_raw(), (*pointer.add(1)).to_raw()] };
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_ne!(unsafe { (*pointer).to_raw() }, before[0], "source moves");
        assert_ne!(
            unsafe { (*pointer.add(1)).to_raw() },
            before[1],
            "destination moves"
        );
    }
}
fn ordinary_attempt() {
    ORDINARY_ATTEMPTS.with(|count| count.set(count.get() + 1));
}
struct Hooks(Option<fn()>);
impl Hooks {
    fn new(args: &[EgclVal], collect: bool) -> Self {
        ARGUMENTS.with(|pointer| pointer.set(args.as_ptr()));
        EFFECTS.with(|count| count.set(0));
        ORDINARY_ATTEMPTS.with(|count| count.set(0));
        COLLECT.with(|flag| flag.set(collect));
        Self(ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.replace(Some(observe_body))))
    }
}
impl Drop for Hooks {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
        ARGUMENTS.with(|pointer| pointer.set(std::ptr::null()));
        COLLECT.with(|flag| flag.set(false));
    }
}
fn install(env: &mut Env, name: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(
        "(defun packed-store-observe () (%ordinary-t2-entry-for-test))
         (defun packed-store-return (array index) (values array index))",
        env,
    )
    .unwrap();
    let _pins =
        ordinary_t2::RecursiveFixturePins::new(&["PACKED-STORE-OBSERVE", "PACKED-STORE-RETURN"]);
    egcl_rt::rooted!(params = reader::read_from_string("(src dst index)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((locally (declare (notinline packed-store-observe packed-store-return))
             (packed-store-observe)
             (setf (aref dst index) (aref src index))
             (packed-store-return dst index)))"
        )
        .unwrap()
        .0
    );
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let input = snapshot_t2_input(symbol, 0).expect("worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input.with_gc_stable_mutator(|input| {
        let mut artifact = compile_t2_artifact(input).expect("tagged worker artifact");
        let mut ir =
            egcl_compiler::t2::build::build_for_transfer_optimization(&input.body, symbol).unwrap();
        assert!(egcl_compiler::t2::packed_f64::specialize(&mut ir) > 0);
        let stores: Vec<_> = ir
            .block_order()
            .iter()
            .flat_map(|&block| ir.block(block).insts.iter())
            .filter(|&&inst| ir.inst(inst).opcode == Opcode::PackedF64Set)
            .collect();
        assert_eq!(
            stores.len(),
            1,
            "the actual installed IR contains the new opcode"
        );
        let state = ir
            .frame_states
            .get(ir.inst(*stores[0]).frame_state.unwrap());
        assert_eq!(state.scopes.len(), 1);
        let scope = &state.scopes[0];
        assert_eq!(scope.function, symbol);
        assert!(
            matches!(
                input.body.code[scope.bcp as usize],
                Instr::CallNamed { nargs: 3, .. }
            ),
            "store guard resumes the original setter with its operands"
        );
        assert!(scope.stack.len() >= 3);
        egcl_compiler::t2::verify::verify(&ir).unwrap();
        artifact.mapped = Some(prepare_ordinary_t2(input, &ir).expect("packed ordinary candidate"));
        artifact.mapped_fallback = None;
        artifact.speculations.clear();
        artifact.cold_guesses.clear();
        artifact
    });
    let native = install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("installed packed ordinary T2");
    assert!(native.is_t2);
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped owner")
    };
    assert!(
        code.ordinary_entry.is_some(),
        "must not silently use mapped-only entry"
    );
    assert!(
        code.lean_root_entry().is_some(),
        "actual root dispatch selects ordinary ABI"
    );
    (symbol, native)
}
fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        unreachable!()
    };
    code
}
fn run(
    native: &NativeCode,
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    ordinary_t2::expect_entry(native);
    with_ordinary_entry_hook_for_test(ordinary_attempt, || run_native(native, symbol, args, env))
}
fn assert_entered() {
    assert_eq!(ORDINARY_ATTEMPTS.with(Cell::get), 1);
    assert!(
        ordinary_t2::observed_entry(),
        "actual machine body published its exact native owner"
    );
    assert_eq!(
        EFFECTS.with(Cell::get),
        1,
        "guard recovery must not replay the earlier effect"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_packed_store_reloads_moving_arrays_and_returns_multiple_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install(&mut env, "ORDINARY-PACKED-STORE-MOVING");
    let _pins =
        ordinary_t2::RecursiveFixturePins::new(&["PACKED-STORE-OBSERVE", "PACKED-STORE-RETURN"]);
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(src = egcl_rt::packed_f64::make(1, -0.0).unwrap());
    egcl_rt::rooted!(dst = egcl_rt::packed_f64::make(1, 91.0).unwrap());
    egcl_rt::rooted!(args = vec![*src, *dst, EgclVal::from_fixnum(0)]);
    let frame = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    let _hooks = Hooks::new(&args, true);
    egcl_rt::rooted!(result = run(&native, symbol, &args, &mut env).unwrap());
    assert_entered();
    assert_eq!(*result, *dst);
    assert_eq!(
        egcl_rt::packed_f64::get(*dst, 0).unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert!(env.mv_active);
    assert_eq!(env.mv, vec![*dst, EgclVal::from_fixnum(0)]);
    assert_eq!(owner(&native).deopt_count(), 0);
    assert_eq!(
        (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
        (frame, sp)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_packed_store_cold_guards_preserve_effects_and_allow_retry() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for bounds in [false, true] {
        let name = if bounds {
            "ORDINARY-PACKED-STORE-BOUNDS"
        } else {
            "ORDINARY-PACKED-STORE-TYPE"
        };
        let (symbol, native) = install(&mut env, name);
        let _pins = ordinary_t2::RecursiveFixturePins::new(&[
            "PACKED-STORE-OBSERVE",
            "PACKED-STORE-RETURN",
        ]);
        egcl_rt::rooted!(src = egcl_rt::packed_f64::make(2, 2.5).unwrap());
        egcl_rt::rooted!(
            dst = if bounds {
                egcl_rt::packed_f64::make(1, 91.0).unwrap()
            } else {
                EgclVal::from_fixnum(17)
            }
        );
        egcl_rt::rooted!(args = vec![*src, *dst, EgclVal::from_fixnum(if bounds { 1 } else { 0 })]);
        let frame = egcl_rt::current_stack().fp();
        let sp = egcl_rt::current_stack().sp();
        let _hooks = Hooks::new(&args, false);
        egcl_rt::rooted!(error = run(&native, symbol, &args, &mut env).unwrap_err());
        assert_entered();
        assert_eq!(
            owner(&native).deopt_count(),
            1,
            "cold store guard reconstructs exactly once"
        );
        egcl_rt::rooted!(
            condition = match error.without_backtrace() {
                EgclError::Signalled(details) => details.condition,
                other => panic!("cold store must signal its public condition: {other:?}"),
            }
        );
        assert!(crate::cli::condition_matches_handler(
            &env,
            *condition,
            "TYPE-ERROR"
        ));
        if bounds {
            assert_eq!(egcl_rt::packed_f64::get(*dst, 0), Some(91.0));
        }
        egcl_rt::rooted!(stored = egcl_rt::gc::alloc_double_float(2.5));
        egcl_rt::rooted!(
            oracle = egcl_stdlib::sequences::set_aref(*dst, if bounds { 1 } else { 0 }, *stored)
                .unwrap_err()
        );
        // The native error boundary signals the library error. Compare the
        // actual condition slots with the raw public setter's semantic data,
        // using the same existing expected-type representation conversion.
        let (expected_datum, expected_type_text) = match oracle.without_backtrace() {
            EgclError::TypeError { datum, expected } => (*datum, expected.clone()),
            other => panic!("public setter must return a raw TYPE-ERROR: {other:?}"),
        };
        egcl_rt::rooted!(expected_datum = expected_datum);
        egcl_rt::rooted!(expected_type = crate::cli::expected_type_value(&expected_type_text));
        egcl_rt::rooted!(datum_key = crate::cli::resolve_sym("DATUM").unwrap());
        egcl_rt::rooted!(type_key = crate::cli::resolve_sym("EXPECTED-TYPE").unwrap());
        egcl_rt::rooted!(
            datum = crate::cli::read_slot_value(*condition, *datum_key, &env).unwrap()
        );
        egcl_rt::rooted!(
            expected = crate::cli::read_slot_value(*condition, *type_key, &env).unwrap()
        );
        assert_eq!(
            *datum, *expected_datum,
            "cold continuation preserves the exact error datum"
        );
        assert!(
            egcl_stdlib::equality::equal(*expected, *expected_type),
            "cold continuation preserves the public setter's expected type"
        );
        assert_eq!(
            (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
            (frame, sp)
        );
        // The failed guard may retire its version. Reinstall the same source
        // before retry so the body marker remains an actual native witness.
        // Reusing the caller Env/stack tests error and MV boundary cleanup.
        let (retry_symbol, retry) = install(&mut env, &format!("{name}-RETRY"));
        egcl_rt::rooted!(replacement = egcl_rt::packed_f64::make(2, 91.0).unwrap());
        args[1] = *replacement;
        env.clear_mv();
        ORDINARY_ATTEMPTS.with(|count| count.set(0));
        egcl_rt::rooted!(result = run(&retry, retry_symbol, &args, &mut env).unwrap());
        assert_eq!(ORDINARY_ATTEMPTS.with(Cell::get), 1);
        assert!(ordinary_t2::observed_entry());
        assert_eq!(owner(&retry).deopt_count(), 0);
        assert_eq!(*result, *replacement);
        assert_eq!(
            egcl_rt::packed_f64::get(*replacement, if bounds { 1 } else { 0 }),
            Some(2.5)
        );
        assert_eq!(EFFECTS.with(Cell::get), 2);
        assert!(env.mv_active);
        assert_eq!(env.mv, vec![*replacement, args[2]]);
        assert_eq!(
            (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
            (frame, sp)
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_packed_store_guard_boxes_value_for_general_vector_and_continues() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install(&mut env, "ORDINARY-PACKED-STORE-GENERAL");
    let _pins =
        ordinary_t2::RecursiveFixturePins::new(&["PACKED-STORE-OBSERVE", "PACKED-STORE-RETURN"]);
    egcl_rt::rooted!(src = egcl_rt::packed_f64::make(1, 2.5).unwrap());
    egcl_rt::rooted!(dst = crate::cli::read_eval_all_env("(vector nil)", &mut env).unwrap());
    egcl_rt::rooted!(args = vec![*src, *dst, EgclVal::from_fixnum(0)]);
    let frame = egcl_rt::current_stack().fp();
    let sp = egcl_rt::current_stack().sp();
    let _hooks = Hooks::new(&args, false);
    egcl_rt::rooted!(result = run(&native, symbol, &args, &mut env).unwrap());
    assert_entered();
    assert_eq!(
        owner(&native).deopt_count(),
        1,
        "the packed store rejects the general destination layout"
    );
    assert_eq!(*result, *dst);
    egcl_rt::rooted!(stored = egcl_stdlib::sequences::svref(*dst, 0).unwrap());
    assert!(
        stored.is_double_float(),
        "cold recovery must box the raw F64 setter operand"
    );
    assert_eq!(stored.as_double_float().to_bits(), 2.5_f64.to_bits());
    assert!(env.mv_active);
    assert_eq!(env.mv, vec![*dst, EgclVal::from_fixnum(0)]);
    assert_eq!(
        (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()),
        (frame, sp)
    );
}
