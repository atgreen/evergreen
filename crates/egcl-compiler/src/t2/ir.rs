// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 block-based SSA IR: the data structures every T2 stage operates on.
//!
//! # Purpose
//!
//! Defines [`Function`] and its arenas, the [`Opcode`] set, instruction and
//! value metadata, the type lattice, and the CFG queries passes share
//! (successors, predecessors, reverse postorder, dominators). Construction from
//! bytecode lives in build.rs and well-formedness checking in verify.rs; this
//! file holds only the representation and the structural editing primitives.
//!
//! # Representation
//!
//! * **Arenas and handles.** `Function` owns three arenas indexed by the `u32`
//!   newtypes [`Block`], [`Inst`], and [`Value`]. `block_order` is the layout;
//!   `block_order[0]` is the entry block.
//! * **Data flow.** An SSA value is either an instruction result
//!   (`ValueDef::Result`) or a typed block parameter (`ValueDef::Param`). Block
//!   parameters are the φ mechanism: there is no Phi instruction and no Region
//!   node. A value's [`ValueData`] carries an [`IRType`] and a
//!   [`ValueRepresentation`].
//! * **Control flow.** Every finished block ends in exactly one terminator
//!   (`Opcode::is_terminator`), whose `targets: Vec<BlockCall>` name each
//!   successor and the arguments bound to its parameters on that edge.
//!   `succs` and `preds` are derived from terminators on demand, never stored.
//! * **Effect ordering.** There is no memory-token chain. Instructions with
//!   `InstFlags::effectful` keep their program order within a block; pure
//!   instructions may be reordered, duplicated, or merged by passes.
//! * **Type vs. representation.** [`IRType`] is a [`TypeBits`] tag bitset
//!   (meet = intersection, join = union) plus optional integer [`Range`] and
//!   `class_id` refinements. [`ValueRepresentation`] is orthogonal:
//!   `Tagged`, `UnboxedFixnum`, `UnboxedF32`, `UnboxedF64`. The Box*/Unbox*
//!   opcodes change representation only. `Tagged` is the only representation
//!   an interpreter slot can hold and the only one that may be a GC pointer,
//!   which is what makes representation a deopt concern.
//! * **Instructions.** [`InstData`] holds the opcode, `args`, `results`, an
//!   [`AuxData`] immediate payload, [`InstFlags`] (`effectful`, `guard`,
//!   `safepoint`, `terminator`, `call`, `commutative`), the terminator
//!   `targets`, an optional `frame_state` for deoptimising instructions, and
//!   an interned `source_pos` (id 0 is unknown).
//!
//! # Opcode groups
//!
//! Constants; fixnum, float, and generic arithmetic with box/unbox
//! conversions; comparisons and type checks; memory and object access (loads,
//! stores, allocation, symbol value and function cells, multiple-value state,
//! and the cleanup/catch/handler landings used by native transfer);
//! non-terminator control (`Call`, `Guard`); and terminators (`Invoke`,
//! `Jump`, `Brif`, `BrTable`, `Return`, `TailCall`, `Throw`, `NlxTransfer`,
//! `Trap`). `Invoke` is a call with a normal edge (target 0) and an exceptional
//! edge (target 1); its results exist only on the normal edge and must reach
//! users as that successor's block parameters.
//!
//! # Function-level metadata
//!
//! * `frame_states` — the interned `FrameStateTable`; deoptimising
//!   instructions refer to entries by `FrameStateId`.
//! * `entry_frame_state` — the interpreter state at bcp 0 with an empty
//!   stack, recorded so speculation can place parameter pre-guards whose deopt
//!   re-runs the whole function in T0 (bliss-x5y.25).
//! * `osr_entries` — loop headers that may be entered from a running
//!   lower-tier activation, each with the header's `FrameState`.
//! * `checked_entry_params` — parameters whose declared type the runtime call
//!   boundary enforces. Deliberately separate from `ValueData::ty`: an
//!   inferred or speculated type is not permission to omit the runtime check.
//! * `variadic` — the entry parameters are pre-collected frame slots filled by
//!   the shared variadic binding path, not positional arguments, so a
//!   positional register self-call entry must be suppressed (bliss-32l).
//! * `handler_cases` — HANDLER-CASE clause metadata consumed by the opt-in
//!   native transfer edges; inlining must remap this before admitting inlined
//!   handler scopes.
//!
//! # Editing primitives
//!
//! `make_block`, `add_block_param`, `push_inst`, `set_terminator` and
//! `set_terminator_with_results`, `refine_type`, `set_repr`;
//! `make_call_exceptional` splits a mapped `Call` into an `Invoke` with a fresh
//! normal-continuation block, projecting the results into that block and into
//! downstream frame states; `inline_call` (crate-private) clones a callee body
//! at a call site, prefixes the suspended caller scopes to every cloned
//! `FrameState`, and joins all callee returns at one continuation block whose
//! parameter replaces the call result.
//!
//! # Analyses
//!
//! `reverse_postorder` is an iterative DFS that skips an out-of-range
//! successor handle instead of panicking, so malformed IR reaches the verifier
//! as a finding. `dominators` computes immediate dominators with the
//! Cooper–Harvey–Kennedy iterative fixpoint over RPO numbering;
//! [`DominatorTree::dominates`] is reflexive and walks the idom chain, with the
//! entry as its own idom.

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
///. Box/unbox instructions change representation, not type.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ValueRepresentation {
    /// A tagged `EgclVal` in a GPR; the only representation an interpreter slot
    /// can hold, and the only one that may be a GC pointer.
    Tagged,
    /// A raw untagged fixnum (`i64`) in a GPR.
    UnboxedFixnum,
    /// A raw `f32` in an XMM register (the immediate single-float, untagged).
    UnboxedF32,
    /// A raw `f64` in an XMM register (a boxed CL double reboxed on deopt).
    UnboxedF64,
}

