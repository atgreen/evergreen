//! P7 — FrameState metadata lowering & deopt runtime (spec §4.10 A4.14, §4.6 A4.04).
//!
//! **Parcel P7. Owner: (sub-agent).** Lower each deoptimising instruction's
//! `FrameState` (frame_state.rs, frozen) to the compiled `StackMap` form
//! (mach.rs) once P6 has assigned locations: for each interpreter slot resolve
//! its `ValueSource` — `Value` → its allocated `Location` (+ rebox note if the
//! representation is unboxed), `Const` → materialise-immediate, `Unbound` →
//! unbound-marker, `Remat` → cold-path recipe — producing exactly the shape the
//! §4.6 A4.04 deopt handler consumes. Also sketch the resume-state reconstruction
//! (given a StackMap + machine state → interpreter locals/stack). The
//! `FrameScope.function` symbol is resolvable from the bytecode function name via
//! `bliss_rt::symbols`. Unit-test the lowering on a hand-built FrameState +
//! allocation, asserting each slot resolves to the expected descriptor.

use crate::t2::frame_state::FrameState;
use crate::t2::mach::MachFunc;

/// Lower every FrameState referenced by `mf` to `mf.stack_maps` deopt entries
/// (spec §4.10 A4.14). Called after P6 has populated `mf.allocation`.
pub fn lower_frame_states(_mf: &mut MachFunc, _frame_states: &[FrameState]) {
    todo!("P7: FrameState -> StackMap lowering (A4.14)")
}
