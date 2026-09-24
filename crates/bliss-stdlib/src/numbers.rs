//! Numeric primitives shared by interpreted and compiled calls (§5.2.1).

use bliss_rt::bignum::{
    BigInt, bigint_from_val, fixnum_from_i128, mag_add, mag_bitlen, ratio_parts_val,
};
use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;
use std::cmp::Ordering;

/// Compare a real number with zero without rounding exact rationals through a
/// float. None denotes an unordered floating-point value (NaN). This path only
/// reads Lisp objects; any bignum copies allocate Rust memory, never Lisp memory.
fn compare_real_to_zero(value: BlissVal) -> Result<Option<Ordering>, BlissError> {
    if value.is_fixnum() {
        return Ok(Some(value.as_fixnum().cmp(&0)));
    }
    if value.is_single_float() {
        return Ok(value.as_single_float().partial_cmp(&0.0));
    }
    if value.is_double_float() {
        return Ok(value.as_double_float().partial_cmp(&0.0));
    }
    if let Some(n) = bigint_from_val(value) {
        return Ok(Some(n.sign.cmp(&0)));
    }
    if let Some((num, den)) = ratio_parts_val(value) {
        let sign = integer(num)?.sign * integer(den)?.sign;
        return Ok(Some(sign.cmp(&0)));
    }
    Err(BlissError::TypeError {
        datum: value,
        expected: "real".into(),
    })
}

/// ZEROP accepts the whole numeric tower, including complex zero.
pub fn zerop(value: BlissVal) -> Result<bool, BlissError> {
    if let Some(real) = bliss_rt::types::complex_realpart(value) {
        let imag = bliss_rt::types::complex_imagpart(value).expect("complex imaginary part");
        return Ok(compare_real_to_zero(real)? == Some(Ordering::Equal)
            && compare_real_to_zero(imag)? == Some(Ordering::Equal));
    }
    compare_real_to_zero(value)
        .map(|order| order == Some(Ordering::Equal))
        .map_err(|_| BlissError::TypeError {
            datum: value,
            expected: "number".into(),
        })
}

/// PLUSP and MINUSP accept reals only; complex numbers have no ordering.
pub fn plusp(value: BlissVal) -> Result<bool, BlissError> {
    compare_real_to_zero(value).map(|order| order == Some(Ordering::Greater))
}

pub fn minusp(value: BlissVal) -> Result<bool, BlissError> {
    compare_real_to_zero(value).map(|order| order == Some(Ordering::Less))
}

fn integer(value: BlissVal) -> Result<BigInt, BlissError> {
    bigint_from_val(value).ok_or_else(|| BlissError::TypeError {
        datum: value,
        expected: "integer".into(),
    })
}

/// Arithmetic shift, with sign extension on right shifts and arbitrary precision
/// on left shifts. Both arguments are checked even for zero operands/counts.
///
/// The common fixnum case allocates nothing. The general case copies both
/// operands into Rust-owned magnitudes before the only GC allocation, `to_val`;
/// no heap reference is read after that allocation.
pub fn ash(value: BlissVal, count: BlissVal) -> Result<BlissVal, BlissError> {
    if value.is_fixnum() && count.is_fixnum() {
        let n = value.as_fixnum();
        let shift = count.as_fixnum();
        if shift <= 0 {
            return Ok(BlissVal::from_fixnum(n >> shift.unsigned_abs().min(63)));
        }
        if shift < 64 {
            if let Some(result) = fixnum_from_i128((n as i128) << shift) {
                return Ok(result);
            }
        }
    }

    let n = integer(value)?;
    let shift = integer(count)?;
    if n.is_zero() || shift.is_zero() {
        return Ok(value);
    }
    // A multi-limb count exceeds any representable operand length. Such a
    // right shift is just the sign; a nonzero left shift cannot fit in memory.
    let bits = if shift.mag.len() == 1 {
        usize::try_from(shift.mag[0]).ok()
    } else {
        None
    };
    if shift.sign < 0 {
        let Some(bits) = bits.filter(|&bits| bits < mag_bitlen(&n.mag)) else {
            return Ok(BlissVal::from_fixnum(if n.sign < 0 { -1 } else { 0 }));
        };
        return Ok(shift_right(&n, bits).to_val());
    }
    shift_left(&n, bits.ok_or(BlissError::Oom)?).map(|result| result.to_val())
}

fn shift_left(n: &BigInt, bits: usize) -> Result<BigInt, BlissError> {
    let words = bits / 64;
    let remainder = bits % 64;
    let len = n
        .mag
        .len()
        .checked_add(words)
        .and_then(|len| len.checked_add(usize::from(remainder != 0)))
        .filter(|&len| len <= u32::MAX as usize)
        .ok_or(BlissError::Oom)?;
    let mut result = Vec::new();
    result.try_reserve_exact(len).map_err(|_| BlissError::Oom)?;
    result.resize(len, 0);
    for (i, &limb) in n.mag.iter().enumerate() {
        result[i + words] |= limb << remainder;
        if remainder != 0 {
            result[i + words + 1] = limb >> (64 - remainder);
        }
    }
    Ok(BigInt::from_mag(n.sign, result))
}

fn shift_right(n: &BigInt, bits: usize) -> BigInt {
    let words = bits / 64;
    let remainder = bits % 64;
    let discarded = n.mag[..words].iter().any(|&limb| limb != 0)
        || (remainder != 0 && n.mag[words] & ((1u64 << remainder) - 1) != 0);
    let mut result = Vec::with_capacity(n.mag.len() - words);
    for i in words..n.mag.len() {
        let mut limb = n.mag[i] >> remainder;
        if remainder != 0 && i + 1 < n.mag.len() {
            limb |= n.mag[i + 1] << (64 - remainder);
        }
        result.push(limb);
    }
    // Magnitudes truncate toward zero; arithmetic right shift rounds negative
    // values down, so any discarded one bit requires one more in magnitude.
    if n.sign < 0 && discarded {
        result = mag_add(&result, &[1]);
    }
    BigInt::from_mag(n.sign, result)
}