/// Bitset of possible CL type tags. One bit per major type.
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

    #[inline]
    pub fn meet(self, o: TypeBits) -> TypeBits {
        TypeBits(self.0 & o.0)
    } // intersection
    #[inline]
    pub fn join(self, o: TypeBits) -> TypeBits {
        TypeBits(self.0 | o.0)
    } // union
    #[inline]
    pub fn is_bottom(self) -> bool {
        self.0 == 0
    }
    #[inline]
    pub fn contains(self, o: TypeBits) -> bool {
        self.0 & o.0 == o.0
    }
}

/// Inclusive integer sub-range refinement, load-bearing for overflow-guard
/// elimination.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Range {
    pub lo: i64,
    pub hi: i64,
}

/// The IR type lattice: a tag bitset plus optional refinements.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct IRType {
    pub bits: TypeBits,
    pub range: Option<Range>,
    pub class_id: Option<u32>,
}

impl IRType {
    pub const BOTTOM: IRType = IRType {
        bits: TypeBits::BOTTOM,
        range: None,
        class_id: None,
    };
    pub const TOP: IRType = IRType {
        bits: TypeBits::TOP,
        range: None,
        class_id: None,
    };
    pub fn of(bits: TypeBits) -> IRType {
        IRType {
            bits,
            range: None,
            class_id: None,
        }
    }
}

// ── Opcodes & instructions ──────────────────────────────────────────

