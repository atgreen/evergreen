// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Type, integer-range and representation inference over the T2 SSA IR.
//!
//! # Purpose
//!
//! [`infer`] is a forward, flow-sensitive dataflow analysis in the sparse
//! conditional constant propagation family. For every reachable `Value` it
//! computes a narrowed [`IRType`] (tag bits, an optional integer [`Range`],
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
//! for guard and branch narrowing, where both facts hold. `ty_widen` saturates
//! a bound that is still growing to `i64::MIN`/`i64::MAX` so loops with
//! non-constant trip counts terminate.
//!
//! # Algorithm
//!
//! 1. Seed the parameters of predecessor-less blocks (the entry; an OSR entry
//!    region) from their declared types.
//! 2. Iterate up to `MAX_PASSES` times over reverse postorder; widening is
//!    switched on after `WIDEN_AFTER` passes. Each pass:
//!    1. Collect `check_refine[B]`, the narrowings of every
//!       `TypeCheck`/`Guard` with a `TypeTag` in block `B`.
//!    2. For each block: join its parameters over predecessor edge
//!       arguments; build a local environment by walking the dominator chain
//!       and meeting in every edge refinement on the chain (including the
//!       block's own) and every check refinement of a **strict** dominator;
//!       then evaluate instructions in program order with `eval`, joining
//!       each result into the global fact and inserting it into the local
//!       environment. A `TypeCheck`/`Guard` met in program order narrows its
//!       operand for the rest of the block.
//!       After evaluating a block, derive its outgoing branch refinements
//!       from the updated facts. `edge_refine[T]` holds bounds true on the
//!       unique edge into `T` from a fixnum comparison (`!=` yields nothing).
//!       Producing these before visiting successors avoids permanently joining
//!       coarse results before the comparison's constant bounds are known.
//!    3. Stop early when no fact changed.
//! 3. Derive representations: a fact confined to fixnum, single-float or
//!    double-float bits becomes the corresponding unboxed representation,
//!    anything else `Tagged`.
//!
//! `eval` returns `⊥` whenever any operand is `⊥`, so an unreached operand
//! never widens a result prematurely. Constants seed singleton ranges;
//! fixnum add/sub/mul/neg propagate ranges with saturating arithmetic; other
//! fixnum ops yield the full fixnum range; float ops propagate float tags;
//! generic arithmetic applies numeric contagion and adds `BIGNUM` whenever
//! `FIXNUM` is possible; comparisons yield `SYMBOL|NULL`; `TypeCheck`/`Guard`
//! results are the operand met with the tag; everything else is `⊤`.
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
//! * Fixed iteration budget (`WIDEN_AFTER` = 400, then 8 widened passes);
//!   a function that has not converged by then keeps its last facts, which
//!   are still sound over-approximations.
//!
//! # Debugging
//!
//! * `EGCL_IR_DUMP` (any value) makes the `TypeInference` pass print the
//!   number of values with a non-`⊥` fact to stderr.

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
        TryIntegerShr | TryStoreIntegerOctets8 | TryStoreIntegerOctets1To8 => IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
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

        // A type check narrows its operand to the checked tag (spec R4.63).
        // A Guard VALUE does the same for its passed-through result: it exists
        // only on the path where the check held (egcl-x5y.25b). A non-TypeTag
        // guard (StringLayout) passes its operand's type through unchanged.
        TypeCheck | Guard => {
            let tag = match aux {
                AuxData::TypeTag(t) => *t,
                _ => IRType::TOP,
            };
            ty_meet(ops.first().copied().unwrap_or(IRType::TOP), tag)
        }

        AllocCons => IRType::of(TypeBits::CONS),
        StringCharOrNil => IRType::of(TypeBits::CHARACTER.join(TypeBits::NULL)),

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

/// A fresh BYTE descriptor may be mutable in this implementation. Infer an
/// LDB result only while that descriptor has one executable consumer, crosses
/// normal single-predecessor edges, and encounters no opaque effects. Calls
/// remain in the IR: these are facts about successful normal returns only.
fn fresh_ldb_facts(f: &Function) -> HashMap<Value, IRType> {
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
                let predecessors = f.preds(block);
                let [predecessor] = predecessors.as_slice() else {
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

/// Infer types, ranges, and representations for every reachable value in `f`.
pub fn infer(f: &Function) -> InferenceResult {
    let ldb_facts = fresh_ldb_facts(f);
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
                    let res = if matches!(idef.opcode, Opcode::TryIntegerShr | Opcode::TryStoreIntegerOctets8 | Opcode::TryStoreIntegerOctets1To8) && index == 1 {
                        IRType::of(TypeBits::FIXNUM)
                    } else { res };
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
            // Compute outgoing bounds only after this block's instructions:
            // its comparison operands (including constants) now have facts.
            // Global facts still accumulate monotonically, so a loop-carried
            // bound that grows on a later pass weakens the inferred result.
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
                                    // preds() deduplicates blocks. Two branch
                                    // outcomes entering the same block do not
                                    // establish either outcome's constraint.
                                    if f.preds(tgt) == vec![b]
                                        && t.targets.iter().filter(|c| c.block == tgt).count() == 1
                                    {
                                        let refs = narrow_cmp(cdef.opcode, ca, cb, taken, &g);
                                        edge_refine.entry(tgt.index()).or_default().extend(refs);
                                    }
                                }
                            }
                        }
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
