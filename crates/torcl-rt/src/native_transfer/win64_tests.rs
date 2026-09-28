use super::*;
use crate::native_transfer::SegmentOwner;
use crate::value::NIL;
use std::sync::atomic::{AtomicUsize, Ordering};

static DROPS: AtomicUsize = AtomicUsize::new(0);
struct DropProbe;
impl Drop for DropProbe {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

extern "win64" fn rust_helper() {
    let _drop = DropProbe;
}

#[unsafe(naked)]
unsafe extern "win64" fn clobber_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "mov rax, rsp", "and eax, 15", "cmp eax, 8", "jne 3f",
        "mov rax, [rcx]",
        "mov qword ptr [rsp + 8], 8",
        "mov qword ptr [rsp + 16], 16",
        "mov qword ptr [rsp + 24], 24",
        "mov qword ptr [rsp + 32], 32",
        "sub rsp, 56", // Rust helper's home area plus saved anchor/mode
        "mov [rsp + 32], r8", "mov [rsp + 40], rax",
        "call {helper}",
        "mov r8, [rsp + 32]", "mov rax, [rsp + 40]", "add rsp, 56",
        "mov rbp, 1",
        "mov rbx, 2",
        "mov rdi, 3",
        "mov rsi, 4",
        "mov r12, 5",
        "mov r13, 6",
        "mov r14, 7",
        "mov r15, 8",
        "pxor xmm6, xmm6",
        "pxor xmm7, xmm7",
        "pxor xmm8, xmm8",
        "pxor xmm9, xmm9",
        "pxor xmm10, xmm10",
        "pxor xmm11, xmm11",
        "pxor xmm12, xmm12",
        "pxor xmm13, xmm13",
        "pxor xmm14, xmm14",
        "pxor xmm15, xmm15",
        "sub rsp, 8", "mov dword ptr [rsp], 0x3f80", "ldmxcsr [rsp]",
        "mov word ptr [rsp], 0x077f", "fldcw [rsp]", "add rsp, 8",
        "test eax, eax", "jz 2f",
        "mov rcx, r8", "mov r8, rax", "mov edx, 336", "jmp {leave}",
        "2:", "mov eax, 336", "ret",
        "3:", "ud2",
        leave = sym leave_native_segment,
        helper = sym rust_helper,
    )
}

#[unsafe(naked)]
unsafe extern "win64" fn register_probe(
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
    _exit: u64,
) -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "push rbp",
        "push rbx",
        "push rdi",
        "push rsi",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 248", "mov r9, rcx", "mov [rsp + 32], rdx", "mov [rsp + 40], r8",
        "stmxcsr [rsp + 48]", "fnstcw [rsp + 52]",
        "movaps [rsp + 64], xmm6",
        "movaps [rsp + 80], xmm7",
        "movaps [rsp + 96], xmm8",
        "movaps [rsp + 112], xmm9",
        "movaps [rsp + 128], xmm10",
        "movaps [rsp + 144], xmm11",
        "movaps [rsp + 160], xmm12",
        "movaps [rsp + 176], xmm13",
        "movaps [rsp + 192], xmm14",
        "movaps [rsp + 208], xmm15",
        "mov rbp, 101",
        "mov rbx, 102",
        "mov rdi, 103",
        "mov rsi, 104",
        "mov r12, 105",
        "mov r13, 106",
        "mov r14, 107",
        "mov r15, 108",
        "mov eax, 606", "movd xmm6, eax", "pshufd xmm6, xmm6, 0",
        "mov eax, 607", "movd xmm7, eax", "pshufd xmm7, xmm7, 0",
        "mov eax, 608", "movd xmm8, eax", "pshufd xmm8, xmm8, 0",
        "mov eax, 609", "movd xmm9, eax", "pshufd xmm9, xmm9, 0",
        "mov eax, 610", "movd xmm10, eax", "pshufd xmm10, xmm10, 0",
        "mov eax, 611", "movd xmm11, eax", "pshufd xmm11, xmm11, 0",
        "mov eax, 612", "movd xmm12, eax", "pshufd xmm12, xmm12, 0",
        "mov eax, 613", "movd xmm13, eax", "pshufd xmm13, xmm13, 0",
        "mov eax, 614", "movd xmm14, eax", "pshufd xmm14, xmm14, 0",
        "mov eax, 615", "movd xmm15, eax", "pshufd xmm15, xmm15, 0",
        "lea rcx, [rip + {entry}]", "lea rdx, [rsp + 40]", "xor r8d, r8d", "call {enter}",
        "cmp rbp, 101", "jne 2f",
        "cmp rbx, 102", "jne 2f",
        "cmp rdi, 103", "jne 2f",
        "cmp rsi, 104", "jne 2f",
        "cmp r12, 105", "jne 2f",
        "cmp r13, 106", "jne 2f",
        "cmp r14, 107", "jne 2f",
        "cmp r15, 108", "jne 2f",
        "mov eax, 606", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm6", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 607", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm7", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 608", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm8", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 609", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm9", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 610", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm10", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 611", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm11", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 612", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm12", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 613", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm13", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 614", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm14", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "mov eax, 615", "movd xmm0, eax", "pshufd xmm0, xmm0, 0", "pcmpeqb xmm0, xmm15", "pmovmskb eax, xmm0", "cmp eax, 65535", "jne 2f",
        "stmxcsr [rsp + 224]", "mov eax, [rsp + 224]", "cmp eax, [rsp + 48]", "jne 2f",
        "fnstcw [rsp + 224]", "mov ax, [rsp + 224]", "cmp ax, [rsp + 52]", "jne 2f",
        "xor eax, eax", "jmp 3f",
        "2:", "mov eax, 1",
        "3:", "ldmxcsr [rsp + 48]", "fldcw [rsp + 52]",
        "movaps xmm6, [rsp + 64]",
        "movaps xmm7, [rsp + 80]",
        "movaps xmm8, [rsp + 96]",
        "movaps xmm9, [rsp + 112]",
        "movaps xmm10, [rsp + 128]",
        "movaps xmm11, [rsp + 144]",
        "movaps xmm12, [rsp + 160]",
        "movaps xmm13, [rsp + 176]",
        "movaps xmm14, [rsp + 192]",
        "movaps xmm15, [rsp + 208]",
        "add rsp, 248",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rsi",
        "pop rdi",
        "pop rbx",
        "pop rbp",
        "ret",
        entry = sym clobber_entry,
        enter = sym enter,
    )
}