/// Instruction opcodes. Representative-but-complete set; extended
/// as needed. Terminators are the last group and set `InstFlags.terminator`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Opcode {
    // Cat 1 — constants (payload in AuxData). Block parameters are the params.
    ConstFixnum,
    ConstFloat,
    ConstChar,
    ConstSymbol,
    ConstNil,
    ConstT,
    ConstHeapObj,
    // Cat 2 — arithmetic / logic
    FixnumAdd,
    FixnumSub,
    FixnumMul,
    FixnumDiv,
    FixnumRem,
    FixnumMod,
    FixnumNeg,
    FixnumShl,
    FixnumShr,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    GenericAdd,
    GenericSub,
    GenericMul,
    GenericDiv,
    LogAnd,
    LogOr,
    LogXor,
    LogNot,
    BoxFixnum,
    UnboxFixnum,
    BoxFloat,
    UnboxFloat,
    WidenI32,
    // Cat 3 — comparison / type checks
    FixnumCmpEq,
    FixnumCmpLt,
    FixnumCmpLe,
    FixnumCmpGt,
    FixnumCmpGe,
    FloatCmpEq,
    FloatCmpLt,
    GenericEq,
    GenericEqual,
    TypeCheck,
    InstanceOf,
    // Cat 4 — memory / object access (effectful unless proven immutable)
    Load,
    Car,
    Cdr,
    VecRef,
    SymbolValue,
    /// `#'f` for a global function name — a RUNTIME load of the symbol's
    /// function cell, not a constant: the function may be redefined between
    /// compile and call. Lowered like `SymbolValue`, through a c2i helper
    /// (bliss-m285).
    SymbolFunction,
    /// Direct UTF-8 byte-length load from a value refined by a preceding
    /// `Guard` carrying `AuxData::StringLayout`.
    StringByteLength,
    /// Simple-string byte access for an ASCII character.  Its string operand
    /// is refined by a preceding `StringLayout` guard; negative, out-of-bounds,
    /// and non-ASCII cases deopt to CL:CHAR.
    StringAsciiCharAt,
    Store,
    SetCar,
    SetCdr,
    VecSet,
    SetSymbolValue,
    WriteBarrier,
    MemoryFence,
    Alloc,
    AllocCons,
    /// Reset the thread's multiple-values state (interpreter `ClearMv`, emitted
    /// after SETQ/SETF and in statement position). No operands, no result;
    /// lowers to a `c2i_clear_mv` call (bliss-mzp).
    ClearMv,
    /// Consume a primary value and copy the current multiple-value tuple into
    /// lexical locals. Results correspond positionally to `slot_base..` and
    /// are NIL-padded by the runtime helper.
    TakeValuesToLocals,
    /// Save the protected primary and the complete runtime multiple-value state
    /// in an execution-owned rooted cleanup continuation. No normal result.
    CleanupSave,
    /// Pop the matching normal cleanup continuation, restore its multiple-value
    /// state and produce its primary. Exceptional entry uses the unwind cursor.
    CleanupRestore,
    /// Verified exceptional/normal cleanup entry with an entry FrameState.
    CleanupLanding,
    /// Consume the prepared catch payload and produce its primary value.
    /// Native-only, noncollecting helper; secondary values remain in runtime state.
    CatchLanding,
    /// Define the condition delivered to a selected HANDLER-CASE clause.
    HandlerLanding,
    // Cat 5a — non-terminator control: calls & guards
    Call,
    Guard,
    // Cat 5b — terminators
    /// A transfer-capable call. Target 0 is normal completion; target 1 is
    /// exceptional propagation. Result values may only be passed to target 0
    /// as block arguments, never used on the exceptional edge or directly in
    /// another instruction. Both edges participate in CFG/liveness analysis.
    /// The frame state describes the pre-call state, not a replay permission.
    Invoke,
    Jump,
    Brif,
    BrTable,
    Return,
    TailCall,
    Throw,
    NlxTransfer,
    Trap,
}

impl Opcode {
    /// Whether this opcode ends a block (carries `BlockCall` successors).
    pub fn is_terminator(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            Invoke | Jump | Brif | BrTable | Return | TailCall | Throw | NlxTransfer | Trap
        )
    }
}

/// Per-instruction flags. Pure by default (all-false).
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

/// Opcode-specific immediate payload.
#[derive(Clone, Debug)]
pub enum AuxData {
    None,
    FixnumImm(i64),
    FloatImm(f32),
    CharImm(char),
    SymbolRef(u32),
    /// Address of the moving-GC value's owning constant-pool slot. Generated
    /// code loads through this slot; the compiler client must keep it rooted
    /// and alive with the native code.
    HeapLiteral {
        slot: usize,
    },
    FieldOffset(u32),
    CallTarget(u32),
    /// Evaluate an owned constant form through the native-transfer bridge.
    HostEval(u16),
    /// Read a function designator through the native-transfer bridge.
    FunctionLookup(u32),
    /// Invoke a THROW with rooted tag and primary arguments; no normal result.
    TransferThrow,
    /// Establish/retire a dynamic CATCH binding in the owning activation.
    /// The helper-v2 request preserves the existing multiple-value state.
    CatchScope {
        push_bcp: u32,
        enter: bool,
    },
    HandlerScope {
        push_bcp: u32,
        enter: bool,
    },
    /// Establish/retire a dynamic HANDLER-BIND cluster.
    HandlerBindScope {
        push_bcp: u32,
        enter: bool,
    },
    /// Establish/retire a dynamic RESTART-CASE cluster.
    RestartCaseScope {
        push_bcp: u32,
        enter: bool,
    },
    HandlerDestination {
        push_bcp: u32,
        table_index: u32,
        clause_index: u32,
    },
    CatchDestination {
        push_bcp: u32,
        resume_bcp: u32,
    },
    /// Save/restore or landing identity. On Invoke this selects cleanup
    /// completion: normal edge pops/restores the saved value, exceptional edge
    /// resumes its pending transfer with the continuation still described by
    /// the cold site's pre-operation scope map. `u32::MAX` means no normal resume.
    CleanupContinuation {
        cleanup_bcp: u32,
        resume_bcp: u32,
    },
    /// Cold propagation after a transfer-capable operation. The attached
    /// FrameState captures the pre-operation values, but propagation must begin
    /// unwinding rather than executing that bytecode operation again. These
    /// static scopes describe required runtime state, not permission to elide it.
    /// NlxTransfer may name verified cleanup and catch landing successors;
    /// no successor means fallback/propagation outside this native CFG. A native target
    /// remains conditional on runtime destination availability and signaling.
    TransferSite {
        origin_bcp: u32,
        scopes: Vec<crate::control_scope::ControlScope>,
    },
    TypeTag(IRType),
    /// Exact class code for the pure bytecode `TypeP` predicate. Unlike a
    /// `TypeTag`, classes such as BOOLEAN and LIST are not equivalent to their
    /// broad type-lattice union.
    TypepClass(u16),
    MemoryFence(egcl_rt::bytecode::MemoryFenceKind),
    ValuesLocals {
        nvars: u16,
        slot_base: u16,
    },
    /// Refines a tagged value to the simple string layouts supported by direct
    /// T2 loads.  Kept distinct from `TypeTag(STRING)`: the latter includes
    /// string representations whose layout still requires generic dispatch.
    StringLayout,
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
    /// Present on guards / deoptimising insts.
    pub frame_state: Option<FrameStateId>,
    /// Source position id; interned in `Function::source_positions`.
    pub source_pos: u32,
}

