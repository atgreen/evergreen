// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FrameState lowering to resolved deopt descriptors, and the reference
//! reconstruction that replays them.
//!
//! # Purpose
//!
//! After register allocation has bound every SSA value to a [`Location`], each
//! deoptimising instruction's [`FrameState`] can be resolved from "which value"
//! to "which register or stack slot, and what to do with the raw word". This
//! module owns that lowering ([`lower_one`], [`lower_frame_states`]) and its
//! output model ([`LoweredDeopt`], [`LoweredScope`], [`SlotDescriptor`],
//! [`Rebox`], [`RematDescriptor`]), plus [`reconstruct`], which evaluates the
//! descriptors against a [`MachineState`] to produce tagged interpreter frames.
//! Which slots exist and in what representation comes from slot_map.rs, not
//! from here.
//!
//! # Contract
//!
//! **Input:** a `FrameState`, a `code_offset` to stamp on the result, and a
//! `loc_of: Fn(Value) -> Option<Location>` binding. Lowering names slots by SSA
//! `Value` while the allocator reports locations per `VReg`; the two are joined
//! by the caller, since VReg numbers are assigned as the `Value` index. The
//! production caller is transfer_map.rs, which builds `loc_of` from the
//! `deopt_uses` positions of `inst_allocations` at the throwing call. There is
//! no field on `mach::StackMap` for the resolved form, so lowering returns a
//! side table rather than writing into `mf.stack_maps`.
//!
//! **Output:** one `LoweredDeopt` per frame state: `code_offset` plus one
//! `LoweredScope` per `FrameState` scope, outermost first. A scope carries
//! `resume_pc`, `function`, `slots` (locals then operand stack, in slot_map
//! order), `num_locals` to split them, and `live_ref_bitmap`, set exactly for
//! a `Tagged` `Value` slot. Each slot is one of:
//!
//! * `InLocation(loc, rebox)` — read `loc`, apply `rebox` (`None` for tagged,
//!   `ReboxFixnum` = tag-shift, `ReboxF32` = pack a single-float immediate,
//!   `ReboxF64` = heap-allocate a double-float).
//! * `MaterializeConst(k)` — write the immediate. Only non-moving immediates
//!   are accepted; a cons, heap object, or function presented as a constant is
//!   `LowerError::MovingConst`, because such a value must stay a located SSA
//!   value loaded from its rooted constant-pool slot.
//! * `Unbound` — write the unbound marker.
//! * `Remat(recipe)` — a `RematOp` over inputs that are themselves descriptors.
//!
//! **Failure** is a [`LowerError`]: an unallocated deopt-live value, a missing
//! or over-deep (probably cyclic) recipe, a bad frame-state id, or a moving
//! constant. Any of these aborts the compile; partial metadata is never
//! installed.
//!
//! # Algorithm
//!
//! 1. For each scope, enumerate slots through `slot_map::slot_specs` so the
//!    order and representation match the OSR import side.
//! 2. Lower each source: `Value` → `loc_of` (or `Unallocated`) plus
//!    `Rebox::for_repr`; `Const` → moving-reference check; `Remat` → fetch the
//!    recipe and recurse on its inputs, bounded by `REMAT_MAX_DEPTH`.
//! 3. Set the live-ref bit from the slot's source kind and `slot_map` repr.
//!    A rematerialised value is never a root: it is recomputed, not scanned.
//! 4. Assemble the `LoweredScope`s into a `LoweredDeopt`.
//!
//! [`reconstruct`] is the inverse: for each scope it reads every `InLocation`
//! through `MachineState::read`, reboxes, replays recipes at the tagged level
//! (box/unbox ops are no-ops on a tagged fixnum, arithmetic reads fixnums), and
//! splits the results into `locals` and `stack`. `ReboxF64` needs the runtime's
//! double-float allocator, supplied through `MachineState::box_double`; because
//! that may collect, completed frames and the frame under construction are
//! rooted for the duration, and the caller must root the returned frames
//! before its next allocation. transfer_capture.rs is the production
//! `MachineState`, replaying a captured transfer snapshot.
//!
//! # Scope limit
//!
//! The native emitters' own guard-deopt paths read `FrameState` and
//! `slot_map` directly and emit their slot code inline; they do not go through
//! `SlotDescriptor`. This module is the authority for the exception-transfer
//! path and the round-trip tests, and `reconstruct` evaluates recipes at the
//! tagged level rather than honouring `result_repr` for unboxed intermediates
//! (the field is carried so that refinement is local).

