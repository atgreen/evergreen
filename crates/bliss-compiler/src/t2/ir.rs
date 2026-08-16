//! T2 block-based SSA IR — core types (spec §4.3).
//!
//! **Phase-0 foundation / frozen contract.** Every T2 parcel builds against the
//! types and signatures here. The arena/CFG mechanics are implemented; the
//! parcel-owned algorithms (IR construction from bytecode → P1; verification →
//! P2) live in their own files and are only referenced here.
//!
//! A `Function` is a CFG of `Block`s over three arenas (`blocks`, `insts`,
//! `values`). Data flow is SSA values (instruction results + typed block
//! parameters); control flow is terminator instructions carrying `BlockCall`
//! successor edges; effect ordering is the program order of effectful
//! instructions within a block. There is no memory-token chain and no Region/Phi
//! (spec §4.3).

use crate::t2::frame_state::FrameStateId;

// ── Handles ─────────────────────────────────────────────────────────

macro_rules! handle {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
        pub struct $name(pub u32);
        impl $name {
            #[inline] pub fn index(self) -> usize { self.0 as usize }
        }
    };
}

handle!(/// A basic block.
    Block);
handle!(/// An instruction.
    Inst);
handle!(/// An SSA value (an instruction result or a block parameter).
    Value);

// ── Representation & type ───────────────────────────────────────────

/// The machine representation of an SSA value, orthogonal to its CL type
/// (spec §4.10 D4.16). Box/unbox instructions change representation, not type.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ValueRepresentation {
    /// A tagged `BlissVal` in a GPR; the only representation an interpreter slot
    /// can hold, and the only one that may be a GC pointer.
    Tagged,
    /// A raw untagged fixnum (`i64`) in a GPR.
    UnboxedFixnum,
    /// A raw `f32` in an XMM register (the immediate single-float, untagged).
    UnboxedF32,
    /// A raw `f64` in an XMM register (a boxed CL double reboxed on deopt).
    UnboxedF64,
}

/// Bitset of possible CL type tags (spec §4.3.2.6). One bit per major type.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct TypeBits(pub u16);

impl TypeBits {
    pub const BOTTOM: TypeBits = TypeBits(0);
    pub const FIXNUM: TypeBits = TypeBits(1 << 0);
    pub const BIGNUM: TypeBits = TypeBits(1 << 1);
    pub const RATIO: TypeBits = TypeBits(1 << 2);
    pub const SINGLE_FLOAT: TypeBits = TypeBits(1 << 3);
    pub const DOUBLE_FLOAT: TypeBits = TypeBits(1 << 4);
    pub const COMPLEX: TypeBits = TypeBits(1 << 5);
    pub const CHARACTER: TypeBits = TypeBits(1 << 6);
    pub const CONS: TypeBits = TypeBits(1 << 7);
    pub const NULL: TypeBits = TypeBits(1 << 8);
    pub const SYMBOL: TypeBits = TypeBits(1 << 9);
    pub const STRING: TypeBits = TypeBits(1 << 10);
    pub const VECTOR: TypeBits = TypeBits(1 << 11);
    pub const FUNCTION: TypeBits = TypeBits(1 << 12);
    pub const INSTANCE: TypeBits = TypeBits(1 << 13);
    pub const OTHER_HEAP: TypeBits = TypeBits(1 << 14);
    pub const TOP: TypeBits = TypeBits(0x7FFF);

    #[inline] pub fn meet(self, o: TypeBits) -> TypeBits { TypeBits(self.0 & o.0) } // intersection
    #[inline] pub fn join(self, o: TypeBits) -> TypeBits { TypeBits(self.0 | o.0) } // union
    #[inline] pub fn is_bottom(self) -> bool { self.0 == 0 }
    #[inline] pub fn contains(self, o: TypeBits) -> bool { self.0 & o.0 == o.0 }
}

/// Inclusive integer sub-range refinement, load-bearing for overflow-guard
/// elimination (spec §4.10 T2-c).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Range {
    pub lo: i64,
    pub hi: i64,
}

/// The IR type lattice (spec D4.08): a tag bitset plus optional refinements.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct IRType {
    pub bits: TypeBits,
    pub range: Option<Range>,
    pub class_id: Option<u32>,
}

impl IRType {
    pub const BOTTOM: IRType = IRType { bits: TypeBits::BOTTOM, range: None, class_id: None };
    pub const TOP: IRType = IRType { bits: TypeBits::TOP, range: None, class_id: None };
    pub fn of(bits: TypeBits) -> IRType { IRType { bits, range: None, class_id: None } }
}

