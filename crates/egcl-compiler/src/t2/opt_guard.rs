// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! P4d — Guard elimination & hoisting (spec §4.5 R4.37, §4.3.6.3, §4.10 R4.63).
//!
//! **Parcel P4d. Owner: (sub-agent).** Remove redundant guards and hoist
//! loop-invariant ones. Three transforms, in order:
//!
//!  1. **Inference-proven removal.** A guard-flagged instruction that checks a
//!     value `v` against a type `τ` (aux `TypeTag(τ)`, `args[0] = v`) is dead if
//!     P3's inference (`crate::t2::infer::infer`) already proves `v`'s tag bits
//!     are a subtype of `τ` (`τ.bits ⊇ ty(v).bits`) — and, conservatively, `τ`'s
//!     range/class refinements are covered too. The guard can never fire.
//!
//!  2. **Dominating-duplicate removal.** A guard is dead if an *equivalent* guard
//!     (same opcode, checked operands, and proof kind) dominates it (strict block
//!     dominance, or an earlier position in the same block). Proof kinds include
//!     both type tags and representation refinements such as `StringLayout`.
//!     The dominating guard already discharged the speculation for the whole
//!     dominated region (spec R4.63), so the second check is redundant. Refined
//!     SSA results are forwarded to the dominating guard's corresponding result.
//!
//!  3. **Loop-invariant hoisting.** A guard inside a natural loop whose checked
//!     operands are all defined outside the loop, that dominates every loop
//!     latch (so it runs each iteration), and whose `FrameState` names only
//!     values that dominate the preheader, is moved to the preheader. Its
//!     `FrameState` stays valid because every value it references still
//!     dominates the new location (spec R4.63). Loops are found here with a
//!     local back-edge scan over the dominator tree (the shared `LoopForest` is
//!     not yet populated).
//!
//! **Deopt-preservation (spec §4.10 R4.60).** When a guard is *removed*, any
//! result value it defined is forwarded to its checked operand `args[0]` — a
//! value that dominates the guard and hence every use of the result — across
//! instruction operands, terminator edge arguments, *and* `FrameState` value
//! sources. No `FrameState` of any surviving instruction is ever orphaned.

use std::collections::{HashMap, HashSet};

use crate::t2::frame_state::ValueSource;
use crate::t2::ir::{AuxData, Block, Function, IRType, Inst, Opcode, Value, ValueDef};
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct GuardElim;

impl Pass for GuardElim {
    fn name(&self) -> &'static str {
        "guard-elim"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        // CFG never changes in this pass (we only remove/move instructions
        // *within* blocks), so one dominator tree serves all three transforms.
        let dom = a.dominators(f).clone();

        let mut changed = false;
        changed |= remove_inference_proven(f);
        changed |= remove_dominating_duplicates(f, &dom);
        changed |= hoist_loop_invariant(f, &dom);

        if changed {
            // Value definitions moved / were forwarded; drop derived analyses.
            a.invalidate();
        }
    }
}

// ── Transform 1: inference-proven guards ────────────────────────────

/// Remove every guard whose checked type is already implied by inference.
fn remove_inference_proven(f: &mut Function) -> bool {
    let inf = crate::t2::infer::infer(f);
    let mut to_remove: Vec<Inst> = Vec::new();

    for b in f.block_order().to_vec() {
        for &inst in &f.block(b).insts {
            let d = f.inst(inst);
            if !d.flags.guard {
                continue;
            }
            let AuxData::TypeTag(tau) = &d.aux else {
                continue;
            };
            let Some(&v) = d.args.first() else { continue };
            let got = inf.ty(v);
            if !got.bits.is_bottom()
                && tau.bits.contains(got.bits)
                && refinements_covered(tau, &got)
            {
                to_remove.push(inst);
            }
        }
    }

    let n = to_remove.len();
    for inst in to_remove {
        remove_guard(f, inst);
    }
    n > 0
}

/// Is `τ`'s range/class refinement already satisfied by the inferred fact? Type
/// bits are checked by the caller; here we conservatively demand that any
/// *narrowing* carried by `τ` (a bounded range, a concrete class) is met by the
/// inferred value, so we never drop a guard that is actually still discriminating.
fn refinements_covered(tau: &IRType, got: &IRType) -> bool {
    if let Some(tr) = tau.range {
        match got.range {
            Some(gr) => {
                if gr.lo < tr.lo || gr.hi > tr.hi {
                    return false;
                }
            }
            None => return false,
        }
    }
    if let Some(tc) = tau.class_id {
        if got.class_id != Some(tc) {
            return false;
        }
    }
    true
}

