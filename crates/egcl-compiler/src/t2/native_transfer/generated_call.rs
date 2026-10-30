// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Machine-only contract for a prepared mapped-to-ordinary lane.
//! Every pointer names retained execution-owned storage; no Rust container,
//! enum or ownership layout is interpreted by emitted instructions.
use super::MappedCallRecord;
use crate::t2::emit::NativeRecursiveControl;
use egcl_rt::call_table::OrdinaryEntryDescriptor;
use egcl_rt::exec_context::LispExecutionContext;
use egcl_rt::native_transfer::{GeneratedSegmentBindings, NativeSegment};
use egcl_rt::value::EgclVal;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicUsize};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GeneratedPublication {
    pub active: *mut usize,
    pub boundary: usize,
    pub previous: *mut usize,
    pub pc: *mut usize,
    pub sp: *mut usize,
    pub registers: [*mut usize; 6],
    pub low: *mut usize,
    pub high: *mut usize,
    pub segment: *mut *mut NativeSegment,
}

#[repr(C)]
pub struct GeneratedLane {
    pub cell: u64,
    pub descriptor_word: *const AtomicUsize,
    pub descriptor: *const OrdinaryEntryDescriptor,
    pub revision_word: *const AtomicU64,
    pub revision: u64,
    pub recompile: *const bool,
    pub function: *const EgclVal,
    pub context: *mut u8,
    pub control: *mut NativeRecursiveControl,
    pub busy: *mut bool,
    pub entry: *const u8,
    pub arity: usize,
    pub stack_required: usize,
}

/// A CaptureContext has a repr(C) first field pointing to this contract. A
/// null prefix means the old mapped dispatcher remains authoritative.
#[repr(C)]
pub struct GeneratedActivation {
    pub lanes: [*const GeneratedLane; 4],
    pub execution: *const LispExecutionContext,
    pub env: *mut u8,
    pub code_start: usize,
    pub code_end: usize,
    pub runtime: GeneratedSegmentBindings,
    pub depth: *mut u32,
    pub publication: GeneratedPublication,
    pub fast_entries: *mut usize,
    pub total_entries: *mut usize,
}

/// Private Rust scope guards occupy opaque aligned storage; the runtime checks
/// their exact size/alignment. Assembly never reads a Vec or its components.
#[repr(C, align(8))]
pub struct GeneratedScopeStorage(pub [MaybeUninit<usize>; 16]);

#[repr(C)]
pub struct GeneratedCallRecord {
    pub capture_slot: *mut usize,
    pub activation: *const GeneratedActivation,
    pub lane: *const GeneratedLane,
    pub previous_capture: usize,
    pub previous_control: u64,
    pub previous_segment: *mut NativeSegment,
    pub previous_publication: usize,
    pub previous_depth: u32,
    pub entered: u32,
    pub segment: MaybeUninit<NativeSegment>,
    pub scope: GeneratedScopeStorage,
    pub mxcsr: u32,
    pub fpcw: u16,
    pub reserved: u16,
    pub current_mxcsr: u32,
    pub current_fpcw: u16,
    pub pad: u16,
}

/// Both callbacks are bounded nonallocating scope/anchor operations. The leave
/// callback may run the existing rooted pending-return slow path if needed.
#[derive(Clone, Copy)]
pub struct GeneratedCallHelpers {
    pub enter: unsafe extern "C" fn(*mut MappedCallRecord),
    pub leave: unsafe extern "C" fn(*mut MappedCallRecord),
}

use egcl_rt::asm::{Asm, Cc, Label};
use std::mem::offset_of;

