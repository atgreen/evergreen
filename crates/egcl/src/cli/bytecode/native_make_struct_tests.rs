// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The baseline (legacy OSR) emitter's MAKE-STRUCT: a collecting helper call
//! over the scanned operand slots, with a deopt to T0 on a miss
//! (bliss-gg3i1.7.2).

use super::*;

/// `(loop while (> n 0) do (setq acc (%make-struct 'name (cons n acc) n))
/// (decf n))` as hand-built bytecode entered at its loop header, so the legacy
/// emitter compiles it. Each fresh cons is a nursery object that the
/// constructor's allocation may move.
fn make_struct_loop(name: EgclVal, env: &mut Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
    let mut body = compile_function("NATIVE-MAKE-STRUCT", NIL, *form, env, false, false).unwrap();
    let gt = resolve_sym(">").unwrap().as_symbol_index();
    let dec = resolve_sym("1-").unwrap().as_symbol_index();
    body.n_locals = 2;
    body.max_stack = 3;
    body.constants = vec![EgclVal::from_fixnum(0), name];
    body.code = vec![
        Instr::PushTag {
            tagbody_id: 0,
            sp_restore: 0,
        },
        Instr::LoadLocal(1),
        Instr::Const(0),
        Instr::CallNamed { sym: gt, nargs: 2 },
        Instr::BrIfFalse(15),
        Instr::LoadLocal(1),
        Instr::LoadLocal(0),
        Instr::AllocCons,
        Instr::LoadLocal(1),
        Instr::MakeStruct { name: 1, nargs: 2 },
        Instr::StoreLocal(0),
        Instr::LoadLocal(1),
        Instr::CallNamed { sym: dec, nargs: 1 },
        Instr::StoreLocal(1),
        Instr::Go {
            tagbody_id: 0,
            target_bcp: 1,
        },
        Instr::LoadLocal(0),
        Instr::Return,
    ];
    Arc::new(body)
}

fn run_loop(osr: &OsrCode, count: i64, env: &mut Env) -> Result<OsrOutcome, EgclError> {
    let thread = egcl_rt::current_thread();
    let stack = thread.stack();
    let frame = stack
        .push_frame(NIL, osr.code_info, osr.num_slots, FLAG_CALL)
        .unwrap();
    unsafe {
        slot_set(frame, 0, NIL);
        slot_set(frame, 1, EgclVal::from_fixnum(count));
    }
    let result = run_native_osr(osr, osr.entries[&1], frame, None, env);
    stack.pop_frame();
    result
}

#[test]
fn baseline_make_struct_builds_from_scanned_slots_and_deopts_on_a_miss() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        definition = reader::read_from_string("(defstruct baseline-ms a b)")
            .unwrap()
            .0
    );
    super::super::eval_form(*definition, &mut env).unwrap();
    let name = resolve_sym("BASELINE-MS").unwrap();
    let body = make_struct_loop(name, &mut env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    let osr = compile_osr_code(&body, u32::MAX).expect("the baseline emitter compiles MAKE-STRUCT");

    let Ok(OsrOutcome::Finished(value)) = run_loop(&osr, 3, &mut env) else {
        panic!("the loop finishes natively");
    };
    assert_eq!(
        crate::cli::format_val(value),
        "#S(BASELINE-MS :A (1 . #S(BASELINE-MS :A (2 . #S(BASELINE-MS :A (3) :B 3)) :B 2)) :B 1)"
    );
    // Many iterations allocate enough to collect while values are on the
    // operand stack; every instance must still hold its own cons.
    let Ok(OsrOutcome::Finished(mut value)) = run_loop(&osr, 2000, &mut env) else {
        panic!("the long loop finishes natively");
    };
    for expected in 1..=2000 {
        let a = egcl_stdlib::slot_value(value, resolve_sym("A").unwrap()).unwrap();
        let b = egcl_stdlib::slot_value(value, resolve_sym("B").unwrap()).unwrap();
        assert_eq!(b, EgclVal::from_fixnum(expected));
        let (head, tail) = cp(a);
        assert_eq!(head, EgclVal::from_fixnum(expected));
        value = tail;
    }
    assert!(value.is_nil());

    // An unknown structure misses the fast path: the native code deopts and
    // T0's general path signals.
    let unknown = EgclVal::from_symbol_index(egcl_rt::symbols::intern("BASELINE-MS-UNDEFINED"));
    let missing = make_struct_loop(unknown, &mut env);
    egcl_rt::rooted!(_missing = ActiveBytecodeRoot::new(&missing));
    let osr = compile_osr_code(&missing, u32::MAX).expect("compiles");
    assert!(!matches!(
        run_loop(&osr, 2, &mut env),
        Ok(OsrOutcome::Finished(_))
    ));
}