/// How a value is defined.
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

/// A bytecode loop header that may be entered from an already-running lower
/// tier activation.  The frame state names the live locals at the header after
/// all SSA rewrites, so the emitter can reconstruct its register state from the
/// shared EgclStack frame.
#[derive(Clone, Debug)]
pub struct OsrEntry {
    pub bcp: u32,
    pub block: Block,
    pub frame_state: crate::t2::frame_state::FrameStateId,
    /// Type facts the entry must establish on the values it imports before
    /// jumping into the loop: a proof the optimiser relies on for a header
    /// parameter (a loop-entry guard split out of the body, bliss-5yz5h) is
    /// only true of values that arrived through the IR's own edges, so an
    /// interpreter value entering here is tested and, on failure, deoptimised
    /// through `frame_state`. A backend that cannot emit the test must decline
    /// the entry.
    pub checks: Vec<(Value, IRType)>,
}

// ── Function ────────────────────────────────────────────────────────

/// A block-based SSA function.
#[derive(Clone, Debug)]
pub struct Function {
    blocks: Vec<BlockData>,
    insts: Vec<InstData>,
    values: Vec<ValueData>,
    /// Block layout order; `block_order[0]` is the entry block.
    block_order: Vec<Block>,
    entry: Block,
    /// Frame-state table for deoptimising instructions.
    pub frame_states: crate::t2::frame_state::FrameStateTable,
    /// Interned source positions; id 0 is "unknown".
    pub source_positions: Vec<SourcePosition>,
    /// Root-function loop headers eligible for on-stack replacement.
    pub osr_entries: Vec<OsrEntry>,
    /// Root-definition clause metadata used by opt-in native transfer edges.
    /// Inlining must remap this domain before admitting inlined handler scopes.
    pub handler_cases: Vec<egcl_rt::bytecode::HandlerCaseInfo>,
    /// The interpreter state at function ENTRY (bcp 0, empty stack), recorded
    /// by the builder so speculation can place parameter pre-guards whose
    /// deopt harmlessly re-runs the whole function in T0 (bliss-x5y.25).
    pub entry_frame_state: Option<crate::t2::frame_state::FrameStateId>,
    /// Entry parameters whose source declaration is enforced by the runtime
    /// call boundary. This is deliberately separate from `ValueData::ty`: an
    /// inferred/speculative type is not permission to omit its runtime guard.
    checked_entry_params: Vec<Value>,
    /// True if the source function has a variadic lambda list. Its entry-block
    /// params are pre-collected frame slots (&rest list, &optional/&key values)
    /// filled by the shared bind_variadic path, NOT positional call arguments —
    /// so a positional register self-call entry must be suppressed (bliss-32l).
    variadic: bool,
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
    /// the builder adds the function parameters as entry-block parameters).
    pub fn new(name: impl Into<String>) -> Function {
        let mut f = Function {
            blocks: Vec::new(),
            insts: Vec::new(),
            values: Vec::new(),
            block_order: Vec::new(),
            entry: Block(0),
            frame_states: crate::t2::frame_state::FrameStateTable::default(),
            source_positions: vec![SourcePosition::default()],
            osr_entries: Vec::new(),
            handler_cases: Vec::new(),
            entry_frame_state: None,
            checked_entry_params: Vec::new(),
            variadic: false,
            name: name.into(),
        };
        let entry = f.make_block();
        f.entry = entry;
        f
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn entry(&self) -> Block {
        self.entry
    }
    /// Whether the source function has a variadic lambda list (bliss-32l).
    pub fn is_variadic(&self) -> bool {
        self.variadic
    }
    pub fn set_variadic(&mut self, v: bool) {
        self.variadic = v;
    }
    pub fn block_order(&self) -> &[Block] {
        &self.block_order
    }

    // ── Arena accessors ──
    pub fn block(&self, b: Block) -> &BlockData {
        &self.blocks[b.index()]
    }
    pub fn block_mut(&mut self, b: Block) -> &mut BlockData {
        &mut self.blocks[b.index()]
    }
    pub fn inst(&self, i: Inst) -> &InstData {
        &self.insts[i.index()]
    }
    pub fn inst_mut(&mut self, i: Inst) -> &mut InstData {
        &mut self.insts[i.index()]
    }
    pub fn value(&self, v: Value) -> &ValueData {
        &self.values[v.index()]
    }
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }
    pub fn num_insts(&self) -> usize {
        self.insts.len()
    }
    pub fn num_values(&self) -> usize {
        self.values.len()
    }