use crate::t2::frame_state::{FrameState, RematOp, RematRecipeId, ValueSource};
use crate::t2::ir::{Value, ValueRepresentation};
use crate::t2::mach::{Location, MachFunc};
use egcl_rt::value::{EgclVal, UNBOUND};

/// Maximum rematerialisation-recipe nesting depth we will lower before giving
/// up. A cyclic recipe (rejected by the verifier) or a pathologically
/// deep one fails lowering rather than blowing the stack.
const REMAT_MAX_DEPTH: usize = 32;

// ── Lowered descriptor model ────────────────────────────────────────────────

/// The rebox step applied to a slot's raw machine word before it is written
/// into a tagged interpreter slot. Derived purely from the value's
/// [`ValueRepresentation`]: only `Tagged` needs no rebox.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Rebox {
    /// Already a tagged `EgclVal`; write through unchanged.
    None,
    /// Raw untagged `i64` fixnum → tag-shift (`n << 3`).
    ReboxFixnum,
    /// Raw `f32` bits → single-float immediate (`(bits << 32) | tag`).
    ReboxF32,
    /// Raw `f64` bits → heap-allocated `double-float` (needs the runtime
    /// allocator, see [`MachineState::box_double`]).
    ReboxF64,
}

impl Rebox {
    /// The rebox implied by a value's machine representation.
    pub fn for_repr(repr: ValueRepresentation) -> Rebox {
        match repr {
            ValueRepresentation::Tagged => Rebox::None,
            ValueRepresentation::UnboxedFixnum => Rebox::ReboxFixnum,
            ValueRepresentation::UnboxedF32 => Rebox::ReboxF32,
            ValueRepresentation::UnboxedF64 => Rebox::ReboxF64,
        }
    }
}

/// One interpreter slot resolved to a concrete deopt descriptor — the lowered
/// form of a [`ValueSource`]. This is what frame reconstruction consumes.
#[derive(Clone, PartialEq, Debug)]
pub enum SlotDescriptor {
    /// The value lives in `Location`; apply `Rebox` to the raw word before
    /// writing the tagged interpreter slot.
    InLocation(Location, Rebox),
    /// Materialise a compile-time-constant immediate (written directly).
    MaterializeConst(EgclVal),
    /// Not live in T2 → the interpreter slot receives UNBOUND-MARKER. A later
    /// reference signals the correct CL error.
    Unbound,
    /// Recompute on the cold deopt path from a lowered recipe. Recipe
    /// inputs are themselves `SlotDescriptor`s, so rematerialisation composes.
    Remat(RematDescriptor),
}

/// A lowered rematerialisation recipe: the same shape as [`crate::t2::frame_state::RematRecipe`]
/// but with each input already resolved to a [`SlotDescriptor`].
#[derive(Clone, PartialEq, Debug)]
pub struct RematDescriptor {
    pub op: RematOp,
    /// `Vec` provides the indirection that keeps `SlotDescriptor` finitely sized
    /// despite the `SlotDescriptor → RematDescriptor → SlotDescriptor` cycle.
    pub inputs: Vec<SlotDescriptor>,
    pub result_repr: ValueRepresentation,
}

/// The lowered deopt metadata for one trap site. Corresponds one-to-one with a deopt [`crate::t2::mach::StackMap`].
#[derive(Clone, PartialEq, Debug)]
pub struct LoweredDeopt {
    /// Native code offset of the trap (copied from the source `StackMap`).
    pub code_offset: u32,
    /// Logical frames, outermost to innermost, matching `FrameState::scopes`.
    pub scopes: Vec<LoweredScope>,
}

