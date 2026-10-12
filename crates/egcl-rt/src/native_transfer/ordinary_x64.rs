// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary Lisp register entry at the outer native segment boundary.

use super::{with_native_segment, NativeOutcome, NativeReturnValues, NativeSegment, SegmentUnavailable};
use crate::exec_context::LispExecutionContext;
use crate::stack::EgclStack;
use crate::value::EgclVal;

/// The tuple is borrowed from the execution's rooted MV buffer. It is valid
/// only for successful returns and until the next safepoint or buffer mutation.
pub struct OrdinaryNativeOutcome {
    pub outcome: NativeOutcome,
    pub values: NativeReturnValues,
}

#[repr(C)]
struct Invocation {
    entry: *const u8,
    execution: *const LispExecutionContext,
    closure: EgclVal,
    arguments: [EgclVal; 4],
    nargs: usize,
    values: NativeReturnValues,
}

/// Enter a bounded ordinary register-ABI root through a native segment anchor.
/// This first adapter accepts zero through four arguments.
///
/// # Safety
/// `entry` must implement the ordinary x86-64 Lisp ABI and its entry admission
/// checks must already be satisfied. `execution` belongs to the running Lisp
/// execution and retains its active native control throughout this call.
/// Closure and arguments must already be rooted; generated code must publish
/// their current native homes before collecting or yielding. As with
/// `invoke_native_segment`, cold exits may discard only generated frames after
/// their roots and cleanup obligations are retired. Root the returned primary
/// before allocating; the returned MV descriptor is borrowed, not itself a root.
pub unsafe fn invoke_ordinary_native_segment(
    entry: *const u8,
    execution: &LispExecutionContext,
    closure: EgclVal,
    arguments: &[EgclVal],
    stack: &EgclStack,
) -> Result<OrdinaryNativeOutcome, SegmentUnavailable> {
    if arguments.len() > 4 {
        return Err(SegmentUnavailable);
    }
    let mut request = Invocation {
        entry, execution, closure,
        arguments: [crate::value::NIL; 4],
        nargs: arguments.len(),
        values: NativeReturnValues::SINGLE,
    };
    request.arguments[..arguments.len()].copy_from_slice(arguments);
    let outcome = unsafe {
        with_native_segment(stack, |anchor, out| enter(&mut request, anchor, out))
    }?;
    Ok(OrdinaryNativeOutcome { outcome, values: request.values })
}

