//! Embedded CPython: initialisation, shutdown, and the Lisp↔Python transition
//! (spec §2.7.8).
//!
//! This is the foundation the rest of the bridge sits on, and it deliberately
//! provides no object model yet — only the ability to start an interpreter, cross
//! into it safely, and stop it.
//!
//! RESOLVED AT RUNTIME, NOT LINKED. CPython is reached through TorCL's own
//! foreign-library loader rather than by linking `libpython`, for two reasons that
//! are both about not making the core binary worse: the default target is a static
//! musl executable that cannot link a shared library at all, and a link-time
//! dependency would make `libpython` mandatory for every build. As a consequence
//! embedding needs a glibc-linked TorCL — `libpython` depends on `libc.so.6` and
//! the glibc loader — which is why this is behind a feature rather than always on.
//!
//! THE TRANSITION IS THE POINT. Every crossing into Python is a managed→foreign
//! transition that publishes Native state first, exactly as any other foreign call
//! does, so a collection can proceed while Python runs. The VM never learns what a
//! GIL is: `PyGILState_Ensure`/`Release` is the supported entry point on a
//! conventional build and remains correct on a free-threaded one, so what a
//! crossing costs can change with the interpreter without the rest of TorCL
//! noticing.

use crate::error::TorclError;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;

/// CPython's `PyGILState_STATE`, opaque to us — it is handed back unchanged.
type GilState = i32;

/// The CPython entry points this layer needs. Resolved once.
struct Api {
    initialize_ex: unsafe extern "C" fn(i32),
    finalize_ex: unsafe extern "C" fn() -> i32,
    is_initialized: unsafe extern "C" fn() -> i32,
    gil_ensure: unsafe extern "C" fn() -> GilState,
    gil_release: unsafe extern "C" fn(GilState),
    /// Releases the GIL and returns the saved thread state. Needed exactly once,
    /// after startup — see [`initialize`].
    eval_save_thread: unsafe extern "C" fn() -> *mut (),
    run_simple_string: unsafe extern "C" fn(*const i8) -> i32,
}

// SAFETY: every field is a code pointer into a library that stays loaded for the
// process lifetime, and CPython's own thread-safety governs what calling them
// concurrently means.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

/// Library names to try, most specific first. `TORCL_PYTHON_LIBRARY` overrides
/// the list entirely, which is how a caller selects a particular interpreter —
/// a virtual environment, a debug build, or a free-threaded one.
const LIBRARY_NAMES: &[&str] = &[
    "libpython3.14.so.1.0",
    "libpython3.13.so.1.0",
    "libpython3.12.so.1.0",
    "libpython3.11.so.1.0",
    "libpython3.so",
];

fn resolve() -> Result<Api, String> {
    let explicit = std::env::var("TORCL_PYTHON_LIBRARY").ok();
    let names: Vec<&str> = match explicit.as_deref() {
        Some(name) => vec![name],
        None => LIBRARY_NAMES.to_vec(),
    };
    let mut attempts = Vec::new();
    for name in names {
        match crate::ffi::load_foreign_library(name) {
            Ok(library) => return bind(library, name),
            Err(error) => attempts.push(format!("{name}: {error}")),
        }
    }
    Err(format!(
        "no CPython library could be loaded (tried {}). Set TORCL_PYTHON_LIBRARY \
         to choose one explicitly.",
        attempts.join("; ")
    ))
}

fn bind(library: *mut (), name: &str) -> Result<Api, String> {
    /// Resolve one symbol, reporting which library was missing it rather than
    /// just that something failed.
    macro_rules! symbol {
        ($name:literal) => {{
            // SAFETY: the handle came from a successful load above, and each
            // signature below is CPython's documented one.
            let found = unsafe { crate::ffi::foreign_symbol(library, $name) }
                .map_err(|error| format!("{name} has no {}: {error}", $name))?;
            // SAFETY: transmuting a resolved code address to its declared
            // signature, which is the whole purpose of resolving it.
            unsafe { std::mem::transmute(found) }
        }};
    }
    Ok(Api {
        initialize_ex: symbol!("Py_InitializeEx"),
        finalize_ex: symbol!("Py_FinalizeEx"),
        is_initialized: symbol!("Py_IsInitialized"),
        gil_ensure: symbol!("PyGILState_Ensure"),
        gil_release: symbol!("PyGILState_Release"),
        eval_save_thread: symbol!("PyEval_SaveThread"),
        run_simple_string: symbol!("PyRun_SimpleString"),
    })
}