/// Resolved descriptors for one logical interpreter frame at a deopt site.
#[derive(Clone, PartialEq, Debug)]
pub struct LoweredScope {
    /// Bytecode PC T0 resumes at.
    pub resume_pc: u32,
    /// Symbol id of the function whose interpreter frame is reconstructed
    /// (`FrameScope.function`; resolvable to a name via `egcl_rt::symbols`).
    pub function: u32,
    /// Per-slot descriptors, `locals` first then `stack`, in slot order.
    pub slots: Vec<SlotDescriptor>,
    /// How many leading entries of `slots` are interpreter locals; the rest are
    /// operand-stack slots (bottom-to-top). Lets `reconstruct` split the frame.
    pub num_locals: usize,
    /// GC live-ref bit per slot: set iff the slot is a `Tagged` `Value` (which
    /// may hold a pointer). Immediates and unboxed values are never roots here.
    pub live_ref_bitmap: Vec<bool>,
}

/// Why a FrameState could not be fully lowered. Any of these fails compilation
/// (the function stays at its current tier); partial metadata is never installed.
#[derive(Clone, PartialEq, Debug)]
pub enum LowerError {
    /// A deopt-live `Value` had no allocated `Location` (the `loc_of` map
    /// returned `None`): a deopt-live value was orphaned.
    Unallocated(Value),
    /// A `Remat` source referenced a recipe index absent from the pool.
    MissingRecipe(RematRecipeId),
    /// Rematerialisation recipes nested past `REMAT_MAX_DEPTH` — treated as a
    /// (probable) cycle, which the verifier is required to reject.
    RematTooDeep,
    /// A deopt `StackMap` named a `FrameStateId` out of range for the table.
    BadFrameStateId(u32),
    /// A moving heap reference was presented as an immediate constant. Such a
    /// value must remain a located SSA value loaded from a rooted pool slot.
    MovingConst(EgclVal),
}

// ── FrameState lowering ─────────────────────────────────────────────────────

/// Lower every deopt `StackMap` in `mf` to its resolved [`LoweredDeopt`] form.
/// Called after register allocation has assigned locations.
///
/// `loc_of` resolves an SSA `Value` to the `Location` the allocator bound it to
/// (see the module docs for how callers build it); returning `None` is a hard
/// failure.
///
/// Returns one entry per deopt stack map (those whose `frame_state` is `Some`),
/// in stack-map order, as a side table: `mach::StackMap` has no field for the
/// resolved form.
pub fn lower_frame_states(
    mf: &MachFunc,
    frame_states: &[FrameState],
    loc_of: impl Fn(Value) -> Option<Location>,
) -> Result<Vec<LoweredDeopt>, LowerError> {
    let mut out = Vec::new();
    for sm in &mf.stack_maps {
        let Some(id) = sm.frame_state else { continue };
        let fs = frame_states
            .get(id.0 as usize)
            .ok_or(LowerError::BadFrameStateId(id.0))?;
        out.push(lower_one(sm.code_offset, fs, &loc_of)?);
    }
    Ok(out)
}