    pub fn mark_entry_param_checked(&mut self, value: Value) {
        if !self.checked_entry_params.contains(&value) {
            self.checked_entry_params.push(value);
        }
    }

    pub fn is_entry_param_checked(&self, value: Value) -> bool {
        self.checked_entry_params.contains(&value)
    }

    /// Whether a handle is in range for this function's arenas. Verification
    /// (verify.rs) and any pass handling possibly-malformed IR should gate on these
    /// before indexing, since the CFG traversals assume in-range successors.
    pub fn is_valid_block(&self, b: Block) -> bool {
        b.index() < self.blocks.len()
    }
    pub fn is_valid_inst(&self, i: Inst) -> bool {
        i.index() < self.insts.len()
    }
    pub fn is_valid_value(&self, v: Value) -> bool {
        v.index() < self.values.len()
    }

    // ── Inferred-fact channel ──
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

    // ── Construction primitives (used by the builder and the passes) ──

    /// Allocate a fresh empty block and append it to the layout.
    pub fn make_block(&mut self) -> Block {
        let b = Block(self.blocks.len() as u32);
        self.blocks.push(BlockData::default());
        self.block_order.push(b);
        b
    }

    /// Add a typed parameter to `block`, returning its `Value`.
    pub fn add_block_param(
        &mut self,
        block: Block,
        ty: IRType,
        repr: ValueRepresentation,
    ) -> Value {
        let num = self.blocks[block.index()].params.len() as u16;
        let v = Value(self.values.len() as u32);
        self.values.push(ValueData {
            def: ValueDef::Param { block, num },
            ty,
            repr,
        });
        self.blocks[block.index()].params.push(v);
        v
    }

    /// Append a (non-terminator) instruction to `block`, returning it plus its
    /// result values (as declared in `result_tys`).
    pub fn push_inst(
        &mut self,
        block: Block,
        data: InstData,
        result_tys: &[(IRType, ValueRepresentation)],
    ) -> (Inst, Vec<Value>) {
        debug_assert!(
            !data.opcode.is_terminator(),
            "use set_terminator for terminators"
        );
        self.append_inst(block, data, result_tys)
    }

    fn append_inst(
        &mut self,
        block: Block,
        mut data: InstData,
        result_tys: &[(IRType, ValueRepresentation)],
    ) -> (Inst, Vec<Value>) {
        let inst = Inst(self.insts.len() as u32);
        let mut results = Vec::with_capacity(result_tys.len());
        for (num, &(ty, repr)) in result_tys.iter().enumerate() {
            let v = Value(self.values.len() as u32);
            self.values.push(ValueData {
                def: ValueDef::Result {
                    inst,
                    num: num as u16,
                },
                ty,
                repr,
            });
            results.push(v);
        }
        data.results = results.clone();
        self.insts.push(data);
        self.blocks[block.index()].insts.push(inst);
        (inst, results)
    }

