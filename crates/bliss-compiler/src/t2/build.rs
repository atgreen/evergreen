//! P1 — Bytecode → block-based SSA builder (spec §4.3.5).
//!
//! **Parcel P1.** `build_from_bytecode` abstractly interprets a
//! `BytecodeFunction`, producing SSA directly in block-parameter form via the
//! Braun et al. algorithm ("Simple and Efficient Construction of SSA Form").
//!
//! Two kinds of interpreter state become SSA `Value`s:
//!
//! - **Operand-stack slots** — the stack machine's push/pop are modelled as a
//!   compile-time `Vec<Value>` per block. A block's live incoming stack entries
//!   become its leading parameters (materialised lazily via `read_var`).
//! - **Local slots** — `LoadLocal`/`StoreLocal` read/write the current SSA
//!   definition of a lexical slot through `read_var`/`write_var`.
//!
//! Basic-block leaders are bytecode index 0, every branch/`Go` target, and the
//! fall-through index after every conditional branch. Loops are built with an
//! **unsealed** header: `read_var` in an unsealed block creates an *incomplete
//! phi* (a header parameter); when the last predecessor (the back-edge) is
//! processed the header is **sealed**, filling each phi's operand on every
//! predecessor edge. `Call`s carry a `FrameState` snapshot of the abstract
//! interpreter state at their `bcp` (spec §4.10).
//!
//! Unmodelled opcodes (handlers, restarts, closures, multiple values, non-local
//! exits) return `Err(BuildError::Unsupported(..))`; the caller keeps such a
//! function at T1 (spec R4.28). Correctness over coverage.

use std::collections::{BTreeSet, HashMap};

use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::inlining::{InlineDecision, InlineOptions, IntrinsicId, decide, metadata_for_symbol};
use crate::t2::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use bliss_rt::bytecode::{BytecodeFunction, Instr};

/// Why the builder could not produce IR for a function (e.g. an opcode not yet
/// modelled). The caller keeps such a function at T1 (spec R4.28).
#[derive(Clone, Debug)]
pub enum BuildError {
    Unsupported(&'static str),
}

/// A Braun variable: either a lexical local slot or an operand-stack position.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
enum Var {
    Local(u16),
    Stack(u16),
}

/// Build a block-based SSA `Function` from `bf` (spec §4.3.5, R4.19).
pub fn build_from_bytecode(bf: &BytecodeFunction) -> Result<Function, BuildError> {
    build_from_bytecode_with_inline_options(bf, InlineOptions::default())
}

/// Build T2 IR with explicit per-call-site inlining policy. The ordinary
/// tiering path uses [`InlineOptions::default`]; this entry point is also the
/// seam where lexical INLINE/NOTINLINE declarations are supplied.
pub fn build_from_bytecode_with_inline_options(
    bf: &BytecodeFunction,
    inline_options: InlineOptions,
) -> Result<Function, BuildError> {
    Builder::new(bf, inline_options).run()
}

struct Builder<'a> {
    bf: &'a BytecodeFunction,
    f: Function,
    /// Sorted bytecode indices that begin a basic block.
    leaders: Vec<usize>,
    /// Leader bytecode index → its `Block`.
    block_of: HashMap<usize, Block>,
    /// Operand-stack depth on entry to each leader's block.
    entry_depth: HashMap<usize, usize>,
    /// Predecessor edges of a block: `(pred_block, target_index_in_pred_terminator)`.
    pred_edges: HashMap<Block, Vec<(Block, usize)>>,
    /// Total structural predecessor-edge count of each block (drives sealing).
    total_preds: Vec<usize>,
    /// Predecessor edges already materialised (their terminator is set).
    seen_preds: Vec<usize>,
    /// Whether a block has been interpreted.
    interpreted: Vec<bool>,
    /// The terminator instruction of each finished block.
    terminator_inst: HashMap<Block, Inst>,
    /// Braun `var_def`: current SSA definition of `(var, block)`.
    current_def: HashMap<(Var, Block), Value>,
    /// Whether all predecessors of a block are known (Braun sealing).
    sealed: Vec<bool>,
    /// Incomplete phis (header params) awaiting operand fill at seal time.
    incomplete_phis: HashMap<Block, Vec<(Var, Value)>>,
    inline_options: InlineOptions,
    remaining_inline_budget: u32,
    /// Body cloning will increment this while descending an inline call tree.
    /// Intrinsic expansions are leaves, so the first implementation stays at 0.
    inline_depth: u8,
}

