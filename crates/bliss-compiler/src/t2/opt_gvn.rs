//! P4a — Global value numbering / CSE (spec §4.5).
//!
//! **Parcel P4a. Owner: (sub-agent).** Deduplicate pure instructions computing
//! the same value (same opcode + aux + operand value-numbers), dominance-aware:
//! a redundant pure instruction is replaced by the dominating one. Skip EFFECTFUL
//! instructions. Preserve the deopt invariant (spec §4.10 R4.60): when replacing
//! a value, update FrameState references too. Implement `Pass`; unit-test on
//! hand-built IR.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Gvn;

impl Pass for Gvn {
    fn name(&self) -> &'static str { "gvn" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4a: dominance-aware global value numbering / CSE")
    }
}
