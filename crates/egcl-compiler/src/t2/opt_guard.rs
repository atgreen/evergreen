// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Guard elimination and loop-invariant guard hoisting.
//!
//! # Purpose
//!
//! [`GuardElim`] removes deoptimising guards whose check cannot fail or has
//! already been discharged on every path, and moves loop-invariant guards to
//! the loop preheader so they execute once instead of per iteration. It runs
//! third in the production mid-end (fold, GVN, guard elimination, DCE),
//! after the builder's body inlining, so equivalent proofs cloned from
//! separate callees collapse to one dominating guard.
//!
//! # Contract
//!
//! **Input:** a well-formed `Function`. A guard is an instruction with the
//! `guard` flag; it carries a `FrameState`, checks `args[0]`, and may define
//! a refined result that passes the operand through. Its proof is either a
//! type (`AuxData::TypeTag`) or a layout refinement (`AuxData::StringLayout`);
//! the two are distinct `GuardKind`s, because knowing a value is a string
//! does not prove it has a directly addressable simple layout.
//!
//! **Output:** removed guards are unlinked from their block (arena entries
//! stay, unreferenced). Every result a removed guard defined is forwarded to
//! a surviving dominating value, in instruction operands, terminator edge
//! arguments, and FrameState value sources, so no FrameState slot is
//! orphaned. Hoisted guards keep their instruction, results and FrameState;
//! only their block changes. The CFG never changes. `Analyses::invalidate`
//! is called if anything changed.
//!
//! # Transforms, in order
//!
//! 1. **Inference-proven removal.** Run [`infer`](crate::t2::infer::infer)
//!    and delete every `TypeTag(τ)` guard on `v` where the inferred fact for
//!    `v` is not `⊥`, its tag bits are a subset of `τ`'s, and any range or
//!    class refinement `τ` carries is already met (`refinements_covered`).
//!    Results forward to `args[0]`, which dominates the guard and hence every
//!    use of the result. Inference facts are not declared types, so when a
//!    result feeds a block argument whose parameter is declared narrower than
//!    `args[0]`, `args[0]`'s declared type is first narrowed to its inferred
//!    fact (if every edge into it allows that), or the guard is kept.
//! 2. **Dominating-duplicate removal.** Collect every guard with a `TypeTag`
//!    or `StringLayout` proof. A guard is redundant when an equivalent guard
//!    (same opcode, same operands, same kind) precedes it on every path:
//!    earlier in the same block, or in a strictly dominating block. Its
//!    results forward to the leader's corresponding results, so a consumer of
//!    the refined value keeps an explicit proof dependency; a result-less
//!    guard simply disappears.
//! 3. **Loop-invariant hoisting.** Loops are found by a back-edge scan over
//!    the dominator tree (the same scan `opt_licm` does); the body is
//!    the reverse-reachable set from the latches stopping at the header. A
//!    guard moves to the preheader when (a) every operand is defined outside
//!    the body, (b) its block dominates every latch, so it already ran on
//!    every iteration and hoisting adds no deopt the original schedule did not
//!    have, and (c) every value its FrameState names dominates the preheader,
//!    so the FrameState stays valid at the new position. Only an existing
//!    preheader is used (the unique out-of-loop predecessor that jumps solely
//!    to the header); none is synthesised.
//!
//! # Rationale
//!
//! A guard that succeeded dominates the region it protects, so a second check
//! of the same fact in that region cannot fail: speculation is discharged
//! once per dominance region. Forwarding a dominated guard's result to the
//! leader's result rather than to the raw operand matters when the result is
//! a refined SSA identity (as speculation and the inlined `CAR`/`CDR`
//! templates produce), since downstream type facts hang off that identity.
//!
//! # Limits
//!
//! * Transform 1 handles `TypeTag` guards only; layout guards are never
//!   proven by inference.
//! * No guard is hoisted out of a loop that lacks a ready-made preheader;
//!   LICM can synthesise one but is not in the production pipeline.
//! * Duplicate detection is pairwise over all guards (quadratic in guard
//!   count).

use std::collections::{HashMap, HashSet};

