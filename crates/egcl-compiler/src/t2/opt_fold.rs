// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Constant folding and strength reduction over fixnum arithmetic.
//!
//! # Purpose
//!
//! [`ConstFold`] evaluates fixnum arithmetic, logic and comparison
//! instructions whose operands are all constant, and rewrites the
//! one-constant-operand shapes that have a cheaper form (`x*2^n` → `x<<n`,
//! `x*-1` → `-x`, `x*0` → `0`) or no form at all (`x+0`, `x-0`, `x*1`, `x/1`,
//! `x|0`, `x^0`, `x&-1` → `x`). It is the first pass of the production
//! mid-end (fold, GVN, guard elimination, DCE).
//!
//! # Contract
//!
//! **Input:** a well-formed `Function`. Only the `Fixnum*`, `Log*` and
//! `FixnumCmp*` opcodes are considered; generic (numeric-tower) and float
//! arithmetic are never folded. An instruction flagged `effectful` or `call`
//! is skipped unless it is a guard with wholly constant operands whose result
//! can be proven in range. Independent binding guards remain ordered effects;
//! identities involving dynamic operands do not remove a runtime type check.
//!
//! **Output:** rewritten instructions and rerouted uses; nothing is unlinked.
//! Instructions that were value-substituted away, and constant operands no
//! longer referenced, remain as dead definitions for DCE.
//! `Analyses::invalidate` is called if anything changed.
//!
//! # Algorithm
//!
//! A single reverse-postorder scan in program order. A `consts` map
//! (`Value → i64`) is seeded from existing `ConstFixnum` definitions and
//! extended as folds produce new constants, so folds compose transitively in
//! one scan. Two disjoint rewrite mechanisms are used:
//!
//! 1. **In-place opcode mutation**, when the result becomes a fresh constant
//!    (full fold; comparison collapsing to `T`/`NIL`; `x*0`) or a cheaper
//!    single instruction (`FixnumShl`, `FixnumNeg`). The instruction's
//!    opcode/args/aux change but its result `Value` is unchanged, so every
//!    existing use stays correct with no use scan. `make_const_fixnum` also
//!    refines the result type to the singleton range `[k, k]`;
//!    `make_const_bool` fixes the representation to `Tagged`. The shift count
//!    for `x*2^n` is materialised as a new `ConstFixnum` inserted immediately
//!    before the instruction so it dominates its use.
//! 2. **Value substitution**, when the result should become an existing
//!    operand (`x+0` → `x` and friends). The IR has no copy instruction, so
//!    the result is entered in a replacement map and a final pass rewrites
//!    every instruction operand, terminator edge argument, and FrameState
//!    value source (including remat-recipe inputs) to the representative. The
//!    IR has no use lists; this scan is the value-to-uses map.
//!
//! # Overflow and errors
//!
//! Fixnums are 61-bit signed, `[-2^60, 2^60-1]`. Arithmetic is evaluated in
//! `i128` and a result outside that range is **not** folded; the instruction
//! is left for the generic or guarded path rather than wrapping. Division,
//! remainder and modulus by a constant zero are never folded: the runtime
//! must signal. `FixnumShl` folds only for shift counts in `0..63`,
//! `FixnumShr` in `0..64`. `cl_mod` implements CL `MOD` (remainder with the
//! divisor's sign) for the constant path.
//!
//! # Guards on reduced instructions
//!
//! A `FixnumMul` may carry an overflow guard (`frame_state`). `x*2^n` and
//! `x<<n` have the same overflow condition, so the rewritten `FixnumShl`
//! inherits flags and `frame_state` unchanged. Identity and `*0` reductions
//! are exact, and a fold proven in range is exact, so in those cases the
//! guard is vacuous and `make_const_fixnum` clears flags and `frame_state`.
//!
//! # Limits
//!
//! * No algebraic reassociation or folding through block parameters.
//! * `0-x`, `x/-1`, and shifts by constants other than in `x*2^n` are not
//!   reduced.
//! * Only fixnum constants are tracked; a `ConstFloat` operand never folds.

use std::collections::HashMap;

