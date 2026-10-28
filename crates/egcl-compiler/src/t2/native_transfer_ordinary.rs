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

/// One entry of a FUNCALL probe table (bliss-6m6ca). Every word is raw: the
/// runtime clears the table's meaning at each collection by moving the
/// `collections` stamp, so `function` is compared, never dereferenced.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OrdinaryFuncallProbeEntry {
    /// Bits of the function object this entry selects for.
    pub function: u64,
    /// `egcl_rt::call_table::invalidation_epoch` when the entry was filled.
    pub epoch: u64,
    /// The runtime's collection stamp when the entry was filled.
    pub collections: u64,
    /// The target cell's revision word, and the revision of `descriptor`.
    pub revision_address: u64,
    pub revision: u64,
    /// The target cell's published `OrdinaryEntryDescriptor`.
    pub descriptor: u64,
}

pub const ORDINARY_FUNCALL_PROBE_ENTRIES: usize = 4;

/// Addresses a FUNCALL probe reads; each outlives the probe's code.
#[derive(Clone, Copy)]
pub struct OrdinaryFuncallProbe {
    /// `[OrdinaryFuncallProbeEntry; ORDINARY_FUNCALL_PROBE_ENTRIES]`.
    pub table: u64,
    /// The invalidation epoch word.
    pub epoch: u64,
    /// The collection stamp word.
    pub collections: u64,
    /// Offset of the active native control in the R13 execution context.
    pub active_control: i32,
    /// Offset of `stack_limit` in that control.
    pub stack_limit: u8,
}

