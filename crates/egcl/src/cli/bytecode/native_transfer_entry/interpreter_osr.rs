// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! OSR from a T0 activation into published mapped code (bliss-s0v50).
//!
//! A loop in a function that is called once never reaches the invocation
//! threshold, so without this it ran interpreted to completion. Mapped code
//! keeps the bytecode locals in its managed activation homes -- the same
//! EgclStack slots T0 uses -- and enters a loop header with them in place, as
//! a mapped T1 loop already does when it moves to T2 (`prepare_osr`). This
//! hands the interpreter's frame to that entry instead.
//!
//! Admission is deliberately narrow. The activation must hold only the local
//! BLOCK and TAGBODY records the destination's scope map expects before the
//! header: those have no runtime registration (no token, no catch tag, no
//! condition cluster and no saved cleanup), so moving the frame moves the
//! whole dynamic state. Its own special bindings may also be live, exactly
//! the ones the scope map names: their guards move to the mapped activation's
//! binding ledger, which deoptimization hands back the same way (bliss-csc8v).
//! Anything else stays interpreted.

use super::*;

/// The outcome of an attempted entry. Every decline is a property of the body
/// and its installed code, so the caller remembers it rather than re-checking
/// on every later back-edge.
pub(in crate::cli::bytecode) enum InterpreterOsr {
    /// Not admitted; the activation is untouched and keeps interpreting.
    Declined,
    /// Not admitted for the values the activation holds now; a later
    /// back-edge may enter. The activation is untouched.
    NotNow,
    /// The mapped activation owned and retired the T0 frame. The T0
    /// activation is complete with this result.
    Retired(Result<EgclVal, EgclError>),
}

/// Whether the T0 records `handlers` are exactly the local BLOCK/TAGBODY
/// scopes the destination maps before `header`, in order, and `bindings` are
/// exactly its live special bindings. Returns each binding's push PC.
fn handlers_match_scopes(
    code: &TransferCode,
    header: u32,
    handlers: &[Handler],
    bindings: &[ScopedDynBind],
) -> Option<Vec<u32>> {
    let scopes = code.control_scopes.before(header)?;
    let mut records = handlers.iter();
    let mut binds = bindings.iter();
    let mut depth = 0;
    let mut push_bcps = Vec::with_capacity(bindings.len());
    for scope in scopes {
        if scope.ownership != Ownership::Local {
            return None;
        }
        match scope.kind {
            // The activation's EnvFrame moves with the state.
            ScopeKind::LexicalEnvironment => continue,
            // A binding T0 established inside the same handler records the
            // mapped ledger would have counted (`validate_special_bindings`).
            ScopeKind::SpecialBinding { symbol } => {
                let binding = binds.next()?;
                if binding.guard.idx != symbol || binding.handler_depth != depth {
                    return None;
                }
                push_bcps.push(scope.push_bcp);
            }
            ScopeKind::Block {
                id,
                register: false,
                ..
            } => {
                let Some(Handler::Block {
                    block_id, token, ..
                }) = records.next()
                else {
                    return None;
                };
                if *block_id != id || !token.is_empty() {
                    return None;
                }
                depth += 1;
            }
            ScopeKind::Tagbody { id } => {
                let Some(Handler::Tag {
                    tagbody_id,
                    token: None,
                    tag_bcps,
                    ..
                }) = records.next()
                else {
                    return None;
                };
                if *tagbody_id != id || !tag_bcps.is_empty() {
                    return None;
                }
                depth += 1;
            }
            _ => return None,
        }
    }
    (records.next().is_none() && binds.next().is_none()).then_some(push_bcps)
}

