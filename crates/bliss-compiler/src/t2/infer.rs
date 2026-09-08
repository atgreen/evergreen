//! P3 — Type / range / representation inference (spec §4.5 R4.31, §4.10 §4.5).
//!
//! **Parcel P3.** A sparse-conditional (SCCP-family) *forward* propagation over
//! the block-based SSA. For every `Value` it infers a narrowed [`IRType`] (tag
//! bits + an integer [`Range`]) and a provable/profitable [`ValueRepresentation`].
//!
//! The lattice (spec D4.08) is `IRType` ordered by `meet` (intersection of tag
//! bits, intersection of integer ranges). Facts start at `⊥` and grow by `join`
//! up an ascending chain to a fixpoint over reverse-postorder; a widening step
//! guarantees termination on loops whose bound is not a compile-time constant.
//!
//! Flow sensitivity comes from two sources, both keyed to the dominator tree:
//!   * `TypeCheck`/`Guard` — the checked value is narrowed to the met type on
//!     the dominated region (spec R4.63: speculative facts are discharged by a
//!     guard, so inference may assume the narrowed type downstream).
//!   * `Brif` on a comparison — on an edge whose target is entered *only* through
//!     that edge, the compared operands are range-narrowed (`i < n` ⇒ the body
//!     sees `i ∈ [lo, n-1]`), which is what lets a counted loop's index be proved
//!     `FIXNUM[0, n]` so its overflow guard can later be dropped (spec §4.10 T2-c).
//!
//! Results live in [`InferenceResult`], a side table owned by the pass: `IRType`
//! is immutable on `ValueData` (the frozen `ir` contract exposes no setter), so
//! inferred facts are kept in maps rather than written back onto the arena.

use std::collections::HashMap;

use crate::t2::ir::{
    AuxData, Function, IRType, Opcode, Range, TypeBits, Value, ValueDef, ValueRepresentation,
};
use crate::t2::pass::{Analyses, Pass};

// ── Result side table ───────────────────────────────────────────────

/// The inferred facts, produced by [`infer`]. Owned by the pass because the
/// frozen `ir` contract has no public setter for a value's inferred type.
#[derive(Clone, Debug, Default)]
pub struct InferenceResult {
    types: HashMap<Value, IRType>,
    reprs: HashMap<Value, ValueRepresentation>,
}

impl InferenceResult {
    /// The inferred `IRType` for `v` (`⊥` if the value was never reached).
    pub fn ty(&self, v: Value) -> IRType {
        self.types.get(&v).copied().unwrap_or(IRType::BOTTOM)
    }

    /// The inferred fact for `v`, if one was computed.
    pub fn get(&self, v: Value) -> Option<&IRType> {
        self.types.get(&v)
    }

    /// The inferred integer range for `v`, if proven.
    pub fn range(&self, v: Value) -> Option<Range> {
        self.types.get(&v).and_then(|t| t.range)
    }

    /// The inferred machine representation for `v` (provable/profitable unboxing).
    pub fn repr(&self, v: Value) -> Option<ValueRepresentation> {
        self.reprs.get(&v).copied()
    }

    /// Number of values with a non-`⊥` fact (for the pass log).
    pub fn known_count(&self) -> usize {
        self.types.values().filter(|t| !t.bits.is_bottom()).count()
    }
}

// ── The pass ────────────────────────────────────────────────────────

/// The type/range/representation inference pass.
#[derive(Default)]
pub struct TypeInference;

impl Pass for TypeInference {
    fn name(&self) -> &'static str {
        "type-inference"
    }

    fn run(&mut self, f: &mut Function, _a: &mut Analyses) {
        // `Analyses` (frozen) has no slot to stash inference results, so the pass
        // recomputes and — for now — only summarises. Downstream passes call
        // `infer` directly. This is a read-only analysis: `f` is not mutated, so
        // no analysis invalidation is required.
        let result = infer(f);
        if std::env::var_os("BLISS_IR_DUMP").is_some() {
            eprintln!(
                "[type-inference] {}: {} values with narrowed facts",
                f.name(),
                result.known_count()
            );
        }
    }
}

// ── Lattice helpers on IRType (ir exposes meet/join only on TypeBits) ─

const FULL: Range = Range {
    lo: i64::MIN,
    hi: i64::MAX,
};

/// CL boolean: a comparison yields `T` (a symbol) or `NIL` (the null type).
fn boolean() -> IRType {
    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL))
}

