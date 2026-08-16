//! P6 — Register allocation via regalloc2 (spec §4.7 R4.45, §4.10 R4.65).
//!
//! **Parcel P6. Owner: (sub-agent).** Adapt `MachFunc` (mach.rs, frozen) to the
//! `regalloc2` crate: implement its `Function`/environment trait over our
//! MachInsts (two register classes: Int/GPR, Float/XMM; fixed-register ABI +
//! c2i constraints; safepoints marked so reftype operands are tracked), run the
//! allocator, and write the results back: fill `MachFunc::allocation`
//! (VReg → Location) and `MachFunc::stack_maps` (one per safepoint, with live GC
//! refs and the resolved FrameState location bindings — spec §4.10 A4.14/R4.65).
//! Unit-test on a small hand-built MachFunc (a few VRegs, one safepoint) that
//! every VReg gets a Location and the safepoint gets a stack map.

use crate::t2::mach::MachFunc;

/// Allocate registers for `mf` in place (spec §4.7 R4.45).
pub fn allocate(_mf: &mut MachFunc) {
    todo!("P6: regalloc2 adapter + stack-map / frame-state binding emission")
}