impl<'a> Builder<'a> {
    fn new(bf: &'a BytecodeFunction, inline_options: InlineOptions) -> Builder<'a> {
        let remaining_inline_budget = inline_options.config.node_budget;
        Builder {
            bf,
            f: Function::new(bf.name.clone()),
            leaders: Vec::new(),
            block_of: HashMap::new(),
            entry_depth: HashMap::new(),
            pred_edges: HashMap::new(),
            total_preds: Vec::new(),
            seen_preds: Vec::new(),
            interpreted: Vec::new(),
            terminator_inst: HashMap::new(),
            current_def: HashMap::new(),
            sealed: Vec::new(),
            incomplete_phis: HashMap::new(),
            inline_options,
            remaining_inline_budget,
            inline_depth: 0,
        }
    }

    // ── Driver ──────────────────────────────────────────────────────

    fn run(mut self) -> Result<Function, BuildError> {
        let code = &self.bf.code;

        // Empty body → a function that returns NIL.
        if code.is_empty() {
            let entry = self.f.entry();
            let nil = self.emit_const_nil(entry);
            self.set_term(entry, ret(nil));
            return Ok(self.f);
        }

        self.find_leaders()?;
        self.compute_depths()?;
        self.create_blocks();
        self.compute_total_preds()?;
        self.seed_entry();
        self.process_blocks()?;
        self.simplify_trivial_phis();
        Ok(self.f)
    }

    // ── Pass 1: basic-block leaders ─────────────────────────────────

    fn find_leaders(&mut self) -> Result<(), BuildError> {
        let code = &self.bf.code;
        let mut set: BTreeSet<usize> = BTreeSet::new();
        set.insert(0);
        for (i, instr) in code.iter().enumerate() {
            match instr {
                Instr::Br(t) => {
                    set.insert(*t as usize);
                }
                Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => {
                    set.insert(*t as usize);
                    set.insert(i + 1); // fall-through
                }
                Instr::Go { target_bcp, .. } => {
                    set.insert(*target_bcp as usize);
                }
                _ => {}
            }
        }
        // Validate every leader lands inside the code.
        for &l in &set {
            if l >= code.len() {
                return Err(BuildError::Unsupported("branch target out of range"));
            }
        }
        self.leaders = set.into_iter().collect();
        Ok(())
    }

    // ── Pass 2: operand-stack depth at each reachable index ─────────

    fn compute_depths(&mut self) -> Result<(), BuildError> {
        let code = &self.bf.code;
        let mut depth_at: HashMap<usize, i32> = HashMap::new();
        let mut work: Vec<usize> = vec![0];
        depth_at.insert(0, 0);

        while let Some(i) = work.pop() {
            let d = depth_at[&i];
            // `push` propagates a depth to a successor index.
            let push = |idx: usize, nd: i32, depth_at: &mut HashMap<usize, i32>, work: &mut Vec<usize>| {
                if idx >= code.len() {
                    return;
                }
                if !depth_at.contains_key(&idx) {
                    depth_at.insert(idx, nd);
                    work.push(idx);
                }
            };
            match &code[i] {
                Instr::Const(_) | Instr::LoadLocal(_) | Instr::LoadGlobal(_) | Instr::Dup => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::StoreLocal(_) | Instr::StoreGlobal(_) | Instr::Pop => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::CallNamed { nargs, .. } => {
                    push(i + 1, d - (*nargs as i32) + 1, &mut depth_at, &mut work);
                }
                Instr::ClearMv => {
                    push(i + 1, d, &mut depth_at, &mut work); // no operand-stack effect
                }
                Instr::PushBlock { .. } | Instr::PushTag { .. } | Instr::PopHandler => {
                    push(i + 1, d, &mut depth_at, &mut work); // handler markers: no stack effect
                }
                Instr::Br(t) => {
                    push(*t as usize, d, &mut depth_at, &mut work);
                }
                Instr::Go { target_bcp, .. } => {
                    push(*target_bcp as usize, d, &mut depth_at, &mut work);
                }
                Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => {
                    push(*t as usize, d - 1, &mut depth_at, &mut work);
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::Return => {}
                _ => return Err(BuildError::Unsupported("opcode not modelled by T2 builder")),
            }
        }

        for (&idx, &d) in &depth_at {
            self.entry_depth.insert(idx, d.max(0) as usize);
        }
        Ok(())
    }

    // ── Pass 3: block creation ──────────────────────────────────────

    fn create_blocks(&mut self) {
        let leaders = self.leaders.clone();
        for &l in &leaders {
            let b = if l == 0 { self.f.entry() } else { self.f.make_block() };
            self.block_of.insert(l, b);
        }
        let n = self.f.num_blocks();
        self.total_preds = vec![0; n];
        self.seen_preds = vec![0; n];
        self.interpreted = vec![false; n];
        self.sealed = vec![false; n];
    }

    /// Inclusive-exclusive bytecode range of the block that starts at leader
    /// index `p` in `self.leaders`.
    fn block_range(&self, p: usize) -> (usize, usize) {
        let start = self.leaders[p];
        let end = self
            .leaders
            .get(p + 1)
            .copied()
            .unwrap_or(self.bf.code.len());
        (start, end)
    }

    // ── Pass 4: structural predecessor counts ───────────────────────

    fn compute_total_preds(&mut self) -> Result<(), BuildError> {
        for p in 0..self.leaders.len() {
            let (start, end) = self.block_range(p);
            for s in self.structural_succs(start, end)? {
                self.total_preds[s.index()] += 1;
            }
        }
        Ok(())
    }

    /// Successor blocks of the block spanning `[start, end)`. The block's
    /// terminator is the **first** control-flow instruction in the range — the
    /// same one `interpret_block` stops at — NOT `code[end-1]`: an unconditional
    /// `Br`/`Go`/`Return` is not followed by a leader, so unreachable trailing
    /// instructions (e.g. a `PopHandler` after a loop's `Go`) may sit after it in
    /// the same block. Reading the last instruction would then miscount edges and
    /// mis-seal loop headers (bliss-fe8). Only the successor multiset matters here
    /// (for predecessor counting); the real edge order comes from `finish_block`.
    fn structural_succs(&self, start: usize, end: usize) -> Result<Vec<Block>, BuildError> {
        let code = &self.bf.code;
        let blk = |t: usize| -> Result<Block, BuildError> {
            self.block_of
                .get(&t)
                .copied()
                .ok_or(BuildError::Unsupported("branch target is not a block leader"))
        };
        for instr in &code[start..end] {
            match instr {
                Instr::Br(t) => return Ok(vec![blk(*t as usize)?]),
                Instr::Go { target_bcp, .. } => return Ok(vec![blk(*target_bcp as usize)?]),
                Instr::BrIfTrue(t) => return Ok(vec![blk(*t as usize)?, blk(end)?]),
                Instr::BrIfFalse(t) => return Ok(vec![blk(end)?, blk(*t as usize)?]),
                Instr::Return => return Ok(vec![]),
                _ => {}
            }
        }
        // No terminator in range: fall through to the next leader (if any).
        if end < code.len() {
            Ok(vec![blk(end)?])
        } else {
            Ok(vec![])
        }
    }

    // ── Entry seeding ───────────────────────────────────────────────

    /// Seed the entry block: locals `0..arity` are function parameters (entry
    /// block parameters); locals `arity..n_locals` start as NIL so a read before
    /// the first store never hits an undefined variable.
    fn seed_entry(&mut self) {
        let entry = self.f.entry();
        let arity = self.bf.arity.min(self.bf.n_locals);
        for i in 0..arity {
            let p = self
                .f
                .add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            self.write_var(Var::Local(i), entry, p);
        }
        for i in arity..self.bf.n_locals {
            let nil = self.emit_const_nil(entry);
            self.write_var(Var::Local(i), entry, nil);
        }
    }

    // ── Main processing loop ────────────────────────────────────────

    fn process_blocks(&mut self) -> Result<(), BuildError> {
        for p in 0..self.leaders.len() {
            let leader = self.leaders[p];
            let block = self.block_of[&leader];

            // Unreachable non-entry block (no predecessors): keep it well-formed
            // with a Trap and move on.
            if block != self.f.entry() && self.total_preds[block.index()] == 0 {
                self.sealed[block.index()] = true;
                self.set_term(block, trap());
                self.interpreted[block.index()] = true;
                continue;
            }

            // Seal now if every predecessor has already been interpreted.
            if self.seen_preds[block.index()] == self.total_preds[block.index()] {
                self.seal(block);
            }

            self.interpret_block(p, block)?;
        }
        Ok(())
    }

    fn interpret_block(&mut self, p: usize, block: Block) -> Result<(), BuildError> {
        let (start, end) = self.block_range(p);
        let depth = self.entry_depth.get(&start).copied().unwrap_or(0);

        // Materialise the incoming operand stack.
        let mut stack: Vec<Value> = Vec::with_capacity(depth);
        for k in 0..depth {
            stack.push(self.read_var(Var::Stack(k as u16), block));
        }

        // Interpret straight-line instructions until a terminator (or the range
        // ends and we fall through).
        let mut term: Option<Term> = None;
        let code = &self.bf.code;
        for i in start..end {
            match &code[i] {
                Instr::Const(idx) => {
                    let v = self.emit_const(block, *idx)?;
                    stack.push(v);
                }
                Instr::LoadLocal(s) => {
                    let v = self.read_var(Var::Local(*s), block);
                    stack.push(v);
                }
                Instr::StoreLocal(s) => {
                    let v = stack.pop().ok_or(BuildError::Unsupported("stack underflow (StoreLocal)"))?;
                    self.write_var(Var::Local(*s), block, v);
                }
                Instr::LoadGlobal(sym) => {
                    let v = self.emit(
                        block,
                        Opcode::SymbolValue,
                        vec![],
                        AuxData::SymbolRef(*sym),
                        effectful(),
                        None,
                        IRType::TOP,
                    );
                    stack.push(v.expect("SymbolValue has a result"));
                }
                Instr::StoreGlobal(sym) => {
                    let v = stack.pop().ok_or(BuildError::Unsupported("stack underflow (StoreGlobal)"))?;
                    self.emit_effect(
                        block,
                        Opcode::SetSymbolValue,
                        vec![v],
                        AuxData::SymbolRef(*sym),
                        None,
                    );
                }
                Instr::Pop => {
                    stack.pop().ok_or(BuildError::Unsupported("stack underflow (Pop)"))?;
                }
                Instr::ClearMv => {
                    // Reset multiple-values state; no operand effect (bliss-mzp).
                    self.emit_effect(block, Opcode::ClearMv, vec![], AuxData::None, None);
                }
                Instr::PushBlock { sp_restore, .. } | Instr::PushTag { sp_restore, .. } => {
                    if *sp_restore != 0 {
                        return Err(BuildError::Unsupported("handler with non-empty sp_restore"));
                    }
                }
                Instr::PopHandler => {}
                Instr::Dup => {
                    let v = *stack.last().ok_or(BuildError::Unsupported("stack underflow (Dup)"))?;
                    stack.push(v);
                }
                Instr::CallNamed { sym, nargs } => {
                    let n = *nargs as usize;
                    if stack.len() < n {
                        return Err(BuildError::Unsupported("stack underflow (CallNamed)"));
                    }
                    // Compiler-known functions reach T2 through shared metadata
                    // and call-site policy. An expansion hook can still decline
                    // for operand-shape reasons (for example dynamic TYPEP),
                    // leaving the normal Call and its FrameState intact.
                    if let Some(metadata) = metadata_for_symbol(*sym) {
                        let policy = self.inline_options.policy_at(i as u32);
                        let decision = decide(
                            metadata,
                            *nargs,
                            policy,
                            self.inline_depth,
                            self.remaining_inline_budget,
                            self.inline_options.config,
                        );
                        if let InlineDecision::Expand(intrinsic) = decision {
                            if self.expand_intrinsic(intrinsic, block, &mut stack, i, start)? {
                                self.remaining_inline_budget -= metadata.cost;
                                continue;
                            }
                        }
                    }
                    // Snapshot the pre-call frame (args still live) for deopt.
                    let bcp = i as u32;
                    let fs = self.build_frame_state(block, &stack, bcp);
                    let split = stack.len() - n;
                    let args: Vec<Value> = stack.split_off(split); // arg0..arg{n-1}
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args,
                            results: vec![],
                            aux: AuxData::CallTarget(*sym),
                            flags: InstFlags { effectful: true, call: true, safepoint: true, ..InstFlags::default() },
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    let _ = inst;
                    stack.push(results[0]);
                }
                Instr::Br(t) => {
                    let s = self.block_of[&(*t as usize)];
                    term = Some(Term::Jump(s));
                    break;
                }
                Instr::Go { target_bcp, .. } => {
                    let s = self.block_of[&(*target_bcp as usize)];
                    term = Some(Term::Jump(s));
                    break;
                }
                Instr::BrIfFalse(t) => {
                    let cond = stack.pop().ok_or(BuildError::Unsupported("stack underflow (BrIfFalse)"))?;
                    let false_blk = self.block_of[&(*t as usize)];
                    let true_blk = self.block_of[&end]; // fall-through
                    term = Some(Term::Brif(cond, true_blk, false_blk));
                    break;
                }
                Instr::BrIfTrue(t) => {
                    let cond = stack.pop().ok_or(BuildError::Unsupported("stack underflow (BrIfTrue)"))?;
                    let true_blk = self.block_of[&(*t as usize)];
                    let false_blk = self.block_of[&end]; // fall-through
                    term = Some(Term::Brif(cond, true_blk, false_blk));
                    break;
                }
                Instr::Return => {
                    let v = match stack.pop() {
                        Some(v) => v,
                        None => self.emit_const_nil(block),
                    };
                    term = Some(Term::Ret(v));
                    break;
                }
                _ => return Err(BuildError::Unsupported("opcode not modelled by T2 builder")),
            }
        }

        // Publish this block's exit definition of each operand-stack slot so
        // successors can read them through `read_var`, then finish the block.
        self.finish_block(block, term, stack, end);
        Ok(())
    }

    /// Write exit stack defs, set the terminator, and do predecessor bookkeeping
    /// (marking edges and sealing any already-interpreted successor whose last
    /// predecessor this block is).
    fn finish_block(&mut self, block: Block, term: Option<Term>, stack: Vec<Value>, end: usize) {
        // The exit operand stack that flows to successors.
        let exit_stack: &[Value] = match &term {
            Some(Term::Ret(_)) => &[], // no successors
            _ => &stack,
        };
        for (k, &v) in exit_stack.iter().enumerate() {
            self.write_var(Var::Stack(k as u16), block, v);
        }

        // Build the terminator and record its outgoing edges as (successor, idx).
        let (data, edges): (InstData, Vec<Block>) = match term {
            Some(Term::Jump(s)) => (jump(s), vec![s]),
            Some(Term::Brif(cond, t, f)) => (brif(cond, t, f), vec![t, f]),
            Some(Term::Ret(v)) => (ret(v), vec![]),
            None => {
                // Fall through to the next leader, or return NIL at code end.
                if end < self.bf.code.len() {
                    let s = self.block_of[&end];
                    (jump(s), vec![s])
                } else {
                    let nil = self.emit_const_nil(block);
                    (ret(nil), vec![])
                }
            }
        };

        let inst = self.f.set_terminator(block, data);
        self.terminator_inst.insert(block, inst);
        self.interpreted[block.index()] = true;

        for (idx, s) in edges.into_iter().enumerate() {
            self.pred_edges.entry(s).or_default().push((block, idx));
            self.seen_preds[s.index()] += 1;
            // A successor already interpreted (a loop header) becomes sealable
            // once its final predecessor — this block — is in.
            if self.interpreted[s.index()]
                && !self.sealed[s.index()]
                && self.seen_preds[s.index()] == self.total_preds[s.index()]
            {
                self.seal(s);
            }
        }
    }

    // ── Braun SSA construction primitives (spec §4.3.5.1) ───────────

    fn write_var(&mut self, var: Var, block: Block, v: Value) {
        self.current_def.insert((var, block), v);
    }

    fn read_var(&mut self, var: Var, block: Block) -> Value {
        if let Some(&v) = self.current_def.get(&(var, block)) {
            return v;
        }
        if !self.sealed[block.index()] {
            // May still gain predecessors: record an incomplete phi.
            let phi = self
                .f
                .add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
            self.incomplete_phis.entry(block).or_default().push((var, phi));
            self.current_def.insert((var, block), phi);
            return phi;
        }
        let preds = self.pred_edges.get(&block).cloned().unwrap_or_default();
        let v = if preds.len() == 1 {
            let val = self.read_var(var, preds[0].0);
            self.current_def.insert((var, block), val);
            val
        } else {
            // Zero or many predecessors: introduce a phi (block parameter).
            let phi = self
                .f
                .add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
            self.current_def.insert((var, block), phi); // break cycles first
            self.add_phi_operands(var, block, phi);
            phi
        };
        v
    }

    fn add_phi_operands(&mut self, var: Var, block: Block, _phi: Value) {
        let preds = self.pred_edges.get(&block).cloned().unwrap_or_default();
        for (pred, idx) in preds {
            let val = self.read_var(var, pred);
            let term = self.terminator_inst[&pred];
            self.f.inst_mut(term).targets[idx].args.push(val);
        }
    }

    fn seal(&mut self, block: Block) {
        if self.sealed[block.index()] {
            return;
        }
        if let Some(phis) = self.incomplete_phis.remove(&block) {
            for (var, phi) in phis {
                self.add_phi_operands(var, block, phi);
            }
        }
        self.sealed[block.index()] = true;
    }

    // ── Trivial-phi elimination (Braun `tryRemoveTrivialPhi`, spec §4.3.5.1) ──

    /// Collapse trivial block parameters as a fixpoint post-pass. A parameter
    /// whose incoming arguments (across every predecessor edge) are all either
    /// itself or one single other value `u` is a trivial phi ≡ `u`. The base
    /// Braun construction leaves these behind for loop-invariant values — e.g. a
    /// `dotimes` bound read inside the loop gets a header phi even though it never
    /// changes — and each one needlessly pins a register, which is what pushes a
    /// global-accumulator loop past the framed emitter's register budget
    /// (bliss-fe8). Removing a parameter drops it from the block and drops the
    /// matching argument from every predecessor edge, keeping the two positionally
    /// consistent (the emitter correlates params↔args by position, not by id).
    fn simplify_trivial_phis(&mut self) {
        loop {
            let mut found: Option<(Block, usize, Value, Value)> = None;
            'outer: for &b in self.block_of.values() {
                let preds = match self.pred_edges.get(&b) {
                    Some(p) if !p.is_empty() => p.clone(),
                    _ => continue, // entry / unreachable: no incoming phi arguments
                };
                let nparams = self.f.block(b).params.len();
                for pos in 0..nparams {
                    let p = self.f.block(b).params[pos];
                    let mut other: Option<Value> = None;
                    let mut trivial = true;
                    for &(pred, idx) in &preds {
                        let term = self.terminator_inst[&pred];
                        let arg = self.f.inst(term).targets[idx].args[pos];
                        if arg == p {
                            continue; // self-reference: does not disqualify
                        }
                        match other {
                            None => other = Some(arg),
                            Some(u) if u == arg => {}
                            Some(_) => {
                                trivial = false;
                                break;
                            }
                        }
                    }
                    // `other == None` means every edge feeds the phi itself — an
                    // undefined/dead cycle; leave it (a real definition never is).
                    if trivial {
                        if let Some(u) = other {
                            found = Some((b, pos, p, u));
                            break 'outer;
                        }
                    }
                }
            }
            let Some((b, pos, p, u)) = found else { break };
            self.replace_value(p, u);
            self.f.block_mut(b).params.remove(pos);
            for &(pred, idx) in &self.pred_edges[&b].clone() {
                let term = self.terminator_inst[&pred];
                self.f.inst_mut(term).targets[idx].args.remove(pos);
            }
        }
    }

    /// Replace every use of value `p` with `u` — in instruction operands, in edge
    /// (block-call) arguments, and in deopt frame states — so a removed trivial
    /// phi leaves no dangling reference.
    fn replace_value(&mut self, p: Value, u: Value) {
        let blocks: Vec<Block> = self.block_of.values().copied().collect();
        for b in blocks {
            let insts = self.f.block(b).insts.clone();
            for inst in insts {
                let d = self.f.inst_mut(inst);
                for a in d.args.iter_mut() {
                    if *a == p {
                        *a = u;
                    }
                }
                for tc in d.targets.iter_mut() {
                    for a in tc.args.iter_mut() {
                        if *a == p {
                            *a = u;
                        }
                    }
                }
            }
        }
        for i in 0..self.f.frame_states.len() {
            let fs = self.f.frame_states.get_mut(crate::t2::frame_state::FrameStateId(i as u32));
            for scope in fs.scopes.iter_mut() {
                for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                    if let ValueSource::Value { value, .. } = src {
                        if *value == p {
                            *value = u;
                        }
                    }
                }
            }
        }
    }

