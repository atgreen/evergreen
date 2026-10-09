// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::{Asm, Cc, NativeOutcome, capture_stack_word};
use crate::t2::emit::TransferCallRequest;

/// Caller-owned machine storage, live until the mapped call returns or escapes.
/// Preserved words are not roots: caller activation shadows remain authoritative.
#[repr(C)]
pub struct MappedCallRecord {
    pub request: *mut TransferCallRequest,
    pub preserved: [u64; 6],
    pub entry: *const u8,
    pub activation: *mut u8,
    pub owner: *mut u8,
    pub cold_entry: *const u8,
    pub outcome: NativeOutcome,
    pub context: *mut egcl_rt::call_table::NativeCallContext,
    pub target: u64,
    pub forward: *const egcl_rt::function::NativeCallableEntries,
}

pub type PrepareMappedCall = unsafe extern "C" fn(u64, *mut MappedCallRecord);
pub type FinishMappedCall = unsafe extern "C" fn(*mut MappedCallRecord);

/// Permanent published native entry. Preparation selects a mapped activation
/// or completes the checked compatibility call. Helpers return through Rust
/// before any capture. Child retirement runs in this permanent mapping, so it
/// may release child executable storage without unmapping a live return PC.
/// Normal retirement must neither collect nor change MV. Cold resumption may
/// run Lisp and must publish a rooted outcome before retiring the child.
pub fn emit_published_call_entry(
    slice: bool,
    prepare: PrepareMappedCall,
    finish: FinishMappedCall,
    resume: FinishMappedCall,
    checked: PrepareMappedCall,
) -> Vec<u8> {
    const SIZE: u32 = std::mem::size_of::<MappedCallRecord>() as u32 + 8; // SysV call alignment
    const {
        assert!(std::mem::size_of::<MappedCallRecord>() == 128);
        assert!(std::mem::offset_of!(MappedCallRecord, preserved) == 8);
        assert!(std::mem::offset_of!(MappedCallRecord, entry) == 56);
        assert!(std::mem::offset_of!(MappedCallRecord, activation) == 64);
        assert!(std::mem::offset_of!(MappedCallRecord, owner) == 72);
        assert!(std::mem::offset_of!(MappedCallRecord, cold_entry) == 80);
        assert!(std::mem::offset_of!(MappedCallRecord, outcome) == 88);
        assert!(std::mem::offset_of!(MappedCallRecord, context) == 104);
        assert!(std::mem::offset_of!(MappedCallRecord, target) == 112);
        assert!(std::mem::offset_of!(MappedCallRecord, forward) == 120);
        assert!(std::mem::offset_of!(egcl_rt::function::NativeCallableEntries, registers) == 0);
        assert!(std::mem::offset_of!(egcl_rt::function::NativeCallableEntries, slice) == 8);
    }
    fn release(a: &mut Asm) {
        a.extend_from_slice(&[0x48, 0x81, 0xc4]);
        a.extend_from_slice(&SIZE.to_le_bytes());
    }
    fn target(a: &mut Asm, address: usize) {
        a.extend_from_slice(&[0x48, 0xb8]);
        a.extend_from_slice(&(address as u64).to_le_bytes());
    }
    fn restore(a: &mut Asm) {
        for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
            capture_stack_word(a, true, reg, 8 + index * 8);
        }
    }
    let mut a = Asm::new();
    let fallback = a.label();
    let forward = a.label();
    let escape = a.label();
    let returned = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x81, 0xec]);
    a.extend_from_slice(&SIZE.to_le_bytes());
    capture_stack_word(&mut a, false, if slice { 1 } else { 9 }, 104);
    capture_stack_word(&mut a, false, 7, 112);
    // Copy the context request, then publish incoming arguments in scanned
    // storage before preparation can poll, compile, or allocate.
    if slice {
        a.extend_from_slice(&[0x49, 0x89, 0xcb]); // r11=rcx
    } else {
        a.extend_from_slice(&[0x4d, 0x89, 0xcb]); // r11=r9
    }
    a.extend_from_slice(&[0x49, 0x8b, 0x03]); // rax=context.request
    capture_stack_word(&mut a, false, 0, 0);
    a.extend_from_slice(&[0x49, 0x89, 0x73, 24]); // context.nargs=rsi
    if slice {
        a.extend_from_slice(&[0x49, 0x89, 0x53, 16]); // context.args=rdx
    } else {
        let spilled = a.label();
        a.extend_from_slice(&[0x4d, 0x8b, 0x53, 16]); // r10=context.args
        a.extend_from_slice(&[0x48, 0x85, 0xf6]);
        a.jcc(Cc::E, spilled);
        a.extend_from_slice(&[0x49, 0x89, 0x12]); // args[0]=rdx
        a.extend_from_slice(&[0x48, 0x83, 0xfe, 1]);
        a.jcc(Cc::E, spilled);
        a.extend_from_slice(&[0x49, 0x89, 0x4a, 8]); // args[1]=rcx
        a.extend_from_slice(&[0x48, 0x83, 0xfe, 2]);
        a.jcc(Cc::E, spilled);
        a.extend_from_slice(&[0x4d, 0x89, 0x42, 16]); // args[2]=r8
        a.bind(spilled);
    }
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, false, reg, 8 + index * 8);
    }
    a.extend_from_slice(&[0x48, 0x8d, 0x05]); // lea rax,[rip+cold]
    let cold_displacement = a.len();
    a.extend_from_slice(&[0; 4]);
    capture_stack_word(&mut a, false, 0, 80);
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // rsi=record
    target(&mut a, prepare as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    capture_stack_word(&mut a, true, 0, 96);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::Ne, escape);
    capture_stack_word(&mut a, true, 0, 120);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::Ne, forward);
    capture_stack_word(&mut a, true, 0, 56);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::E, fallback);
    capture_stack_word(&mut a, true, 7, 64);
    a.extend_from_slice(&[0xff, 0xd0]); // child body: no Rust frame spans this call
    capture_stack_word(&mut a, false, 0, 88);
    a.extend_from_slice(&[0x48, 0x89, 0xe7]);
    target(&mut a, finish as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    a.bind(returned);
    restore(&mut a);
    capture_stack_word(&mut a, true, 0, 88);
    release(&mut a);
    a.extend_from_slice(&[0xc3]);

    let cold_offset = a.len();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    capture_stack_word(&mut a, false, 0, 88);
    a.extend_from_slice(&[0x48, 0x89, 0xe7]);
    target(&mut a, resume as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    capture_stack_word(&mut a, true, 0, 96);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::E, returned);
    a.bind(escape);
    restore(&mut a);
    capture_stack_word(&mut a, true, 7, 0);
    capture_stack_word(&mut a, true, 6, 88);
    capture_stack_word(&mut a, true, 2, 96);
    // r11 is volatile; load before removing the record.
    capture_stack_word(&mut a, true, 11, 104);
    a.extend_from_slice(&[0x4d, 0x8b, 0x5b, 8]); // r11=context.capture
    release(&mut a);
    a.extend_from_slice(&[0x41, 0xff, 0xe3]);

    a.bind(fallback);
    capture_stack_word(&mut a, true, 7, 112);
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // rsi=record
    target(&mut a, checked as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    capture_stack_word(&mut a, true, 0, 96);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::E, returned);
    a.jmp(escape);

    a.bind(forward);
    restore(&mut a);
    capture_stack_word(&mut a, true, 7, 112); // rooted callable slot address
    capture_stack_word(&mut a, true, 1, 104); // original continuation context
    a.extend_from_slice(&[0x48, 0x8b, 0x71, 24]); // invocation count
    let wide = a.label();
    let loaded = a.label();
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 3]);
    a.jcc(Cc::G, wide);
    a.extend_from_slice(&[0x49, 0x89, 0xc9]); // r9=context
    a.extend_from_slice(&[0x4c, 0x8b, 0x51, 16]); // r10=args
    a.extend_from_slice(&[0x48, 0x8b, 0x00]); // rax=register entry
    a.extend_from_slice(&[0x31, 0xd2, 0x31, 0xc9, 0x45, 0x31, 0xc0]);
    a.extend_from_slice(&[0x48, 0x85, 0xf6]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x49, 0x8b, 0x12]);
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 1]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x49, 0x8b, 0x4a, 8]);
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 2]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x4d, 0x8b, 0x42, 16]);
    a.bind(loaded);
    release(&mut a);
    a.extend_from_slice(&[0xff, 0xe0]);
    a.bind(wide);
    a.extend_from_slice(&[0x48, 0x8b, 0x51, 16]); // rdx=args; rcx=context
    a.extend_from_slice(&[0x48, 0x8b, 0x40, 8]); // rax=slice entry
    release(&mut a);
    a.extend_from_slice(&[0xff, 0xe0]);
    let mut bytes = a.finish().expect("local mapped entry labels");
    let relative = i32::try_from(cold_offset as isize - (cold_displacement + 4) as isize).unwrap();
    bytes[cold_displacement..cold_displacement + 4].copy_from_slice(&relative.to_le_bytes());
    bytes
}