use crate::t2::frame_state::ValueSource;
use crate::t2::ir::{AuxData, Block, Function, IRType, Inst, Opcode, TypeBits, Value, ValueDef};
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
        let split_phis = split_loop_entry_guards(f, &dom);
        changed |= !split_phis.is_empty();
        changed |= remove_inference_proven(f);
        changed |= remove_dominating_duplicates(f, &dom);
        changed |= hoist_loop_invariant(f, &dom);

        // A nested (or later) OSR entry can import an outer loop parameter
        // without executing its preheader or header entry check. Establish
        // every refinement introduced above at each direct import. Do this
        // after all guard removal: forwarding a guard result may expose the
        // parameter in a FrameState only during a later transform.
        let fixnum = IRType::of(TypeBits::FIXNUM);
        for osr in &mut f.osr_entries {
            let state = f.frame_states.get(osr.frame_state);
            for &phi in &split_phis {
                let imports_phi = state.scopes.iter().any(|scope| {
                    scope.locals.iter().chain(&scope.stack).any(|source| {
                        matches!(source, ValueSource::Value { value, .. } if *value == phi)
                    })
                });
                if imports_phi && !osr.checks.contains(&(phi, fixnum)) {
                    osr.checks.push((phi, fixnum));
                }
            }
        }

        // Any other guard result an OSR state imports carries a proof made
        // outside the loop, on a path the entry skips: the lower tier's frame
        // may hold a value the guard would have rejected (a T0 activation
        // resumed by an earlier deopt does, bliss-s0v50). Check it at the
        // entry too; a backend that cannot test the type declines the entry.
        // The CFG is unchanged, so `dom` still holds.
        changed |= check_imported_guard_results(f, &dom);

        if changed {
            // Value definitions moved / were forwarded; drop derived analyses.
            a.invalidate();
        }
    }
}

/// Add an entry check for each guard result an OSR entry imports whose guard
/// does not run on the entry's path into the loop. Returns whether any was added.
fn check_imported_guard_results(f: &mut Function, dom: &crate::t2::ir::DominatorTree) -> bool {
    let inst_block = inst_block_map(f);
    let mut added = false;
    for index in 0..f.osr_entries.len() {
        let header = f.osr_entries[index].block;
        let state = f.frame_states.get(f.osr_entries[index].frame_state);
        let imported: Vec<Value> = state.scopes.iter()
            .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
            .filter_map(|source| match source {
                ValueSource::Value { value, .. } => Some(*value),
                _ => None,
            })
            .collect();
        for value in imported {
            let ValueDef::Result { inst, .. } = f.value(value).def else { continue };
            let data = f.inst(inst);
            let (Opcode::Guard, AuxData::TypeTag(ty)) = (data.opcode, &data.aux) else { continue };
            let ty = *ty;
            // A guard the header dominates runs again after the entry.
            if inst_block.get(inst.index()).copied().flatten()
                .is_some_and(|block| dom.dominates(header, block))
            {
                continue;
            }
            let checks = &mut f.osr_entries[index].checks;
            if !checks.iter().any(|&(checked, _)| checked == value) {
                checks.push((value, ty));
                added = true;
            }
        }
    }
    added
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
            // A guard that also converts (an `UnboxFloat` taking a fixnum by
            // contagion, say) cannot be forwarded to its tagged operand even
            // when its check is proven: the result is a different value
            // (bliss-thcbb.1).
            if d.results.iter().any(|&result| f.value(result).repr != f.value(v).repr) {
                continue;
            }
            let got = inf.ty(v);
            if !got.bits.is_bottom()
                && tau.bits.contains(got.bits)
                && refinements_covered(tau, &got)
            {
                to_remove.push(inst);
            }
        }
    }

    // One at a time, so each forwarding check sees the declared types the
    // earlier removals (and refinements) left behind.
    let mut removed = false;
    for inst in to_remove {
        let d = f.inst(inst);
        let Some(&v) = d.args.first() else { continue };
        if forwarding_keeps_declared_types(f, inst, v, &inf.ty(v)) {
            remove_guard(f, inst);
            removed = true;
        }
    }
    removed
}

/// Can `inst`'s results be forwarded to its operand `v` without passing a
/// block argument wider than its parameter's declared type (verify V4)?
///
/// Inference facts live in a side table, so a proven operand's *declared*
/// type may still be wider than the guard result it replaces (bliss-fva34:
/// LOOP's `(if first i (+ i 1))` merge parameter is declared TOP, while the
/// header parameter its guarded result feeds was refined to FIXNUM by loop
/// entry splitting). When an edge would widen, narrow `v`'s declared type to
/// the inferred fact `got` if that is locally checkable; otherwise keep the
/// guard. Forwarding into instruction operands never matters here.
fn forwarding_keeps_declared_types(f: &mut Function, inst: Inst, v: Value, got: &IRType) -> bool {
    let results = f.inst(inst).results.clone();
    let mut need = TypeBits::TOP;
    for &b in f.block_order() {
        for &i in &f.block(b).insts {
            for call in &f.inst(i).targets {
                for (k, a) in call.args.iter().enumerate() {
                    if results.contains(a) {
                        let param = f.block(call.block).params[k];
                        need = need.meet(f.value(param).ty.bits);
                    }
                }
            }
        }
    }
    let have = f.value(v).ty.bits;
    if need.contains(have) {
        return true;
    }
    let narrowed = have.meet(got.bits);
    if narrowed.is_bottom()
        || !need.contains(narrowed)
        || !can_refine_declared(f, v, &results, narrowed)
    {
        return false;
    }
    f.refine_type(v, IRType::of(narrowed));
    true
}

