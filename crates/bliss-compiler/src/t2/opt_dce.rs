//! P4c — Deopt-aware dead-code elimination + rematerialisation (spec §4.5 R4.35,
//! §4.10 A4.13).
//!
//! **Parcel P4c. Owner: (sub-agent).** Mark liveness from NON-deopt uses only
//! (real fast-path / effect uses); FrameState uses do NOT seed liveness. Then for
//! each unmarked value: delete if it has no FrameState uses; else if it is
//! rematerialisable (pure/total/cheap — see `frame_state::RematOp`) build a
//! `RematRecipe`, replace its FrameState uses with `ValueSource::Remat`, and
//! remove it from the fast path; else keep it (deopt-live). Never break the
//! preservation invariant (spec §4.10 R4.60). Implement `Pass`; unit-test that a
//! value used only by a FrameState is rematerialised, not kept live.

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str { "dce" }
    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P4c: deopt-aware DCE + rematerialisation (A4.13)")
    }
}
