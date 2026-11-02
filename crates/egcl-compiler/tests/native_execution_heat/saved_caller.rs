// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compose a real generated callee's exact unwind recipe with its caller's
//! register root. Root the resulting writable save word, collect, and execute
//! the real epilogue POP. This tests physical save/map composition; CLI tests
//! separately exercise the installed-owner directory and full frame walker.
use super::*;
use egcl_compiler::t2::emit::FramedCode;
use egcl_compiler::t2::x64_calls::NativeCallSites;
use egcl_compiler::t2::x64_unwind::{NativeFrameCursor, SYSV_PRESERVED_REGISTERS};
use egcl_rt::jit::JitBuffer;
use std::cell::RefCell;

#[derive(Clone)]
struct Caller {
    code_base: usize,
    sites: NativeCallSites,
    register: u8,
    expected: *const EgclVal,
}
thread_local! {
    static CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
    static OBSERVED: Cell<bool> = const { Cell::new(false) };
}
static CALL_SP: AtomicUsize = AtomicUsize::new(0);

extern "C" fn poll_with_return(request: *mut u64, return_pc: usize) -> u64 {
    let caller = CALLER.with(|slot| slot.borrow().clone());
    let Some(caller) = caller else {
        return super::poll(request);
    };
    let call_sp = CALL_SP.load(Ordering::Acquire);
    let frame_bytes = caller.sites.unwind_recipe().unwrap().frame_bytes() as usize;
    let body_sp = call_sp - 8 - frame_bytes;
    let cursor = NativeFrameCursor {
        pc: return_pc,
        call_sp: body_sp,
        registers: [None; 6],
    };
    let bounds = body_sp..call_sp + 8;
    let step = caller
        .sites
        .unwind(caller.code_base, &cursor, bounds.clone(), |address| {
            (address >= bounds.start && address + 8 <= bounds.end)
                .then(|| unsafe { (address as *const usize).read() })
        })
        .expect("real poll return PC must unwind the generated frame");
    assert_eq!(step.body_sp, Some(body_sp));
    let index = SYSV_PRESERVED_REGISTERS
        .iter()
        .position(|&register| register == caller.register)
        .unwrap();
    let saved = step.caller.registers[index].expect("new heat-register save word") as *mut EgclVal;
    assert_eq!(
        saved as usize,
        body_sp
            + caller
                .sites
                .unwind_recipe()
                .unwrap()
                .saved_offset(caller.register)
                .unwrap() as usize
    );
    // Current raw heat pointer is absent from this frame's GC map; the SAME
    // register in its caller is a tagged value at this writable saved location.
    assert_eq!(unsafe { *saved }, unsafe { *caller.expected });
    let before = unsafe { (*saved).to_raw() };
    egcl_rt::rooted_ref!(_caller_register_root = unsafe { &mut *saved });
    let result = super::poll(request);
    assert_ne!(
        unsafe { (*saved).to_raw() },
        before,
        "caller save word must actually relocate"
    );
    assert_eq!(unsafe { *saved }, unsafe { *caller.expected });
    OBSERVED.with(|slot| slot.set(true));
    result
}

/// Recognize the actual two-instruction hot arm, independently of emitter
/// selection policy or a runtime flag. The fixture contains no other 32-bit
/// memory decrement. A legacy counter first materializes RAX instead.
pub(super) fn selected_register(framed: &FramedCode) -> Option<u8> {
    use iced_x86::{Decoder, DecoderOptions, MemorySize, Mnemonic, Register};
    let mut selected = None;
    let instructions: Vec<_> = Decoder::new(64, &framed.code, DecoderOptions::NONE)
        .into_iter()
        .collect();
    for pair in instructions.windows(2) {
        if pair[0].mnemonic() != Mnemonic::Sub
            || pair[0].memory_size() != MemorySize::UInt32
            || pair[1].mnemonic() != Mnemonic::Jle
        {
            continue;
        }
        let register = match pair[0].memory_base() {
            Register::R12 => 12,
            Register::R14 => 14,
            Register::RBX => 3,
            _ => continue,
        };
        if let Some(old) = selected {
            assert_eq!(old, register);
        }
        selected = Some(register);
    }
    selected
}

pub(super) fn poll_veneer() -> JitBuffer {
    // Forward the real generated CALL's return PC in RSI without changing its
    // stack. A tail jump preserves that exact PC for production unwind lookup.
    let mut code = vec![0x48, 0x8b, 0x34, 0x24, 0x48, 0xb8];
    code.extend_from_slice(&(poll_with_return as *const () as u64).to_le_bytes());
    code.extend_from_slice(&[0xff, 0xe0]);
    JitBuffer::new(&code).unwrap()
}

pub(super) fn check(
    framed: &FramedCode,
    code: &JitBuffer,
    activation: &mut [EgclVal],
    execution: &LispExecutionContext,
) {
    let register = selected_register(framed).expect("actual register arm");
    assert!(matches!(register, 3 | 12 | 14));
    // Wrapper entry: RDI=activation, RSI=payload. A single push aligns RSP
    // before calling the generated managed entry and preserves Rust's register.
    let mut wrapper = Vec::new();
    if register >= 8 {
        wrapper.push(0x41);
    }
    wrapper.push(0x50 + (register & 7));
    wrapper.extend_from_slice(&[
        0x48 | (((register >> 3) & 1) << 2),
        0x8b,
        0xc0 | ((register & 7) << 3) | 6,
    ]); // mov reg,rsi
    wrapper.extend_from_slice(&[0x48, 0x89, 0xe0, 0x49, 0xbb]); // rax=rsp; r11=&CALL_SP
    wrapper.extend_from_slice(&(&CALL_SP as *const _ as u64).to_le_bytes());
    wrapper.extend_from_slice(&[0x49, 0x89, 0x03, 0x48, 0xb8]); // [r11]=rax; rax=callee
    wrapper.extend_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    wrapper.extend_from_slice(&[0xff, 0xd0]);
    wrapper.extend_from_slice(&[0x48 | ((register >> 3) & 1), 0x8b, 0xc0 | (register & 7)]); // rax=restored reg
    if register >= 8 {
        wrapper.push(0x41);
    }
    wrapper.extend_from_slice(&[0x58 + (register & 7), 0xc3]);
    let wrapper = JitBuffer::new(&wrapper).unwrap();
    let run: extern "C" fn(*mut EgclVal, EgclVal) -> EgclVal =
        unsafe { std::mem::transmute(wrapper.as_ptr()) };
    // Allocate only after all executable buffers are published, so this fresh
    // object cannot have been promoted by compilation/publication collections.
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(456.25);
    }
    egcl_rt::rooted!(expected = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
    CALLER.with(|slot| {
        *slot.borrow_mut() = Some(Caller {
            code_base: code.as_ptr() as usize,
            sites: framed.native_calls.as_ref().unwrap().clone(),
            register,
            expected: &*expected,
        })
    });
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CALLER.with(|slot| *slot.borrow_mut() = None);
            COLLECT.with(|slot| slot.set(false));
        }
    }
    let _reset = Reset;
    OBSERVED.with(|slot| slot.set(false));
    execution.request_poll();
    COLLECT.with(|slot| slot.set(true));
    egcl_rt::rooted!(actual = run(activation.as_mut_ptr(), *expected));
    COLLECT.with(|slot| slot.set(false));
    assert!(OBSERVED.with(Cell::get));
    assert_eq!(
        *actual, *expected,
        "actual callee POP restores the relocated caller register"
    );
}
