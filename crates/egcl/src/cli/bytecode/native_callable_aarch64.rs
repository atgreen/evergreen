// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64 permanent callable entries over the physical native segment boundary.
//! The Rust invocation owns scanned callable/argument slots. Dispatch returns
//! before assembly transfers, so a selected exit never skips Rust root guards.
//!
//! Slice: x0=callable-slot, x1=count, x2=rooted-args, x3=context.
//! Registers: x0=callable-slot, x1=count, x2=a0, x3=a1, x4=a2, x5=context.
//! x18 is never touched; x19-x28 and SIMD registers are unchanged by these shims.
//! Static indirect entries carry BTI call pads. This uses the existing platform
//! capability gate and ordinary AAPCS64 pointers, without claiming arm64e or
//! separately enabled PAC/BTI platform coverage.

use super::*;
use egcl_rt::call_table::NativeCallContext;
use egcl_rt::function::NativeCallableEntries;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
#[path = "native_callable_aarch64/tests.rs"]
mod tests;

// Platform activation is explicit until the AArch64 release gates pass.
// Once activated, unsupported capability is an error, never a checked fallback.
pub(in crate::cli) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("EGCL_NATIVE_TRANSFER").as_deref() == Ok("1"))
}

pub(super) fn entries() -> Option<&'static NativeCallableEntries> {
    if !enabled() {
        return None;
    }
    static ENTRIES: std::sync::OnceLock<NativeCallableEntries> = std::sync::OnceLock::new();
    let entries = ENTRIES.get_or_init(|| NativeCallableEntries {
        registers: registers as *const () as usize,
        slice: slice as *const () as usize,
    });
    install_bytecode_root_scanner();
    egcl_rt::function::install_native_entries(entries);
    Some(entries)
}

#[repr(C)]
struct Invocation {
    context: NativeCallContext,
    function: *mut EgclVal,
    entry: usize,
}

const _: () = {
    assert!(std::mem::offset_of!(Invocation, context) == 0);
    assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
    assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
};

struct EntryGuard {
    env: *mut Env,
    error: Option<EgclError>,
}
impl egcl_rt::gc::TraceHostRoots for EntryGuard {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.error.trace_host_roots(visit);
    }
}
impl Drop for EntryGuard {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.env));
        NATIVE_ERROR.with(|slot| slot.replace(self.error.take()));
    }
}

pub(in crate::cli) fn invoke(
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    invoke_entry(function, args, env, false)
}

fn invoke_entry(
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
    register_arguments: bool,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(arguments = args.to_vec());
    egcl_rt::rooted_ref!(_env = &mut *env);
    let entries = entries().ok_or_else(|| {
        EgclError::Internal("AAPCS64 native callable boundary is not activated".into())
    })?;
    let nargs = arguments.len();
    if register_arguments {
        assert!(nargs <= 3);
        arguments.resize(3, NIL);
    }
    let mut invocation = Invocation {
        context: NativeCallContext {
            request: std::ptr::null_mut(),
            capture: native_transfer::leave_native_segment as *const u8,
            args: arguments.as_mut_ptr(),
            nargs,
        },
        function: &mut *function,
        entry: if register_arguments {
            entries.registers
        } else {
            entries.slice
        },
    };
    let mut entry = EntryGuard {
        env: NATIVE_ENV.with(|slot| slot.replace(env)),
        error: NATIVE_ERROR.with(|slot| slot.take()),
    };
    egcl_rt::rooted_ref!(_entry = &mut entry);
    let code = if register_arguments {
        enter_registers as *const u8
    } else {
        enter_slice as *const u8
    };
    let outcome = unsafe {
        native_transfer::invoke_native_segment(
            code,
            (&mut invocation as *mut Invocation).cast(),
            egcl_rt::current_stack(),
        )
    }
    .map_err(|_| {
        EgclError::Internal(
            "AAPCS64 native callable boundary is unavailable: platform capability is unsupported"
                .into(),
        )
    })?;
    egcl_rt::rooted!(primary = outcome.value);
    egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
    match outcome.exit {
        NativeExit::Returned if error.is_none() => Ok(*primary),
        NativeExit::Transfer => Err(error.take().unwrap_or_else(|| {
            EgclError::Internal("AAPCS64 callable transfer lost its error".into())
        })),
        _ => Err(EgclError::Internal(
            "invalid AAPCS64 callable outcome".into(),
        )),
    }
}

#[cfg(test)]
static ENTRY_COUNTS: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };
#[cfg(test)]
static COLLECT_NEXT: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

unsafe extern "C" fn dispatch(
    function: *const EgclVal,
    nargs: usize,
    args: *const EgclVal,
    _context: *mut NativeCallContext,
    out: *mut NativeOutcome,
) {
    let result = guard_c2i(|| {
        // No Lisp allocation occurs until these copies are rooted. The host
        // invocation also roots the original slots, so register spills and
        // moving collections update both representations before they are read.
        egcl_rt::rooted!(function = unsafe { *function });
        egcl_rt::rooted!(
            arguments = if nargs == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(args, nargs) }.to_vec()
            }
        );
        #[cfg(test)]
        {
            let segment = native_transfer::current_segment();
            assert!(!segment.is_null());
            ENTRY_COUNTS.with(|counts| {
                let (all, nested) = counts.get();
                counts.set((
                    all + 1,
                    nested + usize::from(unsafe { !(*segment).previous().is_null() }),
                ));
            });
            if COLLECT_NEXT.with(|flag| flag.replace(false)) {
                use egcl_rt::Collector;
                egcl_rt::HeapCollector::new().minor_gc()?;
            }
        }
        egcl_rt::safepoint::poll_safepoint();
        if let Some(error) = pending_signal_error_for_current_execution() {
            return Err(error);
        }
        let env = NATIVE_ENV.with(|slot| slot.get());
        if env.is_null() {
            return Err(EgclError::Internal(
                "AAPCS64 callable has no evaluator environment".into(),
            ));
        }
        super::super::apply_function_impl(*function, &arguments, unsafe { &mut *env })
    });
    let outcome = match result {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe { out.write(outcome) };
}

