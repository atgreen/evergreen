// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Dead multiple-value-reset elimination.
//!
//! # Purpose
//!
//! [`ClearMvElim`] removes `ClearMv` instructions whose effect can never be
//! observed. The bytecode lowering emits `ClearMv` after every `SETQ`, after
//! single-valued forms in value position, and before returns, so that the
//! thread's multiple-value state never leaks stale values to a consumer. In
//! T2 each one lowers to a runtime helper call (`c2i_mv` with a zero count).
//! Inside a call-free loop that is the only runtime call left, and it is paid
//! once per assignment for a state nothing reads until the loop exits — where
//! the lowering has already placed another `ClearMv` (bliss-y7wdk).
//!
//! # Contract
//!
//! **Input:** a well-formed `Function`.
//!
//! **Output:** `ClearMv` instructions that are dead under the analysis below
//! are unlinked from their block (arena entries stay, unreferenced). No value
//! definition, FrameState, or CFG edge changes, so no analysis is invalidated.
//!
//! # Algorithm
//!
//! One-bit backward liveness of "the multiple-value state may be read before
//! it is next reset":
//!
//! * a **reader** sets the bit: every instruction that may run foreign or
//!   Lisp code or hand the state to a consumer — any instruction with the
//!   `call` flag, `Call`, `Invoke`, `TakeValuesToLocals`, the cleanup, catch
//!   and handler landings, and the function-exit terminators `Return`,
//!   `TailCall`, `Throw`, `NlxTransfer`. A plain call is treated as a reader
//!   rather than a reset because builtins leave the state untouched;
//! * a **`ClearMv`** clears the bit (its own effect makes any earlier reset
//!   redundant);
//! * everything else — arithmetic, guards, loads, stores, branches — is
//!   transparent. A guard that deoptimises resumes the interpreter at the
//!   same bytecode position, and the interpreter then executes the same
//!   sequence of resets and readers this analysis saw, so a deopt edge never
//!   needs the removed reset.
//!
//! `live_in` of each block is computed to a fixpoint over the CFG (iterating
//! in reverse postorder of the reversed problem is unnecessary: the lattice is
//! one bit and the function is small). A `ClearMv` is dead iff the bit is
//! clear immediately after it.
//!
//! # Rationale
//!
//! The lowering emits resets locally, per form, with no knowledge of what
//! follows. Deciding liveness over the whole function is cheap and turns a
//! loop body of pure arithmetic back into straight-line native code.

use crate::t2::ir::{Function, Inst, Opcode};
use crate::t2::pass::{Analyses, Pass};
use std::collections::{HashMap, HashSet};

/// Instructions entered with a proven empty, inactive multiple-value buffer.
///
/// `single_entry` is an ABI precondition supplied by the client, not a fact
/// inferred from the result's SSA type. OSR entries can carry arbitrary pending
/// values and always override that precondition. Calls and control landings
/// forget it; a ClearMv establishes it. Intersecting predecessors makes this a
/// must analysis, including loop backedges.
pub fn single_value_state(f: &Function, single_entry: bool) -> HashSet<Inst> {
    let blocks = f.block_order();
    let incoming = |b, outgoing: &HashMap<_, bool>| {
        if f.osr_entries.iter().any(|entry| entry.block == b) {
            return false;
        }
        let preds = f.preds(b);
        let entry = b == f.entry();
        (if entry { single_entry } else { !preds.is_empty() })
            && preds.iter().all(|pred| outgoing[pred])
    };
    let advance = |inst, single| {
        let data = f.inst(inst);
        if data.opcode == Opcode::ClearMv {
            true
        } else if data.flags.call || matches!(data.opcode,
            Opcode::Call | Opcode::Invoke | Opcode::CleanupSave
                | Opcode::CleanupRestore | Opcode::CleanupLanding
                | Opcode::CatchLanding | Opcode::HandlerLanding)
        {
            false
        } else {
            single
        }
    };
    // Start at the top of the must lattice: cycles without a mutating operation
    // preserve the entry fact, rather than making every loop permanently unknown.
    let mut outgoing: HashMap<_, bool> = blocks.iter().map(|&b| (b, true)).collect();
    loop {
        let mut changed = false;
        for &block in blocks {
            let mut single = incoming(block, &outgoing);
            for &inst in &f.block(block).insts {
                single = advance(inst, single);
            }
            if outgoing[&block] != single {
                outgoing.insert(block, single);
                changed = true;
            }
        }
        if !changed { break; }
    }
    let mut known = HashSet::new();
    for &block in blocks {
        let mut single = incoming(block, &outgoing);
        for &inst in &f.block(block).insts {
            if single { known.insert(inst); }
            single = advance(inst, single);
        }
    }
    known
}

