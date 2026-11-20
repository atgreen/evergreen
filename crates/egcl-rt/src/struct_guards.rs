// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Flags that say whether a DEFSTRUCT function inlined at compiled call sites
//! still has DEFSTRUCT's definition. A site reads its flag and performs the
//! structure access while it is T, and calls the function once it is NIL.
//!
//! Each flag belongs to a guard symbol, which is what bytecode serializes; the
//! dense id is assigned in this process when the symbol is first seen, so
//! every tier reads the flag with one load from a fixed address: no lock,
//! hash or binding lookup. A flag starts NIL: until DEFSTRUCT records the
//! definition in this process, the site calls.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::fxhash::FxBuildHasher;
use crate::value::{EgclVal, NIL, T};

/// Guards beyond this many leave further sites calling (no id is assigned).
pub const CAPACITY: usize = 1 << 16;

#[allow(clippy::declare_interior_mutable_const)]
const UNSET: AtomicU64 = AtomicU64::new(NIL.0);
static FLAGS: [AtomicU64; CAPACITY] = [UNSET; CAPACITY];
static IDS: Mutex<Option<HashMap<u32, u32, FxBuildHasher>>> = Mutex::new(None);

/// The id of guard symbol `symbol`, assigning one on first use.
pub fn id_for(symbol: u32) -> Option<u32> {
    let mut ids = IDS.lock().unwrap();
    let ids = ids.get_or_insert_with(HashMap::default);
    if let Some(&id) = ids.get(&symbol) {
        return Some(id);
    }
    let id = u32::try_from(ids.len())
        .ok()
        .filter(|&id| (id as usize) < CAPACITY)?;
    ids.insert(symbol, id);
    Some(id)
}

/// Set guard `id` to T (`valid`) or NIL.
pub fn set(id: u32, valid: bool) {
    if let Some(flag) = FLAGS.get(id as usize) {
        flag.store(if valid { T.0 } else { NIL.0 }, Ordering::Release);
    }
}

/// Guard `id`'s current value, T or NIL.
pub fn get(id: u32) -> EgclVal {
    FLAGS
        .get(id as usize)
        .map_or(NIL, |flag| EgclVal(flag.load(Ordering::Acquire)))
}

/// The address of guard `id`'s tagged T/NIL word, for native code.
pub fn address(id: u32) -> Option<u64> {
    FLAGS.get(id as usize).map(|flag| flag.as_ptr() as u64)
}