fn fixnum_full() -> IRType {
    IRType {
        bits: TypeBits::FIXNUM,
        range: Some(FULL),
        class_id: None,
    }
}

fn range_hull(a: Range, b: Range) -> Range {
    Range {
        lo: a.lo.min(b.lo),
        hi: a.hi.max(b.hi),
    }
}

/// Intersection; `None` if the ranges are disjoint (an unreachable refinement).
fn range_meet(a: Range, b: Range) -> Option<Range> {
    let lo = a.lo.max(b.lo);
    let hi = a.hi.min(b.hi);
    if lo <= hi {
        Some(Range { lo, hi })
    } else {
        None
    }
}

/// Join (⊔) up the lattice — the fact holds on *either* incoming path. `⊥` is the
/// identity so a value accumulating over predecessor edges keeps its range.
fn ty_join(a: IRType, b: IRType) -> IRType {
    if a.bits.is_bottom() {
        return b;
    }
    if b.bits.is_bottom() {
        return a;
    }
    let range = match (a.range, b.range) {
        (Some(x), Some(y)) => Some(range_hull(x, y)),
        // An unbounded side means we cannot bound the merge.
        _ => None,
    };
    IRType {
        bits: a.bits.join(b.bits),
        range,
        class_id: if a.class_id == b.class_id {
            a.class_id
        } else {
            None
        },
    }
}

