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
