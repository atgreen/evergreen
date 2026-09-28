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
    inc_ref: unsafe extern "C" fn(*mut PyObject),
    dec_ref: unsafe extern "C" fn(*mut PyObject),
    run_simple_string: unsafe extern "C" fn(*const i8) -> i32,
    /// `__main__`'s module object, and its dictionary. Both borrowed, both needed
    /// to reach a name Python has bound — see [`PythonScope::lookup`].
    import_add_module: unsafe extern "C" fn(*const i8) -> *mut PyObject,
    module_get_dict: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    dict_get_item_string: unsafe extern "C" fn(*mut PyObject, *const i8) -> *mut PyObject,

    // ── the object model (§the object model, below) ──
    import_module: unsafe extern "C" fn(*const i8) -> *mut PyObject,
    object_get_attr_string: unsafe extern "C" fn(*mut PyObject, *const i8) -> *mut PyObject,
    object_set_attr_string: unsafe extern "C" fn(*mut PyObject, *const i8, *mut PyObject) -> i32,
    object_call:
        unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
    object_str: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    object_repr: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    object_type: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    object_is_instance: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> i32,
    object_is_true: unsafe extern "C" fn(*mut PyObject) -> i32,
    tuple_new: unsafe extern "C" fn(isize) -> *mut PyObject,
    /// STEALS the reference it is given. See [`PythonScope::call`].
    tuple_set_item: unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> i32,
    long_from_longlong: unsafe extern "C" fn(i64) -> *mut PyObject,
    /// Reports "too large for the C type" through the out-parameter rather than by
    /// setting an exception, which is why this rather than `PyLong_AsLongLong`.
    long_as_longlong_and_overflow: unsafe extern "C" fn(*mut PyObject, *mut i32) -> i64,
    float_from_double: unsafe extern "C" fn(f64) -> *mut PyObject,
    float_as_double: unsafe extern "C" fn(*mut PyObject) -> f64,
    bool_from_long: unsafe extern "C" fn(i64) -> *mut PyObject,
    unicode_from_string_and_size: unsafe extern "C" fn(*const i8, isize) -> *mut PyObject,
    /// Returns a buffer BORROWED from the object, valid only while it lives.
    unicode_as_utf8_and_size: unsafe extern "C" fn(*mut PyObject, *mut isize) -> *const i8,

    // ── errors ──
    err_occurred: unsafe extern "C" fn() -> *mut PyObject,
    err_clear: unsafe extern "C" fn(),
    /// 3.12 and later: the raised exception, normalized, as one owned reference.
    /// `None` on 3.11, which has only the three-part form below.
    err_get_raised_exception: Option<unsafe extern "C" fn() -> *mut PyObject>,
    err_fetch:
        unsafe extern "C" fn(*mut *mut PyObject, *mut *mut PyObject, *mut *mut PyObject),
    sequence_size: unsafe extern "C" fn(*mut PyObject) -> isize,
    sequence_get_item: unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,

    // ── singletons and types, which are DATA symbols ──
    //
    // `Py_None` and `PyLong_Type` are macros for the addresses of static structs,
    // so what is resolved here is the struct itself and its address IS the object.
    none: *mut PyObject,
    long_type: *mut PyObject,
    float_type: *mut PyObject,
    unicode_type: *mut PyObject,
    bool_type: *mut PyObject,
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
            // signature, which is the whole purpose of resolving it. The
            // destination is left to inference on purpose: the `Api` field being
            // initialised is the single authority for each signature, and spelling
            // it again here would be a second copy that could drift from CPython's
            // documented prototype independently.
            #[allow(
                clippy::missing_transmute_annotations,
                reason = "the destination is the Api field's declared signature"
            )]
            unsafe {
                std::mem::transmute(found)
            }
        }};
    }
    /// Resolve a symbol that need not exist, for an entry point that arrived in a
    /// particular CPython version. A missing one is a `None` to fall back from,
    /// not a failure to load the library.
    macro_rules! optional {
        ($name:literal) => {{
            // SAFETY: as `symbol!`, for a symbol that may legitimately be absent.
            unsafe { crate::ffi::foreign_symbol(library, $name) }
                .ok()
                // SAFETY: transmuting a resolved code address to the Option's
                // declared signature.
                .map(|found| unsafe { std::mem::transmute(found) })
        }};
    }
    /// Resolve a DATA symbol, whose address is the value rather than something to
    /// call. `Py_None` and the type objects are macros for exactly this.
    macro_rules! data {
        ($name:literal) => {{
            // SAFETY: the handle came from a successful load above.
            unsafe { crate::ffi::foreign_symbol(library, $name) }
                .map_err(|error| format!("{name} has no {}: {error}", $name))?
                as *mut PyObject
        }};
    }
    Ok(Api {
        initialize_ex: symbol!("Py_InitializeEx"),
        finalize_ex: symbol!("Py_FinalizeEx"),
        is_initialized: symbol!("Py_IsInitialized"),
        gil_ensure: symbol!("PyGILState_Ensure"),
        gil_release: symbol!("PyGILState_Release"),
        eval_save_thread: symbol!("PyEval_SaveThread"),
        inc_ref: symbol!("Py_IncRef"),
        dec_ref: symbol!("Py_DecRef"),
        run_simple_string: symbol!("PyRun_SimpleString"),
        import_add_module: symbol!("PyImport_AddModule"),
        module_get_dict: symbol!("PyModule_GetDict"),
        dict_get_item_string: symbol!("PyDict_GetItemString"),
        import_module: symbol!("PyImport_ImportModule"),
        object_get_attr_string: symbol!("PyObject_GetAttrString"),
        object_set_attr_string: symbol!("PyObject_SetAttrString"),
        object_call: symbol!("PyObject_Call"),
        object_str: symbol!("PyObject_Str"),
        object_repr: symbol!("PyObject_Repr"),
        object_type: symbol!("PyObject_Type"),
        object_is_instance: symbol!("PyObject_IsInstance"),
        object_is_true: symbol!("PyObject_IsTrue"),
        tuple_new: symbol!("PyTuple_New"),
        tuple_set_item: symbol!("PyTuple_SetItem"),
        long_from_longlong: symbol!("PyLong_FromLongLong"),
        long_as_longlong_and_overflow: symbol!("PyLong_AsLongLongAndOverflow"),
        float_from_double: symbol!("PyFloat_FromDouble"),
        float_as_double: symbol!("PyFloat_AsDouble"),
        bool_from_long: symbol!("PyBool_FromLong"),
        unicode_from_string_and_size: symbol!("PyUnicode_FromStringAndSize"),
        unicode_as_utf8_and_size: symbol!("PyUnicode_AsUTF8AndSize"),
        err_occurred: symbol!("PyErr_Occurred"),
        err_clear: symbol!("PyErr_Clear"),
        err_get_raised_exception: optional!("PyErr_GetRaisedException"),
        err_fetch: symbol!("PyErr_Fetch"),
        sequence_size: symbol!("PySequence_Size"),
        sequence_get_item: symbol!("PySequence_GetItem"),
        none: data!("_Py_NoneStruct"),
        long_type: data!("PyLong_Type"),
        float_type: data!("PyFloat_Type"),
        unicode_type: data!("PyUnicode_Type"),
        bool_type: data!("PyBool_Type"),
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
    /// after the GIL is released, and native fault recovery stays disarmed until
    /// after that.
    _foreign: crate::safepoint::ForeignStateScope,
    _recovery: RecoveryDisarmed,
}

/// Disarms this thread's native SIGSEGV recovery for as long as it lives
/// (bliss-ztkuw).
///
/// TorCL redirects a faulting instruction to a native recovery epilogue that
/// unwinds a JIT frame and returns a sentinel, and it decides to do so from the
/// FAULT ADDRESS ALONE — any address below one page is "a null guard". Nothing looks
/// at where the fault happened. So a null dereference inside CPython, in a C
/// extension, or in libffi is indistinguishable from one in compiled Lisp, and with
/// recovery armed it would be rewritten to unwind a Lisp frame that is not on top:
/// CPython would be left mid-operation with its reference counts wrong and the
/// process would carry on as though a Lisp type error had occurred.
///
/// This is the same rule as the c2i boundary's recovery toggle — recovery must be
/// off while a foreign frame is on top — applied at the transition instead of at
/// each caller. Doing it here is what makes the property local: it holds however
/// the crossing was reached, including from a future caller that does not know the
/// rule exists.
///
/// A fault inside CPython therefore reaches the default path and kills the process,
/// which is what CPython itself would do; it is not silently converted into a
/// catchable Lisp condition.
struct RecoveryDisarmed {
    null_ip: usize,
    stack_ip: usize,
}

impl RecoveryDisarmed {
    fn enter() -> Self {
        let saved = Self {
            null_ip: crate::runtime::current_sigsegv_null_guard_recovery_ip(),
            stack_ip: crate::runtime::current_sigsegv_stack_guard_recovery_ip(),
        };
        crate::runtime::set_sigsegv_recovery_ips(0, 0);
        saved
    }
}

impl Drop for RecoveryDisarmed {
    fn drop(&mut self) {
        crate::runtime::set_sigsegv_recovery_ips(self.null_ip, self.stack_ip);
    }
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
            // After the GIL, so a fault while WAITING for it is still ours: at that
            // point this thread is not yet running Python code.
            _recovery: RecoveryDisarmed::enter(),
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

    /// An owned reference to whatever `__main__` has bound to `name`, or `None` if
    /// it is unbound.
    ///
    /// A deliberately narrow way to reach a Python object from Rust: enough to own
    /// and release references, which is what this layer is about, without yet
    /// committing to a calling convention (bliss-dk3nr).
    pub fn lookup(&self, name: &str) -> Option<PyRef> {
        let name = CString::new(name).ok()?;
        let main = CString::new("__main__").ok()?;
        // SAFETY: resolved entry points, and `self` proves the GIL is held. Every
        // pointer here is borrowed, so nothing is released on the way out and the
        // final value is adopted by adding a reference rather than stealing one.
        unsafe {
            let module = (self.api.import_add_module)(main.as_ptr());
            if module.is_null() {
                return None;
            }
            let dict = (self.api.module_get_dict)(module);
            if dict.is_null() {
                return None;
            }
            PyRef::from_borrowed((self.api.dict_get_item_string)(dict, name.as_ptr()), self)
        }
    }
}

impl Drop for PythonScope {
    fn drop(&mut self) {
        // SAFETY: releasing the state this scope acquired, exactly once.
        unsafe { (self.api.gil_release)(self.gil) };
    }
}

// ── Ownership across two collectors ────────────────────────────────
//
// This is the part where TorCL's collector and CPython's differ most, and where
// the design note's instincts are load-bearing rather than stylistic.
//
// Lisp holding Python is reference counting: a proxy owns a reference, and the
// matching release must NOT run from a Lisp GC or finalizer context, because
// `Py_DECREF` can run arbitrary Python — `__del__` — and re-enter CPython at a
// point where no safe state has been published. So releases are QUEUED and drained
// at a crossing (see [`PyRef`], [`drain_releases`]).
//
// Python holding Lisp is the mirror problem, and worse: TorCL's collector MOVES
// objects, so a raw address handed to Python goes stale at the next collection
// with no warning. A [`LispHandle`] is an indirection through a table the collector
// visits, so Python holds a number that stays valid while the value beneath it is
// relocated.

/// An opaque CPython object. Never dereferenced on this side.
#[repr(C)]
pub struct PyObject {
    _opaque: [u8; 0],
}

/// The pending `Py_DECREF` queue.
///
/// `GcWorld` level because it is taken from a `Drop` that may run anywhere,
/// including while the collector is working; nothing here allocates on the Lisp
/// heap, so it cannot itself provoke a collection.
fn releases() -> &'static crate::lock_order::OrderedMutex<Vec<usize>> {
    static QUEUE: OnceLock<crate::lock_order::OrderedMutex<Vec<usize>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        crate::lock_order::OrderedMutex::new(
            crate::lock_order::LockLevel::GcWorld,
            20,
            "CPython release queue",
            Vec::new(),
        )
    })
}

