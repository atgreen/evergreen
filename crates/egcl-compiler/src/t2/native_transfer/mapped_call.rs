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
}

pub type PrepareMappedCall = unsafe extern "C" fn(u64, *mut MappedCallRecord);
pub type FinishMappedCall = unsafe extern "C" fn(*mut MappedCallRecord);

/// Enter a distinct mapped activation after preparation has returned through
/// Rust. A declined preparation tail-calls the explicit legacy adapter; a
/// failed preparation captures the original caller without invoking the target.
/// Child escape lands in this caller-owned veneer, so retirement may release
/// child executable storage without unmapping a live return address.
/// Both retirement and restoration must neither collect nor change MV.
pub fn emit_mapped_call_veneer(
    cell: u64,
    prepare: PrepareMappedCall,
    finish: FinishMappedCall,
    legacy: *const u8,
    capture: *const u8,
) -> Vec<u8> {
    const SIZE: u8 = std::mem::size_of::<MappedCallRecord>() as u8;
    const {
        assert!(std::mem::size_of::<MappedCallRecord>() == 104);
        assert!(std::mem::offset_of!(MappedCallRecord, preserved) == 8);
        assert!(std::mem::offset_of!(MappedCallRecord, entry) == 56);
        assert!(std::mem::offset_of!(MappedCallRecord, activation) == 64);
        assert!(std::mem::offset_of!(MappedCallRecord, owner) == 72);
        assert!(std::mem::offset_of!(MappedCallRecord, cold_entry) == 80);
        assert!(std::mem::offset_of!(MappedCallRecord, outcome) == 88);
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
    let escape = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x83, 0xec, SIZE]);
    capture_stack_word(&mut a, false, 7, 0);
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, false, reg, 8 + index * 8);
    }
    a.extend_from_slice(&[0x48, 0x8d, 0x05]); // lea rax,[rip+cold]
    let cold_displacement = a.len();
    a.extend_from_slice(&[0; 4]);
    capture_stack_word(&mut a, false, 0, 80);
    a.extend_from_slice(&[0x48, 0xbf]);
    a.extend_from_slice(&cell.to_le_bytes());
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // rsi=record
    target(&mut a, prepare as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    capture_stack_word(&mut a, true, 0, 96);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::Ne, escape);
    capture_stack_word(&mut a, true, 0, 56);
    a.extend_from_slice(&[0x48, 0x85, 0xc0]);
    a.jcc(Cc::E, fallback);
    capture_stack_word(&mut a, true, 7, 64);
    a.extend_from_slice(&[0xff, 0xd0]); // child body: no Rust frame spans this call
    capture_stack_word(&mut a, false, 0, 88);
    a.extend_from_slice(&[0x48, 0x89, 0xe7]);
    target(&mut a, finish as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    restore(&mut a);
    capture_stack_word(&mut a, true, 0, 88);
    a.extend_from_slice(&[0x48, 0x83, 0xc4, SIZE, 0xc3]);

    let cold_offset = a.len();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    capture_stack_word(&mut a, false, 0, 88);
    a.extend_from_slice(&[0x48, 0x89, 0xe7]);
    target(&mut a, finish as usize);
    a.extend_from_slice(&[0xff, 0xd0]);
    a.extend_from_slice(&[0xb8, 1, 0, 0, 0]); // NativeExit::Transfer
    capture_stack_word(&mut a, false, 0, 96);
    a.bind(escape);
    restore(&mut a);
    capture_stack_word(&mut a, true, 7, 0);
    capture_stack_word(&mut a, true, 6, 88);
    capture_stack_word(&mut a, true, 2, 96);
    a.extend_from_slice(&[0x48, 0x83, 0xc4, SIZE]);
    target(&mut a, capture as usize);
    a.extend_from_slice(&[0xff, 0xe0]);

    a.bind(fallback);
    restore(&mut a);
    capture_stack_word(&mut a, true, 7, 0);
    a.extend_from_slice(&[0x48, 0x83, 0xc4, SIZE]);
    target(&mut a, legacy as usize);
    a.extend_from_slice(&[0xff, 0xe0]);
    let mut bytes = a.finish().expect("local mapped entry labels");
    let relative = i32::try_from(cold_offset as isize - (cold_displacement + 4) as isize).unwrap();
    bytes[cold_displacement..cold_displacement + 4].copy_from_slice(&relative.to_le_bytes());
    bytes
}
