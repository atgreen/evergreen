// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 register allocation: the `regalloc2` adapter and post-allocation
//! stack-map construction.
//!
//! # Purpose
//!
//! Lowering (lower.rs) emits a [`MachFunc`] over virtual registers, one per SSA
//! value. This module assigns every VReg a physical register or spill slot for
//! each point in its live range, records the spill/reload/shuffle moves that
//! assignment implies, and produces the per-safepoint GC stack maps that the
//! deopt and transfer-site machinery key on. It is the only place T2 runs an
//! allocator; every backend emitter consumes its output tables and never
//! re-derives locations.
//!
//! The allocator itself is `regalloc2` (Ion algorithm: backtracking with
//! live-range splitting and spill-slot coalescing). T2 needs an SSA allocator
//! with live-range splitting, two register classes, fixed-register constraints
//! for the ABI and the c2i convention, and a location for every deopt-live
//! value at every safepoint; regalloc2 provides all of that, so we adopted it
//! rather than write a linear-scan allocator. What this file owns is the adapter:
//! expressing a `MachFunc` through regalloc2's `Function` trait, choosing the
//! `MachineEnv` per target, and translating the `Output` back into
//! `MachFunc` fields.
//!
//! # Contract
//!
//! **Input:** `mf.insts` (defs / uses / `deopt_uses` per instruction, each a
//! `VReg` of class Gpr or Xmm), `mf.blocks` (the machine CFG with block params
//! and edge args), and the per-instruction `safepoint` / `frame_state` marks.
//! Lowering guarantees SSA form (one def per VReg, block params at merges) and
//! no critical edges.
//!
//! **Output**, written in place:
//!
//! * `inst_allocations[i]` — a `Location` per operand of instruction `i`, in
//!   the order defs, uses, deopt_uses. This is what emitters index.
//! * `allocation_edits` — regalloc2's inserted moves (spill, reload, split
//!   resolution, edge reconciliation), each tagged with the instruction and
//!   `Before`/`After` position. Emitters must materialise these; the operand
//!   table alone is incomplete for any split value.
//! * `value_locations` — per-VReg `[start, end)` location ranges in regalloc2's
//!   program-point encoding, obtained via its debug-label facility. The framed
//!   emitters use these to decide which values keep a single home
//!   (`select_frame_homes`, x64_frame.rs) and to resolve deopt metadata.
//! * `num_spill_slots` — slots the prologue must reserve.
//! * `allocation` — one `(VReg, Location)` per VReg, preferring the def site.
//!   A summary for diagnostics and tests only; it is wrong for split values.
//! * `stack_maps` — one per `safepoint` instruction; see below.
//!
//! Failure is a `RegAllocError`, which aborts the T2 compile and leaves the
//! function at its current tier. Nothing partial is installed.
//!
//! # Pipeline
//!
//! 1. **Target environment.** Each backend entry point ([`allocate`],
//!    [`allocate_framed`], [`allocate_framed_a64`], [`allocate_framed_ppc64le`],
//!    [`allocate_framed_s390x`]) builds a `MachineEnv` (allocatable set per
//!    class plus one reserved edit-scratch per class) and a call-clobber
//!    `PRegSet`. x86 uses abstract encodings that emit.rs maps to hardware
//!    registers; the other targets use architectural numbers directly. The
//!    rationale for each pool (ABI reservations, emitter temporaries, the x86
//!    reduced-pool-first retry) is on the respective function.
//! 2. **Adapter construction** (`Adapter::build`): intern VRegs densely, build
//!    operand lists, CFG tables, terminator flags, and per-instruction clobber
//!    sets. The adapter owns its tables so the `MachFunc` is not borrowed
//!    during the run.
//! 3. **Pre-check for impossible demand.** If one instruction requires more
//!    `Reg`-constrained operands of a class in one phase than the pool holds,
//!    return `TooManyLiveRegs` ourselves. regalloc2 0.15.2 underflows
//!    `ProgPoint::prev` while diagnosing this at instruction zero; reporting
//!    it first lets `allocate_framed` retry with its full pool normally.
//! 4. **Run** `regalloc2::run` with `Algorithm::Ion` and `validate_ssa: false`
//!    (lowering already guarantees SSA; the validator rejects some
//!    hand-built test functions).
//! 5. **Write-back** of the tables above, converting `Allocation` to
//!    [`Location`] (`PReg` hw_enc → `PhysReg`, `SpillSlot` index → `StackSlot`).
//! 6. **Stack maps**, computed from the output rather than by regalloc2.
//!
//! # Mapping MachFunc → regalloc2
//!
//! * **Register classes.** [`RegClass::Gpr`] → `Int` (tagged values, unboxed
//!   integers, pointers); [`RegClass::Xmm`] → `Float` (unboxed floats).
//!   `Vector` is unused.
//! * **VRegs.** `MachFunc` VRegs are `(class, num)` and may share `num` across
//!   classes; regalloc2 wants one dense 0-based index space. Every distinct
//!   VReg is interned in first-encounter order and the reverse map is kept
//!   for write-back.
//! * **Operands.** `defs` → `Operand::reg_def` (late, register-constrained);
//!   `uses` → `Operand::reg_use` (early, register-constrained). Defs are
//!   emitted before uses so `Output::inst_allocs`, which is operand-parallel,
//!   lines up with `inst_allocations`.
//! * **`Any`-constrained operands.** Three cases relax `Reg` to `Any` so the
//!   allocator may leave the value in a spill slot: the uses of `INVOKE`, and
//!   of `CALL` on targets with `stack_call_operands`, because those emitters
//!   copy arguments from their homes into a scanned slice one at a time and a
//!   simultaneous register requirement would make any call wider than the
//!   bank unallocatable; the multiple defs of `CALL_RUNTIME` on those same
//!   targets, for the symmetric reason; and every `deopt_uses` entry.
//! * **`deopt_uses` as liveness extension** (bliss-ad1e). A deoptimising
//!   instruction's `FrameState` names values the interpreter frame is rebuilt
//!   from; some have no ordinary use after their def and would otherwise be
//!   dead at the guard. Each is added as an `Any` use at the `Late` position:
//!   late so it survives the instruction's own writes (in-place result reuse
//!   followed by a deopting `jo` must still find the original), `Any` so it
//!   adds no register pressure. This is how "every FrameState value has a
//!   Location at its deopt point" is enforced by construction rather than
//!   checked after.
//! * **Clobbers.** `CALL`, `INVOKE`, `CALL_RUNTIME`, `ALLOC`, `TAILCALL`,
//!   `THROW`, and `NLX_TRANSFER` report the target's caller-saved set via
//!   `inst_clobbers`, so values live across them land in callee-saved
//!   registers or are spilled around the call.
//!
//! ## Block CFG
//!
//! `mf.blocks` is a `Vec<MachBlock>`: `blocks[0]` is the entry, each block is
//! an `[start, end)` range into `mf.insts`, and edges are `MachSucc` records
//! (target + the VReg args bound to that target's params). The mapping is
//! direct:
//!
//! * `block_insns(b)` = `[start, end)`.
//! * `block_params` = `MachBlock.params`, regalloc2's φ replacement. An edge
//!   value is *defined* by being a block param, so it is live-in by
//!   construction; this is what makes loop back-edges into header params
//!   allocate correctly.
//! * `block_succs` from `MachSucc.target`; `block_preds` is the inverted edge
//!   set, computed once.
//! * `branch_blockparams(b, _, succ_idx)` = the interned `MachSucc.args` of
//!   that successor; their count matches the successor's param count.
//! * The terminator is `end - 1`: `is_branch` if the block has successors,
//!   `is_ret` otherwise.
//!
//! regalloc2 rejects unsplit critical edges (`CritEdge`) because it has
//! nowhere to place the reconciling moves; splitting them is lowering's
//! contract and nothing here re-checks it. A branch into a multi-predecessor
//! block must also carry no register operands of its own (`DisallowedBranchArg`);
//! our terminators are pure branches, with edge args travelling only via
//! `MachSucc.args`.
//!
//! If `mf.blocks` is empty (hand-built straight-line functions, several unit
//! tests), the adapter treats all of `mf.insts` as one block with the last
//! instruction as the return.
//!
//! # Stack maps
//!
//! regalloc2 0.15.2 exposes no safepoint or reftype support on the public
//! `Function` trait (`requires_refs_for_safepoint`, `reftype_vregs`,
//! `is_safepoint` exist only in its private `fuzzing` module), so the maps are
//! built after allocation: for each instruction with `safepoint == true`, a
//! [`StackMap`] whose `live_refs` are the `Location`s of that instruction's
//! Gpr-class uses (ordinary and `deopt_uses` alike) and whose `frame_state` is
//! the instruction's `FrameStateId`. `code_offset` carries the **instruction
//! index**, not a byte offset; transfer_map.rs and deopt.rs match maps to
//! instructions by that index, and native offsets only exist after emission.
//!
//! Two known approximations:
//!
//! * Only operands of the safepoint instruction itself are recorded; a value
//!   live across the safepoint but not named by it is absent.
//! * There are no per-VReg reftype flags on `MachInst`, so every Gpr-class
//!   use is reported, unboxed integers included.
//!
//! Both are acceptable because no backend hands `live_refs` to the collector.
//! All framed emitters take the boundary-synchronisation route instead: before
//! each runtime call the exact live tagged set is synchronised into shadow
//! slots on the `EgclStack` activation, which the GC scans, and reloaded
//! afterwards. The maps here serve as the deopt anchor (deopt.rs lowers each
//! `FrameState` by walking them) and as a cross-check (transfer_map.rs refuses
//! a transfer site whose FrameState roots are not all in `live_refs`). A
//! backend that wants to publish register roots directly needs reftype flags
//! and cross-instruction liveness here first.
//!
//! # Debugging
//!
//! * `EGCL_RA_DBG=1` — on allocation failure, dump every instruction (op,
//!   defs, uses, deopt uses, frame state, safepoint) and every block to
//!   stderr alongside the `RegAllocError`.
//! * `EGCL_T2_FRAME_ENV=full|reduced` — pin the x86 framed allocator to one
//!   pool instead of reduced-first with a full-pool retry on
//!   `TooManyLiveRegs` (see [`allocate_framed`]).