// ── Opcodes & instructions ──────────────────────────────────────────

/// Instruction opcodes (spec §4.3.2.5). Representative-but-complete set; extend
/// as parcels need. Terminators are the last group and set `InstFlags.terminator`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Opcode {
    // Cat 1 — constants (payload in AuxData). Block parameters are the params.
    ConstFixnum, ConstFloat, ConstChar, ConstSymbol, ConstNil, ConstT, ConstHeapObj,
    // Cat 2 — arithmetic / logic
    FixnumAdd, FixnumSub, FixnumMul, FixnumDiv, FixnumRem, FixnumMod, FixnumNeg,
    FixnumShl, FixnumShr,
    FloatAdd, FloatSub, FloatMul, FloatDiv,
    GenericAdd, GenericSub, GenericMul, GenericDiv,
    LogAnd, LogOr, LogXor, LogNot,
    BoxFixnum, UnboxFixnum, BoxFloat, UnboxFloat, WidenI32,
    // Cat 3 — comparison / type checks
    FixnumCmpEq, FixnumCmpLt, FixnumCmpLe, FixnumCmpGt, FixnumCmpGe,
    FloatCmpEq, FloatCmpLt,
    GenericEq, GenericEqual, TypeCheck, InstanceOf,
    // Cat 4 — memory / object access (effectful unless proven immutable)
    Load, Car, Cdr, VecRef, SymbolValue,
    Store, SetCar, SetCdr, VecSet, SetSymbolValue, WriteBarrier,
    Alloc, AllocCons,
    // Cat 5a — non-terminator control: calls & guards
    Call, Guard,
    // Cat 5b — terminators
    Jump, Brif, BrTable, Return, TailCall, Throw, NlxTransfer, Trap,
}

impl Opcode {
    /// Whether this opcode ends a block (carries `BlockCall` successors).
    pub fn is_terminator(self) -> bool {
        use Opcode::*;
        matches!(self, Jump | Brif | BrTable | Return | TailCall | Throw | NlxTransfer | Trap)
    }
}

/// Per-instruction flags (spec §4.3.2.4). Pure by default (all-false).
#[derive(Copy, Clone, Default, Eq, PartialEq, Debug)]
pub struct InstFlags {
    /// Observable side effect: program order fixed relative to other effects.
    pub effectful: bool,
    /// Deoptimising guard (carries a `FrameState`).
    pub guard: bool,
    /// A GC safepoint is attached.
    pub safepoint: bool,
    /// Ends its block; has `targets`.
    pub terminator: bool,
    /// May transfer control / clobber caller-saved registers.
    pub call: bool,
    /// Operands may be reordered (for GVN).
    pub commutative: bool,
}

/// A control-flow edge target: a successor block plus the arguments bound to
/// that block's parameters on this edge.
#[derive(Clone, Debug)]
pub struct BlockCall {
    pub block: Block,
    pub args: Vec<Value>,
}

/// Opcode-specific immediate payload (spec §4.3.2.7).
#[derive(Clone, Debug)]
pub enum AuxData {
    None,
    FixnumImm(i64),
    FloatImm(f32),
    CharImm(char),
    SymbolRef(u32),
    HeapLiteral(bliss_rt::value::BlissVal),
    FieldOffset(u32),
    CallTarget(u32),
    TypeTag(IRType),
    ClassRef(u32),
}

/// One instruction. Terminators carry `targets`; guards/deoptimising insts carry
/// `frame_state`.
#[derive(Clone, Debug)]
pub struct InstData {
    pub opcode: Opcode,
    pub args: Vec<Value>,
    pub results: Vec<Value>,
    pub aux: AuxData,
    pub flags: InstFlags,
    /// Successor edges — non-empty only for terminators.
    pub targets: Vec<BlockCall>,
    /// Present on guards / deoptimising insts (spec §4.10 R4.59).
    pub frame_state: Option<FrameStateId>,
    /// Source position id (spec §4.3.7); interned in `Function::source_positions`.
    pub source_pos: u32,
}

/// How a value is defined (spec §4.3.2.3).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ValueDef {
    /// The `num`-th result of an instruction.
    Result { inst: Inst, num: u16 },
    /// The `num`-th parameter of a block.
    Param { block: Block, num: u16 },
}

#[derive(Clone, Debug)]
pub struct ValueData {
    pub def: ValueDef,
    pub ty: IRType,
    pub repr: ValueRepresentation,
}