/// An owned reference to a Python object.
///
/// Dropping one does not call CPython. It appends to the release queue, which is
/// drained the next time a thread crosses into Python — so a proxy becoming
/// unreachable during a collection cannot run `__del__` inside the collector.
pub struct PyRef {
    pointer: *mut PyObject,
}

// SAFETY: the pointer is only dereferenced by CPython, under the GIL, through a
// `PythonScope`; this type itself only stores and enqueues it.
unsafe impl Send for PyRef {}
unsafe impl Sync for PyRef {}

impl PyRef {
    /// Take ownership of a NEW reference, which is what most C-API functions
    /// return. `None` for a null pointer, which is how CPython reports failure.
    ///
    /// # Safety
    /// `pointer` must be a new reference that this `PyRef` may own, or null.
    pub unsafe fn from_owned(pointer: *mut PyObject) -> Option<Self> {
        if pointer.is_null() {
            None
        } else {
            Some(Self { pointer })
        }
    }

    /// Add a reference to a BORROWED pointer, which is what container accessors
    /// such as `PyList_GetItem` return. Requires a crossing, because this calls
    /// `Py_IncRef`.
    ///
    /// # Safety
    /// `pointer` must be a valid borrowed reference, or null.
    pub unsafe fn from_borrowed(pointer: *mut PyObject, _scope: &PythonScope) -> Option<Self> {
        if pointer.is_null() {
            return None;
        }
        let api = api().ok()?;
        // SAFETY: a resolved entry point, a valid pointer, and the scope proves the
        // caller holds the GIL.
        unsafe { (api.inc_ref)(pointer) };
        Some(Self { pointer })
    }