/// Remove resets whose complete effect is already true, including release of
/// secondary GC roots. Unlike backward dead-reset elimination this never moves
/// a reset across a safepoint or assumes that a call returns one value.
pub fn eliminate_redundant(f: &mut Function, single_entry: bool) -> usize {
    let known = single_value_state(f, single_entry);
    let redundant: HashSet<_> = known.into_iter()
        .filter(|&inst| f.inst(inst).opcode == Opcode::ClearMv).collect();
    for block in f.block_order().to_vec() {
        f.block_mut(block).insts.retain(|inst| !redundant.contains(inst));
    }
    redundant.len()
}

#[derive(Default)]
pub struct ClearMvElim;

impl Pass for ClearMvElim {
    fn name(&self) -> &'static str {
        "clear-mv-elim"
    }

    fn run(&mut self, f: &mut Function, _a: &mut Analyses) {
        eliminate(f);
    }
}

/// Whether `op` (or the `call` flag) may read the multiple-value state or
/// deliver it to another activation.
fn reads_mv(f: &Function, inst: Inst) -> bool {
    let d = f.inst(inst);
    d.flags.call
        || matches!(
            d.opcode,
            Opcode::Call
                | Opcode::Invoke
                | Opcode::TakeValuesToLocals
                | Opcode::CleanupSave
                | Opcode::CleanupRestore
                | Opcode::CleanupLanding
                | Opcode::CatchLanding
                | Opcode::HandlerLanding
                | Opcode::Return
                | Opcode::TailCall
                | Opcode::Throw
                | Opcode::NlxTransfer
        )
}

/// Backward transfer over one block: the live bit at the block's start given
/// the bit at its end.
fn transfer(f: &Function, insts: &[Inst], live_out: bool) -> bool {
    let mut live = live_out;
    for &inst in insts.iter().rev() {
        if f.inst(inst).opcode == Opcode::ClearMv {
            live = false;
        } else if reads_mv(f, inst) {
            live = true;
        }
    }
    live
}