/// A basic block: typed parameters (SSA φ) + an ordered instruction list ending
/// in exactly one terminator (maintained by the builder).
#[derive(Clone, Debug, Default)]
pub struct BlockData {
    pub params: Vec<Value>,
    /// Instruction order; the last entry is the terminator once the block is
    /// finished.
    pub insts: Vec<Inst>,
}

// ── Function ────────────────────────────────────────────────────────

/// A block-based SSA function (spec §4.3.2.1).
#[derive(Clone, Debug)]
pub struct Function {
    blocks: Vec<BlockData>,
    insts: Vec<InstData>,
    values: Vec<ValueData>,
    /// Block layout order; `block_order[0]` is the entry block.
    block_order: Vec<Block>,
    entry: Block,
    /// Frame-state table for deoptimising instructions (spec §4.10 D4.15).
    pub frame_states: crate::t2::frame_state::FrameStateTable,
    /// Interned source positions (spec §4.3.7); id 0 is "unknown".
    pub source_positions: Vec<SourcePosition>,
    name: String,
}

#[derive(Copy, Clone, Default, Debug)]
pub struct SourcePosition {
    pub file_id: u16,
    pub line: u32,
    pub column: u16,
    pub macro_depth: u8,
    pub inlined_at: Option<u32>,
}

impl Function {
    /// Create a function with a single empty entry block (no parameters yet;
    /// P1 adds the function parameters as entry-block parameters).
    pub fn new(name: impl Into<String>) -> Function {
        let mut f = Function {
            blocks: Vec::new(),
            insts: Vec::new(),
            values: Vec::new(),
            block_order: Vec::new(),
            entry: Block(0),
            frame_states: crate::t2::frame_state::FrameStateTable::default(),
            source_positions: vec![SourcePosition::default()],
            name: name.into(),
        };
        let entry = f.make_block();
        f.entry = entry;
        f
    }

    pub fn name(&self) -> &str { &self.name }
    pub fn entry(&self) -> Block { self.entry }
    pub fn block_order(&self) -> &[Block] { &self.block_order }

    // ── Arena accessors ──
    pub fn block(&self, b: Block) -> &BlockData { &self.blocks[b.index()] }
    pub fn block_mut(&mut self, b: Block) -> &mut BlockData { &mut self.blocks[b.index()] }
    pub fn inst(&self, i: Inst) -> &InstData { &self.insts[i.index()] }
    pub fn inst_mut(&mut self, i: Inst) -> &mut InstData { &mut self.insts[i.index()] }
    pub fn value(&self, v: Value) -> &ValueData { &self.values[v.index()] }
    pub fn num_blocks(&self) -> usize { self.blocks.len() }
    pub fn num_insts(&self) -> usize { self.insts.len() }
    pub fn num_values(&self) -> usize { self.values.len() }

    /// Whether a handle is in range for this function's arenas. Verification
    /// (P2) and any pass handling possibly-malformed IR should gate on these
    /// before indexing, since the CFG traversals assume in-range successors.
    pub fn is_valid_block(&self, b: Block) -> bool { b.index() < self.blocks.len() }
    pub fn is_valid_inst(&self, i: Inst) -> bool { i.index() < self.insts.len() }
    pub fn is_valid_value(&self, v: Value) -> bool { v.index() < self.values.len() }

    // ── Inferred-fact channel (spec §4.5 R4.31, §4.10) ──
    // The sanctioned way for an analysis (type inference, unboxing) to write a
    // refined type / chosen representation back onto a value, instead of a
    // side table. Type refinement is monotone (narrowing): the tag bits are met
    // with the current bits, and an inferred range/class is adopted.

    /// Narrow value `v`'s type with `ty` (met bits; adopt inferred range/class).
    pub fn refine_type(&mut self, v: Value, ty: IRType) {
        let slot = &mut self.values[v.index()];
        slot.ty.bits = slot.ty.bits.meet(ty.bits);
        if ty.range.is_some() {
            slot.ty.range = ty.range;
        }
        if ty.class_id.is_some() {
            slot.ty.class_id = ty.class_id;
        }
    }

    /// Set value `v`'s machine representation (unboxing decision).
    pub fn set_repr(&mut self, v: Value, repr: ValueRepresentation) {
        self.values[v.index()].repr = repr;
    }

    // ── Construction primitives (used by P1 builder & the passes) ──

