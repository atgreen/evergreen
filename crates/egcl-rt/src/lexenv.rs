// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Heap lexical environments (Backpack, bliss-gg3i1.7.15.4).
//!
//! A captured lexical variable of a heap-environment compilation unit lives in
//! one of these: an ordinary movable, nursery-allocated GC object whose layout
//! the bytecode lowerer fixed at compile time. Every tier addresses a slot by
//! `(depth, index)` from an environment value it already holds, so no access
//! does name work or takes a lock.
//!
//! Body layout (`type_id::LEXENV`), in 8-byte words:
//!
//! ```text
//!   word 0      slot count N (raw, never a reference)
//!   word 1      parent environment, or NIL
//!   word 2+i    slot i, for i < N
//! ```
//!
//! The count is raw so the collector traces exactly `parent` and the `N`
//! slots, never the alignment padding the allocator may add. A slot holds
//! `UNBOUND` until its binding is made. Stores go through the write barrier.

use crate::EgclVal;
use crate::object::type_id;
use crate::value::{NIL, UNBOUND};

/// Byte offset of the parent word from the object's *header* pointer (the
/// address an `EgclVal` of this object carries).
pub const PARENT_OFFSET: usize = 16;
/// Byte offset of slot 0 from the object's header pointer.
pub const SLOT0_OFFSET: usize = 24;

/// Largest slot count one environment may have (the operand is a `u16`).
pub const MAX_SLOTS: usize = u16::MAX as usize;

/// Whether `value` is a heap lexical environment.
#[inline]
pub fn is_env(value: EgclVal) -> bool {
    crate::gc::heap_object_type_id(value) == Some(type_id::LEXENV)
}

/// Allocate an environment of `size` unbound slots whose parent is `parent`
/// (an environment or NIL). Can collect: `parent` is rooted across it and the
/// result is fully initialized before it is returned.
pub fn make(parent: EgclVal, size: usize) -> Option<EgclVal> {
    if size > MAX_SLOTS {
        return None;
    }
    crate::rooted!(parent = parent);
    let body = crate::gc::alloc_typed(8 * (2 + size), type_id::LEXENV)?;
    // SAFETY: a fresh body of `2 + size` words; nothing below can collect.
    unsafe {
        let words = body.cast::<u64>();
        words.write(size as u64);
        words.add(1).cast::<EgclVal>().write(*parent);
        for i in 0..size {
            words.add(2 + i).cast::<EgclVal>().write(UNBOUND);
        }
        Some(EgclVal::from_heap_ptr(body.sub(8)))
    }
}

/// The slot count of `env`, which must be an environment.
#[inline]
fn count(env: EgclVal) -> usize {
    // SAFETY: callers pass a validated environment; word 0 is its raw count.
    unsafe { *(env.as_ptr().add(8) as *const u64) as usize }
}

/// The number of slots of `env`, or `None` when it is not an environment.
pub fn len(env: EgclVal) -> Option<usize> {
    is_env(env).then(|| count(env))
}

/// The parent of `env`, or `None` when it is not an environment.
pub fn parent(env: EgclVal) -> Option<EgclVal> {
    // SAFETY: a validated environment's word 1 is its parent.
    is_env(env).then(|| unsafe { *(env.as_ptr().add(PARENT_OFFSET) as *const EgclVal) })
}

/// The environment `depth` parent links above `env`, or `None` when the chain
/// is shorter than that.
#[inline]
pub fn hop(mut env: EgclVal, depth: u8) -> Option<EgclVal> {
    for _ in 0..depth {
        env = parent(env)?;
    }
    is_env(env).then_some(env)
}

/// The address of slot `index` of the environment `depth` links above `env`,
/// when both exist. No allocation; valid until the next collection.
#[inline]
pub fn slot_address(env: EgclVal, depth: u8, index: u16) -> Option<*mut EgclVal> {
    let env = hop(env, depth)?;
    (usize::from(index) < count(env)).then(|| {
        // SAFETY: in bounds of a validated environment's slots.
        unsafe { env.as_ptr().add(SLOT0_OFFSET + 8 * usize::from(index)) as *mut EgclVal }
    })
}

/// Read slot `(depth, index)` from `env`. `None` for a malformed address. The
/// value may be `UNBOUND` when the binding has not been made.
#[inline]
pub fn load(env: EgclVal, depth: u8, index: u16) -> Option<EgclVal> {
    // SAFETY: an in-bounds slot address.
    slot_address(env, depth, index).map(|slot| unsafe { *slot })
}

/// Write slot `(depth, index)` of `env` through the write barrier. False for
/// a malformed address; nothing was written.
#[inline]
pub fn store(env: EgclVal, depth: u8, index: u16, value: EgclVal) -> bool {
    match slot_address(env, depth, index) {
        Some(slot) => {
            // SAFETY: an in-bounds reference slot of a live heap object.
            unsafe { crate::gc::store_ref(slot, value) };
            true
        }
        None => false,
    }
}

/// A NIL-or-environment check for a parent operand.
#[inline]
pub fn valid_parent(value: EgclVal) -> bool {
    value == NIL || is_env(value)
}
