// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Owned snapshots of logical Lisp calls, without live stack pointers.
//!
//! Snapshots retain Lisp argument values: root them with `rooted!` or
//! `rooted_ref!` across allocation, collection, or a safepoint. They remain
//! valid after execution resumes or unwinds, but do not deep-copy mutable
//! argument objects. Missing metadata is reported explicitly.

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
    /// Original actual arguments when recorded. None means unavailable;
    /// Some(empty) means a call with zero arguments. NIL is an ordinary value.
    pub arguments: Option<Vec<EgclVal>>,
}

impl TraceHostRoots for LogicalFrame {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.arguments.trace_host_roots(visit);
    }
}

#[derive(Clone)]
pub(crate) struct RecordedCall {
    pub anchor: usize,
    pub frame: LogicalFrame,
}

impl TraceHostRoots for RecordedCall {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.frame.trace_host_roots(visit);
    }
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
    pub recorded: Vec<RecordedCall>,
}

impl TraceHostRoots for PendingBacktrace {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.recorded.trace_host_roots(visit);
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
        let mut recorded = self.recorded.iter().rev().peekable();
        for (address, function, origin) in &self.frames {
            let mut recorded_managed = false;
            while recorded.peek().is_some_and(|call| call.anchor == *address) {
                let frame = &recorded.next().unwrap().frame;
                recorded_managed |= frame.origin == FrameOrigin::Managed;
                frames.push(frame.clone());
            }
            if recorded_managed {
                continue;
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
                    arguments: None,
                });
            }
        }
        frames.extend(recorded.map(|call| call.frame.clone()));
        frames.truncate(count);
        frames
    }
}
