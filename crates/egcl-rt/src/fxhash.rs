// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A small, fast, deterministic FxHash-style hasher for internal maps whose
//! keys are short byte strings or integers (symbol names, interned string
//! bytes, indices).
//!
//! The std default `HashMap` uses SipHash — strong but slow, and it processes
//! every key byte. Interning tables that grow to tens of thousands of entries
//! during a large load (babel) spent a large share of the load in `hash_one` +
//! `reserve_rehash` under SipHash (bliss-pohq). FxHash is a cheap multiplicative
//! hash, and its fixed seed also removes the per-process run-to-run
//! nondeterminism SipHash's random seed introduced (bliss-nad).
//!
//! Use with `HashMap<K, V, FxBuildHasher>`; pre-size large interning tables with
//! `with_capacity_and_hasher` to avoid the rehash storm as they grow.

use core::hash::{BuildHasherDefault, Hasher};

/// FxHash-style multiplicative hasher (8 bytes at a time).
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, i: u64) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            self.add(u64::from_le_bytes(bytes[..8].try_into().unwrap()));
            bytes = &bytes[8..];
        }
        for &b in bytes {
            self.add(b as u64);
        }
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.add(i as u64);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// `BuildHasher` for [`FxHasher`] — use as the third `HashMap`/`HashSet` param.
pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
