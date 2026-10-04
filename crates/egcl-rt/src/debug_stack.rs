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
    /// An outbound C call boundary, not an unwound internal C activation.
    Foreign,
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

struct CopiedFrame {
    address: usize,
    function: Option<EgclVal>,
    origin: FrameOrigin,
    installed_name: Option<String>,
    arguments: Option<Vec<EgclVal>>,
}

#[derive(Default)]
pub(crate) struct PendingBacktrace {
    // Control records retain their addresses as interpreter anchors, but do not
    // become function calls in the result.
    frames: Vec<CopiedFrame>,
    pub recorded: Vec<RecordedCall>,
}

impl TraceHostRoots for PendingBacktrace {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.recorded.trace_host_roots(visit);
        for frame in &mut self.frames {
            frame.arguments.trace_host_roots(visit);
            if let Some(function) = &mut frame.function {
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
    /// The frame chain and each non-null CodeInfo must remain valid and immobile
    /// throughout this call.
    pub unsafe fn copy_stack(&mut self, fp: *const Frame, count: usize) {
        let mut calls = 0;
        for frame in unsafe { FrameWalker::new(fp) } {
            if calls == count {
                break;
            }
            let record = unsafe { &*frame };
            let function = (record.frame_type() == FrameType::Call).then_some(record.function);
            calls += usize::from(function.is_some());
            let foreign = record.flags & crate::stack::FOREIGN_CALL != 0;
            let installed_name = if foreign {
                Some(crate::ffi::foreign_frame_name(record.return_pc as usize))
            } else if function.is_some() && !record.code_info.is_null() {
                unsafe { &*record.code_info }.function_name()
            } else {
                None
            };
            let arguments = if function.is_some() && !record.code_info.is_null() {
                unsafe { &*record.code_info }.original_arguments(unsafe { record.locals() })
            } else {
                None
            };
            self.frames.push(CopiedFrame {
                address: frame as usize,
                function,
                origin: if foreign {
                    FrameOrigin::Foreign
                } else {
                    FrameOrigin::Managed
                },
                installed_name,
                arguments,
            });
        }
    }

    pub fn entry(&mut self, function: EgclVal) {
        self.frames.push(CopiedFrame {
            address: 0,
            function: Some(function),
            origin: FrameOrigin::Entry,
            installed_name: None,
            arguments: None,
        });
    }

    pub fn resolve(&self, count: usize) -> Vec<LogicalFrame> {
        let mut frames = Vec::new();
        let mut recorded = self.recorded.iter().rev().peekable();
        for CopiedFrame {
            address,
            function,
            origin,
            installed_name,
            arguments,
        } in &self.frames
        {
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
                    function: installed_name
                        .clone()
                        .or_else(|| crate::symbols::symbol_name_of(name)),
                    origin: *origin,
                    arguments: arguments.clone(),
                });
            }
        }
        frames.extend(recorded.map(|call| call.frame.clone()));
        frames.truncate(count);
        frames
    }
}

/// A zero-slot call record on the execution-owned managed stack. Foreign calls
/// already publish this stack and pin their fiber. This guard adds no name
/// lookup, Lisp allocation, mutex, or per-call heap storage. Declare it before
/// entering Native state and drop it after returning to Running.
#[cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "powerpc64", target_endian = "little", unix)
))]
pub(crate) struct ForeignFrame {
    stack: &'static crate::stack::EgclStack,
    frame: *const Frame,
    _execution_affine: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "powerpc64", target_endian = "little", unix)
))]
impl ForeignFrame {
    pub(crate) fn enter(target: *const ()) -> Result<Self, EgclError> {
        let stack = crate::thread::current_stack();
        let frame = stack
            .push_frame(
                crate::value::NIL,
                std::ptr::null(),
                0,
                crate::stack::FOREIGN_CALL,
            )
            .ok_or_else(|| {
                EgclError::StackOverflow(crate::thread::current_fiber_id().unwrap_or_else(|| {
                    crate::thread::FiberId(crate::thread::current_thread_id().0)
                }))
            })?;
        // No safepoint occurs between publishing the frame and its target.
        // This flagged frame contains no Lisp arguments or CodeInfo.
        unsafe { (*frame).return_pc = target.cast() };
        Ok(Self {
            stack,
            frame,
            _execution_affine: std::marker::PhantomData,
        })
    }
}

#[cfg(any(
    all(target_arch = "x86_64", any(unix, windows)),
    all(target_arch = "aarch64", unix),
    all(target_arch = "powerpc64", target_endian = "little", unix)
))]
impl Drop for ForeignFrame {
    fn drop(&mut self) {
        debug_assert_eq!(self.stack.fp(), self.frame);
        self.stack.pop_frame();
    }
}
