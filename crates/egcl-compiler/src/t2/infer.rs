// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Type, integer-range and representation inference over the T2 SSA IR.
//!
//! # Purpose
//!
//! [`infer`] is a forward, flow-sensitive dataflow analysis in the sparse
//! conditional constant propagation family. For every reachable `Value` it
//! also publishes shared structural evidence in [`InferenceResult::structural`].
//! For numeric types it computes a narrowed [`IRType`] (tag bits, an optional integer [`Range`],
//! an optional class id) and from that a [`ValueRepresentation`] saying
//! whether the value can be carried unboxed. Results go in an
//! [`InferenceResult`] side table; the IR is never mutated.
//!
//! Consumers call `infer` directly: guard elimination uses it to delete
//! guards whose check is already implied, and the x86 framed emitter uses it
//! to drop values proven to be non-pointer immediates from safepoint root
//! synchronisation. [`TypeInference`] exists so the analysis can sit in a
//! `PassManager` pipeline, but as a pass it only logs a summary under
//! `EGCL_IR_DUMP`; `Analyses` has no slot for inference results.
//!
//! # Lattice
//!
//! Facts are `IRType`s. `⊥` (`IRType::BOTTOM`) means "not yet reached";
//! `⊤` means "any value". `ty_join` moves up (union of tag bits, hull of
//! ranges, class id kept only if equal; `⊥` is the identity) and is used at
//! block parameters, where a fact must hold on every incoming edge, and when
//! accumulating a result across iterations. `ty_meet` moves down
//! (intersection of bits and ranges, class id from either side) and is used
//! for guard and branch narrowing, where both facts hold. `ty_widen` moves a
//! loop-header bound that is still growing to the fixnum bound, then to
//! `i64::MIN`/`i64::MAX`, so the ascending chain terminates.
//!
//! # Algorithm
//!
//! 1. Seed the parameters of predecessor-less blocks (the entry; an OSR entry
//!    region) from their declared types.
//! 2. Iterate over reverse postorder until no fact changes (at most
//!    `MAX_PASSES` times). From pass `WIDEN_AFTER` on, the parameters of loop
//!    headers — targets of retreating edges, which every cycle contains — are
//!    widened. Each pass:
//!    1. Collect `check_refine[B]`, the narrowings of every
//!       `Guard` with a `TypeTag`/`StringLayout` in block `B`.
//!    2. For each block: join its parameters over predecessor edge
//!       arguments; build a local environment by walking the dominator chain
//!       and meeting in every edge refinement on the chain (including the
//!       block's own) and every check refinement of a **strict** dominator;
//!       then evaluate instructions in program order with `eval`, joining
//!       each result into the global fact and inserting it into the local
//!       environment. A successful `Guard` met in program order narrows its
//!       operand for the rest of the block.
//!       After evaluating a block, derive its outgoing branch refinements
//!       from the updated facts. `edge_refine[T]` holds bounds true on the
//!       unique edge into `T` from a fixnum comparison (`!=` yields nothing).
//!       Producing these before visiting successors avoids permanently joining
//!       coarse results before the comparison's constant bounds are known.
//!    3. Stop when no fact changed.
//! 3. Narrow, if a widening step fired (otherwise the ascent already reached
//!    the least fixpoint): up to `NARROWING_PASSES` more passes in which every fact is
//!    recomputed rather than joined with its previous value (parameters still
//!    join their entry/OSR seed). Starting from a post-fixpoint, each such pass
//!    stays sound and recovers what widening discarded: the edge refinement
//!    `i < 10` bounds the body's `i + 1`, and so the header's `i`, again.
//! 4. Derive representations: a fact confined to fixnum, single-float or
//!    double-float bits becomes the corresponding unboxed representation,
//!    anything else `Tagged`.
//!
//! `eval` returns `⊥` whenever any operand is `⊥`, so an unreached operand
//! never widens a result prematurely. Constants seed singleton ranges;
//! fixnum add/sub/mul/neg propagate ranges with saturating arithmetic; other
//! fixnum ops yield the full fixnum range; float ops propagate float tags;
//! generic arithmetic applies numeric contagion and adds `BIGNUM` whenever
//! `FIXNUM` is possible; comparisons and `TypeCheck` yield `SYMBOL|NULL`; successful `Guard`
//! results meet the operand with the established type; everything else is `⊤`.
//!
//! # Rationale
//!
//! Flow sensitivity is scoped by dominance rather than by per-edge
//! environments: a refinement attached to a block is valid throughout the
//! region that block dominates, which is cheap to compute and exactly what a
//! counted loop needs (`i < n` on the sole entry to the body proves
//! `i ∈ [lo, n-1]`, letting the index's overflow guard be eliminated later).
//! Edge refinements are only recorded when the target has exactly one incoming
//! edge from the branching block, not merely one distinct predecessor block.
//! Results are a side table so the analysis is non-destructive and any
//! consumer can rerun it on whatever IR it currently holds.
//!
//! # Limits
//!
//! * Each value gets one fact from its definition onward; a guard narrows the
//!   operand only in the dominated region's local environment, not globally.
//!   Speculation reifies guards as fresh SSA values for that reason.
//! * Range tracking covers add, sub, mul and neg only.
//! * Class ids are propagated but never derived here.
//! * Widening plus narrowing can be less precise than the least fixpoint where
//!   a bound is carried around a loop without a comparison that re-establishes
//!   it. The ascending phase is capped at `MAX_PASSES`; a function that has
//!   not converged by then discards unfinished facts to TOP and is not narrowed.
//!
//! # Debugging
//!
//! * `EGCL_IR_DUMP` (any value) makes the `TypeInference` pass print the
//!   number of values with a non-`⊥` fact to stderr.

use std::collections::{HashMap, HashSet};

use crate::t2::ir::{
    AuxData, Block, DominatorTree, Function, IRType, Inst, Opcode, Range, TypeBits, Value,
    ValueDef, ValueRepresentation,
};
use crate::t2::pass::{Analyses, Pass};

// ── Result side table ───────────────────────────────────────────────

/// The inferred facts, produced by [`infer`]. Owned by the pass because the
/// frozen `ir` contract has no public setter for a value's inferred type.
#[derive(Clone, Debug, Default)]
pub struct InferenceResult {
    types: HashMap<Value, IRType>,
    reprs: HashMap<Value, ValueRepresentation>,
    structural: super::value_facts::StructuralFacts,
    before: HashMap<(Inst, Value), IRType>,
}

impl InferenceResult {
    /// Structural evidence from the same immutable IR snapshot as numeric facts.
    pub fn structural(&self) -> &super::value_facts::StructuralFacts {
        &self.structural
    }

    /// The inferred `IRType` for `v` (`⊥` if the value was never reached).
    pub fn ty(&self, v: Value) -> IRType {
        self.types.get(&v).copied().unwrap_or(IRType::BOTTOM)
    }