    /// The borrowed pointer, for handing to the C API. Valid while `self` lives.
    pub fn as_ptr(&self) -> *mut PyObject {
        self.pointer
    }

    /// A second owned reference to the same object. Not `Clone`, because adding a
    /// reference calls into CPython and therefore needs a crossing — a trait
    /// implementation would have to hide that or lie about it.
    pub fn duplicate(&self, scope: &PythonScope) -> Option<Self> {
        // SAFETY: `self.pointer` is a live reference this value owns.
        unsafe { Self::from_borrowed(self.pointer, scope) }
    }
}

impl std::fmt::Debug for PyRef {
    /// The address, not the object. Formatting a Python object means calling
    /// `repr`, which needs the GIL, and `Debug` has no way to prove a crossing is
    /// in force — see [`PythonScope::represent`] for the useful form.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PyRef({:p})", self.pointer)
    }
}

impl Drop for PyRef {
    fn drop(&mut self) {
        // Deliberately does NOT call Py_DecRef. See the module note: this may run
        // from a collection, and a decref can run arbitrary Python.
        if let Ok(mut queue) = releases().lock() {
            queue.push(self.pointer as usize);
        }
    }
}

/// Release every reference queued by a dropped [`PyRef`], returning how many.
///
/// Called at a crossing, where the GIL is held and no collection is in progress.
/// Taking the queue before releasing anything matters: a `__del__` can drop further
/// proxies, and those must land in the next batch rather than mutate the vector
/// being iterated.
pub fn drain_releases(_scope: &PythonScope) -> Result<usize, TorclError> {
    let api = api()?;
    let pending: Vec<usize> = {
        let mut queue = releases()
            .lock()
            .map_err(|_| TorclError::FfiError("the CPython release queue is poisoned".into()))?;
        std::mem::take(&mut *queue)
    };
    for pointer in &pending {
        // SAFETY: each was an owned reference held by a PyRef that has been
        // dropped, so this consumes exactly the count that PyRef held, and the
        // scope proves the GIL is held.
        unsafe { (api.dec_ref)(*pointer as *mut PyObject) };
    }
    Ok(pending.len())
}

/// How many releases are waiting. For tests and diagnostics.
pub fn pending_releases() -> usize {
    releases().lock().map(|queue| queue.len()).unwrap_or(0)
}

// ── Stable handles to Lisp values ──────────────────────────────────

/// A stable name for a Lisp value that Python may hold.
///
/// Not a pointer. TorCL's collector relocates objects, so an address handed across
/// the boundary is valid only until the next collection — and nothing on the Python
/// side would notice it had gone stale. A handle is an index plus a generation, and
/// the value beneath it is visited by the collector, so it is rewritten rather than
/// invalidated when the object moves.
///
/// The generation is what makes a stale handle *detectable*: releasing a handle and
/// allocating another reuses the slot, and without a generation the old handle would
/// silently resolve to the new value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LispHandle {
    slot: u32,
    generation: u32,
}

impl LispHandle {
    /// The handle as one opaque word, which is what actually crosses into Python.
    pub fn to_bits(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.slot)
    }

    /// Recover a handle from its word form.
    pub fn from_bits(bits: u64) -> Self {
        Self {
            slot: bits as u32,
            generation: (bits >> 32) as u32,
        }
    }
}

enum Slot {
    /// Free, holding the generation the next occupant will use.
    Free { generation: u32 },
    /// Occupied by a value the collector keeps alive and rewrites in place.
    Strong {
        generation: u32,
        value: crate::value::TorclVal,
    },
}

/// The handle table. `GcWorld` level because the collector's root scanner takes it;
/// nothing under this lock allocates on the Lisp heap, so holding it cannot provoke
/// the collection that would deadlock against the scanner.
fn handles() -> &'static crate::lock_order::OrderedMutex<Vec<Slot>> {
    static TABLE: OnceLock<crate::lock_order::OrderedMutex<Vec<Slot>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        crate::lock_order::OrderedMutex::new(
            crate::lock_order::LockLevel::GcWorld,
            21,
            "CPython Lisp-handle table",
            Vec::new(),
        )
    })
}

/// Visit every strongly-held value so the collector keeps it alive and rewrites it
/// when the object moves. Registered once, on the first `retain`.
fn scan_lisp_handles(visit: &mut dyn FnMut(*mut crate::value::TorclVal)) {
    let mut table = match handles().lock() {
        Ok(table) => table,
        // A poisoned table must not silently drop roots: without the values the
        // collector would free objects Python still refers to.
        Err(error) => error.into_inner(),
    };
    for slot in table.iter_mut() {
        if let Slot::Strong { value, .. } = slot {
            visit(value as *mut crate::value::TorclVal);
        }
    }
}

