// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Rooted Lisp callback ownership and containment above the native entry ABI.
use super::{AlienType, callback::CallbackAdapter, marshal_to_c, unmarshal_from_c};
use crate::{EgclError, EgclVal, gc::CrossThreadRoot, value::NIL};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub type CallbackRunner = fn(EgclVal, &[EgclVal]) -> Result<EgclVal, EgclError>;
static RUNNER: OnceLock<CallbackRunner> = OnceLock::new();

/// Install the evaluator's function-value dispatcher. No evaluator dependency
/// is introduced into the runtime, and no environment crosses native threads.
pub fn set_callback_runner(runner: CallbackRunner) {
    let _ = RUNNER.set(runner);
}

thread_local! {
    // ForeignStateScope pins a fiber while these carrier-local scopes are live.
    // Successful calls retain only None; failures own separately rooted snapshots.
    static CALL_ERRORS: RefCell<Vec<Option<Box<CallbackFailure>>>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct ForeignCallErrors;
impl ForeignCallErrors {
    pub(super) fn enter() -> Self {
        CALL_ERRORS.with(|scopes| scopes.borrow_mut().push(None));
        Self
    }
    pub(super) fn finish(self) -> Result<(), EgclError> {
        let error =
            CALL_ERRORS.with(|scopes| scopes.borrow_mut().last_mut().and_then(Option::take));
        // The native-state guard has already unpinned the fiber. Restoring the
        // snapshot can poll and migrate it, so pop this carrier's scope FIRST.
        drop(self);
        error.map_or(Ok(()), |error| Err(error.into_error()))
    }
}
impl Drop for ForeignCallErrors {
    fn drop(&mut self) {
        CALL_ERRORS.with(|scopes| {
            scopes.borrow_mut().pop();
        });
    }
}

/// Only error paths allocate this payload. The C boundary retains diagnostic
/// text and historical frames, never a Lisp condition or a nonlocal-exit token.
struct CallbackFailure {
    message: String,
    backtrace: Option<CrossThreadRoot<Vec<crate::debug_stack::LogicalFrame>>>,
}

impl CallbackFailure {
    fn message(message: String) -> Box<Self> {
        Box::new(Self {
            message,
            backtrace: None,
        })
    }

    /// Called while the callback is still an admitted Running mutator. Publishing
    /// the trace before returning to Native state lets later callbacks or other
    /// mutators relocate its objects even after every original frame has left.
    fn from_error(error: EgclError) -> Box<Self> {
        let message = error.to_string();
        let backtrace = match error {
            EgclError::Traced(details) => Some(details.backtrace),
            EgclError::Signalled(details) => Some(details.backtrace),
            _ => None,
        };
        Box::new(Self {
            message,
            backtrace: backtrace.map(CrossThreadRoot::new),
        })
    }

    /// Called after the outbound call has restored Running state and removed its
    /// carrier-local error scope. The root reader may acknowledge a collection.
    fn into_error(self) -> EgclError {
        let error = EgclError::FfiError(self.message);
        let Some(backtrace) = self.backtrace else {
            return error;
        };
        crate::rooted!(frames = Vec::new());
        backtrace.with_gc_stable_mutator(|saved| *frames = saved.clone());
        EgclError::Traced(Box::new(crate::error::TracedError {
            error,
            backtrace: std::mem::take(&mut *frames),
        }))
    }
}

struct Context {
    closure: CrossThreadRoot<EgclVal>,
    result: AlienType,
    arguments: Vec<AlienType>,
    error: Mutex<Option<String>>,
    active: AtomicUsize,
}

impl Context {
    fn invoke(&self, slots: &[u64]) -> Result<u64, EgclError> {
        crate::rooted!(closure = NIL);
        // Copy directly into an already registered local root while the
        // cross-thread root excludes relocation. Never allocate under its gate.
        self.closure.with_gc_stable_mutator(|value| {
            *closure = *value;
        });
        crate::rooted!(arguments = Vec::with_capacity(slots.len()));
        for (slot, ty) in slots.iter().zip(&self.arguments) {
            let argument = unmarshal_from_c(*slot, ty)?;
            arguments.push(argument);
        }
        let runner = RUNNER
            .get()
            .ok_or_else(|| EgclError::FfiError("no Lisp callback runner installed".into()))?;
        crate::rooted!(result = runner(*closure, &arguments)?);
        marshal_to_c(*result, &self.result)
    }

    fn record_error(&self, mut failure: Box<CallbackFailure>) {
        failure.message = format!("foreign callback failed: {}", failure.message);
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure.message.clone());
        CALL_ERRORS.with(|scopes| {
            if let Some(error) = scopes.borrow_mut().last_mut() {
                if error.is_none() {
                    *error = Some(failure);
                }
            }
        });
    }
}