// ── Transform 2: dominating duplicate guards ────────────────────────

/// A guard's proof identity for redundancy matching.  Layout refinements are
/// deliberately distinct from `TypeTag(STRING)`: knowing that a value is a CL
/// string does not prove it has one of the directly addressable simple layouts.
#[derive(Copy, Clone, PartialEq, Debug)]
enum GuardKind {
    Type(IRType),
    StringLayout,
}

/// A guard's identity for redundancy matching.
struct GuardInfo {
    inst: Inst,
    block: Block,
    /// Position within `block.insts` (for same-block ordering).
    pos: usize,
    opcode: Opcode,
    args: Vec<Value>,
    kind: GuardKind,
}

/// Remove any guard that an equivalent guard already dominates.
fn remove_dominating_duplicates(f: &mut Function, dom: &crate::t2::ir::DominatorTree) -> bool {
    let guards = collect_guards(f);
    let mut to_remove: Vec<(Inst, Inst)> = Vec::new();

    for (i, g2) in guards.iter().enumerate() {
        // Redundant iff some equivalent guard runs strictly before it on every
        // path. Keep the leader so a refining guard's result can replace the
        // dominated result, preserving the explicit proof dependency.
        if let Some(g1) = guards.iter().enumerate().find_map(|(j, g1)| {
            (j != i && equivalent(g1, g2) && guard_precedes(g1, g2, dom)).then_some(g1)
        }) {
            to_remove.push((g2.inst, g1.inst));
        }
    }

    let n = to_remove.len();
    for (inst, leader) in to_remove {
        remove_redundant_guard(f, inst, leader);
    }
    n > 0
}

/// Gather every guard kind whose proof is reusable in program order.
fn collect_guards(f: &Function) -> Vec<GuardInfo> {
    let mut out = Vec::new();
    for b in f.block_order().to_vec() {
        for (pos, &inst) in f.block(b).insts.iter().enumerate() {
            let d = f.inst(inst);
            if !d.flags.guard {
                continue;
            }
            let kind = match &d.aux {
                AuxData::TypeTag(tag) => GuardKind::Type(*tag),
                AuxData::StringLayout => GuardKind::StringLayout,
                _ => continue,
            };
            out.push(GuardInfo {
                inst,
                block: b,
                pos,
                opcode: d.opcode,
                args: d.args.clone(),
                kind,
            });
        }
    }
    out
}

/// Two guards check the same thing.
fn equivalent(a: &GuardInfo, b: &GuardInfo) -> bool {
    a.opcode == b.opcode && a.args == b.args && a.kind == b.kind
}

/// Does `g1` provably execute before `g2` on every path that reaches `g2`?
fn guard_precedes(g1: &GuardInfo, g2: &GuardInfo, dom: &crate::t2::ir::DominatorTree) -> bool {
    if g1.block == g2.block {
        g1.pos < g2.pos
    } else {
        dom.dominates(g1.block, g2.block)
    }
}

// ── Transform 3: loop-invariant guard hoisting ──────────────────────

/// Move loop-invariant guards to their loop's preheader (spec R4.63).
fn hoist_loop_invariant(f: &mut Function, dom: &crate::t2::ir::DominatorTree) -> bool {
    let inst_block = inst_block_map(f);

    // Back-edge scan: b→h is a back edge iff h dominates b; h is then a header.
    let mut latches: HashMap<usize, Vec<Block>> = HashMap::new();
    for &b in f.block_order() {
        for s in f.succs(b) {
            if dom.dominates(s, b) {
                latches.entry(s.index()).or_default().push(b);
            }
        }
    }

    // (guard inst, from block, preheader) moves to apply after analysis.
    let mut moves: Vec<(Inst, Block, Block)> = Vec::new();

    for (&hidx, tails) in &latches {
        let header = Block(hidx as u32);
        let body = natural_loop_body(f, header, tails);
        let Some(ph) = preheader(f, header, &body) else {
            continue;
        };

        for &b in &body {
            for &inst in &f.block(b).insts {
                let d = f.inst(inst);
                if !d.flags.guard {
                    continue;
                }
                // (a) invariant: every operand defined outside the loop body.
                let invariant = d
                    .args
                    .iter()
                    .all(|&v| def_block(f, &inst_block, v).is_none_or(|db| !body.contains(&db)));
                if !invariant {
                    continue;
                }
                // (b) runs each iteration: dominates every latch (so hoisting
                //     never *adds* a deopt relative to the original schedule).
                if !tails.iter().all(|&t| dom.dominates(b, t)) {
                    continue;
                }
                // (c) FrameState stays valid: every value it names must dominate
                //     the preheader (spec R4.63 — no source orphaned by the move).
                if !frame_state_dominates(f, &inst_block, inst, ph, dom) {
                    continue;
                }
                moves.push((inst, b, ph));
            }
        }
    }

    let changed = !moves.is_empty();
    for (inst, from, ph) in moves {
        move_inst_before_terminator(f, inst, from, ph);
    }
    changed
}

