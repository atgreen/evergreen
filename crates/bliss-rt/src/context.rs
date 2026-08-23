//! Portable fiber context switch (no libc `ucontext`), bliss-bca.5.
//!
//! The fiber scheduler needs to switch a CPU between the carrier (scheduler)
//! stack and a fiber's own stack. glibc's `getcontext`/`makecontext`/
//! `swapcontext` do this, but they are glibc-only (unavailable / different on
//! musl), which blocks fibers in a fully static build. This module implements
//! the same save/switch semantics directly in a few instructions of assembly.
//!
// The swap/make functions are inherently unsafe (raw stack manipulation); the
// `unsafe fn` marker is the safety boundary. Allow the edition-2024 lint here.
#![allow(unsafe_op_in_unsafe_fn)]
//!
//! A [`Context`] is just a saved stack pointer; [`swap`] pushes the callee-saved
//! registers onto the *current* stack, records the resulting `rsp` into `*from`,
//! then loads `rsp` from `to`, pops that context's callee-saved registers, and
//! `ret`s — resuming wherever `to` last swapped away (or, for a fresh context
//! from [`make`], entering its entry function).

/// A saved execution context: the stack pointer at the swap point. All other
/// state (callee-saved registers, return address) lives on that stack.
pub type Context = *mut u8;

/// An empty/placeholder context (filled in by the first [`swap`] out of it).
pub const NULL: Context = core::ptr::null_mut();

/// Save the current context into `*from` and resume `to`. Returns when some
/// later `swap` switches back into `*from`.
///
/// SAFETY: `from` must point to a writable `Context`; `to` must be a context
/// produced by [`make`] or a previous `swap`, over a stack that is still alive.
#[cfg(all(target_arch = "x86_64", unix))]
#[inline]
pub unsafe fn swap(from: *mut Context, to: Context) {
    context_swap(from, to);
}

#[cfg(all(target_arch = "x86_64", unix))]
#[unsafe(naked)]
unsafe extern "C" fn context_swap(from: *mut Context, to: Context) {
    // SysV: rdi = from, rsi = to. Save callee-saved regs on the current stack,
    // stash rsp into *from, switch to `to`, restore its regs and return into it.
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    )
}

/// Initialise a fresh context on `stack` that, when first swapped to, begins
/// executing `entry`. `entry` MUST NOT return (there is no valid return address
/// above it). Returns the initial saved stack pointer.
#[cfg(all(target_arch = "x86_64", unix))]
pub fn make(stack: &mut [u8], entry: extern "C" fn()) -> Context {
    let base = stack.as_mut_ptr() as usize;
    let end = base + stack.len();
    // 16-align the top, then drop a 64-byte initial frame. `sp` stays 16-aligned
    // so that after the 6 register pops (48 bytes) and `ret` (8 bytes) the entry
    // executes with rsp ≡ 8 (mod 16) — the SysV state at a function's first
    // instruction.
    let aligned_top = end & !15usize;
    debug_assert!(aligned_top.saturating_sub(64) >= base, "fiber stack too small");
    let sp = (aligned_top - 64) as *mut usize;
    // SAFETY: `sp .. sp+56` lies within the stack slice.
    unsafe {
        // 6 callee-saved slots (r15,r14,r13,r12,rbx,rbp) — start zeroed.
        for i in 0..6 {
            sp.add(i).write(0);
        }
        // Return-address slot consumed by `ret`: jump to the entry trampoline.
        sp.add(6).write(entry as usize);
    }
    sp as *mut u8
}

// ── Non-x86_64 / non-unix stub ───────────────────────────────────────────────
// Other targets keep the previous behaviour (fibers fall back to running inline
// on the caller; see thread.rs). These stubs exist only so the module compiles.
#[cfg(not(all(target_arch = "x86_64", unix)))]
pub unsafe fn swap(_from: *mut Context, _to: Context) {}

#[cfg(not(all(target_arch = "x86_64", unix)))]
pub fn make(_stack: &mut [u8], _entry: extern "C" fn()) -> Context {
    NULL
}
