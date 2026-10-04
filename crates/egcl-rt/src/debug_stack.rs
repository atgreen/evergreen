// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Owned snapshots of logical Lisp calls. These contain no live stack pointers
//! or movable Lisp values, and remain valid after execution resumes or unwinds.
//! Source locations, arguments and lexical bindings need compiler metadata;
//! this foundation reports only identities the runtime actually knows.

use crate::gc::TraceHostRoots;
use crate::stack::{Frame, FrameType, FrameWalker};
use crate::{EgclError, EgclVal, FiberId};

pub use crate::thread::FiberCallFrame as CallFrame;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOrigin {
    Interpreted,
    /// A managed activation; this alone does not identify its compiler tier.
    Managed,
    /// The entry function of a fiber that has not started executing.
    Entry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalFrame {
    /// None means the function identity is unavailable, not a function named NIL.
    pub function: Option<String>,
    pub origin: FrameOrigin,
}

/// Capture the current Lisp execution, innermost call first.
pub fn capture_current(count: usize) -> Vec<LogicalFrame> {
    crate::thread::current_backtrace(count)
}

/// Capture an unmounted fiber. None means it is still mounted/running; callers
/// must not mistake that for an empty stack. The state lock excludes mounting.
pub fn capture_fiber(id: FiberId, count: usize) -> Result<Option<Vec<LogicalFrame>>, EgclError> {
    crate::thread::fiber_logical_backtrace(id, count)
}

#[derive(Default)]
pub(crate) struct PendingBacktrace {
    // Control records retain their addresses as interpreter anchors, but do not
    // become function calls in the result.
    frames: Vec<(usize, Option<EgclVal>, FrameOrigin)>,
    pub interpreted: Vec<(usize, String)>,
}

impl TraceHostRoots for PendingBacktrace {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for (_, function, _) in &mut self.frames {
            if let Some(function) = function {
                visit(function);
            }
        }
    }
}

impl PendingBacktrace {
    /// Copy before releasing the owner's stack/mount protection. No Lisp
    /// allocation or safepoint occurs; only Rust-owned storage is allocated.
    ///
    /// # Safety
    /// The frame chain must remain valid and immobile throughout this call.
    pub unsafe fn copy_stack(&mut self, fp: *const Frame, count: usize) {
        let mut calls = 0;
        for frame in unsafe { FrameWalker::new(fp) } {
            if calls == count {
                break;
            }
            let record = unsafe { &*frame };
            let function = (record.frame_type() == FrameType::Call).then_some(record.function);
            calls += usize::from(function.is_some());
            self.frames
                .push((frame as usize, function, FrameOrigin::Managed));
        }
    }

    pub fn entry(&mut self, function: EgclVal) {
        self.frames.push((0, Some(function), FrameOrigin::Entry));
    }

    pub fn resolve(&self, count: usize) -> Vec<LogicalFrame> {
        let mut frames = Vec::new();
        let mut interpreted = self.interpreted.iter().rev().peekable();
        for (address, function, origin) in &self.frames {
            while interpreted
                .peek()
                .is_some_and(|(anchor, _)| anchor == address)
            {
                frames.push(LogicalFrame {
                    function: Some(interpreted.next().unwrap().1.clone()),
                    origin: FrameOrigin::Interpreted,
                });
            }
            if let Some(function) = function {
                let name = if crate::function::is_interpreted_function(*function) {
                    crate::function::name(*function)
                } else {
                    *function
                };
                frames.push(LogicalFrame {
                    function: crate::symbols::symbol_name_of(name),
                    origin: *origin,
                });
            }
        }
        frames.extend(interpreted.map(|(_, name)| LogicalFrame {
            function: Some(name.clone()),
            origin: FrameOrigin::Interpreted,
        }));
        frames.truncate(count);
        frames
    }
}