#[test]
#[ignore = "requires verified compatible host hardening; run explicitly with --ignored"]
fn win64_preserves_integer_simd_and_fp_control_for_every_exit() {
    // On Linux this exercises the same instructions via the win64 calling
    // convention, after checking that host shadow stacks are disabled. Under
    // Windows it uses the platform gate, never overriding mitigations for tests.
    assert!(
        crate::native_transfer::is_supported(),
        "host mitigation state does not permit executing the segment probe"
    );
    for exit in [
        NativeExit::Returned,
        NativeExit::Transfer,
        NativeExit::Deopt,
    ] {
        let mut anchor = NativeSegment {
            saved_sp: 0,
            landing_pc: 0,
            previous: std::ptr::null_mut(),
            owner: SegmentOwner::Thread(crate::thread::NativeThreadId(0)),
            stack_sp: std::ptr::null(),
            stack_fp: std::ptr::null(),
            _pinned: std::marker::PhantomPinned,
        };
        let mut out = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        assert_eq!(
            unsafe { register_probe(&mut anchor, &mut out, exit as u64) },
            0
        );
        assert_eq!(out.exit, exit);
        assert_eq!(out.value, crate::TorclVal::from_fixnum(42));
        assert_eq!((anchor.saved_sp, anchor.landing_pc), (0, 0));
    }
    assert_eq!(DROPS.load(Ordering::SeqCst), 3);
}

#[cfg(windows)]
#[test]
fn windows_unwind_metadata_restores_every_saved_register() {
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    let address = enter as *const () as u64;
    unsafe {
        let mut base = 0;
        let entry = RtlLookupFunctionEntry(address, &mut base, std::ptr::null_mut());
        assert!(
            !entry.is_null(),
            "segment adapter needs a Windows function table"
        );
        let unwind = (base + (*entry).Anonymous.UnwindData as u64) as *const u8;
        let body_offset = *unwind.add(1) as usize;
        let bytes = std::slice::from_raw_parts(
            address as *const u8,
            ((*entry).EndAddress - (*entry).BeginAddress) as usize,
        );
        let landing_offset = bytes
            .windows(4)
            .enumerate()
            .skip(1)
            .find(|(_, bytes)| *bytes == [0xf3, 0x0f, 0x1e, 0xfa])
            .expect("indirect landing must start with ENDBR64")
            .0;
        // Synthetic adapter save area: no native transfer is executed here, so
        // the OS unwind-table test also runs on hosts that refuse ABI activation.
        let mut aligned_stack = [0u128; 20];
        let stack = std::slice::from_raw_parts_mut(aligned_stack.as_mut_ptr().cast::<u64>(), 40);
        for i in 0..8 {
            stack[36 - i] = 101 + i as u64;
        }
        for i in 0..10 {
            stack[8 + i * 2] = 600 + i as u64;
            stack[9 + i * 2] = 700 + i as u64;
        }
        stack[37] = 0x1234_5678;
        let bottom = stack.as_ptr() as u64;
        for offset in [body_offset, landing_offset] {
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = address + offset as u64;
            context.Rsp = bottom;
            let mut handler_data = std::ptr::null_mut();
            let mut establisher = 0;
            RtlVirtualUnwind(
                0,
                base,
                context.Rip,
                entry,
                &mut context,
                &mut handler_data,
                &mut establisher,
                std::ptr::null_mut(),
            );
            assert_eq!(context.Rip, stack[37]);
            assert_eq!(context.Rsp, bottom + 304);
            assert_eq!(
                [
                    context.Rbp,
                    context.Rbx,
                    context.Rdi,
                    context.Rsi,
                    context.R12,
                    context.R13,
                    context.R14,
                    context.R15
                ],
                [101, 102, 103, 104, 105, 106, 107, 108]
            );
            let xmm = &context.Anonymous.Anonymous.Xmm6 as *const M128A;
            for (i, value) in std::slice::from_raw_parts(xmm, 10).iter().enumerate() {
                assert_eq!(value.Low, 600 + i as u64);
                assert_eq!(value.High, 700 + i as i64);
            }
        }
    }
}