/// Meet (⊓) down the lattice — the fact holds on *both* facts (used for guard /
/// branch narrowing). A `None` range is "no constraint" (i.e. the full line).
fn ty_meet(a: IRType, b: IRType) -> IRType {
    let range = match (a.range, b.range) {
        (Some(x), Some(y)) => range_meet(x, y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    };
    IRType {
        bits: a.bits.meet(b.bits),
        range,
        class_id: a.class_id.or(b.class_id),
    }
}

/// Widening (▽): once past the iteration threshold, extend a still-growing bound
/// to saturation so the ascending chain terminates. Sound (over-approximates).
fn ty_widen(old: IRType, new: IRType) -> IRType {
    let range = match (old.range, new.range) {
        (Some(o), Some(n)) => Some(Range {
            lo: if n.lo < o.lo { i64::MIN } else { o.lo },
            hi: if n.hi > o.hi { i64::MAX } else { o.hi },
        }),
        _ => new.range,
    };
    IRType { range, ..new }
}

// ── Instruction transfer function ───────────────────────────────────

fn sat_add(a: i64, b: i64) -> i64 {
    a.saturating_add(b)
}
fn sat_sub(a: i64, b: i64) -> i64 {
    a.saturating_sub(b)
}
fn sat_mul(a: i64, b: i64) -> i64 {
    a.saturating_mul(b)
}

/// Evaluate the abstract result of an instruction from its operand facts.
/// Returns `⊥` when an operand is not yet known, so unknowns never pollute a
/// result with a spuriously-wide fact during the fixpoint.
fn eval(opcode: Opcode, aux: &AuxData, ops: &[IRType]) -> IRType {
    use Opcode::*;

    // Constants have no operands and seed the lattice directly.
    match opcode {
        ConstFixnum => {
            let v = match aux {
                AuxData::FixnumImm(i) => *i,
                _ => 0,
            };
            return IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo: v, hi: v }),
                class_id: None,
            };
        }
        ConstFloat => return IRType::of(TypeBits::SINGLE_FLOAT),
        ConstChar => return IRType::of(TypeBits::CHARACTER),
        ConstNil => return IRType::of(TypeBits::NULL),
        ConstT => return IRType::of(TypeBits::SYMBOL),
        ConstSymbol => return IRType::of(TypeBits::SYMBOL),
        ConstHeapObj => return IRType::TOP,
        _ => {}
    }

    // Any unknown operand ⇒ result still unknown (stay at ⊥ this round).
    if ops.iter().any(|o| o.bits.is_bottom()) {
        return IRType::BOTTOM;
    }

    // Integer range of a fixnum operand (full line if it carries no range yet).
    let r = |i: usize| ops.get(i).and_then(|t| t.range).unwrap_or(FULL);

    match opcode {
        FixnumAdd => {
            let (a, b) = (r(0), r(1));
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range {
                    lo: sat_add(a.lo, b.lo),
                    hi: sat_add(a.hi, b.hi),
                }),
                class_id: None,
            }
        }
        FixnumSub => {
            let (a, b) = (r(0), r(1));
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range {
                    lo: sat_sub(a.lo, b.hi),
                    hi: sat_sub(a.hi, b.lo),
                }),
                class_id: None,
            }
        }
        FixnumMul => {
            let (a, b) = (r(0), r(1));
            let ps = [
                sat_mul(a.lo, b.lo),
                sat_mul(a.lo, b.hi),
                sat_mul(a.hi, b.lo),
                sat_mul(a.hi, b.hi),
            ];
            let lo = *ps.iter().min().unwrap();
            let hi = *ps.iter().max().unwrap();
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo, hi }),
                class_id: None,
            }
        }
        FixnumNeg => {
            let a = r(0);
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range {
                    lo: a.hi.checked_neg().unwrap_or(i64::MAX),
                    hi: a.lo.checked_neg().unwrap_or(i64::MAX),
                }),
                class_id: None,
            }
        }
        // Fixnum ops whose range we do not track precisely: known FIXNUM, full range.
        FixnumDiv | FixnumRem | FixnumMod | FixnumShl | FixnumShr | LogAnd | LogOr | LogXor
        | LogNot | WidenI32 | UnboxFixnum | BoxFixnum => fixnum_full(),

        // Float arithmetic: propagate the float tag(s) of the operands.
        FloatAdd | FloatSub | FloatMul | FloatDiv => {
            let bits = ops
                .iter()
                .fold(TypeBits::BOTTOM, |acc, o| acc.join(o.bits))
                .meet(TypeBits::SINGLE_FLOAT.join(TypeBits::DOUBLE_FLOAT));
            let bits = if bits.is_bottom() {
                TypeBits::SINGLE_FLOAT
            } else {
                bits
            };
            IRType::of(bits)
        }
        BoxFloat | UnboxFloat => {
            let bits = ops[0]
                .bits
                .meet(TypeBits::SINGLE_FLOAT.join(TypeBits::DOUBLE_FLOAT));
            IRType::of(if bits.is_bottom() {
                TypeBits::SINGLE_FLOAT
            } else {
                bits
            })
        }

        // Generic (tower) arithmetic: numeric contagion — the join of the operand
        // numeric tags. Fixnum⊕fixnum may overflow to a bignum, so keep both.
        GenericAdd | GenericSub | GenericMul | GenericDiv => {
            let numeric = TypeBits::FIXNUM
                .join(TypeBits::BIGNUM)
                .join(TypeBits::RATIO)
                .join(TypeBits::SINGLE_FLOAT)
                .join(TypeBits::DOUBLE_FLOAT)
                .join(TypeBits::COMPLEX);
            let mut bits = ops
                .iter()
                .fold(TypeBits::BOTTOM, |acc, o| acc.join(o.bits))
                .meet(numeric);
            if bits.contains(TypeBits::FIXNUM) {
                bits = bits.join(TypeBits::BIGNUM); // exactness may promote
            }
            if bits.is_bottom() {
                bits = numeric;
            }
            IRType::of(bits)
        }

        // Comparisons and predicates yield a CL boolean.
        FixnumCmpEq | FixnumCmpLt | FixnumCmpLe | FixnumCmpGt | FixnumCmpGe | FloatCmpEq
        | FloatCmpLt | GenericEq | GenericEqual | InstanceOf => boolean(),

        // A type check narrows its operand to the checked tag (spec R4.63).
        // A Guard VALUE does the same for its passed-through result: it exists
        // only on the path where the check held (bliss-x5y.25b). A non-TypeTag
        // guard (StringLayout) passes its operand's type through unchanged.
        TypeCheck | Guard => {
            let tag = match aux {
                AuxData::TypeTag(t) => *t,
                _ => IRType::TOP,
            };
            ty_meet(ops.first().copied().unwrap_or(IRType::TOP), tag)
        }

        AllocCons => IRType::of(TypeBits::CONS),

        // Everything else (loads, calls, generic memory) is unconstrained.
        _ => IRType::TOP,
    }
}

// ── Flow-sensitive refinements from Brif conditions ─────────────────

/// A narrowing fact: `value` may be refined to `ty` in a dominated region.
#[derive(Clone)]
struct Refine {
    value: Value,
    ty: IRType,
}

