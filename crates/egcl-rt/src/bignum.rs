// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Arbitrary-precision integers and rationals: egcl's exact numeric tower.
//!
//! This lives in egcl-rt rather than in the interpreter because BOTH the
//! evaluator and the READER need it. `crates/egcl/src/cli.rs` owned it, but
//! `egcl` depends ON `egcl-compiler`, so the reader could not reach it --
//! which is why a ratio literal whose components exceed i64 fell through and
//! was handed back as a SYMBOL (bliss-0dtf). Reducing such a literal to lowest
//! terms needs bignum GCD and exact division, and this is the only place they
//! exist.
//!
//! It is also where AGENTS.md says it belongs: the interpreter must not be the
//! home of library behaviour.
//!
//! Magnitudes are little-endian base-2^64 with no trailing zero limbs, so the
//! empty magnitude is exactly zero.

use crate::error::EgclError;
use crate::object::{ObjectHeader, RatioData, type_id};
use crate::value::EgclVal;
use std::cmp::Ordering;

/// The 61-bit signed fixnum range (3 tag bits). Outside it an integer must be
/// boxed as a BIGNUM.
pub const FIXNUM_MAX: i64 = (1 << 60) - 1;
pub const FIXNUM_MIN: i64 = -(1 << 60);

/// Allocate a heap object of `total_size` bytes (header included) with the
/// given type id. Mirrors the interpreter's former `gc_alloc_obj`.
fn gc_alloc_obj(total_size: usize, type_id: u8) -> *mut u8 {
    let hdr = std::mem::size_of::<ObjectHeader>();
    let body_size = total_size.saturating_sub(hdr).max(1);
    match crate::gc::alloc_typed(body_size, type_id) {
        // SAFETY: alloc_typed returns a pointer past an 8-byte header.
        Some(body) => unsafe { body.sub(hdr) },
        None => std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(total_size, 8).unwrap(),
        ),
    }
}

