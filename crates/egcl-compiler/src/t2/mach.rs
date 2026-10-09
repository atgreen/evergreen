// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 machine-level IR: the data contract between lowering, register
//! allocation, and code emission.
//!
//! # Purpose
//!
//! This module defines only types. Lowering (lower.rs) produces a [`MachFunc`]
//! of [`MachInst`]s over virtual registers with a block CFG; regalloc.rs fills
//! in the allocation tables and GC stack maps; the emitters read the result.
//! No behaviour lives here, so each stage can be tested against hand-built
//! values of these types.
//!
//! # Contract
//!
//! **Produced by lowering:**
//!
//! * `MachFunc::insts` — flat instruction vector in layout order. Each
//!   [`MachInst`] names its `defs`, `uses`, and `deopt_uses` as [`VReg`]s, an
//!   opaque `op` mnemonic (the `lower::op` constants), an optional immediate,
//!   the originating SSA instruction, and its `frame_state` / `safepoint`
//!   annotations.
//! * `MachFunc::blocks` — one [`MachBlock`] per IR block, `blocks[0]` the entry,
//!   each a `[start, end)` range into `insts` ending in its terminator, with
//!   block-parameter VRegs and [`MachSucc`] edges carrying the VReg args bound
//!   to the target's params. Empty `blocks` means a straight-line function and
//!   the allocator treats all of `insts` as one block.
//!
//! **Filled by register allocation**, all indexed consistently with `insts`:
//!
//! * `inst_allocations[i]` — a [`Location`] per operand of instruction `i` in
//!   the order defs, uses, deopt_uses. The table emitters read.
//! * `allocation_edits` — allocator-inserted moves ([`AllocationEdit`]), each
//!   pinned `Before` or `After` an instruction. These are the spills, reloads,
//!   and split-resolution moves; an emitter that replays `inst_allocations`
//!   must also replay these or a split value is read from the wrong place.
//! * `value_locations` — [`ValueLocationRange`]s giving, per VReg, which
//!   location holds it over which program-point interval. Program points use
//!   regalloc2's encoding (`inst * 2 + pos`), not instruction indices. The
//!   framed emitters derive both stable value homes and safepoint liveness from
//!   these.
//! * `num_spill_slots` — eight-byte slots the prologue reserves.
//! * `allocation` — one `(VReg, Location)` per VReg, the def-site location.
//!   Diagnostics and tests only; wrong for any split value.
//! * `stack_maps` — one [`StackMap`] per `safepoint` instruction, whose
//!   `code_offset` is the **instruction index** (native offsets do not exist
//!   until emission), `values` its typed live values before the instruction,
//!   and `frame_state` its deopt state. `live_refs()` selects tagged candidates.
//!   Late deopt operands are a separate phase. Emitters must resolve final
//!   homes and PCs before using these records for GC or DWARF publication.
//!
//! # Register and location model
//!
//! Two [`RegClass`]es: `Gpr` for tagged values, unboxed integers and pointers;
//! `Xmm` for unboxed floats (the name is historical; on System Z, AArch64 and
//! POWER it denotes the scalar FP register file). A [`PhysReg`] encoding is
//! backend-specific: x86 uses an abstract index that `x64_frame::GPR_X86` maps
//! to hardware numbers, the other backends use architectural numbers directly.
//! A [`StackSlot`] is a frame-relative eight-byte spill slot numbered by the
//! allocator; the emitter decides its address.
//!
//! `deopt_uses` exist because a value referenced only by a FrameState has no
//! ordinary use after its definition and would be dead at the guard. Listing it
//! as a late-position, `Any`-constrained use keeps it locatable past the
//! instruction's own writes without adding register pressure (bliss-ad1e).

use crate::t2::frame_state::FrameStateId;
use crate::t2::ir::ValueRepresentation;
use std::collections::{HashMap, HashSet};

/// Register class. Tagged values, unboxed integers, and pointers live in GPRs;
/// unboxed floats live in the target's floating-point registers. `Xmm` is the
/// historical name of the floating-point class; on System Z, AArch64 and POWER
/// it refers to the scalar FP register file.
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

/// A pre-allocation virtual register produced by lowering.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct VReg {
    pub class: RegClass,
    pub num: u32,
}

/// The post-allocation location of a value — where the register allocator
/// bound it. This is what a FrameState `ValueSource::Value` resolves to in the
/// emitted deopt metadata.
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

impl ValueLocationRange {
    pub fn contains(&self, point: u32) -> bool {
        self.start <= point && point < self.end
    }
}