/// Hold `value` alive and hand back a stable name for it.
///
/// Strong: the collector will keep the value reachable until [`release_lisp`], which
/// is the correct default for something Python is about to own. Ownership is the
/// caller's to document on the Python side — a handle that is never released is a
/// leak the collector cannot see through.
pub fn retain_lisp(value: crate::value::TorclVal) -> LispHandle {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| crate::gc::register_root_scanner(scan_lisp_handles));
    let mut table = handles().lock().unwrap_or_else(|error| error.into_inner());
    if let Some((index, generation)) = table.iter().enumerate().find_map(|(index, slot)| match slot
    {
        Slot::Free { generation } => Some((index, *generation)),
        _ => None,
    }) {
        table[index] = Slot::Strong { generation, value };
        return LispHandle {
            slot: index as u32,
            generation,
        };
    }
    table.push(Slot::Strong {
        generation: 0,
        value,
    });
    LispHandle {
        slot: (table.len() - 1) as u32,
        generation: 0,
    }
}

/// The value a handle names, or `None` if the handle has been released — including
/// the case where its slot has since been reused, which the generation catches.
pub fn resolve_lisp(handle: LispHandle) -> Option<crate::value::TorclVal> {
    let table = handles().lock().unwrap_or_else(|error| error.into_inner());
    match table.get(handle.slot as usize) {
        Some(Slot::Strong { generation, value }) if *generation == handle.generation => {
            Some(*value)
        }
        _ => None,
    }
}

/// Drop a handle. Idempotent, and a stale handle is ignored rather than releasing
/// whatever now occupies its slot.
pub fn release_lisp(handle: LispHandle) {
    let mut table = handles().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(Slot::Strong { generation, .. }) = table.get(handle.slot as usize) {
        if *generation == handle.generation {
            // Bump on free, so a handle to the old occupant can never resolve to
            // the next one.
            table[handle.slot as usize] = Slot::Free {
                generation: generation.wrapping_add(1),
            };
        }
    }
}

/// How many handles are held. For tests and diagnostics.
pub fn held_lisp_handles() -> usize {
    handles()
        .lock()
        .map(|table| {
            table
                .iter()
                .filter(|slot| matches!(slot, Slot::Strong { .. }))
                .count()
        })
        .unwrap_or(0)
}

// ── The object model ───────────────────────────────────────────────
//
// What crosses, and in which direction, is a policy rather than an accretion of
// special cases:
//
//   Lisp integer/float  ←→  int/float        by value
//   Lisp string         ←→  str              BY COPY, deliberately
//   NIL / T             ←→  None / True      NIL is None outbound, and inbound
//                                            None and False both arrive as NIL
//   anything else       ←→  PYTHON-OBJECT    a proxy owning one reference
//
// Strings are copied because Python's Unicode representation and TorCL's are not
// worth trying to share: the conversion is O(n) either way, and sharing would buy
// a lifetime problem for nothing. Numeric arrays are the case where copying is the
// wrong answer, and they get the buffer protocol instead (bliss-s8wrr).
//
// NIL maps to None outbound, which is the mapping that makes `(py:call f nil)`
// read correctly; the ambiguity with false is inherent to Lisp's one-false-value
// and is resolved the only way it can be — inbound False also yields NIL.

/// What a fixnum can hold: the tag takes three bits, so an integer outside this
/// range has to stay a Python object rather than be silently truncated.
const FIXNUM_RANGE: std::ops::Range<i64> = -(1i64 << 60)..(1i64 << 60);

/// Enough of a Lisp value's kind to make a conversion failure actionable.
fn describe(value: crate::value::TorclVal) -> &'static str {
    if value.is_cons() {
        "a list"
    } else if value.is_symbol() {
        "a symbol"
    } else if value.is_function() {
        "a function"
    } else if value.is_heap_object() {
        "this object"
    } else {
        "this value"
    }
}

/// Report a Python exception as a TorCL error, clearing it, or `Ok` if none is
/// set. Every entry point below funnels through this: CPython signals failure by
/// returning null *and* setting an exception, and an exception left set leaks into
/// whatever the next call happens to be.
///
/// The result is a [`Raise`] rather than a formatted string, so the condition built
/// from it keeps the exception's type, its frames and the object itself. `fallback`
/// names the operation, for the rare case where CPython reports failure with no
/// exception set at all — a contract violation on its side, which should read as
/// ours rather than as a Python error that never happened.
fn check(scope: &PythonScope, fallback: &str) -> Result<(), TorclError> {
    let api = scope.api;
    // SAFETY: resolved entry points, called on a thread holding the GIL (every
    // caller has a live PythonScope).
    let raised = unsafe {
        if (api.err_occurred)().is_null() {
            return Ok(());
        }
        let raised = match api.err_get_raised_exception {
            // 3.12+: one normalized exception instance, ours to own.
            Some(get) => PyRef::from_owned(get()),
            // 3.11: the three-part form. The value is not normalized, so it may be
            // the argument rather than the instance — which still stringifies to
            // something recognisable, and is why this path is the fallback.
            None => {
                let mut kind = std::ptr::null_mut();
                let mut value = std::ptr::null_mut();
                let mut traceback = std::ptr::null_mut();
                (api.err_fetch)(&mut kind, &mut value, &mut traceback);
                let _kind = PyRef::from_owned(kind);
                let _traceback = PyRef::from_owned(traceback);
                PyRef::from_owned(value)
            }
        };
        (api.err_clear)();
        raised
    };
    let Some(raised) = raised else {
        return Err(TorclError::FfiError(format!(
            "{fallback} failed, and CPython reported no exception"
        )));
    };
    // Describing this one would recurse if we are already describing another.
    let Some(_describing) = Describing::begin() else {
        return Err(TorclError::FfiError(format!(
            "{fallback} failed while Python was already reporting a failure"
        )));
    };
    Err(TorclError::PythonRaised(Box::new(Raise {
        kind: scope.exception_kind(&raised),
        message: scope
            .display(&raised)
            .unwrap_or_else(|_| fallback.to_string()),
        frames: scope.traceback_frames(&raised),
        object: Some(raised),
    })))
}