/// Allocate a RATIO heap object (numerator/denominator are integers) on the
/// shared GC heap, using the spec RatioData layout (bliss-jtc.5).
pub fn alloc_ratio_cli(num: EgclVal, den: EgclVal) -> EgclVal {
    crate::rooted!(num = num);
    crate::rooted!(den = den);
    let ptr = gc_alloc_obj(std::mem::size_of::<RatioData>(), type_id::RATIO) as *mut RatioData;
    unsafe {
        (*ptr).numerator = *num;
        (*ptr).denominator = *den;
        EgclVal::from_heap_ptr(ptr as *mut u8)
    }
}
/// Allocate a BIGNUM heap object (§1.8.1) on the shared GC heap: header + sign +
/// n_limbs + limbs. Layout mirrors the reader so `print_val` renders it correctly.
pub fn alloc_bignum_cli(sign: i32, limbs: &[u64]) -> EgclVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let ptr = gc_alloc_obj(total_size, type_id::BIGNUM);
    unsafe {
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        EgclVal::from_heap_ptr(ptr)
    }
}
/// Floor of a *positive* rational as a BigInt (truncation == floor for ≥ 0).
pub fn bigrat_floor_pos(r: &BigRat) -> BigInt {
    big_divmod(&r.num, &r.den).0
}
/// Reciprocal `den/num` of a nonzero rational.
pub fn bigrat_recip(r: &BigRat) -> BigRat {
    BigRat::new(r.den.clone(), r.num.clone())
}
/// Extract (numerator, denominator) EgclVals from a RATIO heap object.
pub fn ratio_parts_val(v: EgclVal) -> Option<(EgclVal, EgclVal)> {
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let hdr = *(ptr as *const ObjectHeader);
        if hdr.type_id() != type_id::RATIO {
            return None;
        }
        let num = *(ptr.add(8) as *const EgclVal);
        let den = *(ptr.add(16) as *const EgclVal);
        Some((num, den))
    }
}
pub fn mag_trim(mut m: Vec<u64>) -> Vec<u64> {
    while !m.is_empty() && *m.last().unwrap() == 0 {
        m.pop();
    }
    m
}
pub fn mag_cmp(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}
pub fn mag_add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry: u128 = 0;
    for i in 0..a.len().max(b.len()) {
        let av = *a.get(i).unwrap_or(&0) as u128;
        let bv = *b.get(i).unwrap_or(&0) as u128;
        let s = av + bv + carry;
        out.push(s as u64);
        carry = s >> 64;
    }
    if carry != 0 {
        out.push(carry as u64);
    }
    mag_trim(out)
}
/// `a - b`, requiring `a >= b`.
pub fn mag_sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow: i128 = 0;
    for (i, &ai) in a.iter().enumerate() {
        let av = ai as i128;
        let bv = *b.get(i).unwrap_or(&0) as i128;
        let mut d = av - bv - borrow;
        if d < 0 {
            d += 1i128 << 64;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u64);
    }
    mag_trim(out)
}
pub fn mag_mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &av) in a.iter().enumerate() {
        let av = av as u128;
        let mut carry: u128 = 0;
        for (j, &bv) in b.iter().enumerate() {
            let cur = out[i + j] as u128 + av * bv as u128 + carry;
            out[i + j] = cur as u64;
            carry = cur >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = out[k] as u128 + carry;
            out[k] = cur as u64;
            carry = cur >> 64;
            k += 1;
        }
    }
    mag_trim(out)
}
/// Truncating division of magnitudes: returns (quotient, remainder). `b` must
/// be nonzero. Bit-by-bit long division — O(bits · limbs), fine at test sizes.
pub fn mag_divmod(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let n = mag_bitlen(a);
    let mut rem: Vec<u64> = Vec::new();
    let mut quot: Vec<u64> = Vec::new();
    for i in (0..n).rev() {
        rem = mag_shl1(&rem);
        if mag_bit(a, i) == 1 {
            if rem.is_empty() {
                rem.push(1);
            } else {
                rem[0] |= 1;
            }
        }
        if mag_cmp(&rem, b) != Ordering::Less {
            rem = mag_sub(&rem, b);
            mag_set_bit(&mut quot, i);
        }
    }
    (mag_trim(quot), mag_trim(rem))
}
#[derive(Clone, PartialEq, Eq)]
pub struct BigInt {
    pub sign: i8,      // -1, 0, or 1
    pub mag: Vec<u64>, // little-endian; no trailing zero limbs; empty iff sign == 0
}
impl BigInt {
    pub fn zero() -> BigInt {
        BigInt {
            sign: 0,
            mag: Vec::new(),
        }
    }

    pub fn one() -> BigInt {
        BigInt {
            sign: 1,
            mag: vec![1],
        }
    }

    pub fn from_i64(n: i64) -> BigInt {
        if n == 0 {
            return BigInt::zero();
        }
        BigInt {
            sign: if n < 0 { -1 } else { 1 },
            mag: vec![n.unsigned_abs()],
        }
    }

    pub fn from_mag(sign: i8, mag: Vec<u64>) -> BigInt {
        let mag = mag_trim(mag);
        if mag.is_empty() {
            BigInt::zero()
        } else {
            BigInt {
                sign: if sign < 0 { -1 } else { 1 },
                mag,
            }
        }
    }

    pub fn from_parts(sign: i32, limbs: &[u64]) -> BigInt {
        BigInt::from_mag(if sign < 0 { -1 } else { 1 }, limbs.to_vec())
    }

    pub fn is_zero(&self) -> bool {
        self.sign == 0
    }

    pub fn to_f64(&self) -> f64 {
        let mut f = 0.0f64;
        for &limb in self.mag.iter().rev() {
            f = f * 18446744073709551616.0 + limb as f64; // * 2^64
        }
        if self.sign < 0 { -f } else { f }
    }