    /// Finish `block` with a terminator instruction (exactly one,
    /// last). Panics if the block already has a terminator.
    pub fn set_terminator(&mut self, block: Block, mut data: InstData) -> Inst {
        debug_assert!(
            data.opcode.is_terminator(),
            "set_terminator needs a terminator opcode"
        );
        debug_assert!(
            !self.block_has_terminator(block),
            "block already terminated"
        );
        data.flags.terminator = true;
        let inst = Inst(self.insts.len() as u32);
        self.insts.push(data);
        self.blocks[block.index()].insts.push(inst);
        inst
    }

    /// Finish a block with a result-producing terminator. Invoke results
    /// are edge-local definitions; project them through normal block parameters.
    pub fn set_terminator_with_results(
        &mut self,
        block: Block,
        mut data: InstData,
        result_tys: &[(IRType, ValueRepresentation)],
    ) -> (Inst, Vec<Value>) {
        assert_eq!(
            data.opcode,
            Opcode::Invoke,
            "only Invoke defines edge results"
        );
        assert!(
            !self.block_has_terminator(block),
            "block already terminated"
        );
        data.flags.terminator = true;
        self.append_inst(block, data, result_tys)
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
            Some(t) => self.insts[t.index()]
                .targets
                .iter()
                .map(|c| c.block)
                .collect(),
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
                // IR (a dangling block handle) is a verifier finding (V10),
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

    /// Compute the dominator tree (Cooper–Harvey–Kennedy).
    pub fn dominators(&self) -> DominatorTree {
        DominatorTree::compute(self)
    }

    /// Split an ordinary call into normal and exceptional continuations.
    /// The exceptional arguments name values live before the call. Results are
    /// projected into the new normal block, including in downstream frame states.
    /// The caller supplies the selected cleanup/propagation block; this does not
    /// infer Lisp handler ownership or permit native emission without unwind maps.
    pub fn make_call_exceptional(
        &mut self,
        call: Inst,
        exceptional: BlockCall,
    ) -> Result<Block, &'static str> {
        if !self.is_valid_inst(call) || !self.is_valid_block(exceptional.block) {
            return Err("exceptional call or target is outside the function");
        }
        let (block, position) = self
            .block_order
            .iter()
            .find_map(|&block| {
                self.block(block)
                    .insts
                    .iter()
                    .position(|&i| i == call)
                    .map(|position| (block, position))
            })
            .ok_or("exceptional call is not in block layout")?;
        let data = self.inst(call).clone();
        if data.opcode != Opcode::Call
            || data.frame_state.is_none()
            || !matches!(data.aux, AuxData::CallTarget(_))
            || !self.block_has_terminator(block)
        {
            return Err("exceptional conversion requires a mapped call in a finished block");
        }
        if exceptional.args.iter().any(|v| data.results.contains(v)) {
            return Err("exceptional edge cannot use the call result");
        }
        let normal = self.make_block();
        let suffix = self.block_mut(block).insts.split_off(position + 1);
        self.block_mut(normal).insts = suffix;
        for &result in &data.results {
            let value = self.value(result).clone();
            let parameter = self.add_block_param(normal, value.ty, value.repr);
            self.replace_value_everywhere(result, parameter);
        }
        let invoke = self.inst_mut(call);
        invoke.opcode = Opcode::Invoke;
        invoke.flags.terminator = true;
        invoke.flags.effectful = true;
        invoke.flags.call = true;
        invoke.flags.safepoint = true;
        invoke.targets = vec![
            BlockCall {
                block: normal,
                args: data.results,
            },
            exceptional,
        ];
        Ok(normal)
    }

    /// Replace one ordinary call with a cloned callee CFG. The caller block is
    /// split at `call`; callee entry parameters bind directly to the call
    /// arguments, and every callee return jumps to a single continuation block
    /// whose parameter replaces the call result.
    ///
    /// `caller_scopes` describes the suspended logical callers (outermost
    /// first). It is prefixed to every cloned callee FrameState, and cloned
    /// source positions have their inline chain terminated at the call site.
    pub(crate) fn inline_call(
        &mut self,
        call: Inst,
        callee: &Function,
        caller_scopes: &[crate::t2::frame_state::FrameScope],
    ) -> Result<(), &'static str> {
        use crate::t2::frame_state::{FrameState, RematRecipe, ValueSource};
        use std::collections::HashMap;

