// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! x86-64 native multiple-value return sequences.

use egcl_rt::asm::{Asm, Cc};

/// Return one primary in RAX. RDX and RCX are unspecified on this fast path.
pub fn emit_single_return(a: &mut Asm) {
    a.extend_from_slice(&[0xf8, 0xc3]); // clc; ret
}

/// Snapshot runtime values after the original epilogue, then return the tuple.
///
/// Entry RSP must be 8 modulo 16. The SysV helper receives `(primary, dst,
/// u64::MAX)` and writes `{ count: usize, payload: *const EgclVal }` at `dst`.
/// It must not allocate, collect, suspend, or alter the primary's Lisp value:
/// the temporary primary spill is intentionally not a published GC root.
/// Payload storage belongs to the runtime and includes the primary for many
/// values. Zero and one use a null payload. Count zero already has NIL in RAX.
pub fn emit_dynamic_return(a: &mut Asm, snapshot_helper: u64) {
    a.extend_from_slice(&[0x48, 0x83, 0xec, 24]); // sub rsp,24; aligned helper call
    a.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, 16]); // saved primary
    a.extend_from_slice(&[0x48, 0x89, 0xc7]); // rdi=primary
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // rsi=snapshot destination
    a.extend_from_slice(&[0x48, 0xc7, 0xc2, 0xff, 0xff, 0xff, 0xff]); // rdx=-1
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&snapshot_helper.to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]); // call rax
    a.extend_from_slice(&[0x48, 0x8b, 0x14, 0x24]); // rdx=count
    a.extend_from_slice(&[0x48, 0x8b, 0x4c, 0x24, 8]); // rcx=payload
    a.extend_from_slice(&[0x48, 0x8b, 0x44, 0x24, 16]); // rax=primary
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 24]); // teardown before final flags
    emit_tuple_return(a);
}

/// Return RAX/RDX/RCX unchanged, setting CF iff RDX's full-width count is not 1.
pub fn emit_tuple_return(a: &mut Asm) {
    let single = a.label();
    a.extend_from_slice(&[0x48, 0x83, 0xfa, 1]); // cmp rdx,1
    a.jcc(Cc::E, single);
    a.extend_from_slice(&[0xf9, 0xc3]); // stc; ret
    a.bind(single);
    emit_single_return(a);
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use egcl_rt::{jit::JitBuffer, value::EgclVal};
    use std::cell::Cell;

    #[repr(C)]
    struct Snapshot {
        count: usize,
        payload: *const EgclVal,
    }

    #[repr(C)]
    #[derive(Default, Debug)]
    struct Observed {
        primary: u64,
        count: u64,
        payload: u64,
        carry: u8,
    }

    static VALUES: [EgclVal; 3] = [EgclVal(0x100), EgclVal(0x200), EgclVal(0x300)];
    thread_local! {
        static COUNT: Cell<usize> = const { Cell::new(0) };
        static REQUEST: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
    }

    // A nonallocating helper deliberately destroys RAX, a C caller-saved
    // register, after publishing the requested snapshot.
    unsafe extern "C" fn snapshot(primary: u64, dst: *mut EgclVal, n: u64) -> u64 {
        REQUEST.with(|request| request.set(Some((primary, n))));
        let count = COUNT.with(Cell::get);
        unsafe {
            dst.cast::<Snapshot>().write(Snapshot {
                count,
                payload: if count <= 1 {
                    std::ptr::null()
                } else {
                    VALUES.as_ptr()
                },
            });
        }
        !primary
    }

    // Capture the native result before any Rust instruction can overwrite CF
    // or the caller-saved result registers. Keep the SysV call stack aligned.
    #[unsafe(naked)]
    unsafe extern "C" fn observe(
        _entry: *const u8,
        _primary: u64,
        _initial_carry: u64,
        _out: *mut Observed,
    ) {
        core::arch::naked_asm!(
            "push rbx",
            "push r12",
            "sub rsp, 8",
            "mov rbx, rcx",
            "mov r12, rdi",
            "mov rax, rsi",
            "bt rdx, 0",
            "call r12",
            "setc byte ptr [rbx + 24]",
            "mov [rbx], rax",
            "mov [rbx + 8], rdx",
            "mov [rbx + 16], rcx",
            "add rsp, 8",
            "pop r12",
            "pop rbx",
            "ret",
        );
    }

    #[test]
    fn single_return_clears_poisoned_carry_and_preserves_primary() {
        let mut a = Asm::new();
        emit_single_return(&mut a);
        let code = JitBuffer::new(&a.finish().unwrap()).unwrap();
        for carry in [0, 1] {
            let mut out = Observed::default();
            unsafe { observe(code.as_ptr(), 0x1234_5678_9abc_def0, carry, &mut out) };
            assert_eq!(out.primary, 0x1234_5678_9abc_def0);
            assert_eq!(out.carry, 0, "incoming carry {carry}");
        }
    }

    #[test]
    fn tuple_return_classifies_count_without_clobbering_registers() {
        for count in [0u64, 1, 2, (1u64 << 40) + 1, u64::MAX] {
            let payload = if count <= 1 {
                0
            } else {
                VALUES.as_ptr() as u64
            };
            let mut a = Asm::new();
            a.extend_from_slice(&[0x48, 0xba]); // mov rdx,count (preserves CF)
            a.extend_from_slice(&count.to_le_bytes());
            a.extend_from_slice(&[0x48, 0xb9]); // mov rcx,payload
            a.extend_from_slice(&payload.to_le_bytes());
            emit_tuple_return(&mut a);
            let code = JitBuffer::new(&a.finish().unwrap()).unwrap();
            for carry in [0, 1] {
                let mut out = Observed::default();
                unsafe { observe(code.as_ptr(), VALUES[0].0, carry, &mut out) };
                assert_eq!(out.primary, VALUES[0].0);
                assert_eq!(out.count, count);
                assert_eq!(out.payload, payload);
                assert_eq!(
                    out.carry,
                    u8::from(count != 1),
                    "count {count}, carry {carry}"
                );
            }
        }
    }

    #[test]
    fn dynamic_return_captures_zero_one_many_and_full_width_counts() {
        assert_eq!(std::mem::size_of::<Snapshot>(), 16);
        assert_eq!(std::mem::offset_of!(Snapshot, payload), 8);
        let mut a = Asm::new();
        emit_dynamic_return(&mut a, snapshot as *const () as u64);
        let code = JitBuffer::new(&a.finish().unwrap()).unwrap();
        for count in [0, 1, 2, 3, (1usize << 40) + 1, usize::MAX] {
            for carry in [0, 1] {
                COUNT.with(|slot| slot.set(count));
                REQUEST.with(|slot| slot.set(None));
                let primary = if count == 0 {
                    egcl_rt::value::NIL.0
                } else {
                    VALUES[0].0
                };
                let mut out = Observed::default();
                unsafe { observe(code.as_ptr(), primary, carry, &mut out) };
                assert_eq!(REQUEST.with(Cell::get), Some((primary, u64::MAX)));
                assert_eq!(out.primary, primary, "count {count}, carry {carry}");
                assert_eq!(out.count, count as u64);
                assert_eq!(
                    out.payload,
                    if count <= 1 {
                        0
                    } else {
                        VALUES.as_ptr() as u64
                    }
                );
                assert_eq!(out.carry, u8::from(count != 1));
            }
        }
    }
}
