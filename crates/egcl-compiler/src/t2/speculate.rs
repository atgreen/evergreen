// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Profile-guided, single-type speculative lowering of generic arithmetic.
//!
//! # Purpose
//!
//! [`speculate`] rewrites generic `Call`s to arithmetic, comparison and
//! bitwise functions into guarded typed IR ops, committing each site to
//! exactly **one** operand type: the type the SSA already proves, or else the
//! type the interpreter's profile observed there. The typed op keeps the
//! call's `FrameState`, so a wrong-type operand or a fixnum overflow deopts to
//! the interpreter at the site's `bcp`. There is no fallback path for the
//! other type. A polymorphic site is left generic; a mis-speculated site
//! deopts, the profiler observes the new type, and a later recompile commits
//! to that one instead.
//!
//! This is not a `Pass`. The production driver calls it on the freshly built
//! SSA (ordinary Call or native-transfer Invoke), before the mid-end (fold, GVN, guard elimination, DCE), and treats a
//! zero count as fine: the mid-end still improves unspeculated code.
//!
//! # Contract
//!
//! **Input:** a `Function` straight from the builder; `profile(bcp)` returns
//! the one [`SpecType`] to speculate at a call site, or `None` to leave it
//! generic. The site's `bcp` is read from the innermost scope of the call's
//! `FrameState`, where the builder anchors it; a call without a FrameState is
//! never speculated (except the profile-free `NOT`/`NULL` rewrite).
//!
//! **Output:** the number of sites rewritten. Rewritten instructions keep
//! their `Inst` and result `Value`s, so no use rewriting is needed; results
//! are type-refined via `Function::refine_type`. Additional `Guard`
//! instructions and constants may be inserted (below).
//!
//! # Site selection
//!
//! The callee symbol decides the family; `statically_proven_spec_type`
//! (operand SSA types, with fixnum-into-single-float contagion) takes
//! precedence over the profile. Inferred operand constraints veto numeric guesses
//! when an operand is provably nonnumeric; they never remove required guards:
//!
//! * `+ - *` with two arguments → `Fixnum{Add,Sub,Mul}` / `Float{Add,Sub,Mul}`;
//!   unary `-` → `FixnumNeg` (fixnum only); other arities stay generic.
//! * `1+` / `1-` (fixnum) → binary op with a materialised `ConstFixnum 1`.
//! * `< > <= >= =` → typed fixnum compare, or on x86-64 a typed single-float
//!   compare (`FloatCmp*`) that deopts on an unordered (NaN) operand.
//! * `LOGAND LOGIOR LOGXOR LOGNOT` (fixnum) → `Log*`; exact on the tagged
//!   representation, so no overflow and no untag/retag.
//! * `MOD` by a constant positive power of two (fixnum) → `LogAnd` with the
//!   divisor operand **replaced** by the mask on every backend.
//! * On x86-64, binary fixnum `MIN MAX MOD REM` use guarded native templates.
//!   `FLOOR TRUNCATE` use quotient templates only when normal continuations
//!   discard secondary values; observed tuples retain the ordinary call.
//! * `ASH` with two arguments (fixnum) → `FixnumShl`; the emitter checks the
//!   constant shift amount and handles right shifts.
//! * `NOT` / `NULL` → pure `GenericEq(x, NIL)`, no profile, no guard, no
//!   safepoint; correct for every argument type.
//! * `EGCL::%MEMORY-FENCE` with a constant fence keyword → effectful
//!   `MemoryFence` on x86-64 and s390x, after guarding the builtin binding.
//!
//! # Mutation
//!
//! Each selected call is rewritten in place: opcode replaced, any extra
//! constant operand materialised as a const instruction immediately before
//! the site, flags set to `guard + effectful` (a deopt point, ordered; no
//! longer a call or safepoint) with `aux` and `frame_state` retained. The
//! `GenericEq` rewrite instead clears flags and `frame_state` entirely.
//! Every rewrite retains the builtin's single-value return effect in a
//! separate `ClearMv` after the successful operation and its guards.
//! A specialized Invoke then jumps through its original normal projection;
//! newly orphaned exceptional blocks are retired before dominance rewrites.
//! Calls whose exceptional edge is required to reach an OSR header remain
//! generic so its preheader definitions and import state stay valid.
//!
//! Three guard-materialisation steps then run (bliss-x5y.25). Inference
//! assigns each SSA value one fact from its definition onward, so a raw
//! parameter stays `⊤` everywhere even though the emitter checks it before a
//! typed op; these steps reify the discharged proof as an explicit `Guard`
//! value and rewrite the uses it dominates, so the original dies at the guard
//! and what stays live across later safepoints is provably immediate. The
//! emitter's root filter then drops it from GC shadow synchronisation, and a
//! function whose roots all vanish unlocks the direct self-call fast path.
//!
//! 1. `materialize_entry_param_preguards`: a parameter used by at least one
//!    speculated fixnum site, and otherwise only neutrally (self-call
//!    argument, return, edge argument, guard), is
//!    guarded as a fixnum at function entry using the builder's entry
//!    FrameState. Any other use would make a non-fixnum argument legitimate
//!    and the pre-guard a deopt storm.
//! 2. `materialize_self_call_result_guards`: when every return value is
//!    provably a fixnum or another self-call result, each self-call's result
//!    is guarded as a fixnum at the next FrameState-carrying instruction.
//! 3. `materialize_fixnum_operand_guards`: each speculated fixnum site's
//!    variable operands get a `Guard` reusing the site's FrameState. The
//!    site's own FrameState is not rewritten: at a deopt of the guard or the
//!    site, the guard's result does not exist yet.
//!
//! # Limits
//!
//! * Three speculation types: fixnum, single-float and (with
//!   [`SpeculateOptions::double_float`], x86-64 only) double-float; no bignum
//!   fallback. A double site expands to raw `UnboxedF64` IR: guarded
//!   `UnboxFloat` (or `ConstDouble`) operands, a raw `Float*` op and a
//!   `BoxFloat` of the result, which `unbox_f64` then elides where the value
//!   does not escape (bliss-thcbb).
//! * Variadic arithmetic (`(+ a b c)`) and unary `+`/`*` stay generic.
//! * Float comparisons only on x86-64. On the raw double candidate, `SQRT`,
//!   unary `-` and binary `/` of statically known doubles become raw
//!   `FloatSqrt`, `FloatNeg` and `FloatDiv` (bliss-lhy89).
//! * Constant operands are recognised from `ConstFixnum` definitions only;
//!   this runs before folding, so `(mod x (* 2 4))` is not a mask.

use crate::t2::frame_state::FrameStateId;
use crate::t2::ir::{AuxData, Function, IRType, Inst, InstFlags, Opcode, TypeBits, Value};
use std::collections::HashMap;

/// The single type a call site may be speculated as (mutually exclusive).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum SpecType {
    Fixnum,
    SingleFloat,
    /// Boxed double-float operands, computed raw (bliss-thcbb). Produced only
    /// when [`SpeculateOptions::double_float`] admits it.
    DoubleFloat,
}

/// Which speculation families a caller can emit.
#[derive(Copy, Clone, Default, Debug)]
pub struct SpeculateOptions {
    /// Expand double-float sites into raw `UnboxedF64` IR. Only the x86-64
    /// Linux mapped emitter consumes that representation, so the tagged
    /// artifact leaves these sites generic.
    pub double_float: bool,
}

#[derive(Copy, Clone)]
enum Arith {
    Add,
    Sub,
    Mul,
}

#[derive(Copy, Clone)]
enum Cmp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
}

/// The arithmetic op a callee symbol names, if it is one we speculate.
fn arith_of(sym: u32) -> Option<Arith> {
    match egcl_rt::symbols::symbol_name(sym).as_deref() {
        Some("+") => Some(Arith::Add),
        Some("-") => Some(Arith::Sub),
        Some("*") => Some(Arith::Mul),
        _ => None,
    }
}

/// `(MOD x c)` where `c` is a constant, positive power of two → the AND mask
/// that computes it, or `None` when this shape does not apply.
///
/// A tagged fixnum is `n << 3` with a zero tag (value.rs `from_fixnum`), so the
/// shift factors straight out of a bitwise AND and the result is already tagged:
///
///   (mod -7 8):  -7 tagged = -56 = …11001000
///                mask  7 tagged =  56 =   00111000
///                AND            =    8 = tagged 1   ✓ CL says 1
///
/// FLOORED, which is what MOD means, and that is why this works at all: for a
/// POSITIVE power-of-two divisor, two's complement low bits already carry the
/// floored remainder, sign included. REM is NOT this -- `(rem -7 8)` is -7, not
/// 1 -- so REM must not be routed here, and it is not.
///
/// A negative divisor is refused: `(mod 7 -8)` is -1, which the mask does not
/// give. Zero cannot appear (`is_power_of_two` excludes it), so the
/// DIVISION-BY-ZERO path is untouched and still signals from the generic call.
fn mod_mask_of(f: &Function, args: &[crate::t2::ir::Value], consts: &HashMap<Value, i64>) -> Option<i64> {
    let _ = f;
    if args.len() != 2 {
        return None;
    }
    let c = *consts.get(&args[1])?;
    (c > 0 && (c as u64).is_power_of_two()).then(|| c - 1)
}

/// `1+` / `1-` — unary increment/decrement, speculated as the binary fixnum
/// op with a materialised constant-1 operand (bliss-x5y.25: TAK's `(1- z)`
/// stayed a generic call, keeping a safepoint per site and `z` forever
/// unproven).
fn incdec_of(sym: u32) -> Option<Arith> {
    match egcl_rt::symbols::symbol_name(sym).as_deref() {
        Some("1+") => Some(Arith::Add),
        Some("1-") => Some(Arith::Sub),
        _ => None,
    }
}

/// `NOT` / `NULL` — pure boolean tests, equivalent to `(eq x nil)` for EVERY
/// argument type, so the rewrite to `GenericEq(x, nil)` needs no profile, no
/// guard, and no safepoint (bliss-x5y.25: TAK's `(not (< y x))` kept a generic
/// call whose safepoint rooted every live value).
fn is_not_of(sym: u32) -> bool {
    matches!(
        egcl_rt::symbols::symbol_name(sym).as_deref(),
        Some("NOT") | Some("NULL")
    )
}

/// An operand a mutation appends to the rewritten site's argument list,
/// materialised as a const instruction placed just before it.
#[derive(Copy, Clone, Eq, PartialEq)]
enum ExtraArg {
    None,
    FixnumOne,
    Nil,
    /// Replace the SECOND operand with a materialised fixnum constant, rather
    /// than appending a third. `MOD` by a constant power of two becomes a mask
    /// (see `mod_mask_of`), so the divisor operand is not merely unused — it
    /// must go, or the rewritten `LogAnd` would AND against the divisor.
    ReplaceSecondWithFixnum(i64),
    /// Replace a literal keyword argument with an ordered native fence.
    Fence(egcl_rt::bytecode::MemoryFenceKind),
    /// A double-float site: expand into raw operands, a raw operation and,
    /// for arithmetic, a `BoxFloat` of its result (see `expand_double_site`).
    DoubleFloat,
}

/// Literal keyword accepted by the native fence specialization. The caller
/// must separately guard the builtin binding before emitting the operation.
pub fn literal_memory_fence_kind(symbol: u32) -> Option<egcl_rt::bytecode::MemoryFenceKind> {
    use egcl_rt::bytecode::MemoryFenceKind;
    match egcl_rt::symbols::registry_key(symbol).as_deref() {
        Some("KEYWORD:READ" | "KEYWORD:DATA-DEPENDENCY") => Some(MemoryFenceKind::Read),
        Some("KEYWORD:WRITE") => Some(MemoryFenceKind::Write),
        Some("KEYWORD:FULL") => Some(MemoryFenceKind::Full),
        _ => None,
    }
}

#[derive(Copy, Clone)]
enum Bitwise {
    And,
    Or,
    Xor,
    Not,
}

/// A bitwise op we speculate (fixnum only). These are exact on the tagged
/// representation — `tagged(a) OP tagged(b) = (a OP b)<<3` — so no overflow and no
/// untag/retag. Crypto/compression/hashing are dominated by these.
fn bitwise_of(sym: u32) -> Option<Bitwise> {
    match egcl_rt::symbols::symbol_name(sym).as_deref() {
        Some("LOGAND") => Some(Bitwise::And),
        Some("LOGIOR") => Some(Bitwise::Or),
        Some("LOGXOR") => Some(Bitwise::Xor),
        Some("LOGNOT") => Some(Bitwise::Not),
        _ => None,
    }
}

fn bitwise_opcode(b: Bitwise) -> Opcode {
    match b {
        Bitwise::And => Opcode::LogAnd,
        Bitwise::Or => Opcode::LogOr,
        Bitwise::Xor => Opcode::LogXor,
        Bitwise::Not => Opcode::LogNot,
    }
}

/// The comparison a callee symbol names, if it is one we speculate. These feed
/// `Brif`, so speculating them (guarded fixnum/float `Cmp`) is what lets a branch
/// on `(< x n)` reach T2 as a `cmp`+`jcc` instead of a generic call.
fn cmp_of(sym: u32) -> Option<Cmp> {
    match egcl_rt::symbols::symbol_name(sym).as_deref() {
        Some("<") => Some(Cmp::Lt),
        Some(">") => Some(Cmp::Gt),
        Some("<=") => Some(Cmp::Le),
        Some(">=") => Some(Cmp::Ge),
        Some("=") => Some(Cmp::Eq),
        _ => None,
    }
}

/// Numeric calls whose operands can inform T2 speculation. The interpreter
/// uses this alongside the same operation classifiers as the rewrite below,
/// including integer operations whose inputs may require bignums.
pub fn profiles_numeric_operands(sym: u32) -> bool {
    arith_of(sym).is_some()
        || incdec_of(sym).is_some()
        || cmp_of(sym).is_some()
        || bitwise_of(sym).is_some()
        || matches!(egcl_rt::symbols::symbol_name(sym).as_deref(),
            Some("ASH" | "MIN" | "MAX" | "MOD" | "REM" | "FLOOR" | "TRUNCATE"
                | "SQRT" | "/"))
}