    /// Canonicalize: a fixnum when it fits the 61-bit range, else a BIGNUM.
    pub fn to_val(&self) -> EgclVal {
        if self.is_zero() {
            return EgclVal::from_fixnum(0);
        }
        if self.mag.len() == 1 {
            let m = self.mag[0];
            if self.sign > 0 {
                if m <= FIXNUM_MAX as u64 {
                    return EgclVal::from_fixnum(m as i64);
                }
            } else if m <= FIXNUM_MIN.unsigned_abs() {
                return EgclVal::from_fixnum(-(m as i64));
            }
        }
        alloc_bignum_cli(self.sign as i32, &self.mag)
    }
}
pub fn big_neg(a: &BigInt) -> BigInt {
    BigInt {
        sign: -a.sign,
        mag: a.mag.clone(),
    }
}
pub fn big_add(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() {
        return b.clone();
    }
    if b.is_zero() {
        return a.clone();
    }
    if a.sign == b.sign {
        BigInt::from_mag(a.sign, mag_add(&a.mag, &b.mag))
    } else {
        match mag_cmp(&a.mag, &b.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::from_mag(a.sign, mag_sub(&a.mag, &b.mag)),
            Ordering::Less => BigInt::from_mag(b.sign, mag_sub(&b.mag, &a.mag)),
        }
    }
}
pub fn big_sub(a: &BigInt, b: &BigInt) -> BigInt {
    big_add(a, &big_neg(b))
}
pub fn big_mul(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() || b.is_zero() {
        return BigInt::zero();
    }
    BigInt::from_mag(a.sign * b.sign, mag_mul(&a.mag, &b.mag))
}
/// Materialize `a` as an `n`-limb little-endian two's-complement vector.
/// `n` must be large enough that the magnitude fits with a spare sign limb.
pub fn twos_limbs(a: &BigInt, n: usize) -> Vec<u64> {
    let mut v = vec![0u64; n];
    v[..a.mag.len()].copy_from_slice(&a.mag);
    if a.sign < 0 {
        let mut carry = true;
        for x in v.iter_mut() {
            let (y, c) = (!*x).overflowing_add(carry as u64);
            *x = y;
            carry = c;
        }
    }
    v
}
/// Bitwise `op` over the (conceptually infinite) two's-complement forms of `a`
/// and `b` — the LOGAND/LOGIOR/LOGXOR kernel (bliss-gvkz). One extra limb
/// beyond both magnitudes keeps a genuine sign limb, so the result's top bit
/// decides its sign; a negative result is negated back to sign-magnitude.
pub fn big_bitop(a: &BigInt, b: &BigInt, op: fn(u64, u64) -> u64) -> BigInt {
    let n = a.mag.len().max(b.mag.len()) + 1;
    let av = twos_limbs(a, n);
    let bv = twos_limbs(b, n);
    let mut r: Vec<u64> = av.iter().zip(&bv).map(|(&x, &y)| op(x, y)).collect();
    if r[n - 1] >> 63 != 0 {
        let mut carry = true;
        for x in r.iter_mut() {
            let (y, c) = (!*x).overflowing_add(carry as u64);
            *x = y;
            carry = c;
        }
        BigInt::from_mag(-1, r)
    } else {
        BigInt::from_mag(1, r)
    }
}
pub fn big_cmp(a: &BigInt, b: &BigInt) -> Ordering {
    if a.sign != b.sign {
        return a.sign.cmp(&b.sign);
    }
    match a.sign {
        0 => Ordering::Equal,
        1 => mag_cmp(&a.mag, &b.mag),
        _ => mag_cmp(&b.mag, &a.mag),
    }
}
/// Truncating (toward zero) division: (quotient, remainder). `b` nonzero.
pub fn big_divmod(a: &BigInt, b: &BigInt) -> (BigInt, BigInt) {
    let (q, r) = mag_divmod(&a.mag, &b.mag);
    (
        BigInt::from_mag(a.sign * b.sign, q),
        BigInt::from_mag(a.sign, r), // remainder takes the dividend's sign
    )
}
/// Exact division, assuming `g` divides `a` with no remainder.
pub fn big_divexact(a: &BigInt, g: &BigInt) -> BigInt {
    big_divmod(a, g).0
}
/// The rounding mode for the CL integer-division family.
#[derive(Clone, Copy)]
pub enum RoundMode {
    Floor,
    Ceiling,
    Truncate,
    Round,
}
/// `|n|`.
pub fn big_abs(n: &BigInt) -> BigInt {
    BigInt::from_mag(1, n.mag.clone())
}
/// Round the exact rational `r` (den > 0) to an integer per `mode`
/// (FLOOR toward −∞, CEILING toward +∞, TRUNCATE toward 0, ROUND to nearest,
/// ties to even).
pub fn bigrat_round_to_int(r: &BigRat, mode: RoundMode) -> BigInt {
    // big_divmod truncates toward zero; `rem` carries the numerator's sign.
    let (q, rem) = big_divmod(&r.num, &r.den);
    if rem.is_zero() {
        return q;
    }
    let neg = r.num.sign < 0;
    let away = |q: &BigInt| {
        if neg {
            big_sub(q, &BigInt::one())
        } else {
            big_add(q, &BigInt::one())
        }
    };
    match mode {
        RoundMode::Truncate => q,
        RoundMode::Floor => {
            if neg {
                away(&q)
            } else {
                q
            }
        }
        RoundMode::Ceiling => {
            if neg {
                q
            } else {
                away(&q)
            }
        }
        RoundMode::Round => {
            // Compare 2·|rem| with den: <half keeps q, >half rounds away, and a
            // tie rounds to the even neighbour.
            let two_rem = big_mul(&BigInt::from_i64(2), &big_abs(&rem));
            match big_cmp(&two_rem, &r.den) {
                Ordering::Less => q,
                Ordering::Greater => away(&q),
                Ordering::Equal => {
                    let even = q.mag.first().copied().unwrap_or(0) & 1 == 0;
                    if even { q } else { away(&q) }
                }
            }
        }
    }
}
/// CL `MOD` on floats: the remainder of FLOOR, which takes the sign of the
/// DIVISOR (CLHS 12.1.4.1), i.e. `a - b*floor(a/b)`.
///
/// This used to be `a.rem_euclid(b)`, whose result is always non-negative and
/// so disagrees with CL whenever the divisor is negative: `(mod -7.0 -3.0)`
/// answered 2.0 where ANSI requires -1.0, and `(mod 7.0 -3.0)` answered 1.0
/// where ANSI requires -2.0. The integer path was already correct, so only
/// float operands were affected (bliss-mwpb).
pub fn float_mod(a: f64, b: f64) -> f64 {
    a - b * (a / b).floor()
}
/// `EgclVal` fixnum for `n`, or `None` when `n` leaves the 61-bit fixnum range.
pub fn fixnum_from_i128(n: i128) -> Option<EgclVal> {
    if n >= FIXNUM_MIN as i128 && n <= FIXNUM_MAX as i128 {
        Some(EgclVal::from_fixnum(n as i64))
    } else {
        None
    }
}
/// Fixnum/fixnum fast path for FLOOR/CEILING/TRUNCATE/ROUND (and so MOD/REM).
///
/// The general `exact_int_div` path builds a reduced `BigRat` per operand — a
/// heap-allocated limb vector each, plus a gcd loop in `BigRat::new` — then does
/// a rational divide, round, multiply and subtract. That made `(mod i 16)` on
/// two fixnums ~18x more expensive than `(aref v 3)`, and put ~15% of a hot
/// loop's time in malloc/free (bliss-mwpb). Small integers are the common case,
/// so divide them natively; nothing here allocates.
///
/// The semantics are transcribed from `bigrat_round_to_int` and MUST stay
/// identical to it: `BigRat::new` normalizes the denominator positive, so its
/// `neg` is the sign of the exact QUOTIENT (not of `a`); the ROUND tie compares
/// `2*|rem|` against the denominator and breaks toward the even neighbour; and
/// the returned remainder is `a - q*b`. Reducing by the gcd scales `|rem|` and
/// the denominator by the same factor, so comparing against the unreduced `|b|`
/// gives the same ordering.
///
/// Arithmetic is in `i128` so no intermediate can overflow a fixnum operand
/// pair; `None` defers to the general path if a result leaves fixnum range.
pub fn fixnum_int_div(a: i64, b: i64, mode: RoundMode) -> Option<(EgclVal, EgclVal)> {
    debug_assert!(b != 0, "caller must reject a zero divisor");
    let (a, b) = (a as i128, b as i128);
    let tq = a / b; // truncates toward zero
    let trem = a % b; // carries the sign of `a`
    let q = if trem == 0 {
        tq
    } else {
        let neg = (a < 0) != (b < 0);
        let away = if neg { tq - 1 } else { tq + 1 };
        match mode {
            RoundMode::Truncate => tq,
            RoundMode::Floor => {
                if neg {
                    away
                } else {
                    tq
                }
            }
            RoundMode::Ceiling => {
                if neg {
                    tq
                } else {
                    away
                }
            }
            RoundMode::Round => match (2 * trem.abs()).cmp(&b.abs()) {
                Ordering::Less => tq,
                Ordering::Greater => away,
                Ordering::Equal => {
                    if tq % 2 == 0 {
                        tq
                    } else {
                        away
                    }
                }
            },
        }
    };
    Some((fixnum_from_i128(q)?, fixnum_from_i128(a - q * b)?))
}
/// Exact FLOOR/CEILING/TRUNCATE/ROUND of `a`/`b` for rational operands (fixnum,
/// bignum, ratio). Returns the (quotient, remainder = a − quotient·b) as
/// EgclVals, or `None` if either operand is a float (the caller then uses the
/// f64 path). This replaces the old `num_val`→f64→`as i64` path, which
/// overflowed and lost precision on bignums/ratios (bliss-05hy).
pub fn exact_int_div(
    a: EgclVal,
    b: EgclVal,
    mode: RoundMode,
) -> Option<Result<(EgclVal, EgclVal), EgclError>> {
    if a.is_single_float() || b.is_single_float() {
        return None;
    }
    // Fixnum/fixnum without touching BigRat or the allocator (bliss-mwpb).
    if a.is_fixnum() && b.is_fixnum() {
        let bi = b.as_fixnum();
        if bi == 0 {
            return Some(Err(EgclError::ArithmeticError("division by zero".into())));
        }
        if let Some(pair) = fixnum_int_div(a.as_fixnum(), bi, mode) {
            return Some(Ok(pair));
        }
    }
    let ra = as_bigrat(a)?;
    let rb = as_bigrat(b)?;
    if rb.num.is_zero() {
        return Some(Err(EgclError::ArithmeticError("division by zero".into())));
    }
    let q = bigrat_round_to_int(&bigrat_div(&ra, &rb), mode);
    // remainder = a − q·b (exact), computed in Rust-native BigRat.
    let rem = bigrat_sub(&ra, &bigrat_mul(&BigRat::from_bigint(q.clone()), &rb));
    // Convert to EgclVals, rooting the quotient across the remainder alloc.
    crate::rooted!(q_val = q.to_val());
    let rem_val = rem.to_val();
    Some(Ok((*q_val, rem_val)))
}
/// Non-negative gcd of two integers (gcd(0,0) == 0).
pub fn big_gcd(x: &BigInt, y: &BigInt) -> BigInt {
    let mut a = BigInt::from_mag(1, x.mag.clone());
    let mut b = BigInt::from_mag(1, y.mag.clone());
    while !b.is_zero() {
        let (_, r) = big_divmod(&a, &b);
        a = b;
        b = BigInt::from_mag(1, r.mag);
    }
    a
}
/// Read a fixnum or BIGNUM into a BigInt. `None` for any other value.
pub fn bigint_from_val(v: EgclVal) -> Option<BigInt> {
    if v.is_fixnum() {
        return Some(BigInt::from_i64(v.as_fixnum()));
    }
    if v.is_heap_object() {
        unsafe {
            let ptr = v.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::BIGNUM {
                let sign = *(ptr.add(8) as *const i32);
                let n = *(ptr.add(12) as *const u32) as usize;
                let mut limbs = Vec::with_capacity(n);
                for i in 0..n {
                    limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                }
                return Some(BigInt::from_parts(sign, &limbs));
            }
        }
    }
    None
}
/// An exact rational: denominator kept positive and reduced to lowest terms.
#[derive(Clone)]
pub struct BigRat {
    pub num: BigInt,
    pub den: BigInt,
}
impl BigRat {
    pub fn from_i64(n: i64) -> BigRat {
        BigRat {
            num: BigInt::from_i64(n),
            den: BigInt::one(),
        }
    }

