//! The `PY` package's primitives: the thin layer between Lisp forms and
//! `torcl_rt::python` (bliss-dk3nr).
//!
//! Thin on purpose. The object model, the value policy and the reference
//! discipline all live in the runtime (`crates/torcl-rt/src/python.rs`), because
//! they are runtime concerns and because the interpreter must not become a second
//! place where any of it is decided. What is here is argument shuffling and the
//! two things that genuinely belong at the boundary: entering a crossing, and
//! draining the pending releases while inside one.
//!
//! Every entry point takes the same shape — cross, do the work, drain, leave — so
//! a release queued by a collection is never left waiting for a crossing that
//! happens to call `drain` explicitly.
//!
//! ## Without the `python` feature
//!
//! Each primitive reports that embedding was not compiled in. Silence, or a
//! NIL-returning stub, would turn a build-configuration problem into a debugging
//! session at the Lisp level.

use torcl_rt::error::TorclError;
use torcl_rt::value::TorclVal;

#[cfg(not(feature = "python"))]
fn unavailable() -> TorclError {
    TorclError::FfiError(
        "this TorCL was built without embedded CPython: rebuild with the `python` \
         feature, on a target whose loader can open libpython (glibc, not the \
         default static musl)"
            .into(),
    )
}

#[cfg(feature = "python")]
mod enabled {
    use super::*;
    use torcl_rt::python::{self, PyRef, PythonScope};

    /// Enter Python, do the work, then release whatever the collector has queued.
    ///
    /// The drain is here rather than at each call site so that it cannot be
    /// forgotten: a proxy that died during a collection has a reference waiting,
    /// and a crossing is the only context in which paying it is safe.
    ///
    /// Everything coming back goes through `from_python`, with no exceptions — so
    /// `(py:resolve "math.pi")` is a DOUBLE-FLOAT and `(py:resolve "sys.maxsize")`
    /// an integer, exactly as the same values would be if they came back from
    /// `py:call`. One value policy applied everywhere is worth more than letting a
    /// couple of entry points hand back proxies for things that have Lisp
    /// equivalents.
    ///
    /// Every operation also takes its receiver through `to_python` rather than
    /// demanding a proxy. A proxy passes through unchanged and anything else
    /// converts — so `(py:str 5)` is "5", `(py:call-method "hello" "upper")` is
    /// "HELLO", and `(py:typep 5 "builtins.int")` is true. Requiring a proxy made
    /// the surface sharp for no reason a caller could see: a value that had just
    /// crossed BACK as a Lisp string, which the policy says it must, was then a
    /// type error.
    ///
    /// GC-SAFETY. The Lisp values a caller passes in are read BEFORE `body`
    /// allocates anything on the Lisp heap — `to_python` copies out of them and
    /// never allocates — and nothing reads them afterwards. The one allocating step
    /// is converting the result, by which point no Lisp value from the argument
    /// list is live. So there is nothing here to root.
    fn crossing<T>(
        body: impl FnOnce(&PythonScope) -> Result<T, TorclError>,
    ) -> Result<T, TorclError> {
        // Starting the interpreter lazily is safe because `initialize` is
        // idempotent and records its owner: a later shutdown from any other thread
        // is refused rather than dereferencing a thread state that may be gone.
        python::initialize()?;
        let scope = PythonScope::enter()?;
        let result = body(&scope);
        // Drained even when the body failed: the queue is not part of the failure.
        let _ = python::drain_releases(&scope);
        result
    }

    /// A callable named either by a dotted Python name or by an existing proxy.
    fn callable(scope: &PythonScope, designator: TorclVal) -> Result<PyRef, TorclError> {
        if designator.is_string() {
            scope.resolve(&designator.as_string())
        } else {
            scope.unwrap_proxy(designator)
        }
    }

    /// A `&rest` argument list as Python objects.
    ///
    /// No Lisp allocation happens in here, which is what makes reading the caller's
    /// unrooted list safe.
    fn arguments(scope: &PythonScope, list: TorclVal) -> Result<Vec<PyRef>, TorclError> {
        let mut converted = Vec::new();
        let mut rest = list;
        while rest.is_cons() {
            // SAFETY: a cons tag guarantees two readable words at the body.
            let (car, cdr) = unsafe {
                let body = rest.as_ptr() as *const TorclVal;
                (body.read(), body.add(1).read())
            };
            converted.push(scope.to_python(car)?);
            rest = cdr;
        }
        Ok(converted)
    }

