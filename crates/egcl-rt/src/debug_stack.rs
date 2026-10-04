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

struct RecordedCallSlot {
    anchor: usize,
    function: String,
    function_available: bool,
    origin: FrameOrigin,
    arguments: Vec<EgclVal>,
    arguments_available: bool,
}

const RETAINED_FUNCTION_NAME_CAPACITY: usize = 128;
const RETAINED_ARGUMENT_CAPACITY: usize = 64;

impl Default for RecordedCallSlot {
    fn default() -> Self {
        Self {
            anchor: 0,
            function: String::new(),
            function_available: false,
            origin: FrameOrigin::Interpreted,
            arguments: Vec::new(),
            arguments_available: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct RecordedCalls {
    active: usize,
    slots: Vec<RecordedCallSlot>,
}

impl RecordedCalls {
    pub fn push(
        &mut self,
        anchor: usize,
        function: Option<&str>,
        arguments: Option<&[EgclVal]>,
        origin: FrameOrigin,
    ) {
        if self.active == self.slots.len() {
            self.slots.push(RecordedCallSlot::default());
        }
        let slot = &mut self.slots[self.active];
        slot.anchor = anchor;
        slot.function.clear();
        slot.function_available = function.is_some();
        if let Some(function) = function {
            slot.function.push_str(function);
        }
        slot.origin = origin;
        slot.arguments.clear();
        slot.arguments_available = arguments.is_some();
        if let Some(arguments) = arguments {
            slot.arguments.extend_from_slice(arguments);
        }
        self.active += 1;
    }

    pub fn pop(&mut self) {
        self.active = self
            .active
            .checked_sub(1)
            .expect("recorded call stack underflow");
        let slot = &mut self.slots[self.active];
        slot.function.clear();
        slot.function_available = false;
        slot.arguments.clear();
        slot.arguments_available = false;
        if slot.function.capacity() > RETAINED_FUNCTION_NAME_CAPACITY {
            slot.function = String::new();
        }
        if slot.arguments.capacity() > RETAINED_ARGUMENT_CAPACITY {
            slot.arguments = Vec::new();
        }
    }

    pub fn snapshot(&self, count: usize) -> Vec<RecordedCall> {
        self.slots[..self.active]
            .iter()
            .skip(self.active.saturating_sub(count))
            .map(|slot| RecordedCall {
                anchor: slot.anchor,
                frame: LogicalFrame {
                    function: slot.function_available.then(|| slot.function.clone()),
                    origin: slot.origin,
                    arguments: slot.arguments_available.then(|| slot.arguments.clone()),
                },
            })
            .collect()
    }
}

impl TraceHostRoots for RecordedCalls {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for slot in &mut self.slots[..self.active] {
            slot.arguments.trace_host_roots(visit);
        }
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

#[cfg(test)]
mod recorded_call_tests {
    use super::*;

    #[test]
    fn call_depth_reuses_storage_without_rooting_inactive_values() {
        let mut calls = RecordedCalls::default();
        calls.push(
            17,
            Some("A-LONG-RECORDED-FUNCTION-NAME"),
            Some(&[EgclVal::from_fixnum(1), EgclVal::from_fixnum(2)]),
            FrameOrigin::Interpreted,
        );
        let name_storage = calls.slots[0].function.as_ptr();
        let argument_storage = calls.slots[0].arguments.as_ptr();
        calls.pop();

        let mut visits = 0;
        calls.trace_host_roots(&mut |_| visits += 1);
        assert_eq!(visits, 0, "inactive capacity must not retain Lisp roots");

        calls.push(
            23,
            Some("SHORT"),
            Some(&[EgclVal::from_fixnum(3)]),
            FrameOrigin::Managed,
        );
        assert_eq!(calls.slots[0].function.as_ptr(), name_storage);
        assert_eq!(calls.slots[0].arguments.as_ptr(), argument_storage);
        let snapshot = calls.snapshot(1);
        assert_eq!(snapshot[0].anchor, 23);
        assert_eq!(snapshot[0].frame.function.as_deref(), Some("SHORT"));
        assert_eq!(
            snapshot[0].frame.arguments.as_deref(),
            Some(&[EgclVal::from_fixnum(3)][..])
        );
        assert_eq!(snapshot[0].frame.origin, FrameOrigin::Managed);

        calls.pop();
        calls.push(29, None, None, FrameOrigin::Interpreted);
        let snapshot = calls.snapshot(1);
        assert_eq!(snapshot[0].frame.function, None);
        assert_eq!(snapshot[0].frame.arguments, None);
        calls.pop();

        let long_name = "X".repeat(1024);
        let many_arguments = vec![EgclVal::from_fixnum(7); 256];
        calls.push(
            31,
            Some(&long_name),
            Some(&many_arguments),
            FrameOrigin::Interpreted,
        );
        calls.pop();
        assert!(calls.slots[0].function.capacity() <= 128);
        assert!(calls.slots[0].arguments.capacity() <= 64);
    }
}
