// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::native_transfer::{
    OrdinaryHelperOutcome, SysvTransferCapture, emit_ordinary_published_veneer,
};
use egcl_rt::native_transfer::{NativeExit, NativeOutcome, NativeReturnValues};
use egcl_rt::{jit::JitBuffer, value::EgclVal};

#[repr(C)]
struct Request {
    count: usize,
    image_seen: bool,
}

unsafe extern "C" fn helper(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = unsafe { &mut *request.cast::<Request>() };
    let image = unsafe { &mut *image };
    request.image_seen = !image.caller_sp.is_null()
        && !image.return_pc.is_null()
        && image.preserved == [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    // A moving collector may replace these writable caller homes.
    image.preserved[0] = 0x77;
    unsafe {
        out.write(OrdinaryHelperOutcome {
            outcome: NativeOutcome {
                value: EgclVal(0x123),
                exit: NativeExit::Returned,
            },
            values: NativeReturnValues {
                count: request.count,
                payload: if request.count > 1 {
                    0x456usize as *const EgclVal
                } else {
                    std::ptr::null()
                },
            },
            anchor: std::ptr::null_mut(),
        });
    }
}

#[unsafe(naked)]
unsafe extern "C" fn probe(_entry: *const u8, _request: *mut Request, _out: *mut u64) {
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 24",
        "mov [rsp], rdx",
        "mov r11, rdi",
        "mov rdi, rsi",
        "mov rbx, 0x11",
        "mov rbp, 0x22",
        "mov r12, 0x33",
        "mov r13, 0x44",
        "mov r14, 0x55",
        "mov r15, 0x66",
        "call r11",
        "mov r10, [rsp]",
        "mov [r10], rax",
        "mov [r10+8], rdx",
        "mov [r10+16], rcx",
        "setc byte ptr [r10+24]",
        "mov [r10+32], rbx",
        "add rsp, 24",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    );
}

#[test]
fn published_ordinary_veneer_restores_roots_and_complete_return_tuple() {
    let code = JitBuffer::new(&emit_ordinary_published_veneer(helper)).unwrap();
    for count in [0, 1, 3, (1usize << 40) + 1] {
        let mut request = Request {
            count,
            image_seen: false,
        };
        let mut out = [0u64; 5];
        unsafe {
            probe(code.as_ptr(), &mut request, out.as_mut_ptr());
        }
        assert!(request.image_seen);
        assert_eq!(
            out,
            [
                0x123,
                count as u64,
                if count > 1 { 0x456 } else { 0 },
                u64::from(count != 1),
                0x77
            ]
        );
    }
}

use egcl_compiler::t2::native_transfer::{OrdinaryArityRequest, emit_ordinary_arity_veneer};

unsafe extern "C" fn arity_helper(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = unsafe { &*request.cast::<OrdinaryArityRequest>() };
    assert_eq!(request.closure.to_raw(), 0xcafe);
    assert_eq!(request.entry_data, 2);
    assert_eq!(request.nargs, 7);
    assert_eq!(request.register_args.map(EgclVal::to_raw), [11, 22, 33]);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(request.overflow_args, 4) }
            .iter()
            .map(|v| v.to_raw())
            .collect::<Vec<_>>(),
        [44, 55, 66, 77]
    );
    assert_eq!(request.overflow_args.cast::<u64>().cast_const(), unsafe {
        (*image).caller_sp
    });
    unsafe {
        out.write(OrdinaryHelperOutcome {
            outcome: NativeOutcome {
                value: EgclVal(0xabc),
                exit: NativeExit::Returned,
            },
            values: NativeReturnValues::SINGLE,
            anchor: std::ptr::null_mut(),
        });
    }
}

#[unsafe(naked)]
unsafe extern "C" fn arity_probe(_entry: *const u8) -> u64 {
    core::arch::naked_asm!(
        "sub rsp, 40",
        "mov r11, rdi",
        "mov qword ptr [rsp], 44",
        "mov qword ptr [rsp+8], 55",
        "mov qword ptr [rsp+16], 66",
        "mov qword ptr [rsp+24], 77",
        "mov rdi, 11",
        "mov rsi, 22",
        "mov rdx, 33",
        "mov rcx, 7",
        "mov rax, 0xcafe",
        "call r11",
        "add rsp, 40",
        "ret",
    );
}

#[test]
fn wrong_arity_veneer_captures_all_actuals_without_a_callee_frame() {
    let code = JitBuffer::new(&emit_ordinary_arity_veneer(arity_helper, 2)).unwrap();
    assert_eq!(unsafe { arity_probe(code.as_ptr()) }, 0xabc);
}

unsafe extern "C" fn escaping_helper(
    _request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    _image: *mut SysvTransferCapture,
) {
    // Its Drop must run before the assembly leaves any generated frames.
    struct Returned;
    impl Drop for Returned {
        fn drop(&mut self) {
            HELPER_RETURNED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let _returned = Returned;
    unsafe {
        out.write(OrdinaryHelperOutcome {
            outcome: NativeOutcome {
                value: EgclVal(0x789),
                exit: NativeExit::Transfer,
            },
            values: NativeReturnValues::SINGLE,
            anchor: egcl_rt::native_transfer::current_segment(),
        });
    }
}
static HELPER_RETURNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[test]
fn exceptional_veneer_leaves_the_anchor_after_rust_helpers_return() {
    let code = JitBuffer::new(&emit_ordinary_published_veneer(escaping_helper)).unwrap();
    let stack = egcl_rt::stack::EgclStack::new(64 * 1024);
    let result = unsafe {
        egcl_rt::native_transfer::invoke_native_segment(code.as_ptr(), std::ptr::null_mut(), &stack)
    }
    .unwrap();
    assert_eq!(result.value.to_raw(), 0x789);
    assert_eq!(result.exit, NativeExit::Transfer);
    assert!(HELPER_RETURNED.load(std::sync::atomic::Ordering::Relaxed));
    assert!(egcl_rt::native_transfer::current_segment().is_null());
}