/// A named caller only marshals its explicit continuation context and loads the
/// published cell entry. It neither selects the target ABI nor checks results.
/// The request has 32 trailing bytes reserved for NativeCallContext. Tail jumps
/// preserve the original Invoke return PC used by its capture recipe.
pub fn emit_published_call_veneer(
    cell: u64,
    registers: u64,
    slice: u64,
    capture: *const u8,
) -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<TransferCallRequest>() == 32);
        assert!(std::mem::size_of::<egcl_rt::call_table::NativeCallContext>() == 32);
        assert!(std::mem::offset_of!(egcl_rt::call_table::NativeCallContext, capture) == 8);
        assert!(std::mem::offset_of!(egcl_rt::call_table::NativeCallContext, args) == 16);
        assert!(std::mem::offset_of!(egcl_rt::call_table::NativeCallContext, nargs) == 24);
    }
    let mut a = Asm::new();
    let wide = a.label();
    let loaded = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    a.extend_from_slice(&[0x48, 0x89, 0x7f, 32]); // context.request=rdi
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(capture as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0x48, 0x89, 0x47, 40]); // context.capture=rax
    a.extend_from_slice(&[0x48, 0x8b, 0x77, 8]); // rsi=nargs
    a.extend_from_slice(&[0x48, 0x89, 0x77, 56]); // context.nargs=rsi
    a.extend_from_slice(&[0x48, 0x8b, 0x47, 16]); // rax=args
    a.extend_from_slice(&[0x48, 0x89, 0x47, 48]); // context.args=rax
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 3]);
    a.jcc(Cc::G, wide);
    a.extend_from_slice(&[0x4c, 0x8d, 0x4f, 32]); // r9=context
    a.extend_from_slice(&[0x48, 0x8b, 0x47, 16]); // rax=args
    a.extend_from_slice(&[0x31, 0xd2, 0x31, 0xc9, 0x45, 0x31, 0xc0]);
    a.extend_from_slice(&[0x48, 0x85, 0xf6]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x48, 0x8b, 0x10]);
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 1]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x48, 0x8b, 0x48, 8]);
    a.extend_from_slice(&[0x48, 0x83, 0xfe, 2]);
    a.jcc(Cc::E, loaded);
    a.extend_from_slice(&[0x4c, 0x8b, 0x40, 16]);
    a.bind(loaded);
    a.extend_from_slice(&[0x48, 0xbf]);
    a.extend_from_slice(&cell.to_le_bytes());
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&registers.to_le_bytes());
    a.extend_from_slice(&[0xff, 0x20]); // jmp [rax]
    a.bind(wide);
    a.extend_from_slice(&[0x48, 0x8d, 0x4f, 32]); // rcx=context
    a.extend_from_slice(&[0x48, 0x8b, 0x57, 16]); // rdx=args
    a.extend_from_slice(&[0x48, 0xbf]);
    a.extend_from_slice(&cell.to_le_bytes());
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&slice.to_le_bytes());
    a.extend_from_slice(&[0xff, 0x20]);
    a.finish().expect("local published call labels")
}
