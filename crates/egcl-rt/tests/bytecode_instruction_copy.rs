//! Instruction fetch must not require variant-by-variant cloning.
use egcl_rt::bytecode::Instr;

#[test]
fn instructions_are_plain_copyable_metadata() {
    fn require_copy<T: Copy>() {}
    require_copy::<Instr>();
    assert!(!std::mem::needs_drop::<Instr>());

    let code = [Instr::PushBlock {
        block_id: 123,
        name_idx: 45,
        resume_bcp: 678,
        sp_restore: 90,
        register: true,
    }];
    let fetched = code[0];
    for instruction in [fetched, code[0]] {
        assert!(matches!(
            instruction,
            Instr::PushBlock {
                block_id: 123,
                name_idx: 45,
                resume_bcp: 678,
                sp_restore: 90,
                register: true,
            }
        ));
    }
}