/// Enter `native`'s mapped code at loop `header` with the T0 activation whose
/// frame is `frame`. The caller has checked that the operand stack is empty,
/// that the frame is the activation's own and that it holds no cleanup
/// continuation.
///
/// On `Retired` the frame has been popped, the activation's special bindings
/// (`bindings`, left empty) were owned and released by the mapped activation,
/// and the result is the activation's; on `Declined` nothing has changed.
/// `bindings` stays where the caller roots it until nothing more can collect.
pub(in crate::cli::bytecode) fn enter(
    native: &NativeCode,
    header: u32,
    frame: *mut Frame,
    handlers: &[Handler],
    bindings: &mut Vec<ScopedDynBind>,
    env_frame: Option<Arc<SharedCell<EnvFrame>>>,
    env: &mut Env,
) -> InterpreterOsr {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        return InterpreterOsr::Declined;
    };
    let destination = code.osr_destination();
    let Some(offset) = destination.osr_entries.get(&header).copied() else {
        return InterpreterOsr::Declined;
    };
    if !native_transfer::is_supported()
        || !local_control_native_body(&destination.body, true, true, false)
    {
        return InterpreterOsr::Declined;
    }
    let Some(push_bcps) = handlers_match_scopes(destination, header, handlers, bindings) else {
        return InterpreterOsr::Declined;
    };
    let stack = egcl_rt::current_stack();
    // The activation's frame must be the top one: no Special frame (a
    // binding or condition cluster) sits above it.
    if stack.fp() != frame
        || unsafe { usize::from((*frame).num_locals) } < usize::from(destination.body.n_locals)
    {
        return InterpreterOsr::Declined;
    }
    // A raw double home imports only a double-float (bliss-vwpaz). Another
    // datum there now is a property of this iteration, not of the body.
    if !destination.osr_imports_admit(header, |slot| unsafe { slot_get(frame, slot) }) {
        return InterpreterOsr::NotNow;
    }
    // The raw imports' current words, compared (never dereferenced) after
    // `prepare` to show a collection relocated them in place (osr=debug).
    let raw_words = |slots: &[u16]| -> Vec<u64> {
        slots
            .iter()
            .map(|&slot| unsafe { slot_get(frame, slot) }.0)
            .collect()
    };
    let raw_slots = destination
        .osr_raw_doubles
        .get(&header)
        .filter(|_| egcl_rt::log::enabled("osr", egcl_rt::log::DEBUG));
    let before = raw_slots.map(|slots| raw_words(slots));
    // Reserve every ledger while failure still means "keep interpreting".
    // `prepare` may collect: the frame's slots are scanned where they are.
    let Ok(state) = ActivationState::prepare(destination, env) else {
        return InterpreterOsr::Declined;
    };
    if let (Some(slots), Some(before)) = (raw_slots, before) {
        let after = raw_words(slots);
        egcl_rt::blog!(
            "osr",
            egcl_rt::log::DEBUG,
            "{}: raw double imports at header {header}: slots {slots:?} {} ({before:#x?} -> {after:#x?})",
            display_fn_name(&destination.body.name),
            if before == after {
                "unmoved"
            } else {
                "relocated before entry"
            }
        );
    }
    let mut state = Box::new(state);
    // The ledger is reserved for every BindSpecial of the body, so moving the
    // live guards into it below cannot allocate.
    if state.specials.capacity() < bindings.len() {
        return InterpreterOsr::Declined;
    }
    state.env_frame = env_frame;
    egcl_rt::rooted_ref!(_state = &mut *state);
    let slots = destination.slots.max(unsafe { (*frame).num_locals });
    // Nothing allocates, collects or fails between growing the frame and the
    // destination committing its own map in `run_activation`. New slots are
    // NIL; the locals stay where T0 left them, which is where the managed
    // homes read them. The frame is the top one, so no Special suffix moves.
    if unsafe { stack.grow_frame_with_special_suffix(frame, slots, destination.code_info) }
        .is_none()
    {
        return InterpreterOsr::Declined;
    }
    drop(_state);
    // Hand the live bindings over without restoring them: the mapped activation
    // now unbinds each at its UnbindSpecial or on unwind, and a deoptimization
    // returns them to a rebuilt T0 activation. A move, not an allocation.
    state.specials.extend(
        push_bcps
            .into_iter()
            .zip(bindings.drain(..))
            .map(|(push_bcp, binding)| SavedSpecial { push_bcp, binding }),
    );
    if let Some(symbol) = destination.installed_symbol {
        OSR_ENTRY_COUNTS.with(|counts| {
            let mut counts = counts.borrow_mut();
            let count = counts.entry(symbol).or_insert(0);
            *count = count.saturating_add(1);
        });
    }
    egcl_rt::blog!(
        "osr",
        egcl_rt::log::DEBUG,
        "{}: T0 activation entered mapped {} code at header {header}",
        display_fn_name(&destination.body.name),
        if destination.is_t2 { "T2" } else { "T1" }
    );
    // A T0 source never entered native code, so count this activation against
    // the native depth cap as an ordinary entry does.
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let _depth = NativeDepthGuard;
    let resume = OsrResume {
        frame,
        offset,
        state,
    };
    InterpreterOsr::Retired(destination.run_activation(&[], env, Some(resume), NIL))
}