    // ── Frame state (deopt anchor) ──────────────────────────────────

    fn build_frame_state(
        &mut self,
        block: Block,
        stack: &[Value],
        bcp: u32,
    ) -> crate::t2::frame_state::FrameStateId {
        let mut locals = Vec::with_capacity(self.bf.n_locals as usize);
        for i in 0..self.bf.n_locals {
            let v = self.read_var(Var::Local(i), block);
            locals.push(ValueSource::Value {
                value: v,
                repr: ValueRepresentation::Tagged,
            });
        }
        let stack_srcs = stack
            .iter()
            .map(|&v| ValueSource::Value {
                value: v,
                repr: ValueRepresentation::Tagged,
            })
            .collect();
        let scope = FrameScope {
            function: 0, // symbol id unavailable at bytecode level; placeholder.
            bcp,
            locals,
            stack: stack_srcs,
        };
        self.f.frame_states.add(FrameState {
            scopes: vec![scope],
            remat: vec![],
        })
    }

    /// Materialise one metadata-selected leaf expansion. `Ok(false)` means the
    /// hook does not support this operand shape, so the normal call is retained.
    /// The operand stack is changed only after support is proven.
    fn expand_intrinsic(
        &mut self,
        intrinsic: IntrinsicId,
        block: Block,
        stack: &mut Vec<Value>,
        bcp: usize,
        block_start: usize,
    ) -> Result<bool, BuildError> {
        if intrinsic == IntrinsicId::FirstChar {
            // Inline the metadata-owned body below the CL function layer.  Both
            // layout operations are guards anchored at the original call: a
            // wrong type, empty string, or non-ASCII representation resumes at
            // FIRST-CHAR in T0/T1, which evaluates the full Lisp definition and
            // returns NIL or the Unicode character as appropriate.
            let fs = self.build_frame_state(block, stack, bcp as u32);
            let string = stack
                .pop()
                .ok_or(BuildError::Unsupported("stack underflow (FIRST-CHAR)"))?;
            let guard_flags = InstFlags {
                effectful: true,
                guard: true,
                ..InstFlags::default()
            };
            let _byte_length = self
                .emit(
                    block,
                    Opcode::StringByteLength,
                    vec![string],
                    AuxData::None,
                    guard_flags,
                    Some(fs),
                    IRType::of(TypeBits::FIXNUM),
                )
                .ok_or(BuildError::Unsupported("StringByteLength has a result"))?;
            let zero = self
                .emit(
                    block,
                    Opcode::ConstFixnum,
                    vec![],
                    AuxData::FixnumImm(0),
                    InstFlags::default(),
                    None,
                    IRType::of(TypeBits::FIXNUM),
                )
                .expect("ConstFixnum has a result");
            let result = self
                .emit(
                    block,
                    Opcode::StringAsciiCharAt,
                    vec![string, zero],
                    AuxData::None,
                    guard_flags,
                    Some(fs),
                    IRType::of(TypeBits::CHARACTER),
                )
                .ok_or(BuildError::Unsupported("StringAsciiCharAt has a result"))?;
            stack.push(result);
            return Ok(true);
        }

        let type_bits = match intrinsic {
            IntrinsicId::Consp => Some(TypeBits::CONS),
            IntrinsicId::Symbolp => Some(TypeBits::SYMBOL),
            IntrinsicId::Integerp => Some(TypeBits::FIXNUM.join(TypeBits::BIGNUM)),
            IntrinsicId::Stringp => Some(TypeBits::STRING),
            IntrinsicId::TypepConstant => {
                if bcp <= block_start {
                    return Ok(false);
                }
                let Instr::Const(cidx) = self.bf.code[bcp - 1] else {
                    return Ok(false);
                };
                let Some(type_name) = self.bf.constants
                    .get(cidx as usize)
                    .copied()
                    .filter(|v| v.is_symbol())
                    .and_then(|v| crate::reader::symbol_name(v.as_symbol_index()))
                else {
                    return Ok(false);
                };
                match type_name.rsplit(':').next().unwrap_or(&type_name) {
                    "FIXNUM" => Some(TypeBits::FIXNUM),
                    "CONS" => Some(TypeBits::CONS),
                    "SYMBOL" => Some(TypeBits::SYMBOL),
                    "INTEGER" => Some(TypeBits::FIXNUM.join(TypeBits::BIGNUM)),
                    _ => return Ok(false),
                }
            }
            IntrinsicId::Eq | IntrinsicId::Null => None,
            IntrinsicId::FirstChar => unreachable!("handled as an inline body above"),
        };

        if let Some(bits) = type_bits {
            if intrinsic == IntrinsicId::TypepConstant {
                // Its ConstSymbol is now dead and the ordinary DCE pass removes it.
                stack.pop().ok_or(BuildError::Unsupported("stack underflow (TYPEP type)"))?;
            }
            let x = stack.pop().ok_or(BuildError::Unsupported("stack underflow (type predicate)"))?;
            let result = self.emit(
                block,
                Opcode::TypeCheck,
                vec![x],
                AuxData::TypeTag(IRType::of(bits)),
                InstFlags::default(),
                None,
                IRType::TOP,
            ).ok_or(BuildError::Unsupported("TypeCheck has a result"))?;
            stack.push(result);
            return Ok(true);
        }

        let args = match intrinsic {
            IntrinsicId::Null => {
                let x = stack.pop().ok_or(BuildError::Unsupported("stack underflow (NULL)"))?;
                vec![x, self.emit_const_nil(block)]
            }
            IntrinsicId::Eq => {
                let b = stack.pop().ok_or(BuildError::Unsupported("stack underflow (EQ rhs)"))?;
                let a = stack.pop().ok_or(BuildError::Unsupported("stack underflow (EQ lhs)"))?;
                vec![a, b]
            }
            IntrinsicId::Consp | IntrinsicId::Symbolp | IntrinsicId::Integerp
            | IntrinsicId::TypepConstant | IntrinsicId::Stringp => {
                unreachable!("handled as TypeCheck above")
            }
            IntrinsicId::FirstChar => unreachable!("handled as an inline body above"),
        };
        let result = self.emit(
            block,
            Opcode::GenericEq,
            args,
            AuxData::None,
            InstFlags::default(),
            None,
            IRType::TOP,
        ).ok_or(BuildError::Unsupported("GenericEq has a result"))?;
        stack.push(result);
        Ok(true)
    }

