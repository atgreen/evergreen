// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

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
//! registers onto the *current* stack, records the resulting SP into `*from`,
//! then loads SP from `to`, restores that context's callee-saved registers, and
//! `ret`s — resuming wherever `to` last swapped away (or, for a fresh context
//! from [`make`], entering its entry function).

/// A saved execution context: the stack pointer at the swap point. All other
/// state (callee-saved registers, return address) lives on that stack.
pub type Context = *mut u8;

/// An empty/placeholder context (filled in by the first [`swap`] out of it).
pub const NULL: Context = core::ptr::null_mut();

#[cfg(all(egcl_unix_fibers, target_arch = "aarch64"))]
#[path = "context/aarch64.rs"]
mod backend;
#[cfg(all(egcl_unix_fibers, target_arch = "riscv64"))]
#[path = "context/riscv64.rs"]
mod backend;
#[cfg(all(
    egcl_unix_fibers,
    any(target_arch = "powerpc64", target_arch = "s390x")
))]
#[path = "context/elf.rs"]
mod backend;
#[cfg(all(egcl_unix_fibers, not(target_arch = "x86_64")))]
pub use backend::{make, swap};

/// Reserve a zeroed, aligned initial frame and the ABI's caller linkage area.
#[cfg(all(egcl_unix_fibers, not(target_arch = "x86_64")))]
fn initial_frame(stack: &mut [u8], frame: usize, linkage: usize) -> *mut u8 {
    let base = stack.as_mut_ptr() as usize;
    let top = (base + stack.len()) & !15usize;
    assert!(
        top.saturating_sub(base) >= frame + linkage,
        "fiber stack too small"
    );
    let sp = (top - frame - linkage) as *mut u8;
    unsafe { core::ptr::write_bytes(sp, 0, frame + linkage) };
    sp
}

/// Save the current context into `*from` and resume `to`. Returns when some
/// later `swap` switches back into `*from`.
///
/// # Safety
///
/// `from` must point to a writable `Context`; `to` must be a context
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
    debug_assert!(
        aligned_top.saturating_sub(64) >= base,
        "fiber stack too small"
    );
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

// ── Targets without this stack-pointer backend ──────────────────────────────
// Windows x86-64 uses owned OS fibers in thread/windows.rs instead of this
// stack-pointer API. Other targets retain the inline fallback in thread.rs.
// These stubs exist only so this module compiles on those targets.
/// Compatibility stub that performs no context switch.
///
/// # Safety
/// Callers must use the platform's supported fiber backend and must not rely
/// on this stub to save or resume an execution context.
#[cfg(not(egcl_unix_fibers))]
pub unsafe fn swap(_from: *mut Context, _to: Context) {}

#[cfg(not(egcl_unix_fibers))]
pub fn make(_stack: &mut [u8], _entry: extern "C" fn()) -> Context {
    NULL
}
