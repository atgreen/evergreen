// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ownership checks for condition clusters retained across OSR or guard recovery.

use super::*;
use egcl_compiler::control_scope::{ControlScope, Ownership, ScopeKind};

/// Read only a contiguous Special frame immediately above an already validated
/// predecessor. No allocation, interning or safepoint may occur during this walk.
unsafe fn cluster_values<'a>(
    frame: *mut Frame,
    previous: *mut Frame,
    top: usize,
) -> Option<&'a [EgclVal]> {
    let expected = unsafe {
        previous
            .add(1)
            .cast::<EgclVal>()
            .add(usize::from((*previous).num_locals))
    } as usize;
    let start = frame as usize;
    if start != expected || start.checked_add(std::mem::size_of::<Frame>())? > top {
        return None;
    }
    let header = unsafe { &*frame };
    let values = unsafe { frame.add(1).cast::<EgclVal>() };
    let end = (values as usize)
        .checked_add(usize::from(header.num_locals) * std::mem::size_of::<EgclVal>())?;
    if end > top
        || header.prev_fp != previous
        || header.flags != egcl_rt::stack::FrameType::Special as u32
        || !header.function.is_nil()
        || !header.code_info.is_null()
        || !header.return_pc.is_null()
    {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(values, usize::from(header.num_locals)) })
}

/// Validate this activation's complete condition tail without changing any
/// registration. Older caller-owned Env prefixes and stack frames remain valid.
/// The caller retains context and its code, and runs on the owning mutator.
pub(super) unsafe fn validate_condition_clusters(
    context: &CaptureContext,
    scopes: &[ControlScope],
    env: &Env,
) -> bool {
    if !context.recursive.is_null() {
        return false;
    }
    let stack = egcl_rt::current_stack();
    let top = stack.sp() as usize;
    let frame = context.frame;
    let start = frame as usize;
    if start < stack.base() as usize
        || start % std::mem::align_of::<Frame>() != 0
        || start
            .checked_add(std::mem::size_of::<Frame>())
            .is_none_or(|end| end > top)
    {
        return false;
    }
    let header = unsafe { &*frame };
    let callable_end = (unsafe { frame.add(1) } as usize)
        .checked_add(usize::from(header.num_locals) * std::mem::size_of::<EgclVal>());
    if callable_end.is_none_or(|end| end > top) || usize::from(header.num_locals) < context.slots {
        return false;
    }
    let body = unsafe { &*context.body };
    let mut handlers = unsafe { &*context.handlers }.iter();
    let mut binds = unsafe { &*context.handler_binds }.iter();
    let mut restarts = unsafe { &*context.restart_cases }.iter();
    let mut dynamic = unsafe { &*context.dynamic_scopes }.iter();
    let mut handler_end = None;
    let mut restart_end = None;
    let mut previous = frame;
    for scope in scopes {
        let cluster = match scope.kind {
            ScopeKind::HandlerCase { table_index } => {
                if scope.ownership != Ownership::Local {
                    return false;
                }
                let Some(saved) = handlers.next() else {
                    return false;
                };
                let Some(info) = body.handler_cases.get(table_index as usize) else {
                    return false;
                };
                let Some(Instr::PushHandlerCase { hc, .. }) =
                    body.code.get(scope.push_bcp as usize)
                else {
                    return false;
                };
                let Some(DynamicScope::HandlerCase {
                    cluster_base,
                    cluster_frame,
                }) = dynamic.next()
                else {
                    return false;
                };
                if *hc != table_index
                    || saved.push_bcp != scope.push_bcp
                    || (*cluster_base, *cluster_frame) != (saved.cluster_base, saved.cluster_frame)
                    || handler_end.is_some_and(|end| end != saved.cluster_base)
                {
                    return false;
                }
                let Some(live) = env.handlers.get(saved.cluster_base) else {
                    return false;
                };
                let Some(values) = (unsafe { cluster_values(saved.cluster_frame, previous, top) })
                else {
                    return false;
                };
                if saved.clauses.len() != info.clauses.len()
                    || live.entries.len() != info.clauses.len()
                    || values.len() != info.clauses.len().saturating_mul(2)
                {
                    return false;
                }
                for ((saved, info), live) in
                    saved.clauses.iter().zip(&info.clauses).zip(&live.entries)
                {
                    if saved.type_name != info.type_name
                        || live.type_name != info.type_name
                        || saved.body_bcp != info.body_bcp
                        || saved.var_slot != info.var_slot
                        || !matches!(&live.handler, HandlerImpl::HandlerCase { token, .. }
                            if token == &saved.token)
                    {
                        return false;
                    }
                }
                handler_end = saved.cluster_base.checked_add(1);
                if handler_end.is_none() {
                    return false;
                }
                saved.cluster_frame
            }
            ScopeKind::HandlerBind { table_index } => {
                if scope.ownership != Ownership::Local {
                    return false;
                }
                let Some(saved) = binds.next() else {
                    return false;
                };
                let Some(info) = body.handler_binds.get(table_index as usize) else {
                    return false;
                };
                let Some(Instr::PushHandlerBind { hb }) = body.code.get(scope.push_bcp as usize)
                else {
                    return false;
                };
                let Some(DynamicScope::HandlerBind {
                    cluster_base,
                    cluster_frame,
                }) = dynamic.next()
                else {
                    return false;
                };
                if *hb != table_index
                    || saved.push_bcp != scope.push_bcp
                    || (*cluster_base, *cluster_frame) != (saved.cluster_base, saved.cluster_frame)
                    || handler_end.is_some_and(|end| end != saved.cluster_base)
                {
                    return false;
                }
                let Some(live) = env.handlers.get(saved.cluster_base) else {
                    return false;
                };
                let Some(values) = (unsafe { cluster_values(saved.cluster_frame, previous, top) })
                else {
                    return false;
                };
                if live.entries.len() != info.types.len()
                    || values.len() != info.types.len().saturating_mul(2)
                {
                    return false;
                }
                for ((live, name), values) in live
                    .entries
                    .iter()
                    .zip(&info.types)
                    .zip(values.chunks_exact(2))
                {
                    if live.type_name != *name
                        || !matches!(&live.handler, HandlerImpl::Function(function) if *function == values[1])
                    {
                        return false;
                    }
                }
                handler_end = saved.cluster_base.checked_add(1);
                if handler_end.is_none() {
                    return false;
                }
                saved.cluster_frame
            }
            ScopeKind::RestartCase {
                table_index,
                resume_bcp,
            } => {
                if scope.ownership != Ownership::Local {
                    return false;
                }
                let Some(saved) = restarts.next() else {
                    return false;
                };
                let Some(info) = body.restart_cases.get(table_index as usize) else {
                    return false;
                };
                let Some(Instr::PushRestartCase {
                    rc,
                    resume_bcp: instruction_resume,
                    sp_restore,
                }) = body.code.get(scope.push_bcp as usize)
                else {
                    return false;
                };
                let Some(DynamicScope::RestartCase {
                    restart_base,
                    cluster_frame,
                }) = dynamic.next()
                else {
                    return false;
                };
                if *rc != table_index
                    || *instruction_resume != resume_bcp
                    || *sp_restore != scope.sp_restore
                    || saved.push_bcp != scope.push_bcp
                    || saved.resume_bcp != resume_bcp
                    || saved.sp_restore != scope.sp_restore
                    || (*restart_base, *cluster_frame) != (saved.restart_base, saved.cluster_frame)
                    || restart_end.is_some_and(|end| end != saved.restart_base)
                {
                    return false;
                }
                let Some(end) = saved.restart_base.checked_add(info.restarts.len()) else {
                    return false;
                };
                let Some(live) = env.restarts.get(saved.restart_base..end) else {
                    return false;
                };
                let Some(values) = (unsafe { cluster_values(saved.cluster_frame, previous, top) })
                else {
                    return false;
                };
                if values.len() != info.restarts.len().saturating_mul(5) {
                    return false;
                }
                if !live.iter().zip(&info.restarts).all(|(live, info)| {
                    live.name == info.name
                        && live.group_base == saved.restart_base
                        && live.unwind_on_invoke
                        && matches!(&live.function, RestartFunction::Bytecode { .. })
                }) {
                    return false;
                }
                restart_end = Some(end);
                saved.cluster_frame
            }
            _ => continue,
        };
        previous = cluster;
    }
    handlers.next().is_none()
        && binds.next().is_none()
        && restarts.next().is_none()
        && dynamic.next().is_none()
        && handler_end.is_none_or(|end| end == env.handlers.len())
        && restart_end.is_none_or(|end| end == env.restarts.len())
        && stack.fp() == previous
        && unsafe {
            previous
                .add(1)
                .cast::<EgclVal>()
                .add(usize::from((*previous).num_locals))
        } as usize
            == top
}