/// The typed comparison opcode for `(kind, speculated-type)`. Fixnum supports all
/// five on every backend. Single-float comparisons have an emitter only on
/// x86-64 (bliss-9bw59); elsewhere keep their ordinary calls rather than
/// producing IR that would force the entire function out of T2.
fn typed_cmp_opcode(c: Cmp, s: SpecType) -> Option<Opcode> {
    use Opcode::*;
    Some(match (c, s) {
        (Cmp::Lt, SpecType::Fixnum) => FixnumCmpLt,
        (Cmp::Gt, SpecType::Fixnum) => FixnumCmpGt,
        (Cmp::Le, SpecType::Fixnum) => FixnumCmpLe,
        (Cmp::Ge, SpecType::Fixnum) => FixnumCmpGe,
        (Cmp::Eq, SpecType::Fixnum) => FixnumCmpEq,
        (_, SpecType::SingleFloat | SpecType::DoubleFloat) if !cfg!(target_arch = "x86_64") => {
            return None
        }
        (Cmp::Lt, SpecType::SingleFloat) => FloatCmpLt,
        (Cmp::Gt, SpecType::SingleFloat) => FloatCmpGt,
        (Cmp::Le, SpecType::SingleFloat) => FloatCmpLe,
        (Cmp::Ge, SpecType::SingleFloat) => FloatCmpGe,
        (Cmp::Eq, SpecType::SingleFloat | SpecType::DoubleFloat) => FloatCmpEq,
        (Cmp::Lt, SpecType::DoubleFloat) => FloatCmpLt,
        (Cmp::Gt, SpecType::DoubleFloat) => FloatCmpGt,
        (Cmp::Le, SpecType::DoubleFloat) => FloatCmpLe,
        (Cmp::Ge, SpecType::DoubleFloat) => FloatCmpGe,
    })
}

/// The typed opcode a `(kind, speculated-type)` lowers to.
fn typed_opcode(a: Arith, s: SpecType) -> Opcode {
    use Opcode::*;
    match (a, s) {
        (Arith::Add, SpecType::Fixnum) => FixnumAdd,
        (Arith::Sub, SpecType::Fixnum) => FixnumSub,
        (Arith::Mul, SpecType::Fixnum) => FixnumMul,
        (Arith::Add, SpecType::SingleFloat) => FloatAdd,
        (Arith::Sub, SpecType::SingleFloat) => FloatSub,
        (Arith::Mul, SpecType::SingleFloat) => FloatMul,
        (Arith::Add, SpecType::DoubleFloat) => FloatAdd,
        (Arith::Sub, SpecType::DoubleFloat) => FloatSub,
        (Arith::Mul, SpecType::DoubleFloat) => FloatMul,
    }
}

fn result_type(s: SpecType) -> IRType {
    IRType::of(match s {
        SpecType::Fixnum => TypeBits::FIXNUM,
        SpecType::SingleFloat => TypeBits::SINGLE_FLOAT,
        SpecType::DoubleFloat => TypeBits::DOUBLE_FLOAT,
    })
}

/// T or NIL — a comparison's result. Both are non-pointer immediates (NIL/T are
/// tag-111 specials), so typing the result this way lets the emitter's
/// safepoint-root filter drop a live-across boolean from GC shadow sync
/// (bliss-x5y.25 option b).
fn boolean_type() -> IRType {
    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL))
}

fn frame_state_bcp(f: &Function, fs: FrameStateId) -> u32 {
    f.frame_states
        .get(fs)
        .scopes
        .last()
        .map(|s| s.bcp)
        .unwrap_or(0)
}

trait OperandFacts {
    fn bits(&self, value: Value) -> Option<TypeBits>;
}

impl OperandFacts for super::infer::InferenceResult {
    fn bits(&self, value: Value) -> Option<TypeBits> {
        self.get(value).map(|ty| ty.bits)
    }
}

struct CapturedOperandFacts(Vec<(Value, TypeBits)>);

impl OperandFacts for CapturedOperandFacts {
    fn bits(&self, value: Value) -> Option<TypeBits> {
        self.0.iter().find_map(|&(v, bits)| (v == value).then_some(bits))
    }
}

fn operand_bits(
    f: &Function,
    inferred: Option<&dyn OperandFacts>,
    value: Value,
) -> TypeBits {
    let declared = f.value(value).ty.bits;
    let Some(bits) = inferred.and_then(|facts| facts.bits(value)) else { return declared };
    if bits.is_bottom() || declared.is_bottom() { return declared; }
    declared.meet(bits)
}

fn conflicting_operand_facts(f: &Function, inferred: Option<&dyn OperandFacts>, args: &[Value]) -> bool {
    args.iter().any(|&value| {
        let declared = f.value(value).ty.bits;
        inferred.and_then(|facts| facts.bits(value)).is_some_and(|bits|
            !declared.is_bottom() && !bits.is_bottom() && declared.meet(bits).is_bottom())
    })
}

/// Resolve a numeric specialization entirely from SSA types. This is the path
/// used by declared parameters: no sampled type is needed, and a stale profile
/// can never contradict the source assertion. A fixnum constant participates
/// in a single-float operation through CL float contagion.
fn statically_proven_spec_type(f: &Function, args: &[crate::t2::ir::Value], inferred: Option<&dyn OperandFacts>) -> Option<SpecType> {
    let subset =
        |bits: TypeBits, allowed: TypeBits| !bits.is_bottom() && bits.meet(allowed) == bits;
    if !args.is_empty()
        && args
            .iter()
            .all(|&arg| subset(operand_bits(f, inferred, arg), TypeBits::FIXNUM))
    {
        return Some(SpecType::Fixnum);
    }

    let numeric = TypeBits::FIXNUM.join(TypeBits::SINGLE_FLOAT);
    let all_single_numeric = !args.is_empty()
        && args
            .iter()
            .all(|&arg| subset(operand_bits(f, inferred, arg), numeric));
    let has_single = args
        .iter()
        .any(|&arg| subset(operand_bits(f, inferred, arg), TypeBits::SINGLE_FLOAT));
    (all_single_numeric && has_single).then_some(SpecType::SingleFloat)
}

/// Hints choose a guarded guess, never operand admissibility or check removal.
/// Consult only after real proofs, inside an admitted profileSome path.
fn hinted_family(f: &Function, args: &[Value], inferred: Option<&dyn OperandFacts>,
    options: SpeculateOptions) -> Option<SpecType> {
    let mut hinted = false;
    let mut single = false;
    let mut double = false;
    let mut integer = false;
    for &value in args {
        let actual = operand_bits(f, inferred, value);
        let bits = if actual == TypeBits::TOP || actual.is_bottom() {
            let Some(ty) = f.return_type_hints.get(&value) else { continue; };
            hinted = true;
            ty.bits
        } else { actual };
        if bits == TypeBits::SINGLE_FLOAT { single = true; }
        else if bits == TypeBits::DOUBLE_FLOAT { double = true; }
        else if bits == TypeBits::FIXNUM { integer = true; }
        else if !bits.meet(TypeBits::SINGLE_FLOAT.join(TypeBits::DOUBLE_FLOAT)).is_bottom() {
            return None; // A mixed prototype/format is not a precise hint.
        }
    }
    if !hinted { return None; }
    if double && options.double_float && cfg!(target_arch = "x86_64") { Some(SpecType::DoubleFloat) }
    else if single && !double { Some(SpecType::SingleFloat) }
    else if integer && !single && !double { Some(SpecType::Fixnum) }
    else { None }
}

pub(crate) fn accepts_return_hints(symbol: u32, arity: usize) -> bool {
    arity == 2 && (arith_of(symbol).is_some() || cmp_of(symbol).is_some())
}

/// An expectation can reject an incompatible guarded guess without proving
/// anything about the runtime operand. Actual operand facts take precedence.
fn hints_permit_family(f: &Function, args: &[Value], inferred: Option<&dyn OperandFacts>,
    kind: SpecType, comparison: bool) -> bool {
    let allowed = match kind {
        SpecType::Fixnum => TypeBits::FIXNUM,
        SpecType::SingleFloat => TypeBits::SINGLE_FLOAT.join(TypeBits::FIXNUM),
        SpecType::DoubleFloat if comparison => TypeBits::DOUBLE_FLOAT,
        SpecType::DoubleFloat => TypeBits::DOUBLE_FLOAT.join(TypeBits::FIXNUM),
    };
    args.iter().all(|&value| {
        let actual = operand_bits(f, inferred, value);
        if actual != TypeBits::TOP && !actual.is_bottom() { return true; }
        f.return_type_hints.get(&value).is_none_or(|hint|
            hint.bits.is_bottom() || !hint.bits.meet(allowed).is_bottom())
    })
}

/// The IEEE bits of `value` when it is a compile-time double-float operand: a
/// `ConstDouble`, or a `ConstHeapObj` whose literal is a double. The literal is
/// described by the builder's immutable snapshot. Optimization never reads
/// the rooted pool slot or the movable boxed object.
pub(crate) fn double_literal_bits(f: &Function, value: Value) -> Option<u64> {
    let crate::t2::ir::ValueDef::Result { inst, .. } = f.value(value).def else {
        return None;
    };
    let data = f.inst(inst);
    match (data.opcode, &data.aux) {
        (Opcode::ConstDouble, AuxData::DoubleImm(bits)) => Some(*bits),
        (Opcode::ConstHeapObj, AuxData::HeapLiteral { slot })
            if f.value(value).ty.bits == TypeBits::DOUBLE_FLOAT =>
        {
            f.double_literals.get(slot).copied()
        }
        _ => None,
    }
}

/// The exact double a compile-time operand contributes to a double-float
/// operation, as the interpreter converts it (float contagion): a fixnum
/// constant rounds to nearest like `BigRat::to_f64`, a single-float widens
/// exactly. `comparison` keeps only fixnums that are exactly doubles, since
/// the interpreter may compare a rational exactly rather than by converting it.
fn double_constant_operand(
    f: &Function,
    value: Value,
    consts: &HashMap<Value, i64>,
    comparison: bool,
) -> Option<u64> {
    if let Some(bits) = double_literal_bits(f, value) {
        return Some(bits);
    }
    if let Some(&k) = consts.get(&value) {
        if comparison && k.unsigned_abs() > 1 << 53 {
            return None;
        }
        return Some((k as f64).to_bits());
    }
    let crate::t2::ir::ValueDef::Result { inst, .. } = f.value(value).def else {
        return None;
    };
    match (f.inst(inst).opcode, &f.inst(inst).aux) {
        (Opcode::ConstFloat, AuxData::FloatImm(x)) => Some((*x as f64).to_bits()),
        _ => None,
    }
}

/// Whether every operand of a site can take part in a native double-float
/// operation: a compile-time double (literal, fixnum or single-float constant)
/// or a variable that may be a boxed double. At least one operand must be a
/// variable; two constants stay with the generic call.
fn double_operands_admissible(
    f: &Function,
    args: &[Value],
    consts: &HashMap<Value, i64>,
    comparison: bool,
    inferred: Option<&dyn OperandFacts>,
) -> bool {
    // Arithmetic also takes a fixnum variable beside a double (contagion).
    let variable_bits = if comparison {
        TypeBits::DOUBLE_FLOAT
    } else {
        TypeBits::DOUBLE_FLOAT.join(TypeBits::FIXNUM)
    };
    let may_be_double = |arg: Value| {
        double_literal_bits(f, arg).is_some() || {
            let bits = operand_bits(f, inferred, arg);
            double_constant_operand(f, arg, consts, comparison).is_none()
                && !bits.is_bottom()
                && !bits.meet(TypeBits::DOUBLE_FLOAT).is_bottom()
        }
    };
    args.len() == 2
        && args.iter().all(|&arg| {
            double_constant_operand(f, arg, consts, comparison).is_some() || {
                let bits = operand_bits(f, inferred, arg);
                !bits.is_bottom() && !bits.meet(variable_bits).is_bottom()
            }
        })
        // Two constants form a site only with a double literal among them,
        // as `(* 1.5d0 1.5d0)` after a declared double is propagated; the raw
        // operation computes exactly what the interpreter would.
        && (args
            .iter()
            .any(|&arg| double_constant_operand(f, arg, consts, comparison).is_none())
            || (!comparison && args.iter().any(|&arg| double_literal_bits(f, arg).is_some())))
        && args.iter().any(|&arg| may_be_double(arg))
}

/// A double-float site proven from operand types alone: every operand is a
/// compile-time double constant or an SSA value typed exactly DOUBLE-FLOAT.
fn statically_proven_double(
    f: &Function,
    args: &[Value],
    consts: &HashMap<Value, i64>,
    comparison: bool,
    inferred: Option<&dyn OperandFacts>,
) -> bool {
    double_operands_admissible(f, args, consts, comparison, inferred)
        && args.iter().all(|&arg| {
            double_constant_operand(f, arg, consts, comparison).is_some()
                || operand_bits(f, inferred, arg) == TypeBits::DOUBLE_FLOAT
        })
}

/// The site's speculation once double-float operands are considered
/// (bliss-thcbb). A known double operand (a literal such as `1d0`) can never
/// pass a fixnum or single-float guard, so such a site speculates double-float
/// when `options` admit it and every operand can take part; otherwise the
/// later operand reconciliation keeps it generic. A profiled double site is
/// kept only when admitted, with operands that can all take part.
fn reconcile_double(
    f: &Function,
    args: &[Value],
    consts: &HashMap<Value, i64>,
    comparison: bool,
    spec: SpecType,
    options: SpeculateOptions,
    inferred: Option<&dyn OperandFacts>,
) -> Option<SpecType> {
    let admitted = options.double_float
        && cfg!(target_arch = "x86_64")
        && double_operands_admissible(f, args, consts, comparison, inferred);
    let known_double = args.iter().any(|&arg| {
        operand_bits(f, inferred, arg) == TypeBits::DOUBLE_FLOAT || double_literal_bits(f, arg).is_some()
    });
    match spec {
        SpecType::DoubleFloat => admitted.then_some(SpecType::DoubleFloat),
        _ if known_double && admitted => Some(SpecType::DoubleFloat),
        other => Some(other),
    }
}

fn is_float_compare(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::FloatCmpEq | Opcode::FloatCmpLt | Opcode::FloatCmpLe | Opcode::FloatCmpGt
            | Opcode::FloatCmpGe
    )
}

/// Insert `data` immediately before `site` in its block; return its result.
fn insert_before_site(
    f: &mut Function,
    site: Inst,
    data: crate::t2::ir::InstData,
    result: (IRType, crate::t2::ir::ValueRepresentation),
) -> Value {
    let block = f
        .block_order()
        .iter()
        .copied()
        .find(|&b| f.block(b).insts.contains(&site))
        .expect("site is in a block");
    let (inst, results) = f.push_inst(block, data, &[result]);
    let insts = &mut f.block_mut(block).insts;
    let appended = insts.pop();
    debug_assert_eq!(appended, Some(inst));
    let position = insts.iter().position(|&i| i == site).expect("site is in its block");
    insts.insert(position, inst);
    results[0]
}

