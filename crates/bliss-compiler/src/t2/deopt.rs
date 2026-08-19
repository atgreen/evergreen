//! P7 — FrameState metadata lowering & deopt runtime (spec §4.10 A4.14, §4.6 A4.04).
//!
//! **Parcel P7. Owner: (sub-agent).** Lower each deoptimising instruction's
//! `FrameState` (frame_state.rs, frozen) to the compiled `StackMap` form
//! (mach.rs) once P6 has assigned locations: for each interpreter slot resolve
//! its `ValueSource` — `Value` → its allocated `Location` (+ rebox note if the
//! representation is unboxed), `Const` → materialise-immediate, `Unbound` →
//! unbound-marker, `Remat` → cold-path recipe — producing exactly the shape the
//! §4.6 A4.04 deopt handler consumes. Also sketch the resume-state reconstruction
//! (given a StackMap + machine state → interpreter locals/stack). The
//! `FrameScope.function` symbol is resolvable from the bytecode function name via
//! `bliss_rt::symbols`. Unit-test the lowering on a hand-built FrameState +
//! allocation, asserting each slot resolves to the expected descriptor.
//!
//! ## The `Value → Location` assumption (contract gap owed by P5/P6)
//!
//! A frozen `FrameState` names each live slot by its **SSA `Value`** (ir.rs),
//! but the register allocator's result (`MachFunc.allocation`, mach.rs) is a
//! `Vec<(VReg, Location)>` — a **`VReg → Location`** map. The missing link is
//! `Value → VReg`, which lowering (P5) establishes when it selects machine
//! instructions for each SSA value, and which nothing in the frozen surface yet
//! exposes. The full binding A4.14 needs is therefore the composition
//!
//! ```text
//!   Value  --(P5 lowering)-->  VReg  --(P6 regalloc)-->  Location
//! ```
//!
//! Until P5/P6 publish that composition, P7 takes it as an injected parameter:
//! `lower_frame_states` accepts a `loc_of: Fn(Value) -> Option<Location>`
//! closure. A real driver builds it from `mf.allocation` plus P5's value→vreg
//! side table; `None` means "no location for a deopt-live value", which per
//! R4.65 is a hard compile failure (`LowerError::Unallocated`) — the function
//! stays at T1, never ships partial metadata.
//!
//! ## Other contract gaps noted for downstream parcels
//!
//! * **No home for the lowered form in `mach::StackMap`.** `StackMap` carries a
//!   `frame_state: Option<FrameStateId>` but no field for the *resolved*
//!   descriptors. So `lower_frame_states` **returns** a `Vec<LoweredDeopt>`
//!   side table (one per deopt stack map) rather than writing back into
//!   `mf.stack_maps`. When mach.rs grows a `lowered: Option<LoweredDeopt>`
//!   field, the driver can store these back; the algorithm is unchanged.
//! * **F64 rebox needs a runtime double-float allocator.** A4.04 step 3 heap-
//!   allocates a `double-float` when reboxing an `UnboxedF64`. That allocator is
//!   a runtime service, not compiler state, so `reconstruct` obtains it through
//!   the `MachineState::box_double` hook (mocked in tests).
//! * **Const GC roots.** A4.14 only sets the live-ref bit for a `Value` in
//!   `Tagged` representation. A `Const` heap literal is also a GC root, but it is
//!   an immortal one interned in the function's constant pool (§4.3 `AuxData::
//!   HeapLiteral`) and rooted there, so it is intentionally not flagged here.

use crate::t2::frame_state::{FrameState, RematOp, RematRecipeId, ValueSource};
use crate::t2::ir::{Value, ValueRepresentation};
use crate::t2::mach::{Location, MachFunc};
use bliss_rt::value::{BlissVal, UNBOUND};

/// Maximum rematerialisation-recipe nesting depth we will lower before giving
/// up. A cyclic recipe (rejected by the verifier, R4.64) or a pathologically
/// deep one fails lowering rather than blowing the stack.
const REMAT_MAX_DEPTH: usize = 32;

// ── Lowered descriptor model (the A4.14 output shape) ───────────────────────

