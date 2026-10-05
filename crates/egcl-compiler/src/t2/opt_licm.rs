// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Loop-invariant code motion over the T2 SSA IR.
//!
//! # Purpose
//!
//! [`Licm`] finds natural loops, gives each a preheader, and moves pure
//! loop-invariant instructions out of the body into that preheader. It owns
//! its own loop detection (a back-edge scan over the dominator tree), as does
//! `opt_guard`; `pass::Analyses` caches only the dominator tree. The pass never
//! touches effectful, guard, call, safepoint, or
//! terminator instructions and never hoists memory accesses, so it cannot
//! change deopt behaviour: nothing it moves carries a `FrameState`, and no
//! value identity changes.
//!
//! Not in the production T2 pipeline today (the driver runs fold, GVN, guard
//! elimination, DCE); exercised by the integration tests.
//!
//! # Contract
//!
//! **Input:** a well-formed `Function` in block-parameter SSA. Loops may have
//! several back-edges and several entry edges.
//!
//! **Output:** the same function with, for each loop that had something to
//! hoist, a preheader block that is the header's sole non-back-edge
//! predecessor and the hoisted instructions appended to it just before its
//! `Jump`. If the header already had exactly one outside predecessor that
//! jumps only to the header, that block is reused; otherwise a fresh block is
//! synthesised, given parameters mirroring the header's, and every outside
//! entry edge is redirected to it with its existing arguments. SSA dominance
//! is preserved because the preheader dominates the header and hence every
//! body block. `Analyses::invalidate` is called when anything moved.
//!
//! # Algorithm
//!
//! 1. **Loop detection.** For each edge `b → h` where `h` dominates `b`, the
//!    body is `{h} ∪ {n : n reaches b without passing through h}` (backward
//!    flood from `b` that never expands past `h`). Bodies for back-edges that
//!    share a header are unioned into one `NaturalLoop`.
//! 2. **Candidate selection**, to a fixpoint per loop. An instruction is
//!    hoistable when `is_pure_hoistable` accepts it (no effectful / guard /
//!    call / terminator / safepoint flag, and its opcode is on the
//!    `opcode_hoist_safe` allow-list: constants, fixnum/float/generic
//!    arithmetic, logic, box/unbox, comparisons, `TypeCheck`, `InstanceOf`)
//!    and every operand is defined outside the body or is itself already
//!    marked for hoisting. The fixpoint is what lets a chain of invariants
//!    hoist together; discovery order respects operand dependencies.
//! 3. **Preheader** via `ensure_preheader` (reuse or synthesise, above).
//! 4. **Relocation.** Hoisted instructions are removed from their body blocks
//!    and inserted into the preheader before its terminator, in discovery
//!    order.
//!
//! # OSR interaction (bliss-enc)
//!
//! An OSR entry block is an alternate CFG entry that jumps straight into the
//! loop header importing the live loop-carried slots. The header does not
//! dominate it (it is not even reachable from the function entry), so it
//! classifies as an outside predecessor and is redirected through the
//! synthesised preheader like the normal entry edge. That is what makes
//! hoisting safe on the OSR path. The ordering requirement this implies: the
//! OSR entry region must be materialised in the CFG **before** LICM runs. An
//! OSR edge added after hoisting would bypass the preheader and skip the
//! hoisted computations. `hoist_respects_preexisting_osr_entry` pins this.
//!
//! # Limits
//!
//! * No alias analysis, so every memory opcode is pinned, including loads
//!   whose address is invariant.
//! * Nested loops are treated as independent flat bodies; an instruction is
//!   hoisted one level per pass run, not to the outermost valid loop.
//! * Hoisting is unconditional with respect to execution frequency: an
//!   instruction on a conditional path inside the body is still hoisted if
//!   pure and invariant, which is safe (pure) but may compute an unused value.

use crate::t2::ir::{
    AuxData, Block, BlockCall, DominatorTree, Function, Inst, InstData, InstFlags, Opcode, ValueDef,
};
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Licm;

impl Pass for Licm {
    fn name(&self) -> &'static str {
        "licm"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        // Detect natural loops from the *current* CFG, then transform each.
        let dom = f.dominators();
        let loops = detect_loops(f, &dom);

