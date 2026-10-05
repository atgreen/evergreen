// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 deoptimisation frame state: the IR-level description of the interpreter
//! frame a deoptimising instruction must be able to rebuild.
//!
//! # Purpose
//!
//! Every instruction that can deoptimise (a type guard, checked arithmetic, a
//! call or safepoint that may trap back to the interpreter) carries a
//! [`FrameStateId`] into its function's [`FrameStateTable`]. The [`FrameState`]
//! it names says, for every interpreter local and operand-stack slot live at
//! the corresponding bytecode position, where that slot's value comes from if
//! the deopt fires. This module owns only those types and the table; it does
//! not lower them (deopt.rs), enumerate them for a safepoint (slot_map.rs), or
//! build them from bytecode (build.rs).
//!
//! # Contract
//!
//! * `FrameState.scopes` is a non-empty stack of [`FrameScope`]s, outermost
//!   first, innermost last. Inlining prefixes the caller's scopes to every
//!   deoptimising instruction it clones from the callee, and reconstruction
//!   rebuilds the frames in that order.
//! * `FrameScope.function` is the symbol id of the function whose interpreter
//!   frame the scope rebuilds, `bcp` the bytecode position the interpreter
//!   resumes at, `locals` one [`ValueSource`] per local slot by slot number,
//!   and `stack` one per live operand-stack slot, bottom to top.
//! * A [`ValueSource`] is one of: `Value { value, repr }`, a live SSA value in
//!   machine representation `repr` (the allocator binds it to a location and,
//!   if `repr` is unboxed, the deopt path reboxes it); `Const`, a non-moving
//!   immediate materialised directly; `Unbound`, the slot receives the unbound
//!   marker; or `Remat(id)`, a [`RematRecipe`] from `FrameState.remat` replayed
//!   on the cold path.
//! * A [`RematRecipe`] is a [`RematOp`] over inputs that are themselves
//!   `ValueSource`s, so recipes compose. Only operations that are pure, total
//!   and cheap are admitted to `RematOp`; the verifier requires the recipe
//!   graph to be acyclic.
//! * `FrameStateTable` is append-only: ids are dense indices and are never
//!   invalidated. Many guards at the same bytecode position may share one id.
//!
//! # The deopt-preservation invariant
//!
//! A `Value` named by any frame state is *deopt-live* even if nothing on the
//! fast path uses it. Every optimisation pass obeys one rule: never orphan a
//! `ValueSource::Value`. A pass may remove such a value only by substituting a
//! `Const`, a `Remat` recipe, or another value that dominates every deopt use.
//! DCE in particular does not seed its worklist from frame-state uses; it
//! rematerialises or keeps instead. Lowering turns these references into
//! `deopt_uses` operands so register allocation keeps them locatable at the
//! guard, and the verifier checks that each named value dominates the
//! instruction that carries the state.

use crate::t2::ir::{Value, ValueRepresentation};
use egcl_rt::value::EgclVal;

/// Index into `Function::frame_states`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FrameStateId(pub u32);

/// Index into a `FrameState`'s rematerialisation-recipe pool.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct RematRecipeId(pub u32);

/// The abstract interpreter state to reconstruct on a failed guard.
#[derive(Clone, Debug)]
pub struct FrameState {
    /// Logical deopt frames, outermost first and innermost last.
    pub scopes: Vec<FrameScope>,
    /// Rematerialisation recipes referenced by `ValueSource::Remat` in this
    /// frame state (produced by deopt-aware DCE, opt_dce.rs).
    pub remat: Vec<RematRecipe>,
}

#[derive(Clone, Debug)]
pub struct FrameScope {
    /// The function whose interpreter frame this scope reconstructs (symbol id).
    pub function: u32,
    /// Bytecode position T0 resumes at (`resume_pc` in the lowered form).
    pub bcp: u32,
    /// Source for each interpreter local, indexed by local slot number.
    pub locals: Vec<ValueSource>,
    /// Source for each live operand-stack slot, bottom-to-top.
    pub stack: Vec<ValueSource>,
}

/// Where one interpreter slot's value comes from at deopt time.
#[derive(Clone, Debug)]
pub enum ValueSource {
    /// A live SSA value in representation `repr`. The register allocator
    /// binds it to a location; if `repr` is unboxed, the deopt path reboxes it.
    Value {
        value: Value,
        repr: ValueRepresentation,
    },
    /// A compile-time-constant immediate.
    Const(EgclVal),
    /// Not live in T2 — the interpreter slot receives UNBOUND-MARKER.
    Unbound,
    /// Recompute in the cold deopt path from a recipe.
    Remat(RematRecipeId),
}

/// A pure, total, cheap computation replayed on the cold deopt path to
/// reconstruct a value DCE removed from the fast path.
#[derive(Clone, Debug)]
pub struct RematRecipe {
    pub op: RematOp,
    /// Recipe inputs — themselves `ValueSource`s, so rematerialisation composes.
    pub inputs: Vec<ValueSource>,
    pub result_repr: ValueRepresentation,
}

/// The (restricted, provably pure/total) operations eligible for
/// rematerialisation. Extend only with ops proven pure and total.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum RematOp {
    Const,
    BoxFixnum,
    UnboxFixnum,
    BoxFloat,
    UnboxFloat,
    FixnumAdd,
    FixnumSub,
}

/// The per-`Function` frame-state table. Interns `FrameState`s so many guards on
/// the same bytecode position can share one.
#[derive(Clone, Debug, Default)]
pub struct FrameStateTable {
    states: Vec<FrameState>,
}

impl FrameStateTable {
    /// Intern a frame state, returning its id.
    pub fn add(&mut self, fs: FrameState) -> FrameStateId {
        let id = FrameStateId(self.states.len() as u32);
        self.states.push(fs);
        id
    }

    pub fn get(&self, id: FrameStateId) -> &FrameState {
        &self.states[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: FrameStateId) -> &mut FrameState {
        &mut self.states[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (FrameStateId, &FrameState)> {
        self.states
            .iter()
            .enumerate()
            .map(|(i, fs)| (FrameStateId(i as u32), fs))
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut FrameState> {
        self.states.iter_mut()
    }
}