/// The rebox step A4.04 step 3 applies to a slot's raw machine word before it is
/// written into a tagged interpreter slot. Derived purely from the value's
/// [`ValueRepresentation`] (D4.16): only `Tagged` needs no rebox.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Rebox {
    /// Already a tagged `BlissVal`; write through unchanged.
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
    /// The rebox implied by a value's machine representation (A4.14 `box_kind`).
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
/// form of a [`ValueSource`] (spec §4.10 A4.14). This is exactly what the §4.6
/// A4.04 deopt handler consumes when reconstructing an interpreter frame.
#[derive(Clone, PartialEq, Debug)]
pub enum SlotDescriptor {
    /// The value lives in `Location`; apply `Rebox` to the raw word before
    /// writing the tagged interpreter slot (A4.04 step 3).
    InLocation(Location, Rebox),
    /// Materialise a compile-time-constant immediate (A4.04 writes it directly).
    MaterializeConst(BlissVal),
    /// Not live in T2 → the interpreter slot receives UNBOUND-MARKER (A4.04
    /// step 6). A later reference signals the correct CL error.
    Unbound,
    /// Recompute on the cold deopt path from a lowered recipe (A4.13). Recipe
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

/// The lowered deopt metadata for one trap site — the A4.14 `StackMapEntry`.
/// Corresponds one-to-one with a deopt [`crate::t2::mach::StackMap`].
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
    /// Bytecode PC T0 resumes at (`resume_pc`, §4.6 D4.10 / A4.04 step 1b).
    pub resume_pc: u32,
    /// Symbol id of the function whose interpreter frame is reconstructed
    /// (`FrameScope.function`; resolvable to a name via `bliss_rt::symbols`).
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

/// Why a FrameState could not be fully lowered. Per R4.65 any of these MUST fail
/// compilation (the function stays at T1); partial metadata is never installed.
#[derive(Clone, PartialEq, Debug)]
pub enum LowerError {
    /// A deopt-live `Value` had no allocated `Location` (the `loc_of` map
    /// returned `None`). Violates R4.60/R4.65.
    Unallocated(Value),
    /// A `Remat` source referenced a recipe index absent from the pool.
    MissingRecipe(RematRecipeId),
    /// Rematerialisation recipes nested past [`REMAT_MAX_DEPTH`] — treated as a
    /// (probable) cycle, which the verifier is required to reject (R4.64).
    RematTooDeep,
    /// A deopt `StackMap` named a `FrameStateId` out of range for the table.
    BadFrameStateId(u32),
}

// ── A4.14 — FrameState lowering ─────────────────────────────────────────────

/// Lower every deopt `StackMap` in `mf` to its resolved [`LoweredDeopt`] form
/// (spec §4.10 A4.14). Called after P6 has assigned locations.
///
/// `loc_of` resolves an SSA `Value` to the `Location` the allocator bound it to
/// — the `Value → VReg → Location` composition documented at the module top;
/// returning `None` is a hard failure (R4.65).
///
/// Returns one entry per deopt stack map (those whose `frame_state` is `Some`),
/// in stack-map order. See the module docs for why this is a returned side table
/// rather than an in-place write into `mf.stack_maps`.
///
/// Signature note: the parcel stub took `&mut MachFunc` and returned `()`; since
/// `mach::StackMap` has nowhere to store the lowered form (contract gap), P7
/// reads `mf` immutably, threads the `loc_of` binding P5/P6 owe, and returns the
/// metadata instead.
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

/// Lower a single FrameState against a code offset and location map (A4.14 body).
pub fn lower_one(
    code_offset: u32,
    fs: &FrameState,
    loc_of: &impl Fn(Value) -> Option<Location>,
) -> Result<LoweredDeopt, LowerError> {
    let mut scopes = Vec::with_capacity(fs.scopes.len());
    for scope in &fs.scopes {
        let mut slots = Vec::with_capacity(scope.locals.len() + scope.stack.len());
        let mut live_ref_bitmap = Vec::with_capacity(slots.capacity());
        for source in scope.locals.iter().chain(scope.stack.iter()) {
            let (desc, is_ref) = lower_source(source, fs, loc_of, 0)?;
            slots.push(desc);
            live_ref_bitmap.push(is_ref);
        }
        scopes.push(LoweredScope {
            resume_pc: scope.bcp,
            function: scope.function,
            slots,
            num_locals: scope.locals.len(),
            live_ref_bitmap,
        });
    }
    Ok(LoweredDeopt { code_offset, scopes })
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
            // Only a Tagged value may hold a GC pointer (D4.16); unboxed forms
            // are never roots.
            let is_ref = matches!(repr, ValueRepresentation::Tagged);
            Ok((SlotDescriptor::InLocation(loc, rebox), is_ref))
        }
        ValueSource::Const(k) => Ok((SlotDescriptor::MaterializeConst(*k), false)),
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

// ── A4.04 — resume-state reconstruction (the round-trip sketch) ──────────────

/// A read-only view of the trapped T2 machine frame, plus the runtime services
/// the deopt handler needs. Mirrors what A4.04 reads from `t2_frame`. Mocked in
/// tests; backed by the real register file / spill area + heap in production.
pub trait MachineState {
    /// Read the raw machine word currently held at `loc` (register or spill).
    fn read(&self, loc: Location) -> u64;

