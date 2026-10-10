// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Win64 permanent callable entries. The host owns and scans the selected
//! callable and argument buffer for the complete segment. Rust helpers return
//! before the assembly adapter performs an exceptional transfer. These static
//! functions carry normal PE/COFF unwind information; no dynamic SEH table is
//! needed. The physical Win64 segment adapter preserves integer/SIMD/FP state.
//!
//! Slice: RCX=callable-slot, RDX=count, R8=rooted-args, R9=context.
//! Registers: RCX=callable-slot, RDX=count, R8=a0, R9=a1,
//! stack arguments five/six=a2/context. The context owns writable scanned
//! argument storage, including the three register slots. This is the Win64
//! realization of the common NativeCallableEntries/NativeCallContext contract.

use super::*;
use egcl_rt::call_table::NativeCallContext;
use egcl_rt::function::NativeCallableEntries;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
#[path = "native_callable_windows/tests.rs"]
mod tests;

// Platform activation is explicit until the Windows release gates pass.
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
        EgclError::Internal("Win64 native callable boundary is not activated".into())
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
            "Win64 native callable boundary is unavailable: mitigation policy is unsupported"
                .into(),
        )
    })?;
    egcl_rt::rooted!(primary = outcome.value);
    egcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
    match outcome.exit {
        NativeExit::Returned if error.is_none() => Ok(*primary),
        NativeExit::Transfer => Err(error.take().unwrap_or_else(|| {
            EgclError::Internal("Win64 callable transfer lost its error".into())
        })),
        _ => Err(EgclError::Internal("invalid Win64 callable outcome".into())),
    }
}

#[cfg(test)]
static ENTRY_COUNTS: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };
#[cfg(test)]
static COLLECT_NEXT: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

unsafe extern "win64" fn dispatch(
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
                "Win64 callable has no evaluator environment".into(),
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

// No nonvolatile register is modified by these shims. Each non-leaf frame
// reserves Win64's 32-byte home area and maintains 16-byte call alignment.
// The containing executable supplies SEH entries through these directives.
#[unsafe(naked)]
unsafe extern "win64" fn enter_slice(
    _invocation: *mut Invocation,
    _stack: *const egcl_rt::stack::EgclStack,
    _anchor: *mut NativeSegment,
) {
    core::arch::naked_asm!(
        ".seh_proc {this}", "endbr64", "sub rsp, 40", ".seh_stackalloc 40", ".seh_endprologue",
        "mov r11, rcx", "mov [rcx + {request}], r8",
        "mov r9, rcx", "mov rdx, [r11 + {nargs}]", "mov r8, [r11 + {args}]",
        "mov rcx, [r11 + {function}]", "call [r11 + {entry}]",
        "add rsp, 40", "ret", ".seh_endproc",
        this = sym enter_slice,
        request = const std::mem::offset_of!(NativeCallContext, request),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
        args = const std::mem::offset_of!(NativeCallContext, args),
        function = const std::mem::offset_of!(Invocation, function),
        entry = const std::mem::offset_of!(Invocation, entry),
    );
}

#[unsafe(naked)]
unsafe extern "win64" fn enter_registers(
    _invocation: *mut Invocation,
    _stack: *const egcl_rt::stack::EgclStack,
    _anchor: *mut NativeSegment,
) {
    core::arch::naked_asm!(
        ".seh_proc {this}", "endbr64", "sub rsp, 56", ".seh_stackalloc 56", ".seh_endprologue",
        "mov r11, rcx", "mov [rcx + {request}], r8", "mov [rsp + 40], rcx",
        "mov r10, [r11 + {args}]", "mov rax, [r10 + 16]", "mov [rsp + 32], rax",
        "mov r8, [r10]", "mov r9, [r10 + 8]", "mov rdx, [r11 + {nargs}]",
        "mov rcx, [r11 + {function}]", "call [r11 + {entry}]",
        "add rsp, 56", "ret", ".seh_endproc",
        this = sym enter_registers,
        request = const std::mem::offset_of!(NativeCallContext, request),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
        args = const std::mem::offset_of!(NativeCallContext, args),
        function = const std::mem::offset_of!(Invocation, function),
        entry = const std::mem::offset_of!(Invocation, entry),
    );
}

#[unsafe(naked)]
unsafe extern "win64" fn registers() {
    core::arch::naked_asm!(
        "endbr64", "mov r11, [rsp + 48]", "mov r10, [r11 + {args}]",
        "test rdx, rdx", "jz 2f", "mov [r10], r8", "cmp rdx, 1", "je 2f",
        "mov [r10 + 8], r9", "cmp rdx, 2", "je 2f",
        "mov rax, [rsp + 40]", "mov [r10 + 16], rax",
        "2:", "mov r8, r10", "mov r9, r11", "jmp {slice}",
        args = const std::mem::offset_of!(NativeCallContext, args),
        slice = sym slice,
    );
}

#[unsafe(naked)]
unsafe extern "win64" fn slice() {
    core::arch::naked_asm!(
        ".seh_proc {this}", "endbr64", "sub rsp, 72", ".seh_stackalloc 72", ".seh_endprologue",
        "mov [rsp + 56], r9", "mov [r9 + {nargs}], rdx", "mov [r9 + {args}], r8",
        "lea rax, [rsp + 40]", "mov [rsp + 32], rax", "call {dispatch}",
        "mov rax, [rsp + 40]", "cmp qword ptr [rsp + 48], 0", "jne 2f",
        "add rsp, 72", "ret",
        // dispatch and its Rust root guards have returned. Only assembly frames
        // remain above the segment landing, so a transfer cannot skip Drop.
        "2:", "mov r10, [rsp + 56]", "mov rcx, [r10 + {request}]",
        "mov rdx, rax", "mov r8, [rsp + 48]", "lea r11, [r10 + {capture}]",
        "add rsp, 72", "jmp qword ptr [r11]", ".seh_endproc",
        this = sym slice,
        dispatch = sym dispatch,
        request = const std::mem::offset_of!(NativeCallContext, request),
        capture = const std::mem::offset_of!(NativeCallContext, capture),
        args = const std::mem::offset_of!(NativeCallContext, args),
        nargs = const std::mem::offset_of!(NativeCallContext, nargs),
    );
}
