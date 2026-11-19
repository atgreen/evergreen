// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
use egcl_compiler::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, OsrEntry, ValueRepresentation,
};

// The existing ordinary_osr_companion_tests fixture proves these executable
// protocols. Use the same no-tagbody IR so a source TAGBODY classification
// cannot accidentally make this Root-preference regression pass.
fn companion_code(env: &mut Env, name: &str, tier: CompilationTier) -> (u32, Rc<TransferCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} (x) x)"), env).unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    let mut ir = Function::new(name);
    let entry = ir.entry();
    let value = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let state = ir.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: symbol,
            bcp: 0,
            locals: vec![ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            }],
            stack: vec![],
        }],
        remat: vec![],
    });
    ir.set_terminator(
        entry,
        InstData {
            opcode: Opcode::Return,
            args: vec![value],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: Some(state),
            source_pos: 0,
        },
    );
    ir.osr_entries.push(OsrEntry {
        control_scopes: vec![],
        bcp: 0,
        block: entry,
        frame_state: state,
        checks: vec![],
    });
    egcl_compiler::t2::verify::verify(&ir).unwrap();
    let mut code = TransferCode::emit_prepared_with_ordinary(
        Arc::clone(&body),
        retain_native_body(&body),
        &ir,
        Some(&ir),
        None,
        &[],
        false,
        tier,
    )
    .expect("real ordinary primary and mapped OSR companion");
    code.set_installed_symbol(symbol);
    assert!(code.ordinary_entry.is_some() && code.lean_root_entry().is_some());
    assert!(code.osr_entries.is_empty() && code.osr_companion.is_some());
    assert!(code.native_callable_slot.is_none());
    assert!(!t1_ordinary_tagbody_body(&body));
    assert!(ordinary_native_body(&body) && fixed_tagged_parameters(&body));
    assert!(body.self_call_sites.is_empty());
    (symbol, Rc::new(code))
}

#[test]
#[ignore = "Root/T1 flags ON, T2 enabled: actual companion preference with a valid linked caller"]
fn t1_root_rejects_primary_with_companion_and_retains_mapped_execution() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(enabled() && t1_enabled() && t2_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = companion_code(&mut env, "T1-ROOT-OSR-PREFERENCE", CompilationTier::T1);
    assert!(!code.is_t2 && code.ordinary_root_allowed() && !code.needs_recompile());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert!(egcl_rt::function::is_interpreted_function(*function));
    assert!(egcl_rt::function::closure_env(*function).is_nil());
    assert!(
        closure_captured_env(*function).is_none()
            && !super::super::super::has_captured_control(*function)
    );
    let invokes = egcl_rt::function::invoke_count(*function);
    start_activation_counts_for_test(symbol);
    // Full T1 calls already choose the executable mapped companion before the
    // ordinary primary. The canonical Root fast path must preserve that choice.
    assert_eq!(
        with_ordinary_entry_hook_for_test(
            || panic!("full T1 call must not enter the ordinary primary"),
            || code.run_callable(*function, &[EgclVal::from_fixnum(41)], &mut env),
        )
        .unwrap(),
        EgclVal::from_fixnum(41)
    );
    assert_eq!(take_activation_counts_for_test(), (1, 1));

    let destination = code.osr_destination();
    assert!(destination.ordinary_entry.is_none());
    let offset = *destination.osr_entries.get(&0).unwrap();
    let state = Box::new(ActivationState::prepare(destination, &mut env).unwrap());
    let stack = egcl_rt::current_stack();
    let frame = stack
        .push_frame(
            *function,
            destination.code_info,
            destination.slots,
            FLAG_CALL,
        )
        .unwrap();
    unsafe { slot_set(frame, 0, EgclVal::from_fixnum(73)) };
    assert_eq!(stack.fp(), frame, "a valid, linked managed caller exists");
    assert!(!native_error_pending() && NATIVE_DEPTH.with(Cell::get) < native_depth_cap());
    assert!(env.locally_specials.is_empty());
    let sp = stack.sp();
    let capture = CAPTURE.with(Cell::get);
    let depth = NATIVE_DEPTH.with(Cell::get);
    assert!(code.rust_root_entry.get().is_none());
    assert!(try_run(&code, *function, &[EgclVal::from_fixnum(41)], &mut env).is_none());
    // Without the companion exclusion all cached static predicates are true;
    // with the valid caller above the old code would execute the primary.
    assert_eq!(code.rust_root_entry.get().copied(), Some(None));
    assert_eq!((stack.fp(), stack.sp()), (frame.cast_const(), sp));
    assert_eq!(CAPTURE.with(Cell::get), capture);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
    let result = destination
        .run_activation(
            &[],
            &mut env,
            Some(OsrResume {
                frame,
                offset,
                state,
            }),
            *function,
        )
        .unwrap();
    assert_eq!(
        result,
        EgclVal::from_fixnum(73),
        "mapped import keeps the live local"
    );
    assert_eq!(egcl_rt::function::invoke_count(*function), invokes);
}

#[test]
#[ignore = "Root flag ON: T2 companion does not forbid the existing ordinary capability"]
fn t2_companion_keeps_the_existing_root_capability() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = companion_code(&mut env, "T2-ROOT-OSR-CONTROL", CompilationTier::T2);
    assert!(code.is_t2 && !code.needs_recompile());
    assert!(egcl_rt::current_stack().fp().is_null());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    // A missing caller still declines execution, but the immutable T2 entry
    // capability must remain accepted despite the presence of a companion.
    assert!(try_run(&code, *function, &[EgclVal::from_fixnum(41)], &mut env).is_none());
    assert_eq!(
        code.rust_root_entry.get().copied(),
        Some(code.lean_root_entry())
    );
}
