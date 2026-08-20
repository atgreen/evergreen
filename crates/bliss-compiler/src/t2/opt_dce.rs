//! P4c — Deopt-aware dead-code elimination + rematerialisation (spec §4.5 R4.35,
//! §4.10 A4.13, R4.60/R4.62).
//!
//! **Parcel P4c. Owner: (sub-agent).** Mark liveness from NON-deopt uses only
//! (real fast-path / effect uses); FrameState `ValueSource::Value` uses do NOT
//! seed liveness. Then for each unmarked value: delete if it has no FrameState
//! uses; else if it is rematerialisable (pure/total/cheap — see
//! `frame_state::RematOp`) build a `RematRecipe`, replace its FrameState uses
//! with `ValueSource::Remat`, and remove it from the fast path; else keep it
//! (deopt-live). Never break the preservation invariant (spec §4.10 R4.60).
//!
//! ## Approach
//!
//! *Mark.* Liveness is a boolean vector over `Value`s. Roots are the operands of
//! every **pinned** instruction (effectful / guard / call / safepoint /
//! terminator) plus every terminator successor-edge (`BlockCall.args`). Liveness
//! propagates backward through instruction operands to a fixpoint. FrameState
//! `ValueSource::Value` references are deliberately *not* roots — that is the
//! whole point of A4.13.
//!
//! *Deopt-live promotion.* A FrameState value whose defining instruction is
//! removable but whose opcode is **not** rematerialisable (or whose remat recipe
//! cannot be fully resolved) is genuinely deopt-live: it is promoted to a
//! liveness root and re-propagated to a fixpoint, so it (and its operands) stay
//! on the fast path.
//!
//! *Sweep / rematerialise.* Each surviving-but-removable FrameState value source
//! is replaced by a `ValueSource::Remat` whose recipe is built by `resolve`,
//! recursing through operands so pure chains (const → add → box) compose into a
//! single cold-path recipe. Recipes are appended to the owning FrameState's
//! `remat` pool.
//!
//! *Deletion.* The Phase-0 IR is a frozen contract with **no instruction-removal
//! API**. We model deletion by rebuilding each block's public `insts` list
//! (`BlockData::insts`) without the dead instructions, via the public
//! `block`/`block_mut` accessors. The dead `InstData`/`ValueData` remain in the
//! arenas (unreferenced) — the arenas are append-only by contract — but no block
//! lists them, so they never lower. An instruction is deleted iff it is not
//! pinned, has no live result, and has no surviving FrameState reference (the
//! last clause is belt-and-braces against orphaning a slot, R4.60).
//!
//! ## Contract gaps worked around
//!
//! - **No instruction-removal helper** on `Function`: modelled by rebuilding
//!   `BlockData::insts` (see above). A future `Function::remove_inst` /
//!   dead-value compaction would let us reclaim the arenas.
//! - `RematRecipeId` is scoped to a single FrameState's `remat` vec, so a value
//!   named by two FrameStates gets an independent recipe built into each. That
//!   matches the data model (cold-path code is per-deopt-point) and needs no new
//!   API, but there is no cross-FrameState recipe sharing.
//! - `FrameStateTable` exposes `iter`/`get_mut`/`len` but no borrow that lets us
//!   read the IR (`&Function`) while mutating the table, so we `std::mem::take`
//!   the table out of the function for the rewrite and put it back.

use crate::t2::frame_state::{
    FrameState, FrameStateId, RematOp, RematRecipe, RematRecipeId, ValueSource,
};
use crate::t2::ir::{Function, Inst, InstData, Opcode, Value, ValueDef};
use crate::t2::pass::{Analyses, Pass};

