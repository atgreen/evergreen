//! P6 — Register allocation via regalloc2 (spec §4.7 R4.45, §4.10 R4.65).
//!
//! **Parcel P6. Owner: (sub-agent).** Adapt `MachFunc` (mach.rs, frozen) to the
//! `regalloc2` crate: implement its `Function` trait over our MachInsts (two
//! register classes: Gpr→Int, Xmm→Float), run the allocator, and write the
//! results back: fill `MachFunc::allocation` (VReg → Location) and
//! `MachFunc::stack_maps` (one per safepoint, with live GC refs and the resolved
//! FrameState id — spec §4.10 A4.14/R4.65).
//!
//! ## Mapping MachFunc → regalloc2
//!
//! * **Register classes.** Our [`RegClass::Gpr`] maps to `regalloc2::RegClass::Int`
//!   (tagged values / unboxed integers / pointers) and [`RegClass::Xmm`] to
//!   `Float` (unboxed floats). The `Vector` class is unused.
//! * **VRegs.** A `MachFunc` VReg is `(class, num)`; regalloc2 vregs live in a
//!   single 0-based, dense index space. We **intern** every distinct VReg we see
//!   into a dense id (in first-encounter order) and keep the reverse map for
//!   write-back, so two VRegs that happen to share `num` across classes never
//!   collide.
//! * **Operands.** Each `MachInst`'s `defs` become register-constrained
//!   late-defs (`Operand::reg_def`) and its `uses` become register-constrained
//!   early-uses (`Operand::reg_use`), which is the ordinary machine-instruction
//!   model. We emit defs first, then uses, and rely on that order to line the
//!   result `Allocation`s back up (`Output::inst_allocs` is operand-parallel).
//!
//! ## Single-block limitation (documented contract gap)
//!
//! The frozen `MachFunc` exposes a **flat `insts: Vec<MachInst>`** with no block
//! structure, successors, predecessors, or block params. regalloc2 is a
//! CFG-aware allocator whose `Function` trait is built around basic blocks, so
//! for this first cut we model the whole function as a **single basic block**:
//! `num_blocks() == 1`, no succs/preds/params, and the **last instruction is
//! reported as the return** (regalloc2 requires every block to end in a
//! branch/ret). Consequences:
//!
//! * Inputs must be SSA-shaped within that block — every use dominated by its
//!   def — which is exactly what lowering (P5) produces for straight-line code.
//!   A use with no prior def would be a block live-in, which regalloc2 rejects
//!   (`RegAllocError::EntryLivein`).
//! * There is no control flow, so no φ/blockparam handling and no cross-block
//!   spilling decisions.
//!
//! A **real block-CFG on `MachFunc`** would be a follow-up contract addition:
//! `MachFunc` needs an explicit block list (each an inst range), per-block
//! successor/predecessor edges, block-parameter vregs (the SSA φ replacement
//! regalloc2 uses at merges), and branch instructions carrying their outgoing
//! blockparam args. `inst_operands`, `is_branch`, `is_ret`, and
//! `branch_blockparams` would then be driven by that structure instead of the
//! single-block stub here. Critical edges would also need splitting before
//! allocation.
//!
//! ## Stack maps (documented contract gap)
//!
//! regalloc2 **0.15.2 has no public safepoint / reftype support** — the trait
//! methods that once let the allocator track GC references across safepoints
//! (`requires_refs_for_safepoint`, `reftype_vregs`, `is_safepoint`) exist only
//! inside its private `fuzzing` module in this version, not on the public
//! `Function` trait. So we compute stack maps **ourselves** after allocation:
//! for each `MachInst` with `safepoint == true`, we emit a [`StackMap`] whose
//! `live_refs` are the resolved [`Location`]s of that instruction's **Gpr-class
//! uses** (tagged references live into the safepoint live in GPRs) and whose
//! `frame_state` is the instruction's `FrameStateId`. `code_offset` is set to
//! the instruction index as a placeholder — real native code offsets are only
//! known after machine-code emission (a later parcel), which would revisit these
//! maps. A precise implementation additionally wants per-VReg reftype flags on
//! `MachInst` and cross-instruction liveness (a value can be a live reference at
//! a safepoint without being an operand of the safepoint instruction); the
//! single-block operand-based approximation here covers the values named at the
//! safepoint itself.

use std::collections::HashMap;

use regalloc2::{
    Algorithm, Allocation, Block, Function as Ra2Function, Inst as Ra2Inst, InstRange, MachineEnv,
    Operand, OperandKind, PReg, PRegSet, RegClass as Ra2RegClass, RegallocOptions, VReg as Ra2VReg,
};

use crate::t2::mach::{Location, MachFunc, PhysReg, RegClass, StackMap, StackSlot, VReg};