fn api() -> Result<&'static Api, TorclError> {
    API.get_or_init(resolve)
        .as_ref()
        .map_err(|error| TorclError::FfiError(error.clone()))
}

/// True if an interpreter is running.
pub fn is_initialized() -> bool {
    match api() {
        // SAFETY: a resolved CPython entry point taking no arguments.
        Ok(api) => (unsafe { (api.is_initialized)() }) != 0,
        Err(_) => false,
    }
}

/// Serialises interpreter startup and shutdown.
///
/// An embedded CPython is a PROCESS singleton, and `Py_IsInitialized` followed by
/// `Py_InitializeEx` is a check-then-act race: two threads crossing into Python
/// for the first time at once both see "not initialized" and both initialise,
/// which CPython reports as
/// "Fatal Python error: _PyImport_Init: global import state already initialized"
/// and then aborts the process. The lock, not the check, is what makes
/// [`initialize`] idempotent.
static LIFECYCLE: Mutex<()> = Mutex::new(());

/// The thread that started the interpreter, which is the only one that may stop it.
///
/// CPython ties the interpreter's main thread state to whichever thread called
/// `Py_InitializeEx`, and `Py_FinalizeEx` expects to be that thread. If the
/// initialising thread has exited, shutdown does not fail — it SEGFAULTS. That is
/// why startup is explicit rather than performed by whichever thread happens to
/// cross into Python first: one spawned thread initialising and then exiting is
/// enough to make a later `finalize` from the main thread crash the process.
static OWNER: Mutex<Option<ThreadId>> = Mutex::new(None);

/// Start the embedded interpreter, or do nothing if one is already running.
///
/// Signal handlers are DELIBERATELY not installed (`Py_InitializeEx(0)`). TorCL is
/// the process-level signal authority (spec §2.7.8, §2.6): two runtimes must not
/// both believe they own SIGINT, and TorCL additionally uses SIGSEGV for
/// native-frame recovery, so letting CPython install its own handlers would be
/// worse than merely untidy.
pub fn initialize() -> Result<(), TorclError> {
    let api = api()?;
    let _lifecycle = LIFECYCLE
        .lock()
        .map_err(|_| TorclError::FfiError("the CPython lifecycle lock is poisoned".into()))?;
    let mut owner = OWNER
        .lock()
        .map_err(|_| TorclError::FfiError("the CPython owner lock is poisoned".into()))?;
    // SAFETY: resolved entry points, and the lock makes the check-and-start atomic.
    unsafe {
        if (api.is_initialized)() == 0 {
            // Publish Native state around startup: it is long, it allocates, and
            // it must not hold off a collection.
            let _foreign = crate::safepoint::ForeignStateScope::native();
            (api.initialize_ex)(0);
            // Py_InitializeEx RETURNS WITH THE GIL HELD by this thread, and
            // nothing later releases it: PyGILState_Ensure on the same thread sees
            // it as already held, so the matching Release does not drop it either.
            // Every other thread then blocks on its first crossing forever — which
            // a single-threaded test cannot show, because the one thread keeps
            // re-entering a GIL it already owns. Hand it back here, which is what
            // an embedder is expected to do once startup is complete.
            let _ = (api.eval_save_thread)();
            *owner = Some(std::thread::current().id());
        }
    }
    Ok(())
}

