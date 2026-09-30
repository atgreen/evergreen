use egcl_compiler::control_scope::{Ownership, ScopeError, ScopeMap};
use egcl_rt::bytecode::Instr;

fn block(id: u32, resume: u32) -> Instr {
    Instr::PushBlock {
        block_id: id,
        name_idx: 0,
        resume_bcp: resume,
        sp_restore: 0,
        register: false,
    }
}

#[test]
fn exits_remove_inner_scopes_but_go_retains_its_tagbody() {
    let code = vec![
        block(1, 9),
        Instr::PushTag {
            tagbody_id: 2,
            sp_restore: 0,
        },
        block(3, 6),
        Instr::Go {
            tagbody_id: 2,
            target_bcp: 6,
        },
        Instr::PopHandler,
        Instr::Br(6),
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::PopHandler,
        Instr::Return,
    ];
    let map = ScopeMap::analyze(&code).unwrap();
    let go = map.exit_at(3).unwrap();
    assert_eq!(go.removed.len(), 1);
    assert_eq!(go.removed[0].push_bcp, 2);
    assert_eq!(map.before(6).unwrap().len(), 2);
    let ret = map.exit_at(7).unwrap();
    assert_eq!(ret.removed.len(), 2);
    assert_eq!(map.before(9).unwrap().len(), 0);
}

#[test]
fn osr_entry_preserves_inherited_ownership_through_local_scope_changes() {
    let code = vec![
        block(1, 7),
        Instr::PushTag {
            tagbody_id: 2,
            sp_restore: 0,
        },
        block(3, 4),
        Instr::PopHandler,
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::PopHandler,
        Instr::Return,
    ];
    let map = ScopeMap::analyze_osr(&code, 2).unwrap();
    assert!(
        map.before(2)
            .unwrap()
            .iter()
            .all(|s| s.ownership == Ownership::Inherited)
    );
    assert_eq!(
        map.before(3).unwrap().last().unwrap().ownership,
        Ownership::Local
    );
    assert_eq!(map.exit_at(5).unwrap().removed.len(), 2);
    assert!(
        map.exit_at(5)
            .unwrap()
            .removed
            .iter()
            .all(|s| s.ownership == Ownership::Inherited)
    );
}

#[test]
fn scope_joins_and_inactive_targets_are_rejected() {
    let join = [
        Instr::BrIfFalse(2),
        block(1, 3),
        Instr::Const(0),
        Instr::Return,
    ];
    assert!(matches!(
        ScopeMap::analyze(&join),
        Err(ScopeError::InconsistentJoin { .. })
    ));
    let inactive = [
        block(1, 4),
        Instr::PopHandler,
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::Return,
    ];
    assert!(matches!(
        ScopeMap::analyze(&inactive),
        Err(ScopeError::InactiveTarget { bcp: 3 })
    ));
    assert!(matches!(
        ScopeMap::analyze(&[Instr::PopHandler]),
        Err(ScopeError::EmptyPop { bcp: 0 })
    ));
}

