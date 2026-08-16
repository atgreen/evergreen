//! T2 deoptimisation frame state — the keystone metadata (spec §4.10).
//!
//! **Phase-0 foundation / frozen contract.** A `FrameState` is the abstract
//! interpreter state a deoptimising instruction (guard, checked arithmetic,
//! trap) must be able to reconstruct: for every interpreter local and
//! operand-stack slot live at the corresponding bytecode index, where its value
//! comes from at deopt time. It is the IR-level source of truth that the deopt
//! stack map (spec §4.6 A4.04) and OSR entry map (D4.09) are lowered from
//! (A4.14, parcel P7).
//!
//! The **deopt-preservation invariant** (spec §4.10 R4.60) is the rule every
//! optimisation pass obeys: never orphan a `ValueSource::Value` — a value named
//! by any FrameState is *deopt-live* and may only be removed by substituting a
//! `Const`, a rematerialisation `Remat`, or another dominating value.

use crate::t2::ir::{Value, ValueRepresentation};
use bliss_rt::value::BlissVal;

/// Index into `Function::frame_states`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FrameStateId(pub u32);

/// Index into a `FrameState`'s rematerialisation-recipe pool.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct RematRecipeId(pub u32);

/// The abstract interpreter state to reconstruct on a failed guard (spec D4.15).
#[derive(Clone, Debug)]
pub struct FrameState {
    /// Deopt scopes, innermost last (spec R4.67). Length 1 until frame-level
    /// inlining exists; then one scope per logical CL frame to un-inline.
    pub scopes: Vec<FrameScope>,
    /// Rematerialisation recipes referenced by `ValueSource::Remat` in this
    /// frame state (spec §4.10 A4.13, parcel P4c).
    pub remat: Vec<RematRecipe>,
}

#[derive(Clone, Debug)]
pub struct FrameScope {
    /// The function whose interpreter frame this scope reconstructs (symbol id).
    pub function: u32,
    /// Bytecode position T0 resumes at (`resume_pc`, spec §4.6 D4.10).
    pub bcp: u32,
    /// Source for each interpreter local, indexed by local slot number.
    pub locals: Vec<ValueSource>,
    /// Source for each live operand-stack slot, bottom-to-top.
    pub stack: Vec<ValueSource>,
}

/// Where one interpreter slot's value comes from at deopt time (spec D4.15).
#[derive(Clone, Debug)]
pub enum ValueSource {
    /// A live SSA value in representation `repr`. The register allocator (P6)
    /// binds it to a location; if `repr` is unboxed, the deopt path reboxes it.
    Value { value: Value, repr: ValueRepresentation },
    /// A compile-time-constant immediate.
    Const(BlissVal),
    /// Not live in T2 — the interpreter slot receives UNBOUND-MARKER.
    Unbound,
    /// Recompute in the cold deopt path from a recipe (spec A4.13).
    Remat(RematRecipeId),
}

/// A pure, total, cheap computation replayed on the cold deopt path to
/// reconstruct a value DCE removed from the fast path (spec D4.17, parcel P4c).
#[derive(Clone, Debug)]
pub struct RematRecipe {
    pub op: RematOp,
    /// Recipe inputs — themselves `ValueSource`s, so rematerialisation composes.
    pub inputs: Vec<ValueSource>,
    pub result_repr: ValueRepresentation,
}

/// The (restricted, provably pure/total) operations eligible for
/// rematerialisation. Extend as P4c proves more ops safe.
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

    pub fn len(&self) -> usize { self.states.len() }
    pub fn is_empty(&self) -> bool { self.states.is_empty() }
    pub fn iter(&self) -> impl Iterator<Item = (FrameStateId, &FrameState)> {
        self.states.iter().enumerate().map(|(i, fs)| (FrameStateId(i as u32), fs))
    }
}