use std::collections::HashMap;

use regalloc2::{
    Algorithm, Allocation, Block, Edit, Function as Ra2Function, Inst as Ra2Inst, InstPosition,
    InstRange, MachineEnv, Operand, OperandConstraint, OperandKind, OperandPos, PReg, PRegSet,
    RegAllocError, RegClass as Ra2RegClass, RegallocOptions, VReg as Ra2VReg,
};

use crate::t2::mach::{
    AllocationEdit, EditPosition, Location, MachFunc, PhysReg, RegClass, StackMap, StackSlot, VReg,
    ValueLocationRange,
};

/// Number of allocatable GPRs exposed to the allocator (hw_enc `0..N_GPR`); one
/// extra encoding above this is reserved as the class scratch register.
// Keep the final abstract encoding out of the allocatable set: the emitter maps
// it to r15 and uses it as regalloc2's dedicated edit scratch register.
const N_GPR: usize = 13;
/// Number of allocatable XMM registers exposed (hw_enc `0..N_XMM`); one extra
/// encoding above this is reserved as the class scratch register.
const N_XMM: usize = 15;

/// Map our register class to regalloc2's.
fn ra2_class(class: RegClass) -> Ra2RegClass {
    match class {
        RegClass::Gpr => Ra2RegClass::Int,
        RegClass::Xmm => Ra2RegClass::Float,
    }
}

/// Map a regalloc2 class back to ours (`Vector` is unused; folded into Gpr).
fn our_class(class: Ra2RegClass) -> RegClass {
    match class {
        Ra2RegClass::Int => RegClass::Gpr,
        Ra2RegClass::Float => RegClass::Xmm,
        Ra2RegClass::Vector => RegClass::Gpr,
    }
}

/// Convert a regalloc2 `Allocation` to our [`Location`] (`None` for an unset
/// allocation, which should not occur for a constrained operand).
fn to_location(a: Allocation) -> Option<Location> {
    if let Some(preg) = a.as_reg() {
        Some(Location::Register(PhysReg {
            class: our_class(preg.class()),
            encoding: preg.hw_enc() as u8,
        }))
    } else {
        a.as_stack()
            .map(|slot| Location::Stack(StackSlot(slot.index() as u32)))
    }
}

