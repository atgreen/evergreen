// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::error::EgclError;
use egcl_rt::object::ConsCell;
use egcl_rt::value::EgclVal;

pub fn compare_exchange_cons(
    cons: EgclVal,
    car: bool,
    old: EgclVal,
    new: EgclVal,
) -> Result<EgclVal, EgclError> {
    if !cons.is_cons() {
        return Err(EgclError::TypeError {
            datum: cons,
            expected: "CONS".into(),
        });
    }
    let cell = unsafe { cons.as_ptr().cast::<ConsCell>() };
    let slot = if car {
        unsafe { &raw mut (*cell).car }
    } else {
        unsafe { &raw mut (*cell).cdr }
    };
    Ok(unsafe { egcl_rt::sync::atomic::compare_exchange_ref(slot, old, new) })
}

