//! P5 — Lowering / x86-64 instruction selection (spec §4.7 R4.42, §4.7.2).
//!
//! **Parcel P5. Owner: (sub-agent).** Lower a block-based SSA [`Function`] to a
//! [`MachFunc`] of [`MachInst`]s over virtual registers (`mach.rs` is the frozen
//! interface). This is instruction *selection*, not emission: we pick a
//! mnemonic (`op: u32`, encoded by the [`op`] constants below) and the
//! virtual-register operand shape for each IR instruction. Actual byte encoding
//! / addressing-mode assembly is a later emit step (spec §4.7.2 rule 2 folding
//! is therefore only modelled at the mnemonic level here).
//!
//! ## What this pass does
//!
//! * Walks blocks in **layout order** (`Function::block_order`), emitting each
//!   IR block's instructions contiguously into the flat `insts` vector and
//!   recording a parallel [`MachBlock`] whose `[start,end)` covers them (P5b).
//!   `blocks[i]` corresponds to `block_order[i]`; `blocks[0]` is the IR entry.
//! * Maps every SSA [`Value`] to a [`VReg`] whose [`RegClass`] follows the
//!   value's [`ValueRepresentation`] (`Tagged`/`UnboxedFixnum` → `Gpr`;
//!   `UnboxedF32`/`UnboxedF64` → `Xmm`). The VReg number is the value's arena
//!   index, so the mapping is stable and injective.
//! * Selects one or a short sequence of [`MachInst`]s per IR instruction via a
//!   maximal-munch-flavoured `match` (spec §4.7.2). Compares expand to a
//!   flag-setting `CMP`/`UCOMISD` + a `SETcc` materialising the boolean.
//! * Models block parameters + [`BlockCall`] args **two ways** (P5b): the
//!   authoritative out-of-SSA form is now [`MachSucc::args`] on each CFG edge
//!   (regalloc2 / P6b turns block-param args into moves). For backward
//!   compatibility we *also* still emit the inline parallel-move `MachInst`s just
//!   before the terminator; a later parcel can drop the explicit moves once P6b
//!   consumes `MachSucc::args` directly. The two encodings are redundant but
//!   consistent — both name the same (param ← arg) VReg pairs.
//! * Carries the `FrameStateId` and safepoint bit of any guarding / safepointing
//!   IR instruction onto the emitted `MachInst`, so P6 can pin the frame-state
//!   locations and record the GC stack map (spec §4.10 R4.65, §4.7 R4.46).
//!
//! ## Contract gaps worked around here (noted for the leads)
//!
//! * [`MachInst::op`] is an opaque `u32` with no ISA-level operand/addressing
//!   model, immediate fields, or condition-code slot in the frozen `mach.rs`.
//!   We therefore encode the mnemonic in `op` (see [`op`]) and stash any
//!   condition-code / immediate discriminator *in the mnemonic itself* (e.g.
//!   distinct `SETE`/`SETL` opcodes). Constant immediates and memory
//!   displacements have nowhere to live on `MachInst`, so they are implied by
//!   the mnemonic and the (single) def; a real emitter needs an immediate field.
//! * Block structure now lives in `MachFunc::blocks` (P5b): each [`MachBlock`]
//!   carries its `[start,end)` inst range, param VRegs, and `succs` edges with
//!   per-edge VReg args. Individual `MachInst`s still have no successor field,
//!   so the branch *mnemonic* (`JMP`/`BR_COND`/`BR_TABLE`) records only the
//!   condition is held by a zero-code `LIVENESS` anchor immediately before the
//!   mnemonic; the actual targets are on the owning block's `succs`, in
//!   `Function::block_order` position order. The terminator's `targets[k]` maps
//!   1:1 to `succs[k]`, so P6/emit can recover which mnemonic operand selects
//!   which successor edge.
//! * Parallel moves are lowered **sequentially** (no cycle breaking / temp
//!   insertion) and edge moves for the two-way `Brif` / `BrTable` are emitted
//!   before the branch without **critical-edge splitting**. Both are only
//!   correct when edge moves don't form cycles / both edges don't need
//!   conflicting moves; the general fix belongs in P6 alongside real edge
//!   handling. Documented rather than solved because it needs new CFG surface.

use crate::t2::ir::{Function, Inst, Opcode, Value, ValueRepresentation};
use crate::t2::mach::{MachBlock, MachBlockId, MachFunc, MachInst, MachSucc, RegClass, VReg};

/// Backend mnemonic tags encoded into [`MachInst::op`]. The concrete `u32`
/// values are arbitrary but stable; grouped by high nibble for readability.
///
/// Since `mach.rs` gives no place for immediates or condition codes, distinct
/// mnemonics stand in for what would otherwise be operand modifiers.
#[allow(non_upper_case_globals)]
pub mod op {
    // ── 0x00 pseudo / moves ──────────────────────────────────────────
    /// Unselected opcode — a tagged placeholder (spec R4.42 says unlowered
    /// nodes MUST abort compilation; here we mark them so P6/verify can find
    /// them instead of silently dropping the instruction).
    pub const PSEUDO_UNSUPPORTED: u32 = 0x0000;
    /// GPR register-register move (parallel-move lowering, out-of-SSA at edges).
    pub const MOV: u32 = 0x0001;
    /// XMM register-register move (float parallel move).
    pub const FMOV: u32 = 0x0002;
    /// Zero-code allocation anchor. Keeps branch conditions live through edge
    /// moves while leaving the branch mnemonic operand-free for regalloc2.
    pub const LIVENESS: u32 = 0x0003;

    // ── 0x10 constants (immediate materialisation) ───────────────────
    /// Load an integer immediate into a GPR (`mov r, imm`).
    pub const MOV_IMM: u32 = 0x0010;
    /// Materialise a float constant into an XMM reg (`movss`/`movsd` from pool).
    pub const LOAD_FCONST: u32 = 0x0011;
    /// Load a tagged heap/symbol/nil/t constant into a GPR.
    pub const MOV_TAGGED: u32 = 0x0012;