/// Blocks of the natural loop for `header`, given its back-edge tails.
fn natural_loop_body(f: &Function, header: Block, tails: &[Block]) -> HashSet<Block> {
    let mut body: HashSet<Block> = HashSet::new();
    body.insert(header);
    let mut stack: Vec<Block> = Vec::new();
    for &t in tails {
        if body.insert(t) {
            stack.push(t);
        }
    }
    // Reverse-reachability from the latches, stopping at the header (already in
    // `body`, so its predecessors are never expanded).
    while let Some(x) = stack.pop() {
        for p in f.preds(x) {
            if body.insert(p) {
                stack.push(p);
            }
        }
    }
    body
}

/// The unique out-of-loop predecessor of `header`, if it jumps solely to it.
fn preheader(f: &Function, header: Block, body: &HashSet<Block>) -> Option<Block> {
    let outside: Vec<Block> = f
        .preds(header)
        .into_iter()
        .filter(|p| !body.contains(p))
        .collect();
    if outside.len() != 1 {
        return None;
    }
    let ph = outside[0];
    if f.succs(ph) == vec![header] {
        Some(ph)
    } else {
        None
    }
}

/// Do all values named by `inst`'s FrameState dominate `ph`?
fn frame_state_dominates(
    f: &Function,
    inst_block: &[Option<Block>],
    inst: Inst,
    ph: Block,
    dom: &crate::t2::ir::DominatorTree,
) -> bool {
    let Some(fsid) = f.inst(inst).frame_state else {
        return true;
    };
    let fs = f.frame_states.get(fsid);
    let mut ok = true;
    let mut check = |vs: &ValueSource| {
        if let ValueSource::Value { value, .. } = vs {
            match def_block(f, inst_block, *value) {
                Some(db) if dom.dominates(db, ph) => {}
                _ => ok = false,
            }
        }
    };
    for scope in &fs.scopes {
        scope.locals.iter().for_each(&mut check);
        scope.stack.iter().for_each(&mut check);
    }
    for recipe in &fs.remat {
        recipe.inputs.iter().for_each(&mut check);
    }
    ok
}

// ── Shared mutation helpers ─────────────────────────────────────────

/// Remove a guard instruction, forwarding any result to its checked operand and
/// preserving the deopt invariant (spec R4.60) across all use sites.
fn remove_guard(f: &mut Function, inst: Inst) {
    let (operand, results) = {
        let d = f.inst(inst);
        (d.args.first().copied(), d.results.clone())
    };
    if let Some(v) = operand {
        for r in results {
            replace_all_uses(f, r, v);
        }
    }
    detach_inst(f, inst);
}

/// Remove a guard made redundant by `leader`.  A guard that yields a refined
/// SSA identity must forward uses to the leader's refined result, not back to
/// the unchecked input; result-less type guards retain the old no-op behavior.
fn remove_redundant_guard(f: &mut Function, inst: Inst, leader: Inst) {
    let results = f.inst(inst).results.clone();
    let leader_results = f.inst(leader).results.clone();
    let operand = f.inst(inst).args.first().copied();
    for (index, result) in results.into_iter().enumerate() {
        if let Some(replacement) = leader_results.get(index).copied().or(operand) {
            replace_all_uses(f, result, replacement);
        }
    }
    detach_inst(f, inst);
}

/// Move `inst` out of `from` and into `into`, just before `into`'s terminator.
/// The instruction (and its result values / FrameState) are untouched.
fn move_inst_before_terminator(f: &mut Function, inst: Inst, from: Block, into: Block) {
    f.block_mut(from).insts.retain(|&i| i != inst);
    let insts = &mut f.block_mut(into).insts;
    let pos = insts.len().saturating_sub(1); // before the terminator
    insts.insert(pos, inst);
}

/// Unlink `inst` from its block (the arena slot is left dead).
fn detach_inst(f: &mut Function, inst: Inst) {
    for b in f.block_order().to_vec() {
        let insts = &mut f.block_mut(b).insts;
        if insts.contains(&inst) {
            insts.retain(|&i| i != inst);
            return;
        }
    }
}