// Fixed-width address forms handle both RSP and R12 with an explicit SIB.
fn memory(a: &mut Asm, op: &[u8], wide: bool, reg: u8, base: u8, disp: usize) {
    a.push(0x40 | if wide { 8 } else { 0 } | ((reg >> 3) << 2) | (base >> 3));
    a.extend_from_slice(op);
    a.push(0x80 | ((reg & 7) << 3) | (base & 7));
    if base & 7 == 4 {
        a.push(0x24);
    }
    a.extend_from_slice(&(disp as i32).to_le_bytes());
}
fn load(a: &mut Asm, dst: u8, base: u8, disp: usize) {
    memory(a, &[0x8b], true, dst, base, disp);
}
fn store(a: &mut Asm, base: u8, disp: usize, src: u8) {
    memory(a, &[0x89], true, src, base, disp);
}
fn lea(a: &mut Asm, dst: u8, base: u8, disp: usize) {
    memory(a, &[0x8d], true, dst, base, disp);
}
fn reg(a: &mut Asm, dst: u8, src: u8) {
    a.extend_from_slice(&[
        0x48 | ((src >> 3) << 2) | (dst >> 3),
        0x89,
        0xc0 | ((src & 7) << 3) | (dst & 7),
    ]);
}
fn imm(a: &mut Asm, dst: u8, value: u64) {
    a.extend_from_slice(&[0x48 | (dst >> 3), 0xb8 | (dst & 7)]);
    a.extend_from_slice(&value.to_le_bytes());
}
fn alu(a: &mut Asm, operation: u8, dst: u8, value: i32) {
    a.extend_from_slice(&[0x48 | (dst >> 3), 0x81, 0xc0 | (operation << 3) | (dst & 7)]);
    a.extend_from_slice(&value.to_le_bytes());
}
fn cmp(a: &mut Asm, lhs: u8, rhs: u8) {
    a.extend_from_slice(&[
        0x48 | ((rhs >> 3) << 2) | (lhs >> 3),
        0x39,
        0xc0 | ((rhs & 7) << 3) | (lhs & 7),
    ]);
}
fn xchg(a: &mut Asm, address: u8) {
    memory(a, &[0x87], true, 0, address, 0);
}
fn load32(a: &mut Asm, dst: u8, base: u8, disp: usize) {
    memory(a, &[0x8b], false, dst, base, disp);
}
fn store32(a: &mut Asm, base: u8, disp: usize, src: u8) {
    memory(a, &[0x89], false, src, base, disp);
}
fn byte(a: &mut Asm, address: u8, value: u8) {
    memory(a, &[0xc6], false, 0, address, 0);
    a.push(value);
}
const TAIL: usize = offset_of!(MappedCallRecord, generated);
fn word(a: &mut Asm, dst: u8, off: usize) {
    load(a, dst, 4, TAIL + off);
}
fn save(a: &mut Asm, off: usize, src: u8) {
    store(a, 4, TAIL + off, src);
}
fn poll(a: &mut Asm, execution: u8, slow: Label) {
    load(a, 10, execution, egcl_rt::exec_context::offsets::POLL_SLOT);
    load(a, 0, 10, 0);
    alu(a, 7, 0, 0);
    a.jcc(Cc::Ne, slow);
}

