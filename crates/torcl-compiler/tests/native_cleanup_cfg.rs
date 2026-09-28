use torcl_compiler::t2::{
    build,
    ir::{AuxData, Opcode},
    verify,
};
use torcl_rt::TorclVal;
use torcl_rt::bytecode::{BytecodeFunction, Instr};

#[test]
fn throwing_predecessor_preserves_the_local_before_a_later_assignment() {
    let mut body = BytecodeFunction {
        name: "cleanup-merge".into(),
        code: vec![],
        constants: vec![TorclVal::from_fixnum(99)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 1,
        arity: 1,
        params_form: torcl_rt::value::NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
    };
    body.code = vec![
        Instr::PushUnwind {
            cleanup_bcp: 8,
            sp_restore: 0,
        },
        Instr::CallNamed {
            sym: torcl_rt::symbols::intern("MAY-THROW"),
            nargs: 0,
        },
        Instr::Pop,
        Instr::Const(0),
        Instr::StoreLocal(0),
        Instr::LoadLocal(0),
        Instr::PopHandler,
        Instr::EnterCleanupNormal {
            cleanup_bcp: 8,
            resume_bcp: 12,
        },
        Instr::LoadLocal(0),
        Instr::CallNamed {
            sym: torcl_rt::symbols::intern("OBSERVE-CLEANUP"),
            nargs: 1,
        },
        Instr::Pop,
        Instr::CleanupReturn,
        Instr::Return,
    ];
    let f = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&f).unwrap();
    let landing = f
        .block_order()
        .iter()
        .copied()
        .find(|&b| {
            f.block(b)
                .insts
                .iter()
                .any(|&i| f.inst(i).opcode == Opcode::CleanupLanding)
        })
        .expect("native cleanup entry");
    assert!(
        !f.block(landing).params.is_empty(),
        "old and new local must merge"
    );
    let incoming: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .flat_map(|&i| f.inst(i).targets.iter())
        .filter(|edge| edge.block == landing)
        .collect();
    assert_eq!(incoming.len(), 2);
    assert_ne!(
        incoming[0].args, incoming[1].args,
        "exception sees old local; normal sees 99"
    );
    assert!(
        incoming
            .iter()
            .any(|edge| edge.args.contains(&f.block(f.entry()).params[0]))
    );
    let assigned = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .find_map(|&i| {
            matches!(f.inst(i).aux, AuxData::FixnumImm(99)).then(|| f.inst(i).results[0])
        })
        .unwrap();
    assert!(incoming.iter().any(|edge| edge.args.contains(&assigned)));
    assert!(
        f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .any(|&i| f.inst(i).opcode == Opcode::Invoke
                && matches!(f.inst(i).aux, AuxData::CleanupContinuation { .. })),
        "cleanup completion must represent normal and pending-transfer routes"
    );
    let mut bad = f.clone();
    let transfer = bad
        .block_order()
        .iter()
        .flat_map(|&b| &bad.block(b).insts)
        .copied()
        .find(|&i| bad.inst(i).opcode == Opcode::NlxTransfer && !bad.inst(i).targets.is_empty())
        .unwrap();
    if let AuxData::TransferSite { scopes, .. } = &mut bad.inst_mut(transfer).aux {
        scopes.retain(|scope| {
            !matches!(
                scope.kind,
                torcl_compiler::control_scope::ScopeKind::Unwind { .. }
            )
        });
    }
    assert!(
        verify::verify(&bad)
            .unwrap_err()
            .iter()
            .any(|error| error.check == "V13 cleanup"),
        "a native landing requires the selected unwind scope"
    );
}