    pub fn import(name: TorclVal) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        crossing(|scope| {
            let module = scope.import(&name)?;
            scope.from_python(module)
        })
    }

    pub fn exec(source: TorclVal) -> Result<TorclVal, TorclError> {
        let source = source.as_string();
        crossing(|scope| {
            scope.run(&source)?;
            Ok(torcl_rt::value::NIL)
        })
    }

    pub fn resolve(name: TorclVal) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        crossing(|scope| {
            let object = scope.resolve(&name)?;
            scope.from_python(object)
        })
    }

    pub fn call(designator: TorclVal, args: TorclVal) -> Result<TorclVal, TorclError> {
        crossing(|scope| {
            let target = callable(scope, designator)?;
            let arguments = arguments(scope, args)?;
            let result = scope.call(&target, arguments)?;
            scope.from_python(result)
        })
    }

    pub fn call_method(
        object: TorclVal,
        name: TorclVal,
        args: TorclVal,
    ) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            let arguments = arguments(scope, args)?;
            let result = scope.call_method(&receiver, &name, arguments)?;
            scope.from_python(result)
        })
    }

    pub fn getattr(object: TorclVal, name: TorclVal) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            let attribute = scope.getattr(&receiver, &name)?;
            scope.from_python(attribute)
        })
    }

    pub fn setattr(
        object: TorclVal,
        name: TorclVal,
        value: TorclVal,
    ) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            let value = scope.to_python(value)?;
            scope.setattr(&receiver, &name, &value)?;
            // SETF's value, so `(setf (py:getattr x "n") v)` yields v.
            scope.from_python(value)
        })
    }

    pub fn type_of(object: TorclVal) -> Result<TorclVal, TorclError> {
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            let kind = scope.type_of(&receiver)?;
            scope.from_python(kind)
        })
    }

    pub fn typep(object: TorclVal, class: TorclVal) -> Result<TorclVal, TorclError> {
        let class = class.as_string();
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            Ok(if scope.is_instance(&receiver, &class)? {
                torcl_rt::value::T
            } else {
                torcl_rt::value::NIL
            })
        })
    }

    pub fn text(object: TorclVal, escaped: bool) -> Result<TorclVal, TorclError> {
        crossing(|scope| {
            let receiver = scope.to_python(object)?;
            let rendered = if escaped {
                scope.represent(&receiver)?
            } else {
                scope.display(&receiver)?
            };
            Ok(torcl_rt::gc::alloc_character_string(&rendered))
        })
    }

    /// Everything Python has written since the last drain, as `(stdout . stderr)`,
    /// or NIL when both are empty.
    ///
    /// Returned rather than written here: only Lisp knows what `*standard-output*`
    /// currently is — a `WITH-OUTPUT-TO-STRING` may be in force — so the write
    /// belongs in `lib/boot.lisp`, and this is the part that needs a crossing.
    pub fn drain_output() -> Result<TorclVal, TorclError> {
        let (output, error) = crossing(|scope| scope.take_output())?;
        if output.is_empty() && error.is_empty() {
            return Ok(torcl_rt::value::NIL);
        }
        // Both strings rooted before the cons: building the pair allocates, and the
        // first would otherwise be a bare local across it.
        torcl_rt::rooted!(output = torcl_rt::gc::alloc_character_string(&output));
        torcl_rt::rooted!(error = torcl_rt::gc::alloc_character_string(&error));
        Ok(crate::cli::arena_cons(*output, *error))
    }

    /// Make a Lisp function callable from Python as `name` in `__main__`.
    pub fn export(name: TorclVal, function: TorclVal) -> Result<TorclVal, TorclError> {
        let name = name.as_string();
        install_export_dispatch();
        crossing(|scope| {
            scope.export(&name, function)?;
            Ok(torcl_rt::value::NIL)
        })
    }

    /// Teach the runtime how to call a Lisp function, once.
    ///
    /// The runtime owns the C entry point and the handle table but cannot evaluate
    /// Lisp, which lives up here — the same split the GC's finalizer dispatch uses.
    fn install_export_dispatch() {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| torcl_rt::python::set_export_dispatch(dispatch_to_lisp));
    }

    /// Call the Lisp function a handle names. Runs on a thread CPython owns, already
    /// transitioned to managed state by the runtime's C entry point.
    ///
    /// Delegates to the interpreter's existing foreign-callback runner rather than
    /// building an environment here: a call arriving from foreign code needs a fresh
    /// control environment that adopts this thread's definitions, and needs the
    /// caller's nonlocal-exit tokens kept while this callback's are discarded —
    /// subtleties already worked out there for FFI callbacks, and not worth a second
    /// implementation.
    fn dispatch_to_lisp(handle: u64, args: &[TorclVal]) -> Result<TorclVal, TorclError> {
        let handle = torcl_rt::python::LispHandle::from_bits(handle);
        let function = torcl_rt::python::resolve_lisp(handle).ok_or_else(|| {
            TorclError::FfiError(
                "this exported function's handle no longer names anything: the \
                 interpreter it was exported from has gone away"
                    .into(),
            )
        })?;
        crate::cli::foreign_callback_runner(function, args)
    }

    pub fn stop() -> Result<TorclVal, TorclError> {
        // The last drain before shutdown: a queued reference released after
        // Py_FinalizeEx would be a use-after-free.
        if python::is_initialized() {
            let scope = PythonScope::enter()?;
            let _ = python::drain_releases(&scope);
            drop(scope);
        }
        python::finalize()?;
        Ok(torcl_rt::value::T)
    }

    /// How a proxy prints: `#<PYTHON-OBJECT repr>`, so the reader of a backtrace
    /// sees the object rather than an address.
    ///
    /// Returns `None` when there is nothing to show — not a proxy, no interpreter,
    /// or a `__repr__` that itself raised. A printer must never fail, and must
    /// never start an interpreter as a side effect of printing.
    pub fn describe_proxy(value: TorclVal) -> Option<String> {
        if !python::is_proxy(value) || !python::is_initialized() {
            return None;
        }
        let scope = PythonScope::enter().ok()?;
        let object = scope.unwrap_proxy(value).ok()?;
        scope.represent(&object).ok()
    }
}