/// Insert a hot branch after the adapter has saved its caller registers. A
/// miss falls through untouched to the existing preparation path. R11's
/// execution-local capture-cell address was saved before argument marshalling.
pub(super) fn emit_hot_branch(
    a: &mut Asm,
    helpers: GeneratedCallHelpers,
    returned: Label,
    escape: Label,
) {
    use egcl_rt::native_transfer::GENERATED_SEGMENT_LAYOUT as S;
    let cold = a.label();
    let found = a.label();
    let native_return = a.label();
    let native_escape = a.label();
    let cleanup = a.label();
    word(a, 11, offset_of!(GeneratedCallRecord, capture_slot));
    alu(a, 7, 11, 0);
    a.jcc(Cc::E, cold);
    load(a, 0, 11, 0);
    alu(a, 7, 0, 0);
    a.jcc(Cc::E, cold);
    save(a, offset_of!(GeneratedCallRecord, previous_capture), 0);
    load(a, 12, 0, 0);
    alu(a, 7, 12, 0);
    a.jcc(Cc::E, cold);
    save(a, offset_of!(GeneratedCallRecord, activation), 12);
    // Only an actual mapped machine caller may use the prepared activation.
    load(a, 0, 4, super::ADAPTER_FRAME_BYTES);
    load(a, 10, 12, offset_of!(GeneratedActivation, code_start));
    cmp(a, 0, 10);
    a.jcc(Cc::B, cold);
    load(a, 10, 12, offset_of!(GeneratedActivation, code_end));
    cmp(a, 0, 10);
    a.jcc(Cc::Ae, cold);
    load(a, 11, 12, offset_of!(GeneratedActivation, execution));
    poll(a, 11, cold);
    for index in 0..4 {
        let next = a.label();
        load(
            a,
            14,
            12,
            offset_of!(GeneratedActivation, lanes) + index * 8,
        );
        alu(a, 7, 14, 0);
        a.jcc(Cc::E, next);
        load(a, 0, 14, offset_of!(GeneratedLane, cell));
        load(a, 10, 4, offset_of!(MappedCallRecord, target));
        cmp(a, 0, 10);
        a.jcc(Cc::E, found);
        a.bind(next);
    }
    a.jmp(cold);
    a.bind(found);
    load(a, 11, 14, offset_of!(GeneratedLane, revision_word));
    load(a, 0, 11, 0);
    load(a, 10, 14, offset_of!(GeneratedLane, revision));
    cmp(a, 0, 10);
    a.jcc(Cc::Ne, cold);
    load(a, 11, 14, offset_of!(GeneratedLane, descriptor_word));
    load(a, 0, 11, 0);
    load(a, 10, 14, offset_of!(GeneratedLane, descriptor));
    cmp(a, 0, 10);
    a.jcc(Cc::Ne, cold);
    for field in [
        offset_of!(GeneratedLane, busy),
        offset_of!(GeneratedLane, recompile),
    ] {
        load(a, 11, 14, field);
        memory(a, &[0x0f, 0xb6], true, 0, 11, 0);
        alu(a, 7, 0, 0);
        a.jcc(Cc::Ne, cold);
    }
    load(a, 11, 4, offset_of!(MappedCallRecord, context));
    load(a, 0, 11, 24);
    load(a, 10, 14, offset_of!(GeneratedLane, arity));
    cmp(a, 0, 10);
    a.jcc(Cc::Ne, cold);
    load(a, 11, 12, offset_of!(GeneratedActivation, depth));
    load32(a, 0, 11, 0);
    save(a, offset_of!(GeneratedCallRecord, previous_depth), 0);
    load(a, 11, 14, offset_of!(GeneratedLane, control));
    load32(a, 10, 11, offset_of!(NativeRecursiveControl, depth_limit));
    cmp(a, 0, 10);
    a.jcc(Cc::Ae, cold);
    // Conservative overflow-area reservation, bounded by the admitted arity.
    load(a, 0, 14, offset_of!(GeneratedLane, arity));
    alu(a, 0, 0, 2);
    a.extend_from_slice(&[0x48, 0xc1, 0xe0, 3]);
    load(a, 10, 14, offset_of!(GeneratedLane, stack_required));
    alu(a, 0, 10, 4096);
    a.extend_from_slice(&[0x49, 0x01, 0xc2]); // add r10,rax
    reg(a, 0, 4);
    a.extend_from_slice(&[0x4c, 0x29, 0xd0]); // sub rax,r10
    a.jcc(Cc::B, cold);
    load(a, 10, 11, offset_of!(NativeRecursiveControl, stack_limit));
    cmp(a, 0, 10);
    a.jcc(Cc::Be, cold);
    save(a, offset_of!(GeneratedCallRecord, lane), 14);
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, runtime) + offset_of!(GeneratedSegmentBindings, active),
    );
    load(a, 0, 11, 0);
    alu(a, 7, 0, 0);
    a.jcc(Cc::E, cold);
    save(a, offset_of!(GeneratedCallRecord, previous_segment), 0);
    // The helper projects a typed record reference. Initialize every plain
    // scalar before that reference exists, including fields populated later
    // by the machine transaction. Segment/scope explicitly allow uninit bytes.
    imm(a, 0, 0);
    for offset in [
        offset_of!(GeneratedCallRecord, previous_control),
        offset_of!(GeneratedCallRecord, previous_publication),
        offset_of!(GeneratedCallRecord, mxcsr),
        offset_of!(GeneratedCallRecord, current_mxcsr),
    ] {
        save(a, offset, 0);
    }
    reg(a, 7, 4);
    imm(a, 0, helpers.enter as usize as u64);
    a.extend_from_slice(&[0xff, 0xd0]);
    // The nonallocating helper constructed the opaque segment and lexical guard.
    lea(a, 15, 12, offset_of!(GeneratedActivation, publication));
    load(a, 11, 15, offset_of!(GeneratedPublication, pc));
    load(a, 0, 4, super::ADAPTER_FRAME_BYTES);
    store(a, 11, 0, 0);
    load(a, 11, 15, offset_of!(GeneratedPublication, sp));
    lea(a, 0, 4, super::ADAPTER_FRAME_BYTES + 8);
    store(a, 11, 0, 0);
    for index in 0..6 {
        load(
            a,
            11,
            15,
            offset_of!(GeneratedPublication, registers) + index * 8,
        );
        lea(a, 0, 4, offset_of!(MappedCallRecord, preserved) + index * 8);
        store(a, 11, 0, 0);
    }
    load(a, 11, 15, offset_of!(GeneratedPublication, low));
    reg(a, 0, 4);
    store(a, 11, 0, 0);
    word(a, 0, offset_of!(GeneratedCallRecord, previous_segment));
    load(a, 10, 0, S.saved_sp);
    load(a, 11, 15, offset_of!(GeneratedPublication, high));
    store(a, 11, 0, 10);
    load(a, 11, 15, offset_of!(GeneratedPublication, segment));
    store(a, 11, 0, 0);
    load(a, 11, 15, offset_of!(GeneratedPublication, active));
    load(a, 0, 11, 0);
    save(a, offset_of!(GeneratedCallRecord, previous_publication), 0);
    load(a, 10, 15, offset_of!(GeneratedPublication, previous));
    store(a, 10, 0, 0);
    load(a, 0, 15, offset_of!(GeneratedPublication, boundary));
    store(a, 11, 0, 0);
    word(a, 11, offset_of!(GeneratedCallRecord, capture_slot));
    load(a, 0, 14, offset_of!(GeneratedLane, context));
    store(a, 11, 0, 0);
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, runtime) + offset_of!(GeneratedSegmentBindings, control),
    );
    load(a, 0, 14, offset_of!(GeneratedLane, control));
    xchg(a, 11);
    save(a, offset_of!(GeneratedCallRecord, previous_control), 0);
    word(a, 0, offset_of!(GeneratedCallRecord, previous_depth));
    alu(a, 0, 0, 1);
    load(a, 11, 12, offset_of!(GeneratedActivation, depth));
    store32(a, 11, 0, 0);
    load(a, 11, 14, offset_of!(GeneratedLane, control));
    store32(a, 11, offset_of!(NativeRecursiveControl, anchor_depth), 0);
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, runtime) + offset_of!(GeneratedSegmentBindings, active),
    );
    lea(a, 0, 4, TAIL + offset_of!(GeneratedCallRecord, segment));
    store(a, 11, 0, 0);
    lea(a, 11, 4, TAIL + offset_of!(GeneratedCallRecord, segment));
    reg(a, 0, 4);
    store(a, 11, S.landing_sp, 0);
    a.lea_rip_label(0, native_escape);
    store(a, 11, S.landing_pc, 0);
    a.lea_rip_label(0, native_return);
    store(a, 11, S.entry_return_pc, 0);
    a.extend_from_slice(&[0x0f, 0xae, 0x9c, 0x24]);
    a.extend_from_slice(&((TAIL + offset_of!(GeneratedCallRecord, mxcsr)) as u32).to_le_bytes());
    a.extend_from_slice(&[0xd9, 0xbc, 0x24]);
    a.extend_from_slice(&((TAIL + offset_of!(GeneratedCallRecord, fpcw)) as u32).to_le_bytes());
    reg(a, 5, 4); // stable record base, preserved by ordinary callees
    // Allocate/probe the aligned outgoing area, as ordinary_x64::enter does.
    load(a, 0, 14, offset_of!(GeneratedLane, arity));
    alu(a, 7, 0, 3);
    let no_overflow = a.label();
    a.jcc(Cc::Be, no_overflow);
    alu(a, 5, 0, 3);
    a.extend_from_slice(&[0x48, 0xc1, 0xe0, 3]);
    alu(a, 0, 0, 15);
    alu(a, 4, 0, -16);
    reg(a, 10, 4);
    a.extend_from_slice(&[0x49, 0x29, 0xc2]);
    let probe = a.label();
    let last = a.label();
    a.bind(probe);
    reg(a, 0, 4);
    a.extend_from_slice(&[0x4c, 0x29, 0xd0]);
    alu(a, 7, 0, 4096);
    a.jcc(Cc::B, last);
    alu(a, 5, 4, 4096);
    a.extend_from_slice(&[0xf6, 0x04, 0x24, 0]);
    a.jmp(probe);
    a.bind(last);
    reg(a, 4, 10);
    a.extend_from_slice(&[0xf6, 0x04, 0x24, 0]);
    a.bind(no_overflow);
    lea(a, 11, 5, TAIL + offset_of!(GeneratedCallRecord, segment));
    store(a, 11, S.saved_sp, 4);
    load(a, 11, 5, offset_of!(MappedCallRecord, context));
    load(a, 11, 11, 16);
    load(a, 1, 14, offset_of!(GeneratedLane, arity));
    imm(a, 0, 3);
    let copy = a.label();
    let copied = a.label();
    a.bind(copy);
    cmp(a, 0, 1);
    a.jcc(Cc::Ae, copied);
    a.extend_from_slice(&[0x49, 0x8b, 0x14, 0xc3, 0x48, 0x89, 0x54, 0xc4, 0xe8]); // rdx=args[rax]; outgoing[rax-3]=rdx
    alu(a, 0, 0, 1);
    a.jmp(copy);
    a.bind(copied);
    imm(a, 7, egcl_rt::value::NIL.0);
    reg(a, 6, 7);
    reg(a, 2, 7);
    let loaded = a.label();
    alu(a, 7, 1, 0);
    a.jcc(Cc::E, loaded);
    load(a, 7, 11, 0);
    alu(a, 7, 1, 1);
    a.jcc(Cc::E, loaded);
    load(a, 6, 11, 8);
    alu(a, 7, 1, 2);
    a.jcc(Cc::E, loaded);
    load(a, 2, 11, 16);
    a.bind(loaded);
    load(a, 13, 12, offset_of!(GeneratedActivation, execution));
    load(a, 10, 14, offset_of!(GeneratedLane, entry));
    load(a, 11, 14, offset_of!(GeneratedLane, function));
    load(a, 0, 11, 0);
    a.extend_from_slice(&[0x41, 0xff, 0xd2]);
    a.bind(native_return);
    reg(a, 4, 5);
    store(a, 4, offset_of!(MappedCallRecord, outcome), 0);
    let single = a.label();
    let tuple = a.label();
    a.jcc(Cc::Ae, single);
    store(a, 4, offset_of!(MappedCallRecord, return_values), 2);
    store(a, 4, offset_of!(MappedCallRecord, return_values) + 8, 1);
    a.jmp(tuple);
    a.bind(single);
    imm(a, 0, 1);
    store(a, 4, offset_of!(MappedCallRecord, return_values), 0);
    imm(a, 0, 0);
    store(a, 4, offset_of!(MappedCallRecord, return_values) + 8, 0);
    a.bind(tuple);
    imm(a, 0, 0);
    store(a, 4, offset_of!(MappedCallRecord, outcome) + 8, 0);
    a.jmp(cleanup);
    a.bind(native_escape);
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    store(a, 4, offset_of!(MappedCallRecord, outcome), 0);
    store(a, 4, offset_of!(MappedCallRecord, outcome) + 8, 2);
    a.bind(cleanup);
    a.push(0xfc); // CLD before entering the C ABI scope-return helper
    // Scope leave roots the primary before any pending-return slow poll.
    reg(a, 7, 4);
    imm(a, 0, helpers.leave as usize as u64);
    a.extend_from_slice(&[0xff, 0xd0]);
    word(a, 12, offset_of!(GeneratedCallRecord, activation));
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, runtime) + offset_of!(GeneratedSegmentBindings, control),
    );
    word(a, 0, offset_of!(GeneratedCallRecord, previous_control));
    xchg(a, 11);
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, runtime) + offset_of!(GeneratedSegmentBindings, active),
    );
    word(a, 0, offset_of!(GeneratedCallRecord, previous_segment));
    store(a, 11, 0, 0);
    word(a, 11, offset_of!(GeneratedCallRecord, capture_slot));
    word(a, 0, offset_of!(GeneratedCallRecord, previous_capture));
    store(a, 11, 0, 0);
    load(
        a,
        11,
        12,
        offset_of!(GeneratedActivation, publication) + offset_of!(GeneratedPublication, active),
    );
    word(a, 0, offset_of!(GeneratedCallRecord, previous_publication));
    store(a, 11, 0, 0);
    load(a, 11, 12, offset_of!(GeneratedActivation, depth));
    word(a, 0, offset_of!(GeneratedCallRecord, previous_depth));
    store32(a, 11, 0, 0);
    lea(a, 11, 4, TAIL + offset_of!(GeneratedCallRecord, segment));
    imm(a, 0, 0);
    for offset in [S.saved_sp, S.landing_pc, S.entry_return_pc, S.landing_sp] {
        store(a, 11, offset, 0);
    }
    word(a, 14, offset_of!(GeneratedCallRecord, lane));
    load(a, 11, 14, offset_of!(GeneratedLane, busy));
    byte(a, 11, 0);
    a.push(0xfc); // Preserve the existing ordinary shim return convention.
    load(a, 0, 4, offset_of!(MappedCallRecord, outcome) + 8);
    alu(a, 7, 0, 0);
    a.jcc(Cc::Ne, escape);
    a.jmp(returned);
    // Failed guards used preserved registers as scratch. Restore them before
    // entering existing Rust preparation, whose capture trusts those homes.
    a.bind(cold);
    for (i, r) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        load(a, r, 4, 8 + i * 8);
    }
    load(a, 7, 4, offset_of!(MappedCallRecord, target));
}