        let (caller_block, call_pos) = self
            .block_order
            .iter()
            .find_map(|&b| {
                self.blocks[b.index()]
                    .insts
                    .iter()
                    .position(|&i| i == call)
                    .map(|p| (b, p))
            })
            .ok_or("inline call is not in a block")?;
        let call_data = self.inst(call).clone();
        if call_data.opcode != Opcode::Call || call_data.results.len() != 1 {
            return Err("inline target is not a single-result call");
        }
        let callee_entry_params = callee.block(callee.entry()).params.clone();
        if callee_entry_params.len() != call_data.args.len() {
            return Err("callee entry arity does not match call");
        }

        // Split after the call. The old instruction remains in the arena but is
        // no longer in block layout, exactly like other dead IR instructions.
        let continuation = self.make_block();
        let suffix = self.blocks[caller_block.index()]
            .insts
            .split_off(call_pos + 1);
        self.blocks[continuation.index()].insts = suffix;
        self.blocks[caller_block.index()].insts.pop();

        let old_result = call_data.results[0];
        let old_result_data = self.value(old_result).clone();
        let continuation_value =
            self.add_block_param(continuation, old_result_data.ty, old_result_data.repr);
        self.replace_value_everywhere(old_result, continuation_value);

        // Allocate the cloned CFG and bind the callee entry parameters to the
        // caller's SSA arguments. Other block parameters are cloned normally.
        let mut block_map: HashMap<Block, Block> = HashMap::new();
        for &old in callee.block_order() {
            block_map.insert(old, self.make_block());
        }
        let mut value_map: HashMap<Value, Value> = HashMap::new();
        for (&param, &arg) in callee_entry_params.iter().zip(&call_data.args) {
            value_map.insert(param, arg);
        }
        for &old_block in callee.block_order() {
            if old_block == callee.entry() {
                continue;
            }
            let new_block = block_map[&old_block];
            for &old_param in &callee.block(old_block).params {
                let vd = callee.value(old_param);
                let new_param = self.add_block_param(new_block, vd.ty, vd.repr);
                value_map.insert(old_param, new_param);
            }
        }

        // Copy and chain the source-position table before cloning instructions.
        let mut source_map = vec![None; callee.source_positions.len()];
        fn copy_source(
            dst: &mut Vec<SourcePosition>,
            src: &[SourcePosition],
            map: &mut [Option<u32>],
            id: u32,
            call_source: u32,
        ) -> u32 {
            if let Some(mapped) = map[id as usize] {
                return mapped;
            }
            let mut pos = src[id as usize];
            pos.inlined_at = match pos.inlined_at {
                Some(parent) => Some(copy_source(dst, src, map, parent, call_source)),
                None => Some(call_source),
            };
            let mapped = dst.len() as u32;
            dst.push(pos);
            map[id as usize] = Some(mapped);
            mapped
        }
        for id in 0..callee.source_positions.len() as u32 {
            copy_source(
                &mut self.source_positions,
                &callee.source_positions,
                &mut source_map,
                id,
                call_data.source_pos,
            );
        }

        fn map_source(source: &ValueSource, values: &HashMap<Value, Value>) -> ValueSource {
            match source {
                ValueSource::Value { value, repr } => ValueSource::Value {
                    value: values[value],
                    repr: *repr,
                },
                ValueSource::Const(v) => ValueSource::Const(*v),
                ValueSource::Unbound => ValueSource::Unbound,
                ValueSource::Remat(id) => ValueSource::Remat(*id),
            }
        }