/// A machine instruction over virtual (pre-regalloc) or physical (post-regalloc)
/// operands. The opcode set is defined by `lower::op`; this is the envelope the
/// allocator and the emitters bind to.
#[derive(Clone, Debug)]
pub struct MachInst {
    /// SSA instruction selected into this machine instruction. Allocator edits
    /// and operand locations can therefore be related back to FrameStates,
    /// deopt guards, and the live framed emitter without re-deriving order.
    pub source_inst: Option<crate::t2::ir::Inst>,
    /// Backend mnemonic / selector tag (a `lower::op` constant).
    pub op: u32,
    pub defs: Vec<VReg>,
    pub uses: Vec<VReg>,
    /// Immediate operand, when the mnemonic takes one (e.g. `MOV_IMM`, shift
    /// counts). `None` for register-only instructions. Populated by lowering
    /// from the IR constant's `AuxData`; consumed by the emitter.
    pub imm: Option<i64>,
    /// If this instruction is a safepoint or may deopt, the frame state whose
    /// locations the allocator must keep locatable and record.
    pub frame_state: Option<FrameStateId>,
    /// The frame state's register-sourced SSA values, as extra regalloc USES
    /// (bliss-ad1e): a value live only because a deopt may reconstruct it is
    /// otherwise invisible to regalloc2, so its register could be freed or
    /// reused before the guard and the deopt would read a stale slot. Deduped
    /// against `defs`/`uses`; fed to the allocator as Any-constraint late
    /// uses, so they can live in spill slots and survive the instruction's own
    /// writes (an in-place result reuse followed by a deopting `jo`).
    pub deopt_uses: Vec<VReg>,
    /// True if this is a GC safepoint (the allocator emits a stack map for it).
    pub safepoint: bool,
}

/// A machine-level basic block: the CFG structure regalloc2 and code emission
/// need. `insts` is the `[start, end)` half-open range into
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
    /// Explicit machine representation for every VReg. GPR class alone cannot
    /// distinguish a tagged reference candidate from an unboxed integer.
    pub value_reprs: HashMap<VReg, ValueRepresentation>,
    /// The block CFG over `insts`; `blocks[0]` is the entry. Populated by
    /// lowering. Empty means "not block-structured" (a straight-line
    /// single-block view over `insts` is the allocator's fallback in that case).
    pub blocks: Vec<MachBlock>,
    /// Virtual-register → assigned location, filled in by the allocator.
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
    /// Per-safepoint GC stack maps, filled in by the allocator.
    pub stack_maps: Vec<StackMap>,
}

/// Typed value locations before one safepoint instruction, after Before edits.
/// GC candidates are the tagged subset; deopt retains separate late operands.
#[derive(Clone, Debug)]
pub struct StackMap {
    /// Index of the safepoint instruction in `MachFunc::insts`. Native offsets
    /// are not known until emission; consumers match maps to instructions by
    /// this index.
    pub code_offset: u32,
    /// Values live immediately before the instruction, after its Before edits.
    /// Includes dead-after-call arguments, excludes late result definitions.
    /// These are allocator locations; emitters must resolve their final homes
    /// and native PCs before publication to GC or DWARF consumers.
    pub values: Vec<AllocatedValue>,
    /// The deopt frame state resolved to concrete locations, if this is a
    /// deopt point.
    pub frame_state: Option<FrameStateId>,
}

/// One typed SSA value at a physical allocator location. Multiple locations for
/// one value represent copies that are simultaneously available at this point.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocatedValue {
    pub vreg: VReg,
    pub repr: ValueRepresentation,
    pub location: Location,
}

impl StackMap {
    /// Tagged words are reference candidates; the runtime still checks their
    /// EgclVal tags. Unboxed integers and floats must never be scanned as roots.
    pub fn live_refs(&self) -> impl Iterator<Item = &AllocatedValue> {
        self.values
            .iter()
            .filter(|value| value.repr == ValueRepresentation::Tagged)
    }
}

impl MachFunc {
    /// Values read by execution, recovery or an outgoing edge. Allocator debug
    /// ranges also include definition-only intervals for unused block params;
    /// those intervals do not make a value live at a safepoint.
    pub(crate) fn read_vregs(&self) -> HashSet<VReg> {
        self.insts
            .iter()
            .flat_map(|inst| inst.uses.iter().chain(&inst.deopt_uses))
            .chain(
                self.blocks
                    .iter()
                    .flat_map(|block| block.succs.iter().flat_map(|edge| &edge.args)),
            )
            .copied()
            .collect()
    }

    /// Value identities at Before(inst), after allocator Before edits. The
    /// same query serves ordinary safepoints and polls inserted after allocation.
    pub(crate) fn live_vregs_before(
        &self,
        inst: usize,
        read_vregs: &HashSet<VReg>,
    ) -> Option<HashSet<VReg>> {
        let instruction = self.insts.get(inst)?;
        let point = u32::try_from(inst).ok()?.checked_mul(2)?;
        Some(
            self.value_locations
                .iter()
                .filter(|range| range.contains(point) && read_vregs.contains(&range.vreg))
                .map(|range| range.vreg)
                .chain(instruction.uses.iter().copied())
                .collect(),
        )
    }

    /// Allocator homes at a precise instruction phase. These are not yet native
    /// PCs or final emitter homes; both GC and debug emission must resolve them.
    pub fn locations_at(
        &self,
        vreg: VReg,
        inst: usize,
        position: EditPosition,
    ) -> impl Iterator<Item = Location> + '_ {
        let point = inst as u32 * 2 + u32::from(position == EditPosition::After);
        self.value_locations
            .iter()
            .filter(move |range| range.vreg == vreg && range.contains(point))
            .map(|range| range.location)
    }
}
