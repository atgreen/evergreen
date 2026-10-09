#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::native_transfer::emit_helper_veneer;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
use egcl_rt::stack::EgclStack;
use egcl_rt::value::{NIL, EgclVal};

#[repr(C)]
struct Request {
    anchor: *mut NativeSegment,
    drops: usize,
    continued: usize,
    value: EgclVal,
    exit: NativeExit,
}

struct Finished<'a>(&'a mut usize);
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        *self.0 += 1;
    }
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &mut *request.cast::<Request>() };
    let _finished = Finished(&mut request.drops);
    request.anchor = native_transfer::current_segment();
    unsafe {
        out.write(NativeOutcome {
            value: request.value,
            exit: request.exit,
        })
    };
}

// The fixture has no heap roots or Lisp cleanup to retire. A real cold route
// must capture those obligations before it can leave a segment.
#[unsafe(naked)]
unsafe extern "C" fn cold_route(_request: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!(
        "endbr64",
        "mov rdi, [rdi]",
        "jmp {leave}",
        leave = sym native_transfer::leave_native_segment,
    );
}

#[test]
fn successful_helper_returns_all_primary_values_after_rust_cleanup() {
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, cold_route as *const u8)).unwrap();
    let call: unsafe extern "C" fn(*mut u8) -> EgclVal =
        unsafe { std::mem::transmute(veneer.as_ptr()) };
    for value in [EgclVal::from_fixnum(0), NIL, EgclVal::from_fixnum(42)] {
        let mut request = Request {
            anchor: std::ptr::null_mut(),
            drops: 0,
            continued: 0,
            value,
            exit: NativeExit::Returned,
        };
        assert_eq!(
            unsafe { call((&mut request as *mut Request).cast()) },
            value
        );
        assert_eq!(request.drops, 1);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn generated_caller_reaches_segment_landing_only_after_helper_returns() {
    assert!(
        native_transfer::is_supported(),
        "native segment execution gate unavailable"
    );
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, cold_route as *const u8)).unwrap();
    // Entry saves the request, calls the veneer, then records normal completion.
    // There is deliberately no transfer test after the generated-to-generated call.
    let mut caller = vec![0xf3, 0x0f, 0x1e, 0xfa, 0x53, 0x48, 0x89, 0xfb, 0x48, 0xb8];
    caller.extend_from_slice(&(veneer.as_ptr() as u64).to_le_bytes());
    caller.extend_from_slice(&[0xff, 0xd0, 0x48, 0xff, 0x43, 16, 0x5b, 0xc3]);
    let caller = JitBuffer::new(&caller).unwrap();
    let stack = EgclStack::new(64 * 1024);
    for exit in [
        NativeExit::Returned,
        NativeExit::Transfer,
        NativeExit::Deopt,
    ] {
        let mut request = Request {
            anchor: std::ptr::null_mut(),
            drops: 0,
            continued: 0,
            value: EgclVal::from_fixnum(42),
            exit,
        };
        let outcome = unsafe {
            native_transfer::invoke_native_segment(
                caller.as_ptr(),
                (&mut request as *mut Request).cast(),
                &stack,
            )
        }
        .unwrap();
        assert_eq!(outcome.exit, exit);
        assert_eq!(outcome.value, request.value);
        assert_eq!(request.drops, 1, "Rust destructor cannot be skipped");
        assert_eq!(request.continued, usize::from(exit == NativeExit::Returned));
        assert!(native_transfer::current_segment().is_null());
    }
    eprintln!("executed generated helper success/transfer/deopt segment routes");
}

#[test]
fn capture_recipes_reject_clobbered_registers_and_invalid_stack_offsets() {
    use egcl_compiler::t2::native_transfer::SysvCaptureLocation;
    use egcl_compiler::t2::x64_frame::ValueHome;
    for register in [0, 1, 2, 4, 6, 7, 8, 9, 10, 11, 16] {
        assert!(SysvCaptureLocation::for_home(ValueHome::Reg(register), 0, 0).is_err());
    }
    for register in [3, 5, 12, 13, 14, 15] {
        assert!(SysvCaptureLocation::for_home(ValueHome::Reg(register), 0, 0).is_ok());
    }
    assert!(SysvCaptureLocation::for_home(ValueHome::Stack(0), 1, 0).is_ok());
    assert!(SysvCaptureLocation::for_home(ValueHome::Stack(1), 1, 0).is_err());
    assert!(SysvCaptureLocation::for_home(ValueHome::Stack(0), 1, 3).is_err());
    assert!(SysvCaptureLocation::for_home(ValueHome::Stack(u32::MAX - 1), u32::MAX, 0).is_err());
}

