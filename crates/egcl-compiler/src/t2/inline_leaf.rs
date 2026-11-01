// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Leaf builtin calls in ordinary-ABI T2 code (bliss-gg3i1.7.3).
//!
//! An ordinary call to a builtin otherwise enters the runtime through the
//! named adapter: root publication, the execution context, an unwind guard,
//! the safepoint poll and builtin dispatch, ~300 ns a call. For a builtin with
//! a registered leaf helper the emitter places a fast path in front of that
//! call, as `inline_special` does for global variables:
//!
//! 1. when the builtin's function cell can be replaced, compare it with the
//!    absent binding it had when the code was compiled;
//! 2. pass the arguments to the helper in the System V registers;
//! 3. a result other than UNBOUND is the call's single value: clear CF and
//!    jump to the returned label, which the caller binds where the call's own
//!    result arrives. UNBOUND falls through into the unchanged call.
//!
//! A helper never allocates, collects, polls, signals or unwinds, and has no
//! effect when it answers UNBOUND, so the fast path needs no safepoint map and
//! a miss simply makes the original call (no deoptimization: a polymorphic site
//! keeps its compiled code). On a hit the helper leaves the multiple-value
//! buffer empty and inactive, as a single-value ordinary return must.
//!
//! The call's arguments are still in their homes. Homes in caller-saved
//! registers are saved around the helper call, so the fall-through call finds
//! them intact. Only RAX and RDX are written outside that save.

use super::{Asm, Cc, FramedHome, RAX, SCRATCH, load_home, mov_imm64, pop_reg, push_reg};
use crate::t2::ir::{AuxData, InstData, Value};
use std::collections::HashMap;

/// A builtin answered by a leaf helper: `helper(a0, a1, a2) -> value | UNBOUND`.
/// Unused argument registers are unspecified.
#[derive(Clone, Copy, Debug)]
pub struct LeafBuiltin {
    /// The builtin's symbol index.
    pub symbol: u32,
    /// The exact argument count the helper accepts.
    pub nargs: u8,
    /// The helper's address.
    pub helper: u64,
    /// The same helper for a caller whose multiple-value buffer is already
    /// empty and inactive: it leaves the buffer alone.
    pub helper_mv_empty: u64,
    /// The builtin can return more than one value; the helper answers only
    /// the primary. Such a fast path is placed only at calls whose multiple
    /// values are discarded.
    pub primary_only: bool,
}

/// Helpers the runtime installs at startup.
#[derive(Clone, Debug, Default)]
pub struct LeafHelpers {
    pub builtins: Vec<LeafBuiltin>,
    /// `load_global(symbol_index) -> value | UNBOUND`: the current dynamic
    /// binding or global value of a symbol, UNBOUND when it has none.
    pub load_global: u64,
    /// `bind_special(symbol_index, value, symbol_object) -> NIL`: establish a
    /// special binding on the execution's native binding stack and empty the
    /// multiple-value buffer (bliss-gg3i1.7.4). `symbol_object` is the pinned
    /// object of an interned symbol, or 0. Never fails.
    pub bind_special: u64,
    /// `bind_special` leaving the multiple-value buffer alone.
    pub bind_special_keep_values: u64,
    /// `unbind_special(count) -> NIL`: restore the `count` newest native
    /// bindings and empty the multiple-value buffer. Never fails.
    pub unbind_special: u64,
    /// `unbind_special` leaving the multiple-value buffer alone.
    pub unbind_special_keep_values: u64,
}

/// The native special-binding leaf helpers.
#[derive(Clone, Copy)]
pub(super) struct SpecialBindingHelpers {
    pub bind: u64,
    pub bind_keep_values: u64,
    pub unbind: u64,
    pub unbind_keep_values: u64,
}

/// The native special-binding helpers, if all are installed.
pub(super) fn special_binding_helpers() -> Option<SpecialBindingHelpers> {
    let leaf = LEAF.get()?;
    let helpers = SpecialBindingHelpers {
        bind: leaf.bind_special,
        bind_keep_values: leaf.bind_special_keep_values,
        unbind: leaf.unbind_special,
        unbind_keep_values: leaf.unbind_special_keep_values,
    };
    [
        helpers.bind,
        helpers.bind_keep_values,
        helpers.unbind,
        helpers.unbind_keep_values,
    ]
    .iter()
    .all(|&helper| helper != 0)
    .then_some(helpers)
}

static LEAF: std::sync::OnceLock<LeafHelpers> = std::sync::OnceLock::new();

/// Install the leaf helpers. Idempotent; later calls are ignored. Without
/// them every such call keeps the ordinary runtime call.
pub fn install_leaf_helpers(helpers: LeafHelpers) {
    let _ = LEAF.set(helpers);
}

fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var_os("EGCL_NO_LEAF_BUILTINS").is_some())
}

/// The global-read helper, if installed and enabled.
pub(super) fn load_global_helper() -> Option<u64> {
    let helper = LEAF.get()?.load_global;
    (helper != 0 && !disabled()).then_some(helper)
}

fn builtin(symbol: u32, nargs: usize) -> Option<LeafBuiltin> {
    if disabled() {
        return None;
    }
    LEAF.get()?
        .builtins
        .iter()
        .find(|entry| {
            entry.symbol == symbol
                && usize::from(entry.nargs) == nargs
                && nargs <= 3
                && entry.helper != 0
                && entry.helper_mv_empty != 0
        })
        .copied()
}

