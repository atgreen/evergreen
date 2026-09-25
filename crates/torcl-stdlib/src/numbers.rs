//! Numeric primitives shared by interpreted and compiled calls (§5.2.1).

use std::cmp::Ordering;
use torcl_rt::bignum::{
    BigInt, big_cmp, bigint_from_val, fixnum_from_i128, mag_add, mag_bitlen, mag_sub,
    ratio_parts_val,
};
use torcl_rt::error::TorclError;
use torcl_rt::value::TorclVal;

/// Compare a real number with zero without rounding exact rationals through a
/// float. None denotes an unordered floating-point value (NaN). This path only
/// reads Lisp objects; any bignum copies allocate Rust memory, never Lisp memory.
fn compare_real_to_zero(value: TorclVal) -> Result<Option<Ordering>, TorclError> {
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
    Err(TorclError::TypeError {
        datum: value,
        expected: "real".into(),
    })
}

/// ZEROP accepts the whole numeric tower, including complex zero.
pub fn zerop(value: TorclVal) -> Result<bool, TorclError> {
    if let Some(real) = torcl_rt::types::complex_realpart(value) {
        let imag = torcl_rt::types::complex_imagpart(value).expect("complex imaginary part");
        return Ok(compare_real_to_zero(real)? == Some(Ordering::Equal)
            && compare_real_to_zero(imag)? == Some(Ordering::Equal));
    }
    compare_real_to_zero(value)
        .map(|order| order == Some(Ordering::Equal))
        .map_err(|_| TorclError::TypeError {
            datum: value,
            expected: "number".into(),
        })
}

/// PLUSP and MINUSP accept reals only; complex numbers have no ordering.
pub fn plusp(value: TorclVal) -> Result<bool, TorclError> {
    compare_real_to_zero(value).map(|order| order == Some(Ordering::Greater))
}

pub fn minusp(value: TorclVal) -> Result<bool, TorclError> {
    compare_real_to_zero(value).map(|order| order == Some(Ordering::Less))
}

fn integer(value: TorclVal) -> Result<BigInt, TorclError> {
    bigint_from_val(value).ok_or_else(|| TorclError::TypeError {
        datum: value,
        expected: "integer".into(),
    })
}

/// Membership in a sized SIGNED-BYTE/UNSIGNED-BYTE type. None means any width.
/// Compare bit lengths rather than constructing 2^width: even a bignum width
/// needs no giant allocation or host-sized shift. No Lisp allocation occurs.
pub fn byte_type_p(
    value: TorclVal,
    width: Option<TorclVal>,
    signed: bool,
) -> Result<bool, TorclError> {
    let width = width
        .map(|width| {
            bigint_from_val(width)
                .filter(|n| n.sign > 0)
                .ok_or_else(|| TorclError::TypeError {
                    datum: width,
                    expected: "a positive integer byte width".into(),
                })
        })
        .transpose()?;
    let Some(value) = bigint_from_val(value) else {
        return Ok(false);
    };
    if !signed && value.sign < 0 {
        return Ok(false);
    }
    let Some(width) = width else {
        return Ok(true);
    };
    // For negative n, INTEGER-LENGTH counts the bits of -n-1. The signed
    // representation then needs one additional sign bit, including for -1.
    let bits = if value.sign < 0 {
        mag_bitlen(&mag_sub(&value.mag, &[1]))
    } else {
        mag_bitlen(&value.mag)
    };
    let required = BigInt::from_mag(1, vec![bits as u64 + u64::from(signed)]);
    Ok(big_cmp(&required, &width) != Ordering::Greater)
}

/// Arithmetic shift, with sign extension on right shifts and arbitrary precision
/// on left shifts. Both arguments are checked even for zero operands/counts.
///
/// The common fixnum case allocates nothing. The general case copies both
/// operands into Rust-owned magnitudes before the only GC allocation, `to_val`;
/// no heap reference is read after that allocation.
pub fn ash(value: TorclVal, count: TorclVal) -> Result<TorclVal, TorclError> {
    if value.is_fixnum() && count.is_fixnum() {
        let n = value.as_fixnum();
        let shift = count.as_fixnum();
        if shift <= 0 {
            return Ok(TorclVal::from_fixnum(n >> shift.unsigned_abs().min(63)));
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
            return Ok(TorclVal::from_fixnum(if n.sign < 0 { -1 } else { 0 }));
        };
        return Ok(shift_right(&n, bits).to_val());
    }
    shift_left(&n, bits.ok_or(TorclError::Oom)?).map(|result| result.to_val())
}

fn shift_left(n: &BigInt, bits: usize) -> Result<BigInt, TorclError> {
    let words = bits / 64;
    let remainder = bits % 64;
    let len = n
        .mag
        .len()
        .checked_add(words)
        .and_then(|len| len.checked_add(usize::from(remainder != 0)))
        .filter(|&len| len <= u32::MAX as usize)
        .ok_or(TorclError::Oom)?;
    let mut result = Vec::new();
    result.try_reserve_exact(len).map_err(|_| TorclError::Oom)?;
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
