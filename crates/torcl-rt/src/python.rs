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