#[unsafe(naked)]
unsafe extern "C" fn enter(
    _request: *mut Invocation,
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
) {
    core::arch::naked_asm!(
        ".cfi_startproc", "endbr64",
        "push rbp", ".cfi_def_cfa_offset 16", ".cfi_offset rbp, -16",
        "push rbx", ".cfi_def_cfa_offset 24", ".cfi_offset rbx, -24",
        "push r12", ".cfi_def_cfa_offset 32", ".cfi_offset r12, -32",
        "push r13", ".cfi_def_cfa_offset 40", ".cfi_offset r13, -40",
        "push r14", ".cfi_def_cfa_offset 48", ".cfi_offset r14, -48",
        "push r15", ".cfi_def_cfa_offset 56", ".cfi_offset r15, -56",
        // Overflow argument and padding, three private pointers, FP controls,
        // and alignment. Across-call state is outside the callee's argument area.
        "sub rsp, 56", ".cfi_def_cfa_offset 112",
        "mov [rsp + 16], rsi", "mov [rsp + 24], rdx", "mov [rsp + 32], rdi",
        "stmxcsr [rsp + 40]", "fnstcw [rsp + 44]",
        "mov [rsi + {sp}], rsp",
        "lea rax, [rip + 2f]", "mov [rsi + {landing}], rax",
        "lea rax, [rip + 3f]", "mov [rsi + {entry_pc}], rax",
        "mov r10, rdi", "mov r11, [r10 + {entry}]",
        "mov r13, [r10 + {execution}]", "mov rax, [r10 + {closure}]",
        "mov rcx, [r10 + {nargs}]",
        "mov rdi, [r10 + {args}]", "mov rsi, [r10 + {args} + 8]",
        "mov rdx, [r10 + {args} + 24]", "mov [rsp], rdx",
        "mov rdx, [r10 + {args} + 16]", "call r11",
        "3:", "mov r10, [rsp + 32]", "jnc 4f",
        "mov [r10 + {count}], rdx", "mov [r10 + {payload}], rcx", "jmp 5f",
        "4:", "mov qword ptr [r10 + {count}], 1",
        "mov qword ptr [r10 + {payload}], 0",
        "5:", "xor edx, edx",
        "2:", "endbr64",
        "mov rcx, [rsp + 24]", "mov [rcx + {value}], rax", "mov [rcx + {exit}], rdx",
        "mov rcx, [rsp + 16]", "mov qword ptr [rcx + {sp}], 0",
        "mov qword ptr [rcx + {landing}], 0", "mov qword ptr [rcx + {entry_pc}], 0",
        "ldmxcsr [rsp + 40]", "fldcw [rsp + 44]", "cld",
        "add rsp, 56", ".cfi_def_cfa_offset 56",
        "pop r15", ".cfi_def_cfa_offset 48", ".cfi_restore r15",
        "pop r14", ".cfi_def_cfa_offset 40", ".cfi_restore r14",
        "pop r13", ".cfi_def_cfa_offset 32", ".cfi_restore r13",
        "pop r12", ".cfi_def_cfa_offset 24", ".cfi_restore r12",
        "pop rbx", ".cfi_def_cfa_offset 16", ".cfi_restore rbx",
        "pop rbp", ".cfi_def_cfa_offset 8", ".cfi_restore rbp",
        "ret", ".cfi_endproc",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing = const std::mem::offset_of!(NativeSegment, landing_pc),
        entry_pc = const std::mem::offset_of!(NativeSegment, entry_return_pc),
        entry = const std::mem::offset_of!(Invocation, entry),
        execution = const std::mem::offset_of!(Invocation, execution),
        closure = const std::mem::offset_of!(Invocation, closure),
        nargs = const std::mem::offset_of!(Invocation, nargs),
        args = const std::mem::offset_of!(Invocation, arguments),
        count = const std::mem::offset_of!(Invocation, values),
        payload = const std::mem::offset_of!(Invocation, values) + 8,
        value = const std::mem::offset_of!(NativeOutcome, value),
        exit = const std::mem::offset_of!(NativeOutcome, exit),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_context::{ExecutionKind, LispExecutionContext};
    use crate::native_transfer::{current_segment, NativeExit};
    use crate::value::{EgclVal, NIL};

    // Exercise all three argument registers, the overflow word, closure/count,
    // and the reserved execution register before returning a single value.
    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_arguments() {
        core::arch::naked_asm!(
            "endbr64", "cmp rcx, 4", "jne 2f",
            "cmp rax, {nil}", "jne 2f",
            "cmp qword ptr [r13 + 8], 77", "jne 2f",
            "add rdi, rsi", "add rdi, rdx", "add rdi, [rsp + 8]",
            "mov rax, rdi", "clc", "ret",
            "2:", "xor eax, eax", "clc", "ret",
            nil = const NIL.0,
        );
    }

    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_zero_values() {
        core::arch::naked_asm!(
            "endbr64", "mov rax, {nil}", "xor edx, edx", "xor ecx, ecx",
            "stc", "ret", nil = const NIL.0,
        );
    }

    static TUPLE: [EgclVal; 2] = [EgclVal(336), EgclVal(344)];

    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_many_values() {
        core::arch::naked_asm!(
            "endbr64", "mov eax, 336", "mov edx, 2", "lea rcx, [rip + {tuple}]",
            "stc", "ret", tuple = sym TUPLE,
        );
    }

    extern "C" fn anchor_address() -> *mut NativeSegment { current_segment() }

    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_cold_transfer() {
        core::arch::naked_asm!(
            "endbr64", "sub rsp, 8", "call {anchor}", "add rsp, 8",
            "mov rdi, rax", "mov esi, 336", "mov edx, 1",
            "mov rbp, 1", "mov rbx, 2", "mov r12, 3", "mov r13, 4",
            "mov r14, 5", "mov r15, 6", "jmp {leave}",
            anchor = sym anchor_address,
            leave = sym crate::native_transfer::leave_native_segment,
        );
    }

    #[unsafe(naked)]
    unsafe extern "C" fn preserved_register_probe(
        _request: *mut Invocation, _anchor: *mut NativeSegment, _out: *mut NativeOutcome,
    ) -> u64 {
        core::arch::naked_asm!(
            "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
            "sub rsp, 8", "mov rbp, 11", "mov rbx, 12", "mov r12, 13",
            "mov r13, 14", "mov r14, 15", "mov r15, 16", "call {enter}",
            "mov eax, 1", "cmp rbp, 11", "jne 2f", "cmp rbx, 12", "jne 2f",
            "cmp r12, 13", "jne 2f", "cmp r13, 14", "jne 2f",
            "cmp r14, 15", "jne 2f", "cmp r15, 16", "jne 2f", "xor eax, eax",
            "2:", "add rsp, 8", "pop r15", "pop r14", "pop r13", "pop r12",
            "pop rbx", "pop rbp", "ret", enter = sym enter,
        );
    }

    #[test]
    fn ordinary_root_restores_preserved_registers_after_normal_and_cold_returns() {
        crate::gc::ensure_heap_initialized();
        crate::thread::current_thread_id();
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 77, 1);
        for entry in [ordinary_arguments as *const u8, ordinary_cold_transfer as *const u8] {
            let mut request = Invocation {
                entry, execution: &execution, closure: NIL,
                arguments: [1, 2, 3, 4].map(EgclVal::from_fixnum), nargs: 4,
                values: NativeReturnValues::SINGLE,
            };
            let mut mismatch = 1;
            unsafe {
                with_native_segment(crate::current_stack(), |anchor, out| {
                    mismatch = preserved_register_probe(&mut request, anchor, out);
                })
            }.unwrap();
            assert_eq!(mismatch, 0);
        }
    }

    extern "C" fn check_anchor(return_pc: usize, caller_sp: usize) -> u64 {
        let anchor = current_segment();
        assert!(!anchor.is_null());
        let anchor = unsafe { &*anchor };
        u64::from(anchor.return_pc() == return_pc && anchor.saved_sp == caller_sp)
    }

    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_anchor() {
        core::arch::naked_asm!(
            "endbr64", "push rbp", "mov rbp, rsp",
            "mov rdi, [rbp + 8]", "lea rsi, [rbp + 16]",
            "call {check}", "pop rbp", "clc", "ret",
            check = sym check_anchor,
        );
    }

    #[test]
    fn ordinary_root_entry_passes_register_arguments_and_preserves_results() {
        crate::gc::ensure_heap_initialized();
        crate::thread::current_thread_id();
        assert!(crate::native_transfer::is_supported());
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 77, 1);
        let args = [1, 2, 3, 4].map(EgclVal::from_fixnum);
        let single = unsafe {
            invoke_ordinary_native_segment(
                ordinary_arguments as *const u8, &execution, NIL, &args, crate::current_stack(),
            )
        }.unwrap();
        assert_eq!(single.outcome.value, EgclVal::from_fixnum(10));
        assert_eq!(single.outcome.exit, NativeExit::Returned);
        assert_eq!(single.values.count, 1);
        assert!(single.values.payload.is_null());
        let zero = unsafe {
            invoke_ordinary_native_segment(
                ordinary_zero_values as *const u8, &execution, NIL, &[], crate::current_stack(),
            )
        }.unwrap();
        assert_eq!(zero.outcome.value, NIL);
        assert_eq!(zero.values.count, 0);
        assert!(zero.values.payload.is_null());
        let many = unsafe {
            invoke_ordinary_native_segment(
                ordinary_many_values as *const u8, &execution, NIL, &[], crate::current_stack(),
            )
        }.unwrap();
        assert_eq!(many.outcome.value, TUPLE[0]);
        assert_eq!(many.values.count, 2);
        assert_eq!(many.values.payload, TUPLE.as_ptr());
        let cold = unsafe {
            invoke_ordinary_native_segment(
                ordinary_cold_transfer as *const u8, &execution, NIL, &[], crate::current_stack(),
            )
        }.unwrap();
        assert_eq!(cold.outcome.exit, NativeExit::Transfer);
        assert_eq!(cold.outcome.value, TUPLE[0]);
        let anchor = unsafe {
            invoke_ordinary_native_segment(
                ordinary_anchor as *const u8, &execution, NIL, &[], crate::current_stack(),
            )
        }.unwrap();
        assert_eq!(anchor.outcome.value.0, 1);
        assert!(current_segment().is_null());
    }
}