    // ── 0x20 integer ALU (proven-unboxed fixnum, no tag manip) ───────
    pub const ADD: u32 = 0x0020;
    pub const SUB: u32 = 0x0021;
    pub const IMUL: u32 = 0x0022;
    pub const IDIV: u32 = 0x0023;
    pub const IREM: u32 = 0x0024;
    pub const NEG: u32 = 0x0025;
    pub const SHL: u32 = 0x0026;
    pub const SAR: u32 = 0x0027;
    pub const AND: u32 = 0x0028;
    pub const OR: u32 = 0x0029;
    pub const XOR: u32 = 0x002A;
    pub const NOT: u32 = 0x002B;
    /// Sign-extend i32→i64 (`movsxd`).
    pub const MOVSXD: u32 = 0x002C;

    // ── 0x30 float ALU ───────────────────────────────────────────────
    pub const FADD: u32 = 0x0030;
    pub const FSUB: u32 = 0x0031;
    pub const FMUL: u32 = 0x0032;
    pub const FDIV: u32 = 0x0033;

    // ── 0x40 tag manipulation (box / unbox) ──────────────────────────
    /// Box an unboxed fixnum into a tagged value (shift+tag) — modelled as one op.
    pub const BOX_FIXNUM: u32 = 0x0040;
    /// Unbox a tagged fixnum to raw i64 (arithmetic shift right).
    pub const UNBOX_FIXNUM: u32 = 0x0041;
    /// Box an unboxed float (heap-allocates a double / builds immediate single).
    pub const BOX_FLOAT: u32 = 0x0042;
    /// Unbox a tagged float into an XMM register.
    pub const UNBOX_FLOAT: u32 = 0x0043;

    // ── 0x50 compare (flag-setting) + materialise ────────────────────
    /// Integer compare, sets EFLAGS (`cmp a, b`).
    pub const CMP: u32 = 0x0050;
    /// Float ordered compare, sets EFLAGS (`ucomisd`).
    pub const UCOMIS: u32 = 0x0051;
    // SETcc: read EFLAGS into a GPR boolean. One mnemonic per condition since
    // `op` has no condition-code field.
    pub const SETE: u32 = 0x0052;
    pub const SETL: u32 = 0x0053;
    pub const SETLE: u32 = 0x0054;
    pub const SETG: u32 = 0x0055;
    pub const SETGE: u32 = 0x0056;

    // ── 0x60 memory / object access ──────────────────────────────────
    pub const LOAD: u32 = 0x0060;
    pub const STORE: u32 = 0x0061;
    /// Allocate (bump-pointer / runtime call); modelled as a single op.
    pub const ALLOC: u32 = 0x0062;
    /// GC write barrier.
    pub const WRITE_BARRIER: u32 = 0x0063;

    // ── 0x70 calls / guards ──────────────────────────────────────────
    /// Direct/indirect call (clobbers caller-saved; safepoint).
    pub const CALL: u32 = 0x0070;
    /// Call into a runtime helper (generic arithmetic, type checks).
    pub const CALL_RUNTIME: u32 = 0x0071;
    /// Deoptimising guard: a conditional test that branches to the deopt path.
    pub const GUARD: u32 = 0x0072;
    /// A type/tag test (`test`/`cmp` on the tag bits) producing a GPR boolean.
    pub const TAG_TEST: u32 = 0x0073;
    /// Call with an exceptional continuation. Paired with INVOKE_ROUTES in the
    /// same block. This must not be emitted as an ordinary checked CALL.
    pub const INVOKE: u32 = 0x0074;

    // ── 0x80 terminators ─────────────────────────────────────────────
    /// Unconditional branch (`jmp`).
    pub const JMP: u32 = 0x0080;
    /// Conditional branch on a GPR boolean (`test`+`jnz`), two-way.
    pub const BR_COND: u32 = 0x0081;
    /// Jump table / switch dispatch.
    pub const BR_TABLE: u32 = 0x0082;
    /// Function return (`ret`).
    pub const RET: u32 = 0x0083;
    /// Tail call.
    pub const TAILCALL: u32 = 0x0084;
    /// Raise / non-local throw.
    pub const THROW: u32 = 0x0085;
    /// Non-local exit transfer.
    pub const NLX_TRANSFER: u32 = 0x0086;
    /// Deopt trap (unconditional deoptimisation point).
    pub const TRAP: u32 = 0x0087;
    /// Operand-free CFG marker after INVOKE: successor 0 receives a normal
    /// result; successor 1 captures an escaping transfer. This describes an
    /// exceptional edge, not a status test on the successful return path.
    pub const INVOKE_ROUTES: u32 = 0x0088;
}

/// The register class a value with representation `repr` lives in (spec §4.7
/// R4.45): tagged values / unboxed integers / pointers → GPR, unboxed floats →
/// XMM.
#[inline]
pub fn class_of(repr: ValueRepresentation) -> RegClass {
    match repr {
        ValueRepresentation::Tagged | ValueRepresentation::UnboxedFixnum => RegClass::Gpr,
        ValueRepresentation::UnboxedF32 | ValueRepresentation::UnboxedF64 => RegClass::Xmm,
    }
}

/// Per-lowering state: the growing instruction list plus a fresh-VReg counter
/// for temporaries that don't correspond to an SSA value.
struct Lowering<'f> {
    f: &'f Function,
    insts: Vec<MachInst>,
    next_temp: u32,
}

impl<'f> Lowering<'f> {
    fn new(f: &'f Function) -> Self {
        // SSA values own VReg numbers `0..num_values`; temporaries start above.
        Lowering {
            f,
            insts: Vec::new(),
            next_temp: f.num_values() as u32,
        }
    }

    /// The VReg for SSA value `v`: number = arena index, class = its repr.
    fn vreg(&self, v: Value) -> VReg {
        VReg {
            class: class_of(self.f.value(v).repr),
            num: v.0,
        }
    }

    fn vregs(&self, vs: &[Value]) -> Vec<VReg> {
        vs.iter().map(|&v| self.vreg(v)).collect()
    }

    #[allow(dead_code)]
    fn fresh(&mut self, class: RegClass) -> VReg {
        let num = self.next_temp;
        self.next_temp += 1;
        VReg { class, num }
    }