#[cfg(feature = "python")]
pub use enabled::*;

#[cfg(not(feature = "python"))]
mod disabled {
    use super::*;

    pub fn import(_: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn exec(_: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn resolve(_: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn call(_: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn call_method(_: TorclVal, _: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn getattr(_: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn setattr(_: TorclVal, _: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn type_of(_: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn typep(_: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn text(_: TorclVal, _: bool) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn drain_output() -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn export(_: TorclVal, _: TorclVal) -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    pub fn stop() -> Result<TorclVal, TorclError> {
        Err(unavailable())
    }

    /// Nothing can be a proxy in a build with no interpreter, so the printer has
    /// nothing to add.
    pub fn describe_proxy(_: TorclVal) -> Option<String> {
        None
    }
}

#[cfg(not(feature = "python"))]
pub use disabled::*;

/// Is this value a Python proxy? Always false without the feature, since the type
/// cannot be constructed.
pub fn is_proxy(value: TorclVal) -> bool {
    #[cfg(feature = "python")]
    {
        torcl_rt::python::is_proxy(value)
    }
    #[cfg(not(feature = "python"))]
    {
        let _ = value;
        false
    }
}

/// Build a `PY:EXCEPTION` condition instance from a Python raise (bliss-wq5tw).
///
/// Lives here rather than in `cli.rs`'s conversion table because assembling it
/// needs to know what a Python frame is, and because the exception object has to
/// become a proxy — which is a crossing, and the one place a *condition* is allowed
/// to perform one.
#[cfg(feature = "python")]
pub fn build_error_condition(
    env: &mut super::Env,
    raise: &torcl_rt::python::Raise,
) -> Result<TorclVal, torcl_rt::error::TorclError> {
    use torcl_rt::gc::alloc_character_string;

    // The frames first, before the condition exists: this allocates a list of
    // lists, and building it while holding a partially-initialised instance would
    // be the classic way to lose it under a moving collector.
    //
    // Each frame is (FILE LINE FUNCTION), outermost first — Python's own order, so
    // a reader who has seen a Python traceback recognises it.
    torcl_rt::rooted!(frames = torcl_rt::value::NIL);
    for frame in raise.frames.iter().rev() {
        torcl_rt::rooted!(file = alloc_character_string(&frame.file));
        torcl_rt::rooted!(function = alloc_character_string(&frame.function));
        torcl_rt::rooted!(
            entry = super::vec_to_list(&[*file, TorclVal::from_fixnum(frame.line), *function,])
        );
        // Built back to front, so the list comes out in the order above.
        *frames = super::arena_cons(*entry, *frames);
    }

    torcl_rt::rooted!(kind = alloc_character_string(&raise.kind));
    torcl_rt::rooted!(text = alloc_character_string(&raise.message));

    // The exception object, as a proxy, so `(py:error-object e)` can be asked for
    // its attributes. A crossing is needed to add the reference the proxy owns; if
    // one cannot be had — the interpreter is going down, say — the condition is
    // still worth having without it.
    torcl_rt::rooted!(object = torcl_rt::value::NIL);
    if let Some(exception) = raise.object.as_ref() {
        if let Ok(scope) = torcl_rt::python::PythonScope::enter() {
            if let Some(owned) = exception.duplicate(&scope) {
                if let Ok(proxy) = scope.proxy(owned) {
                    *object = proxy;
                }
            }
        }
    }

    // The rendered report, which is what TorCL's printer reads (see the slot's
    // comment in boot.lisp). Built here so an uncaught Python error shows its
    // message AND its frames without anything else having to know how.
    torcl_rt::rooted!(
        report = alloc_character_string(&format!(
            "{}: {}{}{}",
            raise.kind,
            raise.message,
            if raise.frames.is_empty() { "" } else { "\n" },
            raise.render_frames().trim_end()
        ))
    );

    super::build_condition_instance(
        env,
        "TORCL-PYTHON::EXCEPTION",
        &[
            super::resolve_sym("FORMAT-CONTROL").unwrap_or(torcl_rt::value::NIL),
            *report,
            super::resolve_sym("KIND").unwrap_or(torcl_rt::value::NIL),
            *kind,
            super::resolve_sym("TEXT").unwrap_or(torcl_rt::value::NIL),
            *text,
            super::resolve_sym("FRAMES").unwrap_or(torcl_rt::value::NIL),
            *frames,
            super::resolve_sym("OBJECT").unwrap_or(torcl_rt::value::NIL),
            *object,
        ],
    )
}