/// Narrow the operands of a fixnum comparison `a <cmp> b` given the branch taken.
/// Returns refinements for `a` and `b` (only where a bound can be derived).
fn narrow_cmp(op: Opcode, a: Value, b: Value, taken: bool, g: &Facts) -> Vec<Refine> {
    use Opcode::*;
    // Normalise `false` edge to the negated relation.
    let rel = match (op, taken) {
        (FixnumCmpLt, true) | (FixnumCmpGe, false) => FixnumCmpLt,
        (FixnumCmpLe, true) | (FixnumCmpGt, false) => FixnumCmpLe,
        (FixnumCmpGt, true) | (FixnumCmpLe, false) => FixnumCmpGt,
        (FixnumCmpGe, true) | (FixnumCmpLt, false) => FixnumCmpGe,
        (FixnumCmpEq, true) => FixnumCmpEq,
        _ => return Vec::new(), // e.g. `!=`: no useful range narrowing
    };
    let ra = g.get(a).range;
    let rb = g.get(b).range;
    let fix = |range: Range| IRType {
        bits: TypeBits::FIXNUM,
        range: Some(range),
        class_id: None,
    };
    let mut out = Vec::new();
    match rel {
        FixnumCmpLt => {
            // a < b : a ≤ b.hi-1 ; b ≥ a.lo+1
            if let Some(rb) = rb {
                out.push(Refine {
                    value: a,
                    ty: fix(Range {
                        lo: i64::MIN,
                        hi: sat_sub(rb.hi, 1),
                    }),
                });
            }
            if let Some(ra) = ra {
                out.push(Refine {
                    value: b,
                    ty: fix(Range {
                        lo: sat_add(ra.lo, 1),
                        hi: i64::MAX,
                    }),
                });
            }
        }
        FixnumCmpLe => {
            if let Some(rb) = rb {
                out.push(Refine {
                    value: a,
                    ty: fix(Range {
                        lo: i64::MIN,
                        hi: rb.hi,
                    }),
                });
            }
            if let Some(ra) = ra {
                out.push(Refine {
                    value: b,
                    ty: fix(Range {
                        lo: ra.lo,
                        hi: i64::MAX,
                    }),
                });
            }
        }
        FixnumCmpGt => {
            if let Some(rb) = rb {
                out.push(Refine {
                    value: a,
                    ty: fix(Range {
                        lo: sat_add(rb.lo, 1),
                        hi: i64::MAX,
                    }),
                });
            }
            if let Some(ra) = ra {
                out.push(Refine {
                    value: b,
                    ty: fix(Range {
                        lo: i64::MIN,
                        hi: sat_sub(ra.hi, 1),
                    }),
                });
            }
        }
        FixnumCmpGe => {
            if let Some(rb) = rb {
                out.push(Refine {
                    value: a,
                    ty: fix(Range {
                        lo: rb.lo,
                        hi: i64::MAX,
                    }),
                });
            }
            if let Some(ra) = ra {
                out.push(Refine {
                    value: b,
                    ty: fix(Range {
                        lo: i64::MIN,
                        hi: ra.hi,
                    }),
                });
            }
        }
        FixnumCmpEq => {
            // a == b : both take the intersection of the two ranges.
            if let (Some(ra), Some(rb)) = (ra, rb) {
                if let Some(m) = range_meet(ra, rb) {
                    out.push(Refine {
                        value: a,
                        ty: fix(m),
                    });
                    out.push(Refine {
                        value: b,
                        ty: fix(m),
                    });
                }
            }
        }
        _ => {}
    }
    out
}

// ── The fixpoint ────────────────────────────────────────────────────

/// Working store of per-value facts, defaulting to `⊥`.
struct Facts(HashMap<Value, IRType>);

impl Facts {
    fn get(&self, v: Value) -> IRType {
        self.0.get(&v).copied().unwrap_or(IRType::BOTTOM)
    }
}

/// Past this many full passes, switch on widening so non-constant loop bounds
/// terminate. Constant-bounded loops used in practice converge well before this.
const WIDEN_AFTER: usize = 400;
const MAX_PASSES: usize = WIDEN_AFTER + 8;