/// Number of allocatable GPRs exposed to the allocator (hw_enc `0..N_GPR`); one
/// extra encoding above this is reserved as the class scratch register.
const N_GPR: usize = 14;
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
    } else if let Some(slot) = a.as_stack() {
        Some(Location::Stack(StackSlot(slot.index() as u32)))
    } else {
        None
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

/// regalloc2 `Function` adapter over a `MachFunc`, modelled as a single basic
/// block (see module docs). Owns its interned operand tables so it does not
/// borrow the `MachFunc` during allocation.
struct Adapter {
    num_insts: usize,
    num_vregs: usize,
    /// Interned operands per instruction (defs first, then uses).
    operands: Vec<Vec<Operand>>,
    /// Dense regalloc2 vreg index → our VReg.
    reverse: Vec<VReg>,
    /// Index of the instruction reported as the block's return.
    ret_inst: usize,
}

impl Adapter {
    fn build(mf: &MachFunc) -> Adapter {
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
        for inst in &mf.insts {
            let mut ops = Vec::with_capacity(inst.defs.len() + inst.uses.len());
            for &d in &inst.defs {
                ops.push(Operand::reg_def(intern(d)));
            }
            for &u in &inst.uses {
                ops.push(Operand::reg_use(intern(u)));
            }
            operands.push(ops);
        }

        Adapter {
            num_insts: mf.insts.len(),
            num_vregs: reverse.len(),
            operands,
            reverse,
            ret_inst: mf.insts.len().saturating_sub(1),
        }
    }
}

impl Ra2Function for Adapter {
    fn num_insts(&self) -> usize {
        self.num_insts
    }

    fn num_blocks(&self) -> usize {
        1
    }

    fn entry_block(&self) -> Block {
        Block::new(0)
    }

    fn block_insns(&self, _block: Block) -> InstRange {
        InstRange::new(Ra2Inst::new(0), Ra2Inst::new(self.num_insts))
    }

    fn block_succs(&self, _block: Block) -> &[Block] {
        &[]
    }

    fn block_preds(&self, _block: Block) -> &[Block] {
        &[]
    }

    fn block_params(&self, _block: Block) -> &[Ra2VReg] {
        &[]
    }

    fn is_ret(&self, insn: Ra2Inst) -> bool {
        insn.index() == self.ret_inst
    }

    fn is_branch(&self, _insn: Ra2Inst) -> bool {
        false
    }

    fn branch_blockparams(&self, _block: Block, _insn: Ra2Inst, _succ_idx: usize) -> &[Ra2VReg] {
        &[]
    }

    fn inst_operands(&self, insn: Ra2Inst) -> &[Operand] {
        &self.operands[insn.index()]
    }

    fn inst_clobbers(&self, _insn: Ra2Inst) -> PRegSet {
        PRegSet::empty()
    }

    fn num_vregs(&self) -> usize {
        self.num_vregs
    }

    fn spillslot_size(&self, _regclass: Ra2RegClass) -> usize {
        1
    }
}

/// Allocate registers for `mf` in place (spec §4.7 R4.45).
///
/// Fills `mf.allocation` with a `VReg → Location` binding for every virtual
/// register and pushes one [`StackMap`] per safepoint instruction (spec §4.7
/// R4.46, §4.10 R4.65). See the module docs for the single-block and
/// stack-map limitations.
pub fn allocate(mf: &mut MachFunc) {
    mf.allocation.clear();
    mf.stack_maps.clear();

    if mf.insts.is_empty() {
        return;
    }

    let adapter = Adapter::build(mf);
    let env = machine_env();
    let options = RegallocOptions {
        verbose_log: false,
        // The single-block model does not build the block params / edge splits
        // the SSA validator expects; lowering (P5) is responsible for producing
        // SSA-shaped straight-line code.
        validate_ssa: false,
        algorithm: Algorithm::Ion,
    };

    let output = match regalloc2::run(&adapter, &env, &options) {
        Ok(output) => output,
        Err(e) => {
            // No Result channel on this frozen signature; a real pipeline would
            // surface this. Leave `mf` unallocated and report loudly in debug.
            debug_assert!(false, "regalloc2 failed: {e:?}");
            return;
        }
    };

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameStateId;
    use crate::t2::mach::MachInst;

    fn vreg(class: RegClass, num: u32) -> VReg {
        VReg { class, num }
    }

    fn inst(op: u32, defs: Vec<VReg>, uses: Vec<VReg>) -> MachInst {
        MachInst {
            op,
            defs,
            uses,
            frame_state: None,
            safepoint: false,
        }
    }

    /// A small SSA-shaped straight-line function over two GPR vregs and one XMM
    /// vreg, with one safepoint instruction carrying a frame state.
    fn sample() -> MachFunc {
        let g0 = vreg(RegClass::Gpr, 0);
        let g1 = vreg(RegClass::Gpr, 1);
        let f0 = vreg(RegClass::Xmm, 0);

        let safepoint = MachInst {
            op: 3,
            defs: vec![g1],
            uses: vec![g0],
            frame_state: Some(FrameStateId(7)),
            safepoint: true,
        };

        MachFunc {
            insts: vec![
                inst(1, vec![g0], vec![]),      // def g0
                inst(2, vec![f0], vec![]),      // def f0
                safepoint,                      // def g1, use g0  (safepoint)
                inst(0, vec![], vec![g1, f0]),  // ret: use g1, f0  (last => is_ret)
            ],
            ..Default::default()
        }
    }

    #[test]
    fn every_vreg_gets_a_class_appropriate_location() {
        let mut mf = sample();
        allocate(&mut mf);

        let map: HashMap<VReg, Location> = mf.allocation.iter().copied().collect();

        // All three distinct vregs are bound.
        for v in [
            vreg(RegClass::Gpr, 0),
            vreg(RegClass::Gpr, 1),
            vreg(RegClass::Xmm, 0),
        ] {
            let loc = map.get(&v).unwrap_or_else(|| panic!("vreg {v:?} unallocated"));
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
        allocate(&mut mf);

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
        allocate(&mut mf);
        assert!(mf.allocation.is_empty());
        assert!(mf.stack_maps.is_empty());
    }
}
