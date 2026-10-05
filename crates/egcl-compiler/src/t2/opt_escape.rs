// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Escape analysis and scalar replacement of non-escaping conses.
//!
//! # Purpose
//!
//! Two things live here. [`analyse`] is a pure intraprocedural analysis that
//! classifies every `Alloc` / `AllocCons` site as [`EscapeState::NoEscape`]
//! or [`EscapeState::GlobalEscape`] and exposes the answer through
//! [`EscapeResult`]; this is the substrate a stack-allocation lowering would
//! consume, and no such lowering exists yet. [`EscapeAnalysis`] is the `Pass`
//! that applies the one transform currently implemented on top of it: scalar
//! replacement of a non-escaping `AllocCons` whose only uses are `Car`/`Cdr`
//! loads.
//!
//! Not in the production T2 pipeline today; exercised by the integration
//! tests.
//!
//! # Contract
//!
//! **Operand conventions assumed:** `AllocCons.args == [car, cdr]`,
//! `Car.args == [cons]`, `Cdr.args == [cons]`, and mutations
//! `== [object, value, …]`. The constants `CAR_ARG` / `CDR_ARG` and the
//! position predicates in `use_keeps_local` are the single place to adjust if
//! the builder's layout changes.
//!
//! **Analysis output:** one `EscapeState` per allocation result value. A
//! value that is not an allocation site is untracked (`escape_state` returns
//! `None`; `escapes` returns `false`).
//!
//! **Transform output:** for each replaced cons, every `Car` result is
//! substituted by the cons's car operand and every `Cdr` result by its cdr
//! operand, across instruction operands, terminator edge arguments, and
//! FrameState value sources (including remat-recipe inputs). The `AllocCons`
//! and the `Car`/`Cdr` instructions are removed by rebuilding block `insts`
//! lists (the IR has no removal API; the arena entries stay, unreferenced).
//! `Analyses::invalidate` is called when anything changed.
//!
//! # Escape criterion
//!
//! An allocation escapes when its result is used in any way not on the
//! allow-list in `use_keeps_local`:
//!
//! * returned, tail-called, thrown or non-locally transferred;
//! * stored **as the value** into another object (`Store`, `SetCar`, `SetCdr`,
//!   `VecSet`, `WriteBarrier` value operands; the object operand being
//!   mutated is local);
//! * passed to a `Call` in any position (conservatively global; there is no
//!   interprocedural refinement to `ArgEscape`, which is declared but never
//!   produced);
//! * passed as a terminator edge argument (flow into block parameters is not
//!   traced);
//! * used by any other opcode.
//!
//! Local uses are field/element reads (`Car`, `Cdr`, `Load`, `VecRef`,
//! `SymbolValue`, object operand), the object operand of a mutation, and the
//! header inspections `TypeCheck`, `InstanceOf`, `Guard`. Being named by a
//! `FrameState` is not an escape; it is deopt metadata. The classification is
//! a single pass because escape is a sink and no through-object flow is
//! modelled, so no fixpoint is needed. When in doubt, a use is an escape.
//!
//! # Scalar replacement and FrameStates
//!
//! A candidate is a `NoEscape` `AllocCons` all of whose uses are `Car`/`Cdr`
//! loads in the object position. Two FrameState cases matter:
//!
//! * A `Car`/`Cdr` **result** named by a FrameState is fine: it is renamed to
//!   the cons's car/cdr operand, a surviving dominating value.
//! * The **cons itself** named by a FrameState is not replaced. A cons is a
//!   heap object and there is no `RematOp` that rebuilds one on the deopt
//!   path, so the allocation must stay to keep the slot valid. Teaching the
//!   remat vocabulary an `AllocCons` recipe would lift this.
//!
//! # Limits
//!
//! * Only conses are scalar-replaced; `Alloc` sites are classified but never
//!   transformed, and a `NoEscape` cons that is mutated, type-checked, or
//!   passed along an edge is left in place.
//! * No stack allocation is emitted from the `NoEscape` classification.
//! * Block-parameter flow is not traced, so a cons passed around a loop
//!   through a header parameter always escapes.

use std::collections::{HashMap, HashSet};

use crate::t2::frame_state::ValueSource;
use crate::t2::ir::{Function, Inst, Opcode, Value, ValueDef};
use crate::t2::pass::{Analyses, Pass};

/// Operand index of the car in an `AllocCons` (and the cons in `Car`).
const CAR_ARG: usize = 0;
/// Operand index of the cdr in an `AllocCons`.
const CDR_ARG: usize = 1;