// Keep SP 16-byte aligned and preserve FP/LR across every Rust call. The only
// machine-local values are host pointers and the returned outcome; Lisp words
// are owned by the Rust roots for every allocating/yielding extent.
#[unsafe(naked)]
unsafe extern "C" fn enter_slice(
    _invocation: *mut Invocation,
    _stack: *const egcl_rt::stack::EgclStack,
    _anchor: *mut NativeSegment,
) {
    core::arch::naked_asm!(
        ".cfi_startproc", "bti c",
        "stp x29, x30, [sp, #-16]!", ".cfi_def_cfa_offset 16",
        ".cfi_offset 29, -16", ".cfi_offset 30, -8", "mov x29, sp",
        "mov x9, x0", "str x2, [x9, #{request}]", "mov x3, x9",
        "ldr x1, [x9, #{nargs}]", "ldr x2, [x9, #{args}]",
        "ldr x0, [x9, #{function}]", "ldr x16, [x9, #{entry}]", "blr x16",
        "ldp x29, x30, [sp], #16", ".cfi_def_cfa_offset 0",
        ".cfi_restore 29", ".cfi_restore 30", "ret", ".cfi_endproc",
        request = const std::mem::offset_of!(NativeCallContext, request),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
        args = const std::mem::offset_of!(NativeCallContext, args),
        function = const std::mem::offset_of!(Invocation, function),
        entry = const std::mem::offset_of!(Invocation, entry),
    );
}

#[unsafe(naked)]
unsafe extern "C" fn enter_registers(
    _invocation: *mut Invocation,
    _stack: *const egcl_rt::stack::EgclStack,
    _anchor: *mut NativeSegment,
) {
    core::arch::naked_asm!(
        ".cfi_startproc", "bti c",
        "stp x29, x30, [sp, #-16]!", ".cfi_def_cfa_offset 16",
        ".cfi_offset 29, -16", ".cfi_offset 30, -8", "mov x29, sp",
        "mov x5, x0", "str x2, [x5, #{request}]",
        "ldr x16, [x5, #{entry}]", "ldr x9, [x5, #{args}]",
        "ldr x0, [x5, #{function}]", "ldr x1, [x5, #{nargs}]",
        "ldp x2, x3, [x9]", "ldr x4, [x9, #16]", "blr x16",
        "ldp x29, x30, [sp], #16", ".cfi_def_cfa_offset 0",
        ".cfi_restore 29", ".cfi_restore 30", "ret", ".cfi_endproc",
        request = const std::mem::offset_of!(NativeCallContext, request),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
        args = const std::mem::offset_of!(NativeCallContext, args),
        function = const std::mem::offset_of!(Invocation, function),
        entry = const std::mem::offset_of!(Invocation, entry),
    );
}

#[unsafe(naked)]
unsafe extern "C" fn registers() {
    core::arch::naked_asm!(
        "bti c", "ldr x9, [x5, #{args}]", "cbz x1, 2f",
        "str x2, [x9]", "cmp x1, #1", "b.eq 2f",
        "str x3, [x9, #8]", "cmp x1, #2", "b.eq 2f",
        "str x4, [x9, #16]",
        "2:", "mov x2, x9", "mov x3, x5", "b {slice}",
        args = const std::mem::offset_of!(NativeCallContext, args),
        slice = sym slice,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn slice() {
    core::arch::naked_asm!(
        ".cfi_startproc", "bti c",
        "stp x29, x30, [sp, #-48]!", ".cfi_def_cfa_offset 48",
        ".cfi_offset 29, -48", ".cfi_offset 30, -40", "mov x29, sp",
        "str x3, [sp, #16]", "str x1, [x3, #{nargs}]", "str x2, [x3, #{args}]",
        "add x4, sp, #32", "bl {dispatch}",
        "ldp x0, x2, [sp, #32]", "cbnz x2, 2f",
        ".cfi_remember_state",
        "ldp x29, x30, [sp], #48", ".cfi_def_cfa_offset 0",
        ".cfi_restore 29", ".cfi_restore 30", "ret",
        "2:", ".cfi_restore_state",
        // dispatch and its Rust roots have returned. Only these assembly
        // frames remain above the physical segment landing.
        "mov x1, x0", "ldr x9, [sp, #16]", "ldr x0, [x9, #{request}]",
        "ldr x16, [x9, #{capture}]",
        "ldp x29, x30, [sp], #48", ".cfi_def_cfa_offset 0",
        ".cfi_restore 29", ".cfi_restore 30", "br x16", ".cfi_endproc",
        dispatch = sym dispatch,
        request = const std::mem::offset_of!(NativeCallContext, request),
        capture = const std::mem::offset_of!(NativeCallContext, capture),
        args = const std::mem::offset_of!(NativeCallContext, args),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
    );
}
