// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary Lisp register entry at the outer native segment boundary.

use super::{
    NativeOutcome, NativeReturnValues, NativeSegment, SegmentUnavailable, with_native_segment,
};
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
    arguments: *const EgclVal,
    nargs: usize,
    overflow_bytes: usize,
    values: NativeReturnValues,
}

/// Enter an ordinary register-ABI root through a native segment anchor.
/// Arguments after the first three occupy a caller-owned, aligned stack area.
///
/// # Safety
/// `entry` must implement the ordinary x86-64 Lisp ABI and its entry admission
/// checks, including space for the overflow arguments, must already be satisfied.
/// `execution` belongs to the running Lisp execution and retains its active
/// native control throughout this call.
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
    let overflow_bytes = arguments
        .len()
        .saturating_sub(3)
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(15))
        .map(|bytes| bytes & !15)
        .filter(|&bytes| bytes <= isize::MAX as usize)
        .ok_or(SegmentUnavailable)?;
    let mut request = Invocation {
        entry,
        execution,
        closure,
        arguments: arguments.as_ptr(),
        nargs: arguments.len(),
        overflow_bytes,
        values: NativeReturnValues::SINGLE,
    };
    let outcome =
        unsafe { with_native_segment(stack, |anchor, out| enter(&mut request, anchor, out)) }?;
    Ok(OrdinaryNativeOutcome {
        outcome,
        values: request.values,
    })
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
        // Fixed private storage stays above the variable outgoing arguments.
        // RBP locates it on normal return; the anchor's separate landing SP
        // locates it after a cold exit has discarded every callee register.
        "sub rsp, 40", ".cfi_def_cfa_offset 96",
        "mov [rsp], rsi", "mov [rsp + 8], rdx", "mov [rsp + 16], rdi",
        "stmxcsr [rsp + 24]", "fnstcw [rsp + 28]",
        "mov rbp, rsp", ".cfi_def_cfa rbp, 96",
        "mov [rsi + {landing_sp}], rsp",
        // Probe each page instead of jumping over a stack guard for large calls.
        "mov r10, rsp", "sub r10, [rdi + {overflow_bytes}]",
        "6:", "mov rax, rsp", "sub rax, r10", "cmp rax, 4096", "jb 7f",
        "sub rsp, 4096", "test byte ptr [rsp], 0", "jmp 6b",
        "7:", "mov rsp, r10", "test byte ptr [rsp], 0",
        // This remains the exact pre-CALL SP used to terminate native walks.
        "mov [rsi + {sp}], rsp",
        "lea rax, [rip + 2f]", "mov [rsi + {landing}], rax",
        "lea rax, [rip + 3f]", "mov [rsi + {entry_pc}], rax",
        "mov r10, rdi", "mov r11, [r10 + {args}]", "mov rcx, [r10 + {nargs}]",
        "mov eax, 3", "8:", "cmp rax, rcx", "jae 9f",
        "mov rdx, [r11 + rax * 8]", "mov [rsp + rax * 8 - 24], rdx",
        "inc rax", "jmp 8b",
        // Do not read beyond the supplied slice for absent register arguments.
        "9:", "mov rdi, {nil}", "mov rsi, rdi", "mov rdx, rdi",
        "test rcx, rcx", "jz 10f", "mov rdi, [r11]",
        "cmp rcx, 1", "je 10f", "mov rsi, [r11 + 8]",
        "cmp rcx, 2", "je 10f", "mov rdx, [r11 + 16]",
        "10:", "mov r13, [r10 + {execution}]", "mov rax, [r10 + {closure}]",
        "mov r11, [r10 + {entry}]", "call r11",
        "3:", "mov rsp, rbp", ".cfi_def_cfa rsp, 96",
        "mov r10, [rsp + 16]", "jnc 4f",
        "mov [r10 + {count}], rdx", "mov [r10 + {payload}], rcx", "jmp 5f",
        "4:", "mov qword ptr [r10 + {count}], 1",
        "mov qword ptr [r10 + {payload}], 0",
        "5:", "xor edx, edx",
        "2:", "endbr64",
        "mov rcx, [rsp + 8]", "mov [rcx + {value}], rax", "mov [rcx + {exit}], rdx",
        "mov rcx, [rsp]", "mov qword ptr [rcx + {sp}], 0",
        "mov qword ptr [rcx + {landing}], 0", "mov qword ptr [rcx + {entry_pc}], 0",
        "mov qword ptr [rcx + {landing_sp}], 0",
        // Reloading a control word is slow (LDMXCSR and FLDCW are
        // microcoded) and nearly always redundant: generated code rarely
        // changes either. Reload only one that differs (GitHub #487).
        "stmxcsr [rsp + 32]", "mov ecx, [rsp + 32]", "cmp ecx, [rsp + 24]", "je 11f",
        "ldmxcsr [rsp + 24]", "11:",
        "fnstcw [rsp + 36]", "mov cx, [rsp + 36]", "cmp cx, [rsp + 28]", "je 12f",
        "fldcw [rsp + 28]", "12:", "cld",
        "add rsp, 40", ".cfi_def_cfa_offset 56",
        "pop r15", ".cfi_def_cfa_offset 48", ".cfi_restore r15",
        "pop r14", ".cfi_def_cfa_offset 40", ".cfi_restore r14",
        "pop r13", ".cfi_def_cfa_offset 32", ".cfi_restore r13",
        "pop r12", ".cfi_def_cfa_offset 24", ".cfi_restore r12",
        "pop rbx", ".cfi_def_cfa_offset 16", ".cfi_restore rbx",
        "pop rbp", ".cfi_def_cfa_offset 8", ".cfi_restore rbp",
        "ret", ".cfi_endproc",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing_sp = const std::mem::offset_of!(NativeSegment, landing_sp),
        overflow_bytes = const std::mem::offset_of!(Invocation, overflow_bytes),
        nil = const crate::value::NIL.0,
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
    use crate::native_transfer::{NativeExit, current_segment};
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

    extern "C" fn anchor_address() -> *mut NativeSegment {
        current_segment()
    }

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
        _request: *mut Invocation,
        _anchor: *mut NativeSegment,
        _out: *mut NativeOutcome,
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
        for (entry, count) in [
            (ordinary_arguments as *const u8, 4),
            (ordinary_many_arguments as *const u8, 17),
            (ordinary_cold_transfer as *const u8, 17),
        ] {
            let arguments: Vec<_> = (1..=count).map(EgclVal::from_fixnum).collect();
            let mut request = Invocation {
                entry,
                execution: &execution,
                closure: NIL,
                arguments: arguments.as_ptr(),
                nargs: arguments.len(),
                overflow_bytes: ((arguments.len() - 3) * 8 + 15) & !15,
                values: NativeReturnValues::SINGLE,
            };
            let mut mismatch = 1;
            unsafe {
                with_native_segment(crate::current_stack(), |anchor, out| {
                    mismatch = preserved_register_probe(&mut request, anchor, out);
                })
            }
            .unwrap();
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

    // The callee verifies the complete overflow slice and entry alignment, then
    // returns a checksum without retaining any unrooted heap references.
    #[unsafe(naked)]
    unsafe extern "C" fn ordinary_many_arguments() {
        core::arch::naked_asm!(
            "endbr64",
            "mov r10, rsp",
            "and r10, 15",
            "cmp r10, 8",
            "jne 4f",
            "cmp qword ptr [r13 + 8], 77",
            "jne 4f",
            "xor eax, eax",
            "test rcx, rcx",
            "jz 3f",
            "add rax, rdi",
            "cmp rcx, 1",
            "je 3f",
            "add rax, rsi",
            "cmp rcx, 2",
            "je 3f",
            "add rax, rdx",
            "mov r10, 3",
            "2:",
            "cmp r10, rcx",
            "jae 3f",
            "add rax, [rsp + r10 * 8 - 16]",
            "inc r10",
            "jmp 2b",
            "3:",
            "clc",
            "ret",
            "4:",
            "mov rax, -1",
            "clc",
            "ret",
        );
    }

    #[test]
    fn ordinary_root_marshals_arbitrary_overflow_arguments_and_retires_cold_frames() {
        crate::gc::ensure_heap_initialized();
        crate::thread::current_thread_id();
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 77, 1);
        for count in [0, 1, 2, 3, 4, 5, 6, 7, 8, 17, 1028] {
            let args: Vec<_> = (1..=count).map(EgclVal::from_fixnum).collect();
            let returned = unsafe {
                invoke_ordinary_native_segment(
                    ordinary_many_arguments as *const u8,
                    &execution,
                    NIL,
                    &args,
                    crate::current_stack(),
                )
            }
            .expect("ordinary entry must accept all supplied arguments");
            assert_eq!(returned.outcome.exit, NativeExit::Returned);
            assert_eq!(
                returned.outcome.value,
                EgclVal::from_fixnum(count * (count + 1) / 2)
            );
            assert_eq!(returned.values.count, 1);
            assert!(current_segment().is_null());
            let returned = unsafe {
                invoke_ordinary_native_segment(
                    ordinary_cold_transfer as *const u8,
                    &execution,
                    NIL,
                    &args,
                    crate::current_stack(),
                )
            }
            .unwrap();
            assert_eq!(returned.outcome.exit, NativeExit::Transfer);
            assert_eq!(returned.outcome.value, TUPLE[0]);
            assert!(current_segment().is_null());
            let anchor = unsafe {
                invoke_ordinary_native_segment(
                    ordinary_anchor as *const u8,
                    &execution,
                    NIL,
                    &args,
                    crate::current_stack(),
                )
            }
            .unwrap();
            assert_eq!(
                anchor.outcome.value.0, 1,
                "walker boundary must match the actual call SP"
            );
            let many = unsafe {
                invoke_ordinary_native_segment(
                    ordinary_many_values as *const u8,
                    &execution,
                    NIL,
                    &args,
                    crate::current_stack(),
                )
            }
            .unwrap();
            assert_eq!(many.outcome.value, TUPLE[0]);
            assert_eq!(many.values.count, 2);
            assert_eq!(many.values.payload, TUPLE.as_ptr());
        }
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
                ordinary_arguments as *const u8,
                &execution,
                NIL,
                &args,
                crate::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(single.outcome.value, EgclVal::from_fixnum(10));
        assert_eq!(single.outcome.exit, NativeExit::Returned);
        assert_eq!(single.values.count, 1);
        assert!(single.values.payload.is_null());
        let zero = unsafe {
            invoke_ordinary_native_segment(
                ordinary_zero_values as *const u8,
                &execution,
                NIL,
                &[],
                crate::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(zero.outcome.value, NIL);
        assert_eq!(zero.values.count, 0);
        assert!(zero.values.payload.is_null());
        let many = unsafe {
            invoke_ordinary_native_segment(
                ordinary_many_values as *const u8,
                &execution,
                NIL,
                &[],
                crate::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(many.outcome.value, TUPLE[0]);
        assert_eq!(many.values.count, 2);
        assert_eq!(many.values.payload, TUPLE.as_ptr());
        let cold = unsafe {
            invoke_ordinary_native_segment(
                ordinary_cold_transfer as *const u8,
                &execution,
                NIL,
                &[],
                crate::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(cold.outcome.exit, NativeExit::Transfer);
        assert_eq!(cold.outcome.value, TUPLE[0]);
        let anchor = unsafe {
            invoke_ordinary_native_segment(
                ordinary_anchor as *const u8,
                &execution,
                NIL,
                &[],
                crate::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(anchor.outcome.value.0, 1);
        assert!(current_segment().is_null());
    }
}