/// Immediately follow a successful owned-tail growth, before any allocation or
/// safepoint. The stack primitive repaired the frame links; update all private
/// pointers to those relocated headers, leaving Env registrations unchanged.
pub(super) unsafe fn relocate_condition_clusters(context: &mut CaptureContext, delta: usize) {
    unsafe fn relocate(frame: &mut *mut Frame, delta: usize) {
        *frame = unsafe { (*frame).cast::<u8>().add(delta).cast() };
    }
    for saved in unsafe { &mut *context.handlers } {
        unsafe {
            relocate(&mut saved.cluster_frame, delta);
        }
    }
    for saved in unsafe { &mut *context.handler_binds } {
        unsafe {
            relocate(&mut saved.cluster_frame, delta);
        }
    }
    for saved in unsafe { &mut *context.restart_cases } {
        unsafe {
            relocate(&mut saved.cluster_frame, delta);
        }
    }
    for scope in unsafe { &mut *context.dynamic_scopes } {
        let (DynamicScope::HandlerCase { cluster_frame, .. }
        | DynamicScope::HandlerBind { cluster_frame, .. }
        | DynamicScope::RestartCase { cluster_frame, .. }) = scope;
        unsafe {
            relocate(cluster_frame, delta);
        }
    }
    unsafe { &mut *context.cluster_frames }.clear();
}