#[test]
fn osr_inherited_catch_keeps_its_exceptional_resume_reachable() {
    let code = [
        Instr::PushCatch {
            resume_bcp: 3,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::Throw,
        Instr::Return,
    ];
    let map = ScopeMap::analyze_osr(&code, 1).unwrap();
    assert_eq!(map.before(3), Some([].as_slice()));
}

fn normal_cleanup() -> Vec<Instr> {
    vec![
        block(1, 10),
        Instr::PushUnwind {
            cleanup_bcp: 7,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::PopHandler,
        Instr::EnterCleanupNormal {
            cleanup_bcp: 7,
            resume_bcp: 5,
        },
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::Const(0),
        Instr::Pop,
        Instr::CleanupReturn,
        Instr::Return,
    ]
}

#[test]
fn normal_cleanup_retires_its_handler_and_resumes_with_outer_scopes() {
    use egcl_compiler::control_scope::ScopeKind;
    let map = ScopeMap::analyze(&normal_cleanup()).unwrap();
    assert!(matches!(
        map.before(2).unwrap()[1].kind,
        ScopeKind::Unwind { .. }
    ));
    assert!(matches!(
        map.before(7).unwrap()[1].kind,
        ScopeKind::Cleanup { .. }
    ));
    assert_eq!(map.before(5).unwrap().len(), 1);
    assert_eq!(map.cleanup_resumes(7), &[5]);
}

#[test]
fn osr_preserves_inherited_cleanup_handler_and_running_continuation() {
    use egcl_compiler::control_scope::ScopeKind;
    for entry in [2, 7] {
        let map = ScopeMap::analyze_osr(&normal_cleanup(), entry).unwrap();
        let cleanup = map.before(7).unwrap().last().unwrap();
        assert!(matches!(
            cleanup.kind,
            ScopeKind::Cleanup { cleanup_bcp: 7 }
        ));
        assert_eq!(cleanup.ownership, Ownership::Inherited);
        assert_eq!(map.before(5).unwrap().len(), 1);
        assert_eq!(map.before(5).unwrap()[0].ownership, Ownership::Inherited);
    }
}

#[test]
fn lexical_exit_records_cleanup_and_cleanup_can_replace_that_exit() {
    use egcl_compiler::control_scope::ScopeKind;
    let code = [
        block(1, 7),
        Instr::PushUnwind {
            cleanup_bcp: 4,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::Const(1),
        Instr::ReturnFrom { block_id: 1 },
        Instr::CleanupReturn,
        Instr::Return,
    ];
    let map = ScopeMap::analyze(&code).unwrap();
    assert!(matches!(
        map.exit_at(3).unwrap().removed[0].kind,
        ScopeKind::Unwind { .. }
    ));
    assert!(matches!(
        map.exit_at(5).unwrap().removed[0].kind,
        ScopeKind::Cleanup { .. }
    ));
    assert!(
        map.before(6).is_none(),
        "superseded continuation must not return"
    );
    assert_eq!(map.cleanup_resumes(4), &[]);
}

#[test]
fn cleanup_protocol_rejects_orphan_returns_and_mismatched_normal_entries() {
    assert!(ScopeMap::analyze(&[Instr::CleanupReturn]).is_err());
    let mut code = normal_cleanup();
    code[4] = Instr::EnterCleanupNormal {
        cleanup_bcp: 8,
        resume_bcp: 5,
    };
    assert!(ScopeMap::analyze(&code).is_err());
    code[4] = Instr::Br(5);
    assert!(
        ScopeMap::analyze(&code).is_err(),
        "popped cleanup cannot be silently discarded"
    );
}

fn function_with_scopes(code: Vec<Instr>) -> egcl_rt::bytecode::BytecodeFunction {
    egcl_rt::bytecode::BytecodeFunction {
        code,
        constants: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        names: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 2,
        arity: 0,
        name: "scopes".into(),
        params_form: egcl_rt::value::NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    }
}

#[test]
fn condition_clause_unwinds_cluster_but_handler_bind_stays_in_context() {
    use egcl_compiler::control_scope::ScopeKind;
    use egcl_rt::bytecode::{ClauseInfo, HandlerBindInfo, HandlerCaseInfo};
    let mut body = function_with_scopes(vec![
        Instr::PushHandlerBind { hb: 0 },
        Instr::PushHandlerCase {
            hc: 0,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::PopHandlerCase,
        Instr::Br(7),
        Instr::LoadLocal(0),
        Instr::Br(7),
        Instr::PopHandlerBind,
        Instr::Return,
    ]);
    body.handler_binds
        .push(HandlerBindInfo { bindings: vec![] });
    body.handler_cases.push(HandlerCaseInfo {
        clauses: vec![ClauseInfo {
            type_name: "ERROR".into(),
            body_bcp: 5,
            var_slot: Some(0),
        }],
    });
    let map = ScopeMap::analyze_function(&body).unwrap();
    assert_eq!(map.before(2).unwrap().len(), 2);
    let clause = map.before(5).unwrap();
    assert_eq!(clause.len(), 1);
    assert!(matches!(clause[0].kind, ScopeKind::HandlerBind { .. }));
    let osr = ScopeMap::analyze_osr_function(&body, 2).unwrap();
    assert_eq!(osr.before(5).unwrap()[0].ownership, Ownership::Inherited);
    assert!(osr.before(8).unwrap().is_empty());
}

#[test]
fn restart_result_resumes_outside_its_cluster_including_at_osr_entry() {
    use egcl_compiler::control_scope::ScopeKind;
    use egcl_rt::bytecode::RestartCaseInfo;
    let mut body = function_with_scopes(vec![
        Instr::PushRestartCase {
            rc: 0,
            resume_bcp: 4,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::PopRestartCase,
        Instr::Br(4),
        Instr::Return,
    ]);
    body.restart_cases
        .push(RestartCaseInfo { restarts: vec![] });
    let map = ScopeMap::analyze_osr_function(&body, 1).unwrap();
    assert!(matches!(
        map.before(1).unwrap()[0].kind,
        ScopeKind::RestartCase { .. }
    ));
    assert_eq!(map.before(1).unwrap()[0].ownership, Ownership::Inherited);
    assert!(map.before(4).unwrap().is_empty());
    // A restart pop must never silently discard an unrelated handler cluster.
    body.code[2] = Instr::PopHandlerCase;
    assert!(ScopeMap::analyze_function(&body).is_err());
}

#[test]
fn condition_scope_tables_and_clause_targets_are_validated() {
    use egcl_rt::bytecode::{ClauseInfo, HandlerCaseInfo};
    let mut body = function_with_scopes(vec![
        Instr::PushHandlerCase {
            hc: 0,
            sp_restore: 0,
        },
        Instr::Return,
    ]);
    assert!(
        ScopeMap::analyze(&body.code).is_err(),
        "instruction-only analysis lacks clauses"
    );
    assert!(ScopeMap::analyze_function(&body).is_err(), "missing table");
    body.handler_cases.push(HandlerCaseInfo {
        clauses: vec![ClauseInfo {
            type_name: "ERROR".into(),
            body_bcp: 99,
            var_slot: Some(0),
        }],
    });
    assert!(
        ScopeMap::analyze_function(&body).is_err(),
        "invalid clause destination"
    );
    body.handler_cases[0].clauses[0].body_bcp = 1;
    body.handler_cases[0].clauses[0].var_slot = Some(1);
    assert!(
        ScopeMap::analyze_function(&body).is_err(),
        "invalid condition slot"
    );
}

#[test]
fn binding_and_environment_scopes_survive_osr_and_retire_independently() {
    use egcl_compiler::control_scope::ScopeKind;
    let code = [
        Instr::BindSpecial(1),
        Instr::PushEnvChild,
        Instr::BindSpecial(2),
        Instr::PushEnvChild,
        Instr::UnbindSpecial(2),
        Instr::PopEnvChild,
        Instr::PopEnvChild,
        Instr::Return,
    ];
    let map = ScopeMap::analyze_osr(&code, 3).unwrap();
    let before = map.before(4).unwrap();
    assert_eq!(before.len(), 4);
    assert!(matches!(
        before[0].kind,
        ScopeKind::SpecialBinding { symbol: 1 }
    ));
    assert!(matches!(before[1].kind, ScopeKind::LexicalEnvironment));
    assert!(
        before[..3]
            .iter()
            .all(|s| s.ownership == Ownership::Inherited)
    );
    assert_eq!(before[3].ownership, Ownership::Local);
    assert_eq!(map.before(5).unwrap().len(), 2);
    assert!(map.before(7).unwrap().is_empty());
}

#[test]
fn lexical_exit_records_binding_and_environment_restoration() {
    use egcl_compiler::control_scope::ScopeKind;
    let code = [
        block(1, 5),
        Instr::BindSpecial(1),
        Instr::PushEnvChild,
        Instr::Const(0),
        Instr::ReturnFrom { block_id: 1 },
        Instr::Return,
    ];
    let map = ScopeMap::analyze(&code).unwrap();
    let removed = &map.exit_at(4).unwrap().removed;
    assert_eq!(removed.len(), 3);
    assert!(matches!(removed[0].kind, ScopeKind::LexicalEnvironment));
    assert!(matches!(
        removed[1].kind,
        ScopeKind::SpecialBinding { symbol: 1 }
    ));
    assert!(ScopeMap::analyze(&[Instr::UnbindSpecial(1)]).is_err());
    assert!(ScopeMap::analyze(&[Instr::PopEnvChild]).is_err());
}
