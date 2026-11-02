// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Rank-one simple arrays of raw unsigned-byte 8 elements.
//!
//! Body: element-type byte plus seven zero padding bytes, raw length word,
//! then contiguous bytes.
//! The payload has no GC references. Keep the tagged array rooted across any
//! allocation and derive its body address again afterward. The public API
//! returns values, never borrowed interior pointers.

use crate::object::{ElementTypeTag, ObjectHeader, type_id};
use crate::{EgclError, EgclVal};

pub const LENGTH_OFFSET: usize = 8;
pub const DATA_OFFSET: usize = 16;

fn body(array: EgclVal) -> Option<*mut u8> {
    if !array.is_heap_object() {
        return None;
    }
    let header = unsafe { array.as_ptr() };
    let info = unsafe { &*header.cast::<ObjectHeader>() };
    if info.type_id() != type_id::SIMPLE_ARRAY {
        return None;
    }
    let body = unsafe { header.add(if info.is_large_object() { 16 } else { 8 }) };
    (unsafe { body.read() } == ElementTypeTag::U8 as u8).then_some(body)
}

pub fn is_vector(array: EgclVal) -> bool {
    body(array).is_some()
}

pub fn len(array: EgclVal) -> Option<usize> {
    let body = body(array)?;
    Some(unsafe { body.add(LENGTH_OFFSET).cast::<u64>().read() } as usize)
}

/// Allocate zero or more elements. Arithmetic is checked before allocation;
/// the fill is raw bits and therefore needs no root during collection.
pub fn make(length: usize, fill: u8) -> Result<EgclVal, EgclError> {
    let size = length
        .checked_add(DATA_OFFSET)
        // Leave room for the extended header and alignment in metadata that
        // the allocator stores in u32 fields, as well as pointer offsets.
        .filter(|&bytes| bytes <= u32::MAX as usize - 32 && bytes <= isize::MAX as usize - 32)
        .ok_or(EgclError::Oom)?;
    let body = crate::gc::alloc_typed(size, type_id::SIMPLE_ARRAY).ok_or(EgclError::Oom)?;
    unsafe {
        // The discriminant is the first byte on every architecture. Clear
        // padding explicitly, including on big-endian targets.
        body.cast::<u64>().write(0);
        body.write(ElementTypeTag::U8 as u8);
        body.add(LENGTH_OFFSET).cast::<u64>().write(length as u64);
        for index in 0..length {
            body.add(DATA_OFFSET + index).write(fill);
        }
        Ok(EgclVal::from_heap_ptr(
            body.sub(crate::gc::body_header_offset(size)),
        ))
    }
}

/// Read raw bits without allocating or retaining an interior pointer.
pub fn get(array: EgclVal, index: usize) -> Option<u8> {
    let body = body(array)?;
    let length = unsafe { body.add(LENGTH_OFFSET).cast::<u64>().read() } as usize;
    if index >= length {
        return None;
    }
    Some(unsafe { body.add(DATA_OFFSET + index).read() })
}

/// Store without allocation or a reference write barrier: all bits are numeric.
pub fn set(array: EgclVal, index: usize, value: u8) -> bool {
    let Some(body) = body(array) else {
        return false;
    };
    let length = unsafe { body.add(LENGTH_OFFSET).cast::<u64>().read() } as usize;
    if index >= length {
        return false;
    }
    unsafe {
        body.add(DATA_OFFSET + index).write(value);
    }
    true
}

/// Store `value` in elements `start..end` (FILL), as one memset. Returns
/// `false`, touching nothing, if `array` is not a packed octet vector or the
/// range is out of bounds. No allocation.
pub fn fill(array: EgclVal, start: usize, end: usize, value: u8) -> bool {
    let Some(body) = body(array) else {
        return false;
    };
    let length = unsafe { body.add(LENGTH_OFFSET).cast::<u64>().read() } as usize;
    if start > end || end > length {
        return false;
    }
    unsafe { std::ptr::write_bytes(body.add(DATA_OFFSET + start), value, end - start) };
    true
}

/// Copy `count` elements of `src` from `src_start` into `dst` from
/// `dst_start` (REPLACE), as one memmove, so overlapping ranges of one vector
/// behave as if the source were copied first. Returns `false`, touching
/// nothing, if either is not a packed octet vector or a range is out of
/// bounds. No allocation.
pub fn copy(dst: EgclVal, dst_start: usize, src: EgclVal, src_start: usize, count: usize) -> bool {
    let (Some(d), Some(s)) = (body(dst), body(src)) else {
        return false;
    };
    let (Some(dst_len), Some(src_len)) = (len(dst), len(src)) else {
        return false;
    };
    if dst_start.checked_add(count).is_none_or(|e| e > dst_len)
        || src_start.checked_add(count).is_none_or(|e| e > src_len)
    {
        return false;
    }
    unsafe {
        std::ptr::copy(
            s.add(DATA_OFFSET + src_start),
            d.add(DATA_OFFSET + dst_start),
            count,
        )
    };
    true
}
