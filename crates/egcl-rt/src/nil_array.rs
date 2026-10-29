// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! NIL-element storage has a length but no inhabitable element slots.
use crate::object::{ElementTypeTag, ObjectHeader, type_id};
use crate::{EgclError, EgclVal};

pub fn len(value: EgclVal) -> Option<usize> {
    // Pseudoheap handles and off-heap registered strings are valid values.
    // Only NIL arrays allocated in the GC arena have this storage layout.
    if crate::gc::heap_object_type_id(value) != Some(type_id::SIMPLE_ARRAY) {
        return None;
    }
    unsafe {
        let header = &*value.as_ptr().cast::<ObjectHeader>();
        if header.type_id() != type_id::SIMPLE_ARRAY {
            return None;
        }
        let body = value
            .as_ptr()
            .add(if header.is_large_object() { 16 } else { 8 });
        if body.read() != ElementTypeTag::Nil as u8 {
            return None;
        }
        usize::try_from(body.add(8).cast::<u64>().read()).ok()
    }
}

pub fn is_vector(value: EgclVal) -> bool {
    len(value).is_some()
}

/// Raw tag/length only: no references, payload proportional to neither length
/// nor element width. No interior pointer escapes this allocation boundary.
pub fn make(length: usize) -> Result<EgclVal, EgclError> {
    if length >= crate::bignum::FIXNUM_MAX as usize {
        return Err(EgclError::Oom);
    }
    let body = crate::gc::alloc_typed(16, type_id::SIMPLE_ARRAY).ok_or(EgclError::Oom)?;
    unsafe {
        body.cast::<u64>().write(0);
        body.write(ElementTypeTag::Nil as u8);
        body.add(8).cast::<u64>().write(length as u64);
        Ok(EgclVal::from_heap_ptr(body.sub(8)))
    }
}
