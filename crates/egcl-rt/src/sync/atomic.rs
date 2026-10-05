// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use crate::value::EgclVal;
use crate::{bignum, error::EgclError};
use std::sync::atomic::{AtomicU64, Ordering};

/// Compare one aligned Lisp reference slot with `old` and replace it with
/// `new`. The observed value is returned on both success and failure.
///
/// # Safety
/// `slot` must remain live and aligned for the duration of this non-allocating
/// operation. Every concurrent mutator of the same slot must use an atomic
/// operation.
pub unsafe fn compare_exchange_ref(slot: *mut EgclVal, old: EgclVal, new: EgclVal) -> EgclVal {
    crate::gc::write_barrier(slot, old, new);
    let atomic = unsafe { &*slot.cast::<AtomicU64>() };
    match atomic.compare_exchange(old.0, new.0, Ordering::AcqRel, Ordering::Acquire) {
        Ok(actual) | Err(actual) => EgclVal(actual),
    }
}

pub fn checked_fixnum_update(
    old: EgclVal,
    delta: EgclVal,
    subtract: bool,
) -> Result<EgclVal, EgclError> {
    if !old.is_fixnum() || !delta.is_fixnum() {
        return Err(EgclError::TypeError {
            datum: if old.is_fixnum() { delta } else { old },
            expected: "FIXNUM".into(),
        });
    }
    let old = i128::from(old.as_fixnum());
    let delta = i128::from(delta.as_fixnum());
    let next = if subtract { old - delta } else { old + delta };
    if next < i128::from(bignum::FIXNUM_MIN) || next > i128::from(bignum::FIXNUM_MAX) {
        return Err(EgclError::ArithmeticError(
            "atomic fixnum arithmetic overflow".into(),
        ));
    }
    Ok(EgclVal::from_fixnum(next as i64))
}

/// Atomically update a fixnum reference slot and return its previous value.
///
/// # Safety
/// The requirements are the same as [`compare_exchange_ref`].
pub unsafe fn update_fixnum_ref(
    slot: *mut EgclVal,
    delta: EgclVal,
    subtract: bool,
) -> Result<EgclVal, EgclError> {
    let atomic = unsafe { &*slot.cast::<AtomicU64>() };
    let mut observed = EgclVal(atomic.load(Ordering::Acquire));
    loop {
        let replacement = checked_fixnum_update(observed, delta, subtract)?;
        match atomic.compare_exchange_weak(
            observed.0,
            replacement.0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(observed),
            Err(actual) => observed = EgclVal(actual),
        }
    }
}

