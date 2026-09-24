//! Short memcpy calls in the static musl CLI avoid REP MOVSQ setup latency.
//!
//! The linker wraps only this binary's memcpy references. Copies above 64 bytes
//! tail-call the original libc implementation; other platforms are untouched.
//! All loads/stores stay inside the requested ranges, with the head and tail
//! loaded before either is stored. SSE2 is baseline on x86-64: no AVX/ERMS/CPU
//! detection, heap allocation, locks, TLS, or runtime initialization is needed.
//! This leaf obeys the SysV C ABI and returns the original destination.

unsafe extern "C" {
    #[link_name = "__real_memcpy"]
    fn large_copy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8;
}

core::arch::global_asm!(
    r#"
    .pushsection .text.bliss_short_copy,"ax",@progbits
    .p2align 4
    .globl __wrap_memcpy
    .hidden __wrap_memcpy
    .type __wrap_memcpy,@function
__wrap_memcpy:
    .cfi_startproc
    cmp rdx, 64
    ja .Lcopy_libc
    mov rax, rdi
    cmp rdx, 16
    jb .Lcopy_scalar
    cmp rdx, 32
    ja .Lcopy_four_vectors

    movdqu xmm0, [rsi]
    movdqu xmm1, [rsi + rdx - 16]
    movdqu [rdi], xmm0
    movdqu [rdi + rdx - 16], xmm1
    ret

.Lcopy_four_vectors:
    movdqu xmm0, [rsi]
    movdqu xmm1, [rsi + 16]
    movdqu xmm2, [rsi + rdx - 32]
    movdqu xmm3, [rsi + rdx - 16]
    movdqu [rdi], xmm0
    movdqu [rdi + 16], xmm1
    movdqu [rdi + rdx - 32], xmm2
    movdqu [rdi + rdx - 16], xmm3
    ret

.Lcopy_scalar:
    cmp rdx, 8
    jb .Lcopy_below_eight
    mov rcx, [rsi]
    mov r8, [rsi + rdx - 8]
    mov [rdi], rcx
    mov [rdi + rdx - 8], r8
    ret

.Lcopy_below_eight:
    cmp rdx, 4
    jb .Lcopy_below_four
    mov ecx, [rsi]
    mov r8d, [rsi + rdx - 4]
    mov [rdi], ecx
    mov [rdi + rdx - 4], r8d
    ret

.Lcopy_below_four:
    cmp rdx, 2
    jb .Lcopy_below_two
    mov cx, [rsi]
    mov r8w, [rsi + rdx - 2]
    mov [rdi], cx
    mov [rdi + rdx - 2], r8w
    ret

.Lcopy_below_two:
    test rdx, rdx
    jz .Lcopy_return
    mov cl, [rsi]
    mov [rdi], cl
.Lcopy_return:
    ret

.Lcopy_libc:
    jmp {large_copy}
    .cfi_endproc
    .size __wrap_memcpy, .-__wrap_memcpy
    .popsection
    "#,
    large_copy = sym large_copy,
);