impl PythonScope {
    /// The bare name of an exception's class: `ValueError`, not
    /// `<class 'ValueError'>`.
    fn exception_kind(&self, exception: &PyRef) -> String {
        // SAFETY: resolved entry points and a live exception, GIL held.
        let kind = unsafe { PyRef::from_owned((self.api.object_type)(exception.pointer)) };
        let named = kind.and_then(|kind| {
            self.getattr(&kind, "__qualname__")
                .ok()
                .and_then(|name| utf8_of(self.api, &name))
        });
        // A failed lookup would have set an exception of its own; it is not the
        // caller's, so it must not outlive this function.
        self.clear_error();
        named.unwrap_or_else(|| "PythonError".to_string())
    }
}

/// The UTF-8 text of a Python `str`, copied out of CPython's buffer.
///
/// The buffer belongs to the object and is only valid while it lives, so this
/// copies rather than returning a borrow — the copy is the point (see the policy
/// note above).
fn utf8_of(api: &'static Api, text: &PyRef) -> Option<String> {
    let mut length: isize = 0;
    // SAFETY: a resolved entry point under the GIL; the returned buffer is valid
    // while `text` holds its reference, which outlives the copy below.
    unsafe {
        let bytes = (api.unicode_as_utf8_and_size)(text.pointer, &mut length);
        if bytes.is_null() || length < 0 {
            (api.err_clear)();
            return None;
        }
        let slice = std::slice::from_raw_parts(bytes as *const u8, length as usize);
        std::str::from_utf8(slice).ok().map(str::to_string)
    }
}

/// Adopt a call's result: an owned reference, or the exception that explains why
/// there isn't one.
fn adopt(scope: &PythonScope, pointer: *mut PyObject, what: &str) -> Result<PyRef, TorclError> {
    // SAFETY: the caller has just produced `pointer` from a C-API call and is
    // handing over ownership of it.
    match unsafe { PyRef::from_owned(pointer) } {
        Some(reference) => Ok(reference),
        None => {
            check(scope, what)?;
            // Null with no exception set is a CPython contract violation rather
            // than a Python-level failure; report it as ours, not theirs.
            Err(TorclError::FfiError(format!(
                "{what} returned no object and set no exception"
            )))
        }
    }
}

impl PythonScope {
    /// Import a module: `py:import`.
    pub fn import(&self, name: &str) -> Result<PyRef, TorclError> {
        let cname = CString::new(name)
            .map_err(|_| TorclError::FfiError("a module name contains a null byte".into()))?;
        // SAFETY: a resolved entry point, a valid string, GIL held by `self`.
        let module = unsafe { (self.api.import_module)(cname.as_ptr()) };
        adopt(self, module, &format!("importing {name}"))
    }

    /// Resolve a dotted name such as `numpy.mean` or `scipy.optimize.minimize`.
    ///
    /// Which segments are modules and which are attributes is not knowable from
    /// the name — `numpy.mean` is a module and a function, `scipy.optimize` is two
    /// modules — so this imports the longest prefix that IS a module and reaches
    /// the rest by attribute. Python's own import system answers the same question
    /// the same way.
    pub fn resolve(&self, dotted: &str) -> Result<PyRef, TorclError> {
        if dotted.is_empty() {
            return Err(TorclError::FfiError("an empty Python name".into()));
        }
        let segments: Vec<&str> = dotted.split('.').collect();
        for split in (1..=segments.len()).rev() {
            let prefix = segments[..split].join(".");
            let Ok(mut object) = self.import(&prefix) else {
                // A failed import leaves an exception set, which would otherwise
                // surface as the error for a later, unrelated call.
                self.clear_error();
                continue;
            };
            for attribute in &segments[split..] {
                object = self.getattr(&object, attribute)?;
            }
            return Ok(object);
        }
        // Nothing in the name is a module, so it is a builtin: `repr`, `int`,
        // `str.upper`. Python resolves these through the builtins namespace and so
        // must this, or the shortest and most obvious names would be the ones that
        // do not work.
        let mut object = self.import("builtins")?;
        for attribute in &segments {
            object = self.getattr(&object, attribute).map_err(|_| {
                self.clear_error();
                TorclError::FfiError(format!(
                    "{dotted} is neither a module nor a builtin: no module in it \
                     could be imported, and builtins has no {attribute}"
                ))
            })?;
        }
        Ok(object)
    }

    /// `py:getattr`.
    pub fn getattr(&self, object: &PyRef, name: &str) -> Result<PyRef, TorclError> {
        let cname = CString::new(name)
            .map_err(|_| TorclError::FfiError("an attribute name contains a null byte".into()))?;
        // SAFETY: resolved entry point, live reference, valid string, GIL held.
        let attribute =
            unsafe { (self.api.object_get_attr_string)(object.pointer, cname.as_ptr()) };
        adopt(self, attribute, &format!("reading attribute {name}"))
    }

    /// `(setf (py:getattr x "name") v)`.
    pub fn setattr(&self, object: &PyRef, name: &str, value: &PyRef) -> Result<(), TorclError> {
        let cname = CString::new(name)
            .map_err(|_| TorclError::FfiError("an attribute name contains a null byte".into()))?;
        // SAFETY: as above; SetAttrString borrows the value rather than stealing it.
        let status = unsafe {
            (self.api.object_set_attr_string)(object.pointer, cname.as_ptr(), value.pointer)
        };
        if status != 0 {
            check(self, &format!("setting attribute {name}"))?;
            return Err(TorclError::FfiError(format!(
                "setting attribute {name} failed without an exception"
            )));
        }
        Ok(())
    }

    /// Call a Python callable.
    ///
    /// Takes the arguments BY VALUE because `PyTuple_SETITEM` steals a reference:
    /// consuming them makes the transfer visible in the signature instead of
    /// leaving a caller holding references the tuple now owns.
    pub fn call(&self, callable: &PyRef, arguments: Vec<PyRef>) -> Result<PyRef, TorclError> {
        let count = arguments.len();
        // SAFETY: resolved entry points under the GIL. Each SetItem steals the
        // reference it is given, which is why `into_raw` rather than `as_ptr`; the
        // tuple owns them from that point and releases them when it dies.
        let result = unsafe {
            let tuple = adopt(self, (self.api.tuple_new)(count as isize), "building arguments")?;
            for (index, argument) in arguments.into_iter().enumerate() {
                let status =
                    (self.api.tuple_set_item)(tuple.pointer, index as isize, argument.into_raw());
                if status != 0 {
                    check(self, "building arguments")?;
                    return Err(TorclError::FfiError("building arguments failed".into()));
                }
            }
            (self.api.object_call)(callable.pointer, tuple.pointer, std::ptr::null_mut())
        };
        adopt(self, result, "calling a Python object")
    }