    /// Emit a plain (non-safepoint, non-deopt) MachInst.
    fn emit(&mut self, op: u32, defs: Vec<VReg>, uses: Vec<VReg>) {
        self.insts.push(MachInst {
            source_inst: None,
            op,
            defs,
            uses,
            imm: None,
            frame_state: None,
            deopt_uses: Vec::new(),
            safepoint: false,
        });
    }

    fn emit_for(&mut self, source_inst: Inst, op: u32, defs: Vec<VReg>, uses: Vec<VReg>) {
        self.insts.push(MachInst {
            source_inst: Some(source_inst),
            op,
            defs,
            uses,
            imm: None,
            frame_state: None,
            deopt_uses: Vec::new(),
            safepoint: false,
        });
    }

    /// Emit an immediate-materialising MachInst (a constant load). `imm` is the
    /// value the emitter moves into `def` — for a constant that reaches a
    /// function boundary this is the tagged `TorclVal` bits.
    fn emit_imm(&mut self, source_inst: Inst, op: u32, def: VReg, imm: i64) {
        self.insts.push(MachInst {
            source_inst: Some(source_inst),
            op,
            defs: vec![def],
            uses: vec![],
            imm: Some(imm),
            frame_state: None,
            deopt_uses: Vec::new(),
            safepoint: false,
        });
    }

    /// Emit a MachInst that carries the deopt/safepoint annotations of IR
    /// instruction `i` (spec §4.10 R4.65): a guarding or deoptimising inst keeps
    /// its `FrameStateId`; a safepoint inst sets `safepoint` so P6 records a
    /// stack map. The two are independent (a guard is a safepoint; a plain call
    /// is a safepoint without a guard flag).
    fn emit_annotated(&mut self, i: Inst, op: u32, defs: Vec<VReg>, uses: Vec<VReg>) {
        let data = self.f.inst(i);
        let carries_state = data.flags.guard || data.flags.call || data.frame_state.is_some();
        let frame_state = if carries_state {
            data.frame_state
        } else {
            None
        };
        // Extend liveness of the frame state's register-sourced values to this
        // instruction (bliss-ad1e): without an explicit operand, regalloc2 may
        // free or reuse their registers before the guard, and a deopt would
        // reconstruct the interpreter frame from stale slots. Dedup against
        // the defs and against repeats — but NOT against the instruction's own
        // `uses`: a normal use is an Early-position operand, which lets the
        // allocator coalesce the destination onto it, and a destructive
        // sequence before the deopting `jo` (e.g. FixnumMul's `sar dst,3;
        // imul dst,y`) then clobbers the very register the frame state reads.
        // The extra Late-position Any operand keeps such a value alive PAST
        // the instruction's writes, which is the whole bliss-ad1e guarantee
        // (bliss-x9c9: deopt-resume returned sign-flipped/garbage numbers).
        let mut deopt_uses: Vec<VReg> = Vec::new();
        if let Some(fsid) = frame_state {
            let fs = self.f.frame_states.get(fsid);
            use crate::t2::frame_state::ValueSource;
            let mut sources: Vec<_> = fs
                .scopes
                .iter()
                .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
                .collect();
            let mut visited = vec![false; fs.remat.len()];
            while let Some(source) = sources.pop() {
                match source {
                    ValueSource::Value { value, .. } => {
                        let v = self.vreg(*value);
                        if !defs.contains(&v) && !deopt_uses.contains(&v) {
                            deopt_uses.push(v);
                        }
                    }
                    ValueSource::Remat(id) => {
                        // Recipe-only inputs must survive the guard too. Walk
                        // shared recipes once, without recursive host calls.
                        if let Some(seen) = visited.get_mut(id.0 as usize) {
                            if !*seen {
                                *seen = true;
                                sources.extend(&fs.remat[id.0 as usize].inputs);
                            }
                        }
                    }
                    ValueSource::Const(_) | ValueSource::Unbound => {}
                }
            }
        }
        self.insts.push(MachInst {
            source_inst: Some(i),
            op,
            defs,
            uses,
            imm: None,
            frame_state,
            deopt_uses,
            safepoint: data.flags.safepoint,
        });
    }

    /// Lower the block-argument bindings on one edge into sequential register
    /// moves: `param[k] <- arg[k]` for the target block's parameters. This is
    /// the out-of-SSA parallel-move at the edge (spec §4.7 — modelled without
    /// cycle-breaking; see module-level caveat).
    fn emit_edge_moves(&mut self, target: crate::t2::ir::Block, args: &[Value]) {
        let params = self.f.block(target).params.clone();
        for (k, &arg) in args.iter().enumerate() {
            if k >= params.len() {
                break; // arity mismatch is a verifier (P2) finding, not ours.
            }
            let dst = self.vreg(params[k]);
            let src = self.vreg(arg);
            if dst == src {
                continue; // already in place
            }
            let mov = match dst.class {
                RegClass::Gpr => op::MOV,
                RegClass::Xmm => op::FMOV,
            };
            self.emit(mov, vec![dst], vec![src]);
        }
    }
}

