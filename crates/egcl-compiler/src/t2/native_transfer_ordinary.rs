// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Collecting boundaries for the ordinary Lisp register ABI.

use super::{capture_stack_word, SysvTransferCapture};
use egcl_rt::asm::{Asm, Cc};
use egcl_rt::native_transfer::{NativeOutcome, NativeReturnValues, NativeSegment};
use egcl_rt::value::{EgclVal, NIL};
use std::mem::{offset_of, size_of};

/// Helper output remains unrooted after the callback returns: no safepoint,
/// allocation, yield or MV mutation may intervene before assembly consumes it.
#[repr(C)]
pub struct OrdinaryHelperOutcome {
    pub outcome: NativeOutcome,
    pub values: NativeReturnValues,
    /// Active segment to leave after an exceptional helper result. The helper
    /// must settle roots, depth and cleanup obligations before returning.
    pub anchor: *mut NativeSegment,
}

/// Actual incoming arguments captured before the callee establishes a frame.
/// Only min(nargs,3) register words are Lisp roots; the remaining actuals start
/// at overflow_args. The runtime must root these writable locations and closure
/// before allocating a count-dependent copy or building an arity condition.
#[repr(C)]
pub struct OrdinaryEntryRequest {
    pub closure: EgclVal,
    pub nargs: usize,
    pub register_args: [EgclVal; 3],
    pub overflow_args: *mut EgclVal,
    /// Immutable entry-specific data: expected arity or named target ordinal.
    pub entry_data: usize,
    pub reserved: u64,
}

/// Compatibility name for the arity-checking use of incoming capture.
pub type OrdinaryArityRequest = OrdinaryEntryRequest;

/// The callback publishes the caller image, roots request-specific incoming
/// values, and initializes the complete outcome. It must return normally even
/// for Lisp errors: only the subsequent assembly may leave the segment.
pub type OrdinaryPublishedHelper =
    unsafe extern "C" fn(*mut u8, *mut OrdinaryHelperOutcome, *mut SysvTransferCapture);

/// Capture a mapped caller before a collecting ordinary-call fallback.
/// RDI points to its OrdinaryColdRequest. Retain the callback while executable.
pub fn emit_ordinary_published_veneer(helper: OrdinaryPublishedHelper) -> Vec<u8> {
    emit(helper, None)
}

/// Entered by a pre-prologue tail jump with all actual Lisp argument registers
/// unchanged. No callee frame exists; the captured return PC belongs to its
/// caller (or the outer segment entry shim).
pub fn emit_ordinary_arity_veneer(helper: OrdinaryPublishedHelper, expected: usize) -> Vec<u8> {
    emit_ordinary_entry_veneer(helper, expected)
}

/// Capture every actual argument before a binding or interpreter adapter runs.
pub fn emit_ordinary_entry_veneer(helper: OrdinaryPublishedHelper, entry_data: usize) -> Vec<u8> {
    emit(helper, Some(entry_data))
}

fn stack_address(a: &mut Asm, reg: u8, offset: usize) {
    a.extend_from_slice(&[
        0x48 | ((reg >> 3) << 2),
        0x8d,
        0x84 | ((reg & 7) << 3),
        0x24,
    ]);
    a.extend_from_slice(&(offset as u32).to_le_bytes());
}

fn emit(helper: OrdinaryPublishedHelper, expected: Option<usize>) -> Vec<u8> {
    const OUT: usize = size_of::<SysvTransferCapture>();
    const REQUEST: usize = OUT + size_of::<OrdinaryHelperOutcome>();
    const {
        assert!(OUT == 88);
        assert!(size_of::<OrdinaryHelperOutcome>() == 40);
        assert!(size_of::<OrdinaryEntryRequest>() == 64);
        assert!(offset_of!(OrdinaryHelperOutcome, values) == 16);
        assert!(offset_of!(OrdinaryHelperOutcome, anchor) == 32);
        assert!(offset_of!(OrdinaryEntryRequest, overflow_args) == 40);
        assert!(offset_of!(OrdinaryEntryRequest, entry_data) == 48);
    }
    let size = REQUEST + if expected.is_some() { 64 } else { 0 } + 8;
    let mut a = Asm::new();
    let exceptional = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x81, 0xec]);
    a.extend_from_slice(&(size as u32).to_le_bytes());
    if let Some(expected) = expected {
        for (reg, offset) in [(0, 0), (1, 8), (7, 16), (6, 24), (2, 32)] {
            capture_stack_word(&mut a, false, reg, REQUEST + offset);
        }
        stack_address(&mut a, 0, size + 8);
        capture_stack_word(&mut a, false, 0, REQUEST + 40);
        a.extend_from_slice(&[0x48, 0xb8]);
        a.extend_from_slice(&(expected as u64).to_le_bytes());
        capture_stack_word(&mut a, false, 0, REQUEST + 48);
        a.extend_from_slice(&[0x31, 0xc0]);
        capture_stack_word(&mut a, false, 0, REQUEST + 56);
        stack_address(&mut a, 7, REQUEST);
    }
    capture_stack_word(&mut a, false, 7, offset_of!(SysvTransferCapture, request));
    let preserved = offset_of!(SysvTransferCapture, preserved);
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, false, reg, preserved + index * 8);
    }
    stack_address(&mut a, 0, size + 8);
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, caller_sp));
    capture_stack_word(&mut a, true, 0, size);
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, return_pc));
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&NIL.to_raw().to_le_bytes());
    for offset in [offset_of!(SysvTransferCapture, value), OUT] {
        capture_stack_word(&mut a, false, 0, offset);
    }
    a.extend_from_slice(&[0x31, 0xc0]);
    for offset in [
        offset_of!(SysvTransferCapture, exit),
        OUT + 8,
        OUT + 24,
        OUT + 32,
    ] {
        capture_stack_word(&mut a, false, 0, offset);
    }
    a.extend_from_slice(&[0xb8, 1, 0, 0, 0]);
    capture_stack_word(&mut a, false, 0, OUT + 16);
    stack_address(&mut a, 6, OUT);
    a.extend_from_slice(&[0x48, 0x89, 0xe2, 0x48, 0xb8]);
    a.extend_from_slice(&(helper as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]);
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, true, reg, preserved + index * 8);
    }
    capture_stack_word(&mut a, true, 0, OUT);
    capture_stack_word(&mut a, true, 2, OUT + 8);
    a.extend_from_slice(&[0x48, 0x85, 0xd2]);
    a.jcc(Cc::Ne, exceptional);
    capture_stack_word(&mut a, true, 2, OUT + 16);
    capture_stack_word(&mut a, true, 1, OUT + 24);
    stack_address(&mut a, 4, size);
    crate::t2::native_return::emit_tuple_return(&mut a);
    a.bind(exceptional);
    capture_stack_word(&mut a, true, 7, OUT + 32);
    a.extend_from_slice(&[0x48, 0x89, 0xc6]);
    stack_address(&mut a, 4, size);
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(
        &(egcl_rt::native_transfer::leave_native_segment as *const () as u64).to_le_bytes(),
    );
    a.extend_from_slice(&[0xff, 0xe0]);
    a.finish().expect("ordinary helper veneer labels")
}
