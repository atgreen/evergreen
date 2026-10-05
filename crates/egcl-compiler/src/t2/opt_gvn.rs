// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Global value numbering (dominance-based common-subexpression elimination).
//!
//! # Purpose
//!
//! [`Gvn`] replaces a pure instruction with an earlier, dominating instruction
//! that computes the same value, and reroutes every use, including deopt
//! metadata, to the survivor. It runs second in the production mid-end
//! (fold, GVN, guard elimination, DCE). It does not delete anything: the
//! redundant instructions are left as dead definitions for DCE.
//!
//! # Contract
//!
//! **Input:** a well-formed `Function` in block-parameter SSA.
//!
//! **Participation:** an instruction is value-numbered only if it produces a
//! result and has none of the `effectful`, `guard`, `call` flags and is not a
//! terminator. Two instructions are *congruent* when they have the same
//! opcode, the same `aux` payload, and the same operand value-numbers after
//! resolving each operand through the running replacement map (so
//! congruence is transitive across earlier replacements). For a
//! `commutative` opcode the operand list is sorted first, so `a+b` and `b+a`
//! collapse. `AuxData` is neither `Hash` nor `Eq`, so its `Debug` rendering,
//! which is structural and stable, stands in for it in the key.
//!
//! **Output:** every use of a replaced value, in instruction operands,
//! terminator edge arguments, and `FrameState` value sources (locals, stack,
//! remat-recipe inputs), now names the representative. A value named by a
//! FrameState is deopt-live; rerouting it to a dominating equivalent keeps
//! the slot valid, deleting it would orphan the slot. `Analyses::invalidate`
//! is called if any replacement was made.
//!
//! # Algorithm
//!
//! 1. **Number.** One reverse-postorder scan, instructions in program order.
//!    A `leaders` table maps each congruence key to the representative
//!    instruction seen so far and its block. Because of the visit order the
//!    representative is always an earlier definition. When a congruent
//!    instruction's block is dominated by the representative's block
//!    (reflexively, so an earlier definition in the same block qualifies),
//!    each of its results is mapped to the corresponding representative
//!    result. Congruent instructions in incomparable blocks (sibling
//!    branches) are left alone and the newer one becomes the representative
//!    for later, possibly dominated, definitions.
//! 2. **Rewrite.** If any replacement exists, one scan over all instructions
//!    and all FrameStates resolves every value through the replacement chain.
//!    The IR has no use lists; this scan is the value-to-uses map.
//!
//! # Limits
//!
//! * **No memory-dependence model.** Congruence ignores intervening stores
//!   and calls. Field loads such as `Car`/`Cdr` carry default (pure) flags,
//!   so two loads of one field would be congruent even across a mutation.
//!   This is not reached in the production pipeline only because each
//!   inlined accessor guards its own operand, giving the loads distinct
//!   operand values, and GVN runs **before** guard elimination merges those
//!   guards. Reordering the pipeline or running GVN twice needs a memory
//!   token or an `effectful` flag on loads first.
//! * No hoisting to a common dominator: congruent instructions in sibling
//!   branches both survive.
//! * The key is built with `format!`, one string allocation per candidate
//!   instruction.

use std::collections::HashMap;

use crate::t2::frame_state::{FrameStateId, ValueSource};
use crate::t2::ir::{Block, Function, Inst, Value};
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Gvn;

/// Follow the replacement chain to a value's current representative.
fn resolve(map: &HashMap<Value, Value>, mut v: Value) -> Value {
    while let Some(&next) = map.get(&v) {
        if next == v {
            break;
        }
        v = next;
    }
    v
}

/// The congruence key of a pure instruction: opcode + aux + resolved operand
/// value-numbers (canonicalised for commutative opcodes). `AuxData` is not
/// `Hash`/`Eq`, so its `Debug` form (stable and structural) stands in for it.
type Key = (String, Vec<u32>);

impl Pass for Gvn {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        // Own the dominator tree so the immutable analysis borrow does not
        // entangle the mutable rewrite below.
        let dom = a.dominators(f).clone();

