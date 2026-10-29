// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Opaque random states. The payload word is RAW, never GC references.
use crate::object::type_id;
use crate::{EgclError, EgclVal};

pub fn is_random_state(value: EgclVal) -> bool {
    // Public predicates also receive off-heap strings and pseudoheap handles.
    // Random states are always arena allocations; never read an unvalidated header.
    crate::gc::heap_object_type_id(value) == Some(type_id::RANDOM_STATE)
}

/// Snapshot state before allocation; callers never retain a borrowed body.
pub fn snapshot(value: EgclVal) -> Option<u64> {
    if !is_random_state(value) {
        return None;
    }
    unsafe {
        let body = value.as_ptr().add(8).cast::<u64>();
        Some(body.read())
    }
}

pub fn make(seed: u64) -> Result<EgclVal, EgclError> {
    let body = crate::gc::alloc_typed(8, type_id::RANDOM_STATE).ok_or(EgclError::Oom)?;
    unsafe {
        body.cast::<u64>().write(seed);
        Ok(EgclVal::from_heap_ptr(body.sub(8)))
    }
}

/// Preserve the existing 61-bit SplitMix64 state transition. States are seeded
/// at creation, so snapshots preserve even the first future draw. No allocation,
/// polling, or escaped body pointer.
pub fn draw(value: EgclVal) -> Option<u64> {
    let s = snapshot(value)?;
    let s = s.wrapping_add(0x9E37_79B9_7F4A_7C15) & ((1u64 << 61) - 1);
    unsafe {
        let body = value.as_ptr().add(8).cast::<u64>();
        body.write(s);
    }
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    Some(z ^ (z >> 31))
}
