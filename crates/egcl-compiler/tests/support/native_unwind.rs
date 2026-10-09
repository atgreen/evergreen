// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::x64_unwind::NativeFrameCursor;

struct WalkState {
    inner: *const NativeCallSites,
    outer: *const NativeCallSites,
    inner_base: usize,
    outer_base: usize,
    top: *const [usize; 2],
    wrapper_sp: *const usize,
    wrapper_return: usize,
    original: usize,
    relocated: usize,
    collections: usize,
}
thread_local! { static WALK: Cell<*mut WalkState> = const { Cell::new(std::ptr::null_mut()) }; }

unsafe extern "C" fn collect_through_caller_save(_: *mut u8, out: *mut NativeOutcome) {
    let state = unsafe { &mut *WALK.with(Cell::get) };
    let [pc, call_sp] = unsafe { *state.top };
    let wrapper_sp = unsafe { *state.wrapper_sp };
    let cursor = NativeFrameCursor {
        pc,
        call_sp,
        registers: [None; 6],
    };
    let read_word = |address: usize| Some(unsafe { (address as *const usize).read() });
    let inner = unsafe { &*state.inner }
        .unwind(state.inner_base, &cursor, call_sp..wrapper_sp, read_word)
        .expect("unwind the actual inner generated frame");
    let outer = unsafe { &*state.outer }
        .unwind(
            state.outer_base,
            &inner.caller,
            call_sp..wrapper_sp,
            read_word,
        )
        .expect("unwind the suspended generated caller");
    assert_eq!(
        (outer.caller.pc, outer.caller.call_sp),
        (state.wrapper_return, wrapper_sp)
    );
    let root_address = outer.caller.registers[0].expect("outer prologue saved wrapper RBX");
    let root = unsafe { &mut *(root_address as *mut EgclVal) };
    // This is the only root for the wrapper's fresh heap object. Both generated
    // activations contain only NIL. The machine epilogue must reload this word.
    egcl_rt::rooted_ref!(_root = &mut *root);
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        root.to_raw() as usize,
        state.original,
        "the save-slot root must actually move"
    );
    assert_eq!(root.as_double_float(), 1234.5);
    state.relocated = root.to_raw() as usize;
    state.collections += 1;
    unsafe {
        out.write(NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        });
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn nested_generated_frames_relocate_the_register_word_the_caller_restores() {
    let veneer = JitBuffer::new(&emit_helper_veneer(
        collect_through_caller_save,
        std::ptr::null(),
    ))
    .unwrap();
    let mut top = Box::new([0usize; 2]);
    let probe = capture_call(veneer.as_ptr(), top.as_mut_ptr(), false);
    let inner_ir = build_from_bytecode_for_transfers(&body()).unwrap();
    let (inner, _) = emit_framed_transfers(&inner_ir, probe.as_ptr() as u64, 4).unwrap();
    let inner_code = JitBuffer::new(&inner.code).unwrap();
    egcl_rt::rooted!(inner_activation = vec![NIL; 4 + inner.shadow_root_slots as usize]);
    // Adapter tail-jumps into the inner body with its own managed activation.
    // Thus its original return address identifies the outer Invoke directly.
    let mut enter_inner = vec![0x48, 0xbf]; // mov rdi,activation
    enter_inner.extend_from_slice(&(inner_activation.as_mut_ptr() as u64).to_le_bytes());
    enter_inner.extend_from_slice(&[0x48, 0xb8]);
    enter_inner.extend_from_slice(&(inner_code.as_ptr() as u64).to_le_bytes());
    enter_inner.extend_from_slice(&[0xff, 0xe0]);
    let enter_inner = JitBuffer::new(&enter_inner).unwrap();
    let mut outer_body = body();
    outer_body.n_locals = 8;
    outer_body.arity = 8;
    outer_body.min_args = 8;
    outer_body.max_args = Some(8);
    outer_body.max_stack = 8;
    outer_body.code = (0..8).map(Instr::LoadLocal).collect();
    outer_body.code.extend([
        Instr::CallNamed {
            sym: 123456,
            nargs: 8,
        },
        Instr::Return,
    ]);
    let outer_ir = build_from_bytecode_for_transfers(&outer_body).unwrap();
    let (outer, _) = emit_framed_transfers(&outer_ir, enter_inner.as_ptr() as u64, 16).unwrap();
    let outer_code = JitBuffer::new(&outer.code).unwrap();
    let recipe = outer
        .native_calls
        .as_ref()
        .unwrap()
        .unwind_recipe()
        .unwrap();
    assert!(
        recipe.saved_offset(3).is_some(),
        "exercise the saved RBX word"
    );
    assert!(
        outer.native_spill_slots > 1,
        "exercise spills in addition to register saves"
    );
    egcl_rt::rooted!(outer_activation = vec![NIL; 16 + outer.shadow_root_slots as usize]);
    let mut wrapper_sp = Box::new(0usize);
    let stack = EgclStack::new(64 * 1024);
    // No Lisp allocation follows until the helper has rooted the recovered save
    // word. The embedded immediate is intentionally not a registered code root.
    let original = double(1234.5).to_raw() as usize;
    let mut wrapper = vec![0x53, 0x48, 0xbb]; // push rbx; mov rbx,root
    wrapper.extend_from_slice(&(original as u64).to_le_bytes());
    wrapper.extend_from_slice(&[0x48, 0xb8]);
    wrapper.extend_from_slice(&((&mut *wrapper_sp as *mut usize) as u64).to_le_bytes());
    wrapper.extend_from_slice(&[0x48, 0x89, 0x20, 0x48, 0xb8]); // mov [rax],rsp; mov rax,outer
    wrapper.extend_from_slice(&(outer_code.as_ptr() as u64).to_le_bytes());
    wrapper.extend_from_slice(&[0xff, 0xd0]); // call rax
    let wrapper_return_offset = wrapper.len();
    wrapper.extend_from_slice(&[0x48, 0x89, 0xd8, 0x5b, 0xc3]); // mov rax,rbx; pop rbx; ret
    let wrapper = JitBuffer::new(&wrapper).unwrap();
    let mut state = WalkState {
        inner: inner.native_calls.as_ref().unwrap(),
        outer: outer.native_calls.as_ref().unwrap(),
        inner_base: inner_code.as_ptr() as usize,
        outer_base: outer_code.as_ptr() as usize,
        top: &*top,
        wrapper_sp: &*wrapper_sp,
        wrapper_return: wrapper.as_ptr() as usize + wrapper_return_offset,
        original,
        relocated: 0,
        collections: 0,
    };
    WALK.with(|slot| slot.set(&mut state));
    let result = unsafe {
        native_transfer::invoke_native_segment(
            wrapper.as_ptr(),
            outer_activation.as_mut_ptr().cast(),
            &stack,
        )
    }
    .unwrap();
    WALK.with(|slot| slot.set(std::ptr::null_mut()));
    assert_eq!(state.collections, 2);
    assert_eq!(result.exit, NativeExit::Returned);
    assert_ne!(state.relocated, state.original);
    assert_eq!(result.value.to_raw() as usize, state.relocated);
    assert_eq!(result.value.as_double_float(), 1234.5);
}