        // value → representative value (only redundant results are keys).
        let mut replacement: HashMap<Value, Value> = HashMap::new();
        // congruence class → (representative inst, its block).
        let mut leaders: HashMap<Key, (Inst, Block)> = HashMap::new();

        // ── Pass 1: value-number pure instructions in RPO. ──
        for b in f.reverse_postorder() {
            for &inst in &f.block(b).insts {
                let data = f.inst(inst);
                // Only pure, value-producing instructions participate. Anything
                // effectful/guarding/calling or any terminator is skipped —
                // eliminating it could drop a side effect or a control edge.
                if data.flags.effectful
                    || data.flags.guard
                    || data.flags.call
                    || data.opcode.is_terminator()
                    || data.results.is_empty()
                {
                    continue;
                }

                let mut ops: Vec<u32> = data
                    .args
                    .iter()
                    .map(|&v| resolve(&replacement, v).index() as u32)
                    .collect();
                if data.flags.commutative {
                    ops.sort_unstable();
                }
                let key: Key = (format!("{:?}|{:?}", data.opcode, data.aux), ops);

                match leaders.get(&key) {
                    // Congruent to a representative that dominates this block
                    // (reflexive: an earlier def in the same block qualifies).
                    Some(&(leader_inst, leader_block)) if dom.dominates(leader_block, b) => {
                        let leader_results = f.inst(leader_inst).results.clone();
                        for (i, &r) in data.results.iter().enumerate() {
                            if let Some(&lr) = leader_results.get(i) {
                                replacement.insert(r, lr);
                            }
                        }
                    }
                    // No representative, or one in an incomparable branch: this
                    // instruction becomes the representative for its class.
                    _ => {
                        leaders.insert(key, (inst, b));
                    }
                }
            }
        }

        if replacement.is_empty() {
            return;
        }

        // ── Pass 2: rewrite every use of a replaced value. ──
        for i in 0..f.num_insts() {
            let inst = Inst(i as u32);
            let data = f.inst_mut(inst);
            for arg in data.args.iter_mut() {
                *arg = resolve(&replacement, *arg);
            }
            for target in data.targets.iter_mut() {
                for arg in target.args.iter_mut() {
                    *arg = resolve(&replacement, *arg);
                }
            }
        }

