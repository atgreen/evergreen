// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! ELF context backends assembled by build.rs (POWER has no stable inline asm).
use super::Context;

unsafe extern "C" {
    fn egcl_context_swap(from: *mut Context, to: Context);
    fn egcl_context_init(sp: Context);
}

/// Save the current stack and resume another live context.
///
/// # Safety
/// Both contexts and their stacks must remain live, as required by `context::swap`.
#[inline]
pub unsafe fn swap(from: *mut Context, to: Context) {
    unsafe { egcl_context_swap(from, to) }
}

/// Construct a context whose entry must never return.
pub fn make(stack: &mut [u8], entry: extern "C" fn()) -> Context {
    // Keep a real caller linkage/save area above the initial SP: the entry
    // function may spill into it before allocating its own frame.
    #[cfg(target_arch = "powerpc64")]
    let (frame, linkage, pc) = (560, 32, 544);
    #[cfg(target_arch = "s390x")]
    let (frame, linkage, pc) = (144, 160, 64);
    let sp = super::initial_frame(stack, frame, linkage);
    unsafe {
        sp.add(pc).cast::<usize>().write(entry as usize);
        egcl_context_init(sp);
    }
    sp
}
