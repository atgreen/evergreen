//! P1 — Bytecode → block-based SSA builder (spec §4.3.5).
//!
//! **Parcel P1. Owner: (sub-agent).** Implement `build_from_bytecode` against the
//! frozen `ir`/`frame_state` contracts. Abstractly interpret the bytecode: model
//! the operand stack as a compile-time stack of `Value`s, turn local slots into
//! Braun `read_var`/`write_var` variables, start a block at every jump target and
//! fall-through successor, and anchor each instruction to its `bcp`. Every
//! deoptimising instruction gets a `FrameState`. Unit-test with small
//! hand-written `BytecodeFunction`s and assert the built IR verifies (P2).

use crate::t2::ir::Function;
use bliss_rt::bytecode::BytecodeFunction;

/// Why the builder could not produce IR for a function (e.g. an opcode not yet
/// modelled). The caller keeps such a function at T1 (spec R4.28).
#[derive(Clone, Debug)]
pub enum BuildError {
    Unsupported(&'static str),
}

/// Build a block-based SSA `Function` from `bf` (spec §4.3.5, R4.19).
pub fn build_from_bytecode(_bf: &BytecodeFunction) -> Result<Function, BuildError> {
    todo!("P1: bytecode → SSA construction (Braun algorithm, operand-stack modelling)")
}