/// Build the machine environment: `N_GPR` allocatable Int registers plus one
/// scratch, `N_XMM` allocatable Float registers plus one scratch.
fn machine_env() -> MachineEnv {
    let mut int_regs = PRegSet::empty();
    for i in 0..N_GPR {
        int_regs.add(PReg::new(i, Ra2RegClass::Int));
    }
    let mut float_regs = PRegSet::empty();
    for i in 0..N_XMM {
        float_regs.add(PReg::new(i, Ra2RegClass::Float));
    }

    MachineEnv {
        // All allocatable registers are "preferred"; leaving the non-preferred
        // tier empty is valid and keeps the single-block model simple.
        preferred_regs_by_class: [int_regs, float_regs, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(), PRegSet::empty(), PRegSet::empty()],
        // One dedicated scratch per class, one encoding above the allocatable
        // range so it never overlaps a preferred/non-preferred register.
        scratch_by_class: [
            Some(PReg::new(N_GPR, Ra2RegClass::Int)),
            Some(PReg::new(N_XMM, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    }
}

/// Environment used by the live framed emitter.  Registers consumed by its
/// ABI and instruction templates (rax, rdx, rsi, rdi) are excluded, as are
/// r9-r11 which form the reload/result-temporary bank.  rcx/r8 are profitable
/// for call-local ranges; rbx/r12-r15 survive runtime calls.
fn framed_machine_env(reduced: bool) -> MachineEnv {
    // bliss-x5y.29: the REDUCED pool {rcx, r8, rsi, rbx} (abstract 1, 5, 3,
    // 9) carries a single callee-saved register, so a small hot function's
    // activation pays ONE push instead of three — measured ~12% fewer cycles
    // on tak, neutral on fib. A fully caller-saved pool is rejected outright
    // by regalloc2 (a call's result def may not land in a clobbered
    // register: TooManyLiveRegs), and dropping rsi measures WORSE than the
    // full pool (three GPRs is genuine pressure), so one callee-saved
    // register is the sweet spot. The FULL pool adds r12–r15 for
    // pressure-heavy functions (see allocate_framed's retry). Not offered
    // anywhere: rax/rdx (emitter scratch), r9–r11 (the framed emitter's
    // operand temps / regalloc2 edit scratch), and rdi (frame pointer while
    // the interpreter entry loads parameter homes — a parameter homed there
    // would clobber the base mid-load).
    let int_pool: &[usize] = if reduced {
        &[1, 5, 3, 9]
    } else {
        &[1, 5, 3, 9, 10, 11, 12, 13]
    };
    let mut int_regs = PRegSet::empty();
    for &i in int_pool {
        int_regs.add(PReg::new(i, Ra2RegClass::Int));
    }
    let mut float_regs = PRegSet::empty();
    for i in 0..N_XMM {
        float_regs.add(PReg::new(i, Ra2RegClass::Float));
    }
    MachineEnv {
        preferred_regs_by_class: [int_regs, float_regs, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(), PRegSet::empty(), PRegSet::empty()],
        // abstract GPR 8 maps to r11, reserved from the set above
        scratch_by_class: [
            Some(PReg::new(8, Ra2RegClass::Int)),
            Some(PReg::new(N_XMM, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    }
}

/// regalloc2 `Function` adapter over a `MachFunc`. Drives the CFG from
/// `mf.blocks` when present, falling back to a single basic block over all of
/// `mf.insts` when `mf.blocks` is empty (see module docs). Owns its interned
/// operand/CFG tables so it does not borrow the `MachFunc` during allocation.
struct Adapter {
    num_insts: usize,
    num_vregs: usize,
    /// Interned operands per instruction (defs first, then uses).
    operands: Vec<Vec<Operand>>,
    /// Dense regalloc2 vreg index → our VReg.
    reverse: Vec<VReg>,
    /// `[start, end)` inst range per block.
    block_ranges: Vec<(usize, usize)>,
    /// regalloc2 block params per block (interned `MachBlock.params`).
    block_params: Vec<Vec<Ra2VReg>>,
    /// Successor blocks per block.
    block_succs: Vec<Vec<Block>>,
    /// Predecessor blocks per block (inverted edge set).
    block_preds: Vec<Vec<Block>>,
    /// Outgoing blockparam args per block, indexed `[block][succ_idx]`.
    branch_args: Vec<Vec<Vec<Ra2VReg>>>,
    /// Per-instruction terminator flags (length `num_insts`).
    is_branch: Vec<bool>,
    is_ret: Vec<bool>,
    clobbers: Vec<PRegSet>,
    debug_labels: Vec<(Ra2VReg, Ra2Inst, Ra2Inst, u32)>,
}

impl Adapter {
    fn build(mf: &MachFunc, call_clobbers: PRegSet, stack_call_operands: bool) -> Adapter {
        let mut map: HashMap<VReg, usize> = HashMap::new();
        let mut reverse: Vec<VReg> = Vec::new();
        let mut intern = |v: VReg| -> Ra2VReg {
            let idx = *map.entry(v).or_insert_with(|| {
                let i = reverse.len();
                reverse.push(v);
                i
            });
            Ra2VReg::new(idx, ra2_class(v.class))
        };

        let mut operands: Vec<Vec<Operand>> = Vec::with_capacity(mf.insts.len());
        let mut clobbers: Vec<PRegSet> = Vec::with_capacity(mf.insts.len());
        for inst in &mf.insts {
            let mut ops =
                Vec::with_capacity(inst.defs.len() + inst.uses.len() + inst.deopt_uses.len());
            for &d in &inst.defs {
                if stack_call_operands
                    && inst.op == crate::t2::lower::op::CALL_RUNTIME
                    && inst.defs.len() > 1
                {
                    // Multiple values are copied from the activation to their
                    // homes one at a time; they need not all fit in registers.
                    ops.push(Operand::new(
                        intern(d),
                        OperandConstraint::Any,
                        OperandKind::Def,
                        OperandPos::Late,
                    ));
                } else {
                    ops.push(Operand::reg_def(intern(d)));
                }
            }
            for &u in &inst.uses {
                if inst.op == crate::t2::lower::op::INVOKE
                    || (stack_call_operands && inst.op == crate::t2::lower::op::CALL)
                {
                    // Invoke and the slice-call framed emitters copy call
                    // arguments from their homes to a scanned slice. Requiring
                    // every argument in a GPR simultaneously would make calls
                    // wider than the register bank impossible to allocate.
                    ops.push(Operand::new(
                        intern(u),
                        OperandConstraint::Any,
                        OperandKind::Use,
                        OperandPos::Early,
                    ));
                } else {
                    ops.push(Operand::reg_use(intern(u)));
                }
            }
            // Frame-state liveness extension (bliss-ad1e): a value a deopt may
            // reconstruct must stay locatable AT AND AFTER this instruction —
            // Any constraint (register or spill slot both fine; keeps pressure
            // low) at the Late position (survives the inst's own writes, so an
            // in-place result reuse followed by a deopting `jo` still finds
            // the original value).
            for &v in &inst.deopt_uses {
                ops.push(Operand::new(
                    intern(v),
                    OperandConstraint::Any,
                    OperandKind::Use,
                    OperandPos::Late,
                ));
            }
            operands.push(ops);

            let mut set = PRegSet::empty();
            if matches!(
                inst.op,
                crate::t2::lower::op::CALL
                    | crate::t2::lower::op::INVOKE
                    | crate::t2::lower::op::CALL_RUNTIME
                    | crate::t2::lower::op::ALLOC
                    | crate::t2::lower::op::TAILCALL
                    | crate::t2::lower::op::THROW
                    | crate::t2::lower::op::NLX_TRANSFER
            ) {
                set = call_clobbers;
            }
            clobbers.push(set);
        }

        let num_insts = mf.insts.len();
        let mut is_branch = vec![false; num_insts];
        let mut is_ret = vec![false; num_insts];

        let (block_ranges, block_params, block_succs, branch_args) = if mf.blocks.is_empty() {
            // ── Fallback: whole function as one basic block over all insts. ──
            let ret_inst = num_insts.saturating_sub(1);
            if num_insts > 0 {
                is_ret[ret_inst] = true;
            }
            (
                vec![(0, num_insts)],
                vec![Vec::new()],
                vec![Vec::new()],
                vec![Vec::new()],
            )
        } else {
            // ── Real block CFG driven by mf.blocks. ──────────────────────────
            let nb = mf.blocks.len();
            let mut ranges = Vec::with_capacity(nb);
            let mut params = Vec::with_capacity(nb);
            let mut succs: Vec<Vec<Block>> = Vec::with_capacity(nb);
            let mut args: Vec<Vec<Vec<Ra2VReg>>> = Vec::with_capacity(nb);

            for b in &mf.blocks {
                ranges.push((b.start, b.end));
                params.push(b.params.iter().map(|&v| intern(v)).collect());

                let mut this_succs = Vec::with_capacity(b.succs.len());
                let mut this_args = Vec::with_capacity(b.succs.len());
                for s in &b.succs {
                    this_succs.push(Block::new(s.target.0 as usize));
                    this_args.push(s.args.iter().map(|&v| intern(v)).collect());
                }
                succs.push(this_succs);
                args.push(this_args);

                // Terminator = last inst of the block. Branch if it has
                // successors, otherwise a return.
                if b.end > b.start {
                    let term = b.end - 1;
                    if b.succs.is_empty() {
                        is_ret[term] = true;
                    } else {
                        is_branch[term] = true;
                    }
                }
            }

            (ranges, params, succs, args)
        };

        // Invert the successor edges to get predecessors.
        let nb = block_ranges.len();
        let mut block_preds: Vec<Vec<Block>> = vec![Vec::new(); nb];
        for (b, s) in block_succs.iter().enumerate() {
            for &succ in s {
                block_preds[succ.index()].push(Block::new(b));
            }
        }

        let debug_labels = reverse
            .iter()
            .enumerate()
            .map(|(label, v)| {
                (
                    Ra2VReg::new(label, ra2_class(v.class)),
                    Ra2Inst::new(0),
                    Ra2Inst::new(num_insts),
                    label as u32,
                )
            })
            .collect();

        Adapter {
            num_insts,
            num_vregs: reverse.len(),
            operands,
            reverse,
            block_ranges,
            block_params,
            block_succs,
            block_preds,
            branch_args,
            is_branch,
            is_ret,
            clobbers,
            debug_labels,
        }
    }
}

impl Ra2Function for Adapter {
    fn num_insts(&self) -> usize {
        self.num_insts
    }

    fn num_blocks(&self) -> usize {
        self.block_ranges.len()
    }

    fn entry_block(&self) -> Block {
        Block::new(0)
    }

    fn block_insns(&self, block: Block) -> InstRange {
        let (start, end) = self.block_ranges[block.index()];
        InstRange::new(Ra2Inst::new(start), Ra2Inst::new(end))
    }

    fn block_succs(&self, block: Block) -> &[Block] {
        &self.block_succs[block.index()]
    }

    fn block_preds(&self, block: Block) -> &[Block] {
        &self.block_preds[block.index()]
    }

    fn block_params(&self, block: Block) -> &[Ra2VReg] {
        &self.block_params[block.index()]
    }

    fn is_ret(&self, insn: Ra2Inst) -> bool {
        self.is_ret[insn.index()]
    }

    fn is_branch(&self, insn: Ra2Inst) -> bool {
        self.is_branch[insn.index()]
    }

    fn branch_blockparams(&self, block: Block, _insn: Ra2Inst, succ_idx: usize) -> &[Ra2VReg] {
        &self.branch_args[block.index()][succ_idx]
    }

    fn inst_operands(&self, insn: Ra2Inst) -> &[Operand] {
        &self.operands[insn.index()]
    }

    fn inst_clobbers(&self, insn: Ra2Inst) -> PRegSet {
        self.clobbers[insn.index()]
    }

    fn num_vregs(&self) -> usize {
        self.num_vregs
    }

    fn debug_value_labels(&self) -> &[(Ra2VReg, Ra2Inst, Ra2Inst, u32)] {
        &self.debug_labels
    }

    fn spillslot_size(&self, _regclass: Ra2RegClass) -> usize {
        1
    }
}

/// Allocate registers for `mf` in place using the abstract x86 register pool.
///
/// Fills `mf.allocation` with a `VReg → Location` binding for every virtual
/// register and pushes one [`StackMap`] per safepoint instruction. See the
/// module docs for the empty-blocks fallback and the stack-map limitations.
pub fn allocate(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    allocate_with_env(mf, machine_env(), x86_call_clobbers())
}

fn x86_call_clobbers() -> PRegSet {
    let mut set = PRegSet::empty();
    // Abstract encodings, mapped to rax,rcx,rdx,rsi,rdi,r8-r11 in emit.rs.
    for i in 0..=8 {
        set.add(PReg::new(i, Ra2RegClass::Int));
    }
    for i in 0..N_XMM {
        set.add(PReg::new(i, Ra2RegClass::Float));
    }
    set
}

/// Allocate for the live framed x86 emitter, reserving its ABI and scratch
/// registers while still using the same regalloc2 pipeline and edit model.
pub fn allocate_framed(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    // Reduced-pool-first (bliss-x5y.29): short prologues for the small hot
    // functions that dominate recursion, with a FULL-pool retry when the
    // reduced pool genuinely runs out of registers — a retry costs one extra
    // background-thread allocation pass, declining to T1 costs the tier.
    // EGCL_T2_FRAME_ENV=full|reduced pins one environment for debugging.
    match std::env::var("EGCL_T2_FRAME_ENV").as_deref() {
        Ok("full") => return allocate_with_env(mf, framed_machine_env(false), x86_call_clobbers()),
        Ok("reduced") => {
            return allocate_with_env(mf, framed_machine_env(true), x86_call_clobbers());
        }
        _ => {}
    }
    match allocate_with_env(mf, framed_machine_env(true), x86_call_clobbers()) {
        Err(RegAllocError::TooManyLiveRegs) => {
            allocate_with_env(mf, framed_machine_env(false), x86_call_clobbers())
        }
        done => done,
    }
}

/// Allocate for the System Z framed emitter. Unlike the x86 backend's abstract
/// register indices, every returned PhysReg encoding is a hardware number.
///
/// r13 holds the EgclStack activation pointer; r14/r15 are link/stack. Reserve
/// r0-r5 for instruction and ABI temporaries, with r1 as the allocator's edit
/// scratch. Allocate r6-r12, all preserved across C calls. f0-f3 are instruction
/// temporaries and f15 is the floating edit scratch; f4-f14 are allocatable.
/// The emitter must save every callee-saved register it writes, including f15
/// when an allocation edit uses it, and synchronize tagged roots at safepoints.
pub fn allocate_framed_s390x(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    let mut integers = PRegSet::empty();
    for reg in 6..=12 {
        integers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    let mut floats = PRegSet::empty();
    for reg in 4..=14 {
        floats.add(PReg::new(reg, Ra2RegClass::Float));
    }
    let env = MachineEnv {
        preferred_regs_by_class: [integers, floats, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [
            Some(PReg::new(1, Ra2RegClass::Int)),
            Some(PReg::new(15, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    };
    // Linux s390x ELF ABI: r0-r5/r14 and f0-f7 are volatile. This is the
    // 64-bit ABI, not the older 31-bit s390 floating-point convention.
    let mut clobbers = PRegSet::empty();
    for reg in (0..=5).chain(std::iter::once(14)) {
        clobbers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    for reg in 0..=7 {
        clobbers.add(PReg::new(reg, Ra2RegClass::Float));
    }
    allocate_with_call_operands(mf, env, clobbers, true)
}

/// LP64 allocation for the riscv64 framed emitter. `PhysReg.encoding` is the
/// architectural register number.
///
/// The allocatable integer set is s1 and s3–s11: callee-saved, so a value stays
/// put across a runtime call and the prologue saves them once. s2 is reserved
/// for the frame-slots pointer for the whole body (System Z uses r13), s0 is
/// left as the frame pointer, sp/ra/gp/tp are not allocatable, and a0–a3 plus
/// t0–t3 are the emitter's working registers; t6 belongs to the assembler.
///
/// Floats: ft0–ft7 and fa0–fa7, all call-clobbered. The callee-saved fs
/// registers are not offered; nothing in the framed pipeline keeps an unboxed
/// float live across a call.
pub fn allocate_framed_riscv64(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    let mut integers = PRegSet::empty();
    for reg in std::iter::once(9).chain(19..=27) {
        integers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    let mut floats = PRegSet::empty();
    for reg in (0..=7).chain(10..=17) {
        floats.add(PReg::new(reg, Ra2RegClass::Float));
    }
    let env = MachineEnv {
        preferred_regs_by_class: [integers, floats, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [
            Some(PReg::new(29, Ra2RegClass::Int)),
            Some(PReg::new(31, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    };
    // LP64: ra, t0-t6, a0-a7 and ft0-ft11, fa0-fa7 are caller-saved.
    let mut clobbers = PRegSet::empty();
    for reg in std::iter::once(1)
        .chain(5..=7)
        .chain(10..=17)
        .chain(28..=31)
    {
        clobbers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    for reg in (0..=7).chain(10..=17).chain(28..=31) {
        clobbers.add(PReg::new(reg, Ra2RegClass::Float));
    }
    allocate_with_call_operands(mf, env, clobbers, true)
}

/// AAPCS64 allocation for the AArch64 framed emitter. Like the System Z pool,
/// `PhysReg.encoding` is the architectural register number, not an abstract index.
///
/// The allocatable set is x19–x27: callee-saved, so a value stays put across a
/// runtime call without the emitter spilling it, and the prologue saves them once.
/// x28 is reserved to hold the frame-slots pointer for the whole body (System Z
/// uses r13 for this), and x29/x30/x31 are the frame pointer, link register and
/// stack pointer. x16/x17 are IP0/IP1, which the architecture already reserves as
/// call-clobbered scratch, and x0–x5 are the emitter's working registers.
///
/// Floats: v16–v31 are call-clobbered in full. v8–v15 are deliberately NOT offered
/// even though they are nominally callee-saved, because AAPCS64 preserves only
/// their low 64 bits — a caller that stored a wider value there would find the
/// top half gone, and this pool is not the place to encode that subtlety.
pub fn allocate_framed_a64(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    let mut integers = PRegSet::empty();
    for reg in 19..=27 {
        integers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    let mut floats = PRegSet::empty();
    for reg in 16..=30 {
        floats.add(PReg::new(reg, Ra2RegClass::Float));
    }
    let env = MachineEnv {
        preferred_regs_by_class: [integers, floats, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [
            Some(PReg::new(17, Ra2RegClass::Int)),
            Some(PReg::new(31, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    };
    // AAPCS64 volatile registers: x0–x18 (x18 is the platform register, which is
    // reserved rather than ours to keep) and v0–v7 plus v16–v31.
    let mut clobbers = PRegSet::empty();
    for reg in 0..=18 {
        clobbers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    for reg in (0..=7).chain(16..=31) {
        clobbers.add(PReg::new(reg, Ra2RegClass::Float));
    }
    allocate_with_call_operands(mf, env, clobbers, true)
}

/// ELFv2 allocation for the ppc64le framed emitter. As on System Z,
/// `PhysReg.encoding` is the architectural register number rather than an abstract
/// index.
///
/// The allocatable set is r20–r29: nonvolatile, so a value survives a runtime call
/// without the emitter spilling it, and the prologue saves them once as part of the
/// frame convention both tiers share. r14–r16 are reserved for the activation
/// registers, r1 is the stack pointer and r2 the TOC, r11/r12 are the emitter's
/// scratch (r12 additionally because the ABI expects a call target there), and
/// r3–r6 are the working registers, which double as the first argument registers.
///
/// r0 is excluded for a reason peculiar to POWER: several instruction forms read it
/// as a literal zero rather than as r0's contents, so a value allocated there would
/// silently read as 0 in exactly those forms.
///
/// Floats: f14–f29 are nonvolatile, with f30 as the class scratch; f0–f13 are
/// call-clobbered and used as working registers.
pub fn allocate_framed_ppc64le(mf: &mut MachFunc) -> Result<(), RegAllocError> {
    let mut integers = PRegSet::empty();
    for reg in 20..=29 {
        integers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    let mut floats = PRegSet::empty();
    for reg in 14..=29 {
        floats.add(PReg::new(reg, Ra2RegClass::Float));
    }
    let env = MachineEnv {
        preferred_regs_by_class: [integers, floats, PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [
            Some(PReg::new(17, Ra2RegClass::Int)),
            Some(PReg::new(30, Ra2RegClass::Float)),
            None,
        ],
        fixed_stack_slots: Vec::new(),
    };
    // Volatile under ELFv2: r0 and r3–r12, and f0–f13.
    let mut clobbers = PRegSet::empty();
    for reg in std::iter::once(0).chain(3..=12) {
        clobbers.add(PReg::new(reg, Ra2RegClass::Int));
    }
    for reg in 0..=13 {
        clobbers.add(PReg::new(reg, Ra2RegClass::Float));
    }
    allocate_with_call_operands(mf, env, clobbers, true)
}

fn allocate_with_env(
    mf: &mut MachFunc,
    env: MachineEnv,
    call_clobbers: PRegSet,
) -> Result<(), RegAllocError> {
    allocate_with_call_operands(mf, env, call_clobbers, false)
}

fn allocate_with_call_operands(
    mf: &mut MachFunc,
    env: MachineEnv,
    call_clobbers: PRegSet,
    stack_call_operands: bool,
) -> Result<(), RegAllocError> {
    mf.allocation.clear();
    mf.inst_allocations.clear();
    mf.allocation_edits.clear();
    mf.num_spill_slots = 0;
    mf.value_locations.clear();
    mf.stack_maps.clear();

    if mf.insts.is_empty() {
        return Ok(());
    }

    let adapter = Adapter::build(mf, call_clobbers, stack_call_operands);

    // Ion 0.15.2 underflows ProgPoint::prev while diagnosing an impossible
    // register demand at instruction zero. Report that demand ourselves so
    // the framed allocator can retry its larger register pool normally.
    // Uses and defs occupy separate phases; repeated uses of one vreg need
    // only one register, and Any operands may spill.
    let capacity: [usize; 3] = std::array::from_fn(|class| {
        env.preferred_regs_by_class[class].into_iter().count()
            + env.non_preferred_regs_by_class[class].into_iter().count()
    });
    let mut required: [Vec<Ra2VReg>; 6] = std::array::from_fn(|_| Vec::new());
    for operands in &adapter.operands {
        for regs in &mut required {
            regs.clear();
        }
        for operand in operands {
            if operand.constraint() != OperandConstraint::Reg {
                continue;
            }
            let class = operand.class() as usize;
            let phase = usize::from(operand.pos() == OperandPos::Late);
            let regs = &mut required[class * 2 + phase];
            if !regs.contains(&operand.vreg()) {
                regs.push(operand.vreg());
                if regs.len() > capacity[class] {
                    return Err(RegAllocError::TooManyLiveRegs);
                }
            }
        }
    }

    let options = RegallocOptions {
        verbose_log: false,
        // Inputs are already SSA-shaped (lowering's contract): a single def per
        // vreg, block params standing in for φ at merges. We leave the extra SSA
        // validator pass off to match the original behaviour and avoid rejecting
        // valid-but-unconventional hand-built MachFuncs.
        validate_ssa: false,
        algorithm: Algorithm::Ion,
    };

    let output = regalloc2::run(&adapter, &env, &options).inspect_err(|e| {
        if std::env::var_os("EGCL_RA_DBG").is_some() {
            eprintln!("[ra] error {e:?}; machine insts:");
            for (i, inst) in mf.insts.iter().enumerate() {
                eprintln!(
                    "[ra] {i}: op={} defs={:?} uses={:?} deopt={:?} fs={:?} sp={}",
                    inst.op,
                    inst.defs.iter().map(|v| v.num).collect::<Vec<_>>(),
                    inst.uses.iter().map(|v| v.num).collect::<Vec<_>>(),
                    inst.deopt_uses.iter().map(|v| v.num).collect::<Vec<_>>(),
                    inst.frame_state,
                    inst.safepoint,
                );
            }
            for (i, b) in mf.blocks.iter().enumerate() {
                eprintln!(
                    "[ra] block {i}: [{}, {}) params={:?}",
                    b.start,
                    b.end,
                    b.params.iter().map(|v| v.num).collect::<Vec<_>>()
                );
            }
        }
    })?;

    mf.num_spill_slots = output.num_spillslots as u32;
    mf.inst_allocations = (0..adapter.num_insts)
        .map(|i| {
            output
                .inst_allocs(Ra2Inst::new(i))
                .iter()
                .filter_map(|&a| to_location(a))
                .collect()
        })
        .collect();
    mf.allocation_edits = output
        .edits
        .iter()
        .filter_map(|(point, edit)| match edit {
            Edit::Move { from, to } => Some(AllocationEdit {
                inst: point.inst().index(),
                position: match point.pos() {
                    InstPosition::Before => EditPosition::Before,
                    InstPosition::After => EditPosition::After,
                },
                from: to_location(*from)?,
                to: to_location(*to)?,
            }),
        })
        .collect();
    mf.value_locations = output
        .debug_locations
        .iter()
        .filter_map(|(label, start, end, allocation)| {
            Some(ValueLocationRange {
                vreg: *adapter.reverse.get(*label as usize)?,
                start: start.to_index(),
                end: end.to_index(),
                location: to_location(*allocation)?,
            })
        })
        .collect();

    // ── Write back VReg → Location. Prefer a value's def-site allocation as its
    // canonical location; fall back to a use site if it is only ever used. ────
    let mut loc_of: HashMap<VReg, Location> = HashMap::new();
    for i in 0..adapter.num_insts {
        let allocs = output.inst_allocs(Ra2Inst::new(i));
        for (op, alloc) in adapter.operands[i].iter().zip(allocs) {
            let ours = adapter.reverse[op.vreg().vreg()];
            let Some(loc) = to_location(*alloc) else {
                continue;
            };
            match op.kind() {
                OperandKind::Def => {
                    loc_of.insert(ours, loc);
                }
                OperandKind::Use => {
                    loc_of.entry(ours).or_insert(loc);
                }
            }
        }
    }
    // Emit in dense-index order for a deterministic result.
    for v in &adapter.reverse {
        if let Some(loc) = loc_of.get(v) {
            mf.allocation.push((*v, *loc));
        }
    }

    // ── Stack maps: one per safepoint, with live tagged (Gpr) refs and the
    // resolved FrameState id (see module docs for why we compute these
    // ourselves rather than via regalloc2). ───────────────────────────────────
    for i in 0..mf.insts.len() {
        if !mf.insts[i].safepoint {
            continue;
        }
        let allocs = output.inst_allocs(Ra2Inst::new(i));
        let mut live_refs = Vec::new();
        for (op, alloc) in adapter.operands[i].iter().zip(allocs) {
            if op.kind() != OperandKind::Use {
                continue;
            }
            let ours = adapter.reverse[op.vreg().vreg()];
            if ours.class == RegClass::Gpr {
                if let Some(loc) = to_location(*alloc) {
                    live_refs.push(loc);
                }
            }
        }
        mf.stack_maps.push(StackMap {
            code_offset: i as u32,
            live_refs,
            frame_state: mf.insts[i].frame_state,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameStateId;
    use crate::t2::mach::{MachBlock, MachBlockId, MachInst, MachSucc};

    fn vreg(class: RegClass, num: u32) -> VReg {
        VReg { class, num }
    }

    fn inst(op: u32, defs: Vec<VReg>, uses: Vec<VReg>) -> MachInst {
        MachInst {
            source_inst: None,
            op,
            defs,
            uses,
            imm: None,
            frame_state: None,
            deopt_uses: Vec::new(),
            safepoint: false,
        }
    }

    fn entry_call(nargs: u32) -> MachFunc {
        let args: Vec<_> = (0..nargs).map(|n| vreg(RegClass::Gpr, n)).collect();
        let result = vreg(RegClass::Gpr, nargs);
        MachFunc {
            insts: vec![
                inst(crate::t2::lower::op::CALL, vec![result], args.clone()),
                inst(0, vec![], vec![result]),
            ],
            blocks: vec![MachBlock {
                params: args,
                start: 0,
                end: 2,
                succs: vec![],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn entry_call_pressure_retries_with_the_full_register_pool() {
        let mut mf = entry_call(6);
        assert!(matches!(
            allocate_with_env(&mut mf, framed_machine_env(true), x86_call_clobbers()),
            Err(RegAllocError::TooManyLiveRegs)
        ));
        allocate_framed(&mut mf).expect("six arguments fit the full register pool");
    }

    #[test]
    fn impossible_entry_call_pressure_returns_an_error() {
        assert!(matches!(
            allocate_framed(&mut entry_call(9)),
            Err(RegAllocError::TooManyLiveRegs)
        ));
    }

    #[test]
    fn repeated_call_arguments_share_one_register() {
        let mut mf = entry_call(1);
        mf.insts[0].uses = vec![vreg(RegClass::Gpr, 0); 9];
        allocate_with_env(&mut mf, framed_machine_env(true), x86_call_clobbers())
            .expect("repeated arguments do not increase register pressure");
    }

    #[test]
    fn stack_call_arguments_are_not_counted_as_required_registers() {
        allocate_with_call_operands(
            &mut entry_call(12),
            framed_machine_env(true),
            x86_call_clobbers(),
            true,
        )
        .expect("Any operands can spill even at the entry instruction");
    }

    /// A small SSA-shaped straight-line function over two GPR vregs and one XMM
    /// vreg, with one safepoint instruction carrying a frame state.
    fn sample() -> MachFunc {
        let g0 = vreg(RegClass::Gpr, 0);
        let g1 = vreg(RegClass::Gpr, 1);
        let f0 = vreg(RegClass::Xmm, 0);

        let safepoint = MachInst {
            source_inst: None,
            op: 3,
            defs: vec![g1],
            uses: vec![g0],
            imm: None,
            frame_state: Some(FrameStateId(7)),
            deopt_uses: Vec::new(),
            safepoint: true,
        };

        MachFunc {
            insts: vec![
                inst(1, vec![g0], vec![]),     // def g0
                inst(2, vec![f0], vec![]),     // def f0
                safepoint,                     // def g1, use g0  (safepoint)
                inst(0, vec![], vec![g1, f0]), // ret: use g1, f0  (last => is_ret)
            ],
            ..Default::default()
        }
    }

    /// bliss-ad1e: a value referenced ONLY by a guard's frame state (no
    /// ordinary use after its def) must stay locatable at the guard — the
    /// deopt reconstructs the interpreter frame from it. Its `deopt_uses`
    /// entry is what tells regalloc2 it is live there; without it the value
    /// is dead after its def and its register can be recycled.
    #[test]
    fn frame_state_only_value_stays_locatable_at_its_guard() {
        let g0 = vreg(RegClass::Gpr, 0);
        let g1 = vreg(RegClass::Gpr, 1);
        let guard = MachInst {
            source_inst: None,
            op: 3,
            defs: vec![g1],
            uses: vec![],
            imm: None,
            frame_state: Some(FrameStateId(9)),
            deopt_uses: vec![g0],
            safepoint: false,
        };
        let mut mf = MachFunc {
            insts: vec![
                inst(1, vec![g0], vec![]), // def g0 — never ordinarily used again
                guard,                     // inst 1: frame state names g0
                inst(0, vec![], vec![g1]), // ret uses g1 only
            ],
            ..Default::default()
        };
        allocate(&mut mf).expect("regalloc2");
        // The guard instruction is index 1; its Late program point in
        // regalloc2's raw encoding is 2*1 + 1 = 3. Some recorded location
        // range for g0 must still cover it.
        let guard_late = 3;
        let covered = mf
            .value_locations
            .iter()
            .any(|r| r.vreg == g0 && r.start <= guard_late && r.end >= guard_late);
        assert!(
            covered,
            "frame-state-only value g0 has no location at its guard: {:?}",
            mf.value_locations
        );
    }

    #[test]
    fn every_vreg_gets_a_class_appropriate_location() {
        let mut mf = sample();
        allocate(&mut mf).expect("regalloc2");

        let map: HashMap<VReg, Location> = mf.allocation.iter().copied().collect();

        // All three distinct vregs are bound.
        for v in [
            vreg(RegClass::Gpr, 0),
            vreg(RegClass::Gpr, 1),
            vreg(RegClass::Xmm, 0),
        ] {
            let loc = map
                .get(&v)
                .unwrap_or_else(|| panic!("vreg {v:?} unallocated"));
            // If bound to a register, its class must match the vreg's class.
            if let Location::Register(preg) = loc {
                assert_eq!(preg.class, v.class, "class mismatch for {v:?}");
            }
        }

        // No duplicate bindings.
        assert_eq!(map.len(), mf.allocation.len(), "duplicate allocation entry");
    }

    #[test]
    fn safepoint_produces_a_stack_map_with_frame_state() {
        let mut mf = sample();
        allocate(&mut mf).expect("regalloc2");

        assert_eq!(mf.stack_maps.len(), 1, "exactly one safepoint => one map");
        let sm = &mf.stack_maps[0];
        assert_eq!(sm.frame_state, Some(FrameStateId(7)));
        // The safepoint uses g0 (a Gpr tagged ref), so its location is recorded.
        assert!(
            !sm.live_refs.is_empty(),
            "expected the live Gpr ref (g0) in the stack map"
        );
        for loc in &sm.live_refs {
            if let Location::Register(preg) = loc {
                assert_eq!(preg.class, RegClass::Gpr);
            }
        }
    }

    #[test]
    fn empty_function_is_a_no_op() {
        let mut mf = MachFunc::default();
        allocate(&mut mf).expect("regalloc2");
        assert!(mf.allocation.is_empty());
        assert!(mf.stack_maps.is_empty());
    }

    /// Exercise the exact edit stream, including values live through a call
    /// and a value whose only use is a late deopt operand on that call.
    #[test]
    fn s390x_allocations_preserve_values_across_calls_and_spills() {
        use crate::t2::lower::op;

        let mut mf = MachFunc::default();
        for class in [RegClass::Gpr, RegClass::Xmm] {
            for num in 0..24 {
                mf.insts
                    .push(inst(op::MOV_IMM, vec![vreg(class, num)], vec![]));
            }
        }
        let deopt_only = vreg(RegClass::Gpr, 100);
        mf.insts.push(inst(op::MOV_IMM, vec![deopt_only], vec![]));
        let mut call = inst(op::CALL_RUNTIME, vec![vreg(RegClass::Gpr, 101)], vec![]);
        call.deopt_uses.push(deopt_only);
        mf.insts.push(call);
        for class in [RegClass::Gpr, RegClass::Xmm] {
            for num in 0..24 {
                mf.insts.push(inst(op::MOV, vec![], vec![vreg(class, num)]));
            }
        }
        mf.insts
            .push(inst(op::RET, vec![], vec![vreg(RegClass::Gpr, 101)]));
        allocate_framed_s390x(&mut mf).expect("s390x register allocation");
        assert!(mf.num_spill_slots > 0, "fixture must force spills");
        assert!(!mf.allocation_edits.is_empty());

        // Keep identity tokens in physical locations. Poison precisely the
        // ABI's volatile registers at a call, independently of the allocator.
        let key = |loc: Location| match loc {
            Location::Register(p) => (Some(p.class), u32::from(p.encoding)),
            Location::Stack(slot) => (None, slot.0),
        };
        let mut contents = HashMap::new();
        for (index, instruction) in mf.insts.iter().enumerate() {
            for position in [EditPosition::Before, EditPosition::After] {
                if position == EditPosition::After {
                    let locations = &mf.inst_allocations[index];
                    let defs = instruction.defs.len();
                    for (operand, value) in instruction.uses.iter().enumerate() {
                        assert_eq!(contents.get(&key(locations[defs + operand])), Some(value));
                    }
                    if instruction.op == op::CALL_RUNTIME {
                        contents.retain(|&(class, reg), _| match class {
                            Some(RegClass::Gpr) => (6..=13).contains(&reg) || reg == 15,
                            Some(RegClass::Xmm) => (8..=15).contains(&reg),
                            None => true,
                        });
                    }
                    for (operand, value) in instruction.defs.iter().enumerate() {
                        contents.insert(key(locations[operand]), *value);
                    }
                    for (operand, value) in instruction.deopt_uses.iter().enumerate() {
                        let location = locations[defs + instruction.uses.len() + operand];
                        assert_eq!(contents.get(&key(location)), Some(value));
                    }
                }
                for edit in mf
                    .allocation_edits
                    .iter()
                    .filter(|edit| edit.inst == index && edit.position == position)
                {
                    let value = *contents
                        .get(&key(edit.from))
                        .expect("initialized move source");
                    contents.insert(key(edit.to), value);
                }
            }
        }
        for location in mf.inst_allocations.iter().flatten() {
            if let Location::Register(p) = location {
                let allowed = match p.class {
                    RegClass::Gpr => 6..=12,
                    RegClass::Xmm => 4..=14,
                };
                assert!(allowed.contains(&p.encoding), "reserved register {p:?}");
            }
        }
    }

    /// A diamond CFG with a merge block that has a parameter VReg:
    ///
    /// ```text
    ///        entry (b0): def g0; branch
    ///        /                 \
    ///   left (b1)          right (b2)
    ///   def gL; br(gL)     def gR; br(gR)
    ///        \                 /
    ///        merge (b3): param gm
    ///        safepoint(uses g0, gm); ret(gm)
    /// ```
    ///
    /// No critical edges (each of b1/b2 has one pred and one succ), so it
    /// allocates directly. gm arrives only via the incoming edge args, exercising
    /// block-param liveness. The safepoint must yield one StackMap.
    fn diamond() -> MachFunc {
        let g0 = vreg(RegClass::Gpr, 0);
        let gl = vreg(RegClass::Gpr, 1);
        let gr = vreg(RegClass::Gpr, 2);
        let gm = vreg(RegClass::Gpr, 3);

        // Terminator branches carry no register operands of their own — their
        // edge args travel via MachSucc.args / branch_blockparams.
        let br = || inst(100, vec![], vec![]);

        let safepoint = MachInst {
            source_inst: None,
            op: 3,
            defs: vec![],
            uses: vec![g0, gm],
            imm: None,
            frame_state: Some(FrameStateId(11)),
            deopt_uses: Vec::new(),
            safepoint: true,
        };

        MachFunc {
            insts: vec![
                inst(1, vec![g0], vec![]), // 0: b0 def g0
                br(),                      // 1: b0 branch
                inst(2, vec![gl], vec![]), // 2: b1 def gL
                br(),                      // 3: b1 branch -> merge(gL)
                inst(2, vec![gr], vec![]), // 4: b2 def gR
                br(),                      // 5: b2 branch -> merge(gR)
                safepoint,                 // 6: b3 safepoint (use g0, gm)
                inst(0, vec![], vec![gm]), // 7: b3 ret (use gm)
            ],
            blocks: vec![
                MachBlock {
                    params: vec![],
                    start: 0,
                    end: 2,
                    succs: vec![
                        MachSucc {
                            target: MachBlockId(1),
                            args: vec![],
                        },
                        MachSucc {
                            target: MachBlockId(2),
                            args: vec![],
                        },
                    ],
                },
                MachBlock {
                    params: vec![],
                    start: 2,
                    end: 4,
                    succs: vec![MachSucc {
                        target: MachBlockId(3),
                        args: vec![gl],
                    }],
                },
                MachBlock {
                    params: vec![],
                    start: 4,
                    end: 6,
                    succs: vec![MachSucc {
                        target: MachBlockId(3),
                        args: vec![gr],
                    }],
                },
                MachBlock {
                    params: vec![gm],
                    start: 6,
                    end: 8,
                    succs: vec![],
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn diamond_allocates_block_param_and_all_vregs() {
        let mut mf = diamond();
        allocate(&mut mf).expect("regalloc2");

        let map: HashMap<VReg, Location> = mf.allocation.iter().copied().collect();

        // Every VReg — including the merge block param gm — is bound to a
        // class-appropriate location.
        for v in [
            vreg(RegClass::Gpr, 0),
            vreg(RegClass::Gpr, 1),
            vreg(RegClass::Gpr, 2),
            vreg(RegClass::Gpr, 3),
        ] {
            let loc = map
                .get(&v)
                .unwrap_or_else(|| panic!("vreg {v:?} unallocated"));
            if let Location::Register(preg) = loc {
                assert_eq!(preg.class, v.class, "class mismatch for {v:?}");
            }
        }
        assert_eq!(map.len(), mf.allocation.len(), "duplicate allocation entry");
    }

    #[test]
    fn diamond_safepoint_yields_a_stack_map() {
        let mut mf = diamond();
        allocate(&mut mf).expect("regalloc2");

        assert_eq!(mf.stack_maps.len(), 1, "one safepoint => one stack map");
        let sm = &mf.stack_maps[0];
        assert_eq!(sm.frame_state, Some(FrameStateId(11)));
        // The safepoint's two Gpr uses (g0, gm) are recorded as live refs.
        assert_eq!(sm.live_refs.len(), 2, "both live Gpr refs recorded");
        for loc in &sm.live_refs {
            if let Location::Register(preg) = loc {
                assert_eq!(preg.class, RegClass::Gpr);
            }
        }
    }
}