    /// `py:call-method`: the attribute, then the call.
    pub fn call_method(
        &self,
        object: &PyRef,
        name: &str,
        arguments: Vec<PyRef>,
    ) -> Result<PyRef, TorclError> {
        let method = self.getattr(object, name)?;
        self.call(&method, arguments)
    }

    /// `str(object)`, for printing and for `py:str`.
    pub fn display(&self, object: &PyRef) -> Result<String, TorclError> {
        // SAFETY: resolved entry point, live reference, GIL held.
        let text = adopt(
            self,
            unsafe { (self.api.object_str)(object.pointer) },
            "stringifying a Python object",
        )?;
        utf8_of(self.api, &text)
            .ok_or_else(|| TorclError::FfiError("a Python string was not valid UTF-8".into()))
    }

    /// `repr(object)`, which is what a Lisp printer wants: unambiguous.
    pub fn represent(&self, object: &PyRef) -> Result<String, TorclError> {
        // SAFETY: as `display`.
        let text = adopt(
            self,
            unsafe { (self.api.object_repr)(object.pointer) },
            "representing a Python object",
        )?;
        utf8_of(self.api, &text)
            .ok_or_else(|| TorclError::FfiError("a Python repr was not valid UTF-8".into()))
    }

    /// `py:type-of`: the object's type, as a proxy for the type object itself —
    /// not its name, so the result can be called, compared, and asked for its own
    /// attributes the way Python code would.
    pub fn type_of(&self, object: &PyRef) -> Result<PyRef, TorclError> {
        // SAFETY: resolved entry point, live reference, GIL held.
        adopt(
            self,
            unsafe { (self.api.object_type)(object.pointer) },
            "taking the type of a Python object",
        )
    }

    /// `py:typep`, with the class named the way Python names it:
    /// `(py:typep x "numpy.ndarray")`.
    pub fn is_instance(&self, object: &PyRef, class: &str) -> Result<bool, TorclError> {
        let class = self.resolve(class)?;
        // SAFETY: resolved entry point, two live references, GIL held.
        let status = unsafe { (self.api.object_is_instance)(object.pointer, class.pointer) };
        if status < 0 {
            check(self, "testing a Python type")?;
            return Err(TorclError::FfiError("testing a Python type failed".into()));
        }
        Ok(status == 1)
    }

    /// Discard a pending exception. Used where a failure is a legitimate answer
    /// rather than an error — probing an import, above.
    fn clear_error(&self) {
        // SAFETY: a resolved entry point, GIL held.
        unsafe { (self.api.err_clear)() };
    }

    /// Is this object Python's `None`?
    fn is_none(&self, object: *mut PyObject) -> bool {
        self.api.none == object
    }

    fn is_instance_of_type(&self, object: *mut PyObject, class: *mut PyObject) -> bool {
        // SAFETY: a resolved entry point and two live objects, GIL held. A negative
        // result sets an exception, which is cleared rather than reported: this is
        // used to CLASSIFY a value, and failing to classify it is not an error.
        unsafe {
            let status = (self.api.object_is_instance)(object, class);
            if status < 0 {
                (self.api.err_clear)();
                return false;
            }
            status == 1
        }
    }

    /// A Lisp value as a Python object, by the policy at the top of this section.
    pub fn to_python(&self, value: crate::value::TorclVal) -> Result<PyRef, TorclError> {
        if value == crate::value::NIL {
            // NIL is the empty list, false, and "nothing" all at once; None is the
            // mapping that makes an omitted argument read correctly.
            return self.none();
        }
        if value == crate::value::T {
            return adopt(
                self,
                unsafe { (self.api.bool_from_long)(1) },
                "making a Python bool",
            );
        }
        if let Some(reference) = self.proxy_reference(value) {
            // A proxy going back is the object it proxies, not a description of it.
            return reference;
        }
        if value.is_fixnum() {
            // SAFETY: a resolved entry point under the GIL.
            return adopt(
                self,
                unsafe { (self.api.long_from_longlong)(value.as_fixnum()) },
                "making a Python int",
            );
        }
        if value.is_double_float() || value.is_single_float() {
            let double = if value.is_double_float() {
                value.as_double_float()
            } else {
                // Python has one float type, so a single-float widens. The widening
                // is exact; the narrowing on the way back is not, which is why
                // inbound floats are always double-floats.
                f64::from(value.as_single_float())
            };
            // SAFETY: as above.
            return adopt(
                self,
                unsafe { (self.api.float_from_double)(double) },
                "making a Python float",
            );
        }
        if value.is_string() {
            let text = value.as_string();
            // SAFETY: a resolved entry point under the GIL; the pointer and length
            // describe `text`, which outlives the call.
            return adopt(
                self,
                unsafe {
                    (self.api.unicode_from_string_and_size)(
                        text.as_ptr() as *const i8,
                        text.len() as isize,
                    )
                },
                "making a Python str",
            );
        }
        if value.is_character() {
            // A character becomes a one-character string: Python has no character
            // type, and `str` of length one is what its own APIs expect.
            let text = value.as_char().to_string();
            return adopt(
                self,
                unsafe {
                    (self.api.unicode_from_string_and_size)(
                        text.as_ptr() as *const i8,
                        text.len() as isize,
                    )
                },
                "making a Python str",
            );
        }
        Err(TorclError::FfiError(format!(
            "no Python equivalent for {}: pass a number, string, character, NIL, T, \
             or a PYTHON-OBJECT",
            describe(value)
        )))
    }

    /// Python's `None`, as an owned reference.
    fn none(&self) -> Result<PyRef, TorclError> {
        // SAFETY: `none` is the address of CPython's immortal None singleton,
        // resolved once; adding a reference to it is valid and (since 3.12)
        // a no-op.
        unsafe { (self.api.inc_ref)(self.api.none) };
        unsafe { PyRef::from_owned(self.api.none) }
            .ok_or_else(|| TorclError::FfiError("CPython's None was not resolved".into()))
    }

