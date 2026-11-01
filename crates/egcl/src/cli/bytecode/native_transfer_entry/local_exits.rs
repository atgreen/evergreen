// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Commit a local native exit without rewriting its bytecode coordinates.

use super::*;
use egcl_compiler::control_scope::{Ownership, ScopeKind};
use egcl_compiler::t2::emit::TransferCallRequest;

/// Validation is complete before the first removal. Once commit begins, only
/// pre-existing binding guards and catch tokens are dropped: no Lisp callback,
/// allocation, pending transfer, or deoptimization may split the operation.
/// The caller's original pre-exit stack remains precisely published throughout.
pub(super) unsafe fn request(call: &TransferCallRequest) -> Result<EgclVal, EgclError> {
    if call.nargs != 0 {
        return Err(invalid_capture());
    }
    let pointer = CAPTURE.with(Cell::get);
    let env = NATIVE_ENV.with(Cell::get);
    if pointer.is_null() || env.is_null() {
        return Err(invalid_capture());
    }
    let context = unsafe { &*pointer };
    let code = unsafe { &*context.owner };
    let bcp = call.symbol as u32;
    if !matches!(
        code.body.code.get(bcp as usize),
        Some(Instr::ReturnFrom { .. } | Instr::Go { .. })
    ) {
        return Err(invalid_capture());
    }
    let before = code
        .control_scopes
        .before(bcp)
        .ok_or_else(invalid_capture)?;
    let exit = code
        .control_scopes
        .exit_at(bcp)
        .ok_or_else(invalid_capture)?;
    if exit.removed.iter().any(|scope| {
        scope.ownership != Ownership::Local
            || !matches!(
                scope.kind,
                ScopeKind::Block { .. }
                    | ScopeKind::Tagbody { .. }
                    | ScopeKind::SpecialBinding { .. }
                    | ScopeKind::Catch { .. }
                    | ScopeKind::LexicalEnvironment
            )
    }) || !unsafe { validate_special_bindings(context, before) }
    {
        return Err(invalid_capture());
    }
    let catches = unsafe { &*context.catches };
    if !catch_ledger_matches(before, catches, context.catch_base, unsafe { &*env }) {
        return Err(invalid_capture());
    }
    // The scope map is an immutable property of the retained owner. Its
    // removed scopes are the exact reverse suffix of `before`; full-ledger
    // validation above therefore proves every following pop before mutation.
    // Dropping a DynBind restores its saved value without touching Lisp MV.
    for scope in &exit.removed {
        match scope.kind {
            ScopeKind::SpecialBinding { .. } => {
                unsafe { &mut *context.specials }.pop();
            }
            ScopeKind::Catch { .. } => {
                unsafe { &mut *env }.catch_stack.pop();
                unsafe { &mut *context.catches }.pop();
            }
            // A registered BLOCK left by its own or an outer RETURN-FROM
            // (bliss-gg3i1.7.11).
            ScopeKind::Block { register: true, .. } => {
                unsafe { &mut *env }.block_stack.pop();
                unsafe { &mut *context.catches }.pop();
            }
            // Leave a captured binding's child frame, as PopEnvChild does.
            ScopeKind::LexicalEnvironment => {
                pop_env_child(unsafe { &mut *context.env_frame });
            }
            ScopeKind::Block {
                register: false, ..
            }
            | ScopeKind::Tagbody { .. } => {}
            _ => unreachable!("validated local exit scope"),
        }
    }
    #[cfg(test)]
    COMPLETED.with(|count| count.set(count.get() + 1));
    Ok(NIL)
}

#[cfg(test)]
pub(in crate::cli::bytecode) static COMPLETED: egcl_rt::execution_local::ExecutionLocal<
    Cell<usize>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };

/// Exercise failed validation while a real generated caller is published.
/// Only test-owned Rust strings are changed; no Lisp allocation occurs here.
#[cfg(test)]
pub(in crate::cli::bytecode) fn reject_corrupt_catch_for_test() {
    let context = unsafe { &*CAPTURE.with(Cell::get) };
    let code = unsafe { &*context.owner };
    let bcp = code
        .body
        .code
        .iter()
        .enumerate()
        .find_map(|(i, instruction)| {
            matches!(instruction, Instr::ReturnFrom { .. })
                .then(|| {
                    code.control_scopes
                        .exit_at(i as u32)
                        .map(|exit| (i as u32, exit))
                })
                .flatten()
                .filter(|(_, exit)| {
                    exit.removed
                        .iter()
                        .any(|scope| matches!(scope.kind, ScopeKind::Catch { .. }))
                })
                .map(|(bcp, _)| bcp)
        })
        .expect("fixture dynamic exit");
    let env = NATIVE_ENV.with(Cell::get);
    let old = std::mem::replace(
        &mut unsafe { &mut *context.catches }.last_mut().unwrap().token,
        "invalid-test-catch-token".to_owned(),
    );
    let bindings = || {
        unsafe { &*context.specials }
            .iter()
            .map(|saved| {
                (
                    saved.push_bcp,
                    saved.binding.handler_depth,
                    saved.binding.guard.idx,
                    saved.binding.guard.saved,
                    egcl_rt::symbols::symbol_value(saved.binding.guard.idx),
                )
            })
            .collect::<Vec<_>>()
    };
    let catches = || {
        unsafe { &*context.catches }
            .iter()
            .map(|saved| (saved.push_bcp, saved.token.clone()))
            .collect::<Vec<_>>()
    };
    let before_bindings = bindings();
    let before_catches = catches();
    let before_env = unsafe { &*env }.catch_stack.clone();
    let result = unsafe {
        request(&TransferCallRequest {
            symbol: egcl_compiler::t2::emit::TRANSFER_DIRECT_EXIT_REQUEST | u64::from(bcp),
            nargs: 0,
            args: std::ptr::null_mut(),
            activation: context.activation,
        })
    };
    let unchanged = bindings() == before_bindings
        && catches() == before_catches
        && unsafe { &*env }.catch_stack == before_env;
    if let Some(saved) = unsafe { &mut *context.catches }.last_mut() {
        saved.token = old;
    }
    assert!(result.is_err(), "corrupted catch must fail validation");
    assert!(
        unchanged,
        "failed validation cannot pop or restore any binding/catch"
    );
}