        let mut changed = false;
        for lp in &loops {
            if hoist_loop(f, &dom, lp) {
                changed = true;
            }
        }

        if changed {
            // We mutated the CFG (new preheader blocks, redirected edges) and
            // moved value definitions between blocks.
            a.invalidate();
        }
    }
}

/// A natural loop: its header and the set of blocks in its body (header
/// included). Bodies are unioned across all back-edges sharing a header.
struct NaturalLoop {
    header: Block,
    body: Vec<Block>,
}

impl NaturalLoop {
    fn contains(&self, b: Block) -> bool {
        self.body.contains(&b)
    }
}

/// Identify natural loops: a CFG edge `b → h` is a back-edge
/// when `h` dominates `b`; the loop body is `{h} ∪ {n : n reaches b without
/// passing through h}`. Bodies for back-edges that share a header are unioned.
fn detect_loops(f: &Function, dom: &DominatorTree) -> Vec<NaturalLoop> {
    let mut loops: Vec<NaturalLoop> = Vec::new();

    for &b in f.block_order() {
        for h in f.succs(b) {
            if !dom.dominates(h, b) {
                continue; // not a back-edge
            }
            // Natural-loop body for the back-edge b → h.
            let body = loop_body(f, h, b);
            // Merge into an existing loop with the same header, else push.
            if let Some(existing) = loops.iter_mut().find(|l| l.header == h) {
                for blk in body {
                    if !existing.body.contains(&blk) {
                        existing.body.push(blk);
                    }
                }
            } else {
                loops.push(NaturalLoop { header: h, body });
            }
        }
    }

    loops
}

/// Collect the loop body of back-edge `tail → header`: `{header}` plus every
/// block that can reach `tail` without passing through `header` (standard
/// backward flood that never expands past the header).
fn loop_body(f: &Function, header: Block, tail: Block) -> Vec<Block> {
    let mut body = vec![header];
    let mut stack = Vec::new();
    if tail != header {
        body.push(tail);
        stack.push(tail);
    }
    while let Some(n) = stack.pop() {
        for p in f.preds(n) {
            if !body.contains(&p) {
                body.push(p);
                stack.push(p);
            }
        }
    }
    body
}