/// Lower `f` to machine instructions over virtual registers (spec §4.7 R4.42).
///
/// Walks blocks in layout order; selects a `MachInst` sequence per IR
/// instruction; models edges as parallel moves; preserves frame-state /
/// safepoint annotations for P6.
pub fn lower(f: &Function) -> MachFunc {
    let mut lo = Lowering::new(f);
    let order = f.block_order();

    // IR `Block` → `MachBlockId`. Because we emit one `MachBlock` per IR block in
    // layout order, the MachBlockId of a block is its position in `block_order`.
    // A block not in the layout (only possible in malformed IR) maps to u32::MAX;
    // that is a P2 verifier finding, so we record the sentinel rather than panic.
    let mut mach_id = vec![u32::MAX; f.num_blocks()];
    for (i, &b) in order.iter().enumerate() {
        mach_id[b.index()] = i as u32;
    }

    let mut blocks: Vec<MachBlock> = Vec::with_capacity(order.len());

    for &block in order {
        let start = lo.insts.len();
        // Block parameters become this block's `MachBlock::params` (regalloc2's
        // φ replacement). VReg number = the param SSA value's arena index.
        let params = lo.vregs(&f.block(block).params);

        // Instructions in program order; the last is the terminator (if the
        // block is finished — an unfinished block just has no terminator).
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            if data.opcode.is_terminator() {
                lower_terminator(&mut lo, inst);
            } else {
                lower_inst(&mut lo, inst);
            }
        }
        let end = lo.insts.len();

        // One `MachSucc` per terminator `BlockCall`: target = the successor's
        // MachBlockId, args = the VRegs of the BlockCall args (bound to the
        // target block's params on this edge). `targets[k]` ↔ `succs[k]`.
        let succs: Vec<MachSucc> = match f.terminator(block) {
            Some(t) => f
                .inst(t)
                .targets
                .iter()
                .map(|c| MachSucc {
                    target: MachBlockId(mach_id[c.block.index()]),
                    args: lo.vregs(&c.args),
                })
                .collect(),
            None => Vec::new(),
        };

        blocks.push(MachBlock {
            params,
            start,
            end,
            succs,
        });
    }

    // regalloc2 requires critical CFG edges to be split. Keep this mechanical
    // at the machine layer: the synthetic block has no params, and forwards
    // the original SSA edge arguments on its sole outgoing jump. Thus the
    // conditional predecessor carries no block arguments, while the merge's
    // block parameters retain exactly their original incoming values.
    let mut pred_counts = vec![0usize; blocks.len()];
    for block in &blocks {
        for succ in &block.succs {
            pred_counts[succ.target.0 as usize] += 1;
        }
    }
    let mut critical_edges = Vec::new();
    for (pred, block) in blocks.iter().enumerate() {
        if block.succs.len() <= 1 {
            continue;
        }
        for (succ_index, succ) in block.succs.iter().enumerate() {
            if pred_counts[succ.target.0 as usize] > 1 {
                critical_edges.push((pred, succ_index));
            }
        }
    }
    for (pred, succ_index) in critical_edges {
        let forwarded = blocks[pred].succs[succ_index].clone();
        let edge_id = MachBlockId(blocks.len() as u32);
        blocks[pred].succs[succ_index] = MachSucc {
            target: edge_id,
            args: Vec::new(),
        };
        let start = lo.insts.len();
        lo.emit(op::JMP, Vec::new(), Vec::new());
        blocks.push(MachBlock {
            params: Vec::new(),
            start,
            end: lo.insts.len(),
            succs: vec![forwarded],
        });
    }

    MachFunc {
        insts: lo.insts,
        blocks,
        allocation: Vec::new(),
        inst_allocations: Vec::new(),
        allocation_edits: Vec::new(),
        num_spill_slots: 0,
        value_locations: Vec::new(),
        stack_maps: Vec::new(),
    }
}

/// Select a non-terminator IR instruction (spec §4.7.2 maximal munch).
fn lower_inst(lo: &mut Lowering, inst: Inst) {
    use Opcode::*;
    let data = lo.f.inst(inst);
    let defs = lo.vregs(&data.results);
    let uses = lo.vregs(&data.args);

    // Non-moving constants materialise a tagged immediate the emitter can move
    // directly (spec §4.7). Heap literals instead define an allocated value and
    // are loaded from their rooted pool slot by the rich emitter.
    // (Unboxed-representation materialisation + box/unbox insertion is future
    // work; a bare constant feeding a Return is naturally tagged.)
    {
        use crate::t2::ir::AuxData;
        use torcl_rt::value::{NIL_BITS, T_BITS};
        let imm: Option<i64> = match (data.opcode, &data.aux) {
            (ConstFixnum, AuxData::FixnumImm(v)) => {
                Some(torcl_rt::value::TorclVal::from_fixnum(*v).0 as i64)
            }
            (ConstNil, _) => Some(NIL_BITS as i64),
            (ConstT, _) => Some(T_BITS as i64),
            _ => None,
        };
        if let Some(imm) = imm {
            if let Some(&def) = defs.first() {
                let op = if data.opcode == ConstFixnum {
                    op::MOV_IMM
                } else {
                    op::MOV_TAGGED
                };
                lo.emit_imm(inst, op, def, imm);
                return;
            }
        }
    }

    // A binary-op emitter: `op def, use0, use1`.
    //
    // These go through `emit_annotated`, not `emit_for` (bliss-x9c9): a
    // SPECULATED arithmetic op is a deopt point — the emitter's template ends
    // in `jo deopt` and its FrameState reconstructs the T0 frame — so its
    // MachInst must carry that FrameState or regalloc2 never learns the
    // deopt-live values have to survive the instruction's own writes. Without
    // it the allocator coalesced the result onto an operand register that the
    // destructive `sar dst,3; imul dst,y` sequence clobbers before the `jo`,
    // and the deopt read the wrecked register (wrong numeric results at T2).
    // For a non-deopting op `emit_annotated` records nothing extra.
    macro_rules! bin {
        ($op:expr) => {{
            lo.emit_annotated(inst, $op, defs, uses);
        }};
    }
    // A unary-op emitter: `op def, use0`.
    macro_rules! un {
        ($op:expr) => {{
            lo.emit_annotated(inst, $op, defs, uses);
        }};
    }
    // A comparison: flag-setting compare then SETcc into the GPR result. The
    // SETcc half carries the state: it writes the result register, which is
    // where a coalesced operand would be lost before the guard's deopt.
    macro_rules! cmp {
        ($cmpop:expr, $setcc:expr) => {{
            lo.emit_for(inst, $cmpop, vec![], uses);
            lo.emit_annotated(inst, $setcc, defs, vec![]);
        }};
    }

    match data.opcode {
        // ── constants ──
        ConstFixnum => un!(op::MOV_IMM),
        ConstFloat => un!(op::LOAD_FCONST),
        ConstChar => un!(op::MOV_IMM),
        ConstSymbol | ConstNil | ConstT | ConstHeapObj => un!(op::MOV_TAGGED),

        // ── unboxed-fixnum arithmetic (native, no tag manip) ──
        FixnumAdd => bin!(op::ADD),
        FixnumSub => bin!(op::SUB),
        FixnumMul => bin!(op::IMUL),
        FixnumDiv => bin!(op::IDIV),
        FixnumRem | FixnumMod => bin!(op::IREM),
        FixnumNeg => un!(op::NEG),
        FixnumShl => bin!(op::SHL),
        FixnumShr => bin!(op::SAR),

        // ── float arithmetic ──
        FloatAdd => bin!(op::FADD),
        FloatSub => bin!(op::FSUB),
        FloatMul => bin!(op::FMUL),
        FloatDiv => bin!(op::FDIV),

        // ── logic ──
        LogAnd => bin!(op::AND),
        LogOr => bin!(op::OR),
        LogXor => bin!(op::XOR),
        LogNot => un!(op::NOT),

        // ── box / unbox / widen ──
        BoxFixnum => un!(op::BOX_FIXNUM),
        UnboxFixnum => un!(op::UNBOX_FIXNUM),
        BoxFloat => un!(op::BOX_FLOAT),
        UnboxFloat => un!(op::UNBOX_FLOAT),
        WidenI32 => un!(op::MOVSXD),

        // ── compares (flags + SETcc) ──
        FixnumCmpEq => cmp!(op::CMP, op::SETE),
        FixnumCmpLt => cmp!(op::CMP, op::SETL),
        FixnumCmpLe => cmp!(op::CMP, op::SETLE),
        FixnumCmpGt => cmp!(op::CMP, op::SETG),
        FixnumCmpGe => cmp!(op::CMP, op::SETGE),
        FloatCmpEq => cmp!(op::UCOMIS, op::SETE),
        FloatCmpLt => cmp!(op::UCOMIS, op::SETL),

        // ── generic (megamorphic) arith / equality → runtime helper ──
        GenericAdd | GenericSub | GenericMul | GenericDiv | GenericEq | GenericEqual => {
            lo.emit_annotated(inst, op::CALL_RUNTIME, defs, uses);
        }

        // ── type / instance checks ──
        TypeCheck | InstanceOf => {
            // A guarding type check carries a frame state; a pure predicate does not.
            lo.emit_annotated(inst, op::TAG_TEST, defs, uses);
        }

        // ── memory loads (may be effectful) ──
        Load | Car | Cdr | VecRef => un_or_bin(lo, inst, op::LOAD, defs, uses),
        StringByteLength | StringAsciiCharAt => lo.emit_annotated(inst, op::LOAD, defs, uses),
        // A global read/write is emitted as a c2i helper CALL
        // (c2i_load_global / c2i_store_global), which clobbers every
        // caller-saved register — modelling it as a plain load/store let the
        // allocator keep a live value in a caller-saved register across it
        // (latent corruption; found during bliss-x5y.29's zero-push audit).
        SymbolValue | SymbolFunction | SetSymbolValue => {
            lo.emit_annotated(inst, op::CALL_RUNTIME, defs, uses);
        }

        // ── memory stores ──
        Store | SetCar | SetCdr | VecSet => {
            lo.emit_annotated(inst, op::STORE, defs, uses);
        }
        WriteBarrier => lo.emit_annotated(inst, op::WRITE_BARRIER, defs, uses),

        // ── multiple-values reset → runtime helper (no defs/uses) ──
        ClearMv | TakeValuesToLocals => lo.emit_annotated(inst, op::CALL_RUNTIME, defs, uses),

        // ── allocation (safepoint-bearing) ──
        Alloc | AllocCons => lo.emit_annotated(inst, op::ALLOC, defs, uses),

        // ── calls & guards (safepoint / deopt carriers) ──
        Call => lo.emit_annotated(inst, op::CALL, defs, uses),
        Guard => lo.emit_annotated(inst, op::GUARD, defs, uses),

        // Terminators are handled elsewhere; reaching here is a bug.
        Invoke | Jump | Brif | BrTable | Return | TailCall | Throw | NlxTransfer | Trap => {
            lo.emit_for(inst, op::PSEUDO_UNSUPPORTED, defs, uses);
        }
    }
}