/// Replace every use of `old` with `new` — in instruction operands, terminator
/// edge arguments, and FrameState value sources.
fn replace_all_uses(f: &mut Function, old: Value, new: Value) {
    for i in 0..f.num_insts() {
        let d = f.inst_mut(Inst(i as u32));
        for a in &mut d.args {
            if *a == old {
                *a = new;
            }
        }
        for call in &mut d.targets {
            for a in &mut call.args {
                if *a == old {
                    *a = new;
                }
            }
        }
    }
    for k in 0..f.frame_states.len() {
        let fs = f
            .frame_states
            .get_mut(crate::t2::frame_state::FrameStateId(k as u32));
        let repl = |vs: &mut ValueSource| {
            if let ValueSource::Value { value, .. } = vs {
                if *value == old {
                    *value = new;
                }
            }
        };
        for scope in &mut fs.scopes {
            scope.locals.iter_mut().for_each(repl);
            scope.stack.iter_mut().for_each(repl);
        }
        for recipe in &mut fs.remat {
            recipe.inputs.iter_mut().for_each(repl);
        }
    }
}

/// Map each instruction to the block that currently lists it.
fn inst_block_map(f: &Function) -> Vec<Option<Block>> {
    let mut map = vec![None; f.num_insts()];
    for &b in f.block_order() {
        for &inst in &f.block(b).insts {
            if inst.index() < map.len() {
                map[inst.index()] = Some(b);
            }
        }
    }
    map
}