        // Clone in block order. SSA definitions precede their uses, so the
        // value map is complete whenever an operand is encountered.
        for &old_block in callee.block_order() {
            let new_block = block_map[&old_block];
            for &old_inst in &callee.block(old_block).insts {
                let old = callee.inst(old_inst);
                if old.opcode == Opcode::Return {
                    let returned = value_map[&old.args[0]];
                    self.set_terminator(
                        new_block,
                        InstData {
                            opcode: Opcode::Jump,
                            args: vec![],
                            results: vec![],
                            aux: AuxData::None,
                            flags: InstFlags {
                                terminator: true,
                                ..InstFlags::default()
                            },
                            targets: vec![BlockCall {
                                block: continuation,
                                args: vec![returned],
                            }],
                            frame_state: None,
                            source_pos: source_map[old.source_pos as usize].unwrap(),
                        },
                    );
                    continue;
                }

                let frame_state = old.frame_state.map(|id| {
                    let source = callee.frame_states.get(id);
                    let mut scopes = caller_scopes.to_vec();
                    scopes.extend(source.scopes.iter().map(|scope| {
                        crate::t2::frame_state::FrameScope {
                            function: scope.function,
                            bcp: scope.bcp,
                            locals: scope
                                .locals
                                .iter()
                                .map(|s| map_source(s, &value_map))
                                .collect(),
                            stack: scope
                                .stack
                                .iter()
                                .map(|s| map_source(s, &value_map))
                                .collect(),
                        }
                    }));
                    let remat = source
                        .remat
                        .iter()
                        .map(|r| RematRecipe {
                            op: r.op,
                            inputs: r.inputs.iter().map(|s| map_source(s, &value_map)).collect(),
                            result_repr: r.result_repr,
                        })
                        .collect();
                    self.frame_states.add(FrameState { scopes, remat })
                });
                let args = old.args.iter().map(|v| value_map[v]).collect();
                let data = InstData {
                    opcode: old.opcode,
                    args,
                    results: vec![],
                    aux: old.aux.clone(),
                    flags: old.flags,
                    targets: vec![],
                    frame_state,
                    source_pos: source_map[old.source_pos as usize].unwrap(),
                };
                let result_tys: Vec<_> = old
                    .results
                    .iter()
                    .map(|&v| {
                        let vd = callee.value(v);
                        (vd.ty, vd.repr)
                    })
                    .collect();
                let (new_inst, results) = if old.opcode == Opcode::Invoke {
                    self.set_terminator_with_results(new_block, data, &result_tys)
                } else if old.opcode.is_terminator() {
                    (self.set_terminator(new_block, data), vec![])
                } else {
                    self.push_inst(new_block, data, &result_tys)
                };
                for (&old_value, &new_value) in old.results.iter().zip(&results) {
                    value_map.insert(old_value, new_value);
                }
                // Invoke's own results can occur on its normal edge. Allocate
                // and map them before translating successor arguments.
                self.inst_mut(new_inst).targets = old
                    .targets
                    .iter()
                    .map(|target| BlockCall {
                        block: block_map[&target.block],
                        args: target.args.iter().map(|v| value_map[v]).collect(),
                    })
                    .collect();
            }
        }

        self.set_terminator(
            caller_block,
            InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    terminator: true,
                    ..InstFlags::default()
                },
                targets: vec![BlockCall {
                    block: block_map[&callee.entry()],
                    args: vec![],
                }],
                frame_state: None,
                source_pos: call_data.source_pos,
            },
        );
        Ok(())
    }

    fn replace_value_everywhere(&mut self, from: Value, to: Value) {
        for data in &mut self.insts {
            for arg in &mut data.args {
                if *arg == from {
                    *arg = to;
                }
            }
            for target in &mut data.targets {
                for arg in &mut target.args {
                    if *arg == from {
                        *arg = to;
                    }
                }
            }
        }
        for state in self.frame_states.iter_mut() {
            for scope in &mut state.scopes {
                for source in scope.locals.iter_mut().chain(&mut scope.stack) {
                    if let crate::t2::frame_state::ValueSource::Value { value, .. } = source {
                        if *value == from {
                            *value = to;
                        }
                    }
                }
            }
            for recipe in &mut state.remat {
                for source in &mut recipe.inputs {
                    if let crate::t2::frame_state::ValueSource::Value { value, .. } = source {
                        if *value == from {
                            *value = to;
                        }
                    }
                }
            }
        }
    }
}

// ── Dominator tree ──────────────────────────────────────────────────

/// Immediate-dominator tree over the CFG. `idom[entry] = entry`.
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
    // succ/pred derivation, RPO, and dominance — the fixture shape tests use.
    fn jump_to(b: Block) -> InstData {
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![BlockCall {
                block: b,
                args: vec![],
            }],
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
        f.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Brif,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![
                    BlockCall {
                        block: b1,
                        args: vec![],
                    },
                    BlockCall {
                        block: b2,
                        args: vec![],
                    },
                ],
                frame_state: None,
                source_pos: 0,
            },
        );
        f.set_terminator(b1, jump_to(merge));
        f.set_terminator(b2, jump_to(merge));
        f.set_terminator(
            merge,
            InstData {
                opcode: Opcode::Return,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );

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
        let p = f.add_block_param(
            entry,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
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
            &[(
                IRType::of(TypeBits::FIXNUM),
                ValueRepresentation::UnboxedFixnum,
            )],
        );
        assert_eq!(results.len(), 1);
        assert_eq!(
            f.value(p).def,
            ValueDef::Param {
                block: entry,
                num: 0
            }
        );
        assert_eq!(f.value(results[0]).def, ValueDef::Result { inst, num: 0 });
        assert_eq!(f.value(results[0]).repr, ValueRepresentation::UnboxedFixnum);
    }
}
