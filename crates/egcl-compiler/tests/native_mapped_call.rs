#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::emit::TransferCallRequest;
use egcl_compiler::t2::native_transfer::{MappedCallRecord, emit_mapped_call_veneer};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
use egcl_rt::value::{EgclVal, NIL};
use std::cell::RefCell;

#[derive(Clone, Copy)]
enum Preparation {
    Ready,
    Declined,
    Failed,
}
struct State {
    preparation: Preparation,
    request: *mut TransferCallRequest,
    events: Vec<&'static str>,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

unsafe extern "C" fn prepare(cell: u64, record: *mut MappedCallRecord) {
    assert_eq!(cell, 0x1234);
    let record = unsafe { &mut *record };
    STATE.with_borrow_mut(|state| {
        let state = state.as_mut().unwrap();
        assert_eq!(record.request, state.request);
        assert!(!record.cold_entry.is_null());
        state.events.push("prepare");
        record.entry = std::ptr::null();
        record.outcome = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        match state.preparation {
            Preparation::Ready => {
                record.entry = child as *const u8;
                record.activation = state.request.cast();
            }
            Preparation::Declined => {}
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
fn mapped_preparation_distinguishes_entry_decline_and_failure() {
    let code = JitBuffer::new(&emit_mapped_call_veneer(
        0x1234,
        prepare,
        finish,
        legacy as *const u8,
        capture as *const u8,
    ))
    .unwrap();
    let call: unsafe extern "C" fn(*mut TransferCallRequest) -> EgclVal =
        unsafe { std::mem::transmute(code.as_ptr()) };
    for (preparation, expected) in [
        (Preparation::Ready, 42),
        (Preparation::Declined, 43),
        (Preparation::Failed, 44),
    ] {
        let mut request = TransferCallRequest {
            symbol: 9,
            nargs: 0,
            args: std::ptr::null_mut(),
            activation: std::ptr::null_mut(),
        };
        STATE.with_borrow_mut(|state| {
            *state = Some(State {
                preparation,
                request: &mut request,
                events: Vec::new(),
            })
        });
        assert_eq!(
            unsafe { call(&mut request) },
            EgclVal::from_fixnum(expected)
        );
        let state = STATE.with_borrow_mut(Option::take).unwrap();
        assert_eq!(
            state.events,
            match preparation {
                Preparation::Ready => vec!["prepare", "child", "finish"],
                Preparation::Declined => vec!["prepare", "legacy"],
                Preparation::Failed => vec!["prepare", "capture"],
            }
        );
    }
}
