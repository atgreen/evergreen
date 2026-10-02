// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Composite off-heap-body image hooks (bliss-x0f2 M3).
//!
//! egcl-rt's image loader accepts ONE (serialize, allocate, populate) hook
//! triple for the OffHeap section, but more than one stdlib subsystem keeps
//! Lisp VALUES with off-heap bodies: hash tables (`hashtable`) and pathnames
//! (`pathnames`). These wrappers frame each subsystem's bytes as
//! `[len u64][bytes]` sub-blocks, in fixed order, so the section stays a
//! single opaque blob to egcl-rt while each subsystem keeps its own format.

fn put_block(out: &mut Vec<u8>, block: Vec<u8>) {
    out.extend_from_slice(&(block.len() as u64).to_le_bytes());
    out.extend_from_slice(&block);
}

fn get_block<'a>(data: &'a [u8], off: &mut usize) -> Option<&'a [u8]> {
    if data.len() < *off + 8 {
        return None;
    }
    let len = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap()) as usize;
    *off += 8;
    if data.len() < *off + len {
        return None;
    }
    let block = &data[*off..*off + len];
    *off += len;
    Some(block)
}

/// Serialize every off-heap-body subsystem for a core image. GC-safe: each
/// sub-serializer reads raw words only, no EGCL allocation.
pub fn serialize_all() -> Vec<u8> {
    let mut out = Vec::new();
    put_block(&mut out, crate::streams::serialize_interned_strings());
    put_block(&mut out, crate::hashtable::serialize_live_tables());
    put_block(&mut out, crate::pathnames::serialize_pathnames());
    out
}

/// Phase 1: allocate empty bodies for every subsystem; the combined
/// (old, new) pairs are folded into the heap reloc map before Pass 2.
/// Interned strings go first so every later subsystem (and Pass 2 itself)
/// sees them in the fold.
pub fn allocate_all(data: &[u8]) -> Vec<(usize, usize)> {
    let mut off = 0usize;
    let mut pairs = Vec::new();
    if let Some(block) = get_block(data, &mut off) {
        pairs.extend(crate::streams::allocate_interned_strings(block));
    }
    if let Some(block) = get_block(data, &mut off) {
        pairs.extend(crate::hashtable::allocate_live_tables(block));
    }
    if let Some(block) = get_block(data, &mut off) {
        pairs.extend(crate::pathnames::allocate_pathnames(block));
    }
    pairs
}

/// Phase 2: fill every subsystem's bodies once the final remap exists.
/// Strings first: the later populates intern via `make_lisp_string` and must
/// dedup against the restored table entries.
pub fn populate_all(remap: &dyn Fn(u64) -> u64) {
    crate::streams::populate_interned_strings();
    crate::pathnames::populate_pathnames(remap);
    // EQUAL/EQUALP keys can contain pathnames, including inside conses and
    // vectors. Their content hashes require the restored pathname records.
    crate::hashtable::populate_live_tables(remap);
}