/// Transform one loop: find hoistable instructions, materialise a preheader,
/// and move them. Returns whether anything was hoisted.
fn hoist_loop(f: &mut Function, dom: &DominatorTree, lp: &NaturalLoop) -> bool {
    // Map every instruction to the block that currently contains it.
    let inst_block = build_inst_block(f);

    // Iterate to a fixpoint so that chains of invariants hoist together: an
    // instruction becomes hoistable once all its operands are defined outside
    // the loop *or* have themselves been marked for hoisting.
    let mut hoisted: Vec<Inst> = Vec::new();
    loop {
        let mut progressed = false;
        for &b in &lp.body {
            // Snapshot the block's instruction list (indices) — we don't mutate
            // it during detection.
            let insts: Vec<Inst> = f.block(b).insts.clone();
            for i in insts {
                if hoisted.contains(&i) {
                    continue;
                }
                if !is_pure_hoistable(f, i) {
                    continue;
                }
                if operands_invariant(f, i, lp, &inst_block, &hoisted) {
                    hoisted.push(i);
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }

    if hoisted.is_empty() {
        return false;
    }

    // Obtain a preheader (reuse a clean single entry pred, else create one).
    let ph = ensure_preheader(f, dom, lp);

    // Physically relocate the hoisted instructions.
    //  1. Remove them from their current (in-loop) blocks.
    for &b in &lp.body {
        f.block_mut(b).insts.retain(|i| !hoisted.contains(i));
    }
    //  2. Insert them into the preheader, just before its terminator, in the
    //     order discovered (which respects operand dependencies).
    let term = f
        .block_mut(ph)
        .insts
        .pop()
        .expect("preheader must be terminated");
    for &i in &hoisted {
        f.block_mut(ph).insts.push(i);
    }
    f.block_mut(ph).insts.push(term);

    true
}

/// Build an `inst index → defining block` table for the whole function.
fn build_inst_block(f: &Function) -> Vec<Option<Block>> {
    let mut map = vec![None; f.num_insts()];
    for &b in f.block_order() {
        for &i in &f.block(b).insts {
            if i.index() < map.len() {
                map[i.index()] = Some(b);
            }
        }
    }
    map
}

/// Is instruction `i` pure and shape-eligible for hoisting? Rejects anything
/// effectful / guard / call / terminator / safepoint, and — conservatively —
/// every memory-access opcode: a load would need a proven-invariant address and
/// a write barrier must stay adjacent to its store. We do not yet do alias
/// analysis, so memory ops are pinned.
fn is_pure_hoistable(f: &Function, i: Inst) -> bool {
    let data = f.inst(i);
    let fl: InstFlags = data.flags;
    if fl.effectful || fl.guard || fl.call || fl.terminator || fl.safepoint {
        return false;
    }
    opcode_hoist_safe(data.opcode)
}

/// Whitelist of opcodes safe to hoist (pure computation only). Memory access,
/// allocation, calls, guards, and terminators are excluded.
fn opcode_hoist_safe(op: Opcode) -> bool {
    use Opcode::*;
    matches!(
        op,
        ConstFixnum
            | ConstFloat
            | ConstChar
            | ConstSymbol
            | ConstNil
            | ConstT
            | ConstHeapObj
            | FixnumAdd
            | FixnumSub
            | FixnumMul
            | FixnumDiv
            | FixnumRem
            | FixnumMod
            | FixnumNeg
            | FixnumShl
            | FixnumShr
            | FloatAdd
            | FloatSub
            | FloatMul
            | FloatDiv
            | GenericAdd
            | GenericSub
            | GenericMul
            | GenericDiv
            | LogAnd
            | LogOr
            | LogXor
            | LogNot
            | BoxFixnum
            | UnboxFixnum
            | BoxFloat
            | UnboxFloat
            | WidenI32
            | FixnumCmpEq
            | FixnumCmpLt
            | FixnumCmpLe
            | FixnumCmpGt
            | FixnumCmpGe
            | FloatCmpEq
            | FloatCmpLt
            | GenericEq
            | GenericEqual
            | TypeCheck
            | InstanceOf
    )
}

/// Are all operands of `i` defined outside `lp` (or already hoisted)?
fn operands_invariant(
    f: &Function,
    i: Inst,
    lp: &NaturalLoop,
    inst_block: &[Option<Block>],
    hoisted: &[Inst],
) -> bool {
    for &arg in &f.inst(i).args {
        let def_block = match f.value(arg).def {
            ValueDef::Param { block, .. } => Some(block),
            ValueDef::Result { inst, .. } => {
                if hoisted.contains(&inst) {
                    // An already-hoisted operand will live in the preheader,
                    // which is outside the loop — treat as invariant.
                    continue;
                }
                inst_block.get(inst.index()).copied().flatten()
            }
        };
        match def_block {
            Some(b) if lp.contains(b) => return false, // defined inside the loop → variant
            _ => {} // outside the loop (or unknown def) → invariant
        }
    }
    true
}

/// Return a preheader for `lp.header`: a single block that is the header's only
/// non-back-edge predecessor and jumps unconditionally to it. Reuses an
/// existing clean entry predecessor when possible, otherwise synthesises one
/// and redirects all non-back-edge entry edges through it.
///
/// OSR safety (bliss-enc). An OSR entry block is an alternate CFG entry that
/// jumps straight into the loop
/// header importing the live loop-carried slots. It is a genuine non-back-edge
/// predecessor of the header (the header does not dominate it — it is not even
/// reachable from `f.entry()`), so it lands in `outside` and is redirected
/// *through* the synthesised preheader exactly like the normal entry edge. That
/// is what makes hoisting safe on the OSR path: a value sunk into the preheader
/// is still computed before the header is reached via OSR, never bypassed.
/// INVARIANT (must hold when LICM is eventually wired alongside OSR): the OSR
/// entry region MUST be materialised in the CFG *before* LICM runs. If an OSR
/// edge is added after hoisting, it would jump directly to the header and skip
/// the preheader — reintroducing the pre-header-bypass landmine this ordering
/// avoids. The `hoist_respects_preexisting_osr_entry` test pins this.
fn ensure_preheader(f: &mut Function, dom: &DominatorTree, lp: &NaturalLoop) -> Block {
    let header = lp.header;
    let preds = f.preds(header);
    // Partition predecessors into back-edge sources (dominated by the header)
    // and outside entry edges.
    let outside: Vec<Block> = preds
        .iter()
        .copied()
        .filter(|&p| !dom.dominates(header, p))
        .collect();

    // Reuse: exactly one outside pred that jumps *only* to the header.
    if outside.len() == 1 {
        let p = outside[0];
        if p != header && f.succs(p) == vec![header] {
            return p;
        }
    }

    // Otherwise synthesise a fresh preheader.
    let ph = f.make_block();

    // Mirror the header's parameters so multiple entry edges can be merged and
    // forwarded (preserving SSA). The preheader's params feed the header on the
    // single new edge.
    let header_params: Vec<_> = f.block(header).params.clone();
    let mut fwd_args = Vec::with_capacity(header_params.len());
    for hp in header_params {
        let v = f.value(hp);
        let np = f.add_block_param(ph, v.ty, v.repr);
        fwd_args.push(np);
    }

    // Preheader → header jump, forwarding the merged parameters.
    f.set_terminator(
        ph,
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![BlockCall {
                block: header,
                args: fwd_args,
            }],
            frame_state: None,
            source_pos: 0,
        },
    );

    // Redirect every outside entry edge from the header to the preheader,
    // keeping the arguments it already supplied (they now bind the preheader's
    // params).
    for p in outside {
        if let Some(term) = f.terminator(p) {
            for tgt in &mut f.inst_mut(term).targets {
                if tgt.block == header {
                    tgt.block = ph;
                }
            }
        }
    }

    ph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{IRType, TypeBits, Value, ValueRepresentation};

    fn tagged_fixnum() -> (IRType, ValueRepresentation) {
        (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)
    }

    fn jump(target: Block, args: Vec<Value>) -> InstData {
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![BlockCall {
                block: target,
                args,
            }],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn brif(t0: Block, t1: Block, cond: Value, a0: Vec<Value>, a1: Vec<Value>) -> InstData {
        InstData {
            opcode: Opcode::Brif,
            args: vec![cond],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![
                BlockCall {
                    block: t0,
                    args: a0,
                },
                BlockCall {
                    block: t1,
                    args: a1,
                },
            ],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn ret() -> InstData {
        InstData {
            opcode: Opcode::Return,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    /// Which block currently holds instruction `i`?
    fn block_of(f: &Function, i: Inst) -> Block {
        for &b in f.block_order() {
            if f.block(b).insts.contains(&i) {
                return b;
            }
        }
        panic!("inst not found in any block")
    }

    /// Build a counted loop:
    ///
    /// ```text
    ///   entry:  n = const 100          ; jump header(0)
    ///   header(i): cond = i < n        ; brif body, exit
    ///   body:   inv  = a + b           ; loop-invariant  (hoist me)
    ///           var  = i + inv         ; loop-variant    (pin: uses i)
    ///           _eff = <effectful add> ; effectful       (pin)
    ///           i2   = i + 1           ; jump header(i2)  [back-edge]
    ///   exit:   return
    /// ```
    #[test]
    fn counted_loop_hoists_invariant_only() {
        let mut f = Function::new("counted");
        let (ty, repr) = tagged_fixnum();

        let entry = f.entry();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();

        // Loop induction parameter on the header.
        let i = f.add_block_param(header, ty, repr);

        // entry: two invariant constants + jump header(zero).
        let (_, a_res) = f.push_inst(entry, const_fixnum(3), &[(ty, repr)]);
        let a = a_res[0];
        let (_, b_res) = f.push_inst(entry, const_fixnum(4), &[(ty, repr)]);
        let b = b_res[0];
        let (_, zero_res) = f.push_inst(entry, const_fixnum(0), &[(ty, repr)]);
        let zero = zero_res[0];
        f.set_terminator(entry, jump(header, vec![zero]));

        // header: cond = i < n-ish; use a constant compare operand for simplicity.
        let (_, cond_res) = f.push_inst(
            header,
            InstData {
                opcode: Opcode::FixnumCmpLt,
                args: vec![i, a],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(ty, repr)],
        );
        let cond = cond_res[0];
        f.set_terminator(header, brif(body, exit, cond, vec![], vec![]));

        // body: invariant add (a + b) — both defined in entry, outside loop.
        let (inv_inst, _) = f.push_inst(body, add(a, b), &[(ty, repr)]);
        // variant add (i + inv) — uses the header param i, so loop-variant.
        let (var_inst, var_res) =
            f.push_inst(body, add(i, f.inst(inv_inst).results[0]), &[(ty, repr)]);
        let _ = var_res;
        // effectful add — flags mark a side effect; must NOT hoist even though
        // its operands are invariant.
        let (eff_inst, _) = f.push_inst(
            body,
            InstData {
                opcode: Opcode::FixnumAdd,
                args: vec![a, b],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(ty, repr)],
        );
        // i2 = i + 1 (back-edge value).
        let (_, one_res) = f.push_inst(body, const_fixnum(1), &[(ty, repr)]);
        let one = one_res[0];
        let (inc_inst, inc_res) = f.push_inst(body, add(i, one), &[(ty, repr)]);
        let _ = inc_inst;
        let i2 = inc_res[0];
        f.set_terminator(body, jump(header, vec![i2]));

        f.set_terminator(exit, ret());

        // Record where each instruction lives before LICM.
        let inv_before = block_of(&f, inv_inst);
        let var_before = block_of(&f, var_inst);
        let eff_before = block_of(&f, eff_inst);
        assert_eq!(inv_before, body);
        assert_eq!(var_before, body);
        assert_eq!(eff_before, body);

        // Run the pass.
        let mut a_cache = Analyses::new();
        Licm.run(&mut f, &mut a_cache);

        let inv_after = block_of(&f, inv_inst);
        let var_after = block_of(&f, var_inst);
        let eff_after = block_of(&f, eff_inst);

        // The invariant add MUST have moved out of the body.
        assert_ne!(inv_after, body, "invariant add should have been hoisted");
        // It landed in the preheader (here: reused `entry`, the sole clean pred).
        assert_eq!(
            inv_after, entry,
            "invariant add should live in the preheader"
        );
        // The variant add and the effectful add MUST stay pinned in the body.
        assert_eq!(var_after, body, "loop-variant add must stay in the loop");
        assert_eq!(eff_after, body, "effectful add must stay in the loop");
    }

    /// Header with two outside entry edges forces a *fresh* preheader block to
    /// be synthesised, and the invariant hoists into it.
    #[test]
    fn multi_entry_header_creates_preheader() {
        let mut f = Function::new("multi_entry");
        let (ty, repr) = tagged_fixnum();

        let entry = f.entry();
        let ea = f.make_block();
        let eb = f.make_block();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();

        let i = f.add_block_param(header, ty, repr);

        // entry: const cond, brif to two entry blocks.
        let (_, c_res) = f.push_inst(entry, const_fixnum(1), &[(ty, repr)]);
        let c = c_res[0];
        f.set_terminator(entry, brif(ea, eb, c, vec![], vec![]));

        // Two invariant seeds, one per entry arm.
        let (_, a_res) = f.push_inst(ea, const_fixnum(3), &[(ty, repr)]);
        let a = a_res[0];
        let (_, z0) = f.push_inst(ea, const_fixnum(0), &[(ty, repr)]);
        f.set_terminator(ea, jump(header, vec![z0[0]]));

        let (_, b_res) = f.push_inst(eb, const_fixnum(4), &[(ty, repr)]);
        let b = b_res[0];
        let (_, z1) = f.push_inst(eb, const_fixnum(0), &[(ty, repr)]);
        f.set_terminator(eb, jump(header, vec![z1[0]]));

        // header compare + branch.
        let (_, cond_res) = f.push_inst(
            header,
            InstData {
                opcode: Opcode::FixnumCmpLt,
                args: vec![i, a],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(ty, repr)],
        );
        let cond = cond_res[0];
        f.set_terminator(header, brif(body, exit, cond, vec![], vec![]));

        // body: invariant a + b (both defined in the entry arms, outside loop).
        let (inv_inst, _) = f.push_inst(body, add(a, b), &[(ty, repr)]);
        let (_, one_res) = f.push_inst(body, const_fixnum(1), &[(ty, repr)]);
        let (_, inc_res) = f.push_inst(body, add(i, one_res[0]), &[(ty, repr)]);
        f.set_terminator(body, jump(header, vec![inc_res[0]]));

        f.set_terminator(exit, ret());

        let blocks_before = f.num_blocks();
        assert_eq!(block_of(&f, inv_inst), body);

        let mut a_cache = Analyses::new();
        Licm.run(&mut f, &mut a_cache);

        // A fresh preheader block was created.
        assert_eq!(
            f.num_blocks(),
            blocks_before + 1,
            "a new preheader block should be synthesised"
        );
        let inv_after = block_of(&f, inv_inst);
        assert_ne!(
            inv_after, body,
            "invariant should be hoisted out of the body"
        );

        // The preheader is the new block, is the header's only non-back-edge
        // pred, and jumps to the header.
        let dom = f.dominators();
        let preds = f.preds(header);
        let outside: Vec<_> = preds
            .iter()
            .copied()
            .filter(|&p| !dom.dominates(header, p))
            .collect();
        assert_eq!(
            outside.len(),
            1,
            "header should have a single entry pred now"
        );
        let ph = outside[0];
        assert_eq!(inv_after, ph, "invariant should live in the preheader");
        assert_eq!(f.succs(ph), vec![header]);
        // Both original entry arms now route into the preheader.
        assert!(f.succs(ea).contains(&ph));
        assert!(f.succs(eb).contains(&ph));
    }

    /// A loop with no invariant instructions leaves the CFG untouched.
    #[test]
    fn no_invariant_no_preheader() {
        let mut f = Function::new("noop");
        let (ty, repr) = tagged_fixnum();

        let entry = f.entry();
        let header = f.make_block();
        let exit = f.make_block();
        let i = f.add_block_param(header, ty, repr);

        let (_, z) = f.push_inst(entry, const_fixnum(0), &[(ty, repr)]);
        f.set_terminator(entry, jump(header, vec![z[0]]));

        // Only a variant increment + self-branch; nothing to hoist.
        let (_, one) = f.push_inst(header, const_fixnum(1), &[(ty, repr)]);
        let (_, inc) = f.push_inst(header, add(i, one[0]), &[(ty, repr)]);
        f.set_terminator(header, brif(header, exit, i, vec![inc[0]], vec![]));
        f.set_terminator(exit, ret());

        let blocks_before = f.num_blocks();
        let mut a_cache = Analyses::new();
        Licm.run(&mut f, &mut a_cache);
        assert_eq!(
            f.num_blocks(),
            blocks_before,
            "no preheader when nothing hoists"
        );
    }

    /// OSR pre-header-bypass guardrail (bliss-enc).
    ///
    /// Build a counted loop with a hoistable loop-invariant, then materialise an
    /// OSR entry block (the alternate entry that jumps straight into the header)
    /// *before* running LICM. LICM must treat the OSR edge as a genuine
    /// alternate predecessor and route it *through* the synthesised preheader so
    /// the hoisted invariant is still computed on the OSR path — never bypassed.
    /// An alternate entry block whose parameters mirror `header`'s and whose
    /// terminator jumps into `header` with them: the IR shape an OSR entry
    /// region takes once its imports have been materialised.
    fn build_osr_entry(f: &mut Function, header: Block) -> Block {
        let slots: Vec<_> = f
            .block(header)
            .params
            .iter()
            .map(|&p| (f.value(p).ty, f.value(p).repr))
            .collect();
        let entry_block = f.make_block();
        let args: Vec<Value> = slots
            .into_iter()
            .map(|(ty, repr)| f.add_block_param(entry_block, ty, repr))
            .collect();
        f.set_terminator(entry_block, jump(header, args));
        entry_block
    }

    #[test]
    fn hoist_respects_preexisting_osr_entry() {
        let mut f = Function::new("osr_loop");
        let (ty, repr) = tagged_fixnum();

        let entry = f.entry();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();

        // Loop induction parameter on the header (the single loop-carried slot).
        let i = f.add_block_param(header, ty, repr);

        // entry: two invariant seeds + jump header(0).
        let (_, a_res) = f.push_inst(entry, const_fixnum(3), &[(ty, repr)]);
        let a = a_res[0];
        let (_, b_res) = f.push_inst(entry, const_fixnum(4), &[(ty, repr)]);
        let b = b_res[0];
        let (_, zero_res) = f.push_inst(entry, const_fixnum(0), &[(ty, repr)]);
        f.set_terminator(entry, jump(header, vec![zero_res[0]]));

        // header: cond = i < a; brif body, exit.
        let (_, cond_res) = f.push_inst(
            header,
            InstData {
                opcode: Opcode::FixnumCmpLt,
                args: vec![i, a],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(ty, repr)],
        );
        f.set_terminator(header, brif(body, exit, cond_res[0], vec![], vec![]));

        // body: invariant add (a + b), a loop-variant use of it, and the increment.
        let (inv_inst, inv_res) = f.push_inst(body, add(a, b), &[(ty, repr)]);
        // Use the invariant inside the loop so it is genuinely live on every
        // iteration (including the OSR-entered one).
        let (_var_inst, _) = f.push_inst(body, add(i, inv_res[0]), &[(ty, repr)]);
        let (_, one_res) = f.push_inst(body, const_fixnum(1), &[(ty, repr)]);
        let (_, inc_res) = f.push_inst(body, add(i, one_res[0]), &[(ty, repr)]);
        f.set_terminator(body, jump(header, vec![inc_res[0]]));

        f.set_terminator(exit, ret());

        // Materialise the OSR entry region BEFORE LICM (the required ordering).
        // It becomes an alternate, non-dominated predecessor of the header.
        let osr_entry = build_osr_entry(&mut f, header);
        assert_eq!(
            f.succs(osr_entry),
            vec![header],
            "freshly built OSR entry jumps straight to the header"
        );
        assert_eq!(block_of(&f, inv_inst), body);

        // Run LICM.
        let mut a_cache = Analyses::new();
        Licm.run(&mut f, &mut a_cache);

        // (1) The invariant was hoisted out of the loop body.
        let inv_after = block_of(&f, inv_inst);
        assert_ne!(inv_after, body, "invariant add should have been hoisted");

        // (2) The block it landed in is the header's sole non-back-edge
        //     predecessor (the preheader) and jumps to the header.
        let ph = inv_after;
        let dom = f.dominators();
        let outside: Vec<_> = f
            .preds(header)
            .into_iter()
            .filter(|&p| !dom.dominates(header, p))
            .collect();
        assert_eq!(
            outside,
            vec![ph],
            "the header must have exactly one non-back-edge pred: the preheader"
        );
        assert_eq!(f.succs(ph), vec![header], "preheader jumps to the header");

        // (3) THE GUARDRAIL: the OSR entry edge was redirected THROUGH the
        //     preheader, not left bypassing it into the header. So execution
        //     entering via OSR still computes the hoisted invariant.
        assert_eq!(
            f.succs(osr_entry),
            vec![ph],
            "OSR entry must route through the preheader so the hoisted invariant \
             is not bypassed on the OSR path"
        );
        // The normal entry edge is likewise routed through the preheader.
        assert_eq!(f.succs(entry), vec![ph]);
        // Nothing jumps directly to the header except the preheader and the
        // loop's own back-edge (body) — i.e. no alternate entry bypasses it.
        for &blk in f.block_order() {
            if blk == ph || blk == body {
                continue;
            }
            assert!(
                !f.succs(blk).contains(&header),
                "no block other than the preheader/back-edge may jump straight \
                 into the header (block {blk:?} does)"
            );
        }

        // The OSR import map is untouched: it still describes the one live slot,
        // imported into the OSR block's own param (rerouting only changed the
        // terminator's target, not the imports).
        let _ = b;
        assert_eq!(
            f.block(osr_entry).params.len(),
            1,
            "one loop-carried slot imported"
        );
    }

    // ── small InstData builders ──
    fn const_fixnum(v: i64) -> InstData {
        InstData {
            opcode: Opcode::ConstFixnum,
            args: vec![],
            results: vec![],
            aux: AuxData::FixnumImm(v),
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn add(x: Value, y: Value) -> InstData {
        InstData {
            opcode: Opcode::FixnumAdd,
            args: vec![x, y],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }
}