    // ── Instruction emit helpers ────────────────────────────────────

    fn set_term(&mut self, block: Block, data: InstData) {
        let inst = self.f.set_terminator(block, data);
        self.terminator_inst.insert(block, inst);
    }

    /// Emit a pure/effectful non-terminator with a single result, returning it.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        aux: AuxData,
        flags: InstFlags,
        frame_state: Option<crate::t2::frame_state::FrameStateId>,
        ty: IRType,
    ) -> Option<Value> {
        let (_i, results) = self.f.push_inst(
            block,
            InstData {
                opcode,
                args,
                results: vec![],
                aux,
                flags,
                targets: vec![],
                frame_state,
                source_pos: 0,
            },
            &[(ty, ValueRepresentation::Tagged)],
        );
        results.into_iter().next()
    }

    /// Emit a result-less effectful instruction.
    fn emit_effect(
        &mut self,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        aux: AuxData,
        frame_state: Option<crate::t2::frame_state::FrameStateId>,
    ) {
        self.f.push_inst(
            block,
            InstData {
                opcode,
                args,
                results: vec![],
                aux,
                flags: effectful(),
                targets: vec![],
                frame_state,
                source_pos: 0,
            },
            &[],
        );
    }

    fn emit_const_nil(&mut self, block: Block) -> Value {
        self.emit(
            block,
            Opcode::ConstNil,
            vec![],
            AuxData::None,
            InstFlags::default(),
            None,
            IRType::of(TypeBits::NULL),
        )
        .expect("ConstNil has a result")
    }

    /// Classify a constant-pool `BlissVal` and emit the matching `Const*`.
    fn emit_const(&mut self, block: Block, idx: u16) -> Result<Value, BuildError> {
        let val = *self
            .bf
            .constants
            .get(idx as usize)
            .ok_or(BuildError::Unsupported("constant index out of range"))?;

        let (opcode, aux, ty) = if val.is_nil() {
            (Opcode::ConstNil, AuxData::None, IRType::of(TypeBits::NULL))
        } else if val == bliss_rt::value::T {
            (Opcode::ConstT, AuxData::None, IRType::of(TypeBits::SYMBOL))
        } else if val.is_fixnum() {
            (
                Opcode::ConstFixnum,
                AuxData::FixnumImm(val.as_fixnum()),
                IRType::of(TypeBits::FIXNUM),
            )
        } else if val.is_character() {
            (
                Opcode::ConstChar,
                AuxData::CharImm(val.as_char()),
                IRType::of(TypeBits::CHARACTER),
            )
        } else if val.is_single_float() {
            (
                Opcode::ConstFloat,
                AuxData::FloatImm(val.as_single_float()),
                IRType::of(TypeBits::SINGLE_FLOAT),
            )
        } else if val.tag() == bliss_rt::value::TAG_SYMBOL {
            (
                Opcode::ConstSymbol,
                AuxData::SymbolRef(val.as_symbol_index()),
                IRType::of(TypeBits::SYMBOL),
            )
        } else {
            // Cons / heap object / function / other immediate: carry the literal.
            (Opcode::ConstHeapObj, AuxData::HeapLiteral(val), IRType::TOP)
        };

        Ok(self
            .emit(block, opcode, vec![], aux, InstFlags::default(), None, ty)
            .expect("Const* has a result"))
    }
}