// ── Escape state & result ───────────────────────────────────────────

/// Escape classification of an allocation site. This analysis
/// produces `NoEscape` and `GlobalEscape`; `ArgEscape` is reserved for a future
/// interprocedural refinement of capture-free callees.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EscapeState {
    /// Referenced only within the allocating function: never returned, never
    /// stored to the heap, never passed to a call. A stack-allocation / scalar-
    /// replacement candidate.
    NoEscape,
    /// Passed to a callee that does not capture it past its return (or is
    /// inlined). Not produced yet; reserved.
    ArgEscape,
    /// May become reachable from the heap, is returned, or is stored into a
    /// global / another heap object. Must stay heap-allocated.
    GlobalEscape,
}

impl EscapeState {
    #[inline]
    pub fn is_non_escaping(self) -> bool {
        matches!(self, EscapeState::NoEscape)
    }
}

/// The queryable result of escape analysis: one [`EscapeState`] per allocation
/// site, keyed by the allocation's result [`Value`].
#[derive(Clone, Debug, Default)]
pub struct EscapeResult {
    states: HashMap<Value, EscapeState>,
}

impl EscapeResult {
    /// The escape state of an allocation result, or `None` if `v` is not an
    /// allocation site tracked by this analysis.
    pub fn escape_state(&self, v: Value) -> Option<EscapeState> {
        self.states.get(&v).copied()
    }

    /// Whether `v` is a known allocation site that provably does **not** escape.
    pub fn is_non_escaping(&self, v: Value) -> bool {
        matches!(self.states.get(&v), Some(EscapeState::NoEscape))
    }

    /// Whether `v` is a known allocation site that escapes (or may escape). A
    /// non-allocation value returns `false` (it is not tracked); use
    /// [`escape_state`](Self::escape_state) to distinguish "unknown".
    pub fn escapes(&self, v: Value) -> bool {
        matches!(self.states.get(&v), Some(s) if !s.is_non_escaping())
    }

    /// The result value of every non-escaping allocation (the stack-alloc /
    /// scalar-replacement candidate set).
    pub fn non_escaping(&self) -> impl Iterator<Item = Value> + '_ {
        self.states
            .iter()
            .filter(|(_, s)| s.is_non_escaping())
            .map(|(&v, _)| v)
    }

    /// Number of allocation sites analysed.
    pub fn num_sites(&self) -> usize {
        self.states.len()
    }
}

// ── Analysis ────────────────────────────────────────────────────────

/// Is opcode `op` allocating? (`Alloc` / `AllocCons`.)
fn is_alloc(op: Opcode) -> bool {
    matches!(op, Opcode::Alloc | Opcode::AllocCons)
}

/// Whether a use of an allocation as operand `pos` of `op` is *provably local* —
/// i.e. it does NOT let the object escape the frame. Everything not on this
/// allow-list is treated as an escape (conservative default).
fn use_keeps_local(op: Opcode, pos: usize) -> bool {
    use Opcode::*;
    match op {
        // Field / element reads: the operand is the object being read, not stored.
        Car | Cdr | Load | VecRef | SymbolValue => pos == CAR_ARG,
        // Mutations: only operand 0 (the object being mutated) is local; the
        // value operand(s) store the pointer somewhere reachable → escape.
        Store | SetCar | SetCdr | VecSet | WriteBarrier => pos == 0,
        // Header inspections do not create a new reference to the object.
        TypeCheck | InstanceOf | Guard => true,
        // Calls, returns, tail-calls, throws, arithmetic on the pointer, block
        // arguments handled separately — everything else escapes.
        _ => false,
    }
}

/// Run escape analysis over `f`. Pure — does not mutate the IR.
pub fn analyse(f: &Function) -> EscapeResult {
    let mut states: HashMap<Value, EscapeState> = HashMap::new();

    // 1. Register every allocation site, provisionally NoEscape.
    for i in 0..f.num_insts() {
        let d = f.inst(Inst(i as u32));
        if is_alloc(d.opcode) {
            for &r in &d.results {
                states.insert(r, EscapeState::NoEscape);
            }
        }
    }
    if states.is_empty() {
        return EscapeResult { states };
    }

    // 2. Escape propagation over uses (single pass — escape is a sink; there is
    //    no through-object flow in this first cut, so a fixpoint is not needed).
    for i in 0..f.num_insts() {
        let d = f.inst(Inst(i as u32));
        // Instruction operands.
        for (pos, &arg) in d.args.iter().enumerate() {
            if states.contains_key(&arg) && !use_keeps_local(d.opcode, pos) {
                states.insert(arg, EscapeState::GlobalEscape);
            }
        }
        // Block-call arguments flow into block parameters we do not trace here.
        for t in &d.targets {
            for &arg in &t.args {
                if states.contains_key(&arg) {
                    states.insert(arg, EscapeState::GlobalEscape);
                }
            }
        }
    }
    // FrameState value sources are deliberately NOT escapes (deopt metadata).

    EscapeResult { states }
}