    /// Allocate a fresh empty block and append it to the layout.
    pub fn make_block(&mut self) -> Block {
        let b = Block(self.blocks.len() as u32);
        self.blocks.push(BlockData::default());
        self.block_order.push(b);
        b
    }

    /// Add a typed parameter to `block`, returning its `Value`.
    pub fn add_block_param(&mut self, block: Block, ty: IRType, repr: ValueRepresentation) -> Value {
        let num = self.blocks[block.index()].params.len() as u16;
        let v = Value(self.values.len() as u32);
        self.values.push(ValueData { def: ValueDef::Param { block, num }, ty, repr });
        self.blocks[block.index()].params.push(v);
        v
    }

    /// Append a (non-terminator) instruction to `block`, returning it plus its
    /// result values (as declared in `result_tys`).
    pub fn push_inst(
        &mut self,
        block: Block,
        mut data: InstData,
        result_tys: &[(IRType, ValueRepresentation)],
    ) -> (Inst, Vec<Value>) {
        debug_assert!(!data.opcode.is_terminator(), "use set_terminator for terminators");
        let inst = Inst(self.insts.len() as u32);
        let mut results = Vec::with_capacity(result_tys.len());
        for (num, &(ty, repr)) in result_tys.iter().enumerate() {
            let v = Value(self.values.len() as u32);
            self.values.push(ValueData { def: ValueDef::Result { inst, num: num as u16 }, ty, repr });
            results.push(v);
        }
        data.results = results.clone();
        self.insts.push(data);
        self.blocks[block.index()].insts.push(inst);
        (inst, results)
    }

    /// Finish `block` with a terminator instruction (spec §4.3: exactly one,
    /// last). Panics if the block already has a terminator.
    pub fn set_terminator(&mut self, block: Block, mut data: InstData) -> Inst {
        debug_assert!(data.opcode.is_terminator(), "set_terminator needs a terminator opcode");
        debug_assert!(!self.block_has_terminator(block), "block already terminated");
        data.flags.terminator = true;
        let inst = Inst(self.insts.len() as u32);
        self.insts.push(data);
        self.blocks[block.index()].insts.push(inst);
        inst
    }

    pub fn block_has_terminator(&self, block: Block) -> bool {
        self.blocks[block.index()]
            .insts
            .last()
            .is_some_and(|&i| self.insts[i.index()].opcode.is_terminator())
    }

    /// The terminator of a finished block, if present.
    pub fn terminator(&self, block: Block) -> Option<Inst> {
        self.blocks[block.index()]
            .insts
            .last()
            .copied()
            .filter(|&i| self.insts[i.index()].opcode.is_terminator())
    }

    /// Successor blocks of `block` (derived from its terminator's `targets`).
    pub fn succs(&self, block: Block) -> Vec<Block> {
        match self.terminator(block) {
            Some(t) => self.insts[t.index()].targets.iter().map(|c| c.block).collect(),
            None => Vec::new(),
        }
    }

    /// Predecessor blocks of `block` (computed by scanning all terminators).
    pub fn preds(&self, block: Block) -> Vec<Block> {
        let mut preds = Vec::new();
        for &b in &self.block_order {
            if self.succs(b).contains(&block) {
                preds.push(b);
            }
        }
        preds
    }

