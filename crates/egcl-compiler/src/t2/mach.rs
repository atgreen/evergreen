// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 machine-level IR — the lowering/regalloc interface (spec §4.7).
//!
//! **Phase-0 foundation / frozen contract.** Lowering (P5) turns the block-based
//! SSA `Function` into a `MachFunc` of `MachInst`s over virtual registers;
//! register allocation (P6) assigns each virtual register a [`Location`] and,
//! per spec §4.10 R4.65, finalises the GC stack map and every FrameState
//! location binding. `regalloc2` is the intended allocator (spec §4.7 R4.45);
//! this module defines only the EGCL-side types the P6 adapter maps to/from it.

use crate::t2::frame_state::FrameStateId;

/// Register class. Tagged values, unboxed integers, and pointers live in GPRs;
/// unboxed floats live in the target's floating-point registers (spec §4.7
/// R4.45). `Xmm` is the historical name of the floating-point class; on System Z
/// it refers to scalar FPRs.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum RegClass {
    Gpr,
    Xmm,
}

/// A physical register. `encoding` is backend-specific: an abstract index for
/// x86, or the architectural register number for System Z.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct PhysReg {
    pub class: RegClass,
    pub encoding: u8,
}

/// A spill stack slot (frame-pointer-relative), assigned by regalloc.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct StackSlot(pub u32);

/// A pre-allocation virtual register produced by lowering (P5).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct VReg {
    pub class: RegClass,
    pub num: u32,
}

/// The post-allocation location of a value — where the register allocator (P6)
/// bound it. This is what a FrameState `ValueSource::Value` resolves to in the
/// emitted metadata (spec §4.10 A4.14).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Location {
    Register(PhysReg),
    Stack(StackSlot),
}

/// Where an allocator-inserted move occurs relative to a machine instruction.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EditPosition {
    Before,
    After,
}

/// A move inserted by register allocation.  These edits are the authoritative
/// spill/reload and live-range-splitting operations; reducing regalloc2's output
/// to one location per VReg loses precisely this information.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AllocationEdit {
    pub inst: usize,
    pub position: EditPosition,
    pub from: Location,
    pub to: Location,
}

/// A precise location interval for one VReg, reported by regalloc2's debug
/// location facility. Program points use regalloc2's stable raw encoding.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ValueLocationRange {
    pub vreg: VReg,
    pub start: u32,
    pub end: u32,
    pub location: Location,
}

/// A machine instruction over virtual (pre-regalloc) or physical (post-regalloc)
/// operands. The concrete operand model and opcode set are P5's to define; this
/// is the frozen envelope P6 and code emission bind to.
#[derive(Clone, Debug)]
pub struct MachInst {
    /// SSA instruction selected into this machine instruction. Allocator edits
    /// and operand locations can therefore be related back to FrameStates,
    /// deopt guards, and the live framed emitter without re-deriving order.
    pub source_inst: Option<crate::t2::ir::Inst>,
    /// Backend mnemonic / selector tag (P5-defined encoding).
    pub op: u32,
    pub defs: Vec<VReg>,
    pub uses: Vec<VReg>,
    /// Immediate operand, when the mnemonic takes one (e.g. `MOV_IMM`, shift
    /// counts). `None` for register-only instructions. Populated by lowering
    /// (P5) from the IR constant's `AuxData`; consumed by the emitter.
    pub imm: Option<i64>,
    /// If this instruction is a safepoint or may deopt, the frame state whose
    /// locations the allocator must pin and record (spec §4.10 R4.65).
    pub frame_state: Option<FrameStateId>,
    /// The frame state's register-sourced SSA values, as extra regalloc USES
    /// (bliss-ad1e): a value live only because a deopt may reconstruct it is
    /// otherwise invisible to regalloc2, so its register could be freed or
    /// reused before the guard and the deopt would read a stale slot. Deduped
    /// against `defs`/`uses`; fed to the allocator as Any-constraint late
    /// uses, so they can live in spill slots and survive the instruction's own
    /// writes (an in-place result reuse followed by a deopting `jo`).
    pub deopt_uses: Vec<VReg>,
    /// True if this is a GC safepoint (needs a stack map, spec §4.7 R4.46).
    pub safepoint: bool,
}

/// A machine-level basic block (spec §4.7): the CFG structure regalloc2 (P6) and
/// code emission need. `insts` is the `[start, end)` half-open range into
/// `MachFunc::insts` forming this block's body (ending in a branch/return);
/// `params` are the block's parameter VRegs (regalloc2's φ replacement); `succs`
/// are the outgoing edges with the VReg args bound to each target's params.
#[derive(Clone, Debug)]
pub struct MachBlock {
    pub params: Vec<VReg>,
    pub start: usize,
    pub end: usize,
    pub succs: Vec<MachSucc>,
}

/// A machine-block handle (index into `MachFunc::blocks`; block 0 is the entry).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct MachBlockId(pub u32);

/// One CFG edge: a successor block plus the VReg arguments bound to that block's
/// parameters on this edge (parallel-move / φ semantics).
#[derive(Clone, Debug)]
pub struct MachSucc {
    pub target: MachBlockId,
    pub args: Vec<VReg>,
}

/// A lowered function: machine instructions plus the block CFG over them and the
/// register-allocation result once P6 has run.
#[derive(Clone, Debug, Default)]
pub struct MachFunc {
    pub insts: Vec<MachInst>,
    /// The block CFG over `insts`; `blocks[0]` is the entry. Populated by P5
    /// lowering. Empty means "not yet block-structured" (a straight-line
    /// single-block view over `insts` is the fallback P6 uses in that case).
    pub blocks: Vec<MachBlock>,
    /// Virtual-register → assigned location, filled in by P6.
    ///
    /// This is retained as a summary for diagnostics and legacy deopt tests.
    /// Emission MUST use `inst_allocations` plus `allocation_edits`, because a
    /// split live range can occupy different locations at different uses.
    pub allocation: Vec<(VReg, Location)>,
    /// Operand-parallel locations for each `MachInst` (defs first, then uses).
    pub inst_allocations: Vec<Vec<Location>>,
    /// Ordered moves inserted by regalloc2 for spills, reloads, and splits.
    pub allocation_edits: Vec<AllocationEdit>,
    /// Number of eight-byte spill slots required by this function.
    pub num_spill_slots: u32,
    /// Split-aware location ranges used by deopt/OSR metadata and by the framed
    /// backend while it is migrated off its old global register map.
    pub value_locations: Vec<ValueLocationRange>,
    /// Per-safepoint GC stack maps, filled in by P6 (spec §4.7 R4.46).
    pub stack_maps: Vec<StackMap>,
}

/// A GC stack map at one safepoint: which locations hold live tagged pointers,
/// plus the resolved FrameState location bindings for deopt (spec §4.6, §4.10).
#[derive(Clone, Debug)]
pub struct StackMap {
    /// Native code offset of the safepoint / trap.
    pub code_offset: u32,
    /// Locations holding live GC references at this point.
    pub live_refs: Vec<Location>,
    /// The deopt frame state resolved to concrete locations, if this is a
    /// deopt point (spec §4.10 A4.14).
    pub frame_state: Option<FrameStateId>,
}