/// May `v`'s declared type be narrowed to `bits` without breaking an edge into
/// it or an OSR import? An instruction result's declared type is local to its
/// definition. A block parameter additionally needs every incoming argument
/// to be declared within `bits`; parameters of the entry block or an OSR
/// header, and any value an OSR state imports, receive values from outside
/// the IR and are left alone. `forwarded` are the guard results about to be
/// replaced by `v`: a state naming one of them imports `v` once they are
/// forwarded, so it counts as importing `v` already.
fn can_refine_declared(f: &Function, v: Value, forwarded: &[Value], bits: TypeBits) -> bool {
    let imported = f.osr_entries.iter().any(|osr| {
        let state = f.frame_states.get(osr.frame_state);
        let names = |src: &ValueSource| {
            matches!(src, ValueSource::Value { value, .. } if *value == v || forwarded.contains(value))
        };
        state
            .scopes
            .iter()
            .any(|s| s.locals.iter().chain(&s.stack).any(names))
            || state.remat.iter().any(|r| r.inputs.iter().any(names))
    });
    if imported {
        return false;
    }
    match f.value(v).def {
        ValueDef::Result { .. } => true,
        ValueDef::Param { block, num } => {
            if block == f.entry() || f.osr_entries.iter().any(|o| o.block == block) {
                return false;
            }
            f.block_order().iter().all(|&b| {
                f.block(b).insts.iter().all(|&i| {
                    f.inst(i)
                        .targets
                        .iter()
                        .filter(|c| c.block == block)
                        .all(|c| {
                            c.args
                                .get(num as usize)
                                .is_some_and(|&a| bits.contains(f.value(a).ty.bits))
                        })
                })
            })
        }
    }
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
            (j != i
                && equivalent(g1, g2)
                && guard_precedes(g1, g2, dom)
                && !osr_bypasses_block(f, g1.block, g2.block))
            .then_some(g1)
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

/// Ordinary-entry dominance does not cover entry into a running loop. A
/// refined guard result is not an interpreter local and cannot be assumed to
/// exist when OSR bypasses its defining instruction. Keep the consumer's own
/// proof unless every OSR path reaching it executes the leader's block first.
/// Same-block instruction order is checked separately by `guard_precedes`.
fn osr_bypasses_block(f: &Function, leader: Block, consumer: Block) -> bool {
    let mut pending: Vec<_> = f.osr_entries.iter().map(|entry| entry.block).collect();
    let mut seen = HashSet::new();
    while let Some(block) = pending.pop() {
        if block == leader || !seen.insert(block) {
            continue;
        }
        if block == consumer {
            return true;
        }
        pending.extend(f.succs(block));
    }
    false
}

// ── Transform 3: loop-invariant guard hoisting ──────────────────────

/// Move loop-invariant guards to their loop's preheader.
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
                // (d) OSR must not skip the relocated proof. The explicit
                // loop-phi split above has entry checks; this generic move does not.
                if osr_bypasses_block(f, ph, b) {
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

// ── Transform 4: loop-entry guard splitting ─────────────────────────

/// A value whose declared type is already confined to FIXNUM.
fn declared_fixnum(f: &Function, v: Value) -> bool {
    let bits = f.value(v).ty.bits;
    !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits
}

/// A `TypeTag(FIXNUM)` guard with no range or class refinement.
fn is_plain_fixnum_guard(d: &crate::t2::ir::InstData) -> bool {
    d.flags.guard
        && matches!(&d.aux, AuxData::TypeTag(tau)
            if tau.bits == TypeBits::FIXNUM && tau.range.is_none() && tau.class_id.is_none())
}

/// Replace the in-loop fixnum guards on a loop-carried header parameter by a
/// single guard on its entry value in the preheader, prove the parameter
/// FIXNUM, and record the proof as an OSR-entry check (bliss-5yz5h).
///
/// For a header `H` with preheader `P` and an OSR entry, a parameter `φ` of
/// `H` qualifies when some plain fixnum guard inside the loop checks `φ`
/// itself and every latch argument for `φ` is already declared FIXNUM (or is
/// `φ`). Then every value `φ` can take is a fixnum provided its entry value
/// is: that value is guarded once in `P` (unless already declared), the
/// preheader edge is rewired to the guard's narrowed result, `φ` is refined to
/// FIXNUM, and the loop's guards on `φ` are removed. Values an OSR entry
/// imports did not come through the preheader, so the entry records
/// `(φ, FIXNUM)` and tests it before entering the loop (emit.rs); a backend
/// that cannot test declines the entry.
///
/// The preheader guard deoptimises with the OSR entry's own state — the
/// interpreter frame at the header — rewritten so each header parameter reads
/// its preheader edge value. That state describes resumption at the header
/// before the first iteration, which is exactly where a failed entry guard
/// leaves the program. Every source of the rewritten state must dominate the
/// preheader, or the parameter is skipped.
/// Return the refined parameters so the caller can propagate their entry
/// checks after all guard-result aliases have been forwarded.
fn split_loop_entry_guards(f: &mut Function, dom: &crate::t2::ir::DominatorTree) -> Vec<Value> {
    use crate::t2::frame_state::ValueSource;
    use crate::t2::ir::{InstData, InstFlags, ValueRepresentation};

    let inst_block = inst_block_map(f);
    let mut latches: HashMap<usize, Vec<Block>> = HashMap::new();
    for &b in f.block_order() {
        for s in f.succs(b) {
            if dom.dominates(s, b) {
                latches.entry(s.index()).or_default().push(b);
            }
        }
    }
    let fixnum = IRType::of(TypeBits::FIXNUM);
    let mut split_phis = Vec::new();

    for (&hidx, tails) in &latches {
        let header = Block(hidx as u32);
        let body = natural_loop_body(f, header, tails);
        let Some(ph) = preheader(f, header, &body) else {
            continue;
        };
        let Some(osr_idx) = f.osr_entries.iter().position(|o| o.block == header) else {
            continue;
        };
        let Some(ph_term) = f.terminator(ph) else {
            continue;
        };
        let Some(edge_idx) = f
            .inst(ph_term)
            .targets
            .iter()
            .position(|t| t.block == header)
        else {
            continue;
        };
        let params = f.block(header).params.clone();
        let ph_args = f.inst(ph_term).targets[edge_idx].args.clone();
        if ph_args.len() != params.len() {
            continue;
        }

        for (k, &phi) in params.iter().enumerate() {
            if declared_fixnum(f, phi) {
                continue;
            }
            // In-loop plain fixnum guards checking φ itself.
            let guards: Vec<Inst> = body
                .iter()
                .flat_map(|&b| f.block(b).insts.iter().copied())
                .filter(|&i| {
                    let d = f.inst(i);
                    is_plain_fixnum_guard(d) && d.args.first() == Some(&phi)
                })
                .collect();
            if guards.is_empty() {
                continue;
            }
            // Every latch must carry a fixnum (or φ itself) back to φ.
            let latches_ok = tails.iter().all(|&t| {
                f.terminator(t).is_some_and(|term| {
                    f.inst(term)
                        .targets
                        .iter()
                        .filter(|c| c.block == header)
                        .all(|c| {
                            c.args
                                .get(k)
                                .is_some_and(|&a| a == phi || declared_fixnum(f, a))
                        })
                })
            });
            if !latches_ok {
                continue;
            }
            let entry_arg = ph_args[k];

            // The preheader guard's deopt state: the OSR header state with
            // every header parameter replaced by its preheader edge value.
            let osr_fs = f.osr_entries[osr_idx].frame_state;
            let mut state = f.frame_states.get(osr_fs).clone();
            let subst = |src: &mut ValueSource| {
                if let ValueSource::Value { value, .. } = src {
                    if let Some(m) = params.iter().position(|p| p == value) {
                        *value = ph_args[m];
                    }
                }
            };
            for scope in &mut state.scopes {
                scope.locals.iter_mut().for_each(subst);
                scope.stack.iter_mut().for_each(subst);
            }
            for recipe in &mut state.remat {
                recipe.inputs.iter_mut().for_each(subst);
            }
            let dominates_ph = |src: &ValueSource| match src {
                ValueSource::Value { value, .. } => {
                    def_block(f, &inst_block, *value).is_some_and(|db| dom.dominates(db, ph))
                }
                _ => true,
            };
            let sources_ok = state
                .scopes
                .iter()
                .all(|s| s.locals.iter().chain(s.stack.iter()).all(dominates_ph))
                && state
                    .remat
                    .iter()
                    .all(|r| r.inputs.iter().all(dominates_ph));
            if !sources_ok {
                continue;
            }

            if !declared_fixnum(f, entry_arg) {
                let fsid = f.frame_states.add(state);
                let source_pos = f.inst(guards[0]).source_pos;
                let (guard, results) = f.push_inst(
                    ph,
                    InstData {
                        opcode: Opcode::Guard,
                        args: vec![entry_arg],
                        results: vec![],
                        aux: AuxData::TypeTag(fixnum),
                        flags: InstFlags {
                            guard: true,
                            effectful: true,
                            ..InstFlags::default()
                        },
                        targets: vec![],
                        frame_state: Some(fsid),
                        source_pos,
                    },
                    &[(fixnum, ValueRepresentation::Tagged)],
                );
                // push_inst appends after the terminator; move it just before.
                let insts = &mut f.block_mut(ph).insts;
                let appended = insts.pop();
                debug_assert_eq!(appended, Some(guard));
                let pos = insts.len().saturating_sub(1);
                insts.insert(pos, guard);
                f.inst_mut(ph_term).targets[edge_idx].args[k] = results[0];
            }
            f.osr_entries[osr_idx].checks.push((phi, fixnum));
            f.refine_type(phi, fixnum);
            for g in guards {
                remove_guard(f, g);
            }
            split_phis.push(phi);
        }
    }
    split_phis
}

// ── Shared mutation helpers ─────────────────────────────────────────

/// Remove a guard instruction, forwarding any result to its checked operand and
/// keeping every FrameState reference valid across all use sites.
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

    fn brif(cond: Value, then: Block, then_args: Vec<Value>, els: Block) -> InstData {
        InstData {
            args: vec![cond],
            targets: vec![
                BlockCall {
                    block: then,
                    args: then_args,
                },
                BlockCall {
                    block: els,
                    args: vec![],
                },
            ],
            ..base(Opcode::Brif)
        }
    }

    /// entry(a) -> preheader -> header(x, i) [Guard x; x' = x + 1; i' = i + 1]
    /// -> body -Brif-> header(x', i') | exit. The header has an OSR entry
    /// whose state names (x, i). Returns (f, entry, preheader, header, a).
    fn loop_with_guarded_phi() -> (Function, Block, Block, Block, Value) {
        let mut f = Function::new("split");
        let entry = f.entry();
        let a = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (_, c0) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(0),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (_, c1) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(1),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (_, t) = f.push_inst(
            entry,
            base(Opcode::ConstT),
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let ph = f.make_block();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        f.set_terminator(entry, jump(ph));
        let x = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        let i = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(
            ph,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![a, c0[0]],
                }],
                ..base(Opcode::Jump)
            },
        );
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 5,
                locals: vec![
                    ValueSource::Value {
                        value: x,
                        repr: ValueRepresentation::Tagged,
                    },
                    ValueSource::Value {
                        value: i,
                        repr: ValueRepresentation::Tagged,
                    },
                ],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(crate::t2::ir::OsrEntry {
            control_scopes: Vec::new(),
            bcp: 5,
            block: header,
            frame_state: state,
            checks: Vec::new(),
        });
        let (_, xg) = f.push_inst(
            header,
            InstData {
                frame_state: Some(state),
                ..guard(x, fixnum())
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let arith = |v: Value| InstData {
            args: vec![v, c1[0]],
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            frame_state: Some(state),
            ..base(Opcode::FixnumAdd)
        };
        let (_, xn) = f.push_inst(
            header,
            arith(xg[0]),
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (_, inn) = f.push_inst(header, arith(i), &[(fixnum(), ValueRepresentation::Tagged)]);
        f.set_terminator(header, jump(body));
        f.set_terminator(body, brif(t[0], header, vec![xn[0], inn[0]], exit));
        f.set_terminator(exit, ret());
        (f, entry, ph, header, a)
    }

    #[test]
    fn loop_phi_guard_splits_to_preheader_and_osr_check() {
        let (mut f, _entry, ph, header, a) = loop_with_guarded_phi();
        assert_eq!(count_guards(&f), 3, "x guard + two checked adds");
        run(&mut f);

        // The header's guard on x is gone; the preheader now guards a.
        let header_guards: Vec<Inst> = f
            .block(header)
            .insts
            .iter()
            .copied()
            .filter(|&i| f.inst(i).opcode == Opcode::Guard)
            .collect();
        assert!(
            header_guards.is_empty(),
            "in-loop guard on the phi must be removed"
        );
        let ph_guard = f
            .block(ph)
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::Guard)
            .expect("preheader guard on the entry value");
        assert_eq!(f.inst(ph_guard).args, vec![a]);
        let narrowed = f.inst(ph_guard).results[0];
        let ph_term = f.terminator(ph).unwrap();
        assert_eq!(
            f.inst(ph_term).targets[0].args[0],
            narrowed,
            "preheader edge must carry the narrowed value"
        );
        let x = f.block(header).params[0];
        assert_eq!(
            f.value(x).ty.bits,
            TypeBits::FIXNUM,
            "phi refined to FIXNUM"
        );
        assert_eq!(
            f.osr_entries[0].checks,
            vec![(x, IRType::of(TypeBits::FIXNUM))],
            "OSR entry must check the imported phi"
        );
        // The preheader guard's state names a (not x) for the first local.
        let fs = f.frame_states.get(f.inst(ph_guard).frame_state.unwrap());
        assert!(matches!(fs.scopes[0].locals[0], ValueSource::Value { value, .. } if value == a));
        assert!(
            crate::t2::verify::verify(&f).is_ok(),
            "{:?}",
            crate::t2::verify::verify(&f)
        );
    }

    /// Both OSR roots can enter the same activation with the outer phi in a
    /// local slot. The inner root bypasses the outer root's entry check.
    #[test]
    fn split_outer_phi_checks_every_nested_osr_import() {
        let mut f = Function::new("nested-osr-split-proof");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (_, one) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(1),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (_, condition) = f.push_inst(
            entry,
            base(Opcode::ConstNil),
            &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
        );
        let outer = f.make_block();
        let inner_preheader = f.make_block();
        let inner = f.make_block();
        let outer_latch = f.make_block();
        let exit = f.make_block();
        let phi = f.add_block_param(outer, IRType::TOP, ValueRepresentation::Tagged);
        let mut enter = jump(outer);
        enter.targets[0].args = vec![input];
        f.set_terminator(entry, enter);
        f.set_terminator(outer, jump(inner_preheader));
        f.set_terminator(inner_preheader, jump(inner));
        for (block, bcp) in [(outer, 5), (inner, 9)] {
            let frame_state = f.frame_states.add(FrameState {
                scopes: vec![FrameScope {
                    function: 0,
                    bcp,
                    locals: vec![ValueSource::Value {
                        value: phi,
                        repr: ValueRepresentation::Tagged,
                    }],
                    stack: vec![],
                }],
                remat: vec![],
            });
            f.osr_entries.push(crate::t2::ir::OsrEntry {
                control_scopes: Vec::new(),
                bcp,
                block,
                frame_state,
                checks: vec![],
            });
        }
        let inner_state = f.osr_entries[1].frame_state;
        let (proof, refined) = f.push_inst(
            inner,
            InstData {
                frame_state: Some(inner_state),
                ..guard(phi, fixnum())
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (add, result) = f.push_inst(
            inner,
            InstData {
                args: vec![refined[0], one[0]],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(inner_state),
                ..base(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(inner, brif(condition[0], inner, vec![], outer_latch));
        f.set_terminator(outer_latch, brif(condition[0], outer, vec![phi], exit));
        f.set_terminator(exit, InstData { args: vec![result[0]], ..ret() });
        crate::t2::verify::verify(&f).expect("valid nested loop before splitting");

        run(&mut f);
        crate::t2::verify::verify(&f).expect("valid nested loop after splitting");
        assert_eq!(f.value(phi).ty.bits, TypeBits::FIXNUM);
        assert!(!f.block(inner).insts.contains(&proof), "keep loop guard splitting");
        assert_eq!(f.inst(add).args[0], phi, "arithmetic now trusts the outer phi");
        for osr in &f.osr_entries {
            let state = f.frame_states.get(osr.frame_state);
            assert!(matches!(state.scopes[0].locals[0],
                ValueSource::Value { value, .. } if value == phi),
                "the value has an import; this is a missing proof, not a missing home");
            assert_eq!(osr.checks, vec![(phi, fixnum())],
                "each direct import must establish the refinement before arithmetic");
        }
    }

    #[test]
    fn unchecked_osr_phi_is_not_inferred_from_its_edges() {
        // Same loop, but without running the split: x's edges are a (TOP)
        // and a fixnum, i's edges are both fixnums. With an OSR entry and no
        // check, inference must not claim i is a fixnum.
        let (f, _entry, _ph, header, _a) = loop_with_guarded_phi();
        let inf = crate::t2::infer::infer(&f);
        let i = f.block(header).params[1];
        assert_eq!(
            inf.ty(i).bits,
            TypeBits::TOP,
            "OSR can import anything into i"
        );
        let mut f = f;
        f.osr_entries[0]
            .checks
            .push((i, IRType::of(TypeBits::FIXNUM)));
        let inf = crate::t2::infer::infer(&f);
        assert_eq!(
            inf.ty(i).bits,
            TypeBits::FIXNUM,
            "a checked import keeps the edge fact"
        );
    }

    /// LOOP's `(setq i (if first i (+ i 1)))` stepping (bliss-fva34):
    ///
    /// ```text
    /// entry(a, first) -> ph -> header(phi) [OSR names phi]
    /// header: brif first -> A | B
    /// A: jump M(phi_in)          phi_in = phi, or a TOP-declared constant
    /// B: g = Guard(phi); x = g + 1; jump M(x)
    /// M(m): r = Guard(m); brif t -> header(r) | exit
    /// ```
    ///
    /// Splitting refines `phi` to FIXNUM, after which inference proves
    /// `Guard(m)`. Forwarding `r` to `m` puts `m` on the back edge into `phi`.
    /// Returns (f, header, merge block (also the latch), merge guard).
    fn loop_first_merge(declared_top_first_arg: bool) -> (Function, Block, Block, Inst) {
        let mut f = Function::new("loop-first-merge");
        let entry = f.entry();
        let a = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let first = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (_, c1) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(1),
                ..base(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        // Inference knows this is a fixnum; its declared type says nothing.
        let (_, c7) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::FixnumImm(7),
                ..base(Opcode::ConstFixnum)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, t) = f.push_inst(
            entry,
            base(Opcode::ConstT),
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let ph = f.make_block();
        let header = f.make_block();
        let then_first = f.make_block();
        let step = f.make_block();
        let merge = f.make_block();
        let exit = f.make_block();
        let phi = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        let m = f.add_block_param(merge, IRType::TOP, ValueRepresentation::Tagged);
        let jump_with = |b: Block, args: Vec<Value>| InstData {
            targets: vec![BlockCall { block: b, args }],
            ..base(Opcode::Jump)
        };
        f.set_terminator(entry, jump(ph));
        f.set_terminator(ph, jump_with(header, vec![a]));
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 5,
                locals: vec![ValueSource::Value {
                    value: phi,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        f.osr_entries.push(crate::t2::ir::OsrEntry {
            control_scopes: Vec::new(),
            bcp: 5,
            block: header,
            frame_state: state,
            checks: Vec::new(),
        });
        f.set_terminator(header, brif(first, then_first, vec![], step));
        let first_arg = if declared_top_first_arg { c7[0] } else { phi };
        f.set_terminator(then_first, jump_with(merge, vec![first_arg]));
        let (_, g) = f.push_inst(
            step,
            InstData {
                frame_state: Some(state),
                ..guard(phi, fixnum())
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        let (_, x) = f.push_inst(
            step,
            InstData {
                args: vec![g[0], c1[0]],
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                frame_state: Some(state),
                ..base(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(step, jump_with(merge, vec![x[0]]));
        let (merge_guard, r) = f.push_inst(
            merge,
            InstData {
                frame_state: Some(state),
                ..guard(m, fixnum())
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(merge, brif(t[0], header, vec![r[0]], exit));
        f.set_terminator(exit, ret());
        crate::t2::verify::verify(&f).expect("valid loop before guard elimination");
        (f, header, merge, merge_guard)
    }

    #[test]
    fn proven_merge_guard_narrows_merge_param_for_back_edge() {
        let (mut f, header, merge, merge_guard) = loop_first_merge(false);
        run(&mut f);
        if let Err(e) = crate::t2::verify::verify(&f) {
            panic!("guard elimination must keep declared edge types: {e:?}");
        }
        let phi = f.block(header).params[0];
        let m = f.block(merge).params[0];
        assert_eq!(f.value(phi).ty.bits, TypeBits::FIXNUM, "split refines phi");
        assert_eq!(f.value(m).ty.bits, TypeBits::FIXNUM, "merge param narrowed");
        assert!(
            !f.block(merge).insts.contains(&merge_guard),
            "the proven merge guard is removed"
        );
        let back = f.inst(f.terminator(merge).unwrap()).targets[0].args[0];
        assert_eq!(back, m, "back edge carries the merge param");
        assert!(f.osr_entries[0].checks.contains(&(phi, fixnum())));
    }

    #[test]
    fn proven_merge_guard_stays_when_an_edge_is_declared_wider() {
        // The constant is inferred FIXNUM but declared TOP, so the merge param
        // cannot be narrowed; removing its guard would widen the back edge.
        let (mut f, header, merge, merge_guard) = loop_first_merge(true);
        run(&mut f);
        if let Err(e) = crate::t2::verify::verify(&f) {
            panic!("guard elimination must keep declared edge types: {e:?}");
        }
        let m = f.block(merge).params[0];
        assert_eq!(f.value(m).ty.bits, TypeBits::TOP);
        assert!(
            f.block(merge).insts.contains(&merge_guard),
            "the guard that keeps the back edge FIXNUM survives"
        );
        assert_eq!(f.value(f.block(header).params[0]).ty.bits, TypeBits::FIXNUM);
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
    fn proven_contagion_unbox_is_a_conversion_not_a_redundant_guard() {
        // `(* i 1d-3)` with `i` a declared fixnum (bliss-thcbb.1): the
        // fixnum-accepting UnboxFloat's TypeTag covers `i`, but its raw
        // result is the converted double, so it must not forward to `i`.
        let mut f = Function::new("contagion");
        let e = f.entry();
        let i = f.add_block_param(e, fixnum(), ValueRepresentation::Tagged);
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope { function: 0, bcp: 3, locals: vec![], stack: vec![] }],
            remat: vec![],
        });
        let (unbox, raw) = f.push_inst(
            e,
            InstData {
                args: vec![i],
                aux: crate::t2::speculate::contagion_unbox_aux(),
                flags: InstFlags { guard: true, effectful: true, ..InstFlags::default() },
                frame_state: Some(state),
                ..base(Opcode::UnboxFloat)
            },
            &[(IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64)],
        );
        let (mul, _) = f.push_inst(
            e,
            InstData { args: vec![raw[0], raw[0]], ..base(Opcode::FloatMul) },
            &[(IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64)],
        );
        f.set_terminator(e, ret());

        run(&mut f);
        assert!(f.block(e).insts.contains(&unbox), "the conversion stays");
        assert_eq!(f.inst(mul).args, vec![raw[0], raw[0]], "operands stay raw");
    }

    // spec-covers: R4.63
    // "Per-operation re-guarding of an already-narrowed value is a defect."
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

    // spec-covers: R4.63
    // "exactly one guard at the earliest point that dominates all speculative
    // uses (typically a loop preheader)" — the guard moves to the preheader and
    // count_guards stays 1, so hoisting neither duplicates nor drops it.
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
    /// Model CAR before a loop and repeated CDR/CAR inside it. The interpreter
    /// header has MARKER, but no slot for a pre-loop guard's refined alias.
    fn cons_loop(preloop_guard: bool, osr: bool) -> (Function, Block, Block, Inst, Inst, Inst) {
        let mut f = Function::new("cons-osr-proof");
        let entry = f.entry();
        let marker = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let header = f.make_block();
        let exit = f.make_block();
        let state = |bcp| FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp,
                locals: vec![ValueSource::Value {
                    value: marker,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let entry_state = f.frame_states.add(state(0));
        let header_state = f.frame_states.add(state(7));
        let cons = IRType::of(TypeBits::CONS);
        let checked = |fs| InstData {
            frame_state: Some(fs),
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            ..guard(marker, cons)
        };
        if preloop_guard {
            let (_, refined) = f.push_inst(
                entry,
                checked(entry_state),
                &[(cons, ValueRepresentation::Tagged)],
            );
            f.push_inst(
                entry,
                InstData {
                    args: vec![refined[0]],
                    ..base(Opcode::Car)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
        }
        f.set_terminator(entry, jump(header));
        if osr {
            f.osr_entries.push(crate::t2::ir::OsrEntry {
                control_scopes: Vec::new(),
                bcp: 7,
                block: header,
                frame_state: header_state,
                checks: vec![],
            });
        }
        let (loop_guard, refined) = f.push_inst(
            header,
            checked(header_state),
            &[(cons, ValueRepresentation::Tagged)],
        );
        let (cdr, tail) = f.push_inst(
            header,
            InstData {
                args: vec![refined[0]],
                ..base(Opcode::Cdr)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, duplicate) = f.push_inst(
            header,
            checked(header_state),
            &[(cons, ValueRepresentation::Tagged)],
        );
        let (car, head) = f.push_inst(
            header,
            InstData {
                args: vec![duplicate[0]],
                ..base(Opcode::Car)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(header, brif(tail[0], header, vec![], exit));
        f.set_terminator(
            exit,
            InstData {
                args: vec![head[0]],
                ..ret()
            },
        );
        (f, entry, header, loop_guard, cdr, car)
    }

    #[test]
    fn duplicate_cons_guard_respects_osr_entry_and_keeps_local_elimination() {
        for osr in [true, false] {
            let (mut f, entry, header, loop_guard, cdr, car) = cons_loop(true, osr);
            crate::t2::verify::verify(&f).unwrap();
            run(&mut f);
            crate::t2::verify::verify(&f).unwrap();
            let refined = f.inst(cdr).args[0];
            assert_eq!(
                f.inst(car).args[0],
                refined,
                "same-block duplicate still eliminated"
            );
            let ValueDef::Result { inst: leader, .. } = f.value(refined).def else {
                panic!("field load must retain an explicit guard proof");
            };
            if osr {
                assert_eq!(leader, loop_guard, "OSR bypasses the pre-loop CAR guard");
                assert!(f.block(header).insts.contains(&leader));
                assert_eq!(count_guards(&f), 2);
            } else {
                assert!(f.block(entry).insts.contains(&leader));
                assert_eq!(
                    count_guards(&f),
                    1,
                    "ordinary dominance still removes loop guard"
                );
            }
        }
    }

    #[test]
    fn invariant_cons_guard_stays_on_osr_path_but_hoists_without_osr() {
        for osr in [true, false] {
            let (mut f, entry, header, loop_guard, cdr, _) = cons_loop(false, osr);
            crate::t2::verify::verify(&f).unwrap();
            run(&mut f);
            crate::t2::verify::verify(&f).unwrap();
            assert_eq!(count_guards(&f), 1, "redundant in-loop proof still removed");
            assert_eq!(f.inst(cdr).args[0], f.inst(loop_guard).results[0]);
            let expected_block = if osr { header } else { entry };
            assert!(
                f.block(expected_block).insts.contains(&loop_guard),
                "every entry must execute the invariant guard before using its result"
            );
        }
    }
}