    /// Fact immediately before a requested instruction, for one of its operands.
    /// Unrequested sites/values conservatively use the ordinary global fact.
    /// This is a snapshot: rebuild after any IR/CFG/OSR change.
    pub fn ty_before(&self, inst: Inst, value: Value) -> IRType {
        self.before
            .get(&(inst, value))
            .copied()
            .unwrap_or_else(|| self.ty(value))
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
        if std::env::var_os("EGCL_IR_DUMP").is_some() {
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

/// Widening (▽): extend a still-growing bound to the next threshold — the
/// fixnum bound, then saturation — so the ascending chain terminates. Sound
/// (over-approximates); narrowing recovers precision afterwards.
fn ty_widen(old: IRType, new: IRType) -> IRType {
    let range = match (old.range, new.range) {
        (Some(o), Some(n)) => Some(Range {
            lo: if n.lo >= o.lo {
                o.lo
            } else if n.lo >= FIXNUM_BOUNDS.lo {
                FIXNUM_BOUNDS.lo
            } else {
                i64::MIN
            },
            hi: if n.hi <= o.hi {
                o.hi
            } else if n.hi <= FIXNUM_BOUNDS.hi {
                FIXNUM_BOUNDS.hi
            } else {
                i64::MAX
            },
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

// These facts describe tagged fixnums after their guards. In particular, a
// signed i64 range is not a proof that a value fits the tagged representation.
const FIXNUM_BOUNDS: Range = Range {
    lo: -(1i64 << 60),
    hi: (1i64 << 60) - 1,
};

fn bounded_fixnum_operand(operand: &IRType) -> Option<Range> {
    if operand.bits != TypeBits::FIXNUM {
        return None;
    }
    let range = operand.range.unwrap_or(FIXNUM_BOUNDS);
    range_meet(range, FIXNUM_BOUNDS)
}

fn constant_shift_range(opcode: Opcode, ops: &[IRType]) -> Option<Range> {
    let input = bounded_fixnum_operand(ops.first()?)?;
    let count = bounded_fixnum_operand(ops.get(1)?)?;
    if count.lo != count.hi {
        return None;
    }
    let count = count.lo;
    let right = match opcode {
        // ASH's signed count: negative means arithmetic right shift. Avoid
        // negating the count, which can overflow for an extreme input fact.
        Opcode::FixnumShl if count < 0 => Some(count.unsigned_abs().min(63) as u32),
        // This IR opcode has an unsigned-direction count, unlike ASH. Only
        // the supported nonnegative machine-count domain is inferred here.
        Opcode::FixnumShr if (0..64).contains(&count) => Some(count as u32),
        Opcode::FixnumShr => return None,
        _ => None,
    };
    if let Some(count) = right {
        return Some(Range {
            lo: input.lo >> count,
            hi: input.hi >> count,
        });
    }
    if input.lo == 0 && input.hi == 0 {
        return Some(input);
    }
    if count > 60 {
        return None;
    }
    let lo = (input.lo as i128) << count;
    let hi = (input.hi as i128) << count;
    // Do not use wrapping shifts, or claim a bounded result when a left shift
    // can overflow its fixnum domain. The existing overflow guard remains.
    (lo >= FIXNUM_BOUNDS.lo as i128 && hi <= FIXNUM_BOUNDS.hi as i128).then_some(Range {
        lo: lo as i64,
        hi: hi as i64,
    })
}

fn positive_mask_range(ops: &[IRType]) -> Option<Range> {
    let a = bounded_fixnum_operand(ops.first()?)?;
    let b = bounded_fixnum_operand(ops.get(1)?)?;
    let (mask, input) = if a.lo == a.hi && a.lo >= 0 {
        (a.lo, b)
    } else if b.lo == b.hi && b.lo >= 0 {
        (b.lo, a)
    } else {
        return None;
    };
    // For a nonnegative input AND cannot exceed the input either. Negative
    // inputs still produce a nonnegative result bounded by the mask.
    Some(Range {
        lo: 0,
        hi: if input.lo >= 0 {
            mask.min(input.hi)
        } else {
            mask
        },
    })
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
        ConstDouble | FloatSqrt | FloatNeg => return IRType::of(TypeBits::DOUBLE_FLOAT),
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
        FixnumShl | FixnumShr | LogAnd => {
            let range = if opcode == LogAnd {
                positive_mask_range(ops)
            } else {
                constant_shift_range(opcode, ops)
            };
            IRType {
                range: range.or(Some(FULL)),
                ..IRType::of(TypeBits::FIXNUM)
            }
        }
        TryIntegerShr
        | TryStoreIntegerOctets8
        | TryStoreIntegerOctets1To8
        | TryStoreIntegerOctets1To8Layout
        | TryStoreIntegerOctets1To8Extent => IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
        PackedU8Layout | PackedU8AllocationLayout | PackedU8AllocationExtent => {
            IRType::of(TypeBits::FIXNUM.join(TypeBits::NULL))
        }
        IntegerLowBits => match aux {
            AuxData::FixnumImm(mask) if (0..=(1i64 << 60) - 1).contains(mask) => IRType {
                range: Some(Range { lo: 0, hi: *mask }),
                ..IRType::of(TypeBits::FIXNUM)
            },
            _ => IRType::TOP,
        },
        // Fixnum ops whose range we do not track precisely: known FIXNUM, full range.
        FixnumDiv | FixnumFloor | FixnumMin | FixnumMax | FixnumRem | FixnumMod | LogOr
        | LogXor | LogNot | WidenI32 | UnboxFixnum | BoxFixnum => fixnum_full(),

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
        TryPackedF64Set | PackedF64P => IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
        PackedF64Ref | VecF64Ref => IRType::of(TypeBits::DOUBLE_FLOAT),
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
        | FloatCmpLt | FloatCmpLe | FloatCmpGt | FloatCmpGe | GenericEq | GenericEqual
        | InstanceOf => boolean(),

        // A predicate produces a Boolean; it never establishes its truth.
        TypeCheck | StructTypeP => boolean(),
        // Only successful guards refine their operand and passed-through value.
        Guard => ty_meet(
            ops.first().copied().unwrap_or(IRType::TOP),
            guard_type(aux).unwrap_or(IRType::TOP),
        ),

        AllocCons => IRType::of(TypeBits::CONS),
        StringCharOrNil => IRType::of(TypeBits::CHARACTER.join(TypeBits::NULL)),

        // Everything else (loads, calls, generic memory) is unconstrained.
        _ => IRType::TOP,
    }
}

/// The type established by a successful guard. Layout -> type is sound;
/// the reverse implication does not establish a concrete string layout.
fn guard_type(aux: &AuxData) -> Option<IRType> {
    match aux {
        AuxData::TypeTag(tag) => Some(*tag),
        AuxData::StringLayout => Some(IRType::of(TypeBits::STRING)),
        _ => None,
    }
}

/// Predicate semantics are not always the raw representation bitset: SYMBOLP
/// also accepts NIL. Admit only understood tag predicates, without untested
/// range/class annotations. New predicates need an explicit semantic rule.
fn predicate_true_type(bits: TypeBits) -> Option<IRType> {
    let bits = if bits == TypeBits::SYMBOL {
        bits.join(TypeBits::NULL)
    } else if bits == TypeBits::FIXNUM
        || bits == TypeBits::FIXNUM.join(TypeBits::BIGNUM)
        || bits == TypeBits::STRING
        || bits == TypeBits::DOUBLE_FLOAT
    {
        bits
    } else {
        return None;
    };
    Some(IRType::of(bits))
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

/// Loop-header parameters are widened from this pass on. A loop accumulator
/// never converges without widening, so the former 400-pass delay made every
/// loop pay all 400 passes (97% of inference time under `EGCL_FORCE_TIER=t2`).
const WIDEN_AFTER: usize = 2;
/// Work budget for ascending passes. Numeric range widening usually converges
/// sooner, but type bits can require arbitrarily long phi chains. On exhaustion
/// discard unfinished facts to TOP rather than narrowing or publishing them.
const MAX_PASSES: usize = 64;
/// Descending passes from the post-fixpoint, to recover bounds that widening
/// discarded (`i ∈ [0, 10]` for `i < 10` rather than the widened fixnum range).
const NARROWING_PASSES: usize = 2;

/// Targets of retreating edges in reverse postorder. Every CFG cycle, even an
/// irreducible one, contains such an edge, so widening these parameters alone
/// terminates the ascending chain.
fn loop_headers(rpo: &[Block], predecessors: &[Vec<Block>]) -> HashSet<Block> {
    let mut order = HashMap::new();
    for (index, &b) in rpo.iter().enumerate() {
        order.insert(b, index);
    }
    rpo.iter()
        .copied()
        .filter(|b| {
            predecessors[b.index()]
                .iter()
                .any(|p| order.get(p).is_some_and(|&pi| pi >= order[b]))
        })
        .collect()
}

/// A fresh BYTE descriptor may be mutable in this implementation. Infer an
/// LDB result only while that descriptor has one executable consumer, crosses
/// normal single-predecessor edges, and encounters no opaque effects. Calls
/// remain in the IR: these are facts about successful normal returns only.
fn fresh_ldb_facts(f: &Function, predecessors: &[Vec<Block>]) -> HashMap<Value, IRType> {
    use super::builtin_binding::{Binding, snapshot};
    let canonical = |data: &super::ir::InstData, key: &str| {
        let AuxData::CallTarget(symbol) = data.aux else {
            return false;
        };
        matches!(data.opcode, Opcode::Call | Opcode::Invoke)
            && egcl_rt::symbols::registry_key(symbol).as_deref() == Some(key)
            && snapshot(symbol) == Some(Binding::FixedStandard)
    };
    let constant = |value| {
        let ValueDef::Result { inst, .. } = f.value(value).def else {
            return None;
        };
        let data = f.inst(inst);
        match (&data.opcode, &data.aux) {
            (Opcode::ConstFixnum, AuxData::FixnumImm(n)) => Some(*n),
            _ => None,
        }
    };
    let mut uses = HashMap::<Value, usize>::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            for &value in data
                .args
                .iter()
                .chain(data.targets.iter().flat_map(|edge| &edge.args))
            {
                *uses.entry(value).or_default() += 1;
            }
        }
    }
    let mut facts = HashMap::new();
    for &start in f.block_order() {
        for (position, &inst) in f.block(start).insts.iter().enumerate() {
            let ldb = f.inst(inst);
            if !canonical(ldb, "LDB") || ldb.args.len() != 2 || ldb.results.len() != 1 {
                continue;
            }
            let mut descriptor = ldb.args[0];
            let mut block = start;
            let mut end = position;
            let mut visited = std::collections::HashSet::new();
            let width = 'trace: loop {
                if !visited.insert(block)
                    || f.osr_entries.iter().any(|entry| entry.block == block)
                    || uses.get(&descriptor) != Some(&1)
                {
                    break None;
                }
                for &previous in f.block(block).insts[..end].iter().rev() {
                    let data = f.inst(previous);
                    if data.results.contains(&descriptor) {
                        if !canonical(data, "BYTE")
                            || data.args.len() != 2
                            || data.results.len() != 1
                        {
                            break 'trace None;
                        }
                        let Some(width) = constant(data.args[0]) else {
                            break 'trace None;
                        };
                        let Some(position) = constant(data.args[1]) else {
                            break 'trace None;
                        };
                        break 'trace ((0..=60).contains(&width) && position >= 0).then_some(width);
                    }
                    if data.opcode != Opcode::ClearMv
                        && (data.flags.effectful
                            || data.flags.call
                            || data.flags.safepoint
                            || matches!(data.opcode, Opcode::Call | Opcode::Invoke))
                    {
                        break 'trace None;
                    }
                }
                let [predecessor] = predecessors[block.index()].as_slice() else {
                    break None;
                };
                let instructions = &f.block(*predecessor).insts;
                let Some(&terminator) = instructions.last() else {
                    break None;
                };
                let edge = f.inst(terminator);
                // An Invoke's first edge is its successful normal return.
                if !matches!(edge.opcode, Opcode::Jump | Opcode::Invoke)
                    || edge
                        .targets
                        .first()
                        .is_none_or(|target| target.block != block)
                {
                    break None;
                }
                if let ValueDef::Param { block: owner, num } = f.value(descriptor).def {
                    if owner == block {
                        let Some(&argument) = edge.targets[0].args.get(num as usize) else {
                            break None;
                        };
                        descriptor = argument;
                    }
                }
                block = *predecessor;
                // BYTE itself can be the predecessor's Invoke terminator;
                // inspect it before rejecting intervening calls.
                end = instructions.len() - usize::from(edge.opcode == Opcode::Jump);
            };
            if let Some(width) = width {
                facts.insert(
                    ldb.results[0],
                    IRType {
                        bits: TypeBits::FIXNUM,
                        range: Some(Range {
                            lo: 0,
                            hi: (1i64 << width) - 1,
                        }),
                        class_id: None,
                    },
                );
            }
        }
    }
    facts
}

// Presence-enabled compiler diagnostics, independent of inference semantics.
// All subphase intervals are inclusive where documented in the output schema.
#[derive(Default)]
struct InferenceTimings {
    start: Option<std::time::Instant>,
    setup_ns: u128,
    checks_ns: u128,
    fixpoint_ns: u128,
    entry_ns: u128,
    edge_ns: u128,
    replay_ns: u128,
    replay_entry_ns: u128,
    repr_ns: u128,
    passes: usize,
    block_visits: usize,
    replay_blocks: usize,
    replay_insts: usize,
    entry_calls: usize,
    idom_steps: usize,
    dominance_queries: usize,
}

impl InferenceTimings {
    fn new() -> Self {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| std::env::var_os("EGCL_INFER_PHASE_LOG").is_some());
        Self {
            start: enabled.then(std::time::Instant::now),
            ..Self::default()
        }
    }

    fn clock(&self) -> Option<std::time::Instant> {
        self.start.map(|_| std::time::Instant::now())
    }

    fn elapsed(start: Option<std::time::Instant>) -> u128 {
        start.map_or(0, |start| start.elapsed().as_nanos())
    }

    fn finish(&self, f: &Function, blocks: usize, sites: usize) {
        let Some(start) = self.start else { return };
        eprintln!(
            "[infer-phases] function={:?} total_ns={} setup_ns={} checks_ns={} fixpoint_ns={} entry_ns={} edge_ns={} replay_ns={} replay_entry_ns={} repr_ns={} passes={} blocks={} block_visits={} replay_blocks={} replay_insts={} sites={} entry_calls={} idom_steps={} dominance_queries={}",
            f.name(),
            start.elapsed().as_nanos(),
            self.setup_ns,
            self.checks_ns,
            self.fixpoint_ns,
            self.entry_ns,
            self.edge_ns,
            self.replay_ns,
            self.replay_entry_ns,
            self.repr_ns,
            self.passes,
            blocks,
            self.block_visits,
            self.replay_blocks,
            self.replay_insts,
            sites,
            self.entry_calls,
            self.idom_steps,
            self.dominance_queries
        );
    }
}

/// Infer types, ranges, and representations for every reachable value in `f`.
pub fn infer(f: &Function) -> InferenceResult {
    infer_for_sites(f, &[])
}

/// Run inference once and capture operand facts only at the requested sites.
/// Scoped facts are replayed against the frozen conservative global solution
/// (converged facts or the TOP fallback), never an unfinished ascending prefix.
pub fn infer_for_sites(f: &Function, sites: &[Inst]) -> InferenceResult {
    let mut timings = InferenceTimings::new();
    let setup_start = timings.clock();
    // The IR is frozen throughout this invocation. Preserve Function::preds
    // ordering/deduplication once, rather than rescanning the whole CFG in
    // every widening pass and for every candidate comparison edge.
    let predecessors = f.pred_lists();
    let ldb_facts = fresh_ldb_facts(f, &predecessors);
    let structural = super::value_facts::StructuralFacts::analyze(f);
    let rpo = f.reverse_postorder();
    let dom = f.dominators();
    let mut g = Facts(HashMap::new());

    // Seed entry / pred-less block parameters from their declared type once.
    for &b in &rpo {
        if b == f.entry() || predecessors[b.index()].is_empty() {
            for &p in &f.block(b).params {
                g.0.insert(p, f.value(p).ty);
            }
        }
    }
    // An OSR entry hands a loop header parameters straight from an interpreter
    // frame, bypassing every IR edge. A fact joined only over those edges is
    // not a fact about such a value, so seed each unchecked parameter of an
    // OSR target from its declared type too. A parameter the entry checks
    // (`OsrEntry::checks`) is proven on that path as well, so it keeps the
    // edge-derived fact. So does a parameter nothing on the fast path reads —
    // one named only by deopt states: the entry never imports it, its home is
    // NIL-filled there, and the only reader is the deopt stub, so its edge
    // fact stays an honest description of what can be observed (bliss-5yz5h).
    let fast_path_use = |p: Value| -> bool {
        f.block_order().iter().any(|&b| {
            f.block(b).insts.iter().any(|&i| {
                let d = f.inst(i);
                d.args.contains(&p) || d.targets.iter().any(|t| t.args.contains(&p))
            })
        })
    };
    for osr in &f.osr_entries {
        for &p in &f.block(osr.block).params {
            if osr.checks.iter().any(|&(v, _)| v == p) || !fast_path_use(p) {
                continue;
            }
            let seeded = ty_join(g.get(p), f.value(p).ty);
            g.0.insert(p, seeded);
        }
    }

    // These contracts depend only on the frozen IR, never the evolving
    // lattice solution. Share one snapshot across widening iterations and the
    // final requested-site replay; comparison edge bounds remain per-pass.
    timings.setup_ns = InferenceTimings::elapsed(setup_start);
    let checks_start = timings.clock();
    let check_refine = check_refinements(f, &rpo);
    timings.checks_ns = InferenceTimings::elapsed(checks_start);
    let fixpoint_start = timings.clock();
    let headers = loop_headers(&rpo, &predecessors);
    // Parameter facts that hold independent of any IR edge (entry declarations,
    // OSR imports). Narrowing recomputes parameters from their edges, so it
    // must keep joining these in.
    let seeds = g.0.clone();
    let mut narrowing: Option<usize> = None;
    // Without a widening step the ascent reached the least fixpoint exactly,
    // and narrowing has nothing to recover.
    let mut widened = false;

    let mut converged = false;
    for pass in 0.. {
        if narrowing.is_none() && pass >= MAX_PASSES {
            break;
        }
        if timings.start.is_some() {
            timings.passes += 1;
        }
        let widen = narrowing.is_none() && pass >= WIDEN_AFTER;
        let narrow = narrowing.is_some();
        let mut changed = false;

        // Recompute branch/guard refinements against the current facts. A
        // refinement attached to block T is valid throughout the region T
        // dominates. `edge_refine` holds facts known on *entry* to T; `check_refine`
        // holds successful `Guard` narrowings, valid only in blocks T strictly
        // dominates (a use before the check in T itself is handled in program order).
        let mut edge_refine: HashMap<usize, (super::ir::Block, Vec<Refine>)> = HashMap::new();

        for &b in &rpo {
            if timings.start.is_some() {
                timings.block_visits += 1;
            }
            // 1. Block parameter facts = join over predecessor edges (or the
            //    declared type for pred-less blocks, seeded above).
            let preds = predecessors[b.index()].as_slice();
            if !preds.is_empty() {
                let params = f.block(b).params.clone();
                for (num, &p) in params.iter().enumerate() {
                    let mut acc = IRType::BOTTOM;
                    for &pred in preds {
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
                    let next = if narrow {
                        ty_join(seeds.get(&p).copied().unwrap_or(IRType::BOTTOM), acc)
                    } else {
                        let joined = ty_join(old, acc);
                        if widen && headers.contains(&b) {
                            let next = ty_widen(old, joined);
                            widened |= next != joined;
                            next
                        } else {
                            joined
                        }
                    };
                    if next != old {
                        g.0.insert(p, next);
                        changed = true;
                    }
                }
            }

            let mut env = entry_environment(
                b,
                &g,
                &dom,
                &structural,
                &edge_refine,
                &check_refine,
                &mut timings,
            );

            // 3. Walk instructions in program order, using the local environment.
            let fact = |v: Value, env: &HashMap<Value, IRType>, g: &Facts| -> IRType {
                env.get(&v).copied().unwrap_or_else(|| g.get(v))
            };
            for &inst in &f.block(b).insts {
                let idef = f.inst(inst);
                // Invoke terminates its block but still defines the call's
                // normal result. Leaving it at BOTTOM makes successor joins
                // incorrectly inherit only the other incoming values' types.
                if idef.opcode.is_terminator() && idef.results.is_empty() {
                    continue;
                }
                let ops: Vec<IRType> = idef.args.iter().map(|&a| fact(a, &env, &g)).collect();
                let res = idef
                    .results
                    .first()
                    .and_then(|value| ldb_facts.get(value))
                    .copied()
                    .unwrap_or_else(|| eval(idef.opcode, &idef.aux, &ops));
                for (index, &rv) in idef.results.iter().enumerate() {
                    let res = if matches!(
                        idef.opcode,
                        Opcode::TryIntegerShr
                            | Opcode::TryStoreIntegerOctets8
                            | Opcode::TryStoreIntegerOctets1To8
                            | Opcode::TryStoreIntegerOctets1To8Layout
                            | Opcode::TryStoreIntegerOctets1To8Extent
                    ) && index == 1
                    {
                        IRType::of(TypeBits::FIXNUM)
                    } else if idef.opcode == Opcode::ConstHeapObj
                        && f.value(rv).ty.bits == TypeBits::DOUBLE_FLOAT
                        && matches!(idef.aux, AuxData::HeapLiteral { slot }
                            if f.double_literals.contains_key(&slot))
                    {
                        // The builder captured an immutable double literal on
                        // the owner thread. Preserve that semantic type through
                        // joins without inspecting a movable heap object.
                        IRType::of(TypeBits::DOUBLE_FLOAT)
                    } else {
                        res
                    };
                    let old = g.get(rv);
                    let joined = if narrow { res } else { ty_join(old, res) };
                    if joined != old {
                        g.0.insert(rv, joined);
                        changed = true;
                    }
                    env.insert(rv, joined);
                }
                // Checked operations establish original-argument facts only
                // on this successful continuation, after their own pre-state.
                apply_normal_return_facts(f, inst, &g, &mut env);
            }
            // Compute outgoing bounds only after this block's instructions:
            // its comparison operands (including constants) now have facts.
            // Global facts still accumulate monotonically, so a loop-carried
            // bound that grows on a later pass weakens the inferred result.
            let edge_start = timings.clock();
            add_edge_refinements(f, b, &g, &predecessors, &mut edge_refine);
            timings.edge_ns += InferenceTimings::elapsed(edge_start);
        }

        if narrowing.is_none() && !changed {
            converged = true;
        }
        // Descending from a post-fixpoint stays above the least fixpoint, so
        // stopping after any narrowing pass is sound.
        narrowing = match narrowing {
            None if changed => None,
            None if !widened => break,
            None => Some(NARROWING_PASSES),
            Some(left) if changed && left > 1 => Some(left - 1),
            Some(_) => break,
        };
    }

    if !converged {
        // Numeric range widening does not finish arbitrary type-bit chains.
        // An unfinished ascending iterate can exclude reachable values, so do
        // not expose it to guard removal, representation choice or site facts.
        // Include values still absent/BOTTOM, not only existing map entries.
        for &block in &rpo {
            for &parameter in &f.block(block).params {
                g.0.insert(parameter, IRType::TOP);
            }
            for &inst in &f.block(block).insts {
                for &result in &f.inst(inst).results {
                    g.0.insert(result, IRType::TOP);
                }
            }
        }
    }

    timings.fixpoint_ns = InferenceTimings::elapsed(fixpoint_start);
    // Derive representations from the final types.
    let repr_start = timings.clock();
    let mut reprs = HashMap::new();
    for (&v, &t) in &g.0 {
        reprs.insert(v, repr_of(t));
    }
    timings.repr_ns = InferenceTimings::elapsed(repr_start);
    let replay_start = timings.clock();
    let entry_before_replay = timings.entry_ns;
    let before = if sites.is_empty() {
        HashMap::new()
    } else {
        capture_site_facts(
            f,
            sites,
            &g,
            &rpo,
            &dom,
            &structural,
            CaptureContext {
                checks: &check_refine,
                predecessors: &predecessors,
                timings: &mut timings,
            },
        )
    };
    timings.replay_ns = InferenceTimings::elapsed(replay_start);
    timings.replay_entry_ns = timings.entry_ns - entry_before_replay;
    timings.finish(f, rpo.len(), sites.len());
    InferenceResult {
        types: g.0,
        reprs,
        structural,
        before,
    }
}

type EdgeRefinements = HashMap<usize, (Block, Vec<Refine>)>;
type CheckRefinements = HashMap<usize, Vec<Refine>>;

fn check_refinements(f: &Function, rpo: &[Block]) -> CheckRefinements {
    let mut check_refine: CheckRefinements = HashMap::new();
    for &b in rpo {
        // Audited normal-return argument facts, for strictly dominated blocks.
        for &inst in &f.block(b).insts {
            super::normal_return_facts::visit_argument_facts(f, inst, |value, ty| {
                check_refine
                    .entry(b.index())
                    .or_default()
                    .push(Refine { value, ty });
            });
        }
    }

    check_refine
}

fn add_edge_refinements(
    f: &Function,
    b: Block,
    g: &Facts,
    predecessors: &[Vec<Block>],
    edge_refine: &mut EdgeRefinements,
) {
    if let Some(term) = f.terminator(b) {
        let t = f.inst(term);
        if t.opcode == Opcode::Brif && !t.args.is_empty() {
            let cond = t.args[0];
            if let ValueDef::Result { inst, .. } = f.value(cond).def {
                let cdef = f.inst(inst);
                // A true predicate edge narrows only its unique target
                // and dominated region, never the false edge or uses
                // before the branch. OSR imports can bypass the test;
                // leave this optional refinement off in OSR functions.
                if cdef.opcode == Opcode::TypeCheck
                    && cdef.args.len() == 1
                    && f.osr_entries.is_empty()
                {
                    if let (AuxData::TypeTag(tag), Some(call)) = (&cdef.aux, t.targets.first()) {
                        let tgt = call.block;
                        if tgt != f.entry()
                            && predecessors[tgt.index()].as_slice() == [b]
                            && t.targets.iter().filter(|c| c.block == tgt).count() == 1
                        {
                            if let Some(ty) = predicate_true_type(tag.bits) {
                                edge_refine
                                    .entry(tgt.index())
                                    .or_insert_with(|| (b, Vec::new()))
                                    .1
                                    .push(Refine {
                                        value: cdef.args[0],
                                        ty,
                                    });
                            }
                        }
                    }
                }
                if cdef.args.len() == 2 {
                    let (ca, cb) = (cdef.args[0], cdef.args[1]);
                    for (edge, taken) in [(0usize, true), (1usize, false)] {
                        if let Some(call) = t.targets.get(edge) {
                            let tgt = call.block;
                            // preds() deduplicates blocks. Two branch
                            // outcomes entering the same block do not
                            // establish either outcome's constraint.
                            if tgt != f.entry()
                                && predecessors[tgt.index()].as_slice() == [b]
                                && t.targets.iter().filter(|c| c.block == tgt).count() == 1
                            {
                                let refs = narrow_cmp(cdef.opcode, ca, cb, taken, g);
                                edge_refine
                                    .entry(tgt.index())
                                    .or_insert_with(|| (b, Vec::new()))
                                    .1
                                    .extend(refs);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn entry_environment(
    b: Block,
    g: &Facts,
    dom: &DominatorTree,
    structural: &super::value_facts::StructuralFacts,
    edge_refine: &EdgeRefinements,
    check_refine: &CheckRefinements,
    timings: &mut InferenceTimings,
) -> HashMap<Value, IRType> {
    let entry_start = timings.clock();
    if timings.start.is_some() {
        timings.entry_calls += 1;
    }
    let mut env: HashMap<Value, IRType> = HashMap::new();
    let mut d = b;
    loop {
        if timings.start.is_some() {
            timings.idom_steps += 1;
        }
        if let Some((_, refs)) = edge_refine.get(&d.index()).filter(|(source, _)| {
            if timings.start.is_some() {
                timings.dominance_queries += 1;
            }
            structural.block_dominates_all_entries(*source, b)
        }) {
            for rf in refs {
                let cur = env
                    .get(&rf.value)
                    .copied()
                    .unwrap_or_else(|| g.get(rf.value));
                let refined = ty_meet(cur, rf.ty);
                if refined != cur {
                    env.insert(rf.value, refined);
                }
            }
        }
        if d != b {
            if let Some(refs) = check_refine.get(&d.index()).filter(|_| {
                if timings.start.is_some() {
                    timings.dominance_queries += 1;
                }
                structural.block_dominates_all_entries(d, b)
            }) {
                for rf in refs {
                    let cur = env
                        .get(&rf.value)
                        .copied()
                        .unwrap_or_else(|| g.get(rf.value));
                    let refined = ty_meet(cur, rf.ty);
                    if refined != cur {
                        env.insert(rf.value, refined);
                    }
                }
            }
        }
        match dom.idom(d) {
            Some(id) if id != d => d = id,
            _ => break,
        }
    }

    timings.entry_ns += InferenceTimings::elapsed(entry_start);
    env
}

fn apply_normal_return_facts(
    f: &Function,
    inst: Inst,
    g: &Facts,
    env: &mut HashMap<Value, IRType>,
) {
    super::normal_return_facts::visit_argument_facts(f, inst, |value, ty| {
        let current = env.get(&value).copied().unwrap_or_else(|| g.get(value));
        let refined = ty_meet(current, ty);
        if refined != current {
            env.insert(value, refined);
        }
    });
}

struct CaptureContext<'a> {
    checks: &'a CheckRefinements,
    predecessors: &'a [Vec<Block>],
    timings: &'a mut InferenceTimings,
}

fn capture_site_facts(
    f: &Function,
    sites: &[Inst],
    g: &Facts,
    rpo: &[Block],
    dom: &DominatorTree,
    structural: &super::value_facts::StructuralFacts,
    context: CaptureContext<'_>,
) -> HashMap<(Inst, Value), IRType> {
    let CaptureContext {
        checks,
        predecessors,
        timings,
    } = context;
    let requested: HashSet<_> = sites.iter().copied().collect();
    let mut edges = EdgeRefinements::new();
    for &block in rpo {
        let edge_start = timings.clock();
        add_edge_refinements(f, block, g, predecessors, &mut edges);
        timings.edge_ns += InferenceTimings::elapsed(edge_start);
    }
    let mut before = HashMap::new();
    for &block in rpo {
        let instructions = &f.block(block).insts;
        let Some(last) = instructions
            .iter()
            .rposition(|inst| requested.contains(inst))
        else {
            continue;
        };
        // Each replay starts from conservative global facts (the converged
        // solution or TOP fallback) and complete edge/check maps. Other blocks'
        // local environments do not feed it.
        let mut env = entry_environment(block, g, dom, structural, &edges, checks, timings);
        if timings.start.is_some() {
            timings.replay_blocks += 1;
        }
        for &inst in &instructions[..=last] {
            if timings.start.is_some() {
                timings.replay_insts += 1;
            }
            let data = f.inst(inst);
            if requested.contains(&inst) {
                for &value in &data.args {
                    before.insert(
                        (inst, value),
                        env.get(&value).copied().unwrap_or_else(|| g.get(value)),
                    );
                }
            }
            // Its normal-return contract proves nothing about its pre-state.
            apply_normal_return_facts(f, inst, g, &mut env);
        }
    }
    before
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
    fn invoke_results_widen_successor_phi_facts() {
        let mut f = Function::new("invoke-phi");
        let entry = f.entry();
        let condition = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let call = f.make_block();
        let nil_path = f.make_block();
        let merge = f.make_block();
        let cold = f.make_block();
        let merged = f.add_block_param(merge, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(
            entry,
            InstData {
                args: vec![condition],
                targets: vec![
                    BlockCall {
                        block: call,
                        args: vec![],
                    },
                    BlockCall {
                        block: nil_path,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let (invoke, results) = f.push_inst(
            call,
            InstData {
                aux: AuxData::CallTarget(0),
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.inst_mut(invoke).opcode = Opcode::Invoke;
        f.inst_mut(invoke).flags.terminator = true;
        f.inst_mut(invoke).targets = vec![
            BlockCall {
                block: merge,
                args: vec![results[0]],
            },
            BlockCall {
                block: cold,
                args: vec![],
            },
        ];
        let (_, nil) = f.push_inst(
            nil_path,
            inst(Opcode::ConstNil),
            &[(IRType::of(TypeBits::NULL), ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            nil_path,
            InstData {
                targets: vec![BlockCall {
                    block: merge,
                    args: vec![nil[0]],
                }],
                ..inst(Opcode::Jump)
            },
        );
        f.set_terminator(merge, ret());
        f.set_terminator(cold, inst(Opcode::Trap));
        let inferred = infer(&f);
        assert_eq!(inferred.ty(results[0]), IRType::TOP);
        assert_eq!(
            inferred.ty(merged),
            IRType::TOP,
            "an unknown call result joined with NIL can still be a heap object"
        );
        assert_eq!(inferred.repr(merged), Some(ValueRepresentation::Tagged));
    }

    fn ldb_fixture(
        width: i64,
        position: i64,
        byte_name: &str,
        ldb_name: &str,
    ) -> (Function, Value, Value) {
        let mut f = Function::new("fresh-ldb");
        let entry = f.entry();
        let integer = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let width = const_fixnum(&mut f, entry, width);
        let position = const_fixnum(&mut f, entry, position);
        let (_, descriptor) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::CallTarget(egcl_rt::symbols::intern(byte_name)),
                args: vec![width, position],
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, result) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::CallTarget(egcl_rt::symbols::intern(ldb_name)),
                args: vec![descriptor[0], integer],
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(entry, ret());
        (f, descriptor[0], result[0])
    }

    #[test]
    fn fresh_canonical_ldb_bounds_preserve_signaling_calls() {
        for (width, position, expected) in [
            (0, 0, Some(0)),
            (8, 0, Some(255)),
            (60, 1000, Some((1i64 << 60) - 1)),
            (61, 0, None),
            (-1, 0, None),
            (8, -1, None),
        ] {
            let (f, _, result) = ldb_fixture(width, position, "BYTE", "LDB");
            let before = f.block(f.entry()).insts.clone();
            let facts = infer(&f);
            assert_eq!(facts.range(result), expected.map(|hi| Range { lo: 0, hi }));
            assert_eq!(f.block(f.entry()).insts, before);
            for &i in &before {
                let data = f.inst(i);
                if data.opcode == Opcode::Call {
                    assert!(data.flags.call && data.flags.effectful && data.flags.safepoint);
                }
            }
        }
        for (byte, ldb) in [("RANGE-TEST::BYTE", "LDB"), ("BYTE", "RANGE-TEST::LDB")] {
            let (f, _, result) = ldb_fixture(8, 0, byte, ldb);
            assert_eq!(infer(&f).ty(result), IRType::TOP);
        }
    }

    #[test]
    fn ldb_rejects_descriptor_escape_mutation_and_opaque_calls() {
        for escape in [false, true] {
            let (mut f, descriptor, result) = ldb_fixture(8, 0, "BYTE", "LDB");
            let entry = f.entry();
            let (call, _) = f.push_inst(
                entry,
                InstData {
                    args: if escape { vec![descriptor] } else { vec![] },
                    aux: AuxData::CallTarget(egcl_rt::symbols::intern("RANGE-TEST::MUTATE")),
                    flags: InstFlags {
                        call: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    ..inst(Opcode::Call)
                },
                &[],
            );
            assert_eq!(f.block_mut(entry).insts.pop(), Some(call));
            let ValueDef::Result { inst: ldb, .. } = f.value(result).def else {
                unreachable!()
            };
            let at = f.block(entry).insts.iter().position(|&i| i == ldb).unwrap();
            f.block_mut(entry).insts.insert(at, call);
            assert_eq!(infer(&f).ty(result), IRType::TOP);
        }
    }

    #[test]
    fn fresh_ldb_descriptor_crosses_only_unique_normal_edges() {
        let (mut f, descriptor, result) = ldb_fixture(8, 0, "BYTE", "LDB");
        let entry = f.entry();
        let normal = f.make_block();
        let exceptional = f.make_block();
        let alias = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let ValueDef::Result { inst: byte, .. } = f.value(descriptor).def else {
            unreachable!()
        };
        let ValueDef::Result { inst: ldb, .. } = f.value(result).def else {
            unreachable!()
        };
        // Move the LDB to the constructor's normal successor, carrying the
        // fresh descriptor as an edge parameter, as transfer IR does.
        f.block_mut(entry).insts.retain(|&i| i != ldb);
        f.block_mut(entry).insts.truncate(3);
        f.inst_mut(byte).opcode = Opcode::Invoke;
        f.inst_mut(byte).targets = vec![
            BlockCall {
                block: normal,
                args: vec![descriptor],
            },
            BlockCall {
                block: exceptional,
                args: vec![],
            },
        ];
        f.inst_mut(ldb).args[0] = alias;
        f.block_mut(normal).insts.push(ldb);
        f.set_terminator(normal, ret());
        f.set_terminator(exceptional, inst(Opcode::Trap));
        assert_eq!(infer(&f).range(result), Some(Range { lo: 0, hi: 255 }));
        // An exceptional alias is not evidence of successful construction.
        f.inst_mut(byte).targets.swap(0, 1);
        assert_eq!(infer(&f).ty(result), IRType::TOP);
    }

    fn ranged_fixnum(lo: i64, hi: i64) -> IRType {
        IRType {
            range: Some(Range { lo, hi }),
            ..fixnum()
        }
    }

    #[test]
    fn constant_shift_ranges_cover_signed_and_large_counts() {
        let cases = [
            (Opcode::FixnumShl, -9, 17, -3, -2, 2),
            (Opcode::FixnumShr, -9, 17, 3, -2, 2),
            (Opcode::FixnumShl, -9, 17, -1000, -1, 0),
            (Opcode::FixnumShl, 0, 17, -1000, 0, 0),
            (Opcode::FixnumShl, -17, -1, -1000, -1, -1),
            (Opcode::FixnumShl, -9, 17, 3, -72, 136),
            (Opcode::FixnumShl, -9, 17, 0, -9, 17),
            (Opcode::FixnumShl, 0, 0, FIXNUM_BOUNDS.hi, 0, 0),
        ];
        for (opcode, lo, hi, count, expected_lo, expected_hi) in cases {
            assert_eq!(
                eval(
                    opcode,
                    &AuxData::None,
                    &[ranged_fixnum(lo, hi), ranged_fixnum(count, count)]
                )
                .range,
                Some(Range {
                    lo: expected_lo,
                    hi: expected_hi
                })
            );
        }
        for (opcode, input, count) in [
            (
                Opcode::FixnumShl,
                ranged_fixnum(0, FIXNUM_BOUNDS.hi),
                ranged_fixnum(1, 1),
            ),
            (
                Opcode::FixnumShl,
                ranged_fixnum(-1, 1),
                ranged_fixnum(1000, 1000),
            ),
            (
                Opcode::FixnumShl,
                ranged_fixnum(0, 10),
                ranged_fixnum(-2, -1),
            ),
            (
                Opcode::FixnumShr,
                ranged_fixnum(0, 10),
                ranged_fixnum(-1, -1),
            ),
            (
                Opcode::FixnumShr,
                ranged_fixnum(0, 10),
                ranged_fixnum(64, 64),
            ),
            (Opcode::FixnumShl, IRType::TOP, ranged_fixnum(-8, -8)),
        ] {
            assert_eq!(
                eval(opcode, &AuxData::None, &[input, count]).range,
                Some(FULL)
            );
        }
    }

    #[test]
    fn byte_mask_and_shift_ranges_enclose_all_small_inputs() {
        // Enumerate concrete signed inputs rather than mirroring endpoint
        // formulas; arithmetic right shifts must round negative values down.
        for lo in -20..=20 {
            for hi in lo..=20 {
                for count in -8..=8 {
                    let range = eval(
                        Opcode::FixnumShl,
                        &AuxData::None,
                        &[ranged_fixnum(lo, hi), ranged_fixnum(count, count)],
                    )
                    .range
                    .unwrap();
                    for value in lo..=hi {
                        let actual = if count < 0 {
                            value >> -count
                        } else {
                            value << count
                        };
                        assert!((range.lo..=range.hi).contains(&actual));
                    }
                }
                for mask in [0, 1, 5, 15, 255] {
                    for operands in [
                        [ranged_fixnum(lo, hi), ranged_fixnum(mask, mask)],
                        [ranged_fixnum(mask, mask), ranged_fixnum(lo, hi)],
                    ] {
                        let range = eval(Opcode::LogAnd, &AuxData::None, &operands)
                            .range
                            .unwrap();
                        for value in lo..=hi {
                            assert!((range.lo..=range.hi).contains(&(value & mask)));
                        }
                    }
                }
            }
        }
        assert_eq!(
            eval(
                Opcode::LogAnd,
                &AuxData::None,
                &[fixnum(), ranged_fixnum(255, 255)]
            )
            .range,
            Some(Range { lo: 0, hi: 255 })
        );
        assert_eq!(
            eval(
                Opcode::LogAnd,
                &AuxData::None,
                &[fixnum(), ranged_fixnum(-1, -1)]
            )
            .range,
            Some(FULL)
        );
    }

    #[test]
    fn guarded_unsigned56_remainder_entry_keeps_shift_range() {
        let mut f = Function::new("shift-remainder");
        let entry = f.entry();
        // A peeled generic iteration's result guard establishes this fact;
        // the original unsigned64/bignum entry must never be given this type.
        let value = f.add_block_param(
            entry,
            ranged_fixnum(0, (1i64 << 56) - 1),
            ValueRepresentation::Tagged,
        );
        f.mark_entry_param_checked(value);
        let count = const_fixnum(&mut f, entry, -8);
        let (_, shifted) = f.push_inst(
            entry,
            InstData {
                args: vec![value, count],
                ..inst(Opcode::FixnumShl)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let mask = const_fixnum(&mut f, entry, 255);
        let (_, byte) = f.push_inst(
            entry,
            InstData {
                args: vec![shifted[0], mask],
                ..inst(Opcode::LogAnd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(entry, ret());
        let facts = infer(&f);
        assert_eq!(
            facts.range(shifted[0]),
            Some(Range {
                lo: 0,
                hi: (1i64 << 48) - 1
            })
        );
        assert_eq!(facts.range(byte[0]), Some(Range { lo: 0, hi: 255 }));
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
    fn typecheck_produces_a_tagged_boolean_without_narrowing_operand() {
        // entry: p: TOP ; r = TypeCheck(p, FIXNUM) is a pure Boolean predicate.
        let mut f = Function::new("tc");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let (_, r) = f.push_inst(
            e,
            InstData {
                args: vec![p],
                aux: AuxData::TypeTag(fixnum()),
                flags: InstFlags::default(),
                ..inst(Opcode::TypeCheck)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(e, ret());

        let res = infer(&f);
        // Real producers emit a pure predicate, not a failing guard.
        assert_eq!(res.ty(r[0]).bits, TypeBits::SYMBOL.join(TypeBits::NULL));
        assert_eq!(res.ty(p), IRType::TOP);
        assert_eq!(res.repr(r[0]), Some(ValueRepresentation::Tagged));
    }

    #[test]
    fn real_bytecode_predicates_build_boolean_typechecks() {
        use egcl_rt::bytecode::{BytecodeFunction, Instr, typep_class};
        for operation in [
            Instr::TypeP(typep_class::BOOLEAN),
            Instr::CallNamed {
                sym: egcl_rt::symbols::intern("INTEGERP"),
                nargs: 1,
            },
        ] {
            let body = BytecodeFunction {
                self_call_sites: vec![],
                notinline_sites: vec![],
                code: vec![Instr::LoadLocal(0), operation, Instr::Return],
                constants: vec![],
                load_time_values: vec![],
                handler_cases: vec![],
                handler_binds: vec![],
                names: vec![],
                restart_cases: vec![],
                nested_functions: vec![],
                param_layout: vec![],
                param_types: vec![],
                has_env: false,
                n_locals: 1,
                max_stack: 1,
                arity: 1,
                name: "inferred-predicate".into(),
                params_form: egcl_rt::value::NIL,
                min_args: 1,
                max_args: Some(1),
                variadic: false,
                binder_cache: Default::default(),
                closure_template: Default::default(),
            };
            let f = crate::t2::build::build_from_bytecode(&body).unwrap();
            let check = f
                .block_order()
                .iter()
                .flat_map(|&b| &f.block(b).insts)
                .map(|&i| f.inst(i))
                .find(|d| d.opcode == Opcode::TypeCheck)
                .unwrap();
            assert!(!check.flags.guard && !check.flags.effectful && check.frame_state.is_none());
            let facts = infer(&f);
            assert_eq!(
                facts.ty(check.results[0]).bits,
                TypeBits::SYMBOL.join(TypeBits::NULL)
            );
            assert_eq!(
                facts.repr(check.results[0]),
                Some(ValueRepresentation::Tagged)
            );
            assert_eq!(facts.ty(check.args[0]), IRType::TOP);
        }
    }

    #[test]
    fn predicate_refines_only_unique_true_edge_not_earlier_or_false_uses() {
        for (shared, osr) in [(false, false), (true, false), (false, true)] {
            let mut f = Function::new("predicate-regions");
            let entry = f.entry();
            let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
            let (_, predicate) = f.push_inst(
                entry,
                InstData {
                    args: vec![input],
                    aux: AuxData::TypeTag(IRType {
                        range: Some(Range { lo: 0, hi: 0 }),
                        ..fixnum()
                    }),
                    ..inst(Opcode::TypeCheck)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            let one = const_fixnum(&mut f, entry, 1);
            let (_, before) = f.push_inst(
                entry,
                InstData {
                    args: vec![input, one],
                    ..inst(Opcode::GenericAdd)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            // Reuse the Boolean value without treating it as a passed-through
            // operand or a proof that its own tested condition is true.
            let (_, bool_use) = f.push_inst(
                entry,
                InstData {
                    args: vec![predicate[0], predicate[0]],
                    ..inst(Opcode::GenericEq)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            let yes = f.make_block();
            let no = if shared { yes } else { f.make_block() };
            f.set_terminator(
                entry,
                InstData {
                    args: vec![predicate[0]],
                    targets: vec![
                        BlockCall {
                            block: yes,
                            args: vec![],
                        },
                        BlockCall {
                            block: no,
                            args: vec![],
                        },
                    ],
                    ..inst(Opcode::Brif)
                },
            );
            let (_, true_value) = f.push_inst(
                yes,
                InstData {
                    args: vec![input, one],
                    ..inst(Opcode::GenericAdd)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            f.set_terminator(yes, ret());
            let false_value = if shared {
                true_value[0]
            } else {
                let (_, v) = f.push_inst(
                    no,
                    InstData {
                        args: vec![input, one],
                        ..inst(Opcode::GenericAdd)
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                f.set_terminator(no, ret());
                v[0]
            };
            if osr {
                let state = f.frame_states.add(crate::t2::frame_state::FrameState {
                    scopes: vec![],
                    remat: vec![],
                });
                f.osr_entries.push(crate::t2::ir::OsrEntry {
                    block: yes,
                    bcp: 0,
                    frame_state: state,
                    checks: vec![],
                    control_scopes: vec![],
                });
            }
            let site = |value| match f.value(value).def {
                ValueDef::Result { inst, .. } => inst,
                _ => panic!("expected instruction result"),
            };
            let prior_site = site(before[0]);
            let true_site = site(true_value[0]);
            let false_site = site(false_value);
            let facts = infer_for_sites(&f, &[prior_site, true_site, false_site]);
            assert_eq!(facts.ty_before(prior_site, input), IRType::TOP);
            assert_eq!(facts.ty_before(false_site, input), IRType::TOP);
            assert_eq!(
                facts.ty_before(true_site, input),
                if !shared && !osr {
                    fixnum()
                } else {
                    IRType::TOP
                }
            );
            // The predecessor has no requested site in this replay. Its
            // branch contract still applies, unless an alternate OSR bypasses it.
            let only_successor = infer_for_sites(&f, &[true_site]);
            assert_eq!(
                only_successor.ty_before(true_site, input),
                facts.ty_before(true_site, input)
            );
            // Unrequested sites and nonoperands never gain scoped proofs.
            assert_eq!(facts.ty_before(site(bool_use[0]), input), facts.ty(input));
            assert_eq!(
                facts.ty_before(true_site, predicate[0]),
                facts.ty(predicate[0])
            );
            assert!(facts.ty(before[0]).bits.contains(TypeBits::DOUBLE_FLOAT));
            assert!(facts.ty(false_value).bits.contains(TypeBits::DOUBLE_FLOAT));
            assert_eq!(
                facts.ty(bool_use[0]).bits,
                TypeBits::SYMBOL.join(TypeBits::NULL)
            );
            if !shared && !osr {
                assert_eq!(
                    facts.ty(true_value[0]).bits,
                    TypeBits::FIXNUM.join(TypeBits::BIGNUM)
                );
                assert_eq!(
                    facts.range(true_value[0]),
                    None,
                    "tag predicate does not test numeric range"
                );
            } else {
                assert!(
                    facts
                        .ty(true_value[0])
                        .bits
                        .contains(TypeBits::DOUBLE_FLOAT)
                );
            }
            assert_eq!(facts.ty(input), IRType::TOP);
        }
    }

    #[test]
    fn predicate_backedge_does_not_refine_initial_function_entry() {
        let mut f = Function::new("predicate-entry-backedge");
        let e = f.entry();
        let input = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let (_, earlier) = f.push_inst(
            e,
            InstData {
                args: vec![input],
                ..inst(Opcode::Guard)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, predicate) = f.push_inst(
            e,
            InstData {
                args: vec![input],
                aux: AuxData::TypeTag(fixnum()),
                ..inst(Opcode::TypeCheck)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let exit = f.make_block();
        f.set_terminator(
            e,
            InstData {
                args: vec![predicate[0]],
                targets: vec![
                    BlockCall {
                        block: e,
                        args: vec![input],
                    },
                    BlockCall {
                        block: exit,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        f.set_terminator(exit, ret());
        let facts = infer(&f);
        assert_eq!(facts.ty(input), IRType::TOP);
        assert_eq!(
            facts.ty(earlier[0]),
            IRType::TOP,
            "the initial runtime entry bypasses the true backedge"
        );
    }

    #[test]
    fn symbol_predicate_true_edge_keeps_nil_in_its_semantic_type() {
        let mut f = Function::new("symbolp-nil");
        let e = f.entry();
        let input = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let (_, predicate) = f.push_inst(
            e,
            InstData {
                args: vec![input],
                aux: AuxData::TypeTag(IRType::of(TypeBits::SYMBOL)),
                ..inst(Opcode::TypeCheck)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let yes = f.make_block();
        let no = f.make_block();
        f.set_terminator(
            e,
            InstData {
                args: vec![predicate[0]],
                targets: vec![
                    BlockCall {
                        block: yes,
                        args: vec![],
                    },
                    BlockCall {
                        block: no,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let (_, copy) = f.push_inst(
            yes,
            InstData {
                args: vec![input],
                ..inst(Opcode::Guard)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(yes, ret());
        f.set_terminator(no, ret());
        let facts = infer(&f);
        assert_eq!(
            facts.ty(copy[0]).bits,
            TypeBits::SYMBOL.join(TypeBits::NULL)
        );
        assert_eq!(facts.ty(input), IRType::TOP);
    }

    #[test]
    fn osr_bypass_cannot_inherit_an_earlier_string_layout_type() {
        for bypass in [false, true] {
            let mut f = Function::new("layout-type-osr-bypass");
            let e = f.entry();
            let input = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
            let state = f.frame_states.add(crate::t2::frame_state::FrameState {
                scopes: vec![],
                remat: vec![],
            });
            f.push_inst(
                e,
                InstData {
                    args: vec![input],
                    aux: AuxData::StringLayout,
                    frame_state: Some(state),
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..Default::default()
                    },
                    ..inst(Opcode::Guard)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            let body = f.make_block();
            f.set_terminator(
                e,
                InstData {
                    targets: vec![BlockCall {
                        block: body,
                        args: vec![],
                    }],
                    ..inst(Opcode::Jump)
                },
            );
            let (_, observed) = f.push_inst(
                body,
                InstData {
                    args: vec![input],
                    aux: AuxData::TypeTag(IRType::TOP),
                    ..inst(Opcode::Guard)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            f.set_terminator(body, ret());
            f.osr_entries.push(crate::t2::ir::OsrEntry {
                block: if bypass { body } else { e },
                bcp: 0,
                frame_state: state,
                control_scopes: vec![],
                checks: vec![],
            });
            assert_eq!(
                infer(&f).ty(observed[0]).bits,
                if bypass {
                    TypeBits::TOP
                } else {
                    TypeBits::STRING
                }
            );
        }
    }

    #[test]
    fn string_layout_guard_implies_string_type_without_reverse_layout_claim() {
        let mut f = Function::new("string-layout-type");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let state = f.frame_states.add(crate::t2::frame_state::FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (_, checked) = f.push_inst(
            entry,
            InstData {
                args: vec![input],
                aux: AuxData::StringLayout,
                frame_state: Some(state),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                },
                ..inst(Opcode::Guard)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let (_, same_block) = f.push_inst(
            entry,
            InstData {
                args: vec![input],
                ..inst(Opcode::Guard)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        let successor = f.make_block();
        f.set_terminator(
            entry,
            InstData {
                targets: vec![BlockCall {
                    block: successor,
                    args: vec![],
                }],
                ..inst(Opcode::Jump)
            },
        );
        let (_, dominated) = f.push_inst(
            successor,
            InstData {
                args: vec![input],
                ..inst(Opcode::Guard)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(successor, ret());
        let facts = infer(&f);
        for value in [checked[0], same_block[0], dominated[0]] {
            assert_eq!(facts.ty(value).bits, TypeBits::STRING);
        }
        assert_eq!(
            facts.ty(input),
            IRType::TOP,
            "guard does not rewrite earlier/global input facts"
        );
    }

    #[test]
    fn guard_narrows_on_dominated_path() {
        // entry: p: TOP ; Guard(p : FIXNUM) ; jump B.
        // B: x = FixnumAdd(p, 1)  — p is narrowed to FIXNUM on the dominated block,
        // so the add is well-typed and x is FIXNUM.
        let mut f = Function::new("guard");
        let e = f.entry();
        let p = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let state = f.frame_states.add(crate::t2::frame_state::FrameState {
            scopes: vec![],
            remat: vec![],
        });
        f.push_inst(
            e,
            InstData {
                args: vec![p],
                aux: AuxData::TypeTag(fixnum()),
                frame_state: Some(state),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
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
    fn dominating_comparison_bounds_recursive_arguments() {
        let mut f = Function::new("bounded-recursive-arguments");
        let entry = f.entry();
        let n = f.add_block_param(
            entry,
            IRType {
                range: Some(Range { lo: 0, hi: 40 }),
                ..fixnum()
            },
            ValueRepresentation::Tagged,
        );
        let leaf = f.make_block();
        let recursive = f.make_block();
        let two = const_fixnum(&mut f, entry, 2);
        let (_, condition) = f.push_inst(
            entry,
            InstData {
                args: vec![n, two],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                args: vec![condition[0]],
                targets: vec![
                    BlockCall {
                        block: leaf,
                        args: vec![],
                    },
                    BlockCall {
                        block: recursive,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let one_at_leaf = const_fixnum(&mut f, leaf, 1);
        let (_, leaf_result) = f.push_inst(
            leaf,
            InstData {
                args: vec![n, one_at_leaf],
                ..inst(Opcode::FixnumSub)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(leaf, ret());
        let one = const_fixnum(&mut f, recursive, 1);
        let mut children = Vec::new();
        for amount in [one, two] {
            let (_, result) = f.push_inst(
                recursive,
                InstData {
                    args: vec![n, amount],
                    ..inst(Opcode::FixnumSub)
                },
                &[(fixnum(), ValueRepresentation::Tagged)],
            );
            children.push(result[0]);
        }
        f.set_terminator(recursive, ret());

        let inferred = infer(&f);
        assert_eq!(
            inferred.range(n),
            Some(Range { lo: 0, hi: 40 }),
            "the branch must not narrow the entry parameter globally"
        );
        assert_eq!(inferred.range(children[0]), Some(Range { lo: 1, hi: 39 }));
        assert_eq!(inferred.range(children[1]), Some(Range { lo: 0, hi: 38 }));
        assert_eq!(
            inferred.range(leaf_result[0]),
            Some(Range { lo: -1, hi: 0 })
        );
    }

    #[test]
    fn comparison_with_shared_successor_does_not_refine_either_outcome() {
        let mut f = Function::new("shared-comparison-successor");
        let entry = f.entry();
        let n = f.add_block_param(
            entry,
            IRType {
                range: Some(Range { lo: 0, hi: 40 }),
                ..fixnum()
            },
            ValueRepresentation::Tagged,
        );
        // Entry facts are known before the first inference pass. Neither
        // branch outcome may constrain the block that both edges enter.
        let bound = f.add_block_param(
            entry,
            IRType {
                range: Some(Range { lo: 2, hi: 2 }),
                ..fixnum()
            },
            ValueRepresentation::Tagged,
        );
        let successor = f.make_block();
        let (_, condition) = f.push_inst(
            entry,
            InstData {
                args: vec![n, bound],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            InstData {
                args: vec![condition[0]],
                targets: vec![
                    BlockCall {
                        block: successor,
                        args: vec![]
                    };
                    2
                ],
                ..inst(Opcode::Brif)
            },
        );
        let one = const_fixnum(&mut f, successor, 1);
        let (_, result) = f.push_inst(
            successor,
            InstData {
                args: vec![n, one],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        f.set_terminator(successor, ret());
        assert_eq!(infer(&f).range(result[0]), Some(Range { lo: 1, hi: 41 }));

        // A new inference invocation must observe a changed CFG, rather than
        // retaining the prior shared-edge predecessor snapshot.
        let separate_false = f.make_block();
        f.set_terminator(separate_false, ret());
        let branch = f.terminator(entry).unwrap();
        f.inst_mut(branch).targets[1].block = separate_false;
        assert_eq!(infer(&f).range(result[0]), Some(Range { lo: 1, hi: 2 }));
    }

    #[test]
    fn loop_carried_comparison_bound_weakens_later_refinements() {
        let mut f = Function::new("changing-comparison-bound");
        let entry = f.entry();
        let n = f.add_block_param(
            entry,
            IRType {
                range: Some(Range { lo: 0, hi: 40 }),
                ..fixnum()
            },
            ValueRepresentation::Tagged,
        );
        let initial = const_fixnum(&mut f, entry, 40);
        let one = const_fixnum(&mut f, entry, 1);
        let header = f.make_block();
        let bound = f.add_block_param(header, fixnum(), ValueRepresentation::Tagged);
        let body = f.make_block();
        let exit = f.make_block();
        f.set_terminator(
            entry,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![initial],
                }],
                ..inst(Opcode::Jump)
            },
        );
        let (_, condition) = f.push_inst(
            header,
            InstData {
                args: vec![n, bound],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            header,
            InstData {
                args: vec![condition[0]],
                targets: vec![
                    BlockCall {
                        block: exit,
                        args: vec![],
                    },
                    BlockCall {
                        block: body,
                        args: vec![],
                    },
                ],
                ..inst(Opcode::Brif)
            },
        );
        let (_, child) = f.push_inst(
            body,
            InstData {
                args: vec![n, one],
                ..inst(Opcode::FixnumSub)
            },
            &[(fixnum(), ValueRepresentation::Tagged)],
        );
        // This fact is not available on the first header visit. The backedge
        // must weaken the earlier n >= 40 constraint on a subsequent pass.
        let later = const_fixnum(&mut f, body, 2);
        f.set_terminator(
            body,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![later],
                }],
                ..inst(Opcode::Jump)
            },
        );
        f.set_terminator(exit, ret());
        let inferred = infer(&f);
        assert_eq!(inferred.range(bound), Some(Range { lo: 2, hi: 40 }));
        assert_eq!(
            inferred.range(child[0]),
            Some(Range { lo: 1, hi: 39 }),
            "must not retain only the first pass's n >= 40 constraint"
        );
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

        let site = match f.value(i1[0]).def {
            ValueDef::Result { inst, .. } => inst,
            _ => unreachable!(),
        };
        let r = infer_for_sites(&f, &[site]);
        assert_eq!(
            r.ty_before(site, i).range,
            Some(Range { lo: 0, hi: 9 }),
            "site range must replay the final loop solution"
        );
        // The induction variable is a bounded fixnum: 0 ≤ i ≤ 10.
        assert_eq!(r.ty(i).bits, TypeBits::FIXNUM, "index must be a FIXNUM");
        let rng = r.range(i).expect("index must have a proven range");
        assert_eq!(rng, Range { lo: 0, hi: 10 }, "index must be proved bounded");
        // Inside the body, `i` is narrowed below the bound, so i+1 ≤ 10.
        assert_eq!(r.range(i1[0]), Some(Range { lo: 1, hi: 10 }));
        assert_eq!(r.repr(i), Some(ValueRepresentation::UnboxedFixnum));
    }

    #[test]
    fn widened_accumulator_keeps_counted_index_exact() {
        // for (i = 0, s = 0; i < 10; i++) s += i — `s` grows on every pass, so
        // only header widening terminates the ascent; narrowing restores `i`.
        let mut f = Function::new("sum-loop");
        let e = f.entry();
        let header = f.make_block();
        let i = f.add_block_param(header, fixnum(), ValueRepresentation::UnboxedFixnum);
        let s = f.add_block_param(header, fixnum(), ValueRepresentation::UnboxedFixnum);
        let body = f.make_block();
        let exit = f.make_block();
        let zero = const_fixnum(&mut f, e, 0);
        let bound = const_fixnum(&mut f, e, 10);
        f.set_terminator(
            e,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![zero, zero],
                }],
                ..inst(Opcode::Jump)
            },
        );
        let (_, cond) = f.push_inst(
            header,
            InstData {
                args: vec![i, bound],
                ..inst(Opcode::FixnumCmpLt)
            },
            &[(
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                ValueRepresentation::Tagged,
            )],
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
        let one = const_fixnum(&mut f, body, 1);
        let (_, i1) = f.push_inst(
            body,
            InstData {
                args: vec![i, one],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        let (_, s1) = f.push_inst(
            body,
            InstData {
                args: vec![s, i],
                ..inst(Opcode::FixnumAdd)
            },
            &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
        );
        f.set_terminator(
            body,
            InstData {
                targets: vec![BlockCall {
                    block: header,
                    args: vec![i1[0], s1[0]],
                }],
                ..inst(Opcode::Jump)
            },
        );
        f.set_terminator(exit, ret());

        let r = infer(&f);
        assert_eq!(r.range(i), Some(Range { lo: 0, hi: 10 }));
        assert_eq!(r.range(i1[0]), Some(Range { lo: 1, hi: 10 }));
        // The accumulator has no re-establishing comparison: it stays widened,
        // but its lower bound and fixnum tag survive.
        assert_eq!(r.ty(s).bits, TypeBits::FIXNUM);
        let sr = r.range(s).expect("accumulator keeps a range");
        assert_eq!(sr.lo, 0);
        assert!(sr.hi >= 45, "widened bound must cover the real sum");
    }

    #[test]
    fn exhausted_ascent_discards_long_phi_type_guesses() {
        use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
        use crate::t2::pass::{Analyses, Pass};
        // Ascending parameter order propagates the last parameter's double
        // bit only one position left per pass. After COUNT-1 actual rotations
        // the first parameter contains DOUBLE, even when analysis stops early.
        // The short control converges normally and retains useful precision.
        for count in [3, MAX_PASSES + 3] {
            let mut f = Function::new("rotating-phi-types");
            let entry = f.entry();
            let double = f.add_block_param(
                entry,
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::Tagged,
            );
            let zero = const_fixnum(&mut f, entry, 0);
            let (_, tagged_zero) = f.push_inst(
                entry,
                InstData {
                    aux: AuxData::FixnumImm(0),
                    ..inst(Opcode::ConstFixnum)
                },
                &[(fixnum(), ValueRepresentation::Tagged)],
            );
            let bound = const_fixnum(&mut f, entry, (count - 1) as i64);
            let header = f.make_block();
            let body = f.make_block();
            let exit = f.make_block();
            let parameters: Vec<_> = (0..count)
                .map(|_| f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged))
                .collect();
            let counter = f.add_block_param(header, fixnum(), ValueRepresentation::UnboxedFixnum);
            let mut initial = vec![tagged_zero[0]; count];
            initial[count - 1] = double;
            initial.push(zero);
            f.set_terminator(
                entry,
                InstData {
                    targets: vec![BlockCall {
                        block: header,
                        args: initial,
                    }],
                    ..inst(Opcode::Jump)
                },
            );
            let (_, condition) = f.push_inst(
                header,
                InstData {
                    args: vec![counter, bound],
                    ..inst(Opcode::FixnumCmpLt)
                },
                &[(
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                )],
            );
            f.set_terminator(
                header,
                InstData {
                    args: vec![condition[0]],
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
            let one = const_fixnum(&mut f, body, 1);
            let (_, next_counter) = f.push_inst(
                body,
                InstData {
                    args: vec![counter, one],
                    ..inst(Opcode::FixnumAdd)
                },
                &[(fixnum(), ValueRepresentation::UnboxedFixnum)],
            );
            let mut rotated = parameters[1..].to_vec();
            rotated.push(parameters[0]);
            rotated.push(next_counter[0]);
            f.set_terminator(
                body,
                InstData {
                    targets: vec![BlockCall {
                        block: header,
                        args: rotated,
                    }],
                    ..inst(Opcode::Jump)
                },
            );
            let state = f.frame_states.add(FrameState {
                scopes: vec![FrameScope {
                    function: 0,
                    bcp: 1,
                    locals: vec![],
                    stack: vec![ValueSource::Value {
                        value: parameters[0],
                        repr: ValueRepresentation::Tagged,
                    }],
                }],
                remat: vec![],
            });
            let (guard, checked) = f.push_inst(
                exit,
                InstData {
                    args: vec![parameters[0]],
                    aux: AuxData::TypeTag(fixnum()),
                    flags: InstFlags {
                        effectful: true,
                        guard: true,
                        ..Default::default()
                    },
                    frame_state: Some(state),
                    ..inst(Opcode::Guard)
                },
                &[(fixnum(), ValueRepresentation::Tagged)],
            );
            let (_, used) = f.push_inst(
                exit,
                InstData {
                    args: vec![checked[0], tagged_zero[0]],
                    ..inst(Opcode::GenericAdd)
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            f.set_terminator(
                exit,
                InstData {
                    args: vec![used[0]],
                    ..ret()
                },
            );
            crate::t2::verify::verify(&f).unwrap();
            let result = infer_for_sites(&f, &[guard]);
            let expected = if count > MAX_PASSES {
                IRType::TOP
            } else {
                IRType::of(TypeBits::FIXNUM.join(TypeBits::DOUBLE_FLOAT))
            };
            assert_eq!(result.ty(parameters[0]), expected, "count={count}");
            assert_eq!(
                result.ty_before(guard, parameters[0]),
                expected,
                "count={count}"
            );
            assert_eq!(
                result.repr(parameters[0]),
                Some(ValueRepresentation::Tagged)
            );
            // Exercise the actual downstream consumer: a result used only by
            // an instruction must not let an unfinished FIXNUM guess erase
            // the runtime check. Its pre-narrowing recovery source stays live.
            let original = f.inst(guard).clone();
            crate::t2::opt_guard::GuardElim.run(&mut f, &mut Analyses::default());
            assert!(f.block(exit).insts.contains(&guard), "count={count}");
            assert_eq!(f.inst(guard).args, original.args);
            assert_eq!(f.inst(guard).flags, original.flags);
            assert_eq!(f.inst(guard).frame_state, original.frame_state);
            let recovery = f.frame_states.get(state);
            assert!(recovery.remat.is_empty());
            assert_eq!(recovery.scopes.len(), 1);
            let scope = &recovery.scopes[0];
            assert_eq!((scope.function, scope.bcp), (0, 1));
            assert!(scope.locals.is_empty());
            assert!(matches!(scope.stack.as_slice(),
                [ValueSource::Value { value, repr: ValueRepresentation::Tagged }]
                    if *value == parameters[0]));
            crate::t2::verify::verify(&f).unwrap();
        }
    }

    #[test]
    fn heap_double_literal_fact_requires_declared_type_and_immutable_snapshot() {
        // The address is an opaque pool-slot identity. Inference must use only
        // the compiler-owned snapshot, never read this synthetic heap address.
        const SLOT: usize = 0x12340;
        for (declared_double, snapshot, expected) in [
            (true, false, IRType::TOP),
            (true, true, IRType::of(TypeBits::DOUBLE_FLOAT)),
            (false, true, IRType::TOP),
        ] {
            let mut f = Function::new("literal-proof-boundary");
            let entry = f.entry();
            if snapshot {
                f.double_literals.insert(SLOT, 1.25f64.to_bits());
            }
            let (_, result) = f.push_inst(
                entry,
                InstData {
                    aux: AuxData::HeapLiteral { slot: SLOT },
                    ..inst(Opcode::ConstHeapObj)
                },
                &[(
                    if declared_double {
                        IRType::of(TypeBits::DOUBLE_FLOAT)
                    } else {
                        IRType::TOP
                    },
                    ValueRepresentation::Tagged,
                )],
            );
            f.set_terminator(
                entry,
                InstData {
                    args: vec![result[0]],
                    ..ret()
                },
            );
            assert_eq!(
                infer(&f).ty(result[0]),
                expected,
                "declared_double={declared_double}, snapshot={snapshot}"
            );
        }
    }
    #[test]
    fn checked_normal_return_refines_untrusted_declared_double_in_successor() {
        // A declaration without immutable literal metadata is not a global
        // proof. Only the successful runtime check establishes this original
        // SSA value's type in its normal continuation.
        let mut f = Function::new("checked-untrusted-double");
        let entry = f.entry();
        let (_, literal) = f.push_inst(
            entry,
            InstData {
                aux: AuxData::HeapLiteral { slot: 0x12340 },
                ..inst(Opcode::ConstHeapObj)
            },
            &[(
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::Tagged,
            )],
        );
        let literal = literal[0];
        assert!(f.double_literals.is_empty());
        let state = f.frame_states.add(crate::t2::frame_state::FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (guard, _) = f.push_inst(
            entry,
            InstData {
                args: vec![literal],
                aux: AuxData::TypeTag(IRType::of(TypeBits::DOUBLE_FLOAT)),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..Default::default()
                },
                frame_state: Some(state),
                ..inst(Opcode::Guard)
            },
            &[],
        );
        let successor = f.make_block();
        f.set_terminator(
            entry,
            InstData {
                targets: vec![BlockCall {
                    block: successor,
                    args: vec![],
                }],
                ..inst(Opcode::Jump)
            },
        );
        let (later, _) = f.push_inst(
            successor,
            InstData {
                args: vec![literal, literal],
                ..inst(Opcode::GenericEq)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(successor, ret());
        let facts = infer_for_sites(&f, &[guard, later]);
        assert_eq!(
            facts.ty(literal),
            IRType::TOP,
            "no heap read or declaration trust"
        );
        assert_eq!(facts.ty_before(guard, literal), IRType::TOP);
        assert_eq!(
            facts.ty_before(later, literal),
            IRType::of(TypeBits::DOUBLE_FLOAT),
            "the checked predecessor must publish its fact across the edge"
        );
        let only_successor = infer_for_sites(&f, &[later]);
        assert_eq!(
            only_successor.ty_before(later, literal),
            IRType::of(TypeBits::DOUBLE_FLOAT),
            "an unrequested predecessor still publishes its checked contract"
        );
    }

    #[test]
    fn requested_guard_site_captures_prestate_and_later_success_only() {
        let mut f = Function::new("requested-guard-order");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let state = f.frame_states.add(crate::t2::frame_state::FrameState {
            scopes: vec![],
            remat: vec![],
        });
        let (guard, _) = f.push_inst(
            entry,
            InstData {
                args: vec![input],
                aux: AuxData::TypeTag(fixnum()),
                frame_state: Some(state),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                ..inst(Opcode::Guard)
            },
            &[],
        );
        let (later, _) = f.push_inst(
            entry,
            InstData {
                args: vec![input, input],
                ..inst(Opcode::GenericAdd)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(entry, ret());
        let facts = infer_for_sites(&f, &[guard, later, later]);
        assert_eq!(facts.ty_before(guard, input), IRType::TOP);
        assert_eq!(facts.ty_before(later, input), fixnum());
        assert_eq!(facts.ty(input), IRType::TOP);
        assert_eq!(infer(&f).ty_before(later, input), IRType::TOP);
        assert_eq!(
            facts.before.len(),
            2,
            "only requested operand pairs retained"
        );
    }

    #[test]
    fn requested_sites_capture_real_bytecode_branch_operand_facts() {
        use egcl_rt::bytecode::{BytecodeFunction, Instr};
        let body = BytecodeFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            code: vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: egcl_rt::symbols::intern("INTEGERP"),
                    nargs: 1,
                },
                Instr::BrIfFalse(5),
                Instr::LoadLocal(0),
                Instr::Return,
                Instr::LoadLocal(0),
                Instr::Return,
            ],
            constants: vec![],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: 1,
            max_stack: 1,
            arity: 1,
            name: "inferred-predicate".into(),
            params_form: egcl_rt::value::NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
            binder_cache: Default::default(),
            closure_template: Default::default(),
        };
        let f = crate::t2::build::build_from_bytecode(&body).unwrap();
        let branch = f
            .block_order()
            .iter()
            .find_map(|&b| {
                f.terminator(b)
                    .filter(|&i| f.inst(i).opcode == Opcode::Brif)
            })
            .expect("bytecode conditional must build a branch");
        let returns: Vec<_> = f
            .block_order()
            .iter()
            .filter_map(|&b| f.terminator(b))
            .filter(|&i| f.inst(i).opcode == Opcode::Return)
            .collect();
        assert_eq!(returns.len(), 2);
        let facts = infer_for_sites(&f, &returns);
        let yes = f.inst(branch).targets[0].block;
        for site in returns {
            let input = f.inst(site).args[0];
            assert_eq!(facts.ty(input), IRType::TOP);
            let expected = if f.block(yes).insts.contains(&site) {
                IRType::of(TypeBits::FIXNUM.join(TypeBits::BIGNUM))
            } else {
                IRType::TOP
            };
            assert_eq!(facts.ty_before(site, input), expected);
        }
    }
}
