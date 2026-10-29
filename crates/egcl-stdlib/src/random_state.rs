// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Standard random-state construction and the existing SplitMix64 generator.
use egcl_rt::value::{NIL, T};
use egcl_rt::{EgclError, EgclVal};

pub fn random_state_p(value: EgclVal) -> bool {
    egcl_rt::random_state::is_random_state(value)
}

fn state_error(value: EgclVal) -> EgclError {
    EgclError::TypeError {
        datum: value,
        expected: "RANDOM-STATE".into(),
    }
}

/// One entropy-generator step, exactly as the previous interpreter helper.
fn next_random_u64() -> u64 {
    use std::cell::Cell;
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    thread_local! {
        static STATE: Cell<u64> = Cell::new(RandomState::new().build_hasher().finish() | 1);
    }
    STATE.with(|state| {
        let mut z = state.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        state.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    })
}

/// Nonallocating draw. A malformed designator is never treated as a fallback
/// generator: callers receive the standard type error.
pub fn draw(state: EgclVal) -> Result<u64, EgclError> {
    egcl_rt::random_state::draw(state).ok_or_else(|| state_error(state))
}

/// STATE is T for a new state, NIL for a copy of DEFAULT, or an existing state.
pub fn make_random_state(state: EgclVal, default: EgclVal) -> Result<EgclVal, EgclError> {
    if state == T {
        return egcl_rt::random_state::make(next_random_u64() >> 3);
    }
    if state != NIL && !random_state_p(state) {
        return Err(EgclError::TypeError {
            datum: state,
            expected: "(OR RANDOM-STATE (MEMBER NIL T))".into(),
        });
    }
    egcl_rt::rooted!(source = if state == NIL { default } else { state });
    let seed = egcl_rt::random_state::snapshot(*source).ok_or_else(|| state_error(*source))?;
    // Copying never consumes a draw or changes the source, including a fresh
    // state that has not yet been passed to RANDOM.
    egcl_rt::random_state::make(seed)
}

/// Read syntax reconstructs an independent state without consuming a draw.
pub fn from_seed(value: EgclVal) -> Result<EgclVal, EgclError> {
    let seed = egcl_rt::bignum::bigint_from_val(value)
        .filter(|number| number.sign >= 0 && number.mag.len() <= 1)
        .map(|number| number.mag.first().copied().unwrap_or(0))
        .ok_or_else(|| EgclError::TypeError {
            datum: value,
            expected: "(UNSIGNED-BYTE 64)".into(),
        })?;
    egcl_rt::random_state::make(seed)
}