/// Lower a single FrameState against a code offset and location map.
pub fn lower_one(
    code_offset: u32,
    fs: &FrameState,
    loc_of: &impl Fn(Value) -> Option<Location>,
) -> Result<LoweredDeopt, LowerError> {
    let mut scopes = Vec::with_capacity(fs.scopes.len());
    for scope in &fs.scopes {
        // Enumerate through `slot_map` — the same producer the OSR import side
        // reads — so the two directions cannot disagree about which slots are
        // live, in what order, or in what representation (bliss-ht4).
        let specs = crate::t2::slot_map::slot_specs(scope, fs);
        let mut slots = Vec::with_capacity(specs.len());
        let mut live_ref_bitmap = Vec::with_capacity(specs.len());
        for spec in &specs {
            let (desc, _) = lower_source(&spec.source, fs, loc_of, 0)?;
            slots.push(desc);
            // The live-ref bit is set only for a `Value` that is `Tagged`.
            // Representation alone is NOT the rule: a `Remat` slot is
            // recomputed cold rather than scanned (see the module header). The
            // representation half of the test comes from `slot_map`, so it
            // agrees with the OSR import's conversion choice.
            live_ref_bitmap.push(
                matches!(spec.source, ValueSource::Value { .. })
                    && spec.repr == ValueRepresentation::Tagged,
            );
        }
        scopes.push(LoweredScope {
            resume_pc: scope.bcp,
            function: scope.function,
            slots,
            num_locals: crate::t2::slot_map::num_locals(scope),
            live_ref_bitmap,
        });
    }
    Ok(LoweredDeopt {
        code_offset,
        scopes,
    })
}

/// Lower one `ValueSource` to its descriptor. Returns `(descriptor, is_gc_ref)`
/// where `is_gc_ref` feeds the live-ref bitmap (set only for a `Tagged` value).
fn lower_source(
    source: &ValueSource,
    fs: &FrameState,
    loc_of: &impl Fn(Value) -> Option<Location>,
    depth: usize,
) -> Result<(SlotDescriptor, bool), LowerError> {
    match source {
        ValueSource::Value { value, repr } => {
            let loc = loc_of(*value).ok_or(LowerError::Unallocated(*value))?;
            let rebox = Rebox::for_repr(*repr);
            // Only a Tagged value may hold a GC pointer; unboxed forms
            // are never roots.
            let is_ref = matches!(repr, ValueRepresentation::Tagged);
            Ok((SlotDescriptor::InLocation(loc, rebox), is_ref))
        }
        ValueSource::Const(k) => {
            if k.is_cons() || k.is_heap_object() || k.is_function() {
                return Err(LowerError::MovingConst(*k));
            }
            Ok((SlotDescriptor::MaterializeConst(*k), false))
        }
        ValueSource::Unbound => Ok((SlotDescriptor::Unbound, false)),
        ValueSource::Remat(id) => {
            if depth >= REMAT_MAX_DEPTH {
                return Err(LowerError::RematTooDeep);
            }
            let recipe = fs
                .remat
                .get(id.0 as usize)
                .ok_or(LowerError::MissingRecipe(*id))?;
            let mut inputs = Vec::with_capacity(recipe.inputs.len());
            for input in &recipe.inputs {
                // Recurse; a rematerialised value is never itself a GC root (it
                // is recomputed cold, not scanned), so discard the ref flag.
                let (desc, _) = lower_source(input, fs, loc_of, depth + 1)?;
                inputs.push(desc);
            }
            let desc = SlotDescriptor::Remat(RematDescriptor {
                op: recipe.op,
                inputs,
                result_repr: recipe.result_repr,
            });
            Ok((desc, false))
        }
    }
}

// ── Resume-state reconstruction (the round-trip inverse) ────────────────────

/// A read-only view of the trapped T2 machine frame, plus the runtime services
/// the deopt handler needs. Mocked in tests; backed by a captured transfer
/// snapshot in production (transfer_capture.rs).
pub trait MachineState {
    /// Read the raw machine word currently held at `loc` (register or spill).
    fn read(&self, loc: Location) -> u64;

    /// Heap-allocate a `double-float` and return its tagged `EgclVal`. Used to
    /// rebox an `UnboxedF64`. Default panics: only frames that
    /// carry a `ReboxF64` slot need to supply it.
    fn box_double(&self, _x: f64) -> EgclVal {
        panic!("MachineState::box_double required to rebox an UnboxedF64 slot")
    }
}

/// A reconstructed interpreter frame.
#[derive(Clone, PartialEq, Debug)]
pub struct ReconstructedFrame {
    pub function: u32,
    pub resume_pc: u32,
    /// Tagged value for each interpreter local (index = local number).
    pub locals: Vec<EgclVal>,
    /// Tagged value for each live operand-stack slot, bottom-to-top.
    pub stack: Vec<EgclVal>,
}