/// `UnboxFloat` auxiliary data for an arithmetic operand that may also be a
/// fixnum, converted to the nearest double exactly as the interpreter's float
/// contagion does (`BigRat::to_f64` and `cvtsi2sd` both round to nearest). A
/// second operand names the other variable: when both are fixnums the
/// interpreter's answer is an integer, so that guard deopts (bliss-thcbb).
pub fn contagion_unbox_aux() -> AuxData {
    AuxData::TypeTag(IRType::of(TypeBits::FIXNUM.join(TypeBits::DOUBLE_FLOAT)))
}

/// Whether an `UnboxFloat` accepts a fixnum operand (see [`contagion_unbox_aux`]).
pub fn unbox_accepts_fixnum(aux: &AuxData) -> bool {
    matches!(aux, AuxData::TypeTag(ty)
        if ty.bits == TypeBits::FIXNUM.join(TypeBits::DOUBLE_FLOAT))
}

/// Expand a double-float site into raw IR (bliss-thcbb). Each variable operand
/// is unboxed by a guarded `UnboxFloat` that deopts, at the site's own
/// bytecode position, on anything but a boxed double or, for arithmetic, a
/// fixnum joining the double by contagion; a compile-time operand becomes a
/// `ConstDouble` converted exactly as the interpreter's contagion would.
/// Arithmetic then computes into a fresh raw value, and the site itself
/// becomes the `BoxFloat` producing its original tagged result. A comparison
/// keeps the site (now `FloatCmp*` over raw operands, deopting on NaN); its
/// operands must be doubles, since the interpreter compares a fixnum with a
/// double exactly rather than by converting it.
fn expand_double_site(f: &mut Function, site: Inst, opcode: Opcode, consts: &HashMap<Value, i64>) {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let comparison = is_float_compare(opcode);
    let data = f.inst(site).clone();
    let state = data.frame_state.expect("double site keeps its frame state");
    let raw_double = (IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64);
    let constant = |f: &Function, value: Value| double_constant_operand(f, value, consts, comparison);
    // A double partner (literal or proven) makes the result a double whatever
    // this operand is. Beside a fixnum or single-float constant the variable
    // must itself be a double: the interpreter's answer would otherwise be
    // an integer or a single-float. Two unproven variables check the pair.
    let proven_double = |f: &Function, value: Value| {
        double_literal_bits(f, value).is_some() || f.value(value).ty.bits == TypeBits::DOUBLE_FLOAT
    };
    let mut raw = Vec::with_capacity(data.args.len());
    for (index, &arg) in data.args.iter().enumerate() {
        let partner = data.args[1 - index];
        let (aux, args) = if comparison {
            (AuxData::None, vec![arg])
        } else if proven_double(f, partner) {
            (contagion_unbox_aux(), vec![arg])
        } else if constant(f, partner).is_some() {
            (AuxData::None, vec![arg])
        } else if index == 1 && !proven_double(f, arg) {
            (contagion_unbox_aux(), vec![arg, partner])
        } else {
            (contagion_unbox_aux(), vec![arg])
        };
        let value = if let Some(bits) = constant(f, arg) {
            insert_before_site(
                f,
                site,
                InstData {
                    opcode: Opcode::ConstDouble,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::DoubleImm(bits),
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: None,
                    source_pos: data.source_pos,
                },
                raw_double,
            )
        } else {
            let guard_state = f.frame_states.add(f.frame_states.get(state).clone());
            insert_before_site(
                f,
                site,
                InstData {
                    opcode: Opcode::UnboxFloat,
                    args,
                    results: vec![],
                    aux,
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(guard_state),
                    source_pos: data.source_pos,
                },
                raw_double,
            )
        };
        raw.push(value);
    }
    if comparison {
        f.inst_mut(site).args = raw;
        return;
    }
    let result = insert_before_site(
        f,
        site,
        InstData {
            opcode,
            args: raw,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: data.source_pos,
        },
        raw_double,
    );
    f.inst_mut(site).args = vec![result];
}

/// Correct a profiled (or cold-guessed) type against operands whose SSA type
/// already rules it out (bliss-9bw59). A literal `2.0` can never pass a fixnum
/// guard, so a cold fixnum guess at `(- n 2.0)` only bought a certain deopt and,
/// worse, a fixnum guard on a constant that no emitter accepts, costing the
/// whole function its native code. Such a site speculates single-float when an
/// operand is known to be one (fixnum operands join it through contagion), and
/// stays generic when an operand is known to be neither type.
fn reconcile_with_operands(
    f: &Function,
    args: &[crate::t2::ir::Value],
    spec: SpecType,
    inferred: Option<&dyn OperandFacts>,
) -> Option<SpecType> {
    if spec == SpecType::DoubleFloat {
        return Some(spec);
    }
    let numeric = TypeBits::FIXNUM.join(TypeBits::SINGLE_FLOAT);
    let known = |arg: &crate::t2::ir::Value| {
        let bits = operand_bits(f, inferred, *arg);
        (!bits.is_bottom() && bits != TypeBits::TOP).then_some(bits)
    };
    if args.iter().filter_map(known).any(|bits| bits.meet(numeric).is_bottom()) {
        return None;
    }
    let single = |bits: TypeBits| bits.meet(TypeBits::SINGLE_FLOAT) == bits;
    match spec {
        SpecType::Fixnum
            if args.iter().filter_map(known).any(|bits| bits.meet(TypeBits::FIXNUM).is_bottom()) =>
        {
            args.iter().filter_map(known).any(single).then_some(SpecType::SingleFloat)
        }
        _ => Some(spec),
    }
}

/// Operand constraints for one immutable selection phase. Inferred types may
/// correct a profiled family or veto an impossible guess. They never remove
/// runtime guards or broaden floating-point contagion. Inference is lazy and shared
/// across sites, and the snapshot is dropped before any IR mutation.
#[derive(Default)]
struct NumericConstraints(std::cell::OnceCell<super::infer::InferenceResult>);

impl NumericConstraints {
    fn inferred_for<'a>(&'a self, f: &Function, args: &[Value]) -> Option<&'a super::infer::InferenceResult> {
        args.iter().any(|&arg| f.value(arg).ty.bits.0.count_ones() > 1)
            .then(|| self.0.get_or_init(|| super::infer::infer(f)))
    }

    fn excludes_numeric(&self, f: &Function, args: &[Value]) -> bool {
        let numeric = TypeBits::FIXNUM
            .join(TypeBits::BIGNUM)
            .join(TypeBits::RATIO)
            .join(TypeBits::SINGLE_FLOAT)
            .join(TypeBits::DOUBLE_FLOAT)
            .join(TypeBits::COMPLEX);
        let excludes = |bits: TypeBits| !bits.is_bottom() && bits.meet(numeric).is_bottom();
        if args.iter().any(|&arg| excludes(f.value(arg).ty.bits)) {
            return true;
        }
        // Numeric declarations cannot yield a nonnumeric refinement. Avoid a
        // whole analysis when all operands already have numeric declarations.
        if args.iter().all(|&arg| {
            let bits = f.value(arg).ty.bits;
            bits.is_bottom() || bits.meet(numeric) == bits
        }) {
            return false;
        }
        let facts = self.0.get_or_init(|| super::infer::infer(f));
        args.iter().any(|&arg| excludes(facts.ty(arg).bits))
    }
}

/// Split a `+`, `-` or `*` of three or more operands whose site profiled
/// double-float into a left-to-right chain of binary calls (bliss-thcbb.1), so
/// each link can take the binary raw double form. The interpreter folds the
/// same way: `-` from its first operand, `*` onto a 1 seed that leaves a double
/// unchanged, and `+` onto a +0.0 seed, whose only effect (turning a -0.0 sum
/// into +0.0) every raw `FloatAdd` already reproduces. Each link deoptimizes
/// with the whole call's frame state: arithmetic has no effects, so the
/// interpreter simply evaluates the original call again.
fn split_variadic_double_arithmetic(f: &mut Function, profile: &impl Fn(u32) -> Option<SpecType>) {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let constraints = NumericConstraints::default();
    let mut sites = Vec::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            if data.opcode != Opcode::Call || data.args.len() < 3 || data.results.len() != 1 {
                continue;
            }
            let AuxData::CallTarget(sym) = data.aux else { continue };
            let Some(state) = data.frame_state else { continue };
            if arith_of(sym).is_none() || profile(frame_state_bcp(f, state)) != Some(SpecType::DoubleFloat) {
                continue;
            }
            if !constraints.excludes_numeric(f, &data.args) {
                sites.push(inst);
            }
        }
    }
    drop(constraints);
    for site in sites {
        let data = f.inst(site).clone();
        let state = data.frame_state.expect("collected with a frame state");
        let (last, leading) = data.args.split_last().expect("three or more operands");
        let mut accumulated = leading[0];
        for &operand in &leading[1..] {
            let link_state = f.frame_states.add(f.frame_states.get(state).clone());
            accumulated = insert_before_site(
                f,
                site,
                InstData {
                    opcode: Opcode::Call,
                    args: vec![accumulated, operand],
                    results: vec![],
                    aux: data.aux.clone(),
                    flags: data.flags,
                    targets: vec![],
                    frame_state: Some(link_state),
                    source_pos: data.source_pos,
                },
                (IRType::TOP, ValueRepresentation::Tagged),
            );
        }
        f.inst_mut(site).args = vec![accumulated, *last];
    }
}

/// The raw double operation a double-only builtin names, with its arity:
/// `SQRT`, unary `-` and binary `/` (bliss-lhy89).
fn double_only_opcode(sym: u32, argc: usize) -> Option<Opcode> {
    match (egcl_rt::symbols::symbol_name(sym).as_deref(), argc) {
        (Some("SQRT"), 1) => Some(Opcode::FloatSqrt),
        (Some("-"), 1) => Some(Opcode::FloatNeg),
        (Some("/"), 2) => Some(Opcode::FloatDiv),
        _ => None,
    }
}

/// Whether `args` of a double-only site can compute raw. Without a profile
/// the operands must be statically doubles: a unary operand typed exactly
/// DOUBLE-FLOAT (or a double literal); for `/`, at least one proven double
/// variable or literal, the other a compile-time number converted exactly or
/// a fixnum variable beside the proven double (float contagion, rounded to
/// nearest as the interpreter does). A site whose operands `observed` doubles
/// (its profile chose DOUBLE-FLOAT) takes any operand that may be one, as
/// the profiled arithmetic does; its guards then decline it on a miss. A
/// division needs a variable operand and never a constant zero divisor
/// (that always signals, so it stays generic).
fn double_only_operands(
    f: &Function,
    opcode: Opcode,
    args: &[Value],
    consts: &HashMap<Value, i64>,
    observed: bool,
    inferred: Option<&dyn OperandFacts>,
) -> bool {
    let proven = |value: Value| {
        double_literal_bits(f, value).is_some() || operand_bits(f, inferred, value) == TypeBits::DOUBLE_FLOAT
    };
    let may_be_double = |value: Value| {
        let bits = operand_bits(f, inferred, value);
        !bits.is_bottom() && !bits.meet(TypeBits::DOUBLE_FLOAT).is_bottom()
    };
    let constant = |value: Value| double_constant_operand(f, value, consts, false);
    if opcode != Opcode::FloatDiv {
        return args.len() == 1
            && (proven(args[0]) || (observed && constant(args[0]).is_none() && may_be_double(args[0])));
    }
    let fixnum = |value: Value| {
        let bits = operand_bits(f, inferred, value);
        !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits
    };
    let statically = args.len() == 2
        && args.iter().any(|&arg| proven(arg))
        && args.iter().all(|&arg| proven(arg) || constant(arg).is_some() || fixnum(arg))
        && (0..2).all(|index| !fixnum(args[index]) || constant(args[index]).is_some()
            || proven(args[1 - index]));
    args.len() == 2
        && args.iter().any(|&arg| constant(arg).is_none())
        && constant(args[1]).is_none_or(|bits| f64::from_bits(bits) != 0.0)
        && (statically || (observed && double_operands_admissible(f, args, consts, false, inferred)))
}