    /// Reverse postorder over the CFG from the entry block.
    pub fn reverse_postorder(&self) -> Vec<Block> {
        let mut visited = vec![false; self.blocks.len()];
        let mut post = Vec::new();
        // Iterative DFS producing postorder.
        let mut stack: Vec<(Block, usize)> = vec![(self.entry, 0)];
        visited[self.entry.index()] = true;
        while let Some((b, idx)) = stack.pop() {
            let succs = self.succs(b);
            if idx < succs.len() {
                stack.push((b, idx + 1));
                let s = succs[idx];
                // Skip an out-of-range successor rather than panicking: malformed
                // IR (a dangling block handle) is a verifier finding (P2 V10),
                // not a reason to abort traversal.
                if s.index() < visited.len() && !visited[s.index()] {
                    visited[s.index()] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
            }
        }
        post.reverse();
        post
    }

    /// Compute the dominator tree (spec §4.3.4; Cooper–Harvey–Kennedy).
    pub fn dominators(&self) -> DominatorTree {
        DominatorTree::compute(self)
    }
}

// ── Dominator tree ──────────────────────────────────────────────────

/// Immediate-dominator tree over the CFG (spec §4.3.4). `idom[entry] = entry`.
#[derive(Clone, Debug)]
pub struct DominatorTree {
    /// Immediate dominator per block index; the entry dominates itself.
    idom: Vec<Option<Block>>,
    /// Reverse-postorder number per block index (for the fixpoint).
    rpo_num: Vec<usize>,
}

impl DominatorTree {
    fn compute(f: &Function) -> DominatorTree {
        let n = f.num_blocks();
        let rpo = f.reverse_postorder();
        let mut rpo_num = vec![usize::MAX; n];
        for (i, &b) in rpo.iter().enumerate() {
            rpo_num[b.index()] = i;
        }
        let mut idom: Vec<Option<Block>> = vec![None; n];
        idom[f.entry().index()] = Some(f.entry());

        let intersect = |mut a: Block, mut b: Block, idom: &[Option<Block>], rpo_num: &[usize]| {
            while a != b {
                while rpo_num[a.index()] > rpo_num[b.index()] {
                    a = idom[a.index()].expect("processed pred has an idom");
                }
                while rpo_num[b.index()] > rpo_num[a.index()] {
                    b = idom[b.index()].expect("processed pred has an idom");
                }
            }
            a
        };

        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                if b == f.entry() {
                    continue;
                }
                let mut new_idom: Option<Block> = None;
                for p in f.preds(b) {
                    if idom[p.index()].is_none() {
                        continue; // not yet processed
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(cur, p, &idom, &rpo_num),
                    });
                }
                if new_idom != idom[b.index()] {
                    idom[b.index()] = new_idom;
                    changed = true;
                }
            }
        }
        DominatorTree { idom, rpo_num }
    }

    /// Immediate dominator of `b` (the entry's is itself).
    pub fn idom(&self, b: Block) -> Option<Block> {
        self.idom[b.index()]
    }

    /// Does `a` dominate `b`? (Reflexive: a block dominates itself.)
    pub fn dominates(&self, a: Block, b: Block) -> bool {
        let mut cur = b;
        loop {
            if cur == a {
                return true;
            }
            match self.idom[cur.index()] {
                Some(next) if next != cur => cur = next,
                _ => return false, // reached entry (idom==self) or unreachable
            }
        }
    }

    pub fn rpo_num(&self, b: Block) -> usize {
        self.rpo_num[b.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Diamond CFG: entry -> {b1, b2} -> merge. Exercises make_block, terminators,
    // succ/pred derivation, RPO, and dominance — the fixture shape parcels use.
    fn jump_to(b: Block) -> InstData {
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![BlockCall { block: b, args: vec![] }],
            frame_state: None,
            source_pos: 0,
        }
    }

    #[test]
    fn diamond_dominance() {
        let mut f = Function::new("diamond");
        let entry = f.entry();
        let b1 = f.make_block();
        let b2 = f.make_block();
        let merge = f.make_block();

        // entry: brif-ish → for the smoke test just branch to both via two targets.
        f.set_terminator(entry, InstData {
            opcode: Opcode::Brif,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![
                BlockCall { block: b1, args: vec![] },
                BlockCall { block: b2, args: vec![] },
            ],
            frame_state: None,
            source_pos: 0,
        });
        f.set_terminator(b1, jump_to(merge));
        f.set_terminator(b2, jump_to(merge));
        f.set_terminator(merge, InstData {
            opcode: Opcode::Return,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        });

        assert_eq!(f.succs(entry), vec![b1, b2]);
        assert_eq!(f.preds(merge), vec![b1, b2]);

        let dom = f.dominators();
        // entry dominates everything.
        for b in [entry, b1, b2, merge] {
            assert!(dom.dominates(entry, b), "entry must dominate {b:?}");
        }
        // The arms dominate only themselves; the merge is dominated by entry, not
        // by either arm.
        assert!(!dom.dominates(b1, merge));
        assert!(!dom.dominates(b2, merge));
        assert_eq!(dom.idom(merge), Some(entry));
        assert_eq!(dom.idom(b1), Some(entry));
    }

    #[test]
    fn ssa_value_defs_are_tracked() {
        let mut f = Function::new("vals");
        let entry = f.entry();
        let p = f.add_block_param(entry, IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged);
        // A constant instruction producing one fixnum result.
        let (inst, results) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(42),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::UnboxedFixnum)],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(f.value(p).def, ValueDef::Param { block: entry, num: 0 });
        assert_eq!(f.value(results[0]).def, ValueDef::Result { inst, num: 0 });
        assert_eq!(f.value(results[0]).repr, ValueRepresentation::UnboxedFixnum);
    }
}
