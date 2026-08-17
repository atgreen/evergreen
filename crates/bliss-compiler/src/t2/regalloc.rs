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
//! ## Block CFG (P6b) — the real multi-block model
//!
//! `MachFunc` now carries a machine-block CFG in `mf.blocks` (mach.rs): a
//! `Vec<MachBlock>` where `blocks[0]` is the entry, each block is an
//! `[start, end)` half-open range into `mf.insts`, and edges are `MachSucc`
//! records (target block + the VReg args bound to that target's params). We map
//! it onto regalloc2's `Function` CFG directly:
//!
//! * **`num_blocks` / `block_insns`.** One regalloc2 block per `MachBlock`;
//!   `block_insns(b)` is `[MachBlock.start, MachBlock.end)`.
//! * **`block_params`.** `MachBlock.params` (VRegs) become regalloc2 block
//!   params — its SSA-φ replacement. A value that arrives on an incoming edge is
//!   *defined* by being a block param, so it is live-in **by construction**, not
//!   a use-before-def. This is what makes branching and looping code (back-edges
//!   into a loop-header block param) allocate correctly.
//! * **`block_succs` / `block_preds`.** Successors come from `MachSucc.target`;
//!   predecessors are the inverted edge set, computed once at build time.
//! * **`branch_blockparams`.** The outgoing edge args (`MachSucc.args`) for the
//!   `succ_idx`-th successor, interned to regalloc2 vregs. Their count matches
//!   the successor's `block_params` count (φ operand parallelism).
//! * **`is_branch` / `is_ret`.** The terminator of each block is its last
//!   instruction (`end - 1`). A block **with** successors ends in a branch; a
//!   block **without** successors ends in a return.
//!
//! ### Critical edges
//!
//! regalloc2 rejects unsplit critical edges (a block with >1 successor feeding a
//! block with >1 predecessor) with `RegAllocError::CritEdge`. This adapter
//! **asserts none remain**: splitting is the lowering (P5) contract, and a
//! debug build `debug_assert!`s if a critical edge is detected before we run
//! (regalloc2 independently rejects any that slip past in release). A plain
//! diamond (entry→{L,R}→merge) has no critical edges, so it allocates directly.
//! Relatedly, when a successor has multiple predecessors the branch feeding it
//! must carry *no* register operands of its own (only blockparam args); our
//! terminators are pure branches, satisfying regalloc2's `DisallowedBranchArg`
//! rule.
//!
//! ### Empty-blocks fallback (older flat lowering)
//!
//! If `mf.blocks` is **empty** (a lowering that has not yet block-structured its
//! output), we fall back to the original single-basic-block model: one block
//! spanning all of `mf.insts`, no params/succs/preds, and the last instruction
//! reported as the return. Straight-line SSA input allocates exactly as before,
//! so nothing regresses.
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
        }
    }

    /// Detect an unsplit critical edge: a block with >1 successor feeding a
    /// block with >1 predecessor. Splitting them is lowering's contract; this
    /// only reports so `allocate` can `debug_assert`.
    fn has_critical_edge(&self) -> Option<(usize, usize)> {
        for (b, succs) in self.block_succs.iter().enumerate() {
            if succs.len() <= 1 {
                continue;
            }
            for &succ in succs {
                // Entry block has an implicit extra predecessor.
                let extra = usize::from(succ.index() == 0);
                if self.block_preds[succ.index()].len() + extra > 1 {
                    return Some((b, succ.index()));
                }
            }
        }
        None
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

    // Critical edges must be pre-split by lowering (see module docs). Report
    // loudly in debug; regalloc2 independently returns `CritEdge` otherwise.
    debug_assert!(
        adapter.has_critical_edge().is_none(),
        "unsplit critical edge {:?}; lowering (P5) must split it",
        adapter.has_critical_edge()
    );

    let env = machine_env();
    let options = RegallocOptions {
        verbose_log: false,
        // Inputs are already SSA-shaped (lowering's contract): a single def per
        // vreg, block params standing in for φ at merges. We leave the extra SSA
        // validator pass off to match the original behaviour and avoid rejecting
        // valid-but-unconventional hand-built MachFuncs.
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
    use crate::t2::mach::{MachBlock, MachBlockId, MachInst, MachSucc};

    fn vreg(class: RegClass, num: u32) -> VReg {
        VReg { class, num }
    }

    fn inst(op: u32, defs: Vec<VReg>, uses: Vec<VReg>) -> MachInst {
        MachInst {
            op,
            defs,
            uses,
            imm: None,
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
            imm: None,
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
            op: 3,
            defs: vec![],
            uses: vec![g0, gm],
            imm: None,
            frame_state: Some(FrameStateId(11)),
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
                        MachSucc { target: MachBlockId(1), args: vec![] },
                        MachSucc { target: MachBlockId(2), args: vec![] },
                    ],
                },
                MachBlock {
                    params: vec![],
                    start: 2,
                    end: 4,
                    succs: vec![MachSucc { target: MachBlockId(3), args: vec![gl] }],
                },
                MachBlock {
                    params: vec![],
                    start: 4,
                    end: 6,
                    succs: vec![MachSucc { target: MachBlockId(3), args: vec![gr] }],
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
        allocate(&mut mf);

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
        allocate(&mut mf);

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