#[derive(Default)]
pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        let nv = f.num_values();
        let mut live = vec![false; nv];
        let mut work: Vec<Value> = Vec::new();

        // ── MARK: seed from real (non-deopt) uses only ──
        // Operands of every pinned instruction, and every terminator edge's args.
        for i in 0..f.num_insts() {
            let d = f.inst(Inst(i as u32));
            if is_pinned(d) {
                for &v in &d.args {
                    work.push(v);
                }
                for t in &d.targets {
                    for &v in &t.args {
                        work.push(v);
                    }
                }
            }
        }
        propagate(f, &mut live, &mut work);

        // Pull the frame-state table out so we can read the IR immutably while we
        // rewrite deopt metadata (contract gap: no combined borrow available).
        let mut fst = std::mem::take(&mut f.frame_states);
        let mut visiting: Vec<Value> = Vec::new();

        // ── Promote genuinely deopt-live values ──
        // A FrameState value whose recipe cannot be fully resolved (opcode not
        // rematerialisable, or an operand is itself unresolvable) must stay on the
        // fast path. Seed it as live and re-propagate until nothing new promotes.
        loop {
            let mut roots: Vec<Value> = Vec::new();
            for (_id, fs) in fst.iter() {
                for src in frame_value_refs(fs) {
                    if let ValueSource::Value { value, .. } = src {
                        let v = *value;
                        if !live[v.index()] && is_removable(f, &live, v) {
                            visiting.clear();
                            // Dry-run resolve into a throwaway pool.
                            if resolve(f, &live, v, &mut Vec::new(), &mut visiting).is_none() {
                                roots.push(v);
                            }
                        }
                    }
                }
            }
            let mut promoted = false;
            for v in roots {
                if !live[v.index()] {
                    work.push(v);
                    promoted = true;
                }
            }
            if promoted {
                propagate(f, &mut live, &mut work);
            } else {
                break;
            }
        }

        // ── SWEEP / rematerialise: rewrite removable FrameState value sources ──
        for id in 0..fst.len() {
            // Destructure &mut FrameState into disjoint field borrows so we can
            // push recipes into `remat` while walking `scopes`.
            let FrameState { scopes, remat } = fst.get_mut(FrameStateId(id as u32));
            for scope in scopes.iter_mut() {
                for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                    if let ValueSource::Value { value, .. } = src {
                        let v = *value;
                        if is_removable(f, &live, v) {
                            visiting.clear();
                            if let Some(new) = resolve(f, &live, v, remat, &mut visiting) {
                                *src = new;
                            }
                        }
                    }
                }
            }
        }

        // Any value still named by a FrameState (directly, or transitively inside
        // a Remat recipe) must not be deleted — final guard against orphaning.
        let mut still_ref = vec![false; nv];
        for (_id, fs) in fst.iter() {
            collect_value_refs(fs, &mut still_ref);
        }

        f.frame_states = fst;

        // ── Delete: rebuild each block's inst list without the dead ones ──
        let deleted: Vec<bool> = (0..f.num_insts())
            .map(|i| {
                let d = f.inst(Inst(i as u32));
                !is_pinned(d)
                    && !d.results.iter().any(|r| live[r.index()])
                    && !d.results.iter().any(|r| still_ref[r.index()])
            })
            .collect();

        let mut removed_any = false;
        for b in f.block_order().to_vec() {
            let before = f.block(b).insts.len();
            let kept: Vec<Inst> = f
                .block(b)
                .insts
                .iter()
                .copied()
                .filter(|i| !deleted[i.index()])
                .collect();
            if kept.len() != before {
                removed_any = true;
                f.block_mut(b).insts = kept;
            }
        }

        if removed_any {
            // We removed value definitions; drop cached analyses (spec §4.5 Pass).
            a.invalidate();
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Instructions the optimiser must never remove and whose operands are real
/// fast-path uses (spec §4.5 R4.35 "proven side-effect-free" is the negation).
fn is_pinned(d: &InstData) -> bool {
    d.flags.effectful || d.flags.guard || d.flags.call || d.flags.safepoint || d.flags.terminator
}

/// The instruction that defines `v`, or `None` for a block parameter (which has
/// no defining instruction and can never be deleted).
fn def_inst(f: &Function, v: Value) -> Option<Inst> {
    match f.value(v).def {
        ValueDef::Result { inst, .. } => Some(inst),
        ValueDef::Param { .. } => None,
    }
}

/// Does the instruction defining `v` survive on the fast path? A value survives
/// if it is a block parameter, its instruction is pinned, or any of its results
/// is marked live.
fn inst_survives_of(f: &Function, live: &[bool], inst: Inst) -> bool {
    let d = f.inst(inst);
    is_pinned(d) || d.results.iter().any(|r| live[r.index()])
}

/// A value is *removable* from the fast path if it is defined by a
/// non-surviving instruction. Block parameters and live/pinned results are not.
fn is_removable(f: &Function, live: &[bool], v: Value) -> bool {
    match def_inst(f, v) {
        Some(inst) => !inst_survives_of(f, live, inst),
        None => false,
    }
}

/// Map a fast-path opcode to the pure/total/cheap `RematOp` it replays as, or
/// `None` if the opcode is not rematerialisable (spec D4.17 eligibility).
fn remat_op(op: Opcode) -> Option<RematOp> {
    use Opcode::*;
    Some(match op {
        ConstFixnum | ConstFloat | ConstChar | ConstSymbol | ConstNil | ConstT | ConstHeapObj => {
            RematOp::Const
        }
        BoxFixnum => RematOp::BoxFixnum,
        UnboxFixnum => RematOp::UnboxFixnum,
        BoxFloat => RematOp::BoxFloat,
        UnboxFloat => RematOp::UnboxFloat,
        FixnumAdd => RematOp::FixnumAdd,
        FixnumSub => RematOp::FixnumSub,
        _ => return None,
    })
}

/// Resolve `v` to a `ValueSource` usable inside a FrameState / a recipe input.
///
/// - A surviving value (param, pinned, or live result) → `Value { v, repr }`.
/// - A removable value whose opcode is rematerialisable and all of whose
///   operands resolve → a fresh `Remat` recipe appended to `remat` (recursing on
///   operands, so pure chains compose).
/// - Otherwise `None` (not rematerialisable; the caller keeps it deopt-live).
///
/// `visiting` guards against a cyclic recipe (malformed IR, spec R4.64).
fn resolve(
    f: &Function,
    live: &[bool],
    v: Value,
    remat: &mut Vec<RematRecipe>,
    visiting: &mut Vec<Value>,
) -> Option<ValueSource> {
    let repr = f.value(v).repr;
    let inst = match def_inst(f, v) {
        None => return Some(ValueSource::Value { value: v, repr }),
        Some(i) => i,
    };
    if inst_survives_of(f, live, inst) {
        return Some(ValueSource::Value { value: v, repr });
    }
    // Removable: must be rematerialised or it cannot be a FrameState source.
    if visiting.contains(&v) {
        return None; // cyclic — refuse (R4.64)
    }
    // Constants already have the exact tagged representation a FrameState
    // needs. Preserve that payload directly; a zero-input `RematOp::Const`
    // recipe cannot reconstruct which constant it represented.
    {
        use crate::t2::ir::AuxData;
        use bliss_rt::value::{BlissVal, NIL, T};
        let data = f.inst(inst);
        let constant = match (data.opcode, &data.aux) {
            (Opcode::ConstFixnum, AuxData::FixnumImm(n)) => Some(BlissVal::from_fixnum(*n)),
            (Opcode::ConstFloat, AuxData::FloatImm(x)) => Some(BlissVal::from_single_float(*x)),
            (Opcode::ConstChar, AuxData::CharImm(c)) => Some(BlissVal::from_char(*c)),
            (Opcode::ConstSymbol, AuxData::SymbolRef(sym)) => {
                Some(BlissVal::from_symbol_index(*sym))
            }
            (Opcode::ConstHeapObj, AuxData::HeapLiteral(value)) => Some(*value),
            (Opcode::ConstNil, _) => Some(NIL),
            (Opcode::ConstT, _) => Some(T),
            _ => None,
        };
        if let Some(value) = constant {
            return Some(ValueSource::Const(value));
        }
    }
    let op = remat_op(f.inst(inst).opcode)?;
    visiting.push(v);
    let args = f.inst(inst).args.clone();
    let mut inputs = Vec::with_capacity(args.len());
    for arg in args {
        match resolve(f, live, arg, remat, visiting) {
            Some(s) => inputs.push(s),
            None => {
                visiting.pop();
                return None;
            }
        }
    }
    visiting.pop();
    let id = RematRecipeId(remat.len() as u32);
    remat.push(RematRecipe {
        op,
        inputs,
        result_repr: repr,
    });
    Some(ValueSource::Remat(id))
}

/// All `ValueSource`s named directly by a FrameState's locals + stack, across
/// all scopes (does not descend into recipes; used for candidate discovery).
fn frame_value_refs(fs: &FrameState) -> impl Iterator<Item = &ValueSource> {
    fs.scopes
        .iter()
        .flat_map(|s| s.locals.iter().chain(s.stack.iter()))
}

/// Mark every SSA `Value` still referenced by `fs` — directly or transitively
/// through a `Remat` recipe's inputs — so deletion never orphans it (R4.60).
fn collect_value_refs(fs: &FrameState, out: &mut [bool]) {
    fn mark(src: &ValueSource, fs: &FrameState, out: &mut [bool]) {
        match src {
            ValueSource::Value { value, .. } => out[value.index()] = true,
            ValueSource::Remat(id) => {
                for inp in &fs.remat[id.0 as usize].inputs {
                    mark(inp, fs, out);
                }
            }
            ValueSource::Const(_) | ValueSource::Unbound => {}
        }
    }
    for src in frame_value_refs(fs) {
        mark(src, fs, out);
    }
}

/// Drain `work`, marking each popped value live and pushing the operands of its
/// defining instruction (real backward liveness).
fn propagate(f: &Function, live: &mut [bool], work: &mut Vec<Value>) {
    while let Some(v) = work.pop() {
        if live[v.index()] {
            continue;
        }
        live[v.index()] = true;
        if let ValueDef::Result { inst, .. } = f.value(v).def {
            let d = f.inst(inst);
            for &operand in &d.args {
                work.push(operand);
            }
            for t in &d.targets {
                for &operand in &t.args {
                    work.push(operand);
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::FrameScope;
    use crate::t2::ir::{
        AuxData, IRType, InstData, InstFlags, TypeBits, ValueRepresentation as VR,
    };

    fn fixnum() -> IRType {
        IRType::of(TypeBits::FIXNUM)
    }

    fn inst(opcode: Opcode, args: Vec<Value>, flags: InstFlags) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags,
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    fn ret(args: Vec<Value>) -> InstData {
        inst(Opcode::Return, args, InstFlags::default())
    }

    /// A one-scope FrameState whose locals are the given sources.
    fn frame(locals: Vec<ValueSource>) -> FrameState {
        FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals,
                stack: vec![],
            }],
            remat: vec![],
        }
    }

    fn run(f: &mut Function) {
        Dce::default().run(f, &mut Analyses::new());
    }

    /// A constant used only by a FrameState is preserved as its exact tagged
    /// payload; a zero-input recipe cannot encode which constant it was.
    #[test]
    fn deopt_only_pure_value_is_rematerialised() {
        let mut f = Function::new("remat");
        let entry = f.entry();
        let mut constant = inst(Opcode::ConstFixnum, vec![], InstFlags::default());
        constant.aux = AuxData::FixnumImm(7);
        let (cinst, cres) = f.push_inst(
            entry,
            constant,
            &[(fixnum(), VR::Tagged)],
        );
        let c = cres[0];
        f.set_terminator(entry, ret(vec![])); // c has NO real use
        let fsid = f.frame_states.add(frame(vec![ValueSource::Value {
            value: c,
            repr: VR::Tagged,
        }]));

        run(&mut f);

        // Fast-path const instruction removed from the block.
        assert!(!f.block(entry).insts.contains(&cinst));
        // FrameState slot carries the exact constant; no ambiguous recipe exists.
        let fs = f.frame_states.get(fsid);
        assert!(matches!(
            fs.scopes[0].locals[0],
            ValueSource::Const(value)
                if value == bliss_rt::value::BlissVal::from_fixnum(7)
        ));
        assert!(fs.remat.is_empty());
    }

    /// A value with a real (fast-path) use survives DCE untouched.
    #[test]
    fn value_with_real_use_survives() {
        let mut f = Function::new("live");
        let entry = f.entry();
        let a = f.add_block_param(entry, fixnum(), VR::Tagged);
        let b = f.add_block_param(entry, fixnum(), VR::Tagged);
        let (add_inst, add_res) = f.push_inst(
            entry,
            inst(Opcode::FixnumAdd, vec![a, b], InstFlags::default()),
            &[(fixnum(), VR::Tagged)],
        );
        let s = add_res[0];
        f.set_terminator(entry, ret(vec![s])); // real use in the return

        run(&mut f);

        assert!(f.block(entry).insts.contains(&add_inst));
    }

    /// An effectful instruction is never removed, even with no uses at all.
    #[test]
    fn effectful_instruction_is_never_removed() {
        let mut f = Function::new("effect");
        let entry = f.entry();
        let a = f.add_block_param(entry, fixnum(), VR::Tagged);
        let mut fl = InstFlags::default();
        fl.effectful = true;
        let (store_inst, _) = f.push_inst(entry, inst(Opcode::Store, vec![a], fl), &[]);
        f.set_terminator(entry, ret(vec![]));

        run(&mut f);

        assert!(f.block(entry).insts.contains(&store_inst));
    }

    /// A non-rematerialisable value used only by a FrameState stays deopt-live:
    /// the instruction is kept and the FrameState slot stays a `Value` source.
    #[test]
    fn non_rematerialisable_deopt_only_value_stays_live() {
        let mut f = Function::new("deoptlive");
        let entry = f.entry();
        let a = f.add_block_param(entry, fixnum(), VR::Tagged);
        let b = f.add_block_param(entry, fixnum(), VR::Tagged);
        // FixnumMul is pure but NOT in the rematerialisable set.
        let (mul_inst, mul_res) = f.push_inst(
            entry,
            inst(Opcode::FixnumMul, vec![a, b], InstFlags::default()),
            &[(fixnum(), VR::Tagged)],
        );
        let m = mul_res[0];
        f.set_terminator(entry, ret(vec![])); // no real use
        let fsid = f.frame_states.add(frame(vec![ValueSource::Value {
            value: m,
            repr: VR::Tagged,
        }]));

        run(&mut f);

        // Kept on the fast path because it cannot be rematerialised.
        assert!(f.block(entry).insts.contains(&mul_inst));
        let fs = f.frame_states.get(fsid);
        assert!(matches!(
            fs.scopes[0].locals[0],
            ValueSource::Value { .. }
        ));
        assert!(fs.remat.is_empty());
    }

    /// Rematerialisation composes: `(+ const a)` used only by a FrameState
    /// collapses into a nested recipe (Const feeding a FixnumAdd), and both
    /// fast-path instructions disappear.
    #[test]
    fn rematerialisation_composes_through_a_pure_chain() {
        let mut f = Function::new("compose");
        let entry = f.entry();
        let a = f.add_block_param(entry, fixnum(), VR::Tagged);
        let mut constant = inst(Opcode::ConstFixnum, vec![], InstFlags::default());
        constant.aux = AuxData::FixnumImm(11);
        let (cinst, cres) = f.push_inst(
            entry,
            constant,
            &[(fixnum(), VR::Tagged)],
        );
        let c = cres[0];
        let (add_inst, add_res) = f.push_inst(
            entry,
            inst(Opcode::FixnumAdd, vec![c, a], InstFlags::default()),
            &[(fixnum(), VR::Tagged)],
        );
        let s = add_res[0];
        f.set_terminator(entry, ret(vec![])); // s used only by the FrameState
        let fsid = f.frame_states.add(frame(vec![ValueSource::Value {
            value: s,
            repr: VR::Tagged,
        }]));

        run(&mut f);

        // Both pure instructions removed from the fast path.
        assert!(!f.block(entry).insts.contains(&cinst));
        assert!(!f.block(entry).insts.contains(&add_inst));

        let fs = f.frame_states.get(fsid);
        // Top slot is the FixnumAdd recipe.
        let top = match &fs.scopes[0].locals[0] {
            ValueSource::Remat(id) => &fs.remat[id.0 as usize],
            other => panic!("expected Remat, got {other:?}"),
        };
        assert_eq!(top.op, RematOp::FixnumAdd);
        assert_eq!(top.inputs.len(), 2);
        // Input 0 is the exact tagged constant; input 1 is the surviving param `a`.
        assert!(matches!(
            top.inputs[0],
            ValueSource::Const(value)
                if value == bliss_rt::value::BlissVal::from_fixnum(11)
        ));
        match top.inputs[1] {
            ValueSource::Value { value, .. } => assert_eq!(value, a),
            ref other => panic!("expected Value(a), got {other:?}"),
        }
    }
}