/// Helper for load-shaped ops: `def <- [uses...]`. Preserves effect/safepoint
/// annotations so an effectful load records a stack map.
fn un_or_bin(lo: &mut Lowering, inst: Inst, op: u32, defs: Vec<VReg>, uses: Vec<VReg>) {
    lo.emit_annotated(inst, op, defs, uses);
}

/// Select a terminator: emit edge parallel-moves first (out-of-SSA), then the
/// branch/return mnemonic. Spec §4.7.2 rule (4) would fuse compare+branch; we
/// keep them separate because `MachInst` has no fused-operand model.
fn lower_terminator(lo: &mut Lowering, inst: Inst) {
    use Opcode::*;
    let data = lo.f.inst(inst);

    match data.opcode {
        Invoke => {
            let defs = lo.vregs(&data.results);
            let uses = lo.vregs(&data.args);
            lo.emit_annotated(inst, op::INVOKE, defs, uses);
            // regalloc2 requires an operand-free branch instruction. Keep the
            // call's definitions/uses separate from its two CFG successors.
            // Unlike ordinary branch lowering, do not eagerly copy edge args:
            // the result exists only on the normal route. Regalloc owns edge
            // moves through MachSucc::args and the destination block params.
            lo.emit_for(inst, op::INVOKE_ROUTES, vec![], vec![]);
        }
        Jump => {
            // Single successor: safe sequential edge moves, then jmp.
            let t = &data.targets[0];
            let (blk, args) = (t.block, t.args.clone());
            lo.emit_edge_moves(blk, &args);
            lo.emit_for(inst, op::JMP, vec![], vec![]);
        }
        Brif => {
            // Two-way branch on args[0]. Edge moves for both successors are
            // emitted before the branch (no critical-edge split — caveat above).
            let cond = lo.vregs(&data.args);
            for t in &data.targets.clone() {
                lo.emit_edge_moves(t.block, &t.args);
            }
            lo.emit_for(inst, op::LIVENESS, vec![], cond);
            lo.emit_for(inst, op::BR_COND, vec![], vec![]);
        }
        BrTable => {
            let idx = lo.vregs(&data.args);
            for t in &data.targets.clone() {
                lo.emit_edge_moves(t.block, &t.args);
            }
            lo.emit_for(inst, op::LIVENESS, vec![], idx);
            lo.emit_for(inst, op::BR_TABLE, vec![], vec![]);
        }
        Return => {
            // Return values are uses of the ret; no successors.
            let uses = lo.vregs(&data.args);
            lo.emit_for(inst, op::RET, vec![], uses);
        }
        TailCall => {
            let uses = lo.vregs(&data.args);
            lo.emit_annotated(inst, op::TAILCALL, vec![], uses);
        }
        Throw => {
            let uses = lo.vregs(&data.args);
            lo.emit_annotated(inst, op::THROW, vec![], uses);
        }
        NlxTransfer => {
            let uses = lo.vregs(&data.args);
            lo.emit_annotated(inst, op::NLX_TRANSFER, vec![], uses);
        }
        Trap => {
            // A trap is an unconditional deopt point: carry its frame state.
            let uses = lo.vregs(&data.args);
            lo.emit_annotated(inst, op::TRAP, vec![], uses);
        }
        // Non-terminators never reach here.
        _ => lo.emit_for(inst, op::PSEUDO_UNSUPPORTED, vec![], vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameState, FrameStateId};
    use crate::t2::ir::{
        AuxData, BlockCall, IRType, InstData, InstFlags, TypeBits, ValueRepresentation,
    };

    /// Build an `InstData` with the common defaults filled in.
    fn inst(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn ufix() -> (IRType, ValueRepresentation) {
        (
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        )
    }
    fn uf64() -> (IRType, ValueRepresentation) {
        (
            IRType::of(TypeBits::DOUBLE_FLOAT),
            ValueRepresentation::UnboxedF64,
        )
    }

    #[test]
    fn rematerialized_inputs_remain_deopt_live_at_the_guard() {
        use crate::t2::frame_state::{
            FrameScope, RematOp, RematRecipe, RematRecipeId, ValueSource,
        };
        let mut f = Function::new("recipe-liveness");
        let block = f.entry();
        let x = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let checked = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let source = |value| ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        };
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 1,
                bcp: 9,
                locals: vec![ValueSource::Remat(RematRecipeId(0))],
                stack: vec![source(checked)],
            }],
            remat: vec![
                RematRecipe {
                    op: RematOp::FixnumAdd,
                    inputs: vec![
                        ValueSource::Remat(RematRecipeId(1)),
                        ValueSource::Remat(RematRecipeId(1)),
                    ],
                    result_repr: ValueRepresentation::Tagged,
                },
                RematRecipe {
                    op: RematOp::Const,
                    inputs: vec![source(x)],
                    result_repr: ValueRepresentation::Tagged,
                },
            ],
        });
        let mut guard = inst(
            Opcode::Guard,
            vec![checked],
            AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
        );
        guard.frame_state = Some(state);
        let (guard, result) =
            f.push_inst(block, guard, &[(IRType::TOP, ValueRepresentation::Tagged)]);
        f.set_terminator(block, inst(Opcode::Return, vec![result[0]], AuxData::None));
        let machine = lower(&f);
        let guard = machine
            .insts
            .iter()
            .find(|i| i.source_inst == Some(guard))
            .unwrap();
        assert_eq!(
            guard.deopt_uses.iter().filter(|v| v.num == x.0).count(),
            1,
            "shared nested recipe input must have one late use"
        );
        assert!(guard.deopt_uses.iter().any(|v| v.num == checked.0));
    }

    #[test]
    fn const_add_return_lowers_to_gpr_sequence_ending_in_ret() {
        let mut f = Function::new("add");
        let entry = f.entry();

        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)),
            &[ufix()],
        );
        let (_, b) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(3)),
            &[ufix()],
        );
        let (_, c) = f.push_inst(
            entry,
            inst(Opcode::FixnumAdd, vec![a[0], b[0]], AuxData::None),
            &[ufix()],
        );
        f.set_terminator(entry, inst(Opcode::Return, vec![c[0]], AuxData::None));

        let mf = lower(&f);

        // Expect: MOV_IMM, MOV_IMM, ADD, RET.
        let ops: Vec<u32> = mf.insts.iter().map(|m| m.op).collect();
        assert_eq!(ops, vec![op::MOV_IMM, op::MOV_IMM, op::ADD, op::RET]);

        // The ADD's operands are all GPR virtual registers.
        let add = &mf.insts[2];
        assert_eq!(add.op, op::ADD);
        assert_eq!(add.defs.len(), 1);
        assert_eq!(add.uses.len(), 2);
        for r in add.defs.iter().chain(add.uses.iter()) {
            assert_eq!(
                r.class,
                RegClass::Gpr,
                "unboxed-fixnum operands must be GPR"
            );
        }

        // The sequence ends in a return-shaped instruction whose use is the sum.
        let ret = mf.insts.last().unwrap();
        assert_eq!(ret.op, op::RET);
        assert_eq!(ret.uses.len(), 1);
        assert_eq!(ret.uses[0].class, RegClass::Gpr);
    }

    #[test]
    fn float_add_produces_xmm_vregs() {
        let mut f = Function::new("fadd");
        let entry = f.entry();

        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstFloat, vec![], AuxData::FloatImm(1.5)),
            &[uf64()],
        );
        let (_, b) = f.push_inst(
            entry,
            inst(Opcode::ConstFloat, vec![], AuxData::FloatImm(2.5)),
            &[uf64()],
        );
        let (_, c) = f.push_inst(
            entry,
            inst(Opcode::FloatAdd, vec![a[0], b[0]], AuxData::None),
            &[uf64()],
        );
        f.set_terminator(entry, inst(Opcode::Return, vec![c[0]], AuxData::None));

        let mf = lower(&f);
        let fadd = mf
            .insts
            .iter()
            .find(|m| m.op == op::FADD)
            .expect("float add selected");
        assert_eq!(fadd.defs.len(), 1);
        assert_eq!(fadd.uses.len(), 2);
        for r in fadd.defs.iter().chain(fadd.uses.iter()) {
            assert_eq!(r.class, RegClass::Xmm, "unboxed floats must live in XMM");
        }
        // The float constants were also materialised into XMM registers.
        let fconst = mf.insts.iter().find(|m| m.op == op::LOAD_FCONST).unwrap();
        assert_eq!(fconst.defs[0].class, RegClass::Xmm);
    }

    #[test]
    fn call_carries_frame_state_and_safepoint() {
        let mut f = Function::new("call");
        let entry = f.entry();

        // A tagged argument for the call.
        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstSymbol, vec![], AuxData::SymbolRef(7)),
            &[(IRType::of(TypeBits::SYMBOL), ValueRepresentation::Tagged)],
        );

        // Intern a (minimal) frame state and attach it to a safepointing call.
        let fsid: FrameStateId = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let mut call = inst(Opcode::Call, vec![a[0]], AuxData::CallTarget(1));
        call.flags.call = true;
        call.flags.safepoint = true;
        call.frame_state = Some(fsid);
        let (_, r) = f.push_inst(entry, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);

        f.set_terminator(entry, inst(Opcode::Return, vec![r[0]], AuxData::None));

        let mf = lower(&f);
        let call_mi = mf
            .insts
            .iter()
            .find(|m| m.op == op::CALL)
            .expect("call selected");
        assert!(call_mi.safepoint, "call MachInst must be a safepoint");
        assert_eq!(
            call_mi.frame_state,
            Some(fsid),
            "call must carry its frame state"
        );
        // Its argument and result are tagged GPRs.
        assert_eq!(call_mi.uses[0].class, RegClass::Gpr);
        assert_eq!(call_mi.defs[0].class, RegClass::Gpr);
    }

    #[test]
    fn guard_carries_frame_state() {
        let mut f = Function::new("guard");
        let entry = f.entry();
        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
            &[ufix()],
        );

        let fsid = f.frame_states.add(FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let mut g = inst(Opcode::Guard, vec![a[0]], AuxData::None);
        g.flags.guard = true;
        g.flags.safepoint = true;
        g.frame_state = Some(fsid);
        f.push_inst(entry, g, &[]);
        f.set_terminator(entry, inst(Opcode::Return, vec![], AuxData::None));

        let mf = lower(&f);
        let guard_mi = mf
            .insts
            .iter()
            .find(|m| m.op == op::GUARD)
            .expect("guard selected");
        assert_eq!(guard_mi.frame_state, Some(fsid));
        assert!(guard_mi.safepoint);
    }

    #[test]
    fn block_params_become_edge_moves() {
        // entry: const -> jump(merge, [v]); merge(p): return p
        let mut f = Function::new("edge");
        let entry = f.entry();
        let merge = f.make_block();
        let p = f.add_block_param(
            merge,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );

        let (_, v) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(9)),
            &[ufix()],
        );
        let mut jmp = inst(Opcode::Jump, vec![], AuxData::None);
        jmp.targets = vec![BlockCall {
            block: merge,
            args: vec![v[0]],
        }];
        f.set_terminator(entry, jmp);
        f.set_terminator(merge, inst(Opcode::Return, vec![p], AuxData::None));

        let mf = lower(&f);
        // A MOV move for the edge binding p <- v must precede the JMP.
        let mov_idx = mf
            .insts
            .iter()
            .position(|m| m.op == op::MOV)
            .expect("edge move emitted");
        let jmp_idx = mf.insts.iter().position(|m| m.op == op::JMP).unwrap();
        assert!(mov_idx < jmp_idx, "edge move must precede the branch");
        let mov = &mf.insts[mov_idx];
        assert_eq!(mov.defs.len(), 1);
        assert_eq!(mov.uses.len(), 1);
        assert_eq!(mov.defs[0].num, p.0, "move targets the block param's vreg");
        assert_eq!(mov.uses[0].num, v[0].0, "move sources the arg's vreg");
    }

    #[test]
    fn unhandled_terminator_never_placeholders_the_arith_path() {
        // Sanity: the full opcode subset we claim to cover never emits
        // PSEUDO_UNSUPPORTED for the const+add+return shape.
        let mut f = Function::new("nostub");
        let entry = f.entry();
        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)),
            &[ufix()],
        );
        let (_, c) = f.push_inst(
            entry,
            inst(Opcode::FixnumNeg, vec![a[0]], AuxData::None),
            &[ufix()],
        );
        f.set_terminator(entry, inst(Opcode::Return, vec![c[0]], AuxData::None));
        let mf = lower(&f);
        assert!(mf.insts.iter().all(|m| m.op != op::PSEUDO_UNSUPPORTED));
    }

    /// Is `op` a block-ending branch/return MachInst mnemonic?
    fn is_block_terminator(op: u32) -> bool {
        matches!(
            op,
            op::JMP
                | op::BR_COND
                | op::BR_TABLE
                | op::RET
                | op::TAILCALL
                | op::THROW
                | op::NLX_TRANSFER
                | op::TRAP
        )
    }

    #[test]
    fn straight_line_yields_exactly_one_block() {
        // const + add + return in the entry block: a single MachBlock covering
        // the whole flat inst list, no params, no successors.
        let mut f = Function::new("straight");
        let entry = f.entry();
        let (_, a) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(2)),
            &[ufix()],
        );
        let (_, b) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(3)),
            &[ufix()],
        );
        let (_, c) = f.push_inst(
            entry,
            inst(Opcode::FixnumAdd, vec![a[0], b[0]], AuxData::None),
            &[ufix()],
        );
        f.set_terminator(entry, inst(Opcode::Return, vec![c[0]], AuxData::None));

        let mf = lower(&f);
        assert_eq!(
            mf.blocks.len(),
            1,
            "straight-line function is one MachBlock"
        );
        let blk = &mf.blocks[0];
        assert_eq!(blk.start, 0);
        assert_eq!(blk.end, mf.insts.len(), "block covers the whole inst list");
        assert!(blk.params.is_empty(), "entry has no block parameters here");
        assert!(blk.succs.is_empty(), "a returning block has no successors");
        // The block ends in a return-shaped MachInst.
        assert!(is_block_terminator(mf.insts[blk.end - 1].op));
        assert_eq!(mf.insts[blk.end - 1].op, op::RET);
    }

    #[test]
    fn branch_merge_lowers_to_block_cfg() {
        // Diamond:
        //   entry: cond, x=10, y=20 ; brif cond -> [left, right]
        //   left:  jump merge(x)
        //   right: jump merge(y)
        //   merge(p): return p
        let mut f = Function::new("diamond");
        let entry = f.entry();
        let left = f.make_block();
        let right = f.make_block();
        let merge = f.make_block();
        let p = f.add_block_param(
            merge,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );

        let (_, cond) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
            &[ufix()],
        );
        let (_, x) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(10)),
            &[ufix()],
        );
        let (_, y) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(20)),
            &[ufix()],
        );
        let mut brif = inst(Opcode::Brif, vec![cond[0]], AuxData::None);
        brif.targets = vec![
            BlockCall {
                block: left,
                args: vec![],
            },
            BlockCall {
                block: right,
                args: vec![],
            },
        ];
        f.set_terminator(entry, brif);

        let mut ljmp = inst(Opcode::Jump, vec![], AuxData::None);
        ljmp.targets = vec![BlockCall {
            block: merge,
            args: vec![x[0]],
        }];
        f.set_terminator(left, ljmp);

        let mut rjmp = inst(Opcode::Jump, vec![], AuxData::None);
        rjmp.targets = vec![BlockCall {
            block: merge,
            args: vec![y[0]],
        }];
        f.set_terminator(right, rjmp);

        f.set_terminator(merge, inst(Opcode::Return, vec![p], AuxData::None));

        let mf = lower(&f);

        // Four IR blocks → four MachBlocks, in layout order; ≥3 as required.
        assert!(
            mf.blocks.len() >= 3,
            "branch/merge yields at least 3 blocks"
        );
        assert_eq!(mf.blocks.len(), 4);

        // Ranges partition the flat inst list contiguously and each block ends in
        // a branch/return MachInst.
        assert_eq!(mf.blocks[0].start, 0, "blocks[0] is the entry, starts at 0");
        assert_eq!(mf.blocks.last().unwrap().end, mf.insts.len());
        for (i, blk) in mf.blocks.iter().enumerate() {
            assert!(blk.start < blk.end, "block {i} is non-empty");
            if i + 1 < mf.blocks.len() {
                assert_eq!(blk.end, mf.blocks[i + 1].start, "blocks are contiguous");
            }
            assert!(
                is_block_terminator(mf.insts[blk.end - 1].op),
                "block {i} ends in a branch/return MachInst"
            );
        }

        let (mb_entry, mb_left, mb_right, mb_merge) =
            (&mf.blocks[0], &mf.blocks[1], &mf.blocks[2], &mf.blocks[3]);

        // The merge block carries the block param VReg.
        assert_eq!(mb_merge.params.len(), 1);
        assert_eq!(mb_merge.params[0].num, p.0, "merge param is p's vreg");
        assert_eq!(mb_merge.params[0].class, RegClass::Gpr);

        // Entry has two successors (left, right) with no edge args; targets map
        // 1:1 to succs, so MachBlockIds are the layout positions 1 and 2.
        assert_eq!(mb_entry.succs.len(), 2);
        assert_eq!(mb_entry.succs[0].target, MachBlockId(1));
        assert_eq!(mb_entry.succs[1].target, MachBlockId(2));
        assert!(mb_entry.succs[0].args.is_empty());
        assert!(mb_entry.succs[1].args.is_empty());

        // Predecessors of the merge each have one successor edge into merge
        // (MachBlockId 3) whose args carry the right source VReg.
        assert_eq!(mb_left.succs.len(), 1);
        assert_eq!(mb_left.succs[0].target, MachBlockId(3));
        assert_eq!(mb_left.succs[0].args.len(), 1);
        assert_eq!(mb_left.succs[0].args[0].num, x[0].0, "left edge carries x");

        assert_eq!(mb_right.succs.len(), 1);
        assert_eq!(mb_right.succs[0].target, MachBlockId(3));
        assert_eq!(mb_right.succs[0].args.len(), 1);
        assert_eq!(
            mb_right.succs[0].args[0].num, y[0].0,
            "right edge carries y"
        );

        // The merge (returning) block has no successors.
        assert!(mb_merge.succs.is_empty());

        // Backward-compat: the inline edge moves are still emitted (p <- x in the
        // left block, p <- y in the right block), preceding each JMP.
        let left_moves: Vec<&MachInst> = mf.insts[mb_left.start..mb_left.end]
            .iter()
            .filter(|m| m.op == op::MOV)
            .collect();
        assert_eq!(left_moves.len(), 1);
        assert_eq!(left_moves[0].defs[0].num, p.0);
        assert_eq!(left_moves[0].uses[0].num, x[0].0);
    }

    #[test]
    fn critical_merge_edge_is_split_for_regalloc() {
        let mut f = Function::new("critical-edge");
        let entry = f.entry();
        let left = f.make_block();
        let merge = f.make_block();
        let p = f.add_block_param(
            merge,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let (_, cond) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(1)),
            &[ufix()],
        );
        let (_, x) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(10)),
            &[ufix()],
        );
        let (_, y) = f.push_inst(
            entry,
            inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(20)),
            &[ufix()],
        );
        let mut branch = inst(Opcode::Brif, vec![cond[0]], AuxData::None);
        branch.targets = vec![
            BlockCall {
                block: left,
                args: vec![],
            },
            BlockCall {
                block: merge,
                args: vec![y[0]],
            },
        ];
        f.set_terminator(entry, branch);
        let mut jump = inst(Opcode::Jump, vec![], AuxData::None);
        jump.targets = vec![BlockCall {
            block: merge,
            args: vec![x[0]],
        }];
        f.set_terminator(left, jump);
        f.set_terminator(merge, inst(Opcode::Return, vec![p], AuxData::None));

        let mut mf = lower(&f);
        assert_eq!(mf.blocks.len(), 4, "one synthetic edge block is appended");
        let edge = &mf.blocks[3];
        assert_eq!(mf.blocks[0].succs[1].target, MachBlockId(3));
        assert!(mf.blocks[0].succs[1].args.is_empty());
        assert_eq!(edge.succs[0].target, MachBlockId(2));
        assert_eq!(edge.succs[0].args[0].num, y[0].0);
        assert!(mf.insts[mf.blocks[0].end - 1].uses.is_empty());
        assert_eq!(mf.insts[mf.blocks[0].end - 2].op, op::LIVENESS);
        assert_eq!(mf.insts[mf.blocks[0].end - 2].uses[0].num, cond[0].0);
        crate::t2::regalloc::allocate_framed(&mut mf)
            .expect("split edge and operand-free branch allocate");
    }
}
