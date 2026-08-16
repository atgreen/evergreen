//! P4d — Guard elimination & hoisting (spec §4.5 R4.37, §4.3.6.3, §4.10 R4.63).
//!
//! **Parcel P4d. Owner: (sub-agent).** Remove redundant guards: a guard whose
//! condition is implied by a dominating guard, or proven always-true by type/
//! range inference (call `crate::t2::infer::infer(f)` and read the result, or
//! read refined `ValueData.ty` after inference has been applied). Hoist a
//! loop-invariant guard to the preheader, recording the pre-narrowing
//! `ValueSource` in its FrameState (spec R4.63). Implement `Pass`; unit-test that
//! a dominated duplicate guard and an inference-proven guard are removed while a
//! genuinely-needed guard survives.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct GuardElim;

impl Pass for GuardElim {
    fn name(&self) -> &'static str { "guard-elim" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4d: redundant-guard elimination + hoisting")
    }
}
