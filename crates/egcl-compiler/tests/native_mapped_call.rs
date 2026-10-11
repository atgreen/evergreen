#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::emit::TransferCallRequest;
use egcl_compiler::t2::native_transfer::MappedCallRecord;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
use egcl_rt::value::{EgclVal, NIL};
use std::cell::RefCell;

thread_local! {
    static MV_RETURN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(1) };
    static MV_RETURN_FALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
static MV_RETURN_PAYLOAD: [EgclVal; 3] = [EgclVal(336), EgclVal(344), EgclVal(352)];

#[repr(C)]
#[derive(Default, Debug)]
struct ObservedReturn { primary: u64, count: usize, payload: usize, carry: u8 }

#[unsafe(naked)]
unsafe extern "C" fn observe_return(
    _entry: *const u8,
    _context: *mut egcl_rt::call_table::NativeCallContext,
    _out: *mut ObservedReturn,
) {
    core::arch::naked_asm!(
        "push rbx", "mov rbx, rdx", "mov rax, rdi", "mov rcx, rsi",
        "xor edi, edi", "xor esi, esi", "xor edx, edx", "call rax",
        "setc byte ptr [rbx + 24]", "mov [rbx], rax", "mov [rbx + 8], rdx",
        "mov [rbx + 16], rcx", "pop rbx", "ret",
    )
}

unsafe extern "C" fn prepare_values(_: u64, record: *mut MappedCallRecord) {
    let record = unsafe { &mut *record };
    record.forward = std::ptr::null();
    record.activation = std::ptr::null_mut();
    record.owner = std::ptr::null_mut();
    record.entry = if MV_RETURN_FALLBACK.with(std::cell::Cell::get) {
        std::ptr::null()
    } else { child_values as *const u8 };
    record.outcome = NativeOutcome { value: NIL, exit: NativeExit::Returned };
}

unsafe extern "C" fn child_values(_: *mut u8) -> EgclVal { EgclVal::from_fixnum(42) }

unsafe extern "C" fn finish_values(record: *mut MappedCallRecord) {
    let count = MV_RETURN_COUNT.with(std::cell::Cell::get);
    unsafe { (*record).return_values = egcl_rt::native_transfer::NativeReturnValues {
        count,
        payload: if count > 1 { MV_RETURN_PAYLOAD.as_ptr() } else { std::ptr::null() },
    }; }
    // The callback is a C boundary: deliberately destroy the result registers
    // and CF after materializing its tuple. The adapter must reconstruct them.
    unsafe { core::arch::asm!("xor eax,eax", "xor edx,edx", "xor ecx,ecx",
        out("rax") _, out("rdx") _, out("rcx") _, options(nostack)); }
}

unsafe extern "C" fn fallback_values(_: u64, record: *mut MappedCallRecord) {
    unsafe {
        (*record).outcome = NativeOutcome { value: EgclVal::from_fixnum(42), exit: NativeExit::Returned };
        finish_values(record);
    }
}