// ── Pass: scalar replacement of non-escaping conses ─────────────────

#[derive(Default)]
pub struct EscapeAnalysis;

impl Pass for EscapeAnalysis {
    fn name(&self) -> &'static str {
        "escape-analysis"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        let result = analyse(f);

        // Values named *directly* by any FrameState value source. A cons so named
        // cannot be scalar-replaced (a cons is not rematerialisable), so we skip
        // it (R4.60). Car/Cdr results so named are fine — they are renamed to a
        // surviving operand below.
        let fs_named = frame_state_named_values(f);

        // Collect per-cons use information for the NoEscape AllocCons sites, so we
        // can decide which are scalar-replaceable in one scan.
        let mut cand: HashMap<Value, ConsUses> = HashMap::new();
        for v in result.non_escaping() {
            if let ValueDef::Result { inst, .. } = f.value(v).def {
                if f.inst(inst).opcode == Opcode::AllocCons {
                    cand.insert(v, ConsUses::new(inst));
                }
            }
        }
        if cand.is_empty() {
            return;
        }

        // Scan every use of each candidate cons. A candidate stays replaceable
        // only if *all* its uses are Car/Cdr loads (operand 0) — anything else
        // (mutation, type check, block-arg) disqualifies it. Block-call arg uses
        // cannot happen for a NoEscape value (they force GlobalEscape), but we
        // guard anyway.
        for i in 0..f.num_insts() {
            let d = f.inst(Inst(i as u32));
            for (pos, &arg) in d.args.iter().enumerate() {
                if let Some(info) = cand.get_mut(&arg) {
                    match d.opcode {
                        Opcode::Car if pos == CAR_ARG => info.car_cdr_uses.push(Inst(i as u32)),
                        Opcode::Cdr if pos == CAR_ARG => info.car_cdr_uses.push(Inst(i as u32)),
                        _ => info.replaceable = false,
                    }
                }
            }
            for t in &d.targets {
                for &arg in &t.args {
                    if let Some(info) = cand.get_mut(&arg) {
                        info.replaceable = false;
                    }
                }
            }
        }

        // Build the substitution map (Car result → car operand, Cdr result → cdr
        // operand) and the set of instructions to delete.
        let nv = f.num_values();
        let mut subst: Vec<Value> = (0..nv as u32).map(Value).collect();
        let mut delete: HashSet<Inst> = HashSet::new();
        let mut changed = false;

        for (&cons, info) in cand.iter() {
            if !info.replaceable {
                continue;
            }
            // Cannot rematerialise a heap cons named by a FrameState — keep it.
            if fs_named.contains(&cons) {
                continue;
            }
            let alloc = f.inst(info.alloc);
            // Defensive: a well-formed AllocCons has [car, cdr].
            if alloc.args.len() <= CDR_ARG {
                continue;
            }
            let car_op = alloc.args[CAR_ARG];
            let cdr_op = alloc.args[CDR_ARG];

            for &u in &info.car_cdr_uses {
                let ud = f.inst(u);
                let repl = match ud.opcode {
                    Opcode::Car => car_op,
                    Opcode::Cdr => cdr_op,
                    _ => continue,
                };
                // The load has exactly one result (the field value).
                if let Some(&res) = ud.results.first() {
                    subst[res.index()] = repl;
                }
                delete.insert(u);
            }
            delete.insert(info.alloc);
            changed = true;
        }

        if !changed {
            return;
        }

        // Path-compress the substitution: replacements always point at a value
        // defined strictly earlier (an operand of the removed cons), so following
        // the chain terminates.
        resolve_subst(&mut subst);

        // Rewrite every instruction operand and block-call argument.
        for i in 0..f.num_insts() {
            let d = f.inst_mut(Inst(i as u32));
            for arg in d.args.iter_mut() {
                *arg = subst[arg.index()];
            }
            for t in d.targets.iter_mut() {
                for arg in t.args.iter_mut() {
                    *arg = subst[arg.index()];
                }
            }
        }

