//! P4b — Loop-invariant code motion (spec §4.5 R4.34).
//!
//! **Parcel P4b. Owner: (sub-agent).** Detect natural loops (via the dominator
//! tree / back-edges — extend `pass::LoopForest::compute`, but keep that logic in
//! THIS file if it needs a body the frozen stub lacks; do not edit pass.rs) and
//! hoist pure, loop-invariant instructions into the loop preheader. MUST NOT
//! hoist effectful instructions or anything that could change deopt behaviour.
//! Implement `Pass`; unit-test on a hand-built counted loop.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Licm;

impl Pass for Licm {
    fn name(&self) -> &'static str { "licm" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4b: loop-invariant code motion")
    }
}