#[test]
fn published_returns_reconstruct_zero_one_many_after_rust_retirement_and_fallback() {
    use egcl_compiler::t2::native_transfer::emit_published_call_entry;
    let code = JitBuffer::new(&emit_published_call_entry(
        true, prepare_values, finish_values, finish_values, fallback_values,
    )).unwrap();
    for fallback in [false, true] {
        MV_RETURN_FALLBACK.with(|state| state.set(fallback));
        for count in [0, 1, 3] {
            MV_RETURN_COUNT.with(|state| state.set(count));
            let mut context = egcl_rt::call_table::NativeCallContext {
                request: std::ptr::null_mut(), capture: std::ptr::null(),
                args: std::ptr::null_mut(), nargs: 0,
            };
            let mut observed = ObservedReturn::default();
            unsafe { observe_return(code.as_ptr(), &mut context, &mut observed); }
            assert_eq!(observed.primary, EgclVal::from_fixnum(42).0);
            assert_eq!(observed.carry, u8::from(count != 1), "{observed:?}; fallback={fallback}");
            if count != 1 {
                assert_eq!(observed.count, count);
                assert_eq!(observed.payload, if count > 1 { MV_RETURN_PAYLOAD.as_ptr() as usize } else { 0 });
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Preparation {
    Ready,
    Declined,
    Failed,
    CheckedFailed,
}
struct State {
    preparation: Preparation,
    request: *mut TransferCallRequest,
    nargs: usize,
    events: Vec<&'static str>,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

unsafe extern "C" fn prepare(cell: u64, record: *mut MappedCallRecord) {
    assert_eq!(cell, 0x1234);
    let record = unsafe { &mut *record };
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(record.request, state.request);
        let context = unsafe { &*record.context };
        assert_eq!(context.nargs, state.nargs);
        for i in 0..context.nargs {
            assert_eq!(
                unsafe { *context.args.add(i) },
                EgclVal::from_fixnum(i as i64 + 1)
            );
        }
        assert!(!record.cold_entry.is_null());
        state.events.push("prepare");
        record.entry = std::ptr::null();
        record.forward = std::ptr::null();
        record.outcome = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        match state.preparation {
            Preparation::Ready => {
                record.owner = state.request.cast();
                record.entry = child as *const u8;
                record.activation = state.request.cast();
            }
            Preparation::Declined | Preparation::CheckedFailed => {}
            Preparation::Failed => {
                record.outcome = NativeOutcome {
                    value: EgclVal::from_fixnum(17),
                    exit: NativeExit::Transfer,
                };
            }
        }
    });
}
unsafe extern "C" fn child(activation: *mut u8) -> EgclVal {
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(activation, state.request.cast());
        state.events.push("child");
    });
    EgclVal::from_fixnum(42)
}
unsafe extern "C" fn finish(record: *mut MappedCallRecord) {
    unsafe { (*record).return_values = egcl_rt::native_transfer::NativeReturnValues::SINGLE; }
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(unsafe { (*record).request }, state.request);
        state.events.push("finish");
    });
}
unsafe extern "C" fn legacy(request: *mut TransferCallRequest) -> EgclVal {
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(request, state.request);
        state.events.push("legacy");
    });
    EgclVal::from_fixnum(43)
}
unsafe extern "C" fn capture(
    request: *mut TransferCallRequest,
    value: EgclVal,
    exit: NativeExit,
) -> EgclVal {
    assert_eq!(value, EgclVal::from_fixnum(17));
    assert_eq!(exit, NativeExit::Transfer);
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(request, state.request);
        state.events.push("capture");
    });
    EgclVal::from_fixnum(44)
}

#[test]
fn published_entries_accept_register_and_slice_arguments() {
    use egcl_compiler::t2::native_transfer::emit_published_call_entry;
    use egcl_rt::call_table::NativeCallContext;
    unsafe extern "C" fn fallback(_: u64, record: *mut MappedCallRecord) {
        let request = unsafe { (*record).request };
        let value = unsafe { legacy(request) };
        unsafe {
            (*record).return_values = egcl_rt::native_transfer::NativeReturnValues::SINGLE;
            (*record).outcome = if STATE.with_borrow(|state| {
                matches!(
                    state.as_ref().unwrap().preparation,
                    Preparation::CheckedFailed
                )
            }) {
                NativeOutcome {
                    value: EgclVal::from_fixnum(17),
                    exit: NativeExit::Transfer,
                }
            } else {
                NativeOutcome {
                    value,
                    exit: NativeExit::Returned,
                }
            };
        }
    }
    for (n, slice) in (0..=3)
        .map(|n| (n, false))
        .chain((0..=5).map(|n| (n, true)))
    {
        let code = JitBuffer::new(&emit_published_call_entry(
            slice, prepare, finish, finish, fallback,
        ))
        .unwrap();
        for (preparation, expected) in [
            (Preparation::Ready, 42),
            (Preparation::Declined, 43),
            (Preparation::Failed, 44),
            (Preparation::CheckedFailed, 44),
        ] {
            let mut args = std::array::from_fn::<_, 5, _>(|i| {
                if slice && i < n {
                    EgclVal::from_fixnum(i as i64 + 1)
                } else {
                    NIL
                }
            });
            let mut request = TransferCallRequest {
                symbol: 9,
                nargs: 777,
                args: std::ptr::null_mut(),
                activation: std::ptr::null_mut(),
            };
            let mut context = NativeCallContext {
                request: (&mut request as *mut TransferCallRequest).cast(),
                capture: capture as *const u8,
                args: if slice {
                    std::ptr::null_mut()
                } else {
                    args.as_mut_ptr()
                },
                nargs: 0,
            };
            STATE.with_borrow_mut(|state| {
                *state = Some(State {
                    preparation,
                    request: &mut request,
                    nargs: n,
                    events: Vec::new(),
                })
            });
            let result = if slice {
                let call: unsafe extern "C" fn(
                    u64,
                    usize,
                    *mut EgclVal,
                    *mut NativeCallContext,
                ) -> EgclVal = unsafe { std::mem::transmute(code.as_ptr()) };
                unsafe { call(0x1234, n, args.as_mut_ptr(), &mut context) }
            } else {
                let call: unsafe extern "C" fn(
                    u64,
                    usize,
                    EgclVal,
                    EgclVal,
                    EgclVal,
                    *mut NativeCallContext,
                ) -> EgclVal = unsafe { std::mem::transmute(code.as_ptr()) };
                unsafe {
                    call(
                        0x1234,
                        n,
                        EgclVal::from_fixnum(1),
                        EgclVal::from_fixnum(2),
                        EgclVal::from_fixnum(3),
                        &mut context,
                    )
                }
            };
            assert_eq!(result, EgclVal::from_fixnum(expected));
            assert_eq!(request.nargs, 777, "original capture request is immutable");
            assert!(request.args.is_null());
            if !slice {
                for (i, value) in args.iter().enumerate() {
                    assert_eq!(
                        *value,
                        if i < n {
                            EgclVal::from_fixnum(i as i64 + 1)
                        } else {
                            NIL
                        }
                    );
                }
            }
            let state = STATE.with_borrow_mut(Option::take).unwrap();
            assert_eq!(
                state.events,
                match preparation {
                    Preparation::Ready => vec!["prepare", "child", "finish"],
                    Preparation::Declined => vec!["prepare", "legacy"],
                    Preparation::Failed => vec!["prepare", "capture"],
                    Preparation::CheckedFailed => vec!["prepare", "legacy", "capture"],
                }
            );
        }
    }
}