        // Deopt-preservation invariant (spec §4.10 R4.60): a value named by any
        // FrameState is deopt-live; reroute it to its dominating representative
        // rather than orphaning it.
        for idx in 0..f.frame_states.len() {
            let fs = f.frame_states.get_mut(FrameStateId(idx as u32));
            for scope in fs.scopes.iter_mut() {
                for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                    if let ValueSource::Value { value, .. } = src {
                        *value = resolve(&replacement, *value);
                    }
                }
            }
            for recipe in fs.remat.iter_mut() {
                for src in recipe.inputs.iter_mut() {
                    if let ValueSource::Value { value, .. } = src {
                        *value = resolve(&replacement, *value);
                    }
                }
            }
        }

        // Uses were rerouted (and dead defs left behind); drop cached analyses.
        a.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{
        AuxData, BlockCall, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
    };

    const FIX: fn() -> (IRType, ValueRepresentation) =
        || (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged);

    fn fixnum_param(f: &mut Function, b: Block) -> Value {
        f.add_block_param(b, IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)
    }

    /// Append a binary instruction, returning its single result.
    fn binop(
        f: &mut Function,
        b: Block,
        opcode: Opcode,
        args: Vec<Value>,
        flags: InstFlags,
    ) -> Value {
        let (_, results) = f.push_inst(
            b,
            InstData {
                opcode,
                args,
                results: vec![],
                aux: AuxData::None,
                flags,
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[FIX()],
        );
        results[0]
    }

    fn ret(f: &mut Function, b: Block) {
        f.set_terminator(
            b,
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
    }

    fn jump(f: &mut Function, from: Block, to: Block, args: Vec<Value>) {
        f.set_terminator(
            from,
            InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![BlockCall { block: to, args }],
                frame_state: None,
                source_pos: 0,
            },
        );
    }

    fn run_gvn(f: &mut Function) {
        let mut a = Analyses::new();
        let mut g = Gvn;
        g.run(f, &mut a);
    }

    /// The instruction that produced `v` (its args are what we inspect).
    fn producer_args(f: &Function, v: Value) -> Vec<Value> {
        for i in 0..f.num_insts() {
            let inst = Inst(i as u32);
            if f.inst(inst).results.contains(&v) {
                return f.inst(inst).args.clone();
            }
        }
        panic!("no producer for {v:?}");
    }

    // Two identical FixnumAdds in a dominance chain collapse: the consumer's
    // operands both resolve to the first add's result.
    #[test]
    fn congruent_adds_collapse() {
        let mut f = Function::new("collapse");
        let e = f.entry();
        let a = fixnum_param(&mut f, e);
        let b = fixnum_param(&mut f, e);
        let r1 = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );
        let r2 = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );
        // Consumer uses both results; after GVN both must be r1.
        let sum = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![r1, r2],
            InstFlags::default(),
        );
        ret(&mut f, e);

        run_gvn(&mut f);

        assert_eq!(producer_args(&f, sum), vec![r1, r1], "r2 should fold to r1");
    }

    // Congruent adds in incomparable branches (neither dominates) are NOT folded.
    #[test]
    fn non_dominating_pair_survives() {
        let mut f = Function::new("diamond");
        let e = f.entry();
        let a = fixnum_param(&mut f, e);
        let b = fixnum_param(&mut f, e);
        let b1 = f.make_block();
        let b2 = f.make_block();
        let merge = f.make_block();
        let m = f.add_block_param(
            merge,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
        let _ = m;

        f.set_terminator(
            e,
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
        let r1 = binop(
            &mut f,
            b1,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );
        let r2 = binop(
            &mut f,
            b2,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );
        jump(&mut f, b1, merge, vec![r1]);
        jump(&mut f, b2, merge, vec![r2]);
        ret(&mut f, merge);

        run_gvn(&mut f);

        // b2's edge into merge still carries r2, not r1.
        let t2 = f.terminator(b2).unwrap();
        assert_eq!(
            f.inst(t2).targets[0].args,
            vec![r2],
            "sibling add must not fold"
        );
        let t1 = f.terminator(b1).unwrap();
        assert_eq!(f.inst(t1).targets[0].args, vec![r1]);
    }

    // Effectful instructions (e.g. Load) are never value-numbered away, even when
    // identical and in a dominance chain.
    #[test]
    fn effectful_never_eliminated() {
        let mut f = Function::new("effectful");
        let e = f.entry();
        let p = fixnum_param(&mut f, e);
        let eff = InstFlags {
            effectful: true,
            ..InstFlags::default()
        };
        let l1 = binop(&mut f, e, Opcode::Load, vec![p], eff);
        let l2 = binop(&mut f, e, Opcode::Load, vec![p], eff);
        let sum = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![l1, l2],
            InstFlags::default(),
        );
        ret(&mut f, e);

        run_gvn(&mut f);

        assert_eq!(
            producer_args(&f, sum),
            vec![l1, l2],
            "loads must both survive"
        );
    }

    // A value folded by GVN is also rewritten inside a FrameState (R4.60).
    #[test]
    fn frame_state_value_updated() {
        let mut f = Function::new("deopt");
        let e = f.entry();
        let a = fixnum_param(&mut f, e);
        let b = fixnum_param(&mut f, e);
        let r1 = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );
        let r2 = binop(
            &mut f,
            e,
            Opcode::FixnumAdd,
            vec![a, b],
            InstFlags::default(),
        );

        // A frame state naming the (about-to-be-redundant) r2 in a local slot.
        let fsid = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: r2,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        ret(&mut f, e);

        run_gvn(&mut f);

        match &f.frame_states.get(fsid).scopes[0].locals[0] {
            ValueSource::Value { value, .. } => {
                assert_eq!(*value, r1, "deopt-live r2 must be rerouted to r1");
            }
            other => panic!("unexpected value source: {other:?}"),
        }
    }
}