/// Infer types, ranges, and representations for every reachable value in `f`.
pub fn infer(f: &Function) -> InferenceResult {
    let rpo = f.reverse_postorder();
    let dom = f.dominators();
    let mut g = Facts(HashMap::new());

    // Seed entry / pred-less block parameters from their declared type once.
    for &b in &rpo {
        if f.preds(b).is_empty() {
            for &p in &f.block(b).params {
                g.0.insert(p, f.value(p).ty);
            }
        }
    }

    for pass in 0..MAX_PASSES {
        let widen = pass >= WIDEN_AFTER;
        let mut changed = false;

        // Recompute branch/guard refinements against the current facts. A
        // refinement attached to block T is valid throughout the region T
        // dominates. `edge_refine` holds facts known on *entry* to T; `check_refine`
        // holds `TypeCheck`/`Guard` narrowings, valid only in blocks T strictly
        // dominates (a use before the check in T itself is handled in program order).
        let mut edge_refine: HashMap<usize, Vec<Refine>> = HashMap::new();
        let mut check_refine: HashMap<usize, Vec<Refine>> = HashMap::new();

        for &b in &rpo {
            // Branch-condition refinements.
            if let Some(term) = f.terminator(b) {
                let t = f.inst(term);
                if t.opcode == Opcode::Brif && !t.args.is_empty() {
                    let cond = t.args[0];
                    if let ValueDef::Result { inst, .. } = f.value(cond).def {
                        let cdef = f.inst(inst);
                        if cdef.args.len() == 2 {
                            let (ca, cb) = (cdef.args[0], cdef.args[1]);
                            for (edge, taken) in [(0usize, true), (1usize, false)] {
                                if let Some(call) = t.targets.get(edge) {
                                    let tgt = call.block;
                                    // Only sound if the edge is the sole way in.
                                    if f.preds(tgt) == vec![b] {
                                        let refs = narrow_cmp(cdef.opcode, ca, cb, taken, &g);
                                        edge_refine.entry(tgt.index()).or_default().extend(refs);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // TypeCheck / Guard narrowings within this block, for dominated blocks.
            for &inst in &f.block(b).insts {
                let idef = f.inst(inst);
                if matches!(idef.opcode, Opcode::TypeCheck | Opcode::Guard) && !idef.args.is_empty()
                {
                    if let AuxData::TypeTag(tag) = &idef.aux {
                        check_refine.entry(b.index()).or_default().push(Refine {
                            value: idef.args[0],
                            ty: *tag,
                        });
                    }
                }
            }
        }

        for &b in &rpo {
            // 1. Block parameter facts = join over predecessor edges (or the
            //    declared type for pred-less blocks, seeded above).
            let preds = f.preds(b);
            if !preds.is_empty() {
                let params = f.block(b).params.clone();
                for (num, &p) in params.iter().enumerate() {
                    let mut acc = IRType::BOTTOM;
                    for &pred in &preds {
                        if let Some(term) = f.terminator(pred) {
                            for call in &f.inst(term).targets {
                                if call.block == b {
                                    if let Some(&arg) = call.args.get(num) {
                                        acc = ty_join(acc, g.get(arg));
                                    }
                                }
                            }
                        }
                    }
                    let old = g.get(p);
                    let joined = ty_join(old, acc);
                    let next = if widen { ty_widen(old, joined) } else { joined };
                    if next != old {
                        g.0.insert(p, next);
                        changed = true;
                    }
                }
            }

            // 2. Build the flow-sensitive environment at entry to `b`: meet `g`
            //    with every refinement whose scope dominates `b`.
            let mut env: HashMap<Value, IRType> = HashMap::new();
            let mut d = b;
            loop {
                if let Some(refs) = edge_refine.get(&d.index()) {
                    for rf in refs {
                        let cur = env
                            .get(&rf.value)
                            .copied()
                            .unwrap_or_else(|| g.get(rf.value));
                        env.insert(rf.value, ty_meet(cur, rf.ty));
                    }
                }
                if d != b {
                    if let Some(refs) = check_refine.get(&d.index()) {
                        for rf in refs {
                            let cur = env
                                .get(&rf.value)
                                .copied()
                                .unwrap_or_else(|| g.get(rf.value));
                            env.insert(rf.value, ty_meet(cur, rf.ty));
                        }
                    }
                }
                match dom.idom(d) {
                    Some(id) if id != d => d = id,
                    _ => break,
                }
            }

            // 3. Walk instructions in program order, using the local environment.
            let fact = |v: Value, env: &HashMap<Value, IRType>, g: &Facts| -> IRType {
                env.get(&v).copied().unwrap_or_else(|| g.get(v))
            };
            for &inst in &f.block(b).insts {
                let idef = f.inst(inst);
                if idef.opcode.is_terminator() {
                    continue;
                }
                let ops: Vec<IRType> = idef.args.iter().map(|&a| fact(a, &env, &g)).collect();
                let res = eval(idef.opcode, &idef.aux, &ops);
                for &rv in &idef.results {
                    let old = g.get(rv);
                    let joined = ty_join(old, res);
                    if joined != old {
                        g.0.insert(rv, joined);
                        changed = true;
                    }
                    env.insert(rv, joined);
                }
                // In-program-order guard narrowing for the rest of this block.
                if matches!(idef.opcode, Opcode::TypeCheck | Opcode::Guard) && !idef.args.is_empty()
                {
                    if let AuxData::TypeTag(tag) = &idef.aux {
                        let v = idef.args[0];
                        let cur = fact(v, &env, &g);
                        env.insert(v, ty_meet(cur, *tag));
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }

    // Derive representations from the final types.
    let mut reprs = HashMap::new();
    for (&v, &t) in &g.0 {
        reprs.insert(v, repr_of(t));
    }
    InferenceResult { types: g.0, reprs }
}

/// Choose a provable/profitable unboxed representation for a fact, else `Tagged`.
fn repr_of(t: IRType) -> ValueRepresentation {
    let only = |bit: TypeBits| !t.bits.is_bottom() && t.bits.meet(bit) == t.bits;
    if only(TypeBits::FIXNUM) {
        ValueRepresentation::UnboxedFixnum
    } else if only(TypeBits::SINGLE_FLOAT) {
        ValueRepresentation::UnboxedF32
    } else if only(TypeBits::DOUBLE_FLOAT) {
        ValueRepresentation::UnboxedF64
    } else {
        ValueRepresentation::Tagged
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{BlockCall, InstData, InstFlags};

    fn inst(op: Opcode) -> InstData {
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

    fn const_fixnum(f: &mut Function, b: crate::t2::ir::Block, v: i64) -> Value {
        let (_, r) = f.push_inst(
            b,
            InstData {
                aux: AuxData::FixnumImm(v),
                ..inst(Opcode::ConstFixnum)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        r[0]
    }

    fn ret() -> InstData {
        inst(Opcode::Return)
    }

    #[test]
    fn const_gets_singleton_range() {
        let mut f = Function::new("k");
        let e = f.entry();
        let c = const_fixnum(&mut f, e, 42);
        f.set_terminator(e, ret());

        let r = infer(&f);
        assert_eq!(r.ty(c).bits, TypeBits::FIXNUM);
        assert_eq!(r.range(c), Some(Range { lo: 42, hi: 42 }));
        assert_eq!(r.repr(c), Some(ValueRepresentation::UnboxedFixnum));
    }

    #[test]
    fn fixnum_add_sums_ranges() {
        // Two entry params carrying declared ranges, added together.
        let mut f = Function::new("add");
        let e = f.entry();
        let a = f.add_block_param(
            e,
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo: 1, hi: 5 }),
                class_id: None,
            },
            ValueRepresentation::UnboxedFixnum,
        );
        let b = f.add_block_param(
            e,
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo: 10, hi: 20 }),
                class_id: None,
            },
            ValueRepresentation::UnboxedFixnum,
        );
        let (_, sum) = f.push_inst(
            e,
            InstData {
                args: vec![a, b],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(e, ret());

        let r = infer(&f);
        assert_eq!(r.ty(sum[0]).bits, TypeBits::FIXNUM);
        assert_eq!(r.range(sum[0]), Some(Range { lo: 11, hi: 25 }));
    }

    #[test]
    fn subtraction_range_is_flipped() {
        let mut f = Function::new("sub");
        let e = f.entry();
        let a = f.add_block_param(
            e,
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo: 10, hi: 20 }),
                class_id: None,
            },
            ValueRepresentation::UnboxedFixnum,
        );
        let b = f.add_block_param(
            e,
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(Range { lo: 1, hi: 4 }),
                class_id: None,
            },
            ValueRepresentation::UnboxedFixnum,
        );
        let (_, d) = f.push_inst(
            e,
            InstData {
                args: vec![a, b],
                ..inst(Opcode::FixnumSub)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(e, ret());
        // [10,20] - [1,4] = [10-4, 20-1] = [6, 19]
        assert_eq!(infer(&f).range(d[0]), Some(Range { lo: 6, hi: 19 }));
    }

    #[test]
    fn comparison_is_boolean() {
        let mut f = Function::new("cmp");
        let e = f.entry();
        let a = const_fixnum(&mut f, e, 1);
        let b = const_fixnum(&mut f, e, 2);
        let (_, c) = f.push_inst(
            e,
            InstData {
                args: vec![a, b],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret());
        let r = infer(&f);
        assert_eq!(r.ty(c[0]).bits, TypeBits::SYMBOL.join(TypeBits::NULL));
    }

    #[test]
    fn typecheck_narrows_the_met_type() {
        // entry: p: TOP ; r = TypeCheck(p, FIXNUM) ; then use r downstream.
        let mut f = Function::new("tc");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let (_, r) = f.push_inst(
            e,
            InstData {
                args: vec![p],
                aux: AuxData::TypeTag(fixnum()),
                flags: InstFlags {
                    guard: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::TypeCheck)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret());

        let res = infer(&f);
        // The checked result is narrowed to FIXNUM (met with TOP).
        assert_eq!(res.ty(r[0]).bits, TypeBits::FIXNUM);
        assert_eq!(res.repr(r[0]), Some(ValueRepresentation::UnboxedFixnum));
    }

    #[test]
    fn guard_narrows_on_dominated_path() {
        // entry: p: TOP ; Guard(p : FIXNUM) ; jump B.
        // B: x = FixnumAdd(p, 1)  — p is narrowed to FIXNUM on the dominated block,
        // so the add is well-typed and x is FIXNUM.
        let mut f = Function::new("guard");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        f.push_inst(
            e,
            InstData {
                args: vec![p],
                aux: AuxData::TypeTag(fixnum()),
                flags: InstFlags {
                    guard: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::Guard)
            },
            &[],
        );
        let bb = f.make_block();
        f.set_terminator(
            e,
            InstData {
                targets: vec![BlockCall {
                    block: bb,
                    args: vec![],
                }],
                ..inst(Opcode::Jump)
            },
        );
        let one = const_fixnum(&mut f, bb, 1);
        let (_, x) = f.push_inst(
            bb,
            InstData {
                args: vec![p, one],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(bb, ret());

        let res = infer(&f);
        // Without the dominated-path narrowing, `p` would be TOP and the add
        // could not be trusted; with it, x is a fixnum.
        assert_eq!(res.ty(x[0]).bits, TypeBits::FIXNUM);
    }

    #[test]
    fn counted_loop_index_is_bounded_fixnum() {
        // for (i = 0; i < 10; i++) — prove the header index is FIXNUM[0,10].
        let mut f = Function::new("loop");
        let e = f.entry();
        let header = f.make_block();
        let i = f.add_block_param(header, fixnum(), ValueRepresentation::UnboxedFixnum);
        let body = f.make_block();
        let exit = f.make_block();

        // entry: i0 = 0 ; the loop bound 10 ; jump header(i0).
        let i0 = const_fixnum(&mut f, e, 0);
        let bound = const_fixnum(&mut f, e, 10);
        f.set_terminator(
            e,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![i0],
                }],
                ..inst(Opcode::Jump)
            },
        );

        // header: cond = i < 10 ; brif cond -> body, exit.
        let (_, cond) = f.push_inst(
            header,
            InstData {
                args: vec![i, bound],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
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
                ..inst(Opcode::Brif)
            },
        );

        // body: i1 = i + 1 ; jump header(i1).
        let one = const_fixnum(&mut f, body, 1);
        let (_, i1) = f.push_inst(
            body,
            InstData {
                args: vec![i, one],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(
            body,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![i1[0]],
                }],
                ..inst(Opcode::Jump)
            },
        );

        f.set_terminator(exit, ret());

        let r = infer(&f);
        // The induction variable is a bounded fixnum: 0 ≤ i ≤ 10.
        assert_eq!(r.ty(i).bits, TypeBits::FIXNUM, "index must be a FIXNUM");
        let rng = r.range(i).expect("index must have a proven range");
        assert_eq!(rng, Range { lo: 0, hi: 10 }, "index must be proved bounded");
        // Inside the body, `i` is narrowed below the bound, so i+1 ≤ 10.
        assert_eq!(r.range(i1[0]), Some(Range { lo: 1, hi: 10 }));
        assert_eq!(r.repr(i), Some(ValueRepresentation::UnboxedFixnum));
    }
}
