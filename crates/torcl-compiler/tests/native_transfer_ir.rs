//! Calls acquire an explicit exceptional route before native ABI activation.
use torcl_compiler::control_scope::ScopeKind;
use torcl_compiler::t2::build::build_from_bytecode_for_transfers;
use torcl_compiler::t2::ir::{AuxData, Function, Inst, Opcode};
use torcl_compiler::t2::verify::verify;
use torcl_rt::bytecode::{BytecodeFunction, Instr};

fn body() -> BytecodeFunction {
    BytecodeFunction {
        code: vec![
            Instr::PushBlock {
                block_id: 7,
                name_idx: 0,
                resume_bcp: 4,
                sp_restore: 0,
                register: true,
            },
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: 123456,
                nargs: 1,
            },
            Instr::PopHandler,
            Instr::CallNamed {
                sym: 123457,
                nargs: 1,
            },
            Instr::Return,
        ],
        constants: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        names: vec!["BLOCK".into()],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 2,
        max_stack: 2,
        arity: 2,
        name: "transfer-calls".into(),
        params_form: torcl_rt::value::NIL,
        min_args: 2,
        max_args: Some(2),
        variadic: false,
    }
}

fn instructions(f: &Function, opcode: Opcode) -> Vec<Inst> {
    f.block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| f.inst(i).opcode == opcode)
        .collect()
}

#[test]
fn builder_automatically_routes_calls_to_mapped_transfer_continuations() {
    let f = build_from_bytecode_for_transfers(&body()).unwrap();
    assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    assert!(instructions(&f, Opcode::Call).is_empty());
    let calls = instructions(&f, Opcode::Invoke);
    assert_eq!(calls.len(), 2);
    for call in calls {
        let invoke = f.inst(call);
        let transfer = f.inst(f.terminator(invoke.targets[1].block).unwrap());
        assert_eq!(transfer.opcode, Opcode::NlxTransfer);
        assert_eq!(transfer.frame_state, invoke.frame_state);
        let AuxData::TransferSite { origin_bcp, scopes } = &transfer.aux else {
            panic!("cold route must carry the source scope map")
        };
        let frame = f
            .frame_states
            .get(invoke.frame_state.unwrap())
            .scopes
            .last()
            .unwrap();
        assert_eq!(frame.bcp, *origin_bcp);
        assert_eq!(
            frame.locals.len(),
            2,
            "exception-only local remains reconstructible"
        );
        assert_eq!(frame.stack.len(), 1, "capture pre-call operands");
        if *origin_bcp == 2 {
            assert!(
                matches!(scopes.as_slice(), [scope] if matches!(scope.kind, ScopeKind::Block { id: 7, .. }))
            );
        } else {
            assert_eq!(*origin_bcp, 4);
            assert!(scopes.is_empty(), "the second call is outside the block");
        }
        assert!(
            transfer.targets.is_empty(),
            "fallback begins unwinding, not bytecode replay"
        );
    }
}

#[test]
fn transfer_verifier_rejects_missing_or_mismatched_capture_state() {
    let mut f = build_from_bytecode_for_transfers(&body()).unwrap();
    let cold = instructions(&f, Opcode::NlxTransfer)[0];
    let saved = f.inst(cold).frame_state;
    f.inst_mut(cold).frame_state = None;
    assert!(
        verify(&f).is_err(),
        "unmapped transfer cannot preserve roots"
    );
    f.inst_mut(cold).frame_state = saved;
    if let AuxData::TransferSite { origin_bcp, .. } = &mut f.inst_mut(cold).aux {
        *origin_bcp += 1;
    }
    assert!(
        verify(&f).is_err(),
        "capture must name the throwing operation"
    );
}

#[test]
fn cold_capture_survives_dce_and_reaches_machine_root_liveness() {
    use torcl_compiler::t2::frame_state::ValueSource;
    use torcl_compiler::t2::lower::{lower, op};
    use torcl_compiler::t2::opt_dce::Dce;
    use torcl_compiler::t2::pass::{Analyses, Pass};
    let mut f = build_from_bytecode_for_transfers(&body()).unwrap();
    let exception_only = f.block(f.entry()).params[1];
    Dce.run(&mut f, &mut Analyses::new());
    assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    for cold in instructions(&f, Opcode::NlxTransfer) {
        let frame = f
            .frame_states
            .get(f.inst(cold).frame_state.unwrap())
            .scopes
            .last()
            .unwrap();
        assert!(
            matches!(frame.locals[1], ValueSource::Value { value, .. } if value == exception_only)
        );
    }
    let machine = lower(&f);
    let captures: Vec<_> = machine
        .insts
        .iter()
        .filter(|i| i.op == op::NLX_TRANSFER)
        .collect();
    assert_eq!(captures.len(), 2);
    assert!(
        captures
            .iter()
            .all(|i| i.safepoint && i.deopt_uses.iter().any(|v| v.num == exception_only.0))
    );
    assert!(
        machine.insts.iter().any(|i| i.op == op::PSEUDO_UNSUPPORTED),
        "Invoke must remain un-emittable until its machine contract exists"
    );
}

#[test]
fn automatic_call_routes_preserve_loop_and_osr_header_state() {
    let mut source = body();
    source.code = vec![
        Instr::PushTag {
            tagbody_id: 8,
            sp_restore: 0,
        },
        Instr::LoadLocal(0),
        Instr::CallNamed {
            sym: 123456,
            nargs: 1,
        },
        Instr::BrIfFalse(6),
        Instr::Go {
            tagbody_id: 8,
            target_bcp: 1,
        },
        Instr::Br(6),
        Instr::PopHandler,
        Instr::LoadLocal(0),
        Instr::Return,
    ];
    let f = build_from_bytecode_for_transfers(&source).unwrap();
    assert!(verify(&f).is_ok(), "{:?}", verify(&f));
    assert_eq!(instructions(&f, Opcode::Invoke).len(), 1);
    assert_eq!(f.osr_entries.len(), 1);
    assert_eq!(f.osr_entries[0].bcp, 1);
    let frame = f.frame_states.get(f.osr_entries[0].frame_state);
    assert_eq!(frame.scopes.last().unwrap().locals.len(), 2);
    assert!(frame.scopes.last().unwrap().stack.is_empty());
}