impl egcl_rt::gc::TraceHostRoots for ReconstructedFrame {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.locals.trace_host_roots(visit);
        self.stack.trace_host_roots(visit);
    }
}

/// Reconstruct the interpreter frame from lowered deopt metadata and a machine
/// state. This is the round-trip inverse of
/// [`lower_one`]: every descriptor `lower_source` produced is resolved back to a
/// tagged `EgclVal`, applying reboxing and replaying rematerialisation recipes.
///
/// * `InLocation` → read the location, rebox if needed;
/// * `MaterializeConst` → write the immediate directly;
/// * `Unbound` → UNBOUND-MARKER;
/// * `Remat` → replay the cold recipe.
///
/// The machine's tagged inputs must remain rooted and its `read` operation must
/// observe relocated values. Completed frames and the frame being constructed
/// are rooted here because `box_double` may collect. The caller must root the
/// returned frames before its next allocation.
pub fn reconstruct(lowered: &LoweredDeopt, mach: &impl MachineState) -> Vec<ReconstructedFrame> {
    let mut frames = Vec::with_capacity(lowered.scopes.len());
    egcl_rt::rooted_ref!(_completed_frames = &mut frames);
    for scope in &lowered.scopes {
        let mut frame = ReconstructedFrame {
            function: scope.function,
            resume_pc: scope.resume_pc,
            locals: Vec::with_capacity(scope.num_locals),
            stack: Vec::with_capacity(scope.slots.len().saturating_sub(scope.num_locals)),
        };
        {
            egcl_rt::rooted_ref!(_current_frame = &mut frame);
            for (index, slot) in scope.slots.iter().enumerate() {
                let value = eval_slot(slot, mach);
                if index < scope.num_locals {
                    frame.locals.push(value);
                } else {
                    frame.stack.push(value);
                }
            }
        }
        // No Lisp allocation between removing the local root and publishing
        // this frame in the already-rooted completed-frame vector.
        frames.push(frame);
    }
    frames
}

/// Resolve one lowered slot descriptor to its tagged interpreter value.
fn eval_slot(slot: &SlotDescriptor, mach: &impl MachineState) -> EgclVal {
    match slot {
        SlotDescriptor::InLocation(loc, rebox) => apply_rebox(*rebox, mach.read(*loc), mach),
        SlotDescriptor::MaterializeConst(k) => *k,
        SlotDescriptor::Unbound => UNBOUND,
        SlotDescriptor::Remat(recipe) => eval_remat(recipe, mach),
    }
}

/// Apply a rebox step to a raw machine word.
fn apply_rebox(rebox: Rebox, raw: u64, mach: &impl MachineState) -> EgclVal {
    match rebox {
        Rebox::None => EgclVal(raw),
        Rebox::ReboxFixnum => EgclVal::from_fixnum(raw as i64),
        Rebox::ReboxF32 => EgclVal::from_single_float(f32::from_bits(raw as u32)),
        Rebox::ReboxF64 => mach.box_double(f64::from_bits(raw)),
    }
}