    /// A Python object as a Lisp value, by the same policy.
    ///
    /// Consumes the reference: whatever comes back either carries it (a proxy) or
    /// does not need it (an immediate), and leaving the caller holding one is how
    /// a bridge leaks.
    pub fn from_python(&self, object: PyRef) -> Result<crate::value::TorclVal, TorclError> {
        if self.is_none(object.pointer) {
            return Ok(crate::value::NIL);
        }
        // Booleans BEFORE integers: Python's bool is a subclass of int, so the
        // integer test matches True and would turn it into 1.
        if self.is_instance_of_type(object.pointer, self.api.bool_type) {
            // SAFETY: a resolved entry point, a live bool, GIL held.
            let truth = unsafe { (self.api.object_is_true)(object.pointer) };
            return Ok(if truth == 1 {
                crate::value::T
            } else {
                crate::value::NIL
            });
        }
        if self.is_instance_of_type(object.pointer, self.api.long_type) {
            let mut overflow = 0i32;
            // SAFETY: as above. The overflow flag is how CPython reports an
            // integer too large for the C type, without setting an exception.
            let value =
                unsafe { (self.api.long_as_longlong_and_overflow)(object.pointer, &mut overflow) };
            if overflow == 0 && FIXNUM_RANGE.contains(&value) {
                return Ok(crate::value::TorclVal::from_fixnum(value));
            }
            // Python's integers are unbounded and TorCL's fixnums are not. A
            // bignum conversion belongs with the numeric tower rather than here,
            // so an integer that does not fit stays a proxy — visible and exact —
            // rather than being silently truncated or turned into a float.
            return self.proxy(object);
        }
        if self.is_instance_of_type(object.pointer, self.api.float_type) {
            // SAFETY: a resolved entry point, a live float, GIL held.
            let value = unsafe { (self.api.float_as_double)(object.pointer) };
            check(self, "converting a Python float")?;
            return Ok(crate::gc::alloc_double_float(value));
        }
        if self.is_instance_of_type(object.pointer, self.api.unicode_type) {
            let text = utf8_of(self.api, &object).ok_or_else(|| {
                TorclError::FfiError("a Python string was not valid UTF-8".into())
            })?;
            // Copied, per the policy: the Lisp string owns its own storage and the
            // Python object's death cannot invalidate it.
            return Ok(crate::gc::alloc_character_string(&text));
        }
        self.proxy(object)
    }
}

// ── PYTHON-OBJECT: the Lisp-visible proxy ──────────────────────────
//
// One untraced word holding the `PyObject *` this proxy owns a reference to.
// Nulled when the reference has been handed back, which is how a proxy that has
// outlived its interpreter (or an image restore) reports itself rather than
// dereferencing an address that now means something else.
//
// The release is driven by the collector through the ordinary finalizer registry —
// the same mechanism streams and mutexes use for native state — because that
// registry's association with the object is deliberately weak, so holding a proxy
// in the table would not be what keeps it alive. What the destructor must NOT do
// is call CPython: it runs inside the GC pause under the heap lock, and a decref
// can run `__del__`. It enqueues instead, which is what [`PyRef`] already does.

impl PyRef {
    /// Give up ownership without releasing, for handing the reference to something
    /// that will own it (a tuple slot, or a proxy object's body word).
    fn into_raw(self) -> *mut PyObject {
        let pointer = self.pointer;
        std::mem::forget(self);
        pointer
    }
}

/// Is this Lisp value a Python proxy?
pub fn is_proxy(value: crate::value::TorclVal) -> bool {
    value.is_heap_object()
        // SAFETY: a heap-object tag guarantees a readable header.
        && unsafe {
            (*(value.as_ptr() as *const crate::object::ObjectHeader)).type_id()
                == crate::object::type_id::PYTHON_OBJECT
        }
}

impl PythonScope {
    /// Wrap an owned reference as a Lisp `PYTHON-OBJECT`.
    ///
    /// The allocation can move objects, which is why the reference is consumed
    /// only after it succeeds: a failed allocation must not lose the reference,
    /// and a moved object must not be reached through a stale wrapper.
    pub fn proxy(&self, object: PyRef) -> Result<crate::value::TorclVal, TorclError> {
        let body = crate::gc::alloc_typed(8, crate::object::type_id::PYTHON_OBJECT)
            .ok_or(TorclError::Oom)?;
        // SAFETY: a fresh 8-byte body, initialised before anything else can run.
        // No Lisp reference is stored here, so the GC never traces this word.
        let value = unsafe {
            (body as *mut u64).write(object.into_raw() as u64);
            crate::value::TorclVal::from_heap_ptr(body.sub(8))
        };
        // NIL as the finalizer, not the proxy: the registry keys weakly on the
        // body address but roots the finalizer VALUE strongly, so registering the
        // proxy there would make it immortal and its reference would never be
        // released. The destructor needs only the body address, which the dispatch
        // already passes (matching how streams and mutexes register native state).
        crate::gc::register_finalizer(
            crate::value::TorclVal::from_raw(body as u64),
            crate::value::NIL,
        )?;
        Ok(value)
    }

    /// The object a proxy names, as a fresh owned reference, or `None` if `value`
    /// is not a proxy (or is one whose reference has already been given back).
    fn proxy_reference(
        &self,
        value: crate::value::TorclVal,
    ) -> Option<Result<PyRef, TorclError>> {
        if !is_proxy(value) {
            return None;
        }
        // SAFETY: the type discriminator selects exactly this one untraced word.
        let pointer = unsafe { (value.as_ptr().add(8) as *const u64).read() } as *mut PyObject;
        if pointer.is_null() {
            return Some(Err(TorclError::FfiError(
                "this PYTHON-OBJECT no longer refers to anything: its interpreter \
                 has been shut down, or it was restored from a saved image"
                    .into(),
            )));
        }
        // SAFETY: the proxy owns a reference, so the object is live and adding a
        // reference to it is valid; `self` proves the GIL is held.
        Some(
            unsafe { PyRef::from_borrowed(pointer, self) }
                .ok_or_else(|| TorclError::FfiError("a PYTHON-OBJECT with no object".into())),
        )
    }