        // Rewrite FrameState value sources (R4.60): a slot naming a renamed
        // Car/Cdr result now names the surviving car/cdr operand.
        rewrite_frame_states(f, &subst);

        // Delete the AllocCons + Car/Cdr instructions by rebuilding block lists.
        for b in f.block_order().to_vec() {
            let kept: Vec<Inst> = f
                .block(b)
                .insts
                .iter()
                .copied()
                .filter(|i| !delete.contains(i))
                .collect();
            if kept.len() != f.block(b).insts.len() {
                f.block_mut(b).insts = kept;
            }
        }

        // We removed value definitions and rewrote uses (spec §4.5 Pass contract).
        a.invalidate();
    }
}

/// Per-candidate cons bookkeeping gathered during the use scan.
struct ConsUses {
    alloc: Inst,
    car_cdr_uses: Vec<Inst>,
    /// True while every observed use is a `Car`/`Cdr` load.
    replaceable: bool,
}

impl ConsUses {
    fn new(alloc: Inst) -> ConsUses {
        ConsUses {
            alloc,
            car_cdr_uses: Vec::new(),
            replaceable: true,
        }
    }
}

/// Follow each substitution entry to its terminal representative in place.
fn resolve_subst(subst: &mut [Value]) {
    for i in 0..subst.len() {
        let mut cur = subst[i];
        // Guard against a pathological cycle (malformed IR); the well-formed case
        // terminates because replacements point strictly backward.
        let mut guard = 0;
        while subst[cur.index()] != cur && guard <= subst.len() {
            cur = subst[cur.index()];
            guard += 1;
        }
        subst[i] = cur;
    }
}

/// Collect every SSA `Value` named *directly* by a FrameState value source
/// (across all scopes' locals and stack, and remat recipe inputs).
fn frame_state_named_values(f: &Function) -> HashSet<Value> {
    let mut out = HashSet::new();
    for (_id, fs) in f.frame_states.iter() {
        for scope in &fs.scopes {
            for src in scope.locals.iter().chain(scope.stack.iter()) {
                if let ValueSource::Value { value, .. } = src {
                    out.insert(*value);
                }
            }
        }
        for recipe in &fs.remat {
            for src in &recipe.inputs {
                if let ValueSource::Value { value, .. } = src {
                    out.insert(*value);
                }
            }
        }
    }
    out
}

