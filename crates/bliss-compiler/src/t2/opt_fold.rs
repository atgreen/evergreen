//! P4f — Constant folding + strength reduction (spec §4.5 R4.36).
//!
//! **Parcel P4f. Owner: (sub-agent).** Fold instructions with constant operands
//! (`FixnumAdd(ConstFixnum a, ConstFixnum b)` → `ConstFixnum a+b`, comparisons,
//! logic; respect fixnum overflow — a fold that would overflow becomes the
//! generic/guarded form or is left alone), and strength-reduce known-expensive
//! patterns (`FixnumMul` by a power of two → `FixnumShl`; `x*1`→`x`; `x+0`→`x`;
//! division by constant; etc.). Update uses (instruction operands, block-call
//! args, FrameState value sources) when replacing a value (spec §4.10 R4.60).
//! Implement `Pass`; unit-test folding and at least one strength reduction.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct ConstFold;

impl Pass for ConstFold {
    fn name(&self) -> &'static str { "const-fold" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4f: constant folding + strength reduction")
    }
}
