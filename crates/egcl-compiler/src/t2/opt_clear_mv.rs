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
use std::collections::HashMap;

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

/// Remove every dead `ClearMv`; returns how many were removed.
pub fn eliminate(f: &mut Function) -> usize {
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
}