/// Apply `subst` to every FrameState value source (locals, stack, remat inputs).
fn rewrite_frame_states(f: &mut Function, subst: &[Value]) {
    let remap = |src: &mut ValueSource| {
        if let ValueSource::Value { value, .. } = src {
            *value = subst[value.index()];
        }
    };
    for id in 0..f.frame_states.len() {
        let fs = f
            .frame_states
            .get_mut(crate::t2::frame_state::FrameStateId(id as u32));
        for scope in fs.scopes.iter_mut() {
            for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                remap(src);
            }
        }
        for recipe in fs.remat.iter_mut() {
            for src in recipe.inputs.iter_mut() {
                remap(src);
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, FrameStateId};
    use crate::t2::ir::{
        AuxData, IRType, InstData, InstFlags, TypeBits, ValueRepresentation as VR,
    };

    fn cons_ty() -> IRType {
        IRType::of(TypeBits::CONS)
    }
    fn any_ty() -> IRType {
        IRType::TOP
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

    /// entry(a, b): allocate `(cons a b)`, returning (function, cons value,
    /// alloc inst, car=a, cdr=b).
    fn with_cons(f: &mut Function) -> (Value, Value, Value, Inst) {
        let entry = f.entry();
        let a = f.add_block_param(entry, any_ty(), VR::Tagged);
        let b = f.add_block_param(entry, any_ty(), VR::Tagged);
        let (alloc, res) = f.push_inst(
            entry,
            inst(Opcode::AllocCons, vec![a, b], InstFlags::default()),
            &[(cons_ty(), VR::Tagged)],
        );
        (res[0], a, b, alloc)
    }

    fn run(f: &mut Function) {
        EscapeAnalysis.run(f, &mut Analyses::new());
    }

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

    // ── Analysis ──

    /// A cons that is only Car'd/Cdr'd does not escape.
    #[test]
    fn cons_only_car_cdr_does_not_escape() {
        let mut f = Function::new("local");
        let entry = f.entry();
        let (cons, _a, _b, _alloc) = with_cons(&mut f);
        let (_ci, car) = f.push_inst(
            entry,
            inst(Opcode::Car, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        f.push_inst(
            entry,
            inst(Opcode::Cdr, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        f.set_terminator(entry, ret(vec![car[0]]));

        let r = analyse(&f);
        assert_eq!(r.num_sites(), 1);
        assert!(r.is_non_escaping(cons));
        assert!(!r.escapes(cons));
        assert_eq!(r.non_escaping().collect::<Vec<_>>(), vec![cons]);
    }

    /// A cons that is returned escapes (GlobalEscape).
    #[test]
    fn returned_cons_escapes() {
        let mut f = Function::new("returned");
        let entry = f.entry();
        let (cons, _a, _b, _alloc) = with_cons(&mut f);
        f.set_terminator(entry, ret(vec![cons]));

        let r = analyse(&f);
        assert!(r.escapes(cons));
        assert_eq!(r.escape_state(cons), Some(EscapeState::GlobalEscape));
    }

    /// A cons passed to a `Call` escapes.
    #[test]
    fn cons_passed_to_call_escapes() {
        let mut f = Function::new("called");
        let entry = f.entry();
        let (cons, _a, _b, _alloc) = with_cons(&mut f);
        let callee = f.add_block_param(entry, any_ty(), VR::Tagged);
        let fl = InstFlags {
            call: true,
            ..InstFlags::default()
        };
        f.push_inst(entry, inst(Opcode::Call, vec![callee, cons], fl), &[]);
        f.set_terminator(entry, ret(vec![]));

        let r = analyse(&f);
        assert!(r.escapes(cons));
    }

    /// A cons stored as the *value* of another object's field escapes; a cons
    /// merely mutated (object operand) does not.
    #[test]
    fn cons_stored_as_value_escapes_but_object_operand_does_not() {
        let mut f = Function::new("stored");
        let entry = f.entry();
        let a = f.add_block_param(entry, any_ty(), VR::Tagged);
        let b = f.add_block_param(entry, any_ty(), VR::Tagged);
        // Two conses: `outer` is mutated (object operand); `inner` is stored into
        // it (value operand) and therefore escapes.
        let (_o_alloc, o_res) = f.push_inst(
            entry,
            inst(Opcode::AllocCons, vec![a, b], InstFlags::default()),
            &[(cons_ty(), VR::Tagged)],
        );
        let outer = o_res[0];
        let (_i_alloc, i_res) = f.push_inst(
            entry,
            inst(Opcode::AllocCons, vec![a, b], InstFlags::default()),
            &[(cons_ty(), VR::Tagged)],
        );
        let inner = i_res[0];
        let fl = InstFlags {
            effectful: true,
            ..InstFlags::default()
        };
        // SetCdr(outer, inner): outer at pos 0 (object), inner at pos 1 (value).
        f.push_inst(entry, inst(Opcode::SetCdr, vec![outer, inner], fl), &[]);
        f.set_terminator(entry, ret(vec![outer]));

        let r = analyse(&f);
        assert!(r.escapes(inner), "stored-as-value cons must escape");
        // `outer` also escapes here only because it is returned; check the store
        // alone does not escape it by using a fresh function.
        let mut g = Function::new("mutated-local");
        let ge = g.entry();
        let ga = g.add_block_param(ge, any_ty(), VR::Tagged);
        let gb = g.add_block_param(ge, any_ty(), VR::Tagged);
        let (_ga_alloc, gres) = g.push_inst(
            ge,
            inst(Opcode::AllocCons, vec![ga, gb], InstFlags::default()),
            &[(cons_ty(), VR::Tagged)],
        );
        let gcons = gres[0];
        let gfl = InstFlags {
            effectful: true,
            ..InstFlags::default()
        };
        g.push_inst(ge, inst(Opcode::SetCar, vec![gcons, ga], gfl), &[]);
        g.set_terminator(ge, ret(vec![]));
        let gr = analyse(&g);
        assert!(
            gr.is_non_escaping(gcons),
            "a cons only mutated (object operand) does not escape"
        );
    }

    // ── Transform (scalar replacement) ──

    /// A non-escaping cons whose only uses are Car/Cdr is scalar-replaced: the
    /// AllocCons + Car + Cdr vanish and the loads become the car/cdr operands.
    #[test]
    fn non_escaping_cons_is_scalar_replaced() {
        let mut f = Function::new("scalar");
        let entry = f.entry();
        let (cons, a, b, alloc) = with_cons(&mut f);
        let (car_inst, car) = f.push_inst(
            entry,
            inst(Opcode::Car, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        let (cdr_inst, cdr) = f.push_inst(
            entry,
            inst(Opcode::Cdr, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        // Use both loads so their results are observed.
        f.set_terminator(entry, ret(vec![car[0], cdr[0]]));

        run(&mut f);

        // Allocation and both loads removed from the schedule.
        let insts = &f.block(entry).insts;
        assert!(!insts.contains(&alloc), "AllocCons should be removed");
        assert!(!insts.contains(&car_inst), "Car should be removed");
        assert!(!insts.contains(&cdr_inst), "Cdr should be removed");

        // The return now uses the original car/cdr operands directly.
        let term = f.terminator(entry).unwrap();
        assert_eq!(f.inst(term).args, vec![a, b]);
    }

    /// A returned cons is left untouched by the transform (it escapes).
    #[test]
    fn escaping_cons_is_not_transformed() {
        let mut f = Function::new("keep");
        let entry = f.entry();
        let (cons, _a, _b, alloc) = with_cons(&mut f);
        f.set_terminator(entry, ret(vec![cons]));

        run(&mut f);

        assert!(
            f.block(entry).insts.contains(&alloc),
            "escaping AllocCons must be kept"
        );
    }

    /// FrameState handling — the renamed case: a Car result named by a FrameState
    /// is rewritten to the surviving car operand, and the cons is still replaced.
    #[test]
    fn scalar_replacement_rewrites_framestate_of_car_result() {
        let mut f = Function::new("fs-car");
        let entry = f.entry();
        let (cons, a, _b, alloc) = with_cons(&mut f);
        let (_car_inst, car) = f.push_inst(
            entry,
            inst(Opcode::Car, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        f.set_terminator(entry, ret(vec![car[0]]));
        // A FrameState names the Car result.
        let fsid = f.frame_states.add(frame(vec![ValueSource::Value {
            value: car[0],
            repr: VR::Tagged,
        }]));

        run(&mut f);

        assert!(!f.block(entry).insts.contains(&alloc));
        // The FrameState slot now names the car operand `a` (a surviving value).
        match f.frame_states.get(fsid).scopes[0].locals[0] {
            ValueSource::Value { value, .. } => assert_eq!(value, a),
            ref other => panic!("expected Value(a), got {other:?}"),
        }
    }

    /// FrameState handling — the blocked case: a cons named directly by a
    /// FrameState is NOT scalar-replaced (a cons cannot be rematerialised), so the
    /// allocation is kept and the slot stays valid.
    #[test]
    fn framestate_named_cons_is_not_scalar_replaced() {
        let mut f = Function::new("fs-cons");
        let entry = f.entry();
        let (cons, _a, _b, alloc) = with_cons(&mut f);
        f.push_inst(
            entry,
            inst(Opcode::Car, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        f.set_terminator(entry, ret(vec![]));
        let fsid = f.frame_states.add(frame(vec![ValueSource::Value {
            value: cons,
            repr: VR::Tagged,
        }]));

        // Analysis still says it does not escape (deopt naming is not an escape).
        assert!(analyse(&f).is_non_escaping(cons));

        run(&mut f);

        // But the transform keeps it, and the FrameState still names the cons.
        assert!(
            f.block(entry).insts.contains(&alloc),
            "cons named by a FrameState must be kept"
        );
        match f.frame_states.get(FrameStateId(fsid.0)).scopes[0].locals[0] {
            ValueSource::Value { value, .. } => assert_eq!(value, cons),
            ref other => panic!("expected Value(cons), got {other:?}"),
        }
    }

    /// A non-escaping cons that is also mutated (SetCar) is analysed as NoEscape
    /// but NOT scalar-replaced by this first-cut transform.
    #[test]
    fn mutated_non_escaping_cons_is_marked_but_not_replaced() {
        let mut f = Function::new("mutated");
        let entry = f.entry();
        let (cons, a, _b, alloc) = with_cons(&mut f);
        let fl = InstFlags {
            effectful: true,
            ..InstFlags::default()
        };
        f.push_inst(entry, inst(Opcode::SetCar, vec![cons, a], fl), &[]);
        f.push_inst(
            entry,
            inst(Opcode::Car, vec![cons], InstFlags::default()),
            &[(any_ty(), VR::Tagged)],
        );
        f.set_terminator(entry, ret(vec![]));

        assert!(analyse(&f).is_non_escaping(cons));
        run(&mut f);
        assert!(
            f.block(entry).insts.contains(&alloc),
            "mutated cons is not scalar-replaced in this first cut"
        );
    }
}