use crate::t2::frame_state::{FrameStateId, ValueSource};
use crate::t2::ir::{
    AuxData, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use crate::t2::pass::{Analyses, Pass};

/// Largest representable 61-bit signed fixnum (`2^60 - 1`).
const FIXNUM_MAX: i64 = (1 << 60) - 1;
/// Smallest representable 61-bit signed fixnum (`-2^60`).
const FIXNUM_MIN: i64 = -(1 << 60);

#[derive(Default)]
pub struct ConstFold;

/// Clamp an exact (wide) arithmetic result to the fixnum range, or `None` if it
/// would overflow and therefore must not be folded.
fn in_fixnum_range(x: i128) -> Option<i64> {
    if x >= FIXNUM_MIN as i128 && x <= FIXNUM_MAX as i128 {
        Some(x as i64)
    } else {
        None
    }
}

/// Follow the value-substitution chain to a value's current representative.
fn resolve(map: &HashMap<Value, Value>, mut v: Value) -> Value {
    while let Some(&next) = map.get(&v) {
        if next == v {
            break;
        }
        v = next;
    }
    v
}

impl Pass for ConstFold {
    fn name(&self) -> &'static str {
        "const-fold"
    }

    fn run(&mut self, f: &mut Function, a: &mut Analyses) {
        use Opcode::*;

        // Value → constant fixnum payload. Seeded from existing `ConstFixnum`
        // definitions and extended as this pass folds new constants, so folds
        // compose transitively within one program-order scan (e.g. (3+4)+5).
        let mut consts: HashMap<Value, i64> = HashMap::new();
        for i in 0..f.num_insts() {
            let inst = Inst(i as u32);
            let d = f.inst(inst);
            if d.opcode == ConstFixnum {
                if let AuxData::FixnumImm(k) = d.aux {
                    if let Some(&r) = d.results.first() {
                        consts.insert(r, k);
                    }
                }
            }
        }

        // result → existing representative value (identity strength reductions).
        let mut replacement: HashMap<Value, Value> = HashMap::new();
        let mut changed = false;

        // Read an operand's constant value, resolving substitutions first.
        let cval = |consts: &HashMap<Value, i64>,
                    replacement: &HashMap<Value, Value>,
                    v: Value|
         -> Option<i64> { consts.get(&resolve(replacement, v)).copied() };

        for b in f.reverse_postorder() {
            // Clone the block's instruction order so the loop body can freely
            // borrow `f` mutably (and insert new constants) as it goes.
            let insts: Vec<Inst> = f.block(b).insts.clone();
            for inst in insts {
                // Snapshot the fields we need as owned locals so the loop body
                // can mutably borrow `f` (rewrite instructions, insert consts)
                // without holding an immutable borrow across those calls.
                let (op, args, results, effectful, call, guard) = {
                    let d = f.inst(inst);
                    (
                        d.opcode,
                        d.args.clone(),
                        d.results.clone(),
                        d.flags.effectful,
                        d.flags.call,
                        d.flags.guard,
                    )
                };

                // A guard on wholly constant operands can become vacuous.
                // Dynamic identities (x+0, x*1) must not erase its type check.
                // The opcode-specific folds below still reject overflow; other
                // effects, including binding guards, have no folding rule.
                let constant_guard = guard && args.iter()
                    .all(|&value| cval(&consts, &replacement, value).is_some());
                if call || (effectful && !constant_guard) {
                    continue;
                }

                let n = args.len();
                let a0 = if n >= 1 {
                    cval(&consts, &replacement, args[0])
                } else {
                    None
                };
                let a1 = if n >= 2 {
                    cval(&consts, &replacement, args[1])
                } else {
                    None
                };

                // ── Full constant fold: all operands constant. ──
                if n == 2 {
                    if let (Some(x), Some(y)) = (a0, a1) {
                        // Arithmetic/logic → a fixnum (range-checked); a
                        // comparison → T/NIL.
                        let folded: Option<Fold> = match op {
                            FixnumAdd => {
                                (x as i128 + y as i128).pipe(in_fixnum_range).map(Fold::Fix)
                            }
                            FixnumSub => {
                                (x as i128 - y as i128).pipe(in_fixnum_range).map(Fold::Fix)
                            }
                            FixnumMul => {
                                (x as i128 * y as i128).pipe(in_fixnum_range).map(Fold::Fix)
                            }
                            FixnumDiv if y != 0 => {
                                in_fixnum_range(x as i128 / y as i128).map(Fold::Fix)
                            }
                            FixnumRem if y != 0 => {
                                in_fixnum_range(x as i128 % y as i128).map(Fold::Fix)
                            }
                            FixnumMod if y != 0 => in_fixnum_range(cl_mod(x, y)).map(Fold::Fix),
                            FixnumShl if (0..63).contains(&y) => {
                                in_fixnum_range((x as i128) << y).map(Fold::Fix)
                            }
                            FixnumShr if (0..64).contains(&y) => {
                                in_fixnum_range((x as i128) >> y).map(Fold::Fix)
                            }
                            LogAnd => in_fixnum_range((x & y) as i128).map(Fold::Fix),
                            LogOr => in_fixnum_range((x | y) as i128).map(Fold::Fix),
                            LogXor => in_fixnum_range((x ^ y) as i128).map(Fold::Fix),
                            FixnumCmpEq => Some(Fold::Bool(x == y)),
                            FixnumCmpLt => Some(Fold::Bool(x < y)),
                            FixnumCmpLe => Some(Fold::Bool(x <= y)),
                            FixnumCmpGt => Some(Fold::Bool(x > y)),
                            FixnumCmpGe => Some(Fold::Bool(x >= y)),
                            _ => None,
                        };
                        if let Some(fold) = folded {
                            match fold {
                                Fold::Fix(k) => make_const_fixnum(f, inst, k, &mut consts),
                                Fold::Bool(t) => make_const_bool(f, inst, t),
                            }
                            changed = true;
                            continue;
                        }
                    }
                }

                // ── Unary constant fold. ──
                if n == 1 {
                    if let Some(x) = a0 {
                        let folded = match op {
                            FixnumNeg => in_fixnum_range(-(x as i128)),
                            LogNot => in_fixnum_range(!x as i128),
                            _ => None,
                        };
                        if let Some(k) = folded {
                            make_const_fixnum(f, inst, k, &mut consts);
                            changed = true;
                            continue;
                        }
                    }
                }

                // ── Strength reduction (exactly one constant operand). ──
                match op {
                    FixnumMul => {
                        // Commutative: the constant may be on either side.
                        let (c, var) = match (a0, a1) {
                            (Some(c), None) => (c, args[1]),
                            (None, Some(c)) => (c, args[0]),
                            _ => continue,
                        };
                        let var = resolve(&replacement, var);
                        if c == 0 {
                            // x*0 → 0 (fixnum: no NaN concern, spec §4.5.8.1).
                            make_const_fixnum(f, inst, 0, &mut consts);
                            changed = true;
                        } else if c == 1 {
                            replacement.insert(results[0], var);
                            changed = true;
                        } else if c == -1 {
                            // x*-1 → (- x): reuse the instruction as a negate.
                            let r = f.inst_mut(inst);
                            r.opcode = FixnumNeg;
                            r.args = vec![var];
                            r.aux = AuxData::None;
                            changed = true;
                        } else if c > 0 && (c & (c - 1)) == 0 {
                            // x*2^n → x<<n. The shift shares the mul's overflow
                            // condition, so flags/frame_state are inherited.
                            let n = c.trailing_zeros() as i64;
                            let sv = insert_const_fixnum_before(f, b, inst, n, &mut consts);
                            let r = f.inst_mut(inst);
                            r.opcode = FixnumShl;
                            r.args = vec![var, sv];
                            r.aux = AuxData::None;
                            changed = true;
                        }
                    }
                    FixnumAdd => {
                        // x+0 → x (commutative).
                        let (c, var) = match (a0, a1) {
                            (Some(c), None) => (c, args[1]),
                            (None, Some(c)) => (c, args[0]),
                            _ => continue,
                        };
                        if c == 0 {
                            replacement.insert(results[0], resolve(&replacement, var));
                            changed = true;
                        }
                    }
                    FixnumSub => {
                        // x-0 → x (only; 0-x is a negate, not an identity).
                        if a0.is_none() && a1 == Some(0) {
                            replacement.insert(results[0], resolve(&replacement, args[0]));
                            changed = true;
                        }
                    }
                    FixnumDiv => {
                        // x/1 → x.
                        if a0.is_none() && a1 == Some(1) {
                            replacement.insert(results[0], resolve(&replacement, args[0]));
                            changed = true;
                        }
                    }
                    LogOr => {
                        // x|0 → x (commutative).
                        let (c, var) = match (a0, a1) {
                            (Some(c), None) => (c, args[1]),
                            (None, Some(c)) => (c, args[0]),
                            _ => continue,
                        };
                        if c == 0 {
                            replacement.insert(results[0], resolve(&replacement, var));
                            changed = true;
                        }
                    }
                    LogXor => {
                        // x^0 → x (commutative).
                        let (c, var) = match (a0, a1) {
                            (Some(c), None) => (c, args[1]),
                            (None, Some(c)) => (c, args[0]),
                            _ => continue,
                        };
                        if c == 0 {
                            replacement.insert(results[0], resolve(&replacement, var));
                            changed = true;
                        }
                    }
                    LogAnd => {
                        // x&-1 → x (commutative).
                        let (c, var) = match (a0, a1) {
                            (Some(c), None) => (c, args[1]),
                            (None, Some(c)) => (c, args[0]),
                            _ => continue,
                        };
                        if c == -1 {
                            replacement.insert(results[0], resolve(&replacement, var));
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Rewrite uses of value-substituted results (spec §4.10 R4.60). ──
        if !replacement.is_empty() {
            for i in 0..f.num_insts() {
                let d = f.inst_mut(Inst(i as u32));
                for arg in d.args.iter_mut() {
                    *arg = resolve(&replacement, *arg);
                }
                for target in d.targets.iter_mut() {
                    for arg in target.args.iter_mut() {
                        *arg = resolve(&replacement, *arg);
                    }
                }
            }
            // A value named by any FrameState is deopt-live: reroute it to its
            // representative rather than orphaning it.
            for idx in 0..f.frame_states.len() {
                let fs = f.frame_states.get_mut(FrameStateId(idx as u32));
                for scope in fs.scopes.iter_mut() {
                    for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                        if let ValueSource::Value { value, .. } = src {
                            *value = resolve(&replacement, *value);
                        }
                    }
                }
                for recipe in fs.remat.iter_mut() {
                    for src in recipe.inputs.iter_mut() {
                        if let ValueSource::Value { value, .. } = src {
                            *value = resolve(&replacement, *value);
                        }
                    }
                }
            }
        }

        if changed {
            // Value definitions changed (opcodes rewritten, uses rerouted);
            // drop cached analyses so later passes recompute.
            a.invalidate();
        }
    }
}

/// The two outcomes of a full constant fold.
enum Fold {
    Fix(i64),
    Bool(bool),
}

/// A tiny `Option`-friendly pipe so the fold table reads left-to-right.
trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for i128 {}

/// CL `mod`: remainder with the sign of the divisor, for the constant-fold path.
fn cl_mod(x: i64, y: i64) -> i128 {
    let mut r = x as i128 % y as i128;
    if r != 0 && (r < 0) != ((y as i128) < 0) {
        r += y as i128;
    }
    r
}

/// Rewrite `inst` in place to `ConstFixnum k`, keeping its result value so all
/// existing uses stay valid. The fold is proven in-range, so any overflow guard
/// is vacuous and the frame_state is dropped.
fn make_const_fixnum(f: &mut Function, inst: Inst, k: i64, consts: &mut HashMap<Value, i64>) {
    let result = f.inst(inst).results.first().copied();
    let d = f.inst_mut(inst);
    d.opcode = Opcode::ConstFixnum;
    d.args.clear();
    d.aux = AuxData::FixnumImm(k);
    d.flags = InstFlags::default();
    d.frame_state = None;
    if let Some(r) = result {
        consts.insert(r, k);
        // Refine the result to the singleton range [k, k] for downstream passes.
        f.refine_type(
            r,
            IRType {
                bits: TypeBits::FIXNUM,
                range: Some(crate::t2::ir::Range { lo: k, hi: k }),
                class_id: None,
            },
        );
    }
}

/// Rewrite `inst` in place to `ConstT`/`ConstNil` (a comparison result). The
/// result becomes a tagged constant, so its representation is fixed to `Tagged`.
fn make_const_bool(f: &mut Function, inst: Inst, truthy: bool) {
    let result = f.inst(inst).results.first().copied();
    let d = f.inst_mut(inst);
    d.opcode = if truthy {
        Opcode::ConstT
    } else {
        Opcode::ConstNil
    };
    d.args.clear();
    d.aux = AuxData::None;
    d.flags = InstFlags::default();
    d.frame_state = None;
    if let Some(r) = result {
        f.set_repr(r, ValueRepresentation::Tagged);
    }
}

/// Materialise a fresh `ConstFixnum n` in `block`, positioned immediately before
/// `before` (so it dominates the use we are about to create), and return its
/// value. `push_inst` appends after the block terminator; we then move it into
/// place.
fn insert_const_fixnum_before(
    f: &mut Function,
    block: crate::t2::ir::Block,
    before: Inst,
    n: i64,
    consts: &mut HashMap<Value, i64>,
) -> Value {
    let (cinst, cres) = f.push_inst(
        block,
        InstData {
            opcode: Opcode::ConstFixnum,
            args: vec![],
            results: vec![],
            aux: AuxData::FixnumImm(n),
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        )],
    );
    let insts = &mut f.block_mut(block).insts;
    let popped = insts.pop();
    debug_assert_eq!(
        popped,
        Some(cinst),
        "push_inst appends the new const at the end"
    );
    let pos = insts
        .iter()
        .position(|&i| i == before)
        .unwrap_or(insts.len());
    insts.insert(pos, cinst);
    consts.insert(cres[0], n);
    cres[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{Block, BlockCall};

    fn ufix() -> (IRType, ValueRepresentation) {
        (
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        )
    }
    fn tagged() -> (IRType, ValueRepresentation) {
        (IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)
    }

    fn const_fixnum(f: &mut Function, b: Block, k: i64) -> Value {
        let (_, r) = f.push_inst(
            b,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(k),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[ufix()],
        );
        r[0]
    }

    fn binop(f: &mut Function, b: Block, op: Opcode, args: Vec<Value>) -> (Inst, Value) {
        let (i, r) = f.push_inst(
            b,
            InstData {
                opcode: op,
                args,
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[ufix()],
        );
        (i, r[0])
    }

    fn ret(f: &mut Function, b: Block) {
        f.set_terminator(
            b,
            InstData {
                opcode: Opcode::Return,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
    }

    fn run(f: &mut Function) {
        let mut a = Analyses::new();
        ConstFold.run(f, &mut a);
    }

    /// The instruction whose result is `v`.
    fn def_of(f: &Function, v: Value) -> Inst {
        for i in 0..f.num_insts() {
            if f.inst(Inst(i as u32)).results.contains(&v) {
                return Inst(i as u32);
            }
        }
        panic!("no def for {v:?}");
    }

    // FixnumAdd(3, 4) folds to a constant 7 that downstream uses see directly.
    #[test]
    fn folds_add_of_constants() {
        let mut f = Function::new("add");
        let e = f.entry();
        let c3 = const_fixnum(&mut f, e, 3);
        let c4 = const_fixnum(&mut f, e, 4);
        let (sum_inst, sum) = binop(&mut f, e, Opcode::FixnumAdd, vec![c3, c4]);
        // A downstream user of the sum.
        let (_, dn) = binop(&mut f, e, Opcode::FixnumAdd, vec![sum, sum]);
        ret(&mut f, e);

        run(&mut f);

        // The add became a ConstFixnum 7 in place (same result value).
        let d = f.inst(sum_inst);
        assert_eq!(d.opcode, Opcode::ConstFixnum);
        assert!(matches!(d.aux, AuxData::FixnumImm(7)));
        assert!(d.args.is_empty());

        // The downstream add (7+7) also folded transitively to 14.
        let dd = f.inst(def_of(&f, dn));
        assert_eq!(dd.opcode, Opcode::ConstFixnum);
        assert!(matches!(dd.aux, AuxData::FixnumImm(14)));
    }

    #[test]
    fn guarded_constants_fold_without_erasing_overflow_or_dynamic_checks() {
        for (left, expected) in [(3, Opcode::ConstFixnum), (FIXNUM_MAX, Opcode::FixnumAdd)] {
            let mut f = Function::new("guarded-constant");
            let e = f.entry();
            let a = const_fixnum(&mut f, e, left);
            let b = const_fixnum(&mut f, e, 1);
            let (sum, _) = binop(&mut f, e, Opcode::FixnumAdd, vec![a, b]);
            f.inst_mut(sum).flags.guard = true;
            f.inst_mut(sum).flags.effectful = true;
            ret(&mut f, e);
            run(&mut f);
            assert_eq!(f.inst(sum).opcode, expected);
        }
        let mut f = Function::new("guarded-dynamic-identity");
        let e = f.entry();
        let x = f.add_block_param(e, IRType::TOP, ValueRepresentation::Tagged);
        let zero = const_fixnum(&mut f, e, 0);
        let (sum, value) = binop(&mut f, e, Opcode::FixnumAdd, vec![x, zero]);
        f.inst_mut(sum).flags.guard = true;
        f.inst_mut(sum).flags.effectful = true;
        ret(&mut f, e);
        f.inst_mut(f.block(e).insts.last().copied().unwrap()).args = vec![value];
        run(&mut f);
        assert_eq!(f.inst(sum).opcode, Opcode::FixnumAdd);
        assert_eq!(f.inst(*f.block(e).insts.last().unwrap()).args, vec![value]);
    }

    // A fold that would overflow the 61-bit fixnum range is left untouched.
    #[test]
    fn overflowing_fold_is_left_alone() {
        let mut f = Function::new("overflow");
        let e = f.entry();
        let big = const_fixnum(&mut f, e, FIXNUM_MAX);
        let (mul_inst, _) = binop(&mut f, e, Opcode::FixnumMul, vec![big, big]);
        ret(&mut f, e);

        run(&mut f);

        // MAX*MAX far exceeds the range → still a FixnumMul, unfolded.
        assert_eq!(f.inst(mul_inst).opcode, Opcode::FixnumMul);

        // Boundary: MAX + 1 overflows and must NOT fold either.
        let mut f2 = Function::new("boundary");
        let e2 = f2.entry();
        let m = const_fixnum(&mut f2, e2, FIXNUM_MAX);
        let one = const_fixnum(&mut f2, e2, 1);
        let (add_inst, _) = binop(&mut f2, e2, Opcode::FixnumAdd, vec![m, one]);
        ret(&mut f2, e2);
        run(&mut f2);
        assert_eq!(
            f2.inst(add_inst).opcode,
            Opcode::FixnumAdd,
            "MAX+1 must not wrap"
        );
    }

    // FixnumMul(x, 8) strength-reduces to FixnumShl(x, 3).
    #[test]
    fn mul_by_power_of_two_becomes_shift() {
        let mut f = Function::new("shift");
        let e = f.entry();
        let x = f.add_block_param(
            e,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let c8 = const_fixnum(&mut f, e, 8);
        let (mul_inst, _) = binop(&mut f, e, Opcode::FixnumMul, vec![x, c8]);
        ret(&mut f, e);

        run(&mut f);

        let d = f.inst(mul_inst);
        assert_eq!(d.opcode, Opcode::FixnumShl);
        assert_eq!(d.args[0], x, "shifted value is x");
        // Second operand is a fresh ConstFixnum 3, defined before the shift.
        let shamt = d.args[1];
        let shdef = f.inst(def_of(&f, shamt));
        assert_eq!(shdef.opcode, Opcode::ConstFixnum);
        assert!(matches!(shdef.aux, AuxData::FixnumImm(3)));
        // The shift-amount const precedes the shift in program order.
        let order = &f.block(e).insts;
        let shamt_pos = order
            .iter()
            .position(|&i| f.inst(i).results.contains(&shamt))
            .unwrap();
        let shl_pos = order.iter().position(|&i| i == mul_inst).unwrap();
        assert!(shamt_pos < shl_pos, "shift amount must dominate the shift");
    }

    // Identity reductions and x*0.
    #[test]
    fn algebraic_identities() {
        let mut f = Function::new("ident");
        let e = f.entry();
        let x = f.add_block_param(
            e,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let c0 = const_fixnum(&mut f, e, 0);
        let c1 = const_fixnum(&mut f, e, 1);
        // x+0 → x, x*1 → x, x*0 → 0
        let (_, addz) = binop(&mut f, e, Opcode::FixnumAdd, vec![x, c0]);
        let (_, mul1) = binop(&mut f, e, Opcode::FixnumMul, vec![x, c1]);
        let (mul0_inst, _) = binop(&mut f, e, Opcode::FixnumMul, vec![x, c0]);
        // Consumers so the substitutions are observable in args.
        let (cons_inst, _) = binop(&mut f, e, Opcode::FixnumAdd, vec![addz, mul1]);
        ret(&mut f, e);

        run(&mut f);

        // x+0 and x*1 both resolved to x in the consumer's args.
        assert_eq!(f.inst(cons_inst).args, vec![x, x]);
        // x*0 folded to a ConstFixnum 0 in place.
        let d0 = f.inst(mul0_inst);
        assert_eq!(d0.opcode, Opcode::ConstFixnum);
        assert!(matches!(d0.aux, AuxData::FixnumImm(0)));
    }

    // Comparisons fold to ConstT / ConstNil.
    #[test]
    fn compares_fold_to_t_and_nil() {
        let mut f = Function::new("cmp");
        let e = f.entry();
        let c3 = const_fixnum(&mut f, e, 3);
        let c4 = const_fixnum(&mut f, e, 4);
        // 3 < 4 → T ; 3 == 4 → NIL. Comparison results are tagged.
        let (lt_inst, _) = f.push_inst(
            e,
            InstData {
                opcode: Opcode::FixnumCmpLt,
                args: vec![c3, c4],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[tagged()],
        );
        let (eq_inst, _) = f.push_inst(
            e,
            InstData {
                opcode: Opcode::FixnumCmpEq,
                args: vec![c3, c4],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[tagged()],
        );
        ret(&mut f, e);

        run(&mut f);

        assert_eq!(f.inst(lt_inst).opcode, Opcode::ConstT);
        assert_eq!(f.inst(eq_inst).opcode, Opcode::ConstNil);
    }

    // A folded value is also updated inside a FrameState (spec §4.10 R4.60).
    // The x+0 → x substitution reroutes a deopt-live slot naming the add's result.
    #[test]
    fn frame_state_value_updated() {
        let mut f = Function::new("deopt");
        let e = f.entry();
        let x = f.add_block_param(
            e,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let c0 = const_fixnum(&mut f, e, 0);
        let (_, addz) = binop(&mut f, e, Opcode::FixnumAdd, vec![x, c0]);

        // A frame state naming the (about-to-be-substituted) addz in a slot.
        let fsid = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Value {
                    value: addz,
                    repr: ValueRepresentation::UnboxedFixnum,
                }],
                stack: vec![],
            }],
            remat: vec![],
        });
        ret(&mut f, e);

        run(&mut f);

        match &f.frame_states.get(fsid).scopes[0].locals[0] {
            ValueSource::Value { value, .. } => {
                assert_eq!(*value, x, "deopt-live add result must reroute to x");
            }
            other => panic!("unexpected value source: {other:?}"),
        }
    }

    // Block-call args are rewritten when a value is substituted.
    #[test]
    fn block_call_args_updated() {
        let mut f = Function::new("blockcall");
        let e = f.entry();
        let x = f.add_block_param(
            e,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        let c0 = const_fixnum(&mut f, e, 0);
        let (_, addz) = binop(&mut f, e, Opcode::FixnumAdd, vec![x, c0]);
        let target = f.make_block();
        let _tp = f.add_block_param(
            target,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::UnboxedFixnum,
        );
        f.set_terminator(
            e,
            InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![BlockCall {
                    block: target,
                    args: vec![addz],
                }],
                frame_state: None,
                source_pos: 0,
            },
        );
        ret(&mut f, target);

        run(&mut f);

        let t = f.terminator(e).unwrap();
        assert_eq!(
            f.inst(t).targets[0].args,
            vec![x],
            "edge arg must reroute to x"
        );
    }
}