/// Stop the embedded interpreter. Returns an error if CPython reports that
/// shutdown did not complete cleanly.
///
/// Restarting after this is NOT supported: CPython permits re-initialisation only
/// with caveats that extension modules routinely violate, so this is an orderly
/// process shutdown rather than a reusable off switch.
pub fn finalize() -> Result<(), TorclError> {
    let api = api()?;
    let _lifecycle = LIFECYCLE
        .lock()
        .map_err(|_| TorclError::FfiError("the CPython lifecycle lock is poisoned".into()))?;
    let mut owner = OWNER
        .lock()
        .map_err(|_| TorclError::FfiError("the CPython owner lock is poisoned".into()))?;
    // SAFETY: resolved entry points, serialised against startup.
    unsafe {
        if (api.is_initialized)() == 0 {
            return Ok(());
        }
        // Refuse rather than crash. CPython does not check this and simply
        // dereferences a thread state that may belong to an exited thread.
        if *owner != Some(std::thread::current().id()) {
            return Err(TorclError::FfiError(
                "CPython must be shut down by the thread that started it".into(),
            ));
        }
        let status = {
            let _foreign = crate::safepoint::ForeignStateScope::native();
            (api.finalize_ex)()
        };
        if status < 0 {
            return Err(TorclError::FfiError(
                "CPython shutdown reported an error".into(),
            ));
        }
        *owner = None;
    }
    Ok(())
}

/// A crossing into Python: the whole of the ENTER_PYTHON / LEAVE_PYTHON
/// abstraction, and the only sanctioned way to call CPython.
///
/// Holding one means this thread may use the CPython C API. Dropping it returns
/// the thread to Lisp execution.
///
/// FIELD ORDER IS LOAD-BEARING. Rust drops fields in declaration order, so the GIL
/// is released before the thread leaves Native state — the reverse of the order it
/// was acquired in, and the only correct one. Entering the other way round would
/// block on the GIL while still claiming to be running Lisp, which stalls every
/// collection for as long as some other thread holds it.
pub struct PythonScope {
    gil: GilState,
    api: &'static Api,
    /// Declared last so it is dropped last: the thread stays in Native state until
    /// after the GIL is released.
    _foreign: crate::safepoint::ForeignStateScope,
}

impl PythonScope {
    /// Enter Python on this thread.
    ///
    /// Requires an interpreter already started by [`initialize`], and does NOT
    /// start one itself. That is deliberate: CPython binds the interpreter to its
    /// initialising thread, so starting it from whichever thread happened to cross
    /// first means shutdown either crashes (if that thread has exited) or becomes
    /// impossible (if it is still alive but never calls it). Startup is an
    /// embedder's decision about process lifetime, not a side effect of a call.
    pub fn enter() -> Result<Self, TorclError> {
        let api = api()?;
        // SAFETY: a resolved entry point taking no arguments.
        if unsafe { (api.is_initialized)() } == 0 {
            return Err(TorclError::FfiError(
                "no Python interpreter is running: call torcl_rt::python::initialize() \
                 once, from the thread that will also shut it down"
                    .into(),
            ));
        }
        // Publish Native state BEFORE blocking for the GIL, so waiting for another
        // thread's Python work cannot hold off a collection.
        let foreign = crate::safepoint::ForeignStateScope::native();
        // SAFETY: a resolved entry point; the matching release happens in Drop.
        let gil = unsafe { (api.gil_ensure)() };
        Ok(Self {
            gil,
            api,
            _foreign: foreign,
        })
    }

    /// Run a statement for its effect, as `python -c` would.
    ///
    /// Present so this layer can be tested end to end before any object model
    /// exists; it reports only success or failure, because turning a Python
    /// exception into a Lisp condition is a separate concern (bliss-wq5tw).
    pub fn run(&self, source: &str) -> Result<(), TorclError> {
        let source = CString::new(source)
            .map_err(|_| TorclError::FfiError("Python source contains a null byte".into()))?;
        // SAFETY: a resolved entry point, a valid null-terminated string, and this
        // scope proves the caller may use the C API.
        let status = unsafe { (self.api.run_simple_string)(source.as_ptr()) };
        if status != 0 {
            return Err(TorclError::FfiError(
                "Python raised while executing a statement".into(),
            ));
        }
        Ok(())
    }
}

impl Drop for PythonScope {
    fn drop(&mut self) {
        // SAFETY: releasing the state this scope acquired, exactly once.
        unsafe { (self.api.gil_release)(self.gil) };
    }
}