/// Where a helper argument comes from.
#[derive(Clone, Copy)]
pub(super) enum LeafArg {
    Home(FramedHome),
    Immediate(u64),
}

/// Call `helper` with `args` in RDI, RSI, RDX and branch to `slow` when it
/// answers UNBOUND; otherwise RAX holds its result. Caller-saved register homes
/// among the arguments are preserved. `None`, before anything is emitted, when
/// an argument home is a register this sequence cannot preserve.
pub(super) fn emit_call(
    a: &mut Asm,
    helper: u64,
    args: &[LeafArg],
    slow: egcl_rt::asm::Label,
) -> Option<()> {
    const DESTINATIONS: [u8; 3] = [7, 6, 2]; // rdi, rsi, rdx
    if args.len() > DESTINATIONS.len() || helper == 0 {
        return None;
    }
    let mut saved: Vec<u8> = Vec::new();
    for arg in args {
        if let LeafArg::Home(FramedHome::Reg(reg)) = *arg {
            // RAX, RDX and RDI are scratch here and never value homes.
            if matches!(reg, RAX | SCRATCH | 7) || reg == 4 || reg == 5 {
                return None;
            }
            if !super::CALLEE_SAVED_X86.contains(&reg) && !saved.contains(&reg) {
                saved.push(reg);
            }
        }
    }
    // Keep RSP 16-byte aligned at the helper call.
    let pushes = saved.len() + saved.len() % 2;
    for &reg in &saved {
        push_reg(a, reg);
    }
    if saved.len() % 2 == 1 {
        push_reg(a, saved[0]);
    }
    let adjust = i32::try_from(8 * pushes).ok()?;
    // RSI is the only destination that can also be a source home, so it is
    // written last: RDI, then RDX, then RSI.
    for index in [0usize, 2, 1] {
        let Some(&arg) = args.get(index) else {
            continue;
        };
        let dst = DESTINATIONS[index];
        match arg {
            LeafArg::Immediate(bits) => mov_imm64(a, dst, bits as i64),
            LeafArg::Home(home) => load_home(a, dst, home, adjust),
        }
    }
    mov_imm64(a, RAX, helper as i64);
    a.extend_from_slice(&[0xFF, 0xD0]); // call rax
    if saved.len() % 2 == 1 {
        pop_reg(a, saved[0]);
    }
    for &reg in saved.iter().rev() {
        pop_reg(a, reg);
    }
    mov_imm64(a, SCRATCH, egcl_rt::value::UNBOUND.0 as i64);
    super::cmp_rr(a, RAX, SCRATCH);
    a.jcc(Cc::E, slow);
    Some(())
}

/// The fast path for an ordinary call to a leaf builtin. On a hit RAX holds the
/// single result with CF clear, and control jumps to the returned label, which
/// the caller binds where the call's own result arrives. Otherwise control
/// falls through to the call. `None` when nothing was emitted.
///
/// `mv_discarded` says the call's multiple values are never read, which a
/// primary-only helper requires; `mv_empty` that the buffer is already empty
/// and inactive on entry, so the helper need not empty it.
pub(super) fn emit_ordinary_fast_path(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_discarded: bool,
    mv_empty: bool,
) -> Option<egcl_rt::asm::Label> {
    // NOTINLINE calls carry OpaqueCallTarget and keep named dispatch.
    let AuxData::CallTarget(symbol) = data.aux else {
        return None;
    };
    if data.results.len() > 1 {
        return None;
    }
    let entry = builtin(symbol, data.args.len())?;
    if entry.primary_only && !mv_discarded {
        return None;
    }
    // A redefinition replaces the absent function cell the builtin has now.
    let guard = match crate::t2::builtin_binding::snapshot(symbol)? {
        crate::t2::builtin_binding::Binding::FixedStandard => None,
        crate::t2::builtin_binding::Binding::Guarded { address, expected } => {
            Some((address, expected))
        }
    };
    let mut args = Vec::with_capacity(data.args.len());
    for value in &data.args {
        args.push(match constants.get(value) {
            Some(&bits) => LeafArg::Immediate(bits),
            None => LeafArg::Home(*homes.get(value)?),
        });
    }
    if args.iter().any(|arg| {
        matches!(arg, LeafArg::Home(FramedHome::Reg(reg)) if matches!(*reg, RAX | SCRATCH | 7 | 4 | 5))
    }) {
        return None;
    }
    let slow = a.label();
    let done = a.label();
    if let Some((address, expected)) = guard {
        mov_imm64(a, RAX, address as i64);
        mov_imm64(a, SCRATCH, expected as i64);
        a.extend_from_slice(&[0x48, 0x39, 0x10]); // cmp [rax], rdx
        a.jcc(Cc::Ne, slow);
    }
    let helper = if mv_empty {
        entry.helper_mv_empty
    } else {
        entry.helper
    };
    emit_call(a, helper, &args, slow)?;
    a.push(0xf8); // clc: one value; the helper emptied the MV buffer
    a.jmp(done);
    a.bind(slow);
    Some(done)
}
