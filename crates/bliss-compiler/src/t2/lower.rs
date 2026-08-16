//! P5 — Lowering / x86-64 instruction selection (spec §4.7 R4.42).
//!
//! **Parcel P5. Owner: (sub-agent).** Lower a block-based SSA `Function` to a
//! `MachFunc` of `MachInst`s over virtual registers (`mach.rs` is the frozen
//! interface). Maximal-munch instruction selection per block; SSA values → VRegs
//! (GPR for tagged/unboxed-int/pointer, XMM for unboxed float — honour each
//! value's `ValueRepresentation`); block parameters and BlockCall args become
//! parallel moves at edges; carry each deoptimising/safepoint inst's
//! `FrameStateId` onto the corresponding `MachInst` so P6 can pin + record it.
//! Encoding bytes are out of scope here (that's a later emit step / reuse the
//! `bliss` crate's assembler); select and sequence MachInsts. Unit-test that a
//! small `Function` lowers to a plausible MachInst sequence with correct VReg
//! classes and preserved frame-state annotations.

use crate::t2::ir::Function;
use crate::t2::mach::MachFunc;

/// Lower `f` to machine instructions over virtual registers (spec §4.7).
pub fn lower(_f: &Function) -> MachFunc {
    todo!("P5: instruction selection IR -> MachFunc")
}