fn live_at_block_entries(f: &Function) -> HashMap<crate::t2::ir::Block, bool> {
    let blocks: Vec<_> = f.block_order().to_vec();
    let mut live_in: HashMap<_, bool> = blocks.iter().map(|&b| (b, false)).collect();

    // Fixpoint. Starting from "dead everywhere" and only ever raising bits is
    // monotone on the one-bit lattice, so it terminates.
    loop {
        let mut changed = false;
        for &b in blocks.iter().rev() {
            let live_out = f.succs(b).iter().any(|s| live_in[s]);
            let li = transfer(f, &f.block(b).insts, live_out);
            if li != live_in[&b] {
                live_in.insert(b, li);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    live_in
}

/// Instructions whose incoming MV state cannot reach a reader before a reset.
/// Calls and exceptional/control boundaries are conservatively readers, just as
/// in dead-reset elimination. This permits earlier normalization only when all
/// successor paths discard the old tuple; it does not infer a single result.
pub fn unobserved_multiple_value_state(f: &Function) -> HashSet<Inst> {
    let live_in = live_at_block_entries(f);
    let mut unobserved = HashSet::new();
    for &block in f.block_order() {
        let mut live = f.succs(block).iter().any(|successor| live_in[successor]);
        for &inst in f.block(block).insts.iter().rev() {
            if f.inst(inst).opcode == Opcode::ClearMv {
                live = false;
            } else if reads_mv(f, inst) {
                live = true;
            }
            if !live { unobserved.insert(inst); }
        }
    }
    unobserved
}

/// Remove every dead `ClearMv`; returns how many were removed.
pub fn eliminate(f: &mut Function) -> usize {
    let blocks: Vec<_> = f.block_order().to_vec();
    let live_in = live_at_block_entries(f);
    let mut dead: Vec<(_, Inst)> = Vec::new();
    for &b in &blocks {
        let mut live = f.succs(b).iter().any(|s| live_in[s]);
        for &inst in f.block(b).insts.iter().rev() {
            if f.inst(inst).opcode == Opcode::ClearMv {
                if !live {
                    dead.push((b, inst));
                }
                live = false;
            } else if reads_mv(f, inst) {
                live = true;
            }
        }
    }

    for &(b, inst) in &dead {
        f.block_mut(b).insts.retain(|&i| i != inst);
    }
    dead.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{AuxData, Block, BlockCall, InstData, InstFlags};

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

    fn clear_mv() -> InstData {
        InstData {
            flags: InstFlags {
                effectful: true,
                ..InstFlags::default()
            },
            ..base(Opcode::ClearMv)
        }
    }

    fn call() -> InstData {
        InstData {
            flags: InstFlags {
                effectful: true,
                call: true,
                ..InstFlags::default()
            },
            ..base(Opcode::Call)
        }
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

    fn brif(then: Block, els: Block) -> InstData {
        InstData {
            targets: vec![
                BlockCall {
                    block: then,
                    args: vec![],
                },
                BlockCall {
                    block: els,
                    args: vec![],
                },
            ],
            ..base(Opcode::Brif)
        }
    }

    fn count_clear_mv(f: &Function) -> usize {
        f.block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.clone())
            .filter(|&i| f.inst(i).opcode == Opcode::ClearMv)
            .count()
    }

    #[test]
    fn reset_before_return_survives() {
        let mut f = Function::new("ret");
        let e = f.entry();
        f.push_inst(e, clear_mv(), &[]);
        f.set_terminator(e, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 0);
        assert_eq!(count_clear_mv(&f), 1);
    }

    #[test]
    fn consecutive_resets_keep_only_the_last() {
        let mut f = Function::new("triple");
        let e = f.entry();
        f.push_inst(e, clear_mv(), &[]);
        f.push_inst(e, clear_mv(), &[]);
        f.push_inst(e, clear_mv(), &[]);
        f.set_terminator(e, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 2);
        assert_eq!(count_clear_mv(&f), 1);
    }

    #[test]
    fn reset_before_call_survives() {
        // A call may read or publish the state, so the reset ahead of it is
        // observable; the one after it is killed by the return's reset.
        let mut f = Function::new("call");
        let e = f.entry();
        f.push_inst(e, clear_mv(), &[]);
        f.push_inst(e, call(), &[]);
        f.push_inst(e, clear_mv(), &[]);
        f.push_inst(e, clear_mv(), &[]);
        f.set_terminator(e, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 1);
        assert_eq!(count_clear_mv(&f), 2);
    }

    #[test]
    fn loop_body_reset_is_dead_when_every_exit_resets() {
        // entry -> header ; header -> body | exit ; body: ClearMv -> header ;
        // exit: ClearMv ; Return.  The body's reset is dead: every path from
        // it reaches the exit reset or itself before a reader.
        let mut f = Function::new("loop");
        let e = f.entry();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        f.set_terminator(e, jump(header));
        f.set_terminator(header, brif(body, exit));
        f.push_inst(body, clear_mv(), &[]);
        f.set_terminator(body, jump(header));
        f.push_inst(exit, clear_mv(), &[]);
        f.set_terminator(exit, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 1);
        assert_eq!(count_clear_mv(&f), 1);
        assert!(f
            .block(body)
            .insts
            .iter()
            .all(|&i| f.inst(i).opcode != Opcode::ClearMv));
    }

    #[test]
    fn loop_body_reset_survives_an_unreset_exit() {
        // Same shape, but the exit returns without its own reset: the body's
        // reset is the only thing standing between a stale state and the
        // caller.
        let mut f = Function::new("leaky");
        let e = f.entry();
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        f.set_terminator(e, jump(header));
        f.set_terminator(header, brif(body, exit));
        f.push_inst(body, clear_mv(), &[]);
        f.set_terminator(body, jump(header));
        f.set_terminator(exit, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 0);
        assert_eq!(count_clear_mv(&f), 1);
    }

    #[test]
    fn reset_on_one_branch_only_survives_the_other() {
        // entry: ClearMv ; Brif -> a | b.  a: ClearMv ; Return.  b: Return.
        // The entry reset is observable through b.
        let mut f = Function::new("diamond");
        let e = f.entry();
        let a = f.make_block();
        let b = f.make_block();
        f.push_inst(e, clear_mv(), &[]);
        f.set_terminator(e, brif(a, b));
        f.push_inst(a, clear_mv(), &[]);
        f.set_terminator(a, base(Opcode::Return));
        f.set_terminator(b, base(Opcode::Return));
        assert_eq!(eliminate(&mut f), 0);
        assert_eq!(count_clear_mv(&f), 2);
    }

    #[test]
    fn known_entry_elides_resets_but_unknown_entry_keeps_first() {
        for (entry, remaining) in [(true, 0), (false, 1)] {
            let mut f = Function::new("entry");
            let e = f.entry();
            f.push_inst(e, clear_mv(), &[]);
            f.push_inst(e, clear_mv(), &[]);
            f.set_terminator(e, base(Opcode::Return));
            assert_eq!(eliminate_redundant(&mut f, entry), 2 - remaining);
            assert_eq!(count_clear_mv(&f), remaining);
        }
    }

    #[test]
    fn calls_and_control_landings_forget_single_value_state() {
        for op in [Opcode::Call, Opcode::CleanupRestore,
            Opcode::CleanupLanding, Opcode::CatchLanding, Opcode::HandlerLanding]
        {
            let mut f = Function::new("effect");
            let e = f.entry();
            f.push_inst(e, base(op), &[]);
            f.push_inst(e, clear_mv(), &[]);
            f.set_terminator(e, base(Opcode::Return));
            assert_eq!(eliminate_redundant(&mut f, true), 0, "{op:?}");
        }
    }

    #[test]
    fn invoke_forgets_single_state_on_both_successors() {
        let mut f = Function::new("invoke");
        let e = f.entry();
        let normal = f.make_block();
        let exceptional = f.make_block();
        f.set_terminator(e, InstData {
            targets: vec![BlockCall { block: normal, args: vec![] },
                BlockCall { block: exceptional, args: vec![] }],
            ..base(Opcode::Invoke)
        });
        for b in [normal, exceptional] {
            f.push_inst(b, clear_mv(), &[]);
            f.set_terminator(b, base(Opcode::Return));
        }
        assert_eq!(eliminate_redundant(&mut f, true), 0);
    }

    #[test]
    fn join_needs_single_state_from_every_predecessor() {
        let mut f = Function::new("join");
        let e = f.entry();
        let changed = f.make_block();
        let join = f.make_block();
        f.set_terminator(e, brif(changed, join));
        f.push_inst(changed, call(), &[]);
        f.set_terminator(changed, jump(join));
        f.push_inst(join, clear_mv(), &[]);
        f.set_terminator(join, base(Opcode::Return));
        assert_eq!(eliminate_redundant(&mut f, true), 0);
    }

    #[test]
    fn pure_loop_preserves_entry_state_but_osr_does_not() {
        for osr in [false, true] {
            let mut f = Function::new("loop-entry");
            let e = f.entry();
            let header = f.make_block();
            let exit = f.make_block();
            f.set_terminator(e, jump(header));
            f.set_terminator(header, brif(header, exit));
            f.push_inst(exit, clear_mv(), &[]);
            f.set_terminator(exit, base(Opcode::Return));
            if osr {
                f.osr_entries.push(crate::t2::ir::OsrEntry {
                    block: header, bcp: 0, control_scopes: vec![], checks: vec![],
                    frame_state: crate::t2::frame_state::FrameStateId(0),
                });
            }
            assert_eq!(eliminate_redundant(&mut f, true), usize::from(!osr));
        }
    }

    #[test]
    fn incoming_mv_is_dead_only_when_every_successor_discards_it() {
        let mut f = Function::new("mv-branches");
        let entry = f.entry();
        let left = f.make_block();
        let right = f.make_block();
        let (point, _) = f.push_inst(entry, base(Opcode::MemoryFence), &[]);
        f.set_terminator(entry, brif(left, right));
        f.push_inst(left, clear_mv(), &[]);
        f.set_terminator(left, base(Opcode::Return));
        f.set_terminator(right, base(Opcode::Return));
        assert!(!unobserved_multiple_value_state(&f).contains(&point),
            "the right return can still observe the old tuple");
        let ret = f.block_mut(right).insts.pop().unwrap();
        f.push_inst(right, clear_mv(), &[]);
        f.block_mut(right).insts.push(ret);
        assert!(unobserved_multiple_value_state(&f).contains(&point));
    }

    #[test]
    fn incoming_mv_liveness_propagates_through_loop_backedges() {
        let mut f = Function::new("mv-loop");
        let entry = f.entry();
        let loop_body = f.make_block();
        let exit = f.make_block();
        let (point, _) = f.push_inst(entry, base(Opcode::MemoryFence), &[]);
        f.set_terminator(entry, jump(loop_body));
        f.set_terminator(loop_body, brif(loop_body, exit));
        f.push_inst(exit, clear_mv(), &[]);
        f.set_terminator(exit, base(Opcode::Return));
        assert!(unobserved_multiple_value_state(&f).contains(&point));
        let branch = f.block_mut(loop_body).insts.pop().unwrap();
        f.push_inst(loop_body, call(), &[]);
        f.block_mut(loop_body).insts.push(branch);
        assert!(!unobserved_multiple_value_state(&f).contains(&point),
            "a reader on the cycle must keep the incoming tuple live");
    }

}
