//! P4e — Escape analysis + scalar replacement / stack allocation (spec §4.5 R4.33).
//!
//! **Parcel P4e. Owner: (sub-agent).** Identify heap allocations (`Alloc`,
//! `AllocCons`) whose result does not escape its allocating function — not stored
//! into another heap object, not returned, not passed to a call, not captured by
//! a closure — and mark them for stack allocation / scalar replacement (replace
//! field loads/stores with SSA values where the object is fully analysable).
//! Preserve the deopt invariant (a scalar-replaced object may still be named by a
//! FrameState — rematerialise it there, spec §4.10). Implement `Pass`; unit-test
//! that a non-escaping cons is flagged and an escaping one is not.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct EscapeAnalysis;

impl Pass for EscapeAnalysis {
    fn name(&self) -> &'static str { "escape-analysis" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4e: escape analysis + scalar replacement / stack allocation")
    }
}
