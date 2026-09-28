use torcl_compiler::control_scope::{Ownership, ScopeError, ScopeMap};
use torcl_rt::bytecode::Instr;

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
    use torcl_compiler::control_scope::ScopeKind;
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
    use torcl_compiler::control_scope::ScopeKind;
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
    use torcl_compiler::control_scope::ScopeKind;
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