/// The block defining `v` (`Param` block, or the block listing its `Result`
/// instruction). `None` if the defining instruction is no longer listed.
fn def_block(f: &Function, inst_block: &[Option<Block>], v: Value) -> Option<Block> {
    match f.value(v).def {
        ValueDef::Param { block, .. } => Some(block),
        ValueDef::Result { inst, .. } => inst_block.get(inst.index()).copied().flatten(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{BlockCall, IRType, InstData, InstFlags, TypeBits, ValueRepresentation};

    fn base(op: Opcode) -> InstData {
        InstData {
            opcode: op,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn fixnum() -> IRType {
        IRType::of(TypeBits::FIXNUM)
    }

    fn ret() -> InstData {
        base(Opcode::Return)
    }

    fn jump(b: Block) -> InstData {
        InstData {
            targets: vec![BlockCall {
                block: b,
                args: vec![],
            }],
            ..base(Opcode::Jump)
        }
    }

    /// A guard on `v` requiring type `tag`.
    fn guard(v: Value, tag: IRType) -> InstData {
        InstData {
            args: vec![v],
            aux: AuxData::TypeTag(tag),
            flags: InstFlags {
                guard: true,
                ..InstFlags::default()
            },
            ..base(Opcode::Guard)
        }
    }

    /// Count guard-flagged instructions still listed in any block.
    fn count_guards(f: &Function) -> usize {
        f.block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.clone())
            .filter(|&i| f.inst(i).flags.guard)
            .count()
    }

    fn run(f: &mut Function) {
        let mut a = Analyses::new();
        GuardElim.run(f, &mut a);
    }

    #[test]
    fn inference_proven_guard_is_removed() {
        // c = ConstFixnum 42 ; Guard(c : FIXNUM) — inference proves c is a
        // FIXNUM, so the guard can never fire.
        let mut f = Function::new("proven");
        let e = f.entry();
        let (_, c) = f.push_inst(
            e,
            InstData {
                aux: AuxData::FixnumImm(42),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.push_inst(e, guard(c[0], fixnum()), &[]);
        f.set_terminator(e, ret());

        assert_eq!(count_guards(&f), 1);
        run(&mut f);
        assert_eq!(
            count_guards(&f),
            0,
            "inference-proven guard must be removed"
        );
    }

    #[test]
    fn needed_guard_survives() {
        // p : TOP with no dominating guard and no proof — the guard is real.
        let mut f = Function::new("needed");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        f.push_inst(e, guard(p, fixnum()), &[]);
        f.set_terminator(e, ret());

        run(&mut f);
        assert_eq!(count_guards(&f), 1, "an unproven guard must survive");
    }

    #[test]
    fn dominating_duplicate_guard_is_removed() {
        // entry: Guard(p : FIXNUM) ; jump B.   B: Guard(p : FIXNUM) ; ret.
        // p is a TOP param, so inference alone (global facts stay TOP) does NOT
        // prove either guard — only the dominance of the first over the second
        // makes the second redundant.
        let mut f = Function::new("dup");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        f.push_inst(e, guard(p, fixnum()), &[]);
        let b = f.make_block();
        f.set_terminator(e, jump(b));
        f.push_inst(b, guard(p, fixnum()), &[]);
        f.set_terminator(b, ret());

        assert_eq!(count_guards(&f), 2);
        run(&mut f);
        assert_eq!(
            count_guards(&f),
            1,
            "the dominated duplicate must be removed"
        );
        // The surviving guard is the dominating one, in the entry block.
        let survivor_in_entry = f.block(e).insts.iter().any(|&i| f.inst(i).flags.guard);
        assert!(
            survivor_in_entry,
            "the dominating guard must be the survivor"
        );
    }

    #[test]
    fn dominating_layout_guard_forwards_refined_result() {
        // Shape produced when two independently guarded string bodies are
        // inlined into one caller: both guards check the same caller SSA value,
        // while each operation consumes its own guard's refined result.
        let mut f = Function::new("inlined-layout-guards");
        let e = f.entry();
        let string = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let layout = IRType::of(TypeBits::STRING);
        let make_layout_guard = |arg| InstData {
            args: vec![arg],
            aux: AuxData::StringLayout,
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            ..base(Opcode::Guard)
        };
        let (_, first) = f.push_inst(
            e,
            make_layout_guard(string),
            &[(layout, ValueRepresentation::Tagged)],
        );
        let (_, second) = f.push_inst(
            e,
            make_layout_guard(string),
            &[(layout, ValueRepresentation::Tagged)],
        );
        let (load, _) = f.push_inst(
            e,
            InstData {
                args: vec![second[0]],
                ..base(Opcode::StringByteLength)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret());

        run(&mut f);

        assert_eq!(count_guards(&f), 1, "one dominating layout proof remains");
        assert_eq!(
            f.inst(load).args,
            vec![first[0]],
            "the consumer retains an SSA dependency on the surviving proof"
        );
    }

    #[test]
    fn guard_result_is_forwarded_when_removed() {
        // c = ConstFixnum 7 ; r = <guard TypeCheck(c : FIXNUM)> ; x = r + c.
        // Removing the guard must forward its result r to c so `x` still refers
        // to a live value.
        let mut f = Function::new("forward");
        let e = f.entry();
        let (_, c) = f.push_inst(
            e,
            InstData {
                aux: AuxData::FixnumImm(7),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let (_, r) = f.push_inst(
            e,
            InstData {
                args: vec![c[0]],
                aux: AuxData::TypeTag(fixnum()),
                flags: InstFlags {
                    guard: true,
                    ..InstFlags::default()
                },
                ..base(Opcode::TypeCheck)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let (add, _x) = f.push_inst(
            e,
            InstData {
                args: vec![r[0], c[0]],
                ..base(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(e, ret());

        run(&mut f);
        assert_eq!(count_guards(&f), 0, "the proven TypeCheck guard is removed");
        // The add's first operand must have been rewritten from r to c.
        assert_eq!(
            f.inst(add).args,
            vec![c[0], c[0]],
            "result forwarded to operand"
        );
    }

    #[test]
    fn loop_invariant_guard_is_hoisted() {
        // entry(v:TOP) -> header ; header: Guard(v:FIXNUM) ; brif -> body, exit ;
        // body: jump header (back edge) ; exit: ret.
        // v is defined in the (pre)header entry, so the guard is loop-invariant
        // and dominates the sole latch — it hoists to the preheader.
        let mut f = Function::new("hoist");
        let e = f.entry();
        let v = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();

        f.set_terminator(e, jump(header));

        // A FrameState naming only v (defined in the preheader) — stays valid.
        let fsid = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: v,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        let (gi, _) = f.push_inst(
            header,
            InstData {
                frame_state: Some(fsid),
                ..guard(v, fixnum())
            },
            &[],
        );

        // A trivial loop condition so the header is a real branch.
        let (_, cond) = f.push_inst(
            header,
            InstData {
                args: vec![v],
                aux: AuxData::TypeTag(fixnum()),
                ..base(Opcode::InstanceOf)
            },
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
        );
        f.set_terminator(
            header,
            InstData {
                args: vec![cond[0]],
                targets: vec![
                    BlockCall {
                        block: body,
                        args: vec![],
                    },
                    BlockCall {
                        block: exit,
                        args: vec![],
                    },
                ],
                ..base(Opcode::Brif)
            },
        );
        f.set_terminator(body, jump(header));
        f.set_terminator(exit, ret());

        // Before: the guard lives in the header (the loop body).
        assert!(f.block(header).insts.contains(&gi));
        run(&mut f);
        // After: it has moved into the preheader (entry) and left the header.
        assert!(
            f.block(e).insts.contains(&gi),
            "guard must be hoisted to preheader"
        );
        assert!(
            !f.block(header).insts.contains(&gi),
            "guard must leave the loop body"
        );
        assert_eq!(count_guards(&f), 1, "hoisting preserves the guard");
    }
}
