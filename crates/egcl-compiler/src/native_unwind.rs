// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Select the next logical unwind action inside one retained native frame.
//!
//! This is not condition signaling or dynamic target search. The runtime must
//! first select a live destination, identify its owning activation, and retain
//! that activation's code/scope map. Scope identities are meaningful only within
//! that activation. Machine landing validation and dynamic state restoration
//! remain the caller's responsibility; no address is inferred here.

use crate::control_scope::{ControlScope, Ownership, ScopeKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedTarget {
    /// The selected destination belongs to another activation. Inherited OSR
    /// scopes still form a fallback barrier before leaving this native frame.
    OutsideFrame,
    /// Establishing bytecode PC in this exact activation, not a Lisp name or
    /// resume PC (both can be shared by distinct live destinations).
    Scope { push_bcp: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUnwindStep {
    /// Crossing an unselected local catch requires retiring its live record.
    /// Callers may plan further actions on the outer prefix, but must validate
    /// the final landing before changing registrations needed by fallback.
    RetireCatch {
        scope_index: usize,
    },
    /// Retire a crossed local HANDLER-CASE cluster after validating the landing.
    RetireHandler {
        scope_index: usize,
    },
    RunCleanup {
        scope_index: usize,
        /// Number of installed handlers outside this cleanup. Running cleanup
        /// continuations are not handlers. Used to retire crossed continuations.
        handler_depth: usize,
    },
    /// A logical destination, not permission to jump. The caller must validate
    /// its native landing and restore/remove the selected dynamic record (GO
    /// retains its tagbody) before entering it, or materialize fallback.
    EnterTarget {
        scope_index: usize,
    },
    LeaveFrame,
    /// Unknown target, inherited state, or a scope needing unsupported dynamic
    /// restoration. Reconstruct from the still-live frame; never skip the scope.
    Fallback,
}

/// No allocation, GC, mutation or Lisp calls. Each action preserves outer scopes
/// until reached: an inner cleanup may replace the transfer, making them irrelevant.
pub fn next_unwind_step(scopes: &[ControlScope], target: SelectedTarget) -> NativeUnwindStep {
    let selected = match target {
        SelectedTarget::OutsideFrame => None,
        SelectedTarget::Scope { push_bcp } => {
            let mut matches = scopes
                .iter()
                .enumerate()
                .filter(|(_, s)| s.push_bcp == push_bcp);
            let Some((index, scope)) = matches.next() else {
                return NativeUnwindStep::Fallback;
            };
            if matches.next().is_some()
                || !matches!(
                    scope.kind,
                    ScopeKind::Catch { .. }
                        | ScopeKind::Block { .. }
                        | ScopeKind::Tagbody { .. }
                        | ScopeKind::HandlerCase { .. }
                        | ScopeKind::RestartCase { .. }
                )
            {
                return NativeUnwindStep::Fallback;
            }
            Some(index)
        }
    };
    for (index, scope) in scopes.iter().enumerate().rev() {
        if scope.ownership != Ownership::Local {
            return NativeUnwindStep::Fallback;
        }
        if selected == Some(index) {
            return NativeUnwindStep::EnterTarget { scope_index: index };
        }
        match scope.kind {
            ScopeKind::HandlerCase { .. } => {
                return NativeUnwindStep::RetireHandler { scope_index: index };
            }
            ScopeKind::Catch { .. } => {
                return NativeUnwindStep::RetireCatch { scope_index: index };
            }
            ScopeKind::Unwind { .. } => {
                let handler_depth = scopes[..index]
                    .iter()
                    .filter(|scope| {
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
                    })
                    .count();
                return NativeUnwindStep::RunCleanup {
                    scope_index: index,
                    handler_depth,
                };
            }
            // These local records need no dynamic unregistration. The caller
            // retires superseded Cleanup continuations at the returned boundary.
            ScopeKind::Block {
                register: false, ..
            }
            | ScopeKind::Cleanup { .. } => {}
            // Registered catches/blocks/handlers/restarts and bindings must be
            // restored explicitly. Tagbody metadata does not distinguish local
            // lexical uses from NamedTag registration for escaping closures.
            // Native support must not silently erase any of these records.
            _ => return NativeUnwindStep::Fallback,
        }
    }
    NativeUnwindStep::LeaveFrame
}