    /// The object a proxy names, as an owned reference — the entry point every
    /// Lisp-facing operation on a proxy goes through.
    pub fn unwrap_proxy(&self, value: crate::value::TorclVal) -> Result<PyRef, TorclError> {
        self.proxy_reference(value).unwrap_or_else(|| {
            Err(TorclError::TypeError {
                datum: value,
                expected: "PY:OBJECT".into(),
            })
        })
    }
}

/// Release a dead proxy's reference. Called by the collector's finalizer dispatch
/// with the proxy's untagged body address.
///
/// Runs inside the GC pause under the heap lock, so it must neither allocate on
/// the Lisp heap nor call CPython. It does neither: the reference is queued, and
/// the body word is nulled so that a later trace or a stray access through a
/// now-dead proxy finds nothing rather than a pointer CPython may already have
/// reused.
///
/// # Safety
/// `body` must be the body address of a live `PYTHON_OBJECT` whose proxy the
/// collector has determined to be unreachable.
pub unsafe fn finalize_proxy(body: *mut u8) {
    // SAFETY: the caller guarantees this is a PYTHON_OBJECT body, whose single
    // word is the owned pointer.
    unsafe {
        let pointer = (body as *const u64).read() as *mut PyObject;
        if pointer.is_null() {
            return;
        }
        (body as *mut u64).write(0);
        // Constructing a PyRef and dropping it is exactly the queueing path, so
        // the release rule lives in one place rather than two.
        drop(PyRef::from_owned(pointer));
    }
}

// ── Exceptions ─────────────────────────────────────────────────────
//
// A Python failure is carried out of the crossing with its structure intact —
// type, message, frames, and the exception object — rather than flattened into a
// string. The frames are the reason: a mixed-language backtrace is the thing that
// makes this feel deeper than a foreign-function call, and a formatted message has
// already thrown them away.

/// One Python stack frame, as `traceback.extract_tb` reports it.
#[derive(Debug, Clone)]
pub struct Frame {
    pub file: String,
    pub line: i64,
    pub function: String,
}

/// A Python exception, owned by the Rust side.
#[derive(Debug)]
pub struct Raise {
    /// The exception class's name: `ValueError`, `KeyError`.
    pub kind: String,
    /// `str(exception)` — what Python would print after the colon.
    pub message: String,
    /// Outermost first, matching Python's own traceback order.
    pub frames: Vec<Frame>,
    /// The exception itself, so a caller can reach its attributes. `None` when it
    /// could not be recovered — which is rare but must not be fatal.
    pub object: Option<PyRef>,
}

thread_local! {
    /// True while an exception is being described on this thread.
    static DESCRIBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the thread as describing an exception for as long as it lives.
///
/// Describing one means calling back into Python — `str(exception)`, the type's
/// `__qualname__`, `traceback.extract_tb` — and any of those can itself raise, on a
/// stack too deep to walk or a `__str__` that throws. Without this, the failure of
/// the description would be described, whose failure would be described, without
/// bound; the first version of this guarded only the traceback walk, which left the
/// other two able to do it.
///
/// A guard rather than a pair of set calls so that no early return or panic can
/// leave the thread permanently unable to report an exception.
struct Describing;

impl Describing {
    /// `None` if a description is already in progress, in which case the caller
    /// must not start another.
    fn begin() -> Option<Self> {
        if DESCRIBING.with(|flag| flag.replace(true)) {
            None
        } else {
            Some(Describing)
        }
    }
}

impl Drop for Describing {
    fn drop(&mut self) {
        DESCRIBING.with(|flag| flag.set(false));
    }
}

impl PythonScope {
    /// The frames of `exception`'s traceback, outermost first.
    ///
    /// Asks Python's own `traceback` module rather than walking the traceback
    /// object's fields: those are struct members with no stable C accessors, and
    /// `extract_tb` already answers exactly this question.
    ///
    /// Best effort throughout. A missing or unwalkable traceback yields no frames;
    /// it must never turn a Python error into a TorCL one.
    fn traceback_frames(&self, exception: &PyRef) -> Vec<Frame> {
        let extracted = (|| -> Option<Vec<Frame>> {
            let traceback = self.getattr(exception, "__traceback__").ok()?;
            if self.is_none(traceback.pointer) {
                return None;
            }
            let extract = self.resolve("traceback.extract_tb").ok()?;
            let summaries = self.call(&extract, vec![traceback]).ok()?;
            // SAFETY: a resolved entry point and a live sequence, GIL held.
            let count = unsafe { (self.api.sequence_size)(summaries.pointer) };
            if count < 0 {
                self.clear_error();
                return None;
            }
            let mut extracted = Vec::with_capacity(count as usize);
            for index in 0..count {
                // SAFETY: as above, and `index` is within the reported size.
                let item = unsafe { (self.api.sequence_get_item)(summaries.pointer, index) };
                let Some(summary) = (unsafe { PyRef::from_owned(item) }) else {
                    self.clear_error();
                    break;
                };
                let text = |name: &str| {
                    self.getattr(&summary, name)
                        .ok()
                        // SAFETY: a resolved entry point and a live attribute
                        // value, GIL held; `object_str` hands back a new reference.
                        .and_then(|value| unsafe {
                            PyRef::from_owned((self.api.object_str)(value.pointer))
                        })
                        .and_then(|value| utf8_of(self.api, &value))
                        .unwrap_or_default()
                };
                extracted.push(Frame {
                    file: text("filename"),
                    line: text("lineno").parse().unwrap_or(0),
                    function: text("name"),
                });
            }
            Some(extracted)
        })();
        // Anything the extraction left set belongs to the extraction, not to the
        // exception being described.
        self.clear_error();
        extracted.unwrap_or_default()
    }
}

impl Raise {
    /// The mixed-backtrace line for one frame, in the shape the debugger uses.
    pub fn render_frames(&self) -> String {
        self.frames
            .iter()
            .rev()
            .map(|frame| {
                format!(
                    "  Python  {} at {}:{}\n",
                    frame.function, frame.file, frame.line
                )
            })
            .collect()
    }
}