/// A block's control-flow conclusion, captured during interpretation.
enum Term {
    Jump(Block),
    /// `Brif(cond, taken_when_true, taken_when_false)`.
    Brif(Value, Block, Block),
    Ret(Value),
}

// ── Free helpers for terminator InstData ────────────────────────────

fn effectful() -> InstFlags {
    InstFlags {
        effectful: true,
        ..InstFlags::default()
    }
}

fn jump(target: Block) -> InstData {
    InstData {
        opcode: Opcode::Jump,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![crate::t2::ir::BlockCall { block: target, args: vec![] }],
        frame_state: None,
        source_pos: 0,
    }
}

fn brif(cond: Value, t: Block, f: Block) -> InstData {
    InstData {
        opcode: Opcode::Brif,
        args: vec![cond],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![
            crate::t2::ir::BlockCall { block: t, args: vec![] },
            crate::t2::ir::BlockCall { block: f, args: vec![] },
        ],
        frame_state: None,
        source_pos: 0,
    }
}

fn ret(v: Value) -> InstData {
    InstData {
        opcode: Opcode::Return,
        args: vec![v],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

fn trap() -> InstData {
    InstData {
        opcode: Opcode::Trap,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::inlining::{InlineConfig, InlineOptions, InlinePolicy};
    use crate::t2::ir::Opcode;
    use bliss_rt::value::BlissVal;

    fn bf(name: &str, code: Vec<Instr>, constants: Vec<BlissVal>, n_locals: u16, arity: u16, max_stack: u16) -> BytecodeFunction {
        BytecodeFunction {
            code,
            constants,
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            param_layout: vec![],
            has_env: false,
            n_locals,
            max_stack,
            arity,
            name: name.to_string(),
            params_form: bliss_rt::value::NIL,
            min_args: arity,
            max_args: Some(arity),
            variadic: false,
        }
    }

    fn term_opcode(f: &Function, b: Block) -> Opcode {
        let t = f.terminator(b).expect("block terminated");
        f.inst(t).opcode
    }

    fn has_opcode(f: &Function, b: Block, op: Opcode) -> bool {
        f.block(b)
            .insts
            .iter()
            .any(|&i| f.inst(i).opcode == op)
    }

    #[test]
    fn returns_constant() {
        // (lambda () 42)
        let f = build_from_bytecode(&bf(
            "k",
            vec![Instr::Const(0), Instr::Return],
            vec![BlissVal::from_fixnum(42)],
            0,
            0,
            1,
        ))
        .expect("builds");

        assert_eq!(f.num_blocks(), 1);
        let entry = f.entry();
        assert!(has_opcode(&f, entry, Opcode::ConstFixnum));
        assert_eq!(term_opcode(&f, entry), Opcode::Return);
        // The returned value is the const's result.
        let t = f.terminator(entry).unwrap();
        assert_eq!(f.inst(t).args.len(), 1);
    }

    #[test]
    fn if_merge_has_join_parameter() {
        // (lambda (c) (if c 10 20))
        //  0: LoadLocal 0     ; c
        //  1: BrIfFalse 4     ; false -> else
        //  2: Const 0 (=10)   ; then
        //  3: Br 5
        //  4: Const 1 (=20)   ; else
        //  5: Return          ; merge
        let f = build_from_bytecode(&bf(
            "iff",
            vec![
                Instr::LoadLocal(0),
                Instr::BrIfFalse(4),
                Instr::Const(0),
                Instr::Br(5),
                Instr::Const(1),
                Instr::Return,
            ],
            vec![BlissVal::from_fixnum(10), BlissVal::from_fixnum(20)],
            1,
            1,
            2,
        ))
        .expect("builds");

        // entry(0), then(2), else(4), merge(5).
        assert_eq!(f.num_blocks(), 4);
        let order = f.block_order().to_vec();
        let entry = order[0];
        let merge = order[3];

        // The entry ends in a conditional branch with two successors.
        assert_eq!(term_opcode(&f, entry), Opcode::Brif);
        assert_eq!(f.succs(entry).len(), 2);

        // The merge block joins the two arms via exactly one block parameter
        // (the merged result value), and returns it.
        assert_eq!(f.preds(merge).len(), 2);
        assert_eq!(
            f.block(merge).params.len(),
            1,
            "merge should have a single phi parameter for the joined value"
        );
        assert_eq!(term_opcode(&f, merge), Opcode::Return);

        // Both incoming edges to the merge carry exactly one argument.
        for pred in f.preds(merge) {
            let t = f.terminator(pred).unwrap();
            let call = f
                .inst(t)
                .targets
                .iter()
                .find(|c| c.block == merge)
                .unwrap();
            assert_eq!(call.args.len(), 1, "edge into merge passes the joined value");
        }
    }

    #[test]
    fn counted_loop_header_has_parameter() {
        // A loop whose header carries local 0 across the back edge:
        //  0: Const 0            ; A
        //  1: StoreLocal 0       ; l0 = A
        //  2: LoadLocal 0        ; header
        //  3: BrIfFalse 7        ; exit when false
        //  4: Const 1            ; body: B
        //  5: StoreLocal 0       ; l0 = B
        //  6: Br 2               ; back-edge
        //  7: LoadLocal 0        ; exit
        //  8: Return
        let f = build_from_bytecode(&bf(
            "loop",
            vec![
                Instr::Const(0),
                Instr::StoreLocal(0),
                Instr::LoadLocal(0),
                Instr::BrIfFalse(7),
                Instr::Const(1),
                Instr::StoreLocal(0),
                Instr::Br(2),
                Instr::LoadLocal(0),
                Instr::Return,
            ],
            vec![BlissVal::from_fixnum(3), BlissVal::from_fixnum(0)],
            1,
            0,
            1,
        ))
        .expect("builds");

        // entry(0), header(2), body(4), exit(7).
        assert_eq!(f.num_blocks(), 4);
        let order = f.block_order().to_vec();
        let header = order[1];
        let body = order[2];

        // The header is a real loop header: two predecessors (entry + back-edge).
        let preds = f.preds(header);
        assert_eq!(preds.len(), 2, "header has forward + back edge");
        assert!(preds.contains(&body), "body is the back-edge predecessor");

        // The loop-carried local became a header block parameter (an SSA phi).
        assert!(
            !f.block(header).params.is_empty(),
            "header must carry a loop-carried parameter"
        );
        assert_eq!(term_opcode(&f, header), Opcode::Brif);

        // The back-edge is a Jump to the header passing one argument per header
        // parameter.
        let bt = f.terminator(body).unwrap();
        assert_eq!(f.inst(bt).opcode, Opcode::Jump);
        let back = f
            .inst(bt)
            .targets
            .iter()
            .find(|c| c.block == header)
            .unwrap();
        assert_eq!(
            back.args.len(),
            f.block(header).params.len(),
            "back-edge args match header params"
        );
    }

    #[test]
    fn call_carries_frame_state() {
        // (lambda (x) (foo x)) — one named call, which must anchor a FrameState.
        let f = build_from_bytecode(&bf(
            "call",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: 7, nargs: 1 },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            2,
        ))
        .expect("builds");

        let entry = f.entry();
        let call = f
            .block(entry)
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("has a call");
        assert!(call.frame_state.is_some(), "call must carry a FrameState");
        assert_eq!(call.args.len(), 1, "one argument popped for the call");
        assert!(!f.frame_states.is_empty(), "frame-state table populated");
    }

    #[test]
    fn notinline_keeps_known_function_as_a_call() {
        let eq = bliss_rt::symbols::intern("EQ");
        let input = bf(
            "notinline-eq",
            vec![
                Instr::LoadLocal(0),
                Instr::LoadLocal(1),
                Instr::CallNamed { sym: eq, nargs: 2 },
                Instr::Return,
            ],
            vec![], 2, 2, 2,
        );
        let options = InlineOptions::default().with_policy(2, InlinePolicy::NotInline);
        let f = build_from_bytecode_with_inline_options(&input, options).expect("builds");
        let call = f.block(f.entry()).insts.iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("NOTINLINE must retain the call");
        assert!(call.frame_state.is_some(), "retained call keeps precise deopt state");
        assert!(!has_opcode(&f, f.entry(), Opcode::GenericEq));
    }

    #[test]
    fn budget_and_depth_limits_keep_known_function_as_a_call() {
        let null = bliss_rt::symbols::intern("NULL");
        let input = bf(
            "limited-null",
            vec![Instr::LoadLocal(0), Instr::CallNamed { sym: null, nargs: 1 }, Instr::Return],
            vec![], 1, 1, 1,
        );

        let mut no_budget = InlineOptions::default();
        no_budget.config.node_budget = 0;
        let f = build_from_bytecode_with_inline_options(&input, no_budget).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));

        let mut no_depth = InlineOptions::default();
        no_depth.config = InlineConfig { max_depth: 0, ..InlineConfig::default() };
        let f = build_from_bytecode_with_inline_options(&input, no_depth).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));
    }

    #[test]
    fn inline_policy_overrides_profitability_threshold() {
        let null = bliss_rt::symbols::intern("NULL");
        let input = bf(
            "explicit-inline-null",
            vec![Instr::LoadLocal(0), Instr::CallNamed { sym: null, nargs: 1 }, Instr::Return],
            vec![], 1, 1, 1,
        );

        let mut default_policy = InlineOptions::default();
        default_policy.config.small_threshold = 0;
        let f = build_from_bytecode_with_inline_options(&input, default_policy).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));

        let mut explicit_inline = InlineOptions::default().with_policy(1, InlinePolicy::Inline);
        explicit_inline.config.small_threshold = 0;
        let f = build_from_bytecode_with_inline_options(&input, explicit_inline).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::GenericEq));
        assert!(!has_opcode(&f, f.entry(), Opcode::Call));
    }

    #[test]
    fn unsupported_typep_shape_keeps_call_and_frame_state() {
        let typep = bliss_rt::symbols::intern("TYPEP");
        let input = bf(
            "dynamic-typep",
            vec![
                Instr::LoadLocal(0),
                Instr::LoadLocal(1),
                Instr::CallNamed { sym: typep, nargs: 2 },
                Instr::Return,
            ],
            vec![], 2, 2, 2,
        );
        let f = build_from_bytecode(&input).expect("builds");
        let call = f.block(f.entry()).insts.iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("dynamic TYPEP must stay a call");
        assert!(call.frame_state.is_some());
    }

    #[test]
    fn unsupported_opcode_is_reported() {
        // `Throw` is a deferred opcode → the builder must decline, not panic.
        let r = build_from_bytecode(&bf(
            "nlx",
            vec![Instr::Throw, Instr::Return],
            vec![],
            0,
            0,
            2,
        ));
        assert!(matches!(r, Err(BuildError::Unsupported(_))));
    }
}