unsafe extern "C" fn dispatch(context: *mut (), slots: *const u64) -> u64 {
    // SAFETY: LispCallback owns this stable box and its executable entry. The
    // foreign lifetime contract forbids destruction while an entry is active.
    let context = unsafe { &*context.cast::<Context>() };
    struct ActiveEntry<'a>(&'a AtomicUsize);
    impl Drop for ActiveEntry<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    context.active.fetch_add(1, Ordering::SeqCst);
    let _active = ActiveEntry(&context.active);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(unix)]
        crate::runtime::ensure_signal_stack()
            .map_err(|error| CallbackFailure::message(error.to_string()))?;
        let _state = crate::safepoint::ForeignStateScope::lisp();
        let slots = unsafe { std::slice::from_raw_parts(slots, context.arguments.len()) };
        context.invoke(slots).map_err(CallbackFailure::from_error)
    }));
    let error = match result {
        Ok(Ok(value)) => return value,
        Ok(Err(error)) => error,
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic
                        .downcast_ref::<&str>()
                        .map(|message| (*message).to_owned())
                })
                .unwrap_or_else(|| "Rust panic in Lisp callback".into());
            // panic_any permits arbitrary destructors. Reclaim the payload,
            // but contain a destructor's own panic before returning through C.
            if let Err(secondary) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(panic)))
            {
                // Its destructor may panic again indefinitely. Retaining this
                // exceptional secondary payload is preferable to aborting at
                // the C boundary; ordinary payloads are always reclaimed.
                std::mem::forget(secondary);
            }
            CallbackFailure::message(message)
        }
    };
    context.record_error(error);
    // A defined zero C result lets foreign frames return normally. The enclosing
    // outbound call then signals FFI-ERROR; foreign threads use take_error().
    0
}

/// An explicitly retained closure root and distinct C entry. The caller must
/// retire all foreign references and active invocations before dropping it.
pub struct LispCallback {
    adapter: CallbackAdapter,
    context: Box<Context>,
}

impl LispCallback {
    pub fn new(
        closure: EgclVal,
        result: AlienType,
        arguments: Vec<AlienType>,
    ) -> Result<Self, EgclError> {
        let mut context = Box::new(Context {
            closure: CrossThreadRoot::new(closure),
            result,
            arguments,
            error: Mutex::new(None),
            active: AtomicUsize::new(0),
        });
        let address = (&mut *context as *mut Context).cast();
        let adapter = CallbackAdapter::new(&context.result, &context.arguments, address, dispatch)?;
        Ok(Self { adapter, context })
    }

    pub fn as_fn_ptr(&self) -> *const () {
        self.adapter.as_fn_ptr()
    }

    /// Reject reentrant release. Foreign callers must still stop publishing or
    /// entering the C pointer before release; this is not a reclamation barrier.
    pub fn is_active(&self) -> bool {
        self.context.active.load(Ordering::SeqCst) != 0
    }

    /// Consume the most recent failure, including calls made without an active
    /// Lisp-to-C frame on this thread. Diagnostic text owns no Lisp pointers.
    pub fn take_error(&self) -> Option<String> {
        self.context
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}
