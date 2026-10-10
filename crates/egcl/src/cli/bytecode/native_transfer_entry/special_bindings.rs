// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::control_scope::{ControlScope, Ownership, ScopeKind};
use egcl_compiler::t2::emit::{
    TransferCallRequest, TRANSFER_GLOBAL_LOAD_REQUEST, TRANSFER_GLOBAL_STORE_REQUEST,
    TRANSFER_SPECIAL_BIND_REQUEST, TRANSFER_SPECIAL_UNBIND_REQUEST,
};

/// Preserve the original binding guard; re-establishing a binding at OSR or
/// recovery would save the inner value instead of its real dynamic predecessor.
pub(super) struct SavedSpecial {
    pub(super) push_bcp: u32,
    pub(super) binding: ScopedDynBind,
}

impl egcl_rt::gc::TraceHostRoots for SavedSpecial {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.binding.guard.trace_host_roots(visit);
    }
}

fn handler_scope(scope: &ControlScope) -> bool {
    matches!(
        scope.kind,
        ScopeKind::Block { .. }
            | ScopeKind::Tagbody { .. }
            | ScopeKind::Catch { .. }
            | ScopeKind::Unwind { .. }
            | ScopeKind::HandlerCase { .. }
            | ScopeKind::HandlerBind { .. }
            | ScopeKind::RestartCase { .. }
    )
}

/// Source scope maps name every live binding in lexical order. Neither cleanup
/// continuations nor bindings themselves occupy a T0 Handler slot.
pub(super) unsafe fn validate_special_bindings(
    context: &CaptureContext,
    scopes: &[ControlScope],
) -> bool {
    let mut saved = unsafe { &*context.specials }.iter();
    let mut depth = 0;
    for scope in scopes {
        if let ScopeKind::SpecialBinding { symbol } = scope.kind {
            let Some(binding) = saved.next() else {
                return false;
            };
            if scope.ownership != Ownership::Local
                || binding.push_bcp != scope.push_bcp
                || binding.binding.guard.idx != symbol
                || binding.binding.handler_depth != depth
            {
                return false;
            }
        } else if handler_scope(scope) {
            depth += 1;
        }
    }
    saved.next().is_none()
}

/// Published helper-v2 requests. These operations preserve the current multiple
/// values; explicit ClearMv and SetValues bytecodes retain that responsibility.
pub(super) unsafe fn special_request(call: &TransferCallRequest) -> Result<EgclVal, EgclError> {
    let tag = call.symbol & !u64::from(u32::MAX);
    let index = call.symbol as u32;
    if tag == TRANSFER_GLOBAL_LOAD_REQUEST {
        if call.nargs != 0 {
            return Err(invalid_capture());
        }
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        let symbol = EgclVal::from_symbol_index(index);
        let env = unsafe { &*env };
        return env.lookup_var_symbol(symbol)
            .or_else(|| env.lookup_var(&sym_name(symbol)))
            .ok_or(EgclError::UnboundVariable(symbol));
    }
    if tag == TRANSFER_GLOBAL_STORE_REQUEST {
        if call.nargs != 1 || call.args.is_null() {
            return Err(invalid_capture());
        }
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        egcl_rt::rooted!(value = unsafe { *call.args });
        unsafe { &mut *env }.assign_var_symbol(EgclVal::from_symbol_index(index), *value)?;
        return Ok(NIL);
    }
    let context = CAPTURE.with(Cell::get);
    if context.is_null() {
        return Err(invalid_capture());
    }
    let context = unsafe { &*context };
    let code = unsafe { &*context.owner };
    let before = code
        .control_scopes
        .before(index)
        .ok_or_else(invalid_capture)?;
    if !unsafe { validate_special_bindings(context, before) } {
        return Err(invalid_capture());
    }
    let bindings = unsafe { &mut *context.specials };
    match (tag, code.body.code.get(index as usize)) {
        (TRANSFER_SPECIAL_BIND_REQUEST, Some(Instr::BindSpecial(symbol))) => {
            if call.nargs != 1 || call.args.is_null() || bindings.len() == bindings.capacity() {
                return Err(invalid_capture());
            }
            let handler_depth = before.iter().filter(|scope| handler_scope(scope)).count();
            // Establishment only updates the execution-local symbol overlay.
            // The pre-reserved rooted ledger owns its saved value immediately.
            let guard =
                DynBind::establish(EgclVal::from_symbol_index(*symbol), unsafe { *call.args });
            bindings.push(SavedSpecial {
                push_bcp: index,
                binding: ScopedDynBind {
                    guard,
                    handler_depth,
                },
            });
        }
        (TRANSFER_SPECIAL_UNBIND_REQUEST, Some(Instr::UnbindSpecial(count))) => {
            if call.nargs != 0 || usize::from(*count) > bindings.len() {
                return Err(invalid_capture());
            }
            for _ in 0..*count {
                bindings.pop();
            }
        }
        _ => return Err(invalid_capture()),
    }
    Ok(NIL)
}