#[test]
fn caller_loads_the_current_published_entry_for_each_argument_shape() {
    use egcl_compiler::t2::native_transfer::emit_published_call_veneer;
    use egcl_rt::call_table::NativeCallContext;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct Request {
        call: TransferCallRequest,
        context: std::mem::MaybeUninit<NativeCallContext>,
    }
    unsafe fn inspect(n: usize, args: &[EgclVal], context: *mut NativeCallContext) -> EgclVal {
        let context = unsafe { &*context };
        let request = unsafe { &*context.request.cast::<TransferCallRequest>() };
        assert_eq!(context.capture, capture as *const u8);
        assert_eq!(request.nargs, n);
        assert_eq!(context.nargs, n);
        assert_eq!(context.args, request.args);
        for (i, value) in args.iter().take(n).enumerate() {
            assert_eq!(*value, EgclVal::from_fixnum(i as i64 + 1));
        }
        EgclVal::from_fixnum(n as i64)
    }
    unsafe extern "C" fn registers(
        cell: u64,
        n: usize,
        a: EgclVal,
        b: EgclVal,
        c: EgclVal,
        context: *mut NativeCallContext,
    ) -> EgclVal {
        assert_eq!(cell, 0x1234);
        unsafe { inspect(n, &[a, b, c], context) }
    }
    unsafe extern "C" fn slice(
        cell: u64,
        n: usize,
        args: *mut EgclVal,
        context: *mut NativeCallContext,
    ) -> EgclVal {
        assert_eq!(cell, 0x1234);
        unsafe { inspect(n, std::slice::from_raw_parts(args, n), context) }
    }
    unsafe extern "C" fn replacement() -> EgclVal {
        EgclVal::from_fixnum(99)
    }
    let register_slot = AtomicUsize::new(registers as *const () as usize);
    let slice_slot = AtomicUsize::new(slice as *const () as usize);
    let veneer = JitBuffer::new(&emit_published_call_veneer(
        0x1234,
        &register_slot as *const _ as u64,
        &slice_slot as *const _ as u64,
        capture as *const u8,
    ))
    .unwrap();
    let call: unsafe extern "C" fn(*mut TransferCallRequest) -> EgclVal =
        unsafe { std::mem::transmute(veneer.as_ptr()) };
    for n in 0..=5 {
        let mut args: Vec<_> = (1..=n).map(|i| EgclVal::from_fixnum(i as i64)).collect();
        let mut request = Request {
            call: TransferCallRequest {
                symbol: 9,
                nargs: n,
                args: args.as_mut_ptr(),
                activation: std::ptr::null_mut(),
            },
            context: std::mem::MaybeUninit::uninit(),
        };
        register_slot.store(registers as *const () as usize, Ordering::Release);
        slice_slot.store(slice as *const () as usize, Ordering::Release);
        assert_eq!(
            unsafe { call(&mut request.call) },
            EgclVal::from_fixnum(n as i64)
        );
        register_slot.store(replacement as *const () as usize, Ordering::Release);
        slice_slot.store(replacement as *const () as usize, Ordering::Release);
        assert_eq!(unsafe { call(&mut request.call) }, EgclVal::from_fixnum(99));
    }
}