/// Compile `SQRT`, unary `-` and binary `/` of doubles to raw operations
/// (bliss-lhy89): operands proven DOUBLE-FLOAT (a literal, or the result of
/// a raw double site rewritten earlier in this pass or by the main rewrite),
/// or operands the site's profile saw as doubles (see
/// `double_only_operands`). Each site gets the same builtin-binding guard
/// and multiple-value reset as a profiled site, guarded operand unboxing, and
/// becomes the escape `BoxFloat` of the raw result. `SQRT` and negation
/// unbox doubles only: `(sqrt 4)` is a single-float.
///
/// `FloatSqrt` deopts on a negative operand (its root is a complex) and
/// `FloatDiv` on a zero divisor (DIVISION-BY-ZERO is signalled); both
/// resume the original call in the interpreter. A failed guard declines the
/// site (`profile` then answers `None` for it), so the replacement version
/// keeps the call generic instead of deoptimizing again.
fn speculate_double_only_sites(
    f: &mut Function,
    profile: &impl Fn(u32) -> Option<SpecType>,
    consts: &HashMap<Value, i64>,
) -> usize {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let raw_double = (IRType::of(TypeBits::DOUBLE_FLOAT), ValueRepresentation::UnboxedF64);
    let mut n = 0;
    // Program order, so a site sees the refined type of an earlier one.
    let constraints = NumericConstraints::default();
    let sites: Vec<(Inst, SpecType, CapturedOperandFacts)> = {
        f.block_order().iter()
            .flat_map(|&block| f.block(block).insts.iter().copied())
            .filter_map(|site| {
                let data = f.inst(site);
                let AuxData::CallTarget(symbol) = data.aux else { return None };
                if data.opcode != Opcode::Call || data.results.len() != 1
                    || !data.targets.is_empty()
                    || double_only_opcode(symbol, data.args.len()).is_none()
                {
                    return None;
                }
                let state = data.frame_state?;
                let observed = profile(frame_state_bcp(f, state))?;
                if constraints.excludes_numeric(f, &data.args) { return None; }
                let inferred = constraints.inferred_for(f, &data.args).map(|facts| facts as &dyn OperandFacts);
                if conflicting_operand_facts(f, inferred, &data.args) { return None; }
                let captured = CapturedOperandFacts(data.args.iter().map(|&value|
                    (value, operand_bits(f, inferred, value))).collect());
                Some((site, observed, captured))
            })
            .collect()
    };
    drop(constraints);
    for (site, observed, facts) in sites {
        let data = f.inst(site).clone();
        let AuxData::CallTarget(symbol) = data.aux else { continue };
        if data.opcode != Opcode::Call || data.results.len() != 1 || !data.targets.is_empty() {
            continue;
        }
        let Some(opcode) = double_only_opcode(symbol, data.args.len()) else { continue };
        let Some(state) = data.frame_state else { continue };
        // Declined sites were excluded during the immutable selection phase.
        // Earlier expansions preserve these operands' successful values and
        // can strengthen their declarations. Intersect both sources of type
        // evidence; captured TOP never hides a newly declared double, while a
        // known incompatible type cannot be forgotten by the fallback.
        if conflicting_operand_facts(f, Some(&facts), &data.args)
            || !double_only_operands(f, opcode, &data.args, consts,
                observed == SpecType::DoubleFloat, Some(&facts))
        {
            continue;
        }
        let Some(binding) = super::builtin_binding::snapshot(symbol) else { continue };
        let Some((block, position)) = f.block_order().iter().find_map(|&block| {
            f.block(block).insts.iter().position(|&candidate| candidate == site)
                .map(|position| (block, position))
        }) else { continue };
        super::builtin_binding::insert(f, block, position, state, binding);
        let guarded = InstFlags { guard: true, effectful: true, ..InstFlags::default() };
        let raw = if opcode == Opcode::FloatDiv {
            expand_double_site(f, site, opcode, consts);
            let raw = f.inst(site).args[0];
            let crate::t2::ir::ValueDef::Result { inst: divide, .. } = f.value(raw).def else {
                unreachable!("the expansion defines the quotient");
            };
            let divide_state = f.frame_states.add(f.frame_states.get(state).clone());
            let divide = f.inst_mut(divide);
            divide.flags = guarded;
            divide.frame_state = Some(divide_state);
            raw
        } else {
            let operand = data.args[0];
            let input = if let Some(bits) = double_literal_bits(f, operand) {
                insert_before_site(f, site, InstData {
                    opcode: Opcode::ConstDouble,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::DoubleImm(bits),
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: None,
                    source_pos: data.source_pos,
                }, raw_double)
            } else {
                let guard_state = f.frame_states.add(f.frame_states.get(state).clone());
                insert_before_site(f, site, InstData {
                    opcode: Opcode::UnboxFloat,
                    args: vec![operand],
                    results: vec![],
                    aux: AuxData::None,
                    flags: guarded,
                    targets: vec![],
                    frame_state: Some(guard_state),
                    source_pos: data.source_pos,
                }, raw_double)
            };
            let (flags, frame_state) = if opcode == Opcode::FloatSqrt {
                (guarded, Some(f.frame_states.add(f.frame_states.get(state).clone())))
            } else {
                (InstFlags::default(), None)
            };
            insert_before_site(f, site, InstData {
                opcode,
                args: vec![input],
                results: vec![],
                aux: AuxData::None,
                flags,
                targets: vec![],
                frame_state,
                source_pos: data.source_pos,
            }, raw_double)
        };
        {
            let site_data = f.inst_mut(site);
            site_data.opcode = Opcode::BoxFloat;
            site_data.args = vec![raw];
            site_data.aux = AuxData::None;
            site_data.flags = InstFlags { effectful: true, ..InstFlags::default() };
            site_data.frame_state = None;
        }
        f.refine_type(data.results[0], IRType::of(TypeBits::DOUBLE_FLOAT));
        // A successful builtin call resets the multiple values, as in the
        // main rewrite.
        let return_state = f.frame_states.add(f.frame_states.get(state).clone());
        let (clear, _) = f.push_inst(block, InstData {
            opcode: Opcode::ClearMv,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags { effectful: true, call: true, safepoint: true, ..InstFlags::default() },
            targets: vec![],
            frame_state: Some(return_state),
            source_pos: data.source_pos,
        }, &[]);
        let insts = &mut f.block_mut(block).insts;
        let appended = insts.pop();
        debug_assert_eq!(appended, Some(clear));
        let position = insts.iter().position(|&candidate| candidate == site)
            .expect("speculation site remains in its block");
        insts.insert(position + 1, clear);
        n += 1;
    }
    n
}

/// Rewrite generic arithmetic `Call`s into single guarded typed ops guided by
/// static SSA proof first, then `profile` (site `bcp` → the one speculated type,
/// or `None` to leave generic).
/// Returns the number of sites speculated.
pub fn speculate(f: &mut Function, profile: &impl Fn(u32) -> Option<SpecType>) -> usize {
    speculate_with(f, profile, SpeculateOptions::default())
}

/// [`speculate`], optionally expanding double-float sites (bliss-thcbb).
pub fn speculate_with(
    f: &mut Function,
    profile: &impl Fn(u32) -> Option<SpecType>,
    options: SpeculateOptions,
) -> usize {
    speculate_selected(f, profile, options, None)
}

/// Specialize only a proved fast region; the generic/peeled copies keep their
/// original calls. Static operand proofs still take precedence over profiles.
pub(crate) fn speculate_blocks(f: &mut Function, blocks: &[crate::t2::ir::Block]) -> usize {
    speculate_selected(f, &|_| None, SpeculateOptions { double_float: false }, Some(blocks))
}

fn speculate_selected(
    f: &mut Function,
    profile: &impl Fn(u32) -> Option<SpecType>,
    options: SpeculateOptions,
    selected: Option<&[crate::t2::ir::Block]>,
) -> usize {
    if options.double_float && cfg!(target_arch = "x86_64") {
        split_variadic_double_arithmetic(f, profile);
    }
    // Read phase: collect the calls to rewrite (keeps the borrow off `f` for the
    // mutation phase).
    let mut work: Vec<(Inst, Opcode, IRType, ExtraArg)> = Vec::new();
    let constraints = NumericConstraints::default();
    let discarded_values = super::opt_clear_mv::dead_after(f);
    // Constant fixnum operands, so a divisor can be recognised without relying
    // on ConstFold having run -- the T2 driver only runs the mid-end pipeline
    // when `opt` is set, so a rewrite that needed folding afterwards would go
    // unlowered in the unoptimised path.
    let mut consts: HashMap<Value, i64> = HashMap::new();
    let mut fences = HashMap::new();
    for i in 0..f.num_insts() {
        let d = f.inst(Inst(i as u32));
        if d.opcode == Opcode::ConstSymbol
            && let AuxData::SymbolRef(symbol) = d.aux
            && let Some(kind) = literal_memory_fence_kind(symbol)
            && let Some(&result) = d.results.first()
        {
            fences.insert(result, kind);
        }
        if d.opcode == Opcode::ConstFixnum {
            if let AuxData::FixnumImm(k) = d.aux {
                if let Some(&r) = d.results.first() {
                    consts.insert(r, k);
                }
            }
        }
    }
    for &b in f.block_order() {
        if selected.is_some_and(|blocks| !blocks.contains(&b)) { continue; }
        for &inst in &f.block(b).insts {
            let data = f.inst(inst);
            if !matches!(data.opcode, Opcode::Call | Opcode::Invoke) {
                continue;
            }
            let sym = match &data.aux {
                AuxData::CallTarget(s) => *s,
                _ => continue,
            };
            let argc = data.args.len();
            if cfg!(any(target_arch = "x86_64", target_arch = "s390x"))
                && argc == 1
                && egcl_rt::symbols::registry_key(sym).as_deref() == Some("EGCL::%MEMORY-FENCE")
                && let Some(&kind) = fences.get(&data.args[0])
            {
                work.push((inst, Opcode::MemoryFence, IRType::of(TypeBits::NULL), ExtraArg::Fence(kind)));
                continue;
            }
            // NOT/NULL rewrite first: profile-free (correct for every type).
            if is_not_of(sym) && argc == 1 {
                work.push((inst, Opcode::GenericEq, boolean_type(), ExtraArg::Nil));
                continue;
            }
            // The site's bcp is on the Call's FrameState (P1 anchors it there).
            let Some(fs) = data.frame_state else { continue };
            let bcp = frame_state_bcp(f, fs);
            let comparison = cmp_of(sym).is_some();
            let proven_double = options.double_float
                && cfg!(target_arch = "x86_64")
                && (arith_of(sym).is_some() || comparison)
                && statically_proven_double(f, &data.args, &consts, comparison, None);
            let Some(spec) = proven_double
                .then_some(SpecType::DoubleFloat)
                .or_else(|| statically_proven_spec_type(f, &data.args, None))
                .or_else(|| {
                    // New inferred proofs only select a family at an admitted
                    // site. None can encode a previous overflow/domain failure;
                    // operand types alone must not reactivate that speculation.
                    profile(bcp).and_then(|observed| {
                        let inferred = profiles_numeric_operands(sym)
                            .then(|| constraints.inferred_for(f, &data.args))
                            .flatten().map(|facts| facts as &dyn OperandFacts);
                        if conflicting_operand_facts(f, inferred, &data.args) { return None; }
                        let proven = options.double_float && cfg!(target_arch = "x86_64")
                            && (arith_of(sym).is_some() || comparison)
                            && statically_proven_double(f, &data.args, &consts, comparison, inferred);
                        proven.then_some(SpecType::DoubleFloat)
                            .or_else(|| statically_proven_spec_type(f, &data.args, inferred))
                            .or_else(|| {
                                if !accepts_return_hints(sym, argc) { return None; }
                                let hinted = hinted_family(f, &data.args, inferred, options)?;
                                // Reconcile using ACTUAL facts, never hints as
                                // proof. Expansion retains every operand check.
                                reconcile_double(f, &data.args, &consts, comparison, hinted, options, inferred)
                                    .and_then(|spec| reconcile_with_operands(f, &data.args, spec, inferred))
                                    .filter(|&spec| hints_permit_family(f, &data.args, inferred, spec, comparison))
                            })
                            .or_else(|| reconcile_double(f, &data.args, &consts, comparison, observed, options, inferred)
                                .and_then(|spec| reconcile_with_operands(f, &data.args, spec, inferred))
                                .filter(|&spec| hints_permit_family(f, &data.args, inferred, spec, comparison)))
                    })
                })
            else {
                continue;
            };
            if profiles_numeric_operands(sym) && constraints.excludes_numeric(f, &data.args) {
                continue;
            }
            if spec == SpecType::DoubleFloat {
                // Only binary arithmetic and comparisons have a raw double form.
                let opcode = match (arith_of(sym), cmp_of(sym)) {
                    (Some(arith), _) if argc == 2 => typed_opcode(arith, spec),
                    (_, Some(cmp)) if argc == 2 => match typed_cmp_opcode(cmp, spec) {
                        Some(op) => op,
                        None => continue,
                    },
                    _ => continue,
                };
                let ty = if comparison { boolean_type() } else { result_type(spec) };
                work.push((inst, opcode, ty, ExtraArg::DoubleFloat));
                continue;
            }
            if let Some(arith) = arith_of(sym) {
                match (arith, argc) {
                    // Binary arithmetic: the typed op's result is the numeric type.
                    (_, 2) => work.push((
                        inst,
                        typed_opcode(arith, spec),
                        result_type(spec),
                        ExtraArg::None,
                    )),
                    // Unary minus → negation (fixnum only; no FloatNeg opcode yet).
                    (Arith::Sub, 1) if spec == SpecType::Fixnum => {
                        work.push((inst, Opcode::FixnumNeg, result_type(spec), ExtraArg::None))
                    }
                    // 1-arg +/* are identity; variadic (>2) forms: leave generic.
                    _ => {}
                }
            } else if let Some(arith) = incdec_of(sym) {
                // 1+/1- : binary typed op with a materialised constant 1.
                if spec == SpecType::Fixnum && argc == 1 {
                    work.push((
                        inst,
                        typed_opcode(arith, SpecType::Fixnum),
                        result_type(SpecType::Fixnum),
                        ExtraArg::FixnumOne,
                    ));
                }
            } else if let Some(cmp) = cmp_of(sym) {
                // Comparison: guarded typed compare; the result is a boolean
                // (T/NIL) — both immediates, typed as SYMBOL|NULL.
                if argc == 2 {
                    if let Some(op) = typed_cmp_opcode(cmp, spec) {
                        work.push((inst, op, boolean_type(), ExtraArg::None));
                    }
                }
            } else if let Some(bit) = bitwise_of(sym) {
                // Bitwise ops are fixnum-only. Not is unary; And/Or/Xor are binary.
                let want = if matches!(bit, Bitwise::Not) { 1 } else { 2 };
                if spec == SpecType::Fixnum && argc == want {
                    work.push((
                        inst,
                        bitwise_opcode(bit),
                        result_type(SpecType::Fixnum),
                        ExtraArg::None,
                    ));
                }
            } else if spec == SpecType::Fixnum && argc == 2
                && matches!(egcl_rt::symbols::registry_key(sym).as_deref(),
                    Some("MIN" | "MAX" | "MOD"
                        | "REM" | "FLOOR" | "TRUNCATE"))
            {
                let name = egcl_rt::symbols::symbol_name(sym).unwrap();
                if name == "MOD" && let Some(mask) = mod_mask_of(f, &data.args, &consts) {
                    work.push((inst, Opcode::LogAnd, result_type(spec),
                        ExtraArg::ReplaceSecondWithFixnum(mask)));
                } else if cfg!(target_arch = "x86_64") {
                    let opcode = match name.as_str() {
                        "MIN" => Opcode::FixnumMin,
                        "MAX" => Opcode::FixnumMax,
                        "MOD" => Opcode::FixnumMod,
                        "REM" => Opcode::FixnumRem,
                        // The quotient-only form is valid only when all normal
                        // successor paths discard the secondary remainder.
                        "FLOOR" if discarded_values.contains(&inst) => Opcode::FixnumFloor,
                        "TRUNCATE" if discarded_values.contains(&inst) => Opcode::FixnumDiv,
                        _ => continue,
                    };
                    work.push((inst, opcode, result_type(spec), ExtraArg::None));
                }
            } else if egcl_rt::symbols::symbol_name(sym).as_deref() == Some("ASH") {
                // Arithmetic shift by a (constant, checked at emit) amount: left is a
                // multiply by 2^n (overflow-checked), right is an untag/sar/retag.
                if spec == SpecType::Fixnum && argc == 2 {
                    work.push((
                        inst,
                        Opcode::FixnumShl,
                        result_type(SpecType::Fixnum),
                        ExtraArg::None,
                    ));
                }
            }
        }
    }

    // Mutation phase: turn each into a guarded typed op.
    let mut initially_reachable = None;
    drop(constraints);
    let mut n = 0;
    let mut fixnum_sites: Vec<Inst> = Vec::new();
    for (inst, opcode, ty, extra) in work {
        let AuxData::CallTarget(symbol) = f.inst(inst).aux else { continue };
        let Some(binding) = super::builtin_binding::snapshot(symbol) else { continue };
        let Some(state) = f.inst(inst).frame_state else { continue };
        let (block, position) = f.block_order().iter().find_map(|&block| {
            f.block(block).insts.iter().position(|&candidate| candidate == inst)
                .map(|position| (block, position))
        }).expect("speculation site belongs to a block");
        let normal = if f.inst(inst).opcode == Opcode::Invoke {
            // Removing a cold edge must not strand an OSR header or its
            // preheader definitions. Keep the generic call in that case.
            let before = reachable_blocks(f, None, false);
            let after = reachable_blocks(f, Some(inst), false);
            if !before[block.index()] || f.osr_entries.iter().any(|entry| {
                before[entry.block.index()] && !after[entry.block.index()]
            }) {
                continue;
            }
            initially_reachable.get_or_insert_with(|| reachable_blocks(f, None, true));
            Some(f.inst(inst).targets[0].clone())
        } else {
            None
        };
        super::builtin_binding::insert(f, block, position, state, binding);
        n += 1;
        if extra == ExtraArg::DoubleFloat {
            expand_double_site(f, inst, opcode, &consts);
        }
        // Materialise the appended constant operand (1+/1- → const 1; NOT →
        // const NIL) as a const inst placed immediately before the site.
        let extra_value = match extra {
            ExtraArg::None | ExtraArg::Fence(_) | ExtraArg::DoubleFloat => None,
            ExtraArg::FixnumOne => Some(push_const_before(
                f,
                inst,
                Opcode::ConstFixnum,
                AuxData::FixnumImm(1),
                IRType::of(TypeBits::FIXNUM),
            )),
            ExtraArg::Nil => Some(push_const_before(
                f,
                inst,
                Opcode::ConstNil,
                AuxData::None,
                IRType::of(TypeBits::NULL),
            )),
            ExtraArg::ReplaceSecondWithFixnum(k) => Some(push_const_before(
                f,
                inst,
                Opcode::ConstFixnum,
                AuxData::FixnumImm(k),
                IRType::of(TypeBits::FIXNUM),
            )),
        };
        let replace_second = matches!(extra, ExtraArg::ReplaceSecondWithFixnum(_));
        let results = f.inst(inst).results.clone();
        {
            let data = f.inst_mut(inst);
            data.opcode = opcode;
            data.targets.clear();
            if let Some(v) = extra_value {
                if replace_second {
                    data.args[1] = v;
                } else {
                    data.args.push(v);
                }
            }
            if extra == ExtraArg::DoubleFloat && !is_float_compare(opcode) {
                // The site itself is now the escape box of the raw result: the
                // operand guards and raw operation precede it. It cannot deopt;
                // it allocates, so it stays ordered and pinned.
                data.opcode = Opcode::BoxFloat;
                data.aux = AuxData::None;
                data.flags = InstFlags {
                    effectful: true,
                    ..InstFlags::default()
                };
                data.frame_state = None;
            } else if opcode == Opcode::GenericEq {
                // (eq x nil) is PURE and total: no guard, no deopt state, no
                // safepoint. The separate return effect retains its own state.
                data.flags = InstFlags::default();
                data.frame_state = None;
            } else {
                // No longer a generic call/safepoint — now a deopt point:
                // guard-flagged and ordered (effectful), keeping its FrameState
                // so a wrong type or overflow resumes the interpreter.
                // `aux`/`frame_state` are retained.
                data.flags = InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                };
            }
            if let ExtraArg::Fence(kind) = extra {
                data.args.clear();
                data.aux = AuxData::MemoryFence(kind);
                data.flags.guard = false;
                data.frame_state = None;
            }
        }
        for r in results {
            f.refine_type(r, ty);
        }
        // A successful builtin call replaces the thread's multiple values
        // even when its pure result is folded or discarded. Keep this after
        // binding/type/overflow guards so a miss preserves fallback values.
        // Give it private state: operand-guard rewriting may narrow values
        // here, but the operation's own guard must retain pre-guard operands.
        let return_state = f.frame_states.add(f.frame_states.get(state).clone());
        let (clear, _) = f.push_inst(
            block,
            crate::t2::ir::InstData {
                opcode: Opcode::ClearMv,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: Some(return_state),
                source_pos: f.inst(inst).source_pos,
            },
            &[],
        );
        let insts = &mut f.block_mut(block).insts;
        let appended = insts.pop();
        debug_assert_eq!(appended, Some(clear));
        let position = insts.iter().position(|&candidate| candidate == inst)
            .expect("speculation site remains in its block");
        insts.insert(position + 1, clear);
        if let Some(normal) = normal {
            f.set_terminator(block, crate::t2::ir::InstData {
                opcode: Opcode::Jump,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![normal],
                frame_state: None,
                source_pos: 0,
            });
        }
        if fixnum_family(opcode) {
            fixnum_sites.push(inst);
        }
    }
    if let Some(initially_reachable) = initially_reachable {
        let reachable = reachable_blocks(f, None, true);
        // Stable block IDs keep surviving edge and OSR references intact.
        // Only retire paths made unreachable by these successful rewrites.
        for index in 0..f.num_blocks() {
            if initially_reachable[index] && !reachable[index] {
                let block = crate::t2::ir::Block(index as u32);
                fixnum_sites.retain(|site| !f.block(block).insts.contains(site));
                f.block_mut(block).insts.clear();
                f.set_terminator(block, crate::t2::ir::InstData {
                    opcode: Opcode::Trap,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: None,
                    source_pos: 0,
                });
            }
        }
    }
    if options.double_float && cfg!(target_arch = "x86_64") {
        n += speculate_double_only_sites(f, profile, &consts);
    }
    let self_sym = f
        .entry_frame_state
        .map(|id| f.frame_states.get(id).scopes[0].function);
    materialize_entry_param_preguards(f, &fixnum_sites, self_sym);
    if selected.is_none() { materialize_self_call_result_guards(f, self_sym); }
    materialize_fixnum_operand_guards(f, &fixnum_sites);
    n
}