/// Replay a rematerialisation recipe on the cold deopt path.
///
/// The interpreter's slots are always tagged, so this sketch evaluates the whole
/// recipe at the tagged `EgclVal` level: box/unbox ops are representation
/// no-ops here (a fixnum is a fixnum either way), and the arithmetic ops read
/// their operands as fixnums. That is semantically equivalent to the
/// unbox→compute→rebox chain the recipe encodes. A production implementation
/// would honour `result_repr` for intermediate unboxed temporaries; the enum
/// carries it so that refinement is a local change.
fn eval_remat(recipe: &RematDescriptor, mach: &impl MachineState) -> EgclVal {
    let arg = |i: usize| eval_slot(&recipe.inputs[i], mach);
    match recipe.op {
        // Pass-through / representation no-ops at the tagged level.
        RematOp::Const
        | RematOp::BoxFixnum
        | RematOp::UnboxFixnum
        | RematOp::BoxFloat
        | RematOp::UnboxFloat => arg(0),
        RematOp::FixnumAdd => EgclVal::from_fixnum(arg(0).as_fixnum() + arg(1).as_fixnum()),
        RematOp::FixnumSub => EgclVal::from_fixnum(arg(0).as_fixnum() - arg(1).as_fixnum()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameStateId;
    use crate::t2::frame_state::{FrameScope, FrameState, RematRecipe};
    use crate::t2::mach::{PhysReg, RegClass, StackMap, StackSlot};
    use std::collections::HashMap;

    fn gpr(n: u8) -> Location {
        Location::Register(PhysReg {
            class: RegClass::Gpr,
            encoding: n,
        })
    }

    /// A mock T2 frame: a location→raw-word table plus a trivial double boxer.
    /// (`Location` is not `Hash` in the frozen mach.rs, so a linear-scan `Vec`
    /// stands in for a map.)
    struct MockMachine {
        regs: Vec<(Location, u64)>,
    }
    impl MachineState for MockMachine {
        fn read(&self, loc: Location) -> u64 {
            self.regs
                .iter()
                .find(|(l, _)| *l == loc)
                .map(|(_, w)| *w)
                .expect("mock machine has no value for location")
        }
        fn box_double(&self, x: f64) -> EgclVal {
            // Stand-in for a real heap double-float; encodes the bits so the test
            // can observe the round trip deterministically.
            EgclVal(x.to_bits())
        }
    }

    /// Build the fixture FrameState: n (Tagged), i (UnboxedFixnum), a Const, and
    /// an Unbound local; empty operand stack. Returns (frame_state, v_n, v_i).
    fn fixture() -> (FrameState, Value, Value) {
        let v_n = Value(0);
        let v_i = Value(1);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: egcl_rt::symbols::intern("fsum"),
                bcp: 42,
                locals: vec![
                    ValueSource::Value {
                        value: v_n,
                        repr: ValueRepresentation::Tagged,
                    },
                    ValueSource::Value {
                        value: v_i,
                        repr: ValueRepresentation::UnboxedFixnum,
                    },
                    ValueSource::Const(EgclVal::from_fixnum(7)),
                    ValueSource::Unbound,
                ],
                stack: vec![],
            }],
            remat: vec![],
        };
        (fs, v_n, v_i)
    }

    // spec-covers: R4.65
    // FrameState lowers to the descriptor set that frame reconstruction consumes.
    #[test]
    fn each_slot_lowers_to_expected_descriptor() {
        let (fs, v_n, v_i) = fixture();
        let mut map = HashMap::new();
        map.insert(v_n, gpr(0));
        map.insert(v_i, gpr(1));
        let loc_of = move |v: Value| map.get(&v).copied();

        let mut mf = MachFunc::default();
        mf.stack_maps.push(StackMap {
            code_offset: 0x10,
            values: vec![],
            frame_state: Some(FrameStateId(0)),
        });

        let lowered = lower_frame_states(&mf, &[fs], loc_of).expect("lowers cleanly");
        assert_eq!(lowered.len(), 1);
        let d = &lowered[0];

        assert_eq!(d.code_offset, 0x10);
        assert_eq!(d.scopes.len(), 1);
        let scope = &d.scopes[0];
        assert_eq!(scope.resume_pc, 42);
        assert_eq!(scope.function, egcl_rt::symbols::intern("fsum"));
        assert_eq!(scope.num_locals, 4);

        // Tagged value → no rebox; UnboxedFixnum → ReboxFixnum; Const; Unbound.
        assert_eq!(
            scope.slots[0],
            SlotDescriptor::InLocation(gpr(0), Rebox::None)
        );
        assert_eq!(
            scope.slots[1],
            SlotDescriptor::InLocation(gpr(1), Rebox::ReboxFixnum)
        );
        assert_eq!(
            scope.slots[2],
            SlotDescriptor::MaterializeConst(EgclVal::from_fixnum(7))
        );
        assert_eq!(scope.slots[3], SlotDescriptor::Unbound);

        // Only the Tagged value is a GC root.
        assert_eq!(scope.live_ref_bitmap, vec![true, false, false, false]);
    }

    #[test]
    fn reconstruct_over_mock_machine_returns_expected_locals() {
        let (fs, v_n, v_i) = fixture();
        let mut map = HashMap::new();
        map.insert(v_n, gpr(0));
        map.insert(v_i, gpr(1));
        let loc_of = move |v: Value| map.get(&v).copied();

        let lowered = lower_one(0, &fs, &loc_of).expect("lowers cleanly");

        // Mock frame: gpr0 holds a tagged EgclVal (99); gpr1 holds a raw
        // unboxed fixnum (5) that must be reboxed.
        let mach = MockMachine {
            regs: vec![(gpr(0), EgclVal::from_fixnum(99).0), (gpr(1), 5u64)],
        };

        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(
            frame.locals,
            vec![
                EgclVal::from_fixnum(99), // Tagged, read through unchanged
                EgclVal::from_fixnum(5),  // UnboxedFixnum reboxed via n<<3
                EgclVal::from_fixnum(7),  // Const materialised
                UNBOUND,                   // Unbound-marker
            ]
        );
        assert!(frame.stack.is_empty());
    }

    #[test]
    fn f32_slot_reboxes_from_raw_bits() {
        let v = Value(0);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: egcl_rt::symbols::intern("g"),
                bcp: 3,
                locals: vec![ValueSource::Value {
                    value: v,
                    repr: ValueRepresentation::UnboxedF32,
                }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let loc_of = move |q: Value| (q == v).then_some(gpr(4));
        let lowered = lower_one(0, &fs, &loc_of).unwrap();
        assert_eq!(
            lowered.scopes[0].slots[0],
            SlotDescriptor::InLocation(gpr(4), Rebox::ReboxF32)
        );

        let mach = MockMachine {
            regs: vec![(gpr(4), 1.5f32.to_bits() as u64)],
        };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![EgclVal::from_single_float(1.5)]);
    }

    #[test]
    fn remat_recipe_lowers_and_replays() {
        // Slot value = FixnumAdd(Const 3, Const 4), i.e. a value DCE removed from
        // the fast path but reconstructible cold. Result 7.
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: egcl_rt::symbols::intern("h"),
                bcp: 0,
                locals: vec![ValueSource::Remat(RematRecipeId(0))],
                stack: vec![],
            }],
            remat: vec![RematRecipe {
                op: RematOp::FixnumAdd,
                inputs: vec![
                    ValueSource::Const(EgclVal::from_fixnum(3)),
                    ValueSource::Const(EgclVal::from_fixnum(4)),
                ],
                result_repr: ValueRepresentation::UnboxedFixnum,
            }],
        };
        let loc_of = |_: Value| None; // no SSA values referenced
        let lowered = lower_one(0, &fs, &loc_of).unwrap();

        match &lowered.scopes[0].slots[0] {
            SlotDescriptor::Remat(r) => {
                assert_eq!(r.op, RematOp::FixnumAdd);
                assert_eq!(r.inputs.len(), 2);
                assert_eq!(
                    r.inputs[0],
                    SlotDescriptor::MaterializeConst(EgclVal::from_fixnum(3))
                );
            }
            other => panic!("expected Remat, got {other:?}"),
        }
        // A remat slot is never a GC root on its own.
        assert_eq!(lowered.scopes[0].live_ref_bitmap, vec![false]);

        let mach = MockMachine { regs: vec![] };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![EgclVal::from_fixnum(7)]);
    }

    // spec-covers: R4.65
    // "a FrameState that cannot be fully lowered MUST fail compilation ... never
    // install partial metadata" — the unbound Value is a hard LowerError.
    #[test]
    fn unallocated_value_fails_lowering() {
        let v = Value(9);
        let fs = FrameState {
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
        };
        let loc_of = |_: Value| None;
        assert_eq!(lower_one(0, &fs, &loc_of), Err(LowerError::Unallocated(v)));
    }

    #[test]
    fn moving_reference_cannot_be_lowered_as_a_constant() {
        let moving = EgclVal(0x1001);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Const(moving)],
                stack: vec![],
            }],
            remat: vec![],
        };
        let loc_of = |_: Value| None;

        assert_eq!(
            lower_one(0, &fs, &loc_of),
            Err(LowerError::MovingConst(moving))
        );
    }

    #[test]
    fn stack_slots_follow_locals_and_split_correctly() {
        let vloc = Value(0);
        let vstk = Value(1);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 7,
                locals: vec![ValueSource::Value {
                    value: vloc,
                    repr: ValueRepresentation::Tagged,
                }],
                stack: vec![ValueSource::Value {
                    value: vstk,
                    repr: ValueRepresentation::UnboxedFixnum,
                }],
            }],
            remat: vec![],
        };
        let mut map = HashMap::new();
        map.insert(vloc, Location::Stack(StackSlot(2)));
        map.insert(vstk, gpr(1));
        let loc_of = move |q: Value| map.get(&q).copied();
        let lowered = lower_one(0, &fs, &loc_of).unwrap();
        assert_eq!(lowered.scopes[0].num_locals, 1);
        assert_eq!(lowered.scopes[0].slots.len(), 2);

        let mach = MockMachine {
            regs: vec![
                (Location::Stack(StackSlot(2)), EgclVal::from_fixnum(11).0),
                (gpr(1), 22u64),
            ],
        };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![EgclVal::from_fixnum(11)]);
        assert_eq!(frame.stack, vec![EgclVal::from_fixnum(22)]);
    }

    // spec-covers: R4.67
    // A FrameState is a non-empty stack of scopes, outermost first; inlining
    // prefixes caller scopes and reconstruction rebuilds every one in that order.
    #[test]
    fn lowers_and_reconstructs_every_inlined_scope_in_order() {
        let outer_value = Value(0);
        let inner_value = Value(1);
        let outer = egcl_rt::symbols::intern("DEOPT-OUTER");
        let inner = egcl_rt::symbols::intern("DEOPT-INNER");
        let fs = FrameState {
            scopes: vec![
                FrameScope {
                    function: outer,
                    bcp: 8,
                    locals: vec![ValueSource::Value {
                        value: outer_value,
                        repr: ValueRepresentation::Tagged,
                    }],
                    stack: vec![],
                },
                FrameScope {
                    function: inner,
                    bcp: 3,
                    locals: vec![ValueSource::Value {
                        value: inner_value,
                        repr: ValueRepresentation::UnboxedFixnum,
                    }],
                    stack: vec![ValueSource::Const(EgclVal::from_fixnum(9))],
                },
            ],
            remat: vec![],
        };
        let loc_of = |value| match value {
            v if v == outer_value => Some(gpr(0)),
            v if v == inner_value => Some(gpr(1)),
            _ => None,
        };
        let lowered = lower_one(0x44, &fs, &loc_of).unwrap();
        assert_eq!(lowered.scopes.len(), 2);
        assert_eq!(lowered.scopes[0].function, outer);
        assert_eq!(lowered.scopes[1].function, inner);

        let mach = MockMachine {
            regs: vec![(gpr(0), EgclVal::from_fixnum(5).0), (gpr(1), 7)],
        };
        let frames = reconstruct(&lowered, &mach);
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].function, frames[0].resume_pc), (outer, 8));
        assert_eq!(frames[0].locals, vec![EgclVal::from_fixnum(5)]);
        assert_eq!((frames[1].function, frames[1].resume_pc), (inner, 3));
        assert_eq!(frames[1].locals, vec![EgclVal::from_fixnum(7)]);
        assert_eq!(frames[1].stack, vec![EgclVal::from_fixnum(9)]);
    }
}