/// The ordinary entry of the builtin FUNCALL cell: `(funcall f a0 a1 a2)`
/// arrives as RDI=f, RSI=a0, RDX=a1, [RSP+8]=a2 and RCX=count. When `f` is a
/// function object the runtime recorded in `table` for the current
/// invalidation epoch, collection and target cell revision, the probe
/// re-checks the target descriptor's arity and stack bound, shifts the
/// arguments down one place and tail-jumps to the target's ordinary entry,
/// exactly as a named call to it would enter. Otherwise, with every incoming
/// register and stack word intact, it falls through to the bytes the caller
/// appends: the cell's published runtime veneer. The probe is a leaf with no
/// safepoint; it clobbers only R8-R11 before deciding.
pub fn emit_ordinary_funcall_probe(probe: OrdinaryFuncallProbe) -> Vec<u8> {
    use egcl_rt::call_table::{OrdinaryEntryDescriptor, ORDINARY_ARITY_ANY};
    const STRIDE: usize = size_of::<OrdinaryFuncallProbeEntry>();
    fn mem(a: &mut Asm, rex: u8, op: u8, reg: u8, base: u8, disp: i32) {
        // [base + disp32]; base is never RSP/R12 here.
        a.extend_from_slice(&[rex, op, 0x80 | ((reg & 7) << 3) | (base & 7)]);
        a.extend_from_slice(&disp.to_le_bytes());
    }
    fn unsigned_branch(a: &mut Asm, opcode: u8, target: egcl_rt::asm::Label) {
        let at = a.here();
        a.jcc(Cc::E, target);
        a.patch_u8(at + 1, opcode); // 0x86 JBE, 0x82 JB
    }
    let mut a = Asm::new();
    let miss = a.label();
    let hit = a.label();
    a.extend_from_slice(&[0x48, 0x83, 0xf9, 0x01]); // cmp rcx,1
    a.jcc(Cc::L, miss);
    a.extend_from_slice(&[0x48, 0x83, 0xf9, 0x04]); // cmp rcx,4
    a.jcc(Cc::G, miss);
    a.extend_from_slice(&[0x49, 0xba]); // mov r10, table
    a.extend_from_slice(&probe.table.to_le_bytes());
    a.extend_from_slice(&[0x49, 0xb9]); // mov r9, &epoch
    a.extend_from_slice(&probe.epoch.to_le_bytes());
    a.extend_from_slice(&[0x4d, 0x8b, 0x09]); // mov r9,[r9]
    a.extend_from_slice(&[0x49, 0xb8]); // mov r8, &collections
    a.extend_from_slice(&probe.collections.to_le_bytes());
    a.extend_from_slice(&[0x4d, 0x8b, 0x00]); // mov r8,[r8]
    for index in 0..ORDINARY_FUNCALL_PROBE_ENTRIES {
        let next = a.label();
        let field = |offset: usize| (index * STRIDE + offset) as i32;
        mem(&mut a, 0x49, 0x3b, 7, 2, field(offset_of!(OrdinaryFuncallProbeEntry, function))); // cmp rdi,[r10+]
        a.jcc(Cc::Ne, next);
        mem(&mut a, 0x4d, 0x3b, 9, 2, field(offset_of!(OrdinaryFuncallProbeEntry, epoch))); // cmp r9,[r10+]
        a.jcc(Cc::Ne, next);
        mem(&mut a, 0x4d, 0x3b, 8, 2, field(offset_of!(OrdinaryFuncallProbeEntry, collections))); // cmp r8,[r10+]
        a.jcc(Cc::Ne, next);
        mem(&mut a, 0x4d, 0x8b, 11, 2, field(offset_of!(OrdinaryFuncallProbeEntry, revision_address))); // mov r11,[r10+]
        a.extend_from_slice(&[0x4d, 0x8b, 0x1b]); // mov r11,[r11]
        mem(&mut a, 0x4d, 0x3b, 11, 2, field(offset_of!(OrdinaryFuncallProbeEntry, revision))); // cmp r11,[r10+]
        a.jcc(Cc::Ne, next);
        // The revision is unchanged, so the cell still publishes, and its
        // execution still retains, this descriptor.
        mem(&mut a, 0x4d, 0x8b, 11, 2, field(offset_of!(OrdinaryFuncallProbeEntry, descriptor))); // mov r11,[r10+]
        a.jmp(hit);
        a.bind(next);
    }
    a.jmp(miss);
    a.bind(hit);
    // Arity: count-1 actuals, or a descriptor that checks its own count.
    let arity_ok = a.label();
    a.extend_from_slice(&[0x4c, 0x8d, 0x51, 0xff]); // lea r10,[rcx-1]
    mem(&mut a, 0x4d, 0x3b, 10, 3, offset_of!(OrdinaryEntryDescriptor, arity) as i32); // cmp r10,[r11+arity]
    a.jcc(Cc::E, arity_ok);
    debug_assert_eq!(ORDINARY_ARITY_ANY, usize::MAX);
    a.extend_from_slice(&[0x49, 0x83, 0xbb]); // cmp qword [r11+arity], -1
    a.extend_from_slice(&(offset_of!(OrdinaryEntryDescriptor, arity) as i32).to_le_bytes());
    a.extend_from_slice(&[0xff]);
    a.jcc(Cc::Ne, miss);
    a.bind(arity_ok);
    // The target runs on this frame's stack: its own bound must fit above
    // the execution's limit, as a named caller checks before its CALL.
    mem(&mut a, 0x4d, 0x8b, 10, 5, probe.active_control); // mov r10,[r13+control]
    a.extend_from_slice(&[0x4d, 0x85, 0xd2]); // test r10,r10
    a.jcc(Cc::E, miss);
    a.extend_from_slice(&[0x49, 0x89, 0xe1]); // mov r9,rsp
    mem(&mut a, 0x4d, 0x2b, 9, 3, offset_of!(OrdinaryEntryDescriptor, stack_required) as i32); // sub r9,[r11+..]
    unsigned_branch(&mut a, 0x82, miss); // jb: wrapped
    a.extend_from_slice(&[0x4d, 0x3b, 0x4a, probe.stack_limit]); // cmp r9,[r10+limit]
    unsigned_branch(&mut a, 0x86, miss); // jbe
    // Commit: drop the function, shift the actuals into place.
    let no_overflow = a.label();
    a.extend_from_slice(&[0x48, 0x89, 0xf7]); // mov rdi,rsi
    a.extend_from_slice(&[0x48, 0x89, 0xd6]); // mov rsi,rdx
    a.extend_from_slice(&[0x48, 0x83, 0xf9, 0x04]); // cmp rcx,4
    a.jcc(Cc::Ne, no_overflow);
    a.extend_from_slice(&[0x48, 0x8b, 0x54, 0x24, 0x08]); // mov rdx,[rsp+8]
    a.bind(no_overflow);
    a.extend_from_slice(&[0x48, 0xff, 0xc9]); // dec rcx
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax, NIL: no closure
    a.extend_from_slice(&NIL.0.to_le_bytes());
    let entry = offset_of!(OrdinaryEntryDescriptor, entry) as i32;
    a.extend_from_slice(&[0x41, 0xff, 0xa3]); // jmp [r11+entry]
    a.extend_from_slice(&entry.to_le_bytes());
    a.bind(miss);
    a.finish().expect("ordinary FUNCALL probe labels")
}
