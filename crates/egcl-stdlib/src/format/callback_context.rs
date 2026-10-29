// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Scoped explicit FORMATTER continuation claims across host/native callbacks.
//!
//! The active slot owns a handle; each stack scope roots both current and saved
//! handles. Nothing stores a borrowed stack pointer or permanently retains a
//! callable. User callbacks never run with these RefCells borrowed.

use super::arguments::{Arguments, FrameToken, LogicalExit};
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::value::EgclVal;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatterActivation(FrameToken);

struct Invocation {
    callable: EgclVal,
    activation: FormatterActivation,
    arguments: Arguments,
    claimed: bool,
    pending_exit: Option<LogicalExit>,
}

impl TraceHostRoots for Invocation {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.callable);
        self.arguments.trace_host_roots(visit);
        if let Some(exit) = &mut self.pending_exit {
            exit.trace_host_roots(visit);
        }
    }
}

#[derive(Clone)]
struct InvocationHandle(Rc<RefCell<Invocation>>);

impl TraceHostRoots for InvocationHandle {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.0.borrow_mut().trace_host_roots(visit);
    }
}

// SAFETY: an owned scope belongs to one Lisp execution, including migration;
// constructing/restoring this slot invokes no Lisp and allocates no Lisp object.
static ACTIVE: egcl_rt::execution_local::ExecutionLocal<RefCell<Option<InvocationHandle>>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(None)) };

pub(super) struct InvocationScope {
    current: InvocationHandle,
    previous: Option<InvocationHandle>,
}

impl TraceHostRoots for InvocationScope {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.current.trace_host_roots(visit);
        if let Some(previous) = &mut self.previous {
            previous.trace_host_roots(visit);
        }
    }
}

impl InvocationScope {
    /// The caller roots this scope immediately, BEFORE entering any host hook
    /// which builds Lisp argument lists or invokes the expected callable.
    pub(super) fn enter(callable: EgclVal, arguments: Arguments) -> Self {
        let current = InvocationHandle(Rc::new(RefCell::new(Invocation {
            callable,
            activation: FormatterActivation(FrameToken::fresh()),
            arguments,
            claimed: false,
            pending_exit: None,
        })));
        let previous = ACTIVE.with(|slot| slot.replace(Some(current.clone())));
        Self { current, previous }
    }

    pub(super) fn activation(&self) -> FormatterActivation {
        self.current.0.borrow().activation
    }

    pub(super) fn claimed(&self) -> bool {
        self.current.0.borrow().claimed
    }

    /// Only the exact owning invocation can drain its typed result. A real
    /// callback error bypasses this operation and Drop discards the outcome.
    pub(super) fn take_exit(&self) -> Option<LogicalExit> {
        self.current.0.borrow_mut().pending_exit.take()
    }
}

impl Drop for InvocationScope {
    fn drop(&mut self) {
        ACTIVE.with(|slot| slot.replace(self.previous.take()));
        self.current.0.borrow_mut().pending_exit = None;
    }
}

/// Opaque claim available ONLY to the explicit private FORMATTER entry. The
/// host checks exact original closure-object identity before claiming; body,
/// template or owner-code identity must never be substituted for that check.
#[derive(Clone)]
pub struct FormatterEntry {
    invocation: InvocationHandle,
    arguments: Arguments,
    activation: FormatterActivation,
}

impl TraceHostRoots for FormatterEntry {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.invocation.trace_host_roots(visit);
        self.arguments.trace_host_roots(visit);
    }
}

/// Snapshot and root the callable before normalizing its representation in the
/// host. This does not claim the frame and exposes no argument/cursor state.
pub fn pending_formatter_callable() -> Option<(FormatterActivation, EgclVal)> {
    ACTIVE.with(|slot| {
        let handle = slot.borrow().clone()?;
        let state = handle.0.borrow();
        (!state.claimed).then_some((state.activation, state.callable))
    })
}

/// The caller must have compared its rooted self object to the rooted expected
/// callable using exact source-object identity. Between that nonallocating
/// comparison and this claim there must be no callback or new activation.
pub fn claim_formatter_entry(activation: FormatterActivation) -> Option<FormatterEntry> {
    ACTIVE.with(|slot| {
        let handle = slot.borrow().clone()?;
        let arguments = {
            let mut state = handle.0.borrow_mut();
            if state.activation != activation || state.claimed {
                return None;
            }
            state.claimed = true;
            state.arguments.clone()
        };
        Some(FormatterEntry {
            invocation: handle,
            arguments,
            activation,
        })
    })
}

impl FormatterEntry {
    pub(super) fn arguments(&self) -> Arguments {
        self.arguments.clone()
    }

    pub(super) fn record_exit(&self, exit: LogicalExit) {
        let mut state = self.invocation.0.borrow_mut();
        assert_eq!(state.activation, self.activation);
        assert!(state.claimed);
        assert!(state.pending_exit.is_none());
        state.pending_exit = Some(exit);
    }
}