pub(super) struct DeoptWalk {
    pub calls: *const NativeCallSites,
    pub code_base: usize,
    pub top: *const [usize; 2],
    pub entry: *const [usize; 8],
    pub checked: usize,
}
thread_local! { pub(super) static DEOPT_WALK: Cell<*mut DeoptWalk> = const { Cell::new(std::ptr::null_mut()) }; }

pub(super) fn check_deopt_cursor() {
    let state = unsafe { &mut *DEOPT_WALK.with(Cell::get) };
    let [pc, call_sp] = unsafe { *state.top };
    let entry = unsafe { *state.entry };
    let calls = unsafe { &*state.calls };
    assert_eq!(calls.get(pc - state.code_base).unwrap().stack_adjust, 80);
    let cursor = NativeFrameCursor {
        pc,
        call_sp,
        registers: [None; 6],
    };
    let step = calls
        .unwind(state.code_base, &cursor, call_sp..entry[1], |address| {
            Some(unsafe { (address as *const usize).read() })
        })
        .expect("deopt temporary area must unwind to the real caller");
    assert_eq!((step.caller.pc, step.caller.call_sp), (entry[0], entry[1]));
    assert_eq!(step.body_sp, Some(call_sp + 80));
    state.checked += 1;
}

pub(super) fn capture_call(target: *const u8, capture: *mut usize, registers: bool) -> JitBuffer {
    let mut probe = vec![0x48, 0xb8]; // mov rax,capture
    probe.extend_from_slice(&(capture as u64).to_le_bytes());
    probe.extend_from_slice(&[
        0x48, 0x8b, 0x0c, 0x24, // mov rcx,[rsp]
        0x48, 0x89, 0x08, // mov [rax],rcx
        0x48, 0x8d, 0x4c, 0x24, 8, // lea rcx,[rsp+8]
        0x48, 0x89, 0x48, 8, // mov [rax+8],rcx
    ]);
    if registers {
        for (index, register) in [3u8, 5, 12, 13, 14, 15].into_iter().enumerate() {
            probe.extend_from_slice(&[
                0x48 | ((register >> 3) << 2),
                0x89,
                0x40 | ((register & 7) << 3),
                (16 + index * 8) as u8,
            ]);
        }
    }
    probe.extend_from_slice(&[0x48, 0xb8]);
    probe.extend_from_slice(&(target as u64).to_le_bytes());
    probe.extend_from_slice(&[0xff, 0xe0]);
    JitBuffer::new(&probe).unwrap()
}