// Model a selected Invoke's successful edge without mutating the graph.
fn reachable_blocks(f: &Function, normal_only: Option<Inst>, include_osr: bool) -> Vec<bool> {
    let mut seen = vec![false; f.num_blocks()];
    let mut pending = vec![f.entry()];
    if include_osr {
        pending.extend(f.osr_entries.iter().map(|entry| entry.block));
    }
    while let Some(block) = pending.pop() {
        if std::mem::replace(&mut seen[block.index()], true) {
            continue;
        }
        if let Some(term) = f.terminator(block) {
            let targets = &f.inst(term).targets;
            let count = if Some(term) == normal_only { 1 } else { targets.len() };
            pending.extend(targets[..count].iter().map(|edge| edge.block));
        }
    }
    seen
}

/// bliss-x5y.25 (tak shape): pre-guard entry parameters as fixnums AT FUNCTION
/// ENTRY, so a parameter that only later flows into a speculated site (tak's
/// `z`, first touched by `(1- z)` after two recursive calls) is proven before
/// the FIRST safepoint instead of staying a GC root across every call.
///
/// Deopt semantics: the guard borrows the builder's entry FrameState (bcp 0,
/// empty stack) — a failure re-runs the whole function in T0, which is always
/// correct because nothing has executed yet.
///
/// Perma-deopt safety: a parameter is pre-guarded only when at least one use
/// is a speculated fixnum site (runtime evidence of fixnum-ness) and every
/// other use is NEUTRAL — a self-call argument, a return, an edge argument,
/// or a guard. Branch conditions and equality tests are not neutral: they can
/// test a nonnumeric sentinel before entering the numeric path, as in
/// `(if x (+ x 1) 0)`. Any such use (or a generic call or heap accessor) is a
/// path where a non-fixnum argument is legitimate,
/// and an entry pre-guard would turn that call pattern into a deopt storm.
fn materialize_entry_param_preguards(
    f: &mut Function,
    fixnum_sites: &[Inst],
    self_sym: Option<u32>,
) {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let Some(entry_fs) = f.entry_frame_state else {
        return;
    };
    if fixnum_sites.is_empty() {
        return;
    }
    let entry = f.entry();
    let params: Vec<Value> = f.block(entry).params.clone();
    let dom = f.dominators();
    let mut insert_at = 0usize;
    for p in params {
        let bits = f.value(p).ty.bits;
        if !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits {
            continue; // already proven (a declared parameter)
        }
        // The parameter's FLOW SET: itself plus every block parameter it
        // reaches through an edge argument, transitively. After self-tail-call
        // elimination the body is a loop whose header phis carry the
        // parameters, so the entry parameter's only use is the edge into that
        // header and every real use -- the fixnum sites, the self-calls, the
        // return -- is of the phi. Judging the entry parameter by its own
        // uses alone found no evidence, left tak's z unguarded, and with it
        // the self-call results (the base case returns z), which kept both as
        // GC roots across every self-call and off the direct self-call path
        // (bliss-legi4).
        let mut flow: Vec<Value> = vec![p];
        loop {
            let mut grew = false;
            for b in f.block_order().to_vec() {
                let Some(term) = f.terminator(b) else {
                    continue;
                };
                for target in f.inst(term).targets.clone() {
                    for (index, arg) in target.args.iter().enumerate() {
                        if !flow.contains(arg) {
                            continue;
                        }
                        if let Some(&phi) = f.block(target.block).params.get(index)
                            && !flow.contains(&phi)
                        {
                            flow.push(phi);
                            grew = true;
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        let mut evidence = false;
        let mut disqualified = false;
        for b in f.block_order().to_vec() {
            for &inst in &f.block(b).insts.clone() {
                let d = f.inst(inst);
                if d.args.iter().any(|a| flow.contains(a)) {
                    if fixnum_sites.contains(&inst) {
                        evidence = true;
                    } else {
                        let neutral = match d.opcode {
                            Opcode::Call => {
                                matches!(d.aux, AuxData::CallTarget(s) if Some(s) == self_sym)
                            }
                            Opcode::Return | Opcode::Jump | Opcode::Guard => true,
                            _ => false,
                        };
                        if !neutral {
                            disqualified = true;
                        }
                    }
                }
                // Edge arguments (block-param hand-off) are always neutral.
            }
        }
        if !evidence || disqualified {
            continue;
        }
        let gfs = f.frame_states.add(f.frame_states.get(entry_fs).clone());
        let (guard, results) = f.push_inst(
            entry,
            InstData {
                opcode: Opcode::Guard,
                args: vec![p],
                results: vec![],
                aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: Some(gfs),
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let narrowed = results[0];
        let insts = &mut f.block_mut(entry).insts;
        let appended = insts.pop();
        debug_assert_eq!(appended, Some(guard));
        insts.insert(insert_at, guard);
        rewrite_dominated_uses(f, &dom, entry, guard, guard, p, narrowed, false);
        insert_at += 1;
    }
}

/// bliss-x5y.25 (tak shape): speculate SELF-call results as fixnums. Tak's
/// inner recursive results feed the outer call's ARGUMENTS — not arithmetic —
/// so no operand guard ever materialises for them and they stay GC roots
/// across their sibling calls. When the function's every return value is
/// provably a fixnum or another self-call result (so the speculation is
/// self-consistent), guard each self-call's result right after the call, at
/// the next FrameState-carrying instruction (the post-call interpreter state).
fn materialize_self_call_result_guards(f: &mut Function, self_sym: Option<u32>) {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let Some(self_sym) = self_sym else {
        return;
    };
    if !returns_are_fixnum_or_self_call(f, self_sym) {
        return;
    }
    let dom = f.dominators();
    let mut work: Vec<(crate::t2::ir::Block, Inst, Value)> = Vec::new();
    for b in f.block_order().to_vec() {
        for &inst in &f.block(b).insts.clone() {
            let d = f.inst(inst);
            if d.opcode != Opcode::Call || !matches!(d.aux, AuxData::CallTarget(s) if s == self_sym)
            {
                continue;
            }
            let Some(&r) = d.results.first() else {
                continue;
            };
            let bits = f.value(r).ty.bits;
            if !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits {
                continue;
            }
            work.push((b, inst, r));
        }
    }
    for (b, call, r) in work {
        // Anchor: the first FrameState-carrying instruction after the call in
        // the same block; its state is the post-call interpreter state.
        let anchor = {
            let insts = &f.block(b).insts;
            let Some(pos) = insts.iter().position(|&i| i == call) else {
                continue;
            };
            insts[pos + 1..]
                .iter()
                .copied()
                .find(|&i| f.inst(i).frame_state.is_some() && !f.inst(i).results.contains(&r))
        };
        let Some(anchor) = anchor else {
            // No later FrameState carrier in the block: after self-tail-call
            // elimination the LAST inner call's result feeds the back-edge
            // Jump directly (tak's third call), and a Jump carries no state.
            // The resume point is the interpreter's post-call state, which no
            // instruction describes, so synthesise it from the call's own
            // pre-call state: same locals, the raw result on the operand stack
            // in place of the arguments, at the bytecode after the call (the
            // StoreLocal that pops it). A deopt there re-runs nothing. Without
            // this guard the raw result stays TOP, so the loop-header phi it
            // feeds stays TOP, which keeps the parameter and the result as GC
            // roots across every self-call and off the direct self-call path
            // (bliss-legi4).
            let Some(call_fs) = f.inst(call).frame_state else {
                continue;
            };
            let mut state = f.frame_states.get(call_fs).clone();
            if state.scopes.len() != 1 {
                continue;
            }
            let scope = &mut state.scopes[0];
            let Some(pending) = scope.stack.len().checked_sub(f.inst(call).args.len()) else {
                continue;
            };
            scope.bcp += 1;
            scope.stack.truncate(pending);
            scope
                .stack
                .push(crate::t2::frame_state::ValueSource::Value {
                    value: r,
                    repr: ValueRepresentation::Tagged,
                });
            let gfs = f.frame_states.add(state);
            let source_pos = f.inst(call).source_pos;
            let (guard, results) = f.push_inst(
                b,
                InstData {
                    opcode: Opcode::Guard,
                    args: vec![r],
                    results: vec![],
                    aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(gfs),
                    source_pos,
                },
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            );
            let narrowed = results[0];
            let insts = &mut f.block_mut(b).insts;
            let appended = insts.pop();
            debug_assert_eq!(appended, Some(guard));
            let call_pos = insts
                .iter()
                .position(|&i| i == call)
                .expect("call is in its block");
            insts.insert(call_pos + 1, guard);
            rewrite_dominated_uses(f, &dom, b, call, guard, r, narrowed, false);
            continue;
        };
        // A CALL anchor (tak: the next FrameState carrier after the third
        // inner call is the OUTER call) takes the guard BEFORE it: the call's
        // pre-state (result on stack) is cloned for the guard — a failure
        // re-runs that call in T0 — and the call's own args and FrameState
        // are rewritten to the proven value, so the raw result dies at the
        // guard instead of staying a root through the call's safepoint. Any
        // other anchor keeps the eager after-placement.
        let anchor_is_call = f.inst(anchor).opcode == Opcode::Call;
        let anchor_fs = f.inst(anchor).frame_state;
        let gfs = anchor_fs.map(|id| f.frame_states.add(f.frame_states.get(id).clone()));
        let source_pos = f.inst(call).source_pos;
        let (guard, results) = f.push_inst(
            b,
            InstData {
                opcode: Opcode::Guard,
                args: vec![r],
                results: vec![],
                aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                flags: InstFlags {
                    guard: true,
                    effectful: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: gfs,
                source_pos,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let narrowed = results[0];
        let insts = &mut f.block_mut(b).insts;
        let appended = insts.pop();
        debug_assert_eq!(appended, Some(guard));
        let anchor_pos = insts
            .iter()
            .position(|&i| i == anchor)
            .expect("anchor is in its block");
        insts.insert(
            if anchor_is_call {
                anchor_pos
            } else {
                anchor_pos + 1
            },
            guard,
        );
        rewrite_dominated_uses(f, &dom, b, anchor, guard, r, narrowed, anchor_is_call);
    }
}

/// True when every value the function returns is provably a fixnum or the
/// result of a self-call (following block-parameter phis) — the closure
/// condition making self-call result speculation self-consistent.
fn returns_are_fixnum_or_self_call(f: &Function, self_sym: u32) -> bool {
    use crate::t2::ir::ValueDef;
    use std::collections::HashSet;
    fn value_ok(f: &Function, v: Value, self_sym: u32, visiting: &mut HashSet<Value>) -> bool {
        let bits = f.value(v).ty.bits;
        if !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits {
            return true;
        }
        match f.value(v).def {
            ValueDef::Result { inst, .. } => {
                let d = f.inst(inst);
                d.opcode == Opcode::Call && matches!(d.aux, AuxData::CallTarget(s) if s == self_sym)
            }
            ValueDef::Param { block, num } => {
                if !visiting.insert(v) {
                    return true; // loop phi: consistent under the assumption
                }
                // Every incoming edge argument for this parameter must be ok.
                for b in f.block_order() {
                    let Some(term) = f.terminator(*b) else {
                        continue;
                    };
                    for t in &f.inst(term).targets {
                        if t.block == block {
                            let Some(&arg) = t.args.get(num as usize) else {
                                return false;
                            };
                            if !value_ok(f, arg, self_sym, visiting) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
        }
    }
    let mut any_return = false;
    for b in f.block_order() {
        let Some(term) = f.terminator(*b) else {
            continue;
        };
        let d = f.inst(term);
        if d.opcode != Opcode::Return {
            continue;
        }
        any_return = true;
        for &v in &d.args {
            let mut visiting = HashSet::new();
            if !value_ok(f, v, self_sym, &mut visiting) {
                return false;
            }
        }
    }
    any_return
}

/// Create a const-producing instruction and position it immediately before
/// `site` in its block; returns the const's result value.
fn push_const_before(
    f: &mut Function,
    site: Inst,
    opcode: Opcode,
    aux: AuxData,
    ty: IRType,
) -> Value {
    use crate::t2::ir::{InstData, ValueRepresentation};
    let (block, _) = f
        .block_order()
        .to_vec()
        .into_iter()
        .find_map(|b| {
            f.block(b)
                .insts
                .iter()
                .position(|&i| i == site)
                .map(|p| (b, p))
        })
        .expect("site is in a block");
    let (inst, results) = f.push_inst(
        block,
        InstData {
            opcode,
            args: vec![],
            results: vec![],
            aux,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: f.inst(site).source_pos,
        },
        &[(ty, ValueRepresentation::Tagged)],
    );
    let insts = &mut f.block_mut(block).insts;
    let appended = insts.pop();
    debug_assert_eq!(appended, Some(inst));
    let site_pos = insts
        .iter()
        .position(|&i| i == site)
        .expect("site is in its block");
    insts.insert(site_pos, inst);
    results[0]
}

/// The speculated opcodes whose emitter guards every variable operand as a
/// fixnum before executing (emit_arith_inst / emit_fixnum_cmp_value).
fn fixnum_family(op: Opcode) -> bool {
    use Opcode::*;
    matches!(
        op,
        FixnumAdd
            | FixnumSub
            | FixnumMul
            | FixnumDiv
            | FixnumFloor
            | FixnumRem
            | FixnumMod
            | FixnumMin
            | FixnumMax
            | FixnumNeg
            | FixnumShl
            | LogAnd
            | LogOr
            | LogXor
            | LogNot
            | FixnumCmpLt
            | FixnumCmpGt
            | FixnumCmpLe
            | FixnumCmpGe
            | FixnumCmpEq
    )
}

/// bliss-x5y.25 (option b): materialise each speculated fixnum site's
/// discharged operand proof as an explicit SSA `Guard` value — the same shape
/// build.rs gives CAR/CDR's cons proof — and rewrite every use the guard
/// dominates (instruction arguments, edge arguments, and FrameState sources)
/// to the narrowed value.
///
/// Why: inference assigns each SSA value one type from its definition on, so a
/// raw parameter stays TOP everywhere even though the emitter guards it before
/// the op. With the proof reified as a FIXNUM-typed value that replaces the
/// original downstream, the original dies at the guard and what stays live
/// across later call safepoints is provably immediate — the emitter's
/// safepoint-root filter drops it from GC shadow sync, and a function whose
/// roots all vanish loses its frame_base_home, unlocking the direct self-call
/// fast path (emit_call).
///
/// Soundness of the rewrite: the guard passes its input through unchanged, so
/// substituting its result anywhere it dominates preserves values exactly. The
/// guard reuses the site's FrameState (deopt before the op re-runs the op's
/// bytecode in T0 — nothing has executed yet). The site's own FrameState is
/// NOT rewritten: at a guard/op deopt the guard's result does not exist, so
/// those sources must stay the original value.
fn materialize_fixnum_operand_guards(f: &mut Function, sites: &[Inst]) {
    use crate::t2::ir::{InstData, InstFlags, ValueRepresentation};
    if sites.is_empty() {
        return;
    }
    let dom = f.dominators();
    for &site in sites {
        // Locate the site (positions shift as guards are inserted, so look it
        // up fresh each round; T2 bodies are small).
        let Some((block, _)) = f.block_order().to_vec().into_iter().find_map(|b| {
            f.block(b)
                .insts
                .iter()
                .position(|&i| i == site)
                .map(|p| (b, p))
        }) else {
            continue;
        };
        let fs = f.inst(site).frame_state;
        let source_pos = f.inst(site).source_pos;
        let args = f.inst(site).args.clone();
        let mut seen: Vec<Value> = Vec::new();
        for v in args {
            if seen.contains(&v) {
                continue;
            }
            seen.push(v);
            // Already provably fixnum (constant, arith result, earlier guard):
            // the emitter will not re-check it and there is nothing to narrow.
            let bits = f.value(v).ty.bits;
            if !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits {
                continue;
            }
            // EAGER placement for a call result (the Phase-3 half of option b):
            // a result consumed by a speculated fixnum op would normally be
            // guarded just before that op — too late to help any call
            // safepoint in between (fib's r1 stays a GC root across the second
            // recursive call). Instead anchor the guard right after the
            // defining call, at the first following FrameState-carrying
            // instruction in the same block: that state is exactly the
            // post-call interpreter state (the result on the stack), so a
            // guard failure resumes T0 there and simply re-runs the remainder
            // generically. Falls back to just-before-the-site when no such
            // anchor exists.
            let eager = match f.value(v).def {
                crate::t2::ir::ValueDef::Result { inst: def, .. }
                    if f.inst(def).opcode == Opcode::Call =>
                {
                    f.block_order().to_vec().into_iter().find_map(|db| {
                        let insts = &f.block(db).insts;
                        let dp = insts.iter().position(|&i| i == def)?;
                        insts[dp + 1..]
                            .iter()
                            .take_while(|&&i| i != site)
                            .find(|&&i| f.inst(i).frame_state.is_some())
                            .map(|&anchor| (db, anchor))
                    })
                }
                _ => None,
            };
            let (guard_block, anchor, borrowed_fs, rewrite_anchor_args) = match eager {
                Some((db, anchor)) => (db, anchor, f.inst(anchor).frame_state, false),
                None => (block, site, fs, true),
            };
            // The guard gets its own CLONE of the borrowed FrameState. Sharing
            // the id would let a later sibling guard's downstream rewrite
            // reach back into this guard's deopt state and reference a value
            // defined after it (regalloc2 EntryLivein); a private copy pins
            // the state as it exists at the guard.
            let guard_fs = borrowed_fs.map(|id| f.frame_states.add(f.frame_states.get(id).clone()));
            let (guard, results) = f.push_inst(
                guard_block,
                InstData {
                    opcode: Opcode::Guard,
                    args: vec![v],
                    results: vec![],
                    aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                    flags: InstFlags {
                        guard: true,
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: guard_fs,
                    source_pos,
                },
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            );
            let narrowed = results[0];
            // push_inst appended the guard at the end of the block (after the
            // terminator); move it next to its anchor: immediately BEFORE a
            // site anchor (the proof precedes the op), immediately AFTER an
            // eager anchor (whose FrameState the guard borrows).
            let insts = &mut f.block_mut(guard_block).insts;
            let appended = insts.pop();
            debug_assert_eq!(appended, Some(guard));
            let anchor_pos = insts
                .iter()
                .position(|&i| i == anchor)
                .expect("anchor is in its block");
            insts.insert(
                if rewrite_anchor_args {
                    anchor_pos
                } else {
                    anchor_pos + 1
                },
                guard,
            );
            rewrite_dominated_uses(
                f,
                &dom,
                guard_block,
                anchor,
                guard,
                v,
                narrowed,
                rewrite_anchor_args,
            );
        }
    }
}

/// Replace uses of `from` with `to` in everything the guard placed at `anchor`
/// dominates: every instruction after the anchor in `block` (arguments, edge
/// arguments, FrameState sources) and every instruction of every block
/// strictly dominated by `block`. With `rewrite_anchor_args` (the guard sits
/// immediately BEFORE the anchor — the speculated-site placement) the anchor's
/// own arguments are rewritten too, while its FrameState keeps `from`: at a
/// deopt of the guard or the site the guard's result does not exist yet. An
/// eager anchor (guard AFTER it, borrowing its FrameState) is skipped whole
/// for the same reason.
#[allow(clippy::too_many_arguments)]
fn rewrite_dominated_uses(
    f: &mut Function,
    dom: &crate::t2::ir::DominatorTree,
    block: crate::t2::ir::Block,
    site: Inst,
    guard: Inst,
    from: Value,
    to: Value,
    rewrite_anchor_args: bool,
) {
    let rewrite_fs_id = |f: &mut Function, fsid: crate::t2::frame_state::FrameStateId| {
        let fs = f.frame_states.get_mut(fsid);
        for scope in &mut fs.scopes {
            for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                if let crate::t2::frame_state::ValueSource::Value { value, .. } = src {
                    if *value == from {
                        *value = to;
                    }
                }
            }
        }
    };
    let rewrite_frame_state = |f: &mut Function, inst: Inst| {
        let Some(fsid) = f.inst(inst).frame_state else {
            return;
        };
        rewrite_fs_id(f, fsid);
    };
    let rewrite_inst = |f: &mut Function, inst: Inst, with_frame_state: bool| {
        let data = f.inst_mut(inst);
        for a in data.args.iter_mut() {
            if *a == from {
                *a = to;
            }
        }
        for call in data.targets.iter_mut() {
            for a in call.args.iter_mut() {
                if *a == from {
                    *a = to;
                }
            }
        }
        if with_frame_state {
            rewrite_frame_state(f, inst);
        }
    };
    // The anchor itself, for a site anchor (guard placed immediately before
    // it): arguments AND FrameState. The guard's own deopt state is a private
    // CLONE holding the raw value, and at the anchor's deopt the guard has
    // already executed, so its result exists — rewriting the anchor's state
    // is what lets the raw value die at the guard instead of staying a GC
    // root through the anchor's safepoint (bliss-x5y.25 tak outer call).
    if rewrite_anchor_args {
        rewrite_inst(f, site, true);
    }
    // The rest of the site's block, strictly after the site.
    let insts = f.block(block).insts.clone();
    let site_pos = insts
        .iter()
        .position(|&i| i == site)
        .expect("site is in its block");
    for &inst in &insts[site_pos + 1..] {
        // The guard itself consumes `from` — never rewrite it to its own result.
        if inst == guard {
            continue;
        }
        rewrite_inst(f, inst, true);
    }
    // Every block strictly dominated by the site's block.
    for b in f.block_order().to_vec() {
        if b != block && dom.dominates(block, b) {
            for inst in f.block(b).insts.clone() {
                rewrite_inst(f, inst, true);
            }
        }
    }
    // OSR entry import maps (Function::osr_entries) record which value holds
    // each interpreter slot AT a loop header, and are attached to no
    // instruction — sweep them explicitly. A header strictly dominated by the
    // guard must import the NARROWED value: the loop body reads it, and the
    // stale original may be dead there, so the OSR stub would initialise a
    // home the loop never reads and skip the one it does (bliss-x5y.29:
    // live-osr's n imported into pre-guard rcx while the loop read the guard
    // value's stack home).
    let osr_states: Vec<crate::t2::frame_state::FrameStateId> = f
        .osr_entries
        .iter()
        .filter(|o| o.block != block && dom.dominates(block, o.block))
        .map(|o| o.frame_state)
        .collect();
    for fsid in osr_states {
        rewrite_fs_id(f, fsid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::ValueRepresentation;

    // Build `(* a b)`: two params, a Call to `*` carrying a FrameState at bcp 2,
    // then a Return of the result. Returns (function, the call Inst).
    fn build_star() -> (Function, Inst) {
        build_call("*", 2)
    }

    // `(name p0 .. pN-1)` over `argc` TOP parameters, returned.
    fn build_call(name: &str, argc: usize) -> (Function, Inst) {
        let mut f = Function::new("m");
        let entry = f.entry();
        let params: Vec<Value> = (0..argc)
            .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
            .collect();

        let fs = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 2,
                locals: vec![],
                stack: vec![],
            }],
            remat: vec![],
        });
        let star = egcl_rt::symbols::intern(name);
        let (call, results) = f.push_inst(
            entry,
            crate::t2::ir::InstData {
                opcode: Opcode::Call,
                args: params,
                results: vec![],
                aux: AuxData::CallTarget(star),
                flags: InstFlags {
                    call: true,
                    effectful: true,
                    safepoint: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: Some(fs),
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            entry,
            crate::t2::ir::InstData {
                opcode: Opcode::Return,
                args: vec![results[0]],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        (f, call)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn integer_builtins_require_canonical_identity_and_preserve_observed_tuples() {
        use crate::t2::ir::InstData;
        for (name, opcode) in [
            ("MIN", Opcode::FixnumMin),
            ("MAX", Opcode::FixnumMax),
            ("MOD", Opcode::FixnumMod),
            ("REM", Opcode::FixnumRem),
        ] {
            let (mut f, call) = build_call(name, 2);
            assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 1);
            assert_eq!(f.inst(call).opcode, opcode);
        }
        for (name, opcode) in [
            ("FLOOR", Opcode::FixnumFloor),
            ("TRUNCATE", Opcode::FixnumDiv),
        ] {
            let (mut f, call) = build_call(name, 2);
            // The return observes the full tuple: retain the ordinary call.
            assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 0);
            assert_eq!(f.inst(call).opcode, Opcode::Call);
            let block = f.entry();
            let (clear, _) = f.push_inst(
                block,
                InstData {
                    opcode: Opcode::ClearMv,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::None,
                    flags: InstFlags {
                        effectful: true,
                        ..Default::default()
                    },
                    targets: vec![],
                    frame_state: None,
                    source_pos: 0,
                },
                &[],
            );
            let insts = &mut f.block_mut(block).insts;
            assert_eq!(insts.pop(), Some(clear));
            insts.insert(insts.len() - 1, clear);
            assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 1);
            assert_eq!(f.inst(call).opcode, opcode);
        }
        for name in [
            "USER654::MIN",
            "USER654::MAX",
            "USER654::MOD",
            "USER654::REM",
        ] {
            let (mut f, call) = build_call(name, 2);
            assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 0);
            assert_eq!(f.inst(call).opcode, Opcode::Call);
        }
    }

    #[test]
    fn self_call_result_guard_preserves_pending_operands() {
        use crate::t2::frame_state::ValueSource;

        let (mut f, call) = build_star();
        let self_sym = egcl_rt::symbols::intern("*");
        let call_state = f.inst(call).frame_state.unwrap();
        let result = f.inst(call).results[0];
        let args = f.inst(call).args.clone();
        let pending = ValueSource::Const(egcl_rt::value::EgclVal::from_fixnum(37));
        f.frame_states.get_mut(call_state).scopes[0].stack = std::iter::once(pending.clone())
            .chain(args.iter().map(|&value| ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            }))
            .collect();

        materialize_self_call_result_guards(&mut f, Some(self_sym));

        let guard = f
            .block(f.entry())
            .insts
            .iter()
            .copied()
            .find(|&inst| f.inst(inst).opcode == Opcode::Guard && f.inst(inst).args == [result])
            .expect("self-call result must be guarded even without a later frame state");
        let resumed = &f
            .frame_states
            .get(f.inst(guard).frame_state.unwrap())
            .scopes[0];
        assert_eq!(resumed.bcp, 3);
        assert_eq!(
            resumed.stack.len(),
            2,
            "pending operands must survive a post-call deopt"
        );
        assert!(matches!(resumed.stack[0], ValueSource::Const(value)
            if value == egcl_rt::value::EgclVal::from_fixnum(37)));
        assert!(matches!(resumed.stack[1], ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        } if value == result));
    }

    #[test]
    fn fixnum_site_becomes_guarded_fixnum_mul() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|bcp| {
            if bcp == 2 {
                Some(SpecType::Fixnum)
            } else {
                None
            }
        });
        assert_eq!(n, 1);
        let inst = f.inst(call);
        assert_eq!(
            inst.opcode,
            Opcode::FixnumMul,
            "* speculated fixnum → FixnumMul"
        );
        assert!(
            inst.flags.guard,
            "typed op must be guard-flagged (deopt point)"
        );
        assert!(!inst.flags.call, "no longer a generic call");
        assert!(inst.frame_state.is_some(), "keeps the deopt FrameState");
        assert!(f.value(inst.results[0]).ty.bits.contains(TypeBits::FIXNUM));
        // and no second (float) op was emitted — still one instruction.
        assert_eq!(inst.results.len(), 1);
    }

    #[test]
    fn float_site_becomes_float_mul() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|_| Some(SpecType::SingleFloat));
        assert_eq!(n, 1);
        assert_eq!(
            f.inst(call).opcode,
            Opcode::FloatMul,
            "* speculated float → FloatMul"
        );
    }

    #[test]
    fn known_float_operand_overrides_a_fixnum_guess() {
        // `(* x 2.0)` with a cold (fixnum-guessed) profile: the literal can
        // never pass a fixnum guard, so the site speculates single-float.
        let (mut f, call) = build_star();
        let second = f.block(f.entry()).params[1];
        f.refine_type(second, IRType::of(TypeBits::SINGLE_FLOAT));
        let n = speculate(&mut f, &|_| Some(SpecType::Fixnum));
        assert_eq!(n, 1);
        assert_eq!(f.inst(call).opcode, Opcode::FloatMul);
    }

    #[test]
    fn known_non_numeric_operand_keeps_the_site_generic() {
        let (mut f, call) = build_star();
        let second = f.block(f.entry()).params[1];
        f.refine_type(second, IRType::of(TypeBits::DOUBLE_FLOAT));
        for guess in [SpecType::Fixnum, SpecType::SingleFloat] {
            let mut g = f.clone();
            assert_eq!(speculate(&mut g, &|_| Some(guess)), 0, "{guess:?}");
            assert_eq!(g.inst(call).opcode, f.inst(call).opcode);
        }
    }

    #[cfg(target_arch = "x86_64")]
    const DOUBLE_FLOAT: SpeculateOptions = SpeculateOptions { double_float: true };

    /// The opcodes of the entry block, in order.
    #[cfg(target_arch = "x86_64")]
    fn entry_opcodes(f: &Function) -> Vec<Opcode> {
        f.block(f.entry()).insts.iter().map(|&inst| f.inst(inst).opcode).collect()
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn profiled_double_site_expands_to_raw_ir_only_when_admitted() {
        use crate::t2::ir::ValueRepresentation as Repr;
        let (f, call) = build_star();
        // The tagged artifact never sees a double speculation.
        let mut tagged = f.clone();
        assert_eq!(speculate(&mut tagged, &|_| Some(SpecType::DoubleFloat)), 0);
        assert_eq!(tagged.inst(call).opcode, Opcode::Call);

        let mut raw = f.clone();
        assert_eq!(speculate_with(&mut raw, &|_| Some(SpecType::DoubleFloat), DOUBLE_FLOAT), 1);
        let ops = entry_opcodes(&raw);
        let unboxes: Vec<Inst> = raw.block(raw.entry()).insts.iter().copied()
            .filter(|&inst| raw.inst(inst).opcode == Opcode::UnboxFloat)
            .collect();
        assert_eq!(unboxes.len(), 2, "{ops:?}");
        for &unbox in &unboxes {
            let data = raw.inst(unbox);
            assert!(data.flags.guard && data.frame_state.is_some());
            assert!(unbox_accepts_fixnum(&data.aux), "arithmetic takes fixnum contagion");
            assert_eq!(raw.value(data.results[0]).repr, Repr::UnboxedF64);
        }
        // The second of two unproven variables checks the pair for fixnums.
        assert_eq!(raw.inst(unboxes[0]).args.len(), 1);
        assert_eq!(raw.inst(unboxes[1]).args.len(), 2);
        let multiply = *raw.block(raw.entry()).insts.iter()
            .find(|&&inst| raw.inst(inst).opcode == Opcode::FloatMul)
            .expect("raw multiply");
        assert!(raw.inst(multiply).frame_state.is_none());
        assert_eq!(raw.value(raw.inst(multiply).results[0]).repr, Repr::UnboxedF64);
        // The site itself boxes the raw result into its original value.
        assert_eq!(raw.inst(call).opcode, Opcode::BoxFloat);
        assert_eq!(raw.inst(call).args, raw.inst(multiply).results);
        assert_eq!(raw.value(raw.inst(call).results[0]).repr, Repr::Tagged);
        assert_eq!(raw.value(raw.inst(call).results[0]).ty.bits, TypeBits::DOUBLE_FLOAT);
        assert!(crate::t2::verify::verify(&raw).is_ok());
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn declared_double_operands_speculate_without_a_profile() {
        let (mut f, call) = build_star();
        for param in f.block(f.entry()).params.clone() {
            f.refine_type(param, IRType::of(TypeBits::DOUBLE_FLOAT));
        }
        assert_eq!(speculate_with(&mut f, &|_| None, DOUBLE_FLOAT), 1);
        assert_eq!(f.inst(call).opcode, Opcode::BoxFloat);
        // Proven doubles need no fixnum pair check.
        assert!(f.block(f.entry()).insts.iter()
            .filter(|&&inst| f.inst(inst).opcode == Opcode::UnboxFloat)
            .all(|&inst| f.inst(inst).args.len() == 1));
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn known_double_operand_overrides_a_fixnum_guess_when_admitted() {
        let (mut f, call) = build_star();
        let second = f.block(f.entry()).params[1];
        f.refine_type(second, IRType::of(TypeBits::DOUBLE_FLOAT));
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 1);
        assert_eq!(f.inst(call).opcode, Opcode::BoxFloat);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn double_profile_beside_two_fixnums_stays_generic() {
        let (mut f, call) = build_star();
        for param in f.block(f.entry()).params.clone() {
            f.refine_type(param, IRType::of(TypeBits::FIXNUM));
        }
        // Two proven fixnums are integer arithmetic whatever the profile says.
        let mut g = f.clone();
        speculate_with(&mut g, &|_| Some(SpecType::DoubleFloat), DOUBLE_FLOAT);
        assert_ne!(g.inst(call).opcode, Opcode::BoxFloat);
        assert!(!g.block(g.entry()).insts.iter().any(|&inst| g.inst(inst).opcode == Opcode::UnboxFloat));
    }

    #[test]
    fn declared_fixnum_operands_override_an_absent_profile() {
        let (mut f, call) = build_star();
        let entry = f.entry();
        let params = f.block(entry).params.clone();
        for param in params {
            f.refine_type(param, IRType::of(TypeBits::FIXNUM));
        }

        let n = speculate(&mut f, &|_| None);
        assert_eq!(
            n, 1,
            "static parameter proof must not require profile samples"
        );
        assert_eq!(f.inst(call).opcode, Opcode::FixnumMul);
    }

    #[test]
    fn polymorphic_site_is_left_generic() {
        let (mut f, call) = build_star();
        let n = speculate(&mut f, &|_| None); // no consistent type
        assert_eq!(n, 0);
        assert_eq!(
            f.inst(call).opcode,
            Opcode::Call,
            "polymorphic/cold site stays a Call"
        );
    }

    /// The raw operation feeding a rewritten site's escape box.
    #[cfg(target_arch = "x86_64")]
    fn raw_operation(f: &Function, site: Inst) -> &crate::t2::ir::InstData {
        assert_eq!(f.inst(site).opcode, Opcode::BoxFloat);
        let crate::t2::ir::ValueDef::Result { inst, .. } = f.value(f.inst(site).args[0]).def else {
            panic!("the box takes an instruction result");
        };
        f.inst(inst)
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn proven_double_sqrt_becomes_a_guarded_raw_root() {
        let (mut f, call) = build_call("SQRT", 1);
        let x = f.block(f.entry()).params[0];
        f.refine_type(x, IRType::of(TypeBits::DOUBLE_FLOAT));
        // An unprofiled site answers a cold fixnum guess.
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 1);
        let root = raw_operation(&f, call);
        assert_eq!(root.opcode, Opcode::FloatSqrt);
        assert!(root.flags.guard && root.frame_state.is_some(), "a negative operand deopts");
        let crate::t2::ir::ValueDef::Result { inst: unbox, .. } = f.value(root.args[0]).def else {
            panic!("unboxed operand");
        };
        assert_eq!(f.inst(unbox).opcode, Opcode::UnboxFloat);
        assert!(!unbox_accepts_fixnum(&f.inst(unbox).aux), "(sqrt 4) is a single-float");
        assert!(entry_opcodes(&f).contains(&Opcode::ClearMv));
        assert!(crate::t2::verify::verify(&f).is_ok());
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn sqrt_needs_a_double_operand_and_an_undeclined_site() {
        // Unknown operand, cold: stays generic.
        let (mut f, call) = build_call("SQRT", 1);
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 0);
        assert_eq!(f.inst(call).opcode, Opcode::Call);
        // Unknown operand profiled as a double: raw.
        let (mut f, call) = build_call("SQRT", 1);
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::DoubleFloat), DOUBLE_FLOAT), 1);
        assert_eq!(raw_operation(&f, call).opcode, Opcode::FloatSqrt);
        // A declined site (the profile answers None) stays generic.
        let (mut f, call) = build_call("SQRT", 1);
        let x = f.block(f.entry()).params[0];
        f.refine_type(x, IRType::of(TypeBits::DOUBLE_FLOAT));
        assert_eq!(speculate_with(&mut f, &|_| None, DOUBLE_FLOAT), 0);
        assert_eq!(f.inst(call).opcode, Opcode::Call);
        // The tagged artifact never compiles it raw.
        let (mut f, call) = build_call("SQRT", 1);
        let x = f.block(f.entry()).params[0];
        f.refine_type(x, IRType::of(TypeBits::DOUBLE_FLOAT));
        assert_eq!(speculate(&mut f, &|_| Some(SpecType::DoubleFloat)), 0);
        assert_eq!(f.inst(call).opcode, Opcode::Call);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn double_negation_is_an_unguarded_sign_flip() {
        let (mut f, call) = build_call("-", 1);
        let x = f.block(f.entry()).params[0];
        f.refine_type(x, IRType::of(TypeBits::DOUBLE_FLOAT));
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 1);
        let negate = raw_operation(&f, call);
        assert_eq!(negate.opcode, Opcode::FloatNeg);
        assert!(!negate.flags.guard && negate.frame_state.is_none());
        assert!(crate::t2::verify::verify(&f).is_ok());
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn double_division_guards_its_divisor() {
        let (mut f, call) = build_call("/", 2);
        for param in f.block(f.entry()).params.clone() {
            f.refine_type(param, IRType::of(TypeBits::DOUBLE_FLOAT));
        }
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 1);
        let divide = raw_operation(&f, call);
        assert_eq!(divide.opcode, Opcode::FloatDiv);
        assert!(divide.flags.guard && divide.frame_state.is_some(), "a zero divisor deopts");
        assert!(crate::t2::verify::verify(&f).is_ok());
        // A declared fixnum dividend joins a double divisor by contagion.
        let (mut f, call) = build_call("/", 2);
        let params = f.block(f.entry()).params.clone();
        f.refine_type(params[0], IRType::of(TypeBits::FIXNUM));
        f.refine_type(params[1], IRType::of(TypeBits::DOUBLE_FLOAT));
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::Fixnum), DOUBLE_FLOAT), 1);
        assert_eq!(raw_operation(&f, call).opcode, Opcode::FloatDiv);
        // Two fixnums divide rationally, whatever the profile says.
        let (mut f, call) = build_call("/", 2);
        for param in f.block(f.entry()).params.clone() {
            f.refine_type(param, IRType::of(TypeBits::FIXNUM));
        }
        assert_eq!(speculate_with(&mut f, &|_| Some(SpecType::DoubleFloat), DOUBLE_FLOAT), 0);
        assert_eq!(f.inst(call).opcode, Opcode::Call);
    }
    // The producer deliberately declares TOP: only inference knows it returns
    // a Boolean. This distinguishes the negative consumer from the existing
    // declared-type reconciliation in speculation.
    fn call_with_inferred_boolean(name: &str, arity: usize, position: usize) -> (Function, Inst) {
        use crate::t2::frame_state::ValueSource;
        use crate::t2::ir::InstData;
        let (mut f, call) = build_call(name, arity);
        let block = f.entry();
        let input = f.block(block).params[0];
        let (predicate, values) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::TypeCheck,
                args: vec![input],
                results: vec![],
                aux: AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        assert_eq!(f.block_mut(block).insts.pop(), Some(predicate));
        f.block_mut(block).insts.insert(0, predicate);
        let (constant, ones) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(1),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        assert_eq!(f.block_mut(block).insts.pop(), Some(constant));
        f.block_mut(block).insts.insert(1, constant);
        let mut args = vec![ones[0]; arity];
        args[position] = values[0];
        f.inst_mut(call).args = args.clone();
        let state = f.inst(call).frame_state.unwrap();
        f.frame_states.get_mut(state).scopes[0].stack = args
            .into_iter()
            .map(|value| ValueSource::Value {
                value,
                repr: ValueRepresentation::Tagged,
            })
            .collect();
        assert_eq!(f.value(values[0]).ty, IRType::TOP);
        assert_eq!(
            crate::t2::infer::infer(&f).ty(values[0]).bits,
            TypeBits::SYMBOL.join(TypeBits::NULL)
        );
        crate::t2::verify::verify(&f).unwrap();
        (f, call)
    }

    fn assert_boolean_numeric_call_unchanged(
        name: &str,
        arity: usize,
        position: usize,
        profile: SpecType,
        options: SpeculateOptions,
    ) {
        let (mut f, call) = call_with_inferred_boolean(name, arity, position);
        if profile == SpecType::DoubleFloat {
            // Use actual tagged DOUBLE-FLOAT variables beside the Boolean,
            // so another constant/admission rule cannot explain the veto.
            let params = f.block(f.entry()).params.clone();
            for (index, &param) in params.iter().enumerate() {
                if index != position {
                    f.refine_type(param, IRType::of(TypeBits::DOUBLE_FLOAT));
                    f.inst_mut(call).args[index] = param;
                }
            }
            let args = f.inst(call).args.clone();
            let state = f.inst(call).frame_state.unwrap();
            f.frame_states.get_mut(state).scopes[0].stack = args
                .into_iter()
                .map(|value| crate::t2::frame_state::ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                })
                .collect();
            crate::t2::verify::verify(&f).unwrap();
        }
        let original = format!("{:?}", f.inst(call));
        let state = f.inst(call).frame_state.unwrap();
        let original_state = format!("{:?}", f.frame_states.get(state));
        let count = f.num_insts();
        assert_eq!(
            speculate_with(&mut f, &|_| Some(profile), options),
            0,
            "{name}/{arity} Boolean argument {position} must remain generic"
        );
        assert_eq!(f.num_insts(), count, "no split/unbox/guard may be inserted");
        assert_eq!(
            format!("{:?}", f.inst(call)),
            original,
            "opcode, arguments, targets, flags and exact FrameState ID remain intact"
        );
        assert_eq!(
            format!("{:?}", f.frame_states.get(state)),
            original_state,
            "the generic signaling call retains its original evaluated operand stack"
        );
        crate::t2::verify::verify(&f).unwrap();
    }

    #[test]
    fn inferred_boolean_operands_veto_stale_fixnum_calls_in_either_position() {
        for position in 0..2 {
            assert_boolean_numeric_call_unchanged(
                "+",
                2,
                position,
                SpecType::Fixnum,
                SpeculateOptions::default(),
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn inferred_boolean_veto_covers_variadic_and_double_only_entrances() {
        for (name, arity, position) in [
            ("+", 2, 0),
            ("+", 2, 1),
            ("+", 3, 1),
            ("+", 3, 2), // do not specialize a good prefix before seeing a bad tail
            ("SQRT", 1, 0),
            ("-", 1, 0),
            ("/", 2, 0),
            ("/", 2, 1),
        ] {
            assert_boolean_numeric_call_unchanged(
                name,
                arity,
                position,
                SpecType::DoubleFloat,
                DOUBLE_FLOAT,
            );
        }
    }

    #[test]
    fn negative_numeric_veto_does_not_reject_unknown_or_numeric_containing_types() {
        for ty in [
            IRType::TOP,
            IRType::BOTTOM,
            IRType::of(TypeBits::FIXNUM.join(TypeBits::SYMBOL).join(TypeBits::NULL)),
        ] {
            let (mut f, call) = build_call("+", 2);
            for param in f.block(f.entry()).params.clone() {
                f.refine_type(param, ty);
            }
            assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 1, "{ty:?}");
            assert_eq!(f.inst(call).opcode, Opcode::FixnumAdd);
            crate::t2::verify::verify(&f).unwrap();
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn negative_numeric_veto_preserves_single_float_union_and_boolean_operations() {
        let (mut f, call) = build_call("+", 2);
        for param in f.block(f.entry()).params.clone() {
            f.refine_type(
                param,
                IRType::of(TypeBits::FIXNUM.join(TypeBits::SINGLE_FLOAT)),
            );
        }
        assert_eq!(speculate(&mut f, &|_| Some(SpecType::SingleFloat)), 1);
        assert_eq!(f.inst(call).opcode, Opcode::FloatAdd);
        crate::t2::verify::verify(&f).unwrap();
        for name in ["NOT", "NULL"] {
            let (mut f, call) = call_with_inferred_boolean(name, 1, 0);
            assert_eq!(
                speculate_with(&mut f, &|_| Some(SpecType::DoubleFloat), DOUBLE_FLOAT),
                1
            );
            assert_eq!(
                f.inst(call).opcode,
                Opcode::GenericEq,
                "Boolean operations do not enter the numeric veto"
            );
            crate::t2::verify::verify(&f).unwrap();
        }
    }

    #[test]
    fn inferred_boolean_invoke_preserves_exceptional_projection_and_state() {
        use crate::t2::ir::{BlockCall, InstData};
        let (mut f, call) = call_with_inferred_boolean("+", 2, 0);
        let entry = f.entry();
        let returned = f.block_mut(entry).insts.pop().unwrap();
        assert_eq!(f.inst(returned).opcode, Opcode::Return);
        let normal = f.make_block();
        let cold = f.make_block();
        let result = f.add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let produced = f.inst(call).results[0];
        let state = f.inst(call).frame_state.unwrap();
        let invoke = f.inst_mut(call);
        invoke.opcode = Opcode::Invoke;
        invoke.flags.terminator = true;
        invoke.targets = vec![
            BlockCall {
                block: normal,
                args: vec![produced],
            },
            BlockCall {
                block: cold,
                args: vec![],
            },
        ];
        let mut ret = f.inst(returned).clone();
        ret.args = vec![result];
        f.set_terminator(normal, ret);
        let transfer = f.set_terminator(
            cold,
            InstData {
                opcode: Opcode::NlxTransfer,
                args: vec![],
                results: vec![],
                aux: AuxData::TransferSite {
                    origin_bcp: 2,
                    scopes: vec![],
                },
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: Some(state),
                source_pos: 0,
            },
        );
        crate::t2::verify::verify(&f).unwrap();
        let original = format!("{:?}", f.inst(call));
        let original_transfer = format!("{:?}", f.inst(transfer));
        let original_state = format!("{:?}", f.frame_states.get(state));
        let count = f.num_insts();
        assert_eq!(speculate(&mut f, &|_| Some(SpecType::Fixnum)), 0);
        assert_eq!(f.num_insts(), count);
        assert_eq!(
            format!("{:?}", f.inst(call)),
            original,
            "generic Invoke keeps both exact edge projections and all call effects"
        );
        assert_eq!(format!("{:?}", f.inst(transfer)), original_transfer);
        assert_eq!(format!("{:?}", f.frame_states.get(state)), original_state);
        crate::t2::verify::verify(&f).unwrap();
    }

    // An unknown argument selects either literal, then the join is consumed twice.
    // This deliberately exercises the bytecode producer and its TOP-declared phi,
    // rather than manufacturing a narrowed parameter in the test.
    #[cfg(target_arch = "x86_64")]
    fn numeric_join_from_bytecode(left: TypeBits, right: TypeBits) -> (Function, Inst) {
        use egcl_rt::bytecode::{BytecodeFunction, Instr};
        use egcl_rt::value::EgclVal;
        let plus = egcl_rt::symbols::intern("+");
        let owner = egcl_rt::symbols::intern("INFERRED-NUMERIC-JOIN");
        egcl_rt::rooted!(a = if left == TypeBits::DOUBLE_FLOAT {
            egcl_rt::gc::alloc_double_float(1.25)
        } else if left == TypeBits::SINGLE_FLOAT {
            EgclVal::from_single_float(1.25)
        } else {
            EgclVal::from_fixnum(3)
        });
        egcl_rt::rooted!(b = if right == TypeBits::DOUBLE_FLOAT {
            egcl_rt::gc::alloc_double_float(2.5)
        } else if right == TypeBits::SINGLE_FLOAT {
            EgclVal::from_single_float(2.5)
        } else {
            EgclVal::from_fixnum(7)
        });
        let body = BytecodeFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            code: vec![
                Instr::LoadLocal(0),
                Instr::BrIfFalse(4),
                Instr::Const(0),
                Instr::Br(5),
                Instr::Const(1),
                Instr::StoreLocal(1),
                Instr::LoadLocal(1),
                Instr::LoadLocal(1),
                Instr::CallNamed { sym: plus, nargs: 2 },
                Instr::Return,
            ],
            constants: vec![*a, *b],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: 2,
            max_stack: 2,
            arity: 1,
            name: "inferred-numeric-join".into(),
            params_form: egcl_rt::value::NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
            binder_cache: Default::default(),
        };
        // No live heap inspection on the builder side: capture the rooted literals
        // first, exactly as the compilation owner does for a worker.
        let snapshot = crate::t2::constants::BytecodeSnapshot::capture(&body);
        let f = crate::t2::build::build_from_snapshot(
            &snapshot,
            crate::t2::inlining::InlineOptions::default().with_root_symbol(owner),
        ).unwrap();
        crate::t2::verify::verify(&f).unwrap();
        let call = f.block_order().iter()
            .flat_map(|&block| f.block(block).insts.iter().copied())
            .find(|&inst| matches!(f.inst(inst).opcode, Opcode::Call | Opcode::Invoke)
                && matches!(f.inst(inst).aux, AuxData::CallTarget(symbol) if symbol == plus))
            .expect("the builder must retain the generic arithmetic call");
        let facts = crate::t2::infer::infer(&f);
        for &operand in &f.inst(call).args {
            assert_eq!(f.value(operand).ty, IRType::TOP, "join stays undeclared");
            assert!(matches!(f.value(operand).def, crate::t2::ir::ValueDef::Param { .. }),
                "the arithmetic consumes a real forwarding/join parameter");
            assert_eq!(facts.ty(operand).bits, left.join(right));
        }
        (f, call)
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn inferred_numeric_joins_override_stale_profiles_with_the_proven_family() {
        for (family, stale, expected) in [
            (TypeBits::FIXNUM, SpecType::SingleFloat, Opcode::FixnumAdd),
            (TypeBits::SINGLE_FLOAT, SpecType::Fixnum, Opcode::FloatAdd),
            (TypeBits::DOUBLE_FLOAT, SpecType::Fixnum, Opcode::BoxFloat),
        ] {
            let (mut f, call) = numeric_join_from_bytecode(family, family);
            assert_eq!(speculate_with(&mut f, &|_| Some(stale), DOUBLE_FLOAT), 1);
            assert_eq!(f.inst(call).opcode, expected, "inferred family {family:?}");
            if family == TypeBits::DOUBLE_FLOAT {
                assert_eq!(raw_operation(&f, call).opcode, Opcode::FloatAdd);
                assert_eq!(f.value(raw_operation(&f, call).results[0]).repr,
                    ValueRepresentation::UnboxedF64);
            } else {
                assert_eq!(f.value(f.inst(call).results[0]).ty.bits, family);
            }
            crate::t2::verify::verify(&f).unwrap();
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn inferred_numeric_joins_do_not_reenable_a_site_without_feedback() {
        for family in [TypeBits::FIXNUM, TypeBits::SINGLE_FLOAT, TypeBits::DOUBLE_FLOAT] {
            let (mut f, call) = numeric_join_from_bytecode(family, family);
            let before = format!("{:?}", f.inst(call));
            let state = f.inst(call).frame_state.unwrap();
            let before_state = format!("{:?}", f.frame_states.get(state));
            let count = f.num_insts();
            assert_eq!(speculate_with(&mut f, &|_| None, DOUBLE_FLOAT), 0);
            assert_eq!(format!("{:?}", f.inst(call)), before);
            assert_eq!(format!("{:?}", f.frame_states.get(state)), before_state);
            assert_eq!(f.num_insts(), count, "absent/declined feedback is not a new license");
            crate::t2::verify::verify(&f).unwrap();
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn ambiguous_numeric_join_keeps_profile_selection_and_runtime_guards() {
        let (f, call) = numeric_join_from_bytecode(TypeBits::FIXNUM, TypeBits::SINGLE_FLOAT);
        for (profile, expected) in [
            (SpecType::Fixnum, Opcode::FixnumAdd),
            (SpecType::SingleFloat, Opcode::FloatAdd),
        ] {
            let mut g = f.clone();
            assert_eq!(speculate_with(&mut g, &|_| Some(profile), DOUBLE_FLOAT), 1);
            assert_eq!(g.inst(call).opcode, expected);
            assert!(g.inst(call).flags.guard && g.inst(call).frame_state.is_some(),
                "ambiguous joins must retain the typed operation's runtime checks and recovery state");
            crate::t2::verify::verify(&g).unwrap();
        }
    }

}