    /// Heap-allocate a `double-float` and return its tagged `BlissVal`. Used to
    /// rebox an `UnboxedF64` (A4.04 step 3). Default panics: only frames that
    /// carry a `ReboxF64` slot need to supply it.
    fn box_double(&self, _x: f64) -> BlissVal {
        panic!("MachineState::box_double required to rebox an UnboxedF64 slot")
    }
}

/// A reconstructed interpreter frame — the output of A4.04 steps 3 & 6.
#[derive(Clone, PartialEq, Debug)]
pub struct ReconstructedFrame {
    pub function: u32,
    pub resume_pc: u32,
    /// Tagged value for each interpreter local (index = local number).
    pub locals: Vec<BlissVal>,
    /// Tagged value for each live operand-stack slot, bottom-to-top.
    pub stack: Vec<BlissVal>,
}

/// Reconstruct the interpreter frame from lowered deopt metadata and a machine
/// state (spec §4.6 A4.04 steps 3 & 6). This is the round-trip inverse of
/// [`lower_one`]: every descriptor `lower_source` produced is resolved back to a
/// tagged `BlissVal`, applying reboxing and replaying rematerialisation recipes.
///
/// Mirrors A4.04:
/// * `InLocation` → read the location, box if `needs_boxing` (step 3);
/// * `MaterializeConst` → write the immediate directly;
/// * `Unbound` → UNBOUND-MARKER (step 6);
/// * `Remat` → replay the cold recipe (A4.13).
pub fn reconstruct(lowered: &LoweredDeopt, mach: &impl MachineState) -> Vec<ReconstructedFrame> {
    lowered
        .scopes
        .iter()
        .map(|scope| {
            let mut values: Vec<BlissVal> = scope
                .slots
                .iter()
                .map(|slot| eval_slot(slot, mach))
                .collect();
            let stack = values.split_off(scope.num_locals);
            ReconstructedFrame {
                function: scope.function,
                resume_pc: scope.resume_pc,
                locals: values,
                stack,
            }
        })
        .collect()
}

/// Resolve one lowered slot descriptor to its tagged interpreter value.
fn eval_slot(slot: &SlotDescriptor, mach: &impl MachineState) -> BlissVal {
    match slot {
        SlotDescriptor::InLocation(loc, rebox) => apply_rebox(*rebox, mach.read(*loc), mach),
        SlotDescriptor::MaterializeConst(k) => *k,
        SlotDescriptor::Unbound => UNBOUND,
        SlotDescriptor::Remat(recipe) => eval_remat(recipe, mach),
    }
}

/// Apply a rebox step to a raw machine word (A4.04 step 3 `box_value`).
fn apply_rebox(rebox: Rebox, raw: u64, mach: &impl MachineState) -> BlissVal {
    match rebox {
        Rebox::None => BlissVal(raw),
        Rebox::ReboxFixnum => BlissVal::from_fixnum(raw as i64),
        Rebox::ReboxF32 => BlissVal::from_single_float(f32::from_bits(raw as u32)),
        Rebox::ReboxF64 => mach.box_double(f64::from_bits(raw)),
    }
}

/// Replay a rematerialisation recipe on the cold deopt path (A4.13).
///
/// The interpreter's slots are always tagged, so this sketch evaluates the whole
/// recipe at the tagged `BlissVal` level: box/unbox ops are representation
/// no-ops here (a fixnum is a fixnum either way), and the arithmetic ops read
/// their operands as fixnums. That is semantically equivalent to the
/// unbox→compute→rebox chain the recipe encodes. A production implementation
/// would honour `result_repr` for intermediate unboxed temporaries; the enum
/// carries it so that refinement is a local change.
fn eval_remat(recipe: &RematDescriptor, mach: &impl MachineState) -> BlissVal {
    let arg = |i: usize| eval_slot(&recipe.inputs[i], mach);
    match recipe.op {
        // Pass-through / representation no-ops at the tagged level.
        RematOp::Const
        | RematOp::BoxFixnum
        | RematOp::UnboxFixnum
        | RematOp::BoxFloat
        | RematOp::UnboxFloat => arg(0),
        RematOp::FixnumAdd => BlissVal::from_fixnum(arg(0).as_fixnum() + arg(1).as_fixnum()),
        RematOp::FixnumSub => BlissVal::from_fixnum(arg(0).as_fixnum() - arg(1).as_fixnum()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, RematRecipe};
    use crate::t2::mach::{PhysReg, RegClass, StackMap, StackSlot};
    use crate::t2::frame_state::FrameStateId;
    use std::collections::HashMap;

    fn gpr(n: u8) -> Location {
        Location::Register(PhysReg { class: RegClass::Gpr, encoding: n })
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
        fn box_double(&self, x: f64) -> BlissVal {
            // Stand-in for a real heap double-float; encodes the bits so the test
            // can observe the round trip deterministically.
            BlissVal(x.to_bits())
        }
    }

    /// Build the fixture FrameState: n (Tagged), i (UnboxedFixnum), a Const, and
    /// an Unbound local; empty operand stack. Returns (frame_state, v_n, v_i).
    fn fixture() -> (FrameState, Value, Value) {
        let v_n = Value(0);
        let v_i = Value(1);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: bliss_rt::symbols::intern("fsum"),
                bcp: 42,
                locals: vec![
                    ValueSource::Value { value: v_n, repr: ValueRepresentation::Tagged },
                    ValueSource::Value { value: v_i, repr: ValueRepresentation::UnboxedFixnum },
                    ValueSource::Const(BlissVal::from_fixnum(7)),
                    ValueSource::Unbound,
                ],
                stack: vec![],
            }],
            remat: vec![],
        };
        (fs, v_n, v_i)
    }

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
            live_refs: vec![],
            frame_state: Some(FrameStateId(0)),
        });

        let lowered = lower_frame_states(&mf, &[fs], loc_of).expect("lowers cleanly");
        assert_eq!(lowered.len(), 1);
        let d = &lowered[0];

        assert_eq!(d.code_offset, 0x10);
        assert_eq!(d.scopes.len(), 1);
        let scope = &d.scopes[0];
        assert_eq!(scope.resume_pc, 42);
        assert_eq!(scope.function, bliss_rt::symbols::intern("fsum"));
        assert_eq!(scope.num_locals, 4);

        // Tagged value → no rebox; UnboxedFixnum → ReboxFixnum; Const; Unbound.
        assert_eq!(scope.slots[0], SlotDescriptor::InLocation(gpr(0), Rebox::None));
        assert_eq!(scope.slots[1], SlotDescriptor::InLocation(gpr(1), Rebox::ReboxFixnum));
        assert_eq!(scope.slots[2], SlotDescriptor::MaterializeConst(BlissVal::from_fixnum(7)));
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

        // Mock frame: gpr0 holds a tagged BlissVal (99); gpr1 holds a raw
        // unboxed fixnum (5) that must be reboxed.
        let mach = MockMachine {
            regs: vec![(gpr(0), BlissVal::from_fixnum(99).0), (gpr(1), 5u64)],
        };

        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(
            frame.locals,
            vec![
                BlissVal::from_fixnum(99), // Tagged, read through unchanged
                BlissVal::from_fixnum(5),  // UnboxedFixnum reboxed via n<<3
                BlissVal::from_fixnum(7),  // Const materialised
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
                function: bliss_rt::symbols::intern("g"),
                bcp: 3,
                locals: vec![ValueSource::Value { value: v, repr: ValueRepresentation::UnboxedF32 }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let loc_of = move |q: Value| (q == v).then_some(gpr(4));
        let lowered = lower_one(0, &fs, &loc_of).unwrap();
        assert_eq!(lowered.scopes[0].slots[0], SlotDescriptor::InLocation(gpr(4), Rebox::ReboxF32));

        let mach = MockMachine { regs: vec![(gpr(4), 1.5f32.to_bits() as u64)] };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![BlissVal::from_single_float(1.5)]);
    }

    #[test]
    fn remat_recipe_lowers_and_replays() {
        // Slot value = FixnumAdd(Const 3, Const 4), i.e. a value DCE removed from
        // the fast path but reconstructible cold. Result 7.
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: bliss_rt::symbols::intern("h"),
                bcp: 0,
                locals: vec![ValueSource::Remat(RematRecipeId(0))],
                stack: vec![],
            }],
            remat: vec![RematRecipe {
                op: RematOp::FixnumAdd,
                inputs: vec![
                    ValueSource::Const(BlissVal::from_fixnum(3)),
                    ValueSource::Const(BlissVal::from_fixnum(4)),
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
                assert_eq!(r.inputs[0], SlotDescriptor::MaterializeConst(BlissVal::from_fixnum(3)));
            }
            other => panic!("expected Remat, got {other:?}"),
        }
        // A remat slot is never a GC root on its own.
        assert_eq!(lowered.scopes[0].live_ref_bitmap, vec![false]);

        let mach = MockMachine { regs: vec![] };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![BlissVal::from_fixnum(7)]);
    }

    #[test]
    fn unallocated_value_fails_lowering() {
        let v = Value(9);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value { value: v, repr: ValueRepresentation::Tagged }],
                stack: vec![],
            }],
            remat: vec![],
        };
        let loc_of = |_: Value| None;
        assert_eq!(lower_one(0, &fs, &loc_of), Err(LowerError::Unallocated(v)));
    }

    #[test]
    fn stack_slots_follow_locals_and_split_correctly() {
        let vloc = Value(0);
        let vstk = Value(1);
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 7,
                locals: vec![ValueSource::Value { value: vloc, repr: ValueRepresentation::Tagged }],
                stack: vec![ValueSource::Value { value: vstk, repr: ValueRepresentation::UnboxedFixnum }],
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
                (Location::Stack(StackSlot(2)), BlissVal::from_fixnum(11).0),
                (gpr(1), 22u64),
            ],
        };
        let frames = reconstruct(&lowered, &mach);
        let frame = &frames[0];
        assert_eq!(frame.locals, vec![BlissVal::from_fixnum(11)]);
        assert_eq!(frame.stack, vec![BlissVal::from_fixnum(22)]);
    }

    #[test]
    fn lowers_and_reconstructs_every_inlined_scope_in_order() {
        let outer_value = Value(0);
        let inner_value = Value(1);
        let outer = bliss_rt::symbols::intern("DEOPT-OUTER");
        let inner = bliss_rt::symbols::intern("DEOPT-INNER");
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
                    stack: vec![ValueSource::Const(BlissVal::from_fixnum(9))],
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
            regs: vec![
                (gpr(0), BlissVal::from_fixnum(5).0),
                (gpr(1), 7),
            ],
        };
        let frames = reconstruct(&lowered, &mach);
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].function, frames[0].resume_pc), (outer, 8));
        assert_eq!(frames[0].locals, vec![BlissVal::from_fixnum(5)]);
        assert_eq!((frames[1].function, frames[1].resume_pc), (inner, 3));
        assert_eq!(frames[1].locals, vec![BlissVal::from_fixnum(7)]);
        assert_eq!(frames[1].stack, vec![BlissVal::from_fixnum(9)]);
    }
}
