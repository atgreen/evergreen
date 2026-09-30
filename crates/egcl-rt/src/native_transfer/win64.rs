//! Win64 physical boundary. The same instructions are compiled for Linux unit
//! probes using Rust's `win64` ABI; only COFF unwind directives are omitted there.

use super::{NativeExit, NativeOutcome, NativeSegment, EgclStack};

#[cfg(windows)]
pub(super) fn is_supported() -> bool {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessMitigationPolicy, ProcessControlFlowGuardPolicy,
        ProcessUserShadowStackPolicy,
    };
    [ProcessControlFlowGuardPolicy, ProcessUserShadowStackPolicy]
        .into_iter()
        .all(|policy| {
            // Both policy structures are one DWORD of flags. A sentinel detects
            // emulators that report success without filling the output buffer.
            // Refuse unknown bits as well as active CFG/shadow-stack policies.
            let mut flags = u32::MAX;
            let ok = unsafe {
                GetProcessMitigationPolicy(
                    GetCurrentProcess(),
                    policy,
                    (&mut flags as *mut u32).cast(),
                    std::mem::size_of_val(&flags),
                )
            };
            ok != 0 && flags == 0
        })
}

#[cfg(windows)]
macro_rules! seh {
    ($directive:literal) => {
        $directive
    };
}
#[cfg(not(windows))]
macro_rules! seh {
    ($directive:literal) => {
        ""
    };
}

#[unsafe(naked)]
pub(super) unsafe extern "win64" fn enter(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: *const EgclStack,
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
) {
    core::arch::naked_asm!(
        // Keep the symbol operand used in the ELF test build too.
        "// {function}",
        seh!(".seh_proc {function}"),
        "endbr64",
        "push rbp", seh!(".seh_pushreg rbp"),
        "push rbx", seh!(".seh_pushreg rbx"),
        "push rdi", seh!(".seh_pushreg rdi"),
        "push rsi", seh!(".seh_pushreg rsi"),
        "push r12", seh!(".seh_pushreg r12"),
        "push r13", seh!(".seh_pushreg r13"),
        "push r14", seh!(".seh_pushreg r14"),
        "push r15", seh!(".seh_pushreg r15"),
        // 32-byte home area, anchor/out pointers, FP control, alignment, then
        // XMM6-XMM15. Eight pushes and 232 bytes leave RSP aligned for calls.
        "sub rsp, 232", seh!(".seh_stackalloc 232"),
        "movaps [rsp + 64], xmm6", seh!(".seh_savexmm xmm6, 64"),
        "movaps [rsp + 80], xmm7", seh!(".seh_savexmm xmm7, 80"),
        "movaps [rsp + 96], xmm8", seh!(".seh_savexmm xmm8, 96"),
        "movaps [rsp + 112], xmm9", seh!(".seh_savexmm xmm9, 112"),
        "movaps [rsp + 128], xmm10", seh!(".seh_savexmm xmm10, 128"),
        "movaps [rsp + 144], xmm11", seh!(".seh_savexmm xmm11, 144"),
        "movaps [rsp + 160], xmm12", seh!(".seh_savexmm xmm12, 160"),
        "movaps [rsp + 176], xmm13", seh!(".seh_savexmm xmm13, 176"),
        "movaps [rsp + 192], xmm14", seh!(".seh_savexmm xmm14, 192"),
        "movaps [rsp + 208], xmm15", seh!(".seh_savexmm xmm15, 208"),
        seh!(".seh_endprologue"),
        "mov [rsp + 32], r9",
        "mov rax, [rsp + 336]", // fifth argument at original RSP + 40
        "mov [rsp + 40], rax",
        "stmxcsr [rsp + 48]", "fnstcw [rsp + 52]",
        "mov [r9 + {sp}], rsp",
        "lea rax, [rip + 2f]", "mov [r9 + {landing}], rax",
        "mov r11, rcx", "mov rcx, rdx", "mov rdx, r8", "mov r8, r9",
        "call r11",
        "xor edx, edx",
        "2:", "endbr64",
        "mov rcx, [rsp + 40]",
        "mov [rcx + {value}], rax", "mov [rcx + {exit}], rdx",
        "mov rcx, [rsp + 32]",
        "mov qword ptr [rcx + {sp}], 0", "mov qword ptr [rcx + {landing}], 0",
        "ldmxcsr [rsp + 48]", "fldcw [rsp + 52]", "cld",
        "movaps xmm6, [rsp + 64]", "movaps xmm7, [rsp + 80]",
        "movaps xmm8, [rsp + 96]", "movaps xmm9, [rsp + 112]",
        "movaps xmm10, [rsp + 128]", "movaps xmm11, [rsp + 144]",
        "movaps xmm12, [rsp + 160]", "movaps xmm13, [rsp + 176]",
        "movaps xmm14, [rsp + 192]", "movaps xmm15, [rsp + 208]",
        // This ADD/POP*/RET sequence is a Windows-recognizable epilogue.
        "add rsp, 232",
        "pop r15", "pop r14", "pop r13", "pop r12",
        "pop rsi", "pop rdi", "pop rbx", "pop rbp", "ret",
        seh!(".seh_endproc"),
        function = sym enter,
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing = const std::mem::offset_of!(NativeSegment, landing_pc),
        value = const std::mem::offset_of!(NativeOutcome, value),
        exit = const std::mem::offset_of!(NativeOutcome, exit),
    )
}

/// Leave generated frames after helpers returned and transfer preparation ended.
///
/// # Safety
/// `anchor` must belong to this execution's active segment. Only generated or
/// assembly frames eligible for discard may lie between this stub and landing;
/// their roots and cleanup obligations must already be settled. The invocation
/// must have passed the platform hardening check before entering generated code.
#[unsafe(naked)]
pub unsafe extern "win64" fn leave_native_segment(
    _anchor: *mut NativeSegment,
    _value: u64,
    _exit: NativeExit,
) -> ! {
    core::arch::naked_asm!(
        "endbr64", "mov rax, rdx", "mov rdx, r8",
        "mov rsp, [rcx + {sp}]", "jmp qword ptr [rcx + {landing}]",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing = const std::mem::offset_of!(NativeSegment, landing_pc),
    )
}

#[cfg(test)]
#[path = "win64_tests.rs"]
mod tests;