#[test]
fn legacy_cell_veneer_reloads_slice_entries_and_preserves_primary_values() {
    use egcl_compiler::t2::emit::TransferCallRequest;
    use egcl_compiler::t2::native_transfer::emit_legacy_cell_veneer;
    use std::sync::atomic::{AtomicUsize, Ordering};

    std::thread_local! {
        static RECOVERY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static ENTRY_RECOVERY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static STATUS_RECOVERY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    unsafe extern "C" fn first(
        cell: u64,
        count: usize,
        args: *const EgclVal,
        token: u64,
    ) -> EgclVal {
        ENTRY_RECOVERY.set(RECOVERY.get());
        // Cold bridges restore legacy recovery after their Rust callback.
        RECOVERY.set(1);
        assert_eq!(count, 5);
        assert_eq!(token, 0);
        unsafe {
            *(cell as *mut usize) += 1;
            *args
        }
    }
    unsafe extern "C" fn last(
        cell: u64,
        count: usize,
        args: *const EgclVal,
        token: u64,
    ) -> EgclVal {
        ENTRY_RECOVERY.set(RECOVERY.get());
        // Cold bridges restore legacy recovery after their Rust callback.
        RECOVERY.set(1);
        assert_eq!(count, 5);
        assert_eq!(token, 0);
        unsafe {
            *(cell as *mut usize) += 10;
            *args.add(4)
        }
    }
    extern "C" fn recovery_toggle(enabled: u64) {
        RECOVERY.set(enabled);
    }
    extern "C" fn returned() -> u64 {
        STATUS_RECOVERY.set(RECOVERY.get());
        0
    }

    let entry = AtomicUsize::new(first as *const () as usize);
    let mut calls = 0usize;
    let veneer = JitBuffer::new(&emit_legacy_cell_veneer(
        &mut calls as *mut usize as u64,
        &entry as *const AtomicUsize as u64,
        recovery_toggle,
        returned,
        cold_route as *const u8,
    ))
    .unwrap();
    let call: unsafe extern "C" fn(*mut TransferCallRequest) -> EgclVal =
        unsafe { std::mem::transmute(veneer.as_ptr()) };
    for value in [NIL, EgclVal::from_fixnum(0), EgclVal::from_fixnum(42)] {
        let mut args = [value, NIL, NIL, NIL, EgclVal::from_fixnum(99)];
        let mut request = TransferCallRequest {
            symbol: 0,
            nargs: args.len(),
            args: args.as_mut_ptr(),
            activation: std::ptr::null_mut(),
        };
        entry.store(first as *const () as usize, Ordering::Release);
        assert_eq!(unsafe { call(&mut request) }, value);
        assert_eq!(ENTRY_RECOVERY.get(), 1, "legacy target needs its recovery");
        assert_eq!(
            STATUS_RECOVERY.get(),
            0,
            "mapped status/capture cannot use legacy recovery"
        );
        entry.store(last as *const () as usize, Ordering::Release);
        assert_eq!(unsafe { call(&mut request) }, EgclVal::from_fixnum(99));
        assert_eq!(ENTRY_RECOVERY.get(), 1);
        assert_eq!(STATUS_RECOVERY.get(), 0);
        assert_eq!(RECOVERY.get(), 0);
    }
    assert_eq!(calls, 33);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn legacy_cell_error_disables_recovery_after_rust_returns_before_cold_exit() {
    use egcl_compiler::t2::emit::TransferCallRequest;
    use egcl_compiler::t2::native_transfer::emit_legacy_cell_veneer;
    use std::sync::atomic::AtomicUsize;
    std::thread_local! {
        static RECOVERY: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static STATUS_RECOVERY: std::cell::Cell<u64> = const { std::cell::Cell::new(9) };
    }
    struct State {
        request: TransferCallRequest,
        drops: usize,
        entry_recovery: u64,
    }
    unsafe extern "C" fn legacy(cell: u64, _: usize, _: *const EgclVal, _: u64) -> EgclVal {
        let state = unsafe { &mut *(cell as *mut State) };
        let _finished = Finished(&mut state.drops);
        state.entry_recovery = RECOVERY.get();
        // The fixture cold route reads the segment anchor from word zero.
        state.request.symbol = native_transfer::current_segment() as u64;
        RECOVERY.set(1);
        EgclVal::from_fixnum(42)
    }
    extern "C" fn recovery_toggle(enabled: u64) {
        RECOVERY.set(enabled);
    }
    extern "C" fn pending() -> u64 {
        STATUS_RECOVERY.set(RECOVERY.get());
        1
    }
    let mut state = State {
        request: TransferCallRequest {
            symbol: 0,
            nargs: 0,
            args: std::ptr::null_mut(),
            activation: std::ptr::null_mut(),
        },
        drops: 0,
        entry_recovery: 0,
    };
    let entry = AtomicUsize::new(legacy as *const () as usize);
    let veneer = JitBuffer::new(&emit_legacy_cell_veneer(
        &mut state as *mut State as u64,
        &entry as *const AtomicUsize as u64,
        recovery_toggle,
        pending,
        cold_route as *const u8,
    ))
    .unwrap();
    let stack = EgclStack::new(64 * 1024);
    let outcome = unsafe {
        native_transfer::invoke_native_segment(
            veneer.as_ptr(),
            (&mut state.request as *mut TransferCallRequest).cast(),
            &stack,
        )
    }
    .unwrap();
    assert_eq!(outcome.exit, NativeExit::Transfer);
    assert_eq!(outcome.value, EgclVal::from_fixnum(42));
    assert_eq!(state.drops, 1);
    assert_eq!(state.entry_recovery, 1);
    assert_eq!(STATUS_RECOVERY.get(), 0);
    assert_eq!(RECOVERY.get(), 0);
}

#[repr(C)]
struct PublishedRequest {
    return_pc: usize,
    call_sp: usize,
    restored: [u64; 6],
    drops: usize,
}

unsafe extern "C" fn published_helper(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut egcl_compiler::t2::native_transfer::SysvTransferCapture,
) {
    let request = unsafe { &mut *request.cast::<PublishedRequest>() };
    let image = unsafe { &mut *image };
    assert_eq!(image.request, (request as *mut PublishedRequest).cast());
    assert_eq!(image.return_pc as usize, request.return_pc);
    assert_eq!(image.caller_sp as usize, request.call_sp);
    assert_eq!(image.value, NIL, "initialize the whole image before Rust borrows it");
    assert_eq!(image.exit, NativeExit::Returned);
    for (index, word) in image.preserved.iter_mut().enumerate() {
        assert_eq!(*word, 0x1100 + index as u64);
        *word = 0x2200 + index as u64;
    }
    let _finished = Finished(&mut request.drops);
    unsafe { out.write(NativeOutcome { value: EgclVal::from_fixnum(42), exit: NativeExit::Returned }); }
}

#[test]
fn published_helper_captures_before_rust_and_reloads_writable_register_words() {
    use egcl_compiler::t2::native_transfer::emit_published_helper_veneer;
    let veneer = JitBuffer::new(&emit_published_helper_veneer(published_helper, std::ptr::null())).unwrap();
    let registers = [3u8, 5, 12, 13, 14, 15];
    let mut caller = Vec::new();
    for reg in registers {
        if reg >= 8 { caller.push(0x41); }
        caller.push(0x50 | (reg & 7));
    }
    caller.extend_from_slice(&[
        0x48, 0x83, 0xec, 8, // align call and retain request
        0x48, 0x89, 0x3c, 0x24, // mov [rsp],rdi
        0x48, 0x89, 0x67, 8, // mov [rdi+8],rsp
    ]);
    for (index, reg) in registers.into_iter().enumerate() {
        caller.extend_from_slice(&[0x48 | (reg >> 3), 0xb8 | (reg & 7)]);
        caller.extend_from_slice(&(0x1100u64 + index as u64).to_le_bytes());
    }
    caller.extend_from_slice(&[0x48, 0xb8]);
    caller.extend_from_slice(&(veneer.as_ptr() as u64).to_le_bytes());
    caller.extend_from_slice(&[0xff, 0xd0]);
    let return_offset = caller.len();
    caller.extend_from_slice(&[0x48, 0x8b, 0x3c, 0x24]); // recover request
    for (index, reg) in registers.into_iter().enumerate() {
        caller.extend_from_slice(&[0x48 | ((reg >> 3) << 2), 0x89, 0x47 | ((reg & 7) << 3), (16 + index * 8) as u8]);
    }
    caller.extend_from_slice(&[0x48, 0x83, 0xc4, 8]);
    for reg in registers.into_iter().rev() {
        if reg >= 8 { caller.push(0x41); }
        caller.push(0x58 | (reg & 7));
    }
    caller.push(0xc3);
    let caller = JitBuffer::new(&caller).unwrap();
    let call: unsafe extern "C" fn(*mut PublishedRequest) -> EgclVal = unsafe { std::mem::transmute(caller.as_ptr()) };
    let mut request = PublishedRequest {
        return_pc: caller.as_ptr() as usize + return_offset,
        call_sp: 0,
        restored: [0; 6],
        drops: 0,
    };
    assert_eq!(unsafe { call(&mut request) }, EgclVal::from_fixnum(42));
    assert_eq!(request.restored, [0x2200, 0x2201, 0x2202, 0x2203, 0x2204, 0x2205]);
    assert_eq!(request.drops, 1);
}