    pub fn from_bigint(n: BigInt) -> BigRat {
        BigRat {
            num: n,
            den: BigInt::one(),
        }
    }

    /// Build a reduced rational; `den` must be nonzero.
    pub fn new(mut num: BigInt, mut den: BigInt) -> BigRat {
        if den.sign < 0 {
            num = big_neg(&num);
            den = big_neg(&den);
        }
        if num.is_zero() {
            return BigRat {
                num: BigInt::zero(),
                den: BigInt::one(),
            };
        }
        let g = big_gcd(&num, &den);
        if big_cmp(&g, &BigInt::one()) != Ordering::Equal {
            num = big_divexact(&num, &g);
            den = big_divexact(&den, &g);
        }
        BigRat { num, den }
    }

    pub fn is_integer(&self) -> bool {
        big_cmp(&self.den, &BigInt::one()) == Ordering::Equal
    }

    pub fn to_f64(&self) -> f64 {
        self.num.to_f64() / self.den.to_f64()
    }

    pub fn to_val(&self) -> EgclVal {
        if self.is_integer() {
            self.num.to_val()
        } else {
            alloc_ratio_cli(self.num.to_val(), self.den.to_val())
        }
    }
}
pub fn bigrat_neg(a: &BigRat) -> BigRat {
    BigRat {
        num: big_neg(&a.num),
        den: a.den.clone(),
    }
}
pub fn bigrat_add(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_add(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}
pub fn bigrat_sub(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_sub(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}
pub fn bigrat_mul(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.num), big_mul(&a.den, &b.den))
}
/// `a / b`; `b` must be nonzero (the caller checks for zero divisors).
pub fn bigrat_div(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.den), big_mul(&a.den, &b.num))
}
pub fn bigrat_cmp(a: &BigRat, b: &BigRat) -> Ordering {
    // Denominators are positive, so comparing cross-products is order-preserving.
    big_cmp(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den))
}
/// Raise an exact rational to a non-negative integer power.
pub fn bigrat_pow(base: &BigRat, mut e: u64) -> BigRat {
    let mut result = BigRat::from_i64(1);
    let mut b = base.clone();
    while e > 0 {
        if e & 1 == 1 {
            result = bigrat_mul(&result, &b);
        }
        e >>= 1;
        if e > 0 {
            b = bigrat_mul(&b, &b);
        }
    }
    result
}
/// A fixnum, BIGNUM, or RATIO as an exact rational; `None` otherwise.
pub fn as_bigrat(v: EgclVal) -> Option<BigRat> {
    if let Some(n) = bigint_from_val(v) {
        return Some(BigRat::from_bigint(n));
    }
    if let Some((nv, dv)) = ratio_parts_val(v) {
        let n = bigint_from_val(nv)?;
        let d = bigint_from_val(dv)?;
        if d.is_zero() {
            return None;
        }
        return Some(BigRat::new(n, d));
    }
    None
}
pub fn mag_bit(m: &[u64], idx: usize) -> u64 {
    (m[idx / 64] >> (idx % 64)) & 1
}
pub fn mag_bitlen(m: &[u64]) -> usize {
    if m.is_empty() {
        return 0;
    }
    let top = m.len() - 1;
    64 * top + (64 - m[top].leading_zeros() as usize)
}
pub fn mag_shl1(m: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(m.len() + 1);
    let mut carry = 0u64;
    for &x in m {
        out.push((x << 1) | carry);
        carry = x >> 63;
    }
    if carry != 0 {
        out.push(carry);
    }
    mag_trim(out)
}
pub fn mag_set_bit(m: &mut Vec<u64>, idx: usize) {
    let w = idx / 64;
    while m.len() <= w {
        m.push(0);
    }
    m[w] |= 1u64 << (idx % 64);
}
