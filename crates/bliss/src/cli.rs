//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_compiler::macroexpand::{
    self as compiler_macroexpand, Environment as MacroexpandEnv, FunctionInfo, VariableInfo,
};
use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::lock_order::{LockLevel, OrderedMutex};

mod bytecode;
use bliss_rt::object::{ConsCell, ObjectHeader, RatioData, type_id};
use bliss_rt::runtime::parse_cli as parse_runtime_cli;
use bliss_rt::value::{BlissVal, EOF, NIL, T};

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock, Mutex, Once, Weak};
use std::thread::ThreadId;

// ── CLI arguments ──────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct CliArgs {
    pub image: Option<String>,
    pub eval: Option<String>,
    pub load: Option<String>,
    pub no_image: bool,
    pub bootstrap: bool,
    pub no_bootstrap: bool,
    pub workers: Option<usize>,
    pub heap_size: Option<String>,
    pub help: bool,
    pub version: bool,
    pub sandbox: bool,
    pub no_init: bool,
    pub cl_args: Vec<String>,
    pub script: Option<String>,
    /// `--load-report FILE`: fault-tolerantly evaluate every top-level form in
    /// FILE, continuing past errors, and print a categorized punch-list of the
    /// forms that failed. A diagnostic aid for bring-up (e.g. loading ASDF).
    pub load_report: Option<String>,
}

impl CliArgs {
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut shared_args = Vec::new();
        let mut cl_args = Vec::new();
        let mut bootstrap = false;
        let mut no_bootstrap = false;
        let mut sandbox = false;
        let mut no_init = false;
        let mut help = false;
        let mut version = false;
        let mut script = None;
        let mut load_report = None;
        let mut saw_double_dash = false;

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if saw_double_dash {
                cl_args.push(arg.clone());
                i += 1;
                continue;
            }

            match arg.as_str() {
                "--" => {
                    saw_double_dash = true;
                    i += 1;
                }
                "--help" | "--version" => {
                    if arg == "--help" {
                        help = true;
                    } else {
                        version = true;
                    }
                    i += 1;
                }
                s if is_forwarded_runtime_flag(s) => {
                    shared_args.push(arg.clone());
                    if runtime_flag_requires_value(arg) {
                        let value = args.get(i + 1).ok_or_else(|| {
                            BlissError::Internal(format!("{} requires a value", arg))
                        })?;
                        shared_args.push(value.clone());
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "-e" => {
                    shared_args.push("--eval".into());
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| BlissError::Internal("-e requires a value".into()))?;
                    shared_args.push(value.clone());
                    i += 2;
                }
                "--bootstrap" => {
                    // The prelude now loads by default; --bootstrap is kept as an
                    // accepted no-op for backward compatibility.
                    bootstrap = true;
                    i += 1;
                }
                "--no-bootstrap" => {
                    no_bootstrap = true;
                    i += 1;
                }
                "--sandbox" => {
                    sandbox = true;
                    i += 1;
                }
                "--no-init" => {
                    no_init = true;
                    i += 1;
                }
                "--load-report" => {
                    let value = args.get(i + 1).ok_or_else(|| {
                        BlissError::Internal("--load-report requires a file argument".into())
                    })?;
                    load_report = Some(value.clone());
                    i += 2;
                }
                s if s.starts_with('-') => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", s)));
                }
                _ => {
                    if script.is_none() {
                        script = Some(arg.clone());
                    }
                    i += 1;
                }
            }
        }

        let (config, runtime_cl_args) = parse_runtime_cli(&shared_args)?;
        if !runtime_cl_args.is_empty() {
            cl_args.extend(runtime_cl_args);
        }

        let r = CliArgs {
            image: extract_flag_value(&shared_args, "--image"),
            eval: config.eval_form.clone(),
            load: config.load_file.clone(),
            no_image: shared_args.iter().any(|arg| arg == "--no-image"),
            bootstrap,
            no_bootstrap,
            workers: extract_flag_value(&shared_args, "--workers")
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| BlissError::Internal("--workers requires a numeric value".into()))?,
            heap_size: extract_size_arg(&shared_args, "--heap-size"),
            help,
            version,
            sandbox,
            no_init,
            cl_args,
            script,
            load_report,
        };
        if r.image.is_some() && r.no_image {
            return Err(BlissError::Internal(
                "--image and --no-image are contradictory".into(),
            ));
        }
        if r.sandbox && r.no_image {
            return Err(BlissError::Internal(
                "--sandbox and --no-image are contradictory".into(),
            ));
        }
        if r.eval.is_some() && r.load.is_some() {
            return Err(BlissError::Internal(
                "--eval and --load are contradictory".into(),
            ));
        }
        Ok(r)
    }
}

fn is_forwarded_runtime_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--no-image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--jit-dump"
            | "--log-level"
    )
}

fn runtime_flag_requires_value(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--log-level"
    )
}

fn extract_size_arg(args: &[String], flag: &str) -> Option<String> {
    extract_flag_value(args, flag)
}

fn extract_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| {
        if pair[0] == flag {
            Some(pair[1].clone())
        } else {
            None
        }
    })
}

// ── Arena allocator (replaces Box::leak) ─────────────────────────
// A bump allocator that collects all allocations and frees them on drop.
// Uses a generational scheme: blocks can be promoted to a "permanent" set
// that survives arena compaction, while temporary blocks are freed.
struct Arena {
    blocks: Vec<(*mut u8, std::alloc::Layout)>,
    /// Permanent blocks that survive compaction (referenced by the environment)
    permanent: Vec<(*mut u8, std::alloc::Layout)>,
    /// Threshold: when blocks exceed this count, a compaction is suggested
    compaction_threshold: usize,
}

impl Arena {
    fn new() -> Self {
        Arena {
            blocks: Vec::new(),
            permanent: Vec::new(),
            compaction_threshold: 10000,
        }
    }

    /// Promote all current blocks to permanent status.
    /// Call this after loading an image or defining persistent functions
    /// so those allocations survive future compaction.
    fn promote_all(&mut self) {
        self.permanent.append(&mut self.blocks);
    }

    /// Free all non-permanent blocks. This reclaims memory from temporary
    /// allocations (e.g., intermediate eval results in the REPL).
    /// SAFETY: Caller must ensure no references to non-permanent blocks exist.
    fn compact(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        self.blocks.clear();
    }

    /// Returns true if the number of temporary blocks exceeds the threshold
    fn should_compact(&self) -> bool {
        self.blocks.len() > self.compaction_threshold
    }

    fn alloc_cons(&mut self, car: BlissVal, cdr: BlissVal) -> BlissVal {
        let roots = bliss_rt::ShadowRootScope::new();
        let car = roots.root(car);
        let cdr = roots.root(cdr);
        // Primary path (bliss-jtc.1): allocate the cons on the shared GC heap so
        // it uses the same layout as compiled code and is visible to heap walking.
        // A headered GC object; the body (car@0, cdr@8) is what `from_cons_ptr`
        // points at, exactly like the old headerless cell.
        if let Some(body) = bliss_rt::gc::alloc_typed(16, type_id::CONS) {
            unsafe {
                let cell = body as *mut ConsCell;
                (*cell).car = car.get();
                (*cell).cdr = cdr.get();
                return BlissVal::from_cons_ptr(body);
            }
        }
        // Fallback (heap unavailable/exhausted): a local block freed on drop.
        let layout = std::alloc::Layout::new::<ConsCell>();
        unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            let cell = ptr as *mut ConsCell;
            (*cell).car = car.get();
            (*cell).cdr = cdr.get();
            self.blocks.push((ptr, layout));
            BlissVal::from_cons_ptr(ptr)
        }
    }

    fn alloc_str(&mut self, s: &str) -> BlissVal {
        let b = s.as_bytes();
        // Primary path (bliss-jtc.1): allocate the string on the shared GC heap.
        // Body layout [len:u64 | bytes]; the value points at the object header
        // (body−8), matching the old std::alloc layout [header | len | bytes].
        if let Some(body) = bliss_rt::gc::alloc_typed(8 + b.len(), type_id::SIMPLE_BASE_STRING) {
            unsafe {
                *(body as *mut u64) = b.len() as u64;
                std::ptr::copy_nonoverlapping(b.as_ptr(), body.add(8), b.len());
                return BlissVal::from_heap_ptr(body.sub(8));
            }
        }
        // Fallback: a local block freed on drop.
        let sz = 8 + 8 + b.len();
        let layout = std::alloc::Layout::from_size_align(sz, 8).unwrap();
        unsafe {
            let p = std::alloc::alloc_zeroed(layout);
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            *(p as *mut ObjectHeader) =
                ObjectHeader::new(type_id::SIMPLE_BASE_STRING, sz.div_ceil(8) as u16);
            *(p.add(8) as *mut u64) = b.len() as u64;
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(16), b.len());
            self.blocks.push((p, layout));
            BlissVal::from_heap_ptr(p)
        }
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        for &(ptr, layout) in &self.permanent {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
    }
}

// Thread-local arena so alloc functions can be called from anywhere in the evaluator.
thread_local! {
    static ARENA: RefCell<Arena> = RefCell::new(Arena::new());
}

/// Bytecode compiler-macro expanders live in bliss-compiler's Send + Sync
/// global callback table. Keep weak handles here so the evaluator GC scanner
/// can relocate their literal pools without retaining obsolete redefinitions.
static LOADED_COMPILER_MACRO_FUNCTIONS: LazyLock<
    Mutex<Vec<Weak<Mutex<bliss_rt::bytecode::BytecodeFunction>>>>,
> = LazyLock::new(|| Mutex::new(Vec::new()));

fn arena_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    ARENA.with(|a| a.borrow_mut().alloc_cons(car, cdr))
}

fn arena_str(s: &str) -> BlissVal {
    let val = ARENA.with(|a| a.borrow_mut().alloc_str(s));
    bliss_stdlib::register_string(val, s);
    val
}

// ── First-class PACKAGE objects, backed by the stdlib registry (bliss-bhs) ──
//
// A package is a distinct CL type, not a string (CLHS 11.1), represented as a
// `SPECIAL`-tagged meta-handle carrying an opaque id — so `(typep p 'package)`
// is true while `(typep "P" 'package)` is NIL, and `(eq (find-package :p)
// (find-package :nickname))` is T.
//
// Stage 4 of bliss-bhs retired the interpreter's own `env.packages` map: ALL
// package storage — names, nicknames, use-lists, exports, and each package's
// symbol index (including COMMON-LISP's) — now lives in the single process-global
// `bliss_stdlib` PackageRegistry, the authoritative store the standard library
// already implemented. We keep exactly one registry alive and active for the
// thread here and drive it through the `bliss_stdlib::{find_package, intern, …}`
// free functions. Package ids are drawn from a reserved HIGH base (`1 << 40`, in
// the registry) so they can never coincide with the low-range CLOS/macro/class
// meta-handles.
//
// Symbol *identity* still lives in the global reader symbol table
// (`bliss_rt::symbols`): a symbol IS a table index, and the interpreter owns the
// naming scheme (COMMON-LISP symbols are bare `CAR`, keywords `:X`, others
// `PKG::X`). The registry stores, per package, the map from bare name to that
// already-interned symbol — it indexes membership, it does not re-allocate.

thread_local! {
    /// The one live package registry for this thread. Kept here so it stays
    /// active (and un-dropped) for the whole session; never cloned into child
    /// `Env`s (the activation guard's Drop would deactivate the store).
    static PACKAGE_REGISTRY: std::cell::RefCell<Option<bliss_stdlib::PackageRegistry>> =
        const { std::cell::RefCell::new(None) };
}

/// Install a fresh registry seeded with the standard packages. Mirrors CLOS's
/// reset-on-top-level-`Env::new` (`bootstrap_clos`): a new top-level environment
/// starts from the standard package layout, so tests and REPL sessions are
/// isolated. `PackageRegistry::new` activates the new store before the old
/// registry is dropped, so the drop is a no-op for activation.
fn reset_package_registry() {
    let reg = bliss_stdlib::PackageRegistry::new();
    PACKAGE_REGISTRY.with(|cell| *cell.borrow_mut() = Some(reg));
    seed_standard_packages_registry();
}

/// Ensure a registry exists without resetting it. Mirrors
/// `ensure_clos_bootstrapped`: macro / compiler-macro expansion environments
/// share the caller's live packages rather than wiping them.
fn ensure_package_registry() {
    let missing = PACKAGE_REGISTRY.with(|cell| cell.borrow().is_none());
    if missing {
        reset_package_registry();
    }
}

/// Seed the standard package layout into the active registry. Matches the layout
/// the interpreter has always used: COMMON-LISP (nick CL); COMMON-LISP-USER
/// (nick CL-USER) using COMMON-LISP; KEYWORD; BLISS-INTERNAL; BLISS-EXT using
/// COMMON-LISP. The reader keeps its own parallel package-name registration.
fn seed_standard_packages_registry() {
    let _ = bliss_stdlib::make_package("COMMON-LISP", &["CL"], &[]);
    let _ = bliss_stdlib::make_package("COMMON-LISP-USER", &["CL-USER"], &["COMMON-LISP"]);
    let _ = bliss_stdlib::make_package("KEYWORD", &[], &[]);
    let _ = bliss_stdlib::make_package("BLISS-INTERNAL", &[], &[]);
    let _ = bliss_stdlib::make_package("BLISS-EXT", &[], &["COMMON-LISP"]);
    for name in [
        "COMMON-LISP",
        "COMMON-LISP-USER",
        "KEYWORD",
        "BLISS-INTERNAL",
        "BLISS-EXT",
    ] {
        reader::register_package(name);
    }
    reader::register_package("CL-USER");
}

/// The canonical package object for `canonical_name` (nicknames resolved first).
/// Find-or-create against the registry, so `*PACKAGE*` and package designators
/// always yield an object; interned by the registry: same name ⇒ same handle.
fn package_object(canonical_name: &str) -> BlissVal {
    ensure_package_registry();
    if let Some(p) = bliss_stdlib::find_package(canonical_name) {
        return p;
    }
    bliss_stdlib::make_package(canonical_name, &[], &[]).unwrap_or(NIL)
}

/// True if `v` is a first-class package object (a live handle in the registry) —
/// the `packagep` / `typep 'package` predicate.
fn is_package_object(v: BlissVal) -> bool {
    bliss_stdlib::is_package(v)
}

/// The canonical name of a package object, or `None` if `v` is not one.
fn package_object_name(v: BlissVal) -> Option<String> {
    bliss_stdlib::package_name(v)
}

// ── Stream designator resolution ─────────────────────────────────
// The interpreter delegates all stream state to `bliss_stdlib::streams`
// (a stream is a heap object with type_id STREAM). These helpers turn the
// CL stream *designators* T / NIL / stream into a concrete stream object and
// route character output through the standard-library Gray-stream API.

/// True if `val` is a real stream object (heap object with STREAM type_id).
fn is_stream(val: BlissVal) -> bool {
    bliss_rt::types::streamp(val)
}

/// Resolve an output stream designator: `T` and `NIL` both denote the current
/// `*standard-output*`; any other value is taken to be a stream object.
fn resolve_output_stream(designator: BlissVal, env: &Env) -> BlissVal {
    if designator == T || designator.is_nil() {
        env.lookup_var("*STANDARD-OUTPUT*").unwrap_or(NIL)
    } else {
        designator
    }
}

/// Resolve an input stream designator: `NIL` denotes `*standard-input*`, `T`
/// denotes `*terminal-io*`; any other value is taken to be a stream object.
fn resolve_input_stream(designator: BlissVal, env: &Env) -> BlissVal {
    if designator.is_nil() {
        env.lookup_var("*STANDARD-INPUT*").unwrap_or(NIL)
    } else if designator == T {
        env.lookup_var("*TERMINAL-IO*").unwrap_or(NIL)
    } else {
        designator
    }
}

/// Write a string to a resolved output stream via the stdlib stream API.
fn write_str_to(stream: BlissVal, s: &str) -> Result<(), BlissError> {
    let sv = bliss_stdlib::make_lisp_string(s);
    bliss_stdlib::stream_write_string(stream, sv, 0, None)
}

/// True if `s` is a user-defined Gray stream: a CLOS instance whose class
/// precedence list includes `FUNDAMENTAL-STREAM`. The standard stream functions
/// route such streams through the Gray generic functions (spec §5.5.2,
/// bliss-jtc.7b); built-in Rust-backed streams take the fast stdlib path.
fn is_gray_stream(s: BlissVal) -> bool {
    bliss_stdlib::is_instance(s)
        && instance_class_hierarchy_names(s)
            .map(|names| names.iter().any(|n| n == "FUNDAMENTAL-STREAM"))
            .unwrap_or(false)
}

/// Read one character from an input stream, or `None` at end of input. Handles
/// both native and Gray streams.
fn stream_next_char(stream: BlissVal, env: &mut Env) -> Result<Option<char>, BlissError> {
    if is_gray_stream(stream) {
        let r = invoke_generic_function("STREAM-READ-CHAR", &[stream], env)?;
        Ok(r.is_character().then(|| r.as_char()))
    } else {
        let r = bliss_stdlib::stream_read_char(stream)?;
        Ok((r != EOF).then(|| r.as_char()))
    }
}

/// Push one character back onto an input stream (its unread buffer holds one).
fn stream_push_char(stream: BlissVal, ch: char, env: &mut Env) -> Result<(), BlissError> {
    let cv = BlissVal::from_char(ch);
    if is_gray_stream(stream) {
        invoke_generic_function("STREAM-UNREAD-CHAR", &[stream, cv], env)?;
        Ok(())
    } else {
        bliss_stdlib::stream_unread_char(stream, cv)
    }
}

/// Read a single Lisp form from an input stream (shared by READ and
/// READ-PRESERVING-WHITESPACE). The stream only buffers one un-read character, so
/// we grow a buffer one char at a time and re-parse; the first time a parse
/// consumes fewer chars than the buffer we have read exactly one terminator past
/// a complete form, which we push back. Returns `None` at end of input (only
/// whitespace/comments remained). Neither consumes trailing whitespace beyond the
/// single terminator. bliss-lb6.14: ASDF's slurp-stream-forms reads source files.
fn read_one_form_from_stream(
    stream: BlissVal,
    env: &mut Env,
) -> Result<Option<BlissVal>, BlissError> {
    let mut buffer = String::new();
    loop {
        match stream_next_char(stream, env)? {
            None => {
                if buffer.trim().is_empty() {
                    return Ok(None);
                }
                let (form, _) = read_from_string_in_env(&buffer, env)?;
                return Ok(Some(form));
            }
            Some(c) => {
                buffer.push(c);
                if let Ok((form, consumed)) = read_from_string_in_env(&buffer, env) {
                    let total = buffer.chars().count();
                    if consumed < total {
                        for lc in buffer
                            .chars()
                            .skip(consumed)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                        {
                            stream_push_char(stream, lc, env)?;
                        }
                        return Ok(Some(form));
                    }
                }
            }
        }
    }
}

/// The global value of symbol `idx` from its heap value cell, or `None` if the
/// cell is unbound (bliss-jtc.6 Stage C2). This is the authoritative store for
/// global (non-lexical) variable values.
fn global_value_cell(idx: u32) -> Option<BlissVal> {
    match bliss_rt::symbols::symbol_value(idx) {
        Some(v) if v != bliss_rt::value::UNBOUND => Some(v),
        _ => None,
    }
}

/// The global interpreted-function object bound to `name`'s function cell, or
/// `None` (bliss-jtc.6.8). This is the authoritative store for ordinary global
/// (defun) functions; lexical FLET/LABELS and `(setf f)` functions stay in
/// `Env.funs`.
fn global_fn(name: &str) -> Option<BlissVal> {
    let idx = bliss_rt::symbols::find_index(name)?;
    let cell = bliss_rt::symbols::symbol_function(idx)?;
    bliss_rt::function::is_interpreted_function(cell).then_some(cell)
}

/// Coerce a value being installed into a symbol's global function cell (via
/// `(setf (symbol-function s) v)` / `(setf (fdefinition s) v)` and the
/// bytecode-lowered `BLISS::SET-SYMBOL-FUNCTION` primitive) into the canonical
/// stored representation — an interpreted-function object.
///
/// The cell is only honoured by `global_fn`/`fn_bound`/`fboundp`/`callable_body`
/// when it holds an interpreted-function object. But `(setf (symbol-function s)
/// v)` accepts any function designator, and two common designators are NOT
/// interpreted-function objects: `#'foo` (which the FUNCTION form evaluates to
/// the bare *symbol* FOO when foo is a global) and `(lambda …)` (which evaluates
/// to the interpreter closure cons `(BLISS::CLOSURE . id)`). Storing either
/// verbatim left the installed name "undefined" (bliss-57m). Resolve them to a
/// real function object here:
///   * an interpreted-function object → stored as-is;
///   * a symbol designator → the current global definition it names (CL captures
///     the function, not the name — CLHS 5.1.2.7);
///   * a non-capturing closure `(BLISS::CLOSURE . id)` → reified into an
///     interpreted-function object carrying its lambda list and body.
///
/// A *capturing* closure installed this way still loses its captures: both the
/// operator-position path (`callable_body` → `eval_lambda_call`) and the
/// invoke-object path bind against the caller frame, ignoring the function
/// object's env cell — that is the capturing-closure work tracked in
/// bliss-jtc.23.3. Designators this can't resolve (e.g. `#'car`, a builtin) are
/// returned unchanged.
fn coerce_installed_function(env: &Env, name_sym: BlissVal, val: BlissVal) -> BlissVal {
    if bliss_rt::function::is_interpreted_function(val) {
        return val;
    }
    if val.is_symbol() {
        // `#'foo` / a symbol function designator: install foo's current global
        // definition, not a by-name alias.
        return global_fn(&sym_name(val)).unwrap_or(val);
    }
    if val.is_cons() {
        let (h, t) = cp(val);
        if h.is_symbol() && sym_name(h) == "BLISS::CLOSURE" && t.is_fixnum() {
            let id = t.as_fixnum() as u64;
            let closure = { env.closures.borrow().get(&id).cloned() };
            if let Some(closure) = closure {
                // params_form/body are bare BlissVals; alloc_interpreted roots
                // them before it can allocate, and nothing allocates between the
                // clone above and this call, so they cannot go stale.
                return bliss_rt::function::alloc_interpreted(
                    closure.params_form,
                    closure.body,
                    NIL,
                    name_sym,
                );
            }
        }
    }
    val
}

/// Resolve a function designator to its tiered function object (FnMeta), for the
/// bliss-jtc.10 profiling-introspection builtins. Accepts an interpreted-function
/// object directly (`#'foo`) or a symbol naming a global function (`'foo`).
/// Returns `None` for builtins, generics, macros, and lexical functions, which
/// carry no tiered FnMeta record.
fn resolve_tiered_fn(val: BlissVal) -> Option<BlissVal> {
    if bliss_rt::function::is_interpreted_function(val) {
        return Some(val);
    }
    if val.is_symbol() {
        return global_fn(&sym_name(val));
    }
    None
}

/// True if `name` names a function — lexically (FLET/LABELS or `(setf f)` in
/// `Env.funs`) or globally (a bound function cell).
fn fn_bound(env: &Env, name: &str) -> bool {
    env.funs.borrow().contains_key(name) || global_fn(name).is_some()
}

/// Resolve `name` to a callable `(params_form, body)` — lexical `Env.funs` first,
/// then the global function cell. Records an invocation on the function object
/// (FnMeta invoke counter) when resolved globally (bliss-jtc.6.8).
/// Canonical name-map key for a function name form. A `(SETF place)` name (from
/// `(defun (setf place) …)`) maps to the string `"(SETF PLACE)"`; a plain symbol
/// maps to its symbol name. `SETF` builds the same key to find the writer
/// function for a `(setf (place …) v)` form (CLHS 5.1.2.9).
fn function_name_key(name_form: BlissVal) -> String {
    if name_form.is_cons() {
        let (head, tail) = cp(name_form);
        if head.is_symbol() && symbol_bare_name(&sym_name(head)) == "SETF" && tail.is_cons() {
            return format!("(SETF {})", sym_name(cp(tail).0));
        }
    }
    sym_name(name_form)
}

/// The interned-symbol name under which a source-free `(defun (setf place) …)`
/// writer is installed and looked up. `place_name` is `sym_name(place)`; both
/// the compile-file installer and the SETF store path compute it the same way,
/// so the same symbol identity is used at install and at call. Colons are
/// sanitized so the whole thing reads back as one symbol in BLISS-INTERNAL.
pub(super) fn setf_writer_symbol_name(place_name: &str) -> String {
    format!("BLISS-INTERNAL::%SETF-WRITER-{}", place_name.replace(':', "."))
}

/// Whether `place_name` (an accessor's `sym_name`) has a user `(defun (setf
/// place) …)` writer known at compile time (registered as a global setf writer).
/// Used by the portable SETF lowering to recognize a user setf-function place.
pub(super) fn env_has_setf_writer(place_name: &str) -> bool {
    let key = format!("(SETF {place_name})");
    if GLOBAL_SETF_FNS.with(|m| m.borrow().contains_key(&key)) {
        return true;
    }
    // A `(defun (setf place) …)` writer loaded from a `.bfasl` installs its
    // function on the mangled `%SETF-WRITER-place` symbol rather than in
    // GLOBAL_SETF_FNS (which only records source-evaluated writers). Recognise
    // that case too, so a place whose writer was defined in an earlier
    // source-free unit (e.g. alexandria's `(setf lastcar)` from lists.lisp) is
    // still lowered to the portable writer call. Mirrors the acceptance test in
    // the tree-walker's SETF `other =>` branch.
    resolve_sym(&setf_writer_symbol_name(place_name))
        .map(|s| bytecode::is_registered(s.as_symbol_index()))
        .unwrap_or(false)
}

fn callable_body(env: &Env, name: &str) -> Option<(BlissVal, BlissVal)> {
    if let Some(fdef) = env.funs.borrow().get(name) {
        return Some((fdef.params_form, fdef.body));
    }
    // Global `(setf place)` writers registered by a top-level defun (any file).
    if let Some(pb) = GLOBAL_SETF_FNS.with(|m| {
        m.borrow()
            .get(name)
            .map(|fdef| (fdef.params_form, fdef.body))
    }) {
        return Some(pb);
    }
    let f = global_fn(name)?;
    bliss_rt::function::record_invocation(f);
    Some((
        bliss_rt::function::lambda_list(f),
        bliss_rt::function::body(f),
    ))
}

/// True if `val` is a keyword symbol (name in the KEYWORD package). Used to
/// tell an optional positional stream argument apart from &key start/end.
fn is_keyword_arg(val: BlissVal) -> bool {
    val.is_symbol() && sym_name(val).starts_with("KEYWORD:")
}

/// Parse trailing (already-evaluated) `:start`/`:end` keyword pairs for a bounded
/// sequence op, clamped to `[0, len]` with `start <= end`. Defaults: start 0,
/// end len.
fn read_start_end_keys(kv: &[BlissVal], len: usize) -> (usize, usize) {
    let mut start = 0usize;
    let mut end = len;
    let mut i = 0;
    while i + 1 < kv.len() {
        match symbol_bare_name(&sym_name(kv[i])).as_str() {
            "START" if kv[i + 1].is_fixnum() => start = kv[i + 1].as_fixnum() as usize,
            "END" if kv[i + 1].is_fixnum() => end = kv[i + 1].as_fixnum() as usize,
            _ => {}
        }
        i += 2;
    }
    let start = start.min(len);
    let end = end.min(len).max(start);
    (start, end)
}

/// Destructively set element `i` of a mutable sequence, dispatching on strings
/// (character storage via `string_set_char`) vs vectors (`set_elt`).
fn seq_set_elt(seq: BlissVal, i: usize, val: BlissVal) -> Result<(), BlissError> {
    if is_string_value(seq) {
        bliss_stdlib::string_set_char(seq, i, val)?;
        Ok(())
    } else {
        bliss_stdlib::set_elt(seq, i, val)
    }
}

// ── Closure representation ───────────────────────────────────────
#[derive(Clone)]
struct Closure {
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Rc<RefCell<EnvFrame>>,
}

#[derive(Clone, Copy)]
enum EvalContext {
    Repl,
    Eval,
    Load,
    CompileFile,
}

// ── Environment for variable/function bindings ───────────────────
// Uses Rc for shared global definitions (classes, methods, closures). Lexical
// function, macro, and symbol-macro maps retain copy-on-write isolation, while
// RefCell makes their BlissVal slots legal relocation targets for the moving
// collector.
#[derive(Clone)]
struct Env {
    frame: Rc<RefCell<EnvFrame>>,
    funs: Rc<RefCell<HashMap<String, FunDef>>>,
    macros: Rc<RefCell<HashMap<String, MacroDef>>>,
    /// User SETF-expanders (DEFINE-SETF-EXPANDER / DEFSETF), keyed by access-fn
    /// name. Shared and mutated in place (like `closures`) so a definition made
    /// inside a child env — e.g. a MACROLET body or a macro expansion — is
    /// visible globally, matching how DEFUN installs into the symbol cell.
    setf_expanders: Rc<RefCell<HashMap<String, SetfExpander>>>,
    symbol_macros: Rc<RefCell<HashMap<u32, BlissVal>>>,
    // These four are GLOBAL definitions (packages, classes, generic functions,
    // methods): shared and mutated in place so a definition made inside a child
    // Env (a FLET/MACROLET body, as when ASDF loads a system's files) is visible
    // everywhere, matching CL semantics (bliss-lb6.22). Only `funs`/`macros` are
    // genuinely lexical (FLET/MACROLET locals) and are cloned for child Envs.
    classes: Rc<RefCell<HashMap<String, ClassDef>>>,
    generics: Rc<RefCell<HashMap<String, GenericDef>>>,
    methods: Rc<RefCell<HashMap<String, Vec<MethodDef>>>>,
    current_package: String,
    sandbox: bool,
    restarts: Vec<RestartEntry>,
    handlers: Vec<HandlerCluster>,
    /// Multiple values from last (values ...) or (floor ...) call
    mv: Vec<BlissVal>,
    mv_active: bool,
    /// Closures stored by name or lambda id
    closures: Rc<RefCell<HashMap<u64, Closure>>>,
    block_stack: Vec<(String, String)>,
    catch_stack: Vec<(String, String)>,
    /// Tags visible for GO: (tag-name, tagbody-token)
    tag_stack: Vec<(String, String)>,
    method_context: Vec<MethodContext>,
    eval_context: EvalContext,
}

#[derive(Clone, Default)]
struct EnvFrame {
    vars: HashMap<String, BlissVal>,
    symbol_vars: HashMap<u32, BlissVal>,
    parent: Option<Rc<RefCell<EnvFrame>>>,
}

/// An owned, thread-safe (`Arc`-based) deep-copy snapshot of a lexical
/// [`EnvFrame`] chain.
///
/// The live evaluator represents lexical scopes as `Rc<RefCell<EnvFrame>>`,
/// which closures, restarts, and the ordinary macro-expansion path
/// ([`expand_macro`]) all *share* by `Rc::clone` (see bliss-gd4). This frozen
/// form exists solely for the compiler-macro / macroexpand-environment registry
/// bridge: `bliss-compiler` stores macro and compiler-macro expanders as
/// `Arc<dyn Fn(..) + Send + Sync + 'static>` in a *global* table
/// ([`macroexpand::CompilerMacroFn`] / `MacroFn`) that outlives the defining
/// `Env`. Such a closure cannot capture an `Rc<RefCell<EnvFrame>>` — it is
/// neither `Send`/`Sync` nor `'static` against a transient `Env`, and would
/// dangle once that `Env` is gone. So at definition time we `freeze` the frame
/// into this immutable `Arc` structure, and at expansion time `thaw` it into a
/// throwaway `Env` to recover the lexical variables the expander was defined in.
/// This copy is therefore load-bearing (a lifetime/`Send` requirement, not the
/// gd4 aliasing bug) and cannot be replaced by frame sharing while the registry
/// stays in a separate crate behind `Send + Sync` bounds. See bliss-9sf.
struct FrozenEnvFrame {
    vars: Mutex<HashMap<String, BlissVal>>,
    symbol_vars: Mutex<HashMap<u32, BlissVal>>,
    parent: Option<Arc<FrozenEnvFrame>>,
}

struct FrozenMacroCapture {
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Arc<FrozenEnvFrame>,
    funs: HashMap<String, FunDef>,
    classes: HashMap<String, ClassDef>,
    methods: HashMap<String, Vec<MethodDef>>,
    symbol_macros: HashMap<u32, BlissVal>,
}

#[derive(Clone)]
struct FunDef {
    params: Vec<String>,
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
}

#[derive(Clone)]
struct MacroDef {
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Rc<RefCell<EnvFrame>>,
    /// Source-free BFASL macros execute this pre-lowered expander. Source and
    /// lexical MACROLET definitions retain the ordinary body representation.
    bytecode: Option<Rc<RefCell<bliss_rt::bytecode::BytecodeFunction>>>,
}

/// A registered SETF-expander.
#[derive(Clone)]
enum SetfExpander {
    /// DEFINE-SETF-EXPANDER / long-form DEFSETF: a macro-like function of the
    /// place's subforms returning the five setf-expansion values.
    Expander(MacroDef),
    /// Short-form DEFSETF `(defsetf access-fn update-fn)`: store the new value
    /// via `(update-fn arg… new)`.
    ShortUpdate(BlissVal),
}

#[allow(dead_code)]
#[derive(Clone)]
struct ClassDef {
    name: String,
    supers: Vec<String>,
    slots: Vec<SlotDef>,
    class_slot_values: Arc<Mutex<HashMap<String, Option<BlissVal>>>>,
}

#[derive(Clone)]
struct SlotDef {
    name: String,
    /// All `:initarg` names declared for this slot (a slot may declare several,
    /// e.g. both `:licence` and `:license`). Bare names, no leading colon.
    initargs: Vec<String>,
    accessor: Option<String>,
    readers: Vec<String>,
    writers: Vec<String>,
    initform: Option<BlissVal>,
    allocation: SlotAllocation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotAllocation {
    Instance,
    Class,
}

#[derive(Clone)]
struct MethodDef {
    method_id: BlissVal,
    /// Specializers for the required parameters only (parameters before any
    /// lambda-list keyword). Their count is the number of required parameters.
    specializers: Vec<MethodSpecializer>,
    /// The method's ordinary lambda list with specializers stripped, so it can
    /// be bound with the same binder as functions (handles &optional/&rest/&key).
    lambda_list: BlissVal,
    qualifier: bliss_stdlib::MethodQualifier,
    body: BlissVal,
}

#[derive(Clone)]
struct GenericDef {
    generic_function: BlissVal,
    combination: bliss_stdlib::MethodCombinationType,
}

#[derive(Clone)]
enum MethodSpecializer {
    Any,
    Class(String),
    Eql(BlissVal),
}

#[derive(Clone)]
struct MethodContext {
    args: Vec<BlissVal>,
    next: NextMethod,
}

#[derive(Clone)]
enum NextMethod {
    Standard {
        around: Vec<MethodDef>,
        before: Vec<MethodDef>,
        primary: Vec<MethodDef>,
        after: Vec<MethodDef>,
    },
    Primary {
        primary: Vec<MethodDef>,
    },
}

#[derive(Clone)]
struct RestartEntry {
    name: String,
    function: RestartFunction,
    interactive_function: Option<RestartFunction>,
    test_function: Option<RestartFunction>,
    unwind_on_invoke: bool,
}

#[derive(Clone)]
enum RestartFunction {
    FunctionForm {
        function_form: BlissVal,
        // Share the LIVE lexical frame (like closures/macros) so a setf/setq
        // inside the restart function persists to the establishing scope.
        // Frozen snapshots dropped writes (bliss-gd4).
        captured_frame: Rc<RefCell<EnvFrame>>,
    },
    Bytecode {
        function: Rc<RefCell<bliss_rt::bytecode::BytecodeFunction>>,
        captured_frame: Rc<RefCell<EnvFrame>>,
    },
    ContinueNil,
}

/// Sentinel error used for non-local control flow when invoke-restart is called.
#[derive(Debug)]
#[allow(dead_code)]
struct RestartInvoked {
    name: String,
    result: BlissVal,
}

#[derive(Clone)]
struct HandlerEntry {
    type_name: String,
    handler: HandlerImpl,
}

/// The set of handlers established by a single HANDLER-BIND or HANDLER-CASE
/// form — one *cluster*, per §5.4 (D5.10). Grouping matters for the signalling
/// rule R5.94/R5.102: while any handler in a cluster runs, that whole cluster
/// (not merely the one handler) plus all newer clusters are disestablished, so
/// a re-signalled condition is seen only by strictly-older clusters. A flat
/// per-handler stack cannot express "the rest of my own HANDLER-BIND is also
/// hidden". `entries` are held in source order and tried front-to-back, so a
/// HANDLER-CASE's first matching clause wins.
///
/// Stage-4 backing is this `Vec`; the spec's stack-allocated cluster is stage-5
/// work (bliss-2yo). See bliss-uh4.1.
#[derive(Clone)]
struct HandlerCluster {
    entries: Vec<HandlerEntry>,
}

#[derive(Clone)]
enum HandlerImpl {
    Function(BlissVal),
    HandlerCase {
        token: String,
        var_name: Option<String>,
        body: BlissVal,
        // Live lexical frame of the HANDLER-CASE form (shared, not snapshotted).
        captured_frame: Rc<RefCell<EnvFrame>>,
    },
}

thread_local! {
    static CONTROL_VALUES: RefCell<HashMap<String, BlissVal>> = RefCell::new(HashMap::new());
    static CONTROL_COUNTER: RefCell<u64> = const { RefCell::new(0) };
    static MACROEXPAND_ENVIRONMENTS: RefCell<HashMap<u64, MacroexpandEnv>> = RefCell::new(HashMap::new());
    static NEXT_MACROEXPAND_ENVIRONMENT_ID: RefCell<u64> = const { RefCell::new(1) };
    /// Global macro table. A top-level DEFMACRO has global effect (like DEFUN,
    /// which installs into the symbol's function cell), so its definition must
    /// survive the throwaway child Envs used during compile/load — storing it in
    /// a per-Env `macros` map lost it across files (bliss-lb6.22). MACROLET
    /// macros stay lexical in `Env::macros` and shadow these.
    static GLOBAL_MACROS: RefCell<HashMap<String, MacroDef>> = RefCell::new(HashMap::new());

    /// Global `(defun (setf place) …)` writer functions, keyed by the canonical
    /// `"(SETF PLACE)"` string. Like top-level DEFMACRO (above), a top-level
    /// `(setf place)` defun is a *global* definition and must survive the
    /// throwaway child Envs used during compile/load — storing it in a per-Env
    /// `funs` map lost it across files, so `(setf (place …) v)` in a later file
    /// failed with "SETF: unsupported place" even though the writer was defined
    /// (bliss-d0b). FLET-local `(setf place)` writers stay lexical in `Env.funs`.
    static GLOBAL_SETF_FNS: RefCell<HashMap<String, FunDef>> = RefCell::new(HashMap::new());

    /// Names of variables established by DEFCONSTANT — bliss models a constant as
    /// an ordinary global binding, so this set is how CONSTANTP (and library code
    /// like alexandria's DEFINE-CONSTANT, used by babel) can tell a defconstant'd
    /// symbol from a defparameter.
    static CONSTANT_VARS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());

    /// Memoized macro-expander registrations for the bytecode compiler
    /// (bliss-gq5.8). Building a MacroexpandEnv used to re-freeze every macro's
    /// captured frame, allocate a fresh expander closure, and re-parse the
    /// name — for EVERY compiled form. During a large load (asdf/alexandria)
    /// that is O(forms × macros) and dominated compile time. Keyed by macro
    /// name, the value carries the macro's identity (params/body BlissVal bits +
    /// captured-frame Rc pointer) so a redefinition re-registers, and caches the
    /// registered handle and resolved symbol so unchanged macros are reused.
    static MACRO_FN_CACHE: RefCell<HashMap<String, MacroFnCacheEntry>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone, Copy)]
struct MacroFnCacheEntry {
    params_bits: u64,
    body_bits: u64,
    frame_ptr: usize,
    bytecode_ptr: usize,
    handle: BlissVal,
    symbol: BlissVal,
}

/// Register a global (top-level DEFMACRO) macro.
fn global_macro_insert(name: String, def: MacroDef) {
    GLOBAL_MACROS.with(|m| m.borrow_mut().insert(name, def));
}

fn install_loaded_macro(
    name: BlissVal,
    function: Rc<bliss_rt::bytecode::BytecodeFunction>,
    env: &Env,
) {
    install_evaluator_global_root_scanner();
    global_macro_insert(
        sym_name(name),
        MacroDef {
            params_form: NIL,
            body: NIL,
            captured_frame: Rc::clone(&env.frame),
            bytecode: Some(Rc::new(RefCell::new((*function).clone()))),
        },
    );
}

/// Install a source-free `(define-setf-expander place …)` writer loaded from a
/// `.bfasl`: a setf expander whose body is the compiled bytecode function.
fn install_loaded_setf_expander(
    name: BlissVal,
    function: Rc<bliss_rt::bytecode::BytecodeFunction>,
    env: &Env,
) {
    install_evaluator_global_root_scanner();
    env.setf_expanders.borrow_mut().insert(
        sym_name(name),
        SetfExpander::Expander(MacroDef {
            params_form: NIL,
            body: NIL,
            captured_frame: Rc::clone(&env.frame),
            bytecode: Some(Rc::new(RefCell::new((*function).clone()))),
        }),
    );
}

fn install_loaded_compiler_macro(
    name: BlissVal,
    function: Rc<bliss_rt::bytecode::BytecodeFunction>,
) {
    install_evaluator_global_root_scanner();
    let function = Arc::new(Mutex::new((*function).clone()));
    LOADED_COMPILER_MACRO_FUNCTIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(Arc::downgrade(&function));
    compiler_macroexpand::define_compiler_macro(
        name,
        Arc::new(move |form, _macro_env| {
            let (_, args) = cp(form);
            let function = function
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let mut env = Env::new_for_macro_expansion(false);
            bytecode::run_macro(Rc::new(function), &list_to_vec(args), &mut env)
        }),
    );
}

/// Remove a global macro (FMAKUNBOUND / redefinition as a function).
fn global_macro_remove(name: &str) {
    GLOBAL_MACROS.with(|m| {
        m.borrow_mut().remove(name);
    });
}

/// Look up a macro visible in `env`: a lexical MACROLET macro shadows a global
/// one; a package-qualified name also matches by its bare leaf name.
fn lookup_macro(env: &Env, name: &str) -> Option<MacroDef> {
    if let Some(def) = env.macros.borrow().get(name).cloned() {
        return Some(def);
    }
    let leaf = symbol_leaf_name(name);
    if leaf != name {
        if let Some(def) = env.macros.borrow().get(leaf).cloned() {
            return Some(def);
        }
    }
    GLOBAL_MACROS.with(|m| {
        let g = m.borrow();
        g.get(name).cloned().or_else(|| {
            if leaf != name {
                g.get(leaf).cloned()
            } else {
                None
            }
        })
    })
}

/// True if `name` names a macro visible in `env` (lexical or global).
fn macro_defined(env: &Env, name: &str) -> bool {
    if env.macros.borrow().contains_key(name) {
        return true;
    }
    let leaf = symbol_leaf_name(name);
    if leaf != name && env.macros.borrow().contains_key(leaf) {
        return true;
    }
    GLOBAL_MACROS.with(|m| {
        let g = m.borrow();
        g.contains_key(name) || (leaf != name && g.contains_key(leaf))
    })
}

fn next_control_token(prefix: &str) -> String {
    let id = CONTROL_COUNTER.with(|counter| {
        let id = *counter.borrow();
        *counter.borrow_mut() = id + 1;
        id
    });
    format!("{prefix}:{id}")
}

fn next_macro_function_handle() -> BlissVal {
    // Draw from the compiler's single registry-key counter so global-macro
    // handles never collide with macrolet-local expander keys in the shared
    // MACRO_FUNCTION_REGISTRY (bliss-6b2). Two independent fixnum counters both
    // starting at 1 would mint the same key and one expander would clobber the
    // other — e.g. an asdf macrolet's `check` overwriting global `defvar`.
    compiler_macroexpand::next_registered_macro_key()
}

fn macroexpand_environment_handle_symbol() -> BlissVal {
    resolve_sym("BLISS::MACROEXPAND-ENV").unwrap_or(NIL)
}

fn store_macroexpand_environment(env: MacroexpandEnv) -> BlissVal {
    let id = NEXT_MACROEXPAND_ENVIRONMENT_ID.with(|counter| {
        let id = *counter.borrow();
        *counter.borrow_mut() = id + 1;
        id
    });
    MACROEXPAND_ENVIRONMENTS.with(|envs| {
        envs.borrow_mut().insert(id, env);
    });
    arena_cons(
        macroexpand_environment_handle_symbol(),
        BlissVal::from_fixnum(id as i64),
    )
}

fn load_macroexpand_environment(handle: BlissVal) -> Option<MacroexpandEnv> {
    if !handle.is_cons() {
        return None;
    }
    let (tag, payload) = cp(handle);
    if tag != macroexpand_environment_handle_symbol() || !payload.is_fixnum() {
        return None;
    }
    let id = payload.as_fixnum();
    if id <= 0 {
        return None;
    }
    MACROEXPAND_ENVIRONMENTS.with(|envs| envs.borrow().get(&(id as u64)).cloned())
}

fn store_control_value(token: &str, value: BlissVal) {
    CONTROL_VALUES.with(|values| {
        values.borrow_mut().insert(token.to_string(), value);
    });
}

/// Deep-copy a live lexical frame chain into an owned, `Send + Sync` snapshot.
/// Only the global macro/compiler-macro registry bridge needs this; see
/// [`FrozenEnvFrame`].
fn freeze_env_frame(frame: &Rc<RefCell<EnvFrame>>) -> Arc<FrozenEnvFrame> {
    let borrowed = frame.borrow();
    Arc::new(FrozenEnvFrame {
        vars: Mutex::new(borrowed.vars.clone()),
        symbol_vars: Mutex::new(borrowed.symbol_vars.clone()),
        parent: borrowed.parent.as_ref().map(freeze_env_frame),
    })
}

fn thaw_env_frame(frame: &Arc<FrozenEnvFrame>) -> Rc<RefCell<EnvFrame>> {
    Rc::new(RefCell::new(EnvFrame {
        vars: frame
            .vars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        symbol_vars: frame
            .symbol_vars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        parent: frame.parent.as_ref().map(thaw_env_frame),
    }))
}

fn take_control_value(token: &str) -> BlissVal {
    CONTROL_VALUES.with(|values| values.borrow_mut().remove(token).unwrap_or(NIL))
}

fn handler_case_token(error: &BlissError) -> Option<String> {
    let BlissError::Internal(message) = error else {
        return None;
    };
    message
        .strip_prefix("__HANDLER_CASE__:")
        .map(ToString::to_string)
}

fn restart_invoked_name(error: &BlissError) -> Option<String> {
    let BlissError::Internal(message) = error else {
        return None;
    };
    message
        .strip_prefix("__RESTART_INVOKED__:")
        .map(ToString::to_string)
}

fn make_simple_condition(
    class_name: &str,
    format_control: BlissVal,
    format_args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let class = ensure_condition_class_registered(env, class_name)?;
    bliss_stdlib::make_instance(
        class,
        &[
            resolve_sym("FORMAT-CONTROL").unwrap_or(NIL),
            format_control,
            resolve_sym("FORMAT-ARGUMENTS").unwrap_or(NIL),
            vec_to_list(format_args),
        ],
    )
}

fn make_simple_error_condition(message: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    make_simple_condition("SIMPLE-ERROR", message, &[], env)
}

/// A human-readable report for a condition instance — what `~a` and an uncaught
/// error should show, rather than the bare `#<TYPE-ERROR>` / type name. Uses the
/// FORMAT-CONTROL/ARGUMENTS of simple conditions, and the standard slots of the
/// built-in condition types. Returns `None` if `cond` is not a condition instance.
fn condition_report_string(env: &Env, cond: BlissVal) -> Option<String> {
    if !bliss_stdlib::is_instance(cond) {
        return None;
    }
    let names = instance_class_hierarchy_names(cond)?;
    let read = |slot: &str| -> Option<BlissVal> {
        resolve_sym(slot)
            .and_then(|s| read_slot_value(cond, s, env).ok())
            .filter(|v| !v.is_nil())
    };
    // Simple conditions carry a format control + arguments.
    if let Some(fc) = read("FORMAT-CONTROL") {
        if is_string_value(fc) {
            let args = read("FORMAT-ARGUMENTS")
                .map(list_to_vec)
                .unwrap_or_default();
            let control = val_as_str(fc);
            return Some(format_control_message(&control, &args).unwrap_or(control));
        }
    }
    let is = |t: &str| names.iter().any(|n| n == t);
    if is("TYPE-ERROR") {
        let datum = read("DATUM").map(format_val).unwrap_or_else(|| "?".into());
        let expected = read("EXPECTED-TYPE")
            .map(format_val)
            .unwrap_or_else(|| "?".into());
        return Some(format!("The value {datum} is not of type {expected}."));
    }
    if is("UNBOUND-VARIABLE") {
        let name = read("NAME").map(format_val).unwrap_or_default();
        return Some(format!("The variable {name} is unbound."));
    }
    if is("UNDEFINED-FUNCTION") {
        let name = read("NAME").map(format_val).unwrap_or_default();
        return Some(format!("The function {name} is undefined."));
    }
    if is("PACKAGE-ERROR") {
        let pkg = read("PACKAGE").map(format_val).unwrap_or_default();
        return Some(format!(
            "Package error{}.",
            if pkg.is_empty() {
                String::new()
            } else {
                format!(": {pkg}")
            }
        ));
    }
    // Fall back to the most specific class name.
    names.first().cloned()
}

fn make_simple_warning_condition(
    format_control: BlissVal,
    format_args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    make_simple_condition("SIMPLE-WARNING", format_control, format_args, env)
}

/// Convert a raw evaluator error into the CL condition it denotes, so that
/// HANDLER-CASE (and hence IGNORE-ERRORS) catches ordinary runtime errors —
/// TYPE-ERROR, UNBOUND-VARIABLE, UNDEFINED-FUNCTION, arithmetic, etc. — and not
/// only conditions raised explicitly through SIGNAL/ERROR. Returns `Ok(None)`
/// for errors that are not conditions: `Internal` (which also carries the
/// control-flow tokens used by BLOCK/RETURN-FROM, HANDLER-CASE, and restarts)
/// and `Shutdown`, so those keep propagating unchanged.
fn bliss_error_to_condition(
    env: &mut Env,
    error: &BlissError,
) -> Result<Option<BlissVal>, BlissError> {
    let name_sym = resolve_sym("NAME").unwrap_or(NIL);
    let condition = match error {
        BlissError::TypeError { datum, expected } => build_condition_instance(
            env,
            "TYPE-ERROR",
            &[
                resolve_sym("DATUM").unwrap_or(NIL),
                *datum,
                resolve_sym("EXPECTED-TYPE").unwrap_or(NIL),
                arena_str(expected),
            ],
        )?,
        BlissError::UnboundVariable(sym) => {
            build_condition_instance(env, "UNBOUND-VARIABLE", &[name_sym, *sym])?
        }
        BlissError::UndefinedFunction(sym) => {
            build_condition_instance(env, "UNDEFINED-FUNCTION", &[name_sym, *sym])?
        }
        BlissError::ArithmeticError(msg) => {
            let type_name = if msg.contains("division") {
                "DIVISION-BY-ZERO"
            } else {
                "ARITHMETIC-ERROR"
            };
            build_condition_instance(env, type_name, &[])?
        }
        BlissError::PackageError(_) => build_condition_instance(env, "PACKAGE-ERROR", &[])?,
        BlissError::StreamError(_) => build_condition_instance(env, "STREAM-ERROR", &[])?,
        BlissError::FileError(_) => build_condition_instance(env, "FILE-ERROR", &[])?,
        BlissError::Oom => {
            // Heap is exhausted: the storage-failure path must not allocate, so
            // hand back a STORAGE-CONDITION preallocated at startup rather than
            // building a fresh instance (R5.110, bliss-uh4.2).
            bliss_stdlib::acquire_preallocated_storage_condition()?
        }
        BlissError::StackOverflow(_) => {
            // Only the control stack overflowed — the heap is fine — so build a
            // CLI-native STORAGE-CONDITION here (allocation is safe, and unlike a
            // preallocated stdlib-pool instance it is recognised by the CLI's
            // condition type matching). R2.20; see bliss-nmq.
            build_condition_instance(env, "STORAGE-CONDITION", &[])?
        }
        BlissError::SandboxViolation(msg) => make_simple_error_condition(arena_str(msg), env)?,
        BlissError::ProgramError(_) => build_condition_instance(env, "PROGRAM-ERROR", &[])?,
        _ => return Ok(None),
    };
    Ok(Some(condition))
}

/// Construct a condition instance of `type_name` from already-evaluated initarg
/// pairs (`key value key value …`). Explicit initargs and `:default-initargs`
/// are applied first, then slot `:initform`s fill any remaining unbound slots —
/// so conditions honour the same MAKE-INSTANCE protocol as ordinary CLOS
/// objects (R5.80). Shared by MAKE-CONDITION and the ERROR/SIGNAL/WARN/CERROR
/// condition-designator coercion.
fn build_condition_instance(
    env: &mut Env,
    type_name: &str,
    initarg_pairs: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    build_condition_instance_impl(env, type_name, initarg_pairs, false)
}

/// Build a condition instance of `type_name` with the given initargs. When
/// `pinned_gc` is set the instance is allocated in the GC heap and pinned — used
/// to seed the immortal STORAGE-CONDITION pool with instances whose class the CLI
/// recognizes (bliss-4v8 + bliss-5mf); otherwise it is a normal std::alloc CLOS
/// instance like every other condition the interpreter builds.
fn build_condition_instance_impl(
    env: &mut Env,
    type_name: &str,
    initarg_pairs: &[BlissVal],
    pinned_gc: bool,
) -> Result<BlissVal, BlissError> {
    let class = ensure_condition_class_registered(env, type_name)?;
    let slot_specs = condition_slot_specs(env, type_name);
    let mut initargs = Vec::new();
    let mut seen_initargs = Vec::new();
    let mut i = 0;
    while i + 1 < initarg_pairs.len() {
        let key = initarg_pairs[i];
        let val = initarg_pairs[i + 1];
        let key_name = symbol_bare_name(&sym_name(key));
        let slot_name = slot_specs
            .iter()
            .find(|(_, initarg)| initarg == &key_name)
            .map(|(slot_name, _)| slot_name.clone())
            .unwrap_or_else(|| key_name.clone());
        seen_initargs.push(key_name);
        initargs.push(resolve_sym(&slot_name).unwrap_or(NIL));
        initargs.push(val);
        i += 2;
    }
    for (initarg_name, default_value) in condition_default_initargs(env, type_name) {
        if seen_initargs.iter().any(|seen| seen == &initarg_name) {
            continue;
        }
        if let Some((slot_name, _)) = slot_specs
            .iter()
            .find(|(_, initarg)| initarg == &initarg_name)
        {
            initargs.push(resolve_sym(slot_name).unwrap_or(NIL));
            initargs.push(default_value);
        }
    }
    let instance = if pinned_gc {
        // GC-heap + pinned so the pooled instance is immortal and non-moving
        // (D5.13), while still carrying the CLI-recognized condition class.
        let inst = bliss_stdlib::clos::allocate_instance_pinned_gc(class)?;
        bliss_stdlib::clos::initialize_instance(inst, &initargs)?;
        inst
    } else {
        bliss_stdlib::make_instance(class, &initargs)?
    };
    let explicit_slots: Vec<String> = initargs
        .chunks_exact(2)
        .map(|pair| symbol_bare_name(&sym_name(pair[0])))
        .collect();
    let cond_class_name = class_name_for_instance_class(class);
    apply_class_initforms(instance, &cond_class_name, env, None, &explicit_slots)?;
    Ok(instance)
}

/// Coerce a condition designator to a condition instance per ANSI CL. A symbol
/// datum names a condition type and is built via `build_condition_instance` with
/// `args` as initargs; an existing instance is returned unchanged; anything else
/// (typically a format-control string) yields `None` so the caller can apply its
/// own default (e.g. SIMPLE-ERROR).
fn coerce_condition_designator(
    env: &mut Env,
    datum: BlissVal,
    args: &[BlissVal],
) -> Result<Option<BlissVal>, BlissError> {
    if bliss_stdlib::is_instance(datum) {
        return Ok(Some(datum));
    }
    if datum.is_symbol() {
        let type_name = sym_name(datum);
        return Ok(Some(build_condition_instance(env, &type_name, args)?));
    }
    Ok(None)
}

fn eval_handler_impl(
    handler: &HandlerImpl,
    condition: BlissVal,
    env: &mut Env,
) -> Result<(), BlissError> {
    match handler {
        HandlerImpl::Function(function) => {
            let _ = apply_function(*function, &[condition], env)?;
            Ok(())
        }
        HandlerImpl::HandlerCase {
            token,
            var_name: _,
            body: _,
            captured_frame: _,
        } => {
            store_control_value(token, condition);
            Err(BlissError::Internal(format!("__HANDLER_CASE__:{token}")))
        }
    }
}

fn signal_condition_object(condition: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    maybe_break_on_signals(condition, env)?;
    // Visit clusters newest-first; within a cluster try its handlers in source
    // order (first HANDLER-CASE clause wins). SIGNAL returns NIL if no handler
    // transfers control (R5.102).
    for ci in (0..env.handlers.len()).rev() {
        run_handler_cluster(env, condition, ci)?;
    }
    Ok(NIL)
}

/// Run the handlers of the single cluster at `ci` against `condition`, in source
/// order. While a handler runs, its whole cluster and every newer cluster are
/// disestablished (R5.94/R5.102), so a condition it re-signals is seen only by
/// strictly-older clusters — never itself or a sibling handler of the same
/// HANDLER-BIND. `Ok(())` means every matching handler declined (returned
/// normally); `Err` means a handler transferred control (a HANDLER-CASE token,
/// INVOKE-RESTART, or a non-local exit) and must propagate.
///
/// Disestablishment is done by moving the tail `env.handlers[ci..]` out with
/// `split_off` (no deep clone of the whole stack) and appending it back
/// afterwards; only the current cluster is cloned so it can be iterated while
/// `env.handlers` is mutated.
fn run_handler_cluster(env: &mut Env, condition: BlissVal, ci: usize) -> Result<(), BlissError> {
    if ci >= env.handlers.len() {
        return Ok(());
    }
    let cluster = env.handlers[ci].clone();
    for entry in &cluster.entries {
        if condition_matches_handler(env, condition, &entry.type_name) {
            let tail = env.handlers.split_off(ci);
            let result = eval_handler_impl(&entry.handler, condition, env);
            env.handlers.extend(tail);
            result?;
        }
    }
    Ok(())
}

/// Run the handlers of the HANDLER-BIND cluster at index `ci` for a raw evaluator
/// error that bypassed `signal_condition_object`. As the error unwinds, each
/// enclosing HANDLER-BIND runs its own cluster exactly once (see
/// `eval_handler_bind`). Same semantics as `run_handler_cluster`.
fn run_handler_bind_handlers(
    env: &mut Env,
    condition: BlissVal,
    ci: usize,
) -> Result<(), BlissError> {
    run_handler_cluster(env, condition, ci)
}

/// Signal `condition` through the active handler stack. If a handler transfers
/// control (unwinds), the resulting error propagates so HANDLER-CASE can run its
/// clause; otherwise a terminal error carrying `msg` is returned — mirroring
/// ANSI `ERROR`, which never returns normally. This is how internal CLOS
/// failures (unbound slot, no applicable method, no next method) surface as real
/// catchable condition objects rather than opaque runtime errors.
fn signal_and_raise(env: &mut Env, condition: BlissVal, msg: String) -> BlissError {
    match signal_condition_object(condition, env) {
        Ok(_) => BlissError::Internal(format!("ERROR: {}", msg)),
        Err(error) => error,
    }
}

fn condition_matches_type_spec(env: &Env, condition: BlissVal, type_spec: BlissVal) -> bool {
    if type_spec.is_nil() {
        return false;
    }
    if type_spec == T {
        return true;
    }
    if type_spec.is_symbol() {
        return condition_matches_handler(env, condition, &sym_name(type_spec));
    }
    let Ok(class) = resolve_class_metaobject(env, type_spec) else {
        return false;
    };
    bliss_stdlib::compute_class_precedence_list(bliss_stdlib::class_of(condition))
        .map(|cpl| cpl.into_iter().any(|entry| entry == class))
        .unwrap_or(false)
}

fn maybe_break_on_signals(condition: BlissVal, env: &mut Env) -> Result<(), BlissError> {
    let Some(type_spec) = env.lookup_var("*BREAK-ON-SIGNALS*") else {
        return Ok(());
    };
    if !condition_matches_type_spec(env, condition, type_spec) {
        return Ok(());
    }

    env.set_var("*BREAK-ON-SIGNALS*", NIL);
    let debugger_result = bliss_stdlib::invoke_debugger(condition);
    env.set_var("*BREAK-ON-SIGNALS*", type_spec);
    match debugger_result {
        Ok(()) | Err(_) => Ok(()),
    }
}

fn method_combination_from_name(name: &str) -> Option<bliss_stdlib::MethodCombinationType> {
    match symbol_bare_name(name).as_str() {
        "STANDARD" => Some(bliss_stdlib::MethodCombinationType::Standard),
        "+" | "PLUS" => Some(bliss_stdlib::MethodCombinationType::Plus),
        "AND" => Some(bliss_stdlib::MethodCombinationType::And),
        "OR" => Some(bliss_stdlib::MethodCombinationType::Or),
        "LIST" => Some(bliss_stdlib::MethodCombinationType::List),
        "APPEND" => Some(bliss_stdlib::MethodCombinationType::Append),
        "NCONC" => Some(bliss_stdlib::MethodCombinationType::Nconc),
        "MIN" => Some(bliss_stdlib::MethodCombinationType::Min),
        "MAX" => Some(bliss_stdlib::MethodCombinationType::Max),
        "PROGN" => Some(bliss_stdlib::MethodCombinationType::Progn),
        _ => None,
    }
}

/// True if `v` is a genuine function object — a real function value
/// (`is_function`) or an interpreter closure, represented as the cons
/// `(BLISS::CLOSURE . id)`. A bare symbol (what `#'foo` yields for a named
/// function) and an ordinary data cons (e.g. `(make-instance c)`) are NOT
/// functions, so `(typep '(make-instance c) 'function)` is correctly NIL and
/// UIOP's ENSURE-FUNCTION etypecase falls through to its CONS clause (bliss-lb6).
fn is_function_value(v: BlissVal) -> bool {
    if v.is_function() {
        return true;
    }
    if v.is_cons() {
        let (h, t) = cp(v);
        return h.is_symbol() && sym_name(h) == "BLISS::CLOSURE" && t.is_fixnum();
    }
    false
}

fn resolve_class_metaobject(env: &Env, class: BlissVal) -> Result<BlissVal, BlissError> {
    if !class.is_symbol() {
        return Ok(class);
    }
    if let Some(found) = bliss_stdlib::find_class(class) {
        return Ok(found);
    }
    let name = sym_name(class);
    if builtin_condition_definition(&name).is_some() {
        return ensure_condition_class_registered(env, &name);
    }
    Ok(class)
}

fn resolve_slot_symbol(class_name: &str, key: BlissVal, env: &Env) -> Result<BlissVal, BlissError> {
    let key_name = symbol_bare_name(&sym_name(key));
    if let Some(slot) = lookup_slot_by_initarg(env, class_name, &key_name) {
        return Ok(resolve_sym(&slot.name).unwrap_or(NIL));
    }
    // An initarg that matches no slot :initarg is still valid if an applicable
    // INITIALIZE-INSTANCE / SHARED-INITIALIZE method declares it as a &key
    // parameter (CLHS 7.1.2) — e.g. babel's CHARACTER-ENCODING consumes
    // :literal-char-code-limit that way. Pass it through as its bare-named symbol
    // rather than erroring: slot-filling skips it (no matching slot) while the
    // aux-method &key binding still receives it by name.
    Ok(resolve_sym(&key_name).unwrap_or(key))
}

fn class_name_for_instance_class(class: BlissVal) -> String {
    if class.is_symbol() {
        return sym_name(class);
    }
    let name = bliss_stdlib::class_name(class);
    if name.is_symbol() {
        sym_name(name)
    } else {
        val_as_str(class)
    }
}

fn lookup_slot_def(env: &Env, class_name: &str, slot_name: &str) -> Option<SlotDef> {
    let class_def = env.classes.borrow().get(class_name).cloned()?;
    if let Some(slot) = class_def.slots.iter().find(|slot| slot.name == slot_name) {
        return Some(slot.clone());
    }
    for super_name in &class_def.supers {
        if let Some(slot) = lookup_slot_def(env, super_name, slot_name) {
            return Some(slot);
        }
    }
    None
}

fn lookup_slot_by_initarg(env: &Env, class_name: &str, initarg: &str) -> Option<SlotDef> {
    let class_def = env.classes.borrow().get(class_name).cloned()?;
    if let Some(slot) = class_def.slots.iter().find(|slot| {
        slot.initargs
            .iter()
            .any(|slot_initarg| slot_initarg == initarg)
            || slot.name == initarg
    }) {
        return Some(slot.clone());
    }
    for super_name in &class_def.supers {
        if let Some(slot) = lookup_slot_by_initarg(env, super_name, initarg) {
            return Some(slot);
        }
    }
    None
}

fn split_initargs_for_class(
    env: &Env,
    class_name: &str,
    initargs: &[BlissVal],
) -> (Vec<BlissVal>, Vec<(String, BlissVal)>) {
    let mut instance_initargs = Vec::new();
    let mut class_initargs = Vec::new();
    let mut i = 0;
    while i + 1 < initargs.len() {
        let slot_sym = initargs[i];
        let value = initargs[i + 1];
        let slot_name = symbol_bare_name(&sym_name(slot_sym));
        if matches!(
            lookup_slot_def(env, class_name, &slot_name).map(|slot| slot.allocation),
            Some(SlotAllocation::Class)
        ) {
            class_initargs.push((slot_name, value));
        } else {
            instance_initargs.push(slot_sym);
            instance_initargs.push(value);
        }
        i += 2;
    }
    (instance_initargs, class_initargs)
}

fn write_class_slot_value(env: &Env, class_name: &str, slot_name: &str, value: Option<BlissVal>) {
    if let Some(class_def) = env.classes.borrow().get(class_name).cloned() {
        class_def
            .class_slot_values
            .lock()
            .unwrap()
            .insert(slot_name.to_string(), value);
    }
}

/// If `slot_name` names a `:allocation :class` slot reachable from `class_name`,
/// return the class that OWNS the shared value cell. A class slot's value lives
/// in its DEFINING class (not the instance's class or a subclass), so an
/// inherited class slot must be resolved to where it was declared. Walks the CLI
/// super graph first; falls back to any class declaring the slot class-allocated,
/// which also papers over CLI/stdlib class-graph divergence for deep multiple
/// inheritance (bliss-lb6.14: ASDF's LOAD-OP inherits DOWNWARD-OPERATION's class
/// slot through three superclasses).
fn class_slot_owner(env: &Env, class_name: &str, slot_name: &str) -> Option<String> {
    fn walk(
        env: &Env,
        class_name: &str,
        slot_name: &str,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        if !seen.insert(class_name.to_string()) {
            return None;
        }
        let cd = env.classes.borrow().get(class_name).cloned()?;
        // Slot names are stored as the full symbol name (possibly package-
        // qualified); compare bare-to-bare since `slot_name` is already bare.
        if let Some(slot) = cd
            .slots
            .iter()
            .find(|s| symbol_bare_name(&s.name) == slot_name)
        {
            return (slot.allocation == SlotAllocation::Class).then(|| class_name.to_string());
        }
        for sup in &cd.supers {
            if let Some(owner) = walk(env, sup, slot_name, seen) {
                return Some(owner);
            }
        }
        None
    }
    let mut seen = HashSet::new();
    walk(env, class_name, slot_name, &mut seen).or_else(|| {
        env.classes.borrow().iter().find_map(|(name, cd)| {
            cd.slots
                .iter()
                .any(|s| {
                    symbol_bare_name(&s.name) == slot_name && s.allocation == SlotAllocation::Class
                })
                .then(|| name.clone())
        })
    })
}

fn read_slot_value(instance: BlissVal, slot: BlissVal, env: &Env) -> Result<BlissVal, BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if class_slot_owner(env, &class_name, &slot_name).is_some() {
        // The shared value is stored in the instance's class under the slot's
        // full (as-declared) name — see instance initialization below.
        let key = sym_name(slot);
        if let Some(class_def) = env.classes.borrow().get(&class_name).cloned() {
            let values = class_def.class_slot_values.lock().unwrap();
            if let Some(Some(value)) = values.get(&key).or_else(|| values.get(&slot_name)) {
                return Ok(*value);
            }
        }
        return Err(BlissError::UnboundVariable(slot));
    }
    // A slot read on a non-instance (e.g. NIL reaching a reader accessor whose
    // receiver turned out empty) yields NIL rather than the uncatchable
    // "not an instance" internal error — ASDF's readers rely on this, e.g.
    // (system-source-file nil) => nil (bliss-lb6.14).
    if !bliss_stdlib::is_instance(instance) {
        return Ok(NIL);
    }
    bliss_stdlib::slot_value(instance, slot)
}

/// Read a slot, signalling a catchable `unbound-slot` condition (R5.71) when the
/// slot is unbound on a genuine instance. The condition carries `:name` (the slot
/// name) and `:instance`, so handlers can inspect it like any CLOS object.
fn slot_value_or_signal(
    instance: BlissVal,
    slot: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    match read_slot_value(instance, slot, env) {
        Ok(value) => Ok(value),
        Err(BlissError::UnboundVariable(_)) if bliss_stdlib::is_instance(instance) => {
            let condition = build_condition_instance(
                env,
                "UNBOUND-SLOT",
                &[
                    resolve_sym("NAME").unwrap_or(NIL),
                    slot,
                    resolve_sym("INSTANCE").unwrap_or(NIL),
                    instance,
                ],
            )?;
            Err(signal_and_raise(
                env,
                condition,
                format!("slot {} is unbound", symbol_bare_name(&sym_name(slot))),
            ))
        }
        Err(error) => Err(error),
    }
}

fn slot_is_bound(instance: BlissVal, slot: BlissVal, env: &Env) -> Result<bool, BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if class_slot_owner(env, &class_name, &slot_name).is_some() {
        let key = sym_name(slot);
        if let Some(class_def) = env.classes.borrow().get(&class_name).cloned() {
            let values = class_def.class_slot_values.lock().unwrap();
            return Ok(matches!(
                values.get(&key).or_else(|| values.get(&slot_name)),
                Some(Some(_))
            ));
        }
    }
    bliss_stdlib::slot_boundp(instance, slot)
}

fn write_slot_value(
    instance: BlissVal,
    slot: BlissVal,
    value: BlissVal,
    env: &Env,
) -> Result<(), BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if class_slot_owner(env, &class_name, &slot_name).is_some() {
        write_class_slot_value(env, &class_name, &sym_name(slot), Some(value));
        return Ok(());
    }
    bliss_stdlib::set_slot_value(instance, slot, value)
}

/// Class precedence names for a class, most-specific-first, via preorder DFS
/// over the `env.classes` superclass links (deduped). Used to compute effective
/// slots so inherited slot definitions participate in instance initialization.
fn class_precedence_names(env: &Env, class_name: &str) -> Vec<String> {
    fn visit(
        env: &Env,
        name: &str,
        order: &mut Vec<String>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if !seen.insert(name.to_string()) {
            return;
        }
        order.push(name.to_string());
        if let Some(class_def) = env.classes.borrow().get(name).cloned() {
            for super_name in &class_def.supers {
                visit(env, super_name, order, seen);
            }
        }
    }
    let mut order = Vec::new();
    let mut seen = std::collections::HashSet::new();
    visit(env, class_name, &mut order, &mut seen);
    order
}

/// Effective slots for a class, most-specific-first, deduped by name. A slot's
/// `:initform` is inherited from the most specific class that supplies one, so a
/// subclass that redeclares a slot without an initform does not shadow an
/// inherited default — matching ANSI effective-slot computation.
fn effective_slots_for_class(env: &Env, class_name: &str) -> Vec<SlotDef> {
    let mut result: Vec<SlotDef> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for cname in class_precedence_names(env, class_name) {
        let Some(class_def) = env.classes.borrow().get(&cname).cloned() else {
            continue;
        };
        for slot in &class_def.slots {
            if let Some(&i) = index.get(&slot.name) {
                if result[i].initform.is_none() && slot.initform.is_some() {
                    result[i].initform = slot.initform;
                }
            } else {
                index.insert(slot.name.clone(), result.len());
                result.push(slot.clone());
            }
        }
    }
    result
}

fn apply_class_initforms(
    instance: BlissVal,
    class_name: &str,
    env: &mut Env,
    eligible_slots: Option<&[String]>,
    explicit_slots: &[String],
) -> Result<(), BlissError> {
    // Walk the full class precedence list so inherited slot initforms are
    // applied, not just those declared on the instance's own class. Class-
    // allocated slot values are stored in the instance class's shared map,
    // consistent with `read_slot_value`/`write_class_slot_value`.
    let effective = effective_slots_for_class(env, class_name);
    let class_slot_values = env
        .classes
        .borrow()
        .get(class_name)
        .map(|class_def| Arc::clone(&class_def.class_slot_values));
    for slot in &effective {
        if explicit_slots.iter().any(|name| name == &slot.name) {
            continue;
        }
        if let Some(eligible) = eligible_slots
            && !eligible.iter().any(|name| name == &slot.name)
        {
            continue;
        }
        let Some(initform) = slot.initform else {
            continue;
        };
        let slot_sym = resolve_sym(&slot.name).unwrap_or(NIL);
        let already_bound = match slot.allocation {
            SlotAllocation::Class => class_slot_values
                .as_ref()
                .map(|values| matches!(values.lock().unwrap().get(&slot.name), Some(Some(_))))
                .unwrap_or(false),
            SlotAllocation::Instance => bliss_stdlib::slot_boundp(instance, slot_sym)?,
        };
        if already_bound {
            continue;
        }
        let value = eval_form(initform, env)?;
        match slot.allocation {
            SlotAllocation::Class => {
                if let Some(values) = class_slot_values.as_ref() {
                    values
                        .lock()
                        .unwrap()
                        .insert(slot.name.clone(), Some(value));
                }
            }
            SlotAllocation::Instance => {
                bliss_stdlib::set_slot_value(instance, slot_sym, value)?;
            }
        }
    }
    Ok(())
}

fn evaluated_initargs(
    class_name: &str,
    init_args: BlissVal,
    env: &mut Env,
) -> Result<Vec<BlissVal>, BlissError> {
    let args_vec = list_to_vec(init_args);
    // Root the accumulated key/value pairs across the loop (bliss-6b2 #2): each
    // later eval_form allocates and can relocate the earlier, Vec-resident values.
    let roots = bliss_rt::ShadowRootScope::new();
    let mut initargs: Vec<bliss_rt::ShadowRoot> = Vec::new();
    let mut i = 0;
    while i + 1 < args_vec.len() {
        let key = eval_form(args_vec[i], env)?;
        let value = eval_form(args_vec[i + 1], env)?;
        initargs.push(roots.root(resolve_slot_symbol(class_name, key, env)?));
        initargs.push(roots.root(value));
        i += 2;
    }
    Ok(initargs.iter().map(|r| r.get()).collect())
}

/// Specificity distance for a method specialized on a built-in CL type name when
/// the argument is an immediate value (fixnum, character, string, …) that has no
/// user-registered CLOS class carrying that name. Returns `None` if the argument
/// is not of that type. Depths encode the numeric/type supertype chain so a
/// `fixnum` specializer outranks `integer` outranks `number`, etc.
fn builtin_type_specializer_distance(name: &str, arg: BlissVal) -> Option<usize> {
    match symbol_bare_name(name).as_str() {
        "FIXNUM" => arg.is_fixnum().then_some(1),
        "INTEGER" => arg.is_fixnum().then_some(2),
        "RATIONAL" => arg.is_fixnum().then_some(3),
        "REAL" => (arg.is_fixnum() || arg.is_single_float()).then_some(4),
        "NUMBER" => (arg.is_fixnum() || arg.is_single_float()).then_some(5),
        "SINGLE-FLOAT" => arg.is_single_float().then_some(1),
        "FLOAT" => arg.is_single_float().then_some(2),
        "STRING" | "SIMPLE-STRING" | "BASE-STRING" => is_string_value(arg).then_some(1),
        "CHARACTER" => arg.is_character().then_some(1),
        "NULL" => arg.is_nil().then_some(1),
        "KEYWORD" => is_keyword_arg(arg).then_some(1),
        "SYMBOL" => arg.is_symbol().then_some(2),
        "CONS" => arg.is_cons().then_some(1),
        "LIST" => arg.is_list().then_some(2),
        // PATHNAME is a distinct built-in type (not a CLOS instance); ASDF's
        // source-registry dispatches methods on it (bliss-lb6.14).
        "PATHNAME" => bliss_stdlib::is_pathname(arg).then_some(1),
        "ATOM" => (!arg.is_cons()).then_some(6),
        "T" => Some(usize::MAX / 4),
        _ => None,
    }
}

fn method_specificity_vector(
    env: &Env,
    method: &MethodDef,
    args: &[BlissVal],
) -> Option<Vec<usize>> {
    if method.specializers.len() > args.len() {
        return None;
    }
    let mut distances = Vec::with_capacity(method.specializers.len());
    for (arg, specializer) in args.iter().zip(method.specializers.iter()) {
        match specializer {
            MethodSpecializer::Any => distances.push(usize::MAX / 4),
            MethodSpecializer::Eql(expected) => {
                if arg != expected {
                    return None;
                }
                distances.push(0);
            }
            MethodSpecializer::Class(name) => {
                let specializer_sym = resolve_sym(name).unwrap_or(NIL);
                let arg_class = bliss_stdlib::class_of(*arg);
                // Prefer a genuine CLOS-class match via the argument's class
                // precedence list; fall back to built-in immediate types (integer,
                // string, …) whose classes are not user-registered by name.
                let clos_distance = resolve_class_metaobject(env, specializer_sym)
                    .ok()
                    .and_then(|specializer_class| {
                        bliss_stdlib::compute_class_precedence_list(arg_class)
                            .ok()
                            .and_then(|cpl| {
                                cpl.iter().position(|&class| class == specializer_class)
                            })
                            .map(|pos| pos + 1)
                    });
                let distance = match clos_distance {
                    Some(distance) => distance,
                    // A CLOS instance is a tagged fixnum id; never let it match a
                    // built-in immediate-type specializer (integer, symbol, …).
                    None if !bliss_stdlib::is_instance(*arg) => {
                        builtin_type_specializer_distance(name, *arg)?
                    }
                    None => return None,
                };
                distances.push(distance);
            }
        }
    }
    Some(distances)
}

fn bind_method_params(
    env: &mut Env,
    method: &MethodDef,
    args: &[BlissVal],
) -> Result<(), BlissError> {
    // Bind through the ordinary lambda-list binder so &optional/&rest/&key
    // parameters in method lambda lists behave exactly as in functions — but
    // with implicit &allow-other-keys, since keyword validation for a generic
    // call is against the union of all applicable methods' keywords, not this
    // one method's (CLHS 7.6.5; bliss-lb6.14).
    bind_lambda_list_ex(method.lambda_list, args, env, true)
}

fn invoke_method(
    env: &mut Env,
    method: &MethodDef,
    args: &[BlissVal],
    next: Option<NextMethod>,
) -> Result<BlissVal, BlissError> {
    let parent = Rc::clone(&env.frame);
    let pushed = next.is_some();
    with_child_frame(env, parent, move |env| {
        bind_method_params(env, method, args)?;
        if let Some(next) = next {
            env.method_context.push(MethodContext {
                args: args.to_vec(),
                next,
            });
        }
        let result = eval_progn(method.body, env);
        if pushed {
            env.method_context.pop();
        }
        result
    })
}

fn invoke_restart_function(
    function: &RestartFunction,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    match function {
        RestartFunction::FunctionForm {
            function_form,
            captured_frame,
        } => {
            let mut restart_env = env.child_with_parent(Rc::clone(captured_frame));
            let function = eval_form(*function_form, &mut restart_env)?;
            apply_function(function, args, &mut restart_env)
        }
        RestartFunction::Bytecode {
            function,
            captured_frame,
        } => {
            let function = Rc::new(function.borrow().clone());
            let saved = std::mem::replace(&mut env.frame, Rc::clone(captured_frame));
            let result = bytecode::run(function, args, NIL, env);
            env.frame = saved;
            result
        }
        RestartFunction::ContinueNil => Ok(args.first().copied().unwrap_or(NIL)),
    }
}

fn restart_applies(
    restart: &RestartEntry,
    condition: Option<BlissVal>,
    env: &mut Env,
) -> Result<bool, BlissError> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let Some(test_function) = &restart.test_function else {
        return Ok(true);
    };
    Ok(!invoke_restart_function(test_function, &[condition], env)?.is_nil())
}

fn invoke_primary_chain(
    env: &mut Env,
    primary: &[MethodDef],
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    let Some((method, rest)) = primary.split_first() else {
        // call-next-method with an exhausted chain: signal a catchable error
        // condition (no-next-method) rather than an opaque runtime failure.
        let msg = "no next method available for call-next-method".to_string();
        let condition = make_simple_error_condition(arena_str(&msg), env)?;
        return Err(signal_and_raise(env, condition, msg));
    };
    invoke_method(
        env,
        method,
        args,
        Some(NextMethod::Primary {
            primary: rest.to_vec(),
        }),
    )
}

fn invoke_standard_methods(
    env: &mut Env,
    around: &[MethodDef],
    before: &[MethodDef],
    primary: &[MethodDef],
    after: &[MethodDef],
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    if let Some((method, rest)) = around.split_first() {
        return invoke_method(
            env,
            method,
            args,
            Some(NextMethod::Standard {
                around: rest.to_vec(),
                before: before.to_vec(),
                primary: primary.to_vec(),
                after: after.to_vec(),
            }),
        );
    }

    for method in before {
        let _ = invoke_method(env, method, args, None)?;
    }
    let result = invoke_primary_chain(env, primary, args)?;
    for method in after {
        let _ = invoke_method(env, method, args, None)?;
    }
    Ok(result)
}

/// Whether `call-next-method` from the current context would find another
/// method to run (drives `next-method-p`).
fn method_context_has_next(context: &MethodContext) -> bool {
    match &context.next {
        NextMethod::Standard {
            around,
            before,
            primary,
            after,
        } => !around.is_empty() || !before.is_empty() || !primary.is_empty() || !after.is_empty(),
        NextMethod::Primary { primary } => !primary.is_empty(),
    }
}

fn invoke_next_method(
    env: &mut Env,
    context: &MethodContext,
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    match &context.next {
        NextMethod::Standard {
            around,
            before,
            primary,
            after,
        } => invoke_standard_methods(env, around, before, primary, after, args),
        NextMethod::Primary { primary } => invoke_primary_chain(env, primary, args),
    }
}

fn combine_short_form_results(
    combination: bliss_stdlib::MethodCombinationType,
    results: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    match combination {
        bliss_stdlib::MethodCombinationType::List => Ok(vec_to_list(results)),
        bliss_stdlib::MethodCombinationType::Append
        | bliss_stdlib::MethodCombinationType::Nconc => {
            let mut combined = Vec::new();
            for result in results {
                combined.extend(list_to_vec(*result));
            }
            Ok(vec_to_list(&combined))
        }
        bliss_stdlib::MethodCombinationType::Progn
        | bliss_stdlib::MethodCombinationType::Standard => {
            Ok(results.last().copied().unwrap_or(NIL))
        }
        bliss_stdlib::MethodCombinationType::Plus => {
            let mut sum = 0.0;
            let mut is_float = false;
            for result in results {
                sum += num_val(*result)?;
                is_float |= result.is_single_float();
            }
            Ok(if is_float {
                BlissVal::from_single_float(sum as f32)
            } else {
                BlissVal::from_fixnum(sum as i64)
            })
        }
        bliss_stdlib::MethodCombinationType::And => {
            let mut last = T;
            for result in results {
                if result.is_nil() {
                    return Ok(NIL);
                }
                last = *result;
            }
            Ok(last)
        }
        bliss_stdlib::MethodCombinationType::Or => {
            for result in results {
                if !result.is_nil() {
                    return Ok(*result);
                }
            }
            Ok(NIL)
        }
        bliss_stdlib::MethodCombinationType::Min => {
            let mut best = *results.first().unwrap_or(&NIL);
            let mut best_num = num_val(best)?;
            for result in &results[1..] {
                let num = num_val(*result)?;
                if num < best_num {
                    best = *result;
                    best_num = num;
                }
            }
            Ok(best)
        }
        bliss_stdlib::MethodCombinationType::Max => {
            let mut best = *results.first().unwrap_or(&NIL);
            let mut best_num = num_val(best)?;
            for result in &results[1..] {
                let num = num_val(*result)?;
                if num > best_num {
                    best = *result;
                    best_num = num;
                }
            }
            Ok(best)
        }
    }
}

/// Build and signal a catchable error condition for a failed generic-function
/// dispatch (`no-applicable-method`, R5.??). Returns the terminal error when no
/// handler transfers control.
fn no_applicable_method_error(env: &mut Env, name: &str) -> BlissError {
    let msg = format!("no applicable method for generic function {}", name);
    match make_simple_error_condition(arena_str(&msg), env) {
        Ok(condition) => signal_and_raise(env, condition, msg),
        Err(error) => error,
    }
}

/// Run user-defined auxiliary methods of a given qualifier on an initialization
/// generic function (`initialize-instance` / `shared-initialize`) during
/// MAKE-INSTANCE. The built-in slot initialization stands in for the default
/// primary method, so this covers the canonical `:after` construction hook.
/// `:after` methods run least-specific-first per standard method combination.
fn run_initialization_aux_methods(
    env: &mut Env,
    gf_name: &str,
    args: &[BlissVal],
    qualifier: bliss_stdlib::MethodQualifier,
) -> Result<(), BlissError> {
    let methods = match env.methods.borrow().get(gf_name).cloned() {
        Some(methods) if !methods.is_empty() => methods.clone(),
        _ => return Ok(()),
    };
    let mut applicable: Vec<(MethodDef, Vec<usize>)> = methods
        .into_iter()
        .filter(|method| method.qualifier == qualifier)
        .filter_map(|method| method_specificity_vector(env, &method, args).map(|key| (method, key)))
        .collect();
    applicable.sort_by(|a, b| a.1.cmp(&b.1));
    let mut ordered: Vec<MethodDef> = applicable.into_iter().map(|(method, _)| method).collect();
    if qualifier == bliss_stdlib::MethodQualifier::After {
        ordered.reverse();
    }
    for method in ordered {
        invoke_method(env, &method, args, None)?;
    }
    Ok(())
}

/// True if the generic `name` has at least one method applicable to `args`.
fn has_applicable_method(env: &Env, name: &str, args: &[BlissVal]) -> bool {
    env.methods
        .borrow()
        .get(name)
        .map(|methods| {
            methods
                .iter()
                .any(|m| method_specificity_vector(env, m, args).is_some())
        })
        .unwrap_or(false)
}

fn invoke_generic_function(
    name: &str,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let methods = env.methods.borrow().get(name).cloned().unwrap_or_default();
    if methods.is_empty() {
        return Err(no_applicable_method_error(env, name));
    }

    let mut applicable: Vec<(MethodDef, Vec<usize>)> = methods
        .into_iter()
        .filter_map(|method| method_specificity_vector(env, &method, args).map(|key| (method, key)))
        .collect();
    applicable.sort_by(|a, b| a.1.cmp(&b.1));
    if applicable.is_empty() {
        return Err(no_applicable_method_error(env, name));
    }

    let ordered: Vec<MethodDef> = applicable.into_iter().map(|(method, _)| method).collect();
    let mut method_map = HashMap::new();
    let method_ids: Vec<BlissVal> = ordered
        .iter()
        .map(|method| {
            method_map.insert(method.method_id, method.clone());
            method.method_id
        })
        .collect();

    let combination = env
        .generics
        .borrow()
        .get(name)
        .map(|generic| generic.combination)
        .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
    let effective = bliss_stdlib::compute_effective_method(NIL, combination, &method_ids)?;
    match combination {
        bliss_stdlib::MethodCombinationType::Standard => {
            let Some((around_ids, before_ids, primary_ids, after_ids)) =
                bliss_stdlib::clos::get_effective_method(effective)
            else {
                return Err(BlissError::Internal(
                    "missing standard effective method".into(),
                ));
            };
            let around: Vec<MethodDef> = around_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let before: Vec<MethodDef> = before_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let primary: Vec<MethodDef> = primary_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let after: Vec<MethodDef> = after_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            invoke_standard_methods(env, &around, &before, &primary, &after, args)
        }
        other => {
            let Some((short_combination, method_ids)) =
                bliss_stdlib::clos::get_short_form_method(effective)
            else {
                return Err(BlissError::Internal(
                    "missing short-form effective method".into(),
                ));
            };
            debug_assert_eq!(other, short_combination);
            let mut results = Vec::new();
            for method_id in method_ids {
                let method = method_map.get(&method_id).ok_or_else(|| {
                    BlissError::Internal("short-form method lookup failed".into())
                })?;
                results.push(invoke_method(env, method, args, None)?);
            }
            combine_short_form_results(short_combination, &results)
        }
    }
}

/// State shared by one environment-root walk. Captured lexical frames and
/// class-slot cells can be reachable through several definitions; they must be
/// visited once so a relocation callback never observes the same mutable slot
/// twice during one collection.
#[derive(Default)]
struct EnvRootVisitState {
    frames: HashSet<usize>,
    class_slot_cells: HashSet<usize>,
}

fn visit_env_frame_roots(
    frame: &Rc<RefCell<EnvFrame>>,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    let identity = Rc::as_ptr(frame) as usize;
    if !state.frames.insert(identity) {
        return;
    }

    let parent = {
        let mut frame = frame.borrow_mut();
        for value in frame.vars.values_mut() {
            visit(value as *mut BlissVal);
        }
        for value in frame.symbol_vars.values_mut() {
            visit(value as *mut BlissVal);
        }
        frame.parent.clone()
    };
    if let Some(parent) = parent {
        visit_env_frame_roots(&parent, state, visit);
    }
}

fn visit_fun_def_roots(def: &mut FunDef, visit: &mut dyn FnMut(*mut BlissVal)) {
    visit(&mut def.params_form);
    visit(&mut def.body);
}

fn visit_macro_def_roots(
    def: &mut MacroDef,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    visit(&mut def.params_form);
    visit(&mut def.body);
    if let Some(function) = &def.bytecode {
        for constant in &mut function.borrow_mut().constants {
            visit(constant);
        }
    }
    visit_env_frame_roots(&def.captured_frame, state, visit);
}

fn visit_setf_expander_roots(
    expander: &mut SetfExpander,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    match expander {
        SetfExpander::Expander(def) => visit_macro_def_roots(def, state, visit),
        SetfExpander::ShortUpdate(function) => visit(function),
    }
}

fn visit_class_def_roots(
    class: &mut ClassDef,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    for slot in &mut class.slots {
        if let Some(initform) = &mut slot.initform {
            visit(initform);
        }
    }

    let identity = Arc::as_ptr(&class.class_slot_values) as usize;
    if state.class_slot_cells.insert(identity) {
        let mut cells = match class.class_slot_values.lock() {
            Ok(cells) => cells,
            Err(poisoned) => poisoned.into_inner(),
        };
        for value in cells.values_mut().flatten() {
            visit(value);
        }
    }
}

fn visit_method_def_roots(def: &mut MethodDef, visit: &mut dyn FnMut(*mut BlissVal)) {
    visit(&mut def.method_id);
    for specializer in &mut def.specializers {
        if let MethodSpecializer::Eql(value) = specializer {
            visit(value);
        }
    }
    visit(&mut def.lambda_list);
    visit(&mut def.body);
}

fn visit_next_method_roots(next: &mut NextMethod, visit: &mut dyn FnMut(*mut BlissVal)) {
    let mut visit_methods = |methods: &mut Vec<MethodDef>| {
        for method in methods {
            visit_method_def_roots(method, visit);
        }
    };
    match next {
        NextMethod::Standard {
            around,
            before,
            primary,
            after,
        } => {
            visit_methods(around);
            visit_methods(before);
            visit_methods(primary);
            visit_methods(after);
        }
        NextMethod::Primary { primary } => visit_methods(primary),
    }
}

fn visit_restart_function_roots(
    function: &mut RestartFunction,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    match function {
        RestartFunction::FunctionForm {
            function_form,
            captured_frame,
        } => {
            visit(function_form);
            visit_env_frame_roots(captured_frame, state, visit);
        }
        RestartFunction::Bytecode {
            function,
            captured_frame,
        } => {
            visit_bytecode_function_roots(&mut function.borrow_mut(), visit);
            visit_env_frame_roots(captured_frame, state, visit);
        }
        RestartFunction::ContinueNil => {}
    }
}

fn visit_bytecode_function_roots(
    function: &mut bliss_rt::bytecode::BytecodeFunction,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    for constant in &mut function.constants {
        visit(constant);
    }
    for restart_case in &mut function.restart_cases {
        for restart in &mut restart_case.restarts {
            visit_bytecode_function_roots(&mut restart.function, visit);
        }
    }
    for nested in &mut function.nested_functions {
        visit_bytecode_function_roots(nested, visit);
    }
}

fn visit_handler_roots(
    handler: &mut HandlerImpl,
    state: &mut EnvRootVisitState,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    match handler {
        HandlerImpl::Function(function) => visit(function),
        HandlerImpl::HandlerCase {
            body,
            captured_frame,
            ..
        } => {
            visit(body);
            visit_env_frame_roots(captured_frame, state, visit);
        }
    }
}

fn visit_frozen_env_frame_roots(
    frame: &Arc<FrozenEnvFrame>,
    visited: &mut HashSet<usize>,
    visit: &mut dyn FnMut(*mut BlissVal),
) {
    let identity = Arc::as_ptr(frame) as usize;
    if !visited.insert(identity) {
        return;
    }
    {
        let mut vars = frame
            .vars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for value in vars.values_mut() {
            visit(value);
        }
    }
    {
        let mut vars = frame
            .symbol_vars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for value in vars.values_mut() {
            visit(value);
        }
    }
    if let Some(parent) = &frame.parent {
        visit_frozen_env_frame_roots(parent, visited, visit);
    }
}

fn frozen_macro_captures() -> &'static OrderedMutex<Vec<Weak<OrderedMutex<FrozenMacroCapture>>>> {
    static CAPTURES: std::sync::OnceLock<
        OrderedMutex<Vec<Weak<OrderedMutex<FrozenMacroCapture>>>>,
    > = std::sync::OnceLock::new();
    CAPTURES.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            18,
            "evaluator frozen macro GC roots",
            Vec::new(),
        )
    })
}

fn register_frozen_macro_capture(
    capture: FrozenMacroCapture,
) -> Arc<OrderedMutex<FrozenMacroCapture>> {
    static NEXT_CAPTURE_ORDER: AtomicU64 = AtomicU64::new(1);
    install_evaluator_global_root_scanner();
    let capture = Arc::new(OrderedMutex::new(
        LockLevel::ExecutionObject,
        NEXT_CAPTURE_ORDER.fetch_add(1, AtomicOrdering::Relaxed),
        "frozen macro capture",
        capture,
    ));
    let mut captures = frozen_macro_captures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    captures.retain(|weak| weak.strong_count() != 0);
    captures.push(Arc::downgrade(&capture));
    capture
}

fn scan_evaluator_global_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    CONTROL_VALUES.with(|values| {
        for value in values.borrow_mut().values_mut() {
            visit(value);
        }
    });
    MACROEXPAND_ENVIRONMENTS.with(|environments| {
        for environment in environments.borrow_mut().values_mut() {
            environment.visit_gc_roots(visit);
        }
    });

    let mut state = EnvRootVisitState::default();
    GLOBAL_MACROS.with(|macros| {
        for definition in macros.borrow_mut().values_mut() {
            visit_macro_def_roots(definition, &mut state, visit);
        }
    });
    GLOBAL_SETF_FNS.with(|functions| {
        for definition in functions.borrow_mut().values_mut() {
            visit_fun_def_roots(definition, visit);
        }
    });
    MACRO_FN_CACHE.with(|cache| {
        for entry in cache.borrow_mut().values_mut() {
            let mut params = BlissVal(entry.params_bits);
            let mut body = BlissVal(entry.body_bits);
            visit(&mut params);
            visit(&mut body);
            entry.params_bits = params.0;
            entry.body_bits = body.0;
            visit(&mut entry.handle);
            visit(&mut entry.symbol);
        }
    });

    let captures = frozen_macro_captures()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut frozen_frames = HashSet::new();
    for weak in captures.iter() {
        let Some(capture) = weak.upgrade() else {
            continue;
        };
        let mut capture = capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        visit(&mut capture.params_form);
        visit(&mut capture.body);
        visit_frozen_env_frame_roots(&capture.captured_frame, &mut frozen_frames, visit);
        for definition in capture.funs.values_mut() {
            visit_fun_def_roots(definition, visit);
        }
        for class in capture.classes.values_mut() {
            visit_class_def_roots(class, &mut state, visit);
        }
        for methods in capture.methods.values_mut() {
            for method in methods {
                visit_method_def_roots(method, visit);
            }
        }
        for expansion in capture.symbol_macros.values_mut() {
            visit(expansion);
        }
    }

    let mut loaded_compiler_macros = LOADED_COMPILER_MACRO_FUNCTIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loaded_compiler_macros.retain(|weak| weak.strong_count() != 0);
    for weak in loaded_compiler_macros.iter() {
        let Some(function) = weak.upgrade() else {
            continue;
        };
        let mut function = function
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for constant in &mut function.constants {
            visit(constant);
        }
    }
}

fn install_evaluator_global_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_evaluator_global_roots));
}

impl Env {
    fn funs_mut(&mut self) -> std::cell::RefMut<'_, HashMap<String, FunDef>> {
        if Rc::strong_count(&self.funs) > 1 {
            let definitions = self.funs.borrow().clone();
            self.funs = Rc::new(RefCell::new(definitions));
        }
        self.funs.borrow_mut()
    }

    fn macros_mut(&mut self) -> std::cell::RefMut<'_, HashMap<String, MacroDef>> {
        if Rc::strong_count(&self.macros) > 1 {
            let definitions = self.macros.borrow().clone();
            self.macros = Rc::new(RefCell::new(definitions));
        }
        self.macros.borrow_mut()
    }

    fn symbol_macros_mut(&mut self) -> std::cell::RefMut<'_, HashMap<u32, BlissVal>> {
        if Rc::strong_count(&self.symbol_macros) > 1 {
            let definitions = self.symbol_macros.borrow().clone();
            self.symbol_macros = Rc::new(RefCell::new(definitions));
        }
        self.symbol_macros.borrow_mut()
    }

    /// Enumerate every mutable `BlissVal` slot owned by this tree-walker
    /// environment. A moving collector can rewrite each yielded pointer in
    /// place. Live top-level evaluation registers this visitor with the runtime
    /// through [`LiveEnvRootGuard`], alongside the evaluator's global/static
    /// scanners and scoped roots for transient Rust locals.
    fn visit_gc_roots(&mut self, visit: &mut dyn FnMut(*mut BlissVal)) {
        let mut state = EnvRootVisitState::default();

        visit_env_frame_roots(&self.frame, &mut state, visit);

        for def in self.funs.borrow_mut().values_mut() {
            visit_fun_def_roots(def, visit);
        }
        for def in self.macros.borrow_mut().values_mut() {
            visit_macro_def_roots(def, &mut state, visit);
        }
        for expander in self.setf_expanders.borrow_mut().values_mut() {
            visit_setf_expander_roots(expander, &mut state, visit);
        }
        for expansion in self.symbol_macros.borrow_mut().values_mut() {
            visit(expansion);
        }
        for class in self.classes.borrow_mut().values_mut() {
            visit_class_def_roots(class, &mut state, visit);
        }
        for generic in self.generics.borrow_mut().values_mut() {
            visit(&mut generic.generic_function);
        }
        for methods in self.methods.borrow_mut().values_mut() {
            for method in methods {
                visit_method_def_roots(method, visit);
            }
        }
        for restart in &mut self.restarts {
            visit_restart_function_roots(&mut restart.function, &mut state, visit);
            if let Some(function) = &mut restart.interactive_function {
                visit_restart_function_roots(function, &mut state, visit);
            }
            if let Some(function) = &mut restart.test_function {
                visit_restart_function_roots(function, &mut state, visit);
            }
        }
        for cluster in &mut self.handlers {
            for entry in &mut cluster.entries {
                visit_handler_roots(&mut entry.handler, &mut state, visit);
            }
        }
        for value in &mut self.mv {
            visit(value);
        }
        for closure in self.closures.borrow_mut().values_mut() {
            visit(&mut closure.params_form);
            visit(&mut closure.body);
            visit_env_frame_roots(&closure.captured_frame, &mut state, visit);
        }
        for context in &mut self.method_context {
            for arg in &mut context.args {
                visit(arg);
            }
            visit_next_method_roots(&mut context.next, visit);
        }
    }

    /// A fresh top-level environment. Resets the process-global CLOS state so
    /// each independent program (and each test) starts from a clean class
    /// registry.
    fn new(sandbox: bool) -> Self {
        Self::new_impl(sandbox, true)
    }

    /// A transient environment for macro / compiler-macro expansion. Unlike
    /// [`Env::new`] it does NOT reset the process-global CLOS state — it shares
    /// the caller's classes. Resetting here (as the old `Env::new` did) wiped
    /// every user class defined before a later macro expansion, which corrupted
    /// the ASDF load: `make-instance` of an early class (e.g. `system`) failed
    /// with "no slot layout" because the class had been erased (bliss-lb6).
    fn new_for_macro_expansion(sandbox: bool) -> Self {
        Self::new_impl(sandbox, false)
    }

    fn new_impl(sandbox: bool, reset_clos: bool) -> Self {
        install_evaluator_global_root_scanner();
        if reset_clos {
            let _ = bliss_stdlib::bootstrap_clos();
        } else {
            let _ = bliss_stdlib::ensure_clos_bootstrapped();
        }
        // Package storage is the process-global stdlib registry (bliss-bhs Stage
        // 4). Follow the same reset/ensure discipline as CLOS above: a top-level
        // environment starts from the standard package layout; a macro-expansion
        // environment shares the caller's live packages.
        if reset_clos {
            reset_package_registry();
        } else {
            ensure_package_registry();
        }
        // Establish the condition classes and preallocate the STORAGE-CONDITION
        // pool at startup, before any user code runs, so the heap-exhaustion /
        // stack-overflow path never has to allocate (R5.110, bliss-uh4.2). A
        // failure here means the condition system is unusable — fail loudly
        // rather than limping on and lazily allocating on the low-memory path.
        bliss_stdlib::initialize_condition_runtime_support()
            .expect("initialize condition runtime support (STORAGE-CONDITION pool) at startup");
        let mut env = Env {
            frame: Rc::new(RefCell::new(EnvFrame::default())),
            funs: Rc::new(RefCell::new(HashMap::new())),
            macros: Rc::new(RefCell::new(HashMap::new())),
            setf_expanders: Rc::new(RefCell::new(HashMap::new())),
            symbol_macros: Rc::new(RefCell::new(HashMap::new())),
            classes: Rc::new(RefCell::new(HashMap::new())),
            generics: Rc::new(RefCell::new(HashMap::new())),
            methods: Rc::new(RefCell::new(HashMap::new())),
            current_package: "COMMON-LISP-USER".to_string(),
            sandbox,
            restarts: Vec::new(),
            handlers: Vec::new(),
            mv: Vec::new(),
            mv_active: false,
            closures: Rc::new(RefCell::new(HashMap::new())),
            block_stack: Vec::new(),
            catch_stack: Vec::new(),
            tag_stack: Vec::new(),
            method_context: Vec::new(),
            eval_context: EvalContext::Repl,
        };
        env.define_local("*MODULE-PROVIDER-FUNCTIONS*", NIL);
        env.define_local("*LOAD-HOOKS*", NIL);
        // *features*: :BLISS plus the host OS so portable code (e.g. UIOP's
        // DETECT-OS) can identify the platform via FEATUREP.
        let mut features = vec![resolve_sym(":BLISS").unwrap_or(NIL)];
        #[cfg(unix)]
        features.push(resolve_sym(":UNIX").unwrap_or(NIL));
        #[cfg(target_os = "linux")]
        features.push(resolve_sym(":LINUX").unwrap_or(NIL));
        #[cfg(target_os = "macos")]
        features.push(resolve_sym(":DARWIN").unwrap_or(NIL));
        #[cfg(windows)]
        {
            features.push(resolve_sym(":WINDOWS").unwrap_or(NIL));
            features.push(resolve_sym(":WIN32").unwrap_or(NIL));
        }
        // *FEATURES* lives in the global symbol-value cell (not a frame binding),
        // so the reader's #+/#- conditionals can consult it directly — and (push
        // :foo *features*) routes to the same cell. lookup_var/set_var fall through
        // to the cell for this interned name. Initialise it ONLY ONCE: a later
        // Env::new (loading a file creates a fresh Env) must NOT reset the global
        // cell and wipe features another load already added (e.g. asdf's :ASDF*).
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static FEATURES_INITIALISED: AtomicBool = AtomicBool::new(false);
            if !FEATURES_INITIALISED.swap(true, Ordering::Relaxed) {
                bliss_rt::symbols::set_symbol_value(
                    bliss_rt::symbols::intern("*FEATURES*"),
                    vec_to_list(&features),
                );
            }
        }
        env.define_local("*MODULES*", NIL); // names of REQUIRE'd/PROVIDE'd modules
        env.define_local("*PACKAGE*", package_object("COMMON-LISP-USER"));
        env.seed_standard_constant(
            "MOST-POSITIVE-FIXNUM",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "MOST-NEGATIVE-FIXNUM",
            BlissVal::from_fixnum(-(1_i64 << 60)),
        );
        env.seed_standard_constant("CHAR-CODE-LIMIT", BlissVal::from_fixnum(0x110000));
        env.seed_standard_constant("ARRAY-RANK-LIMIT", BlissVal::from_fixnum(8));
        env.seed_standard_constant(
            "ARRAY-DIMENSION-LIMIT",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "ARRAY-TOTAL-SIZE-LIMIT",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "CALL-ARGUMENTS-LIMIT",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "LAMBDA-PARAMETERS-LIMIT",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "MULTIPLE-VALUES-LIMIT",
            BlissVal::from_fixnum((1_i64 << 60) - 1),
        );
        env.seed_standard_constant(
            "INTERNAL-TIME-UNITS-PER-SECOND",
            BlissVal::from_fixnum(1000),
        );
        // Install the stdlib's GC hooks (stream tracing + finalizer dispatch)
        // before allocating any stream, so stream handles are traced and their
        // finalizers can close unclosed file descriptors (bliss-jtc.7a).
        bliss_stdlib::streams::install_gc_hooks();
        // Standard stream special variables, bound to real terminal streams
        // backed by the process stdio (see bliss_stdlib::streams). *terminal-io*
        // / *query-io* / *debug-io* share the stdin object for their input side;
        // routing all output builtins through these keeps a single stream model.
        let stdin_stream = bliss_stdlib::make_stdin();
        let stdout_stream = bliss_stdlib::make_stdout();
        let stderr_stream = bliss_stdlib::make_stderr();
        env.define_local("*STANDARD-INPUT*", stdin_stream);
        env.define_local("*STANDARD-OUTPUT*", stdout_stream);
        env.define_local("*ERROR-OUTPUT*", stderr_stream);
        env.define_local("*TRACE-OUTPUT*", stdout_stream);
        env.define_local("*TERMINAL-IO*", stdout_stream);
        env.define_local("*QUERY-IO*", stdout_stream);
        env.define_local("*DEBUG-IO*", stdout_stream);
        env.define_local("*TYPE-DEFINITIONS*", NIL);
        env.define_local("*CONDITION-TYPES*", NIL);
        env.define_local("*CONDITION-DEFINITIONS*", NIL);
        env.define_local("*BREAK-ON-SIGNALS*", NIL);

        // bliss-5mf: reseed the STORAGE-CONDITION pool with CLI-native instances
        // whose class the CLI's condition matcher and TYPE-OF recognize (the
        // stdlib preallocated them under its own hardcoded condition-symbol class,
        // which the CLI reads back as a different symbol). They stay pinned in the
        // GC heap and immortal (D5.13). Runs after the env is functional and
        // before any user code; best-effort — a failure leaves the stdlib pool.
        {
            let n = bliss_stdlib::conditions::storage_condition_pool_size();
            let mut pool = Vec::with_capacity(n);
            let mut ok = true;
            for _ in 0..n {
                match build_condition_instance_impl(&mut env, "STORAGE-CONDITION", &[], true) {
                    Ok(inst) => pool.push(inst),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let _ = bliss_stdlib::conditions::set_storage_condition_pool(&pool);
            }
        }
        env
    }

    /// Create a child environment sharing lexical maps until a local definition
    /// detaches them through the copy-on-write mutation helpers above.
    fn child(&self) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                symbol_vars: HashMap::new(),
                parent: Some(Rc::clone(&self.frame)),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            setf_expanders: Rc::clone(&self.setf_expanders),
            symbol_macros: Rc::clone(&self.symbol_macros),
            classes: Rc::clone(&self.classes),
            generics: Rc::clone(&self.generics),
            methods: Rc::clone(&self.methods),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            method_context: self.method_context.clone(),
            eval_context: self.eval_context,
        }
    }

    fn child_with_parent(&self, parent: Rc<RefCell<EnvFrame>>) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                symbol_vars: HashMap::new(),
                parent: Some(parent),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            setf_expanders: Rc::clone(&self.setf_expanders),
            symbol_macros: Rc::clone(&self.symbol_macros),
            classes: Rc::clone(&self.classes),
            generics: Rc::clone(&self.generics),
            methods: Rc::clone(&self.methods),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            method_context: self.method_context.clone(),
            eval_context: self.eval_context,
        }
    }

    fn lookup_var(&self, name: &str) -> Option<BlissVal> {
        let frame = self.frame.borrow();
        if let Some(val) = frame.vars.get(name) {
            return Some(*val);
        }
        let parent = frame.parent.clone();
        drop(frame);
        if let Some(val) = parent.and_then(|parent| Self::lookup_frame(&parent, name)) {
            return Some(val);
        }
        // Global fallback (bliss-jtc.6 Stage C2): a global binding not on the
        // frame stack lives in the symbol's heap value cell.
        bliss_rt::symbols::find_index(name).and_then(global_value_cell)
    }

    fn lookup_var_symbol(&self, symbol: BlissVal) -> Option<BlissVal> {
        let idx = symbol.as_symbol_index();
        let frame = self.frame.borrow();
        if let Some(val) = frame.symbol_vars.get(&idx) {
            return Some(*val);
        }
        let parent = frame.parent.clone();
        drop(frame);
        if let Some(val) = parent.and_then(|parent| Self::lookup_symbol_frame(&parent, idx)) {
            return Some(val);
        }
        if let Some(val) = global_value_cell(idx) {
            return Some(val);
        }
        let name = sym_name(symbol);
        if let Some(val) = self.lookup_var(&name) {
            return Some(val);
        }
        // A package-qualified symbol also resolves to a same-named lexical
        // binding via its bare name (e.g. PKG:X to a local X). Keywords are
        // EXCLUDED: they are self-evaluating and must never resolve to a variable
        // — otherwise `:x` inside a function with a parameter `x` would return the
        // parameter's value (bliss-lb6.14 / bfasl-bytecode-unit merge).
        let bare = symbol_bare_name(&name);
        if bare != name && !name.starts_with("KEYWORD:") {
            return self.lookup_var(&bare);
        }
        None
    }

    fn set_var(&mut self, name: &str, val: BlissVal) {
        if Self::set_frame_var(&self.frame, name, val) {
            return;
        }
        // Not bound on the frame stack → global assignment into the value cell
        // (bliss-jtc.6 Stage C2). Uninterned names have no cell, so keep the old
        // local-definition behaviour for them.
        match bliss_rt::symbols::find_index(name) {
            Some(idx) => bliss_rt::symbols::set_symbol_value(idx, val),
            None => self.define_local(name, val),
        }
    }

    fn set_var_symbol(&mut self, symbol: BlissVal, val: BlissVal) {
        if Self::set_symbol_frame_var(&self.frame, symbol.as_symbol_index(), val) {
            return;
        }
        let name = sym_name(symbol);
        if Self::set_frame_var(&self.frame, &name, val) {
            return;
        }
        // Not bound on the frame stack → global assignment into the symbol's
        // value cell (bliss-jtc.6 Stage C2).
        bliss_rt::symbols::set_symbol_value(symbol.as_symbol_index(), val);
    }

    fn lookup_symbol_macro(&self, symbol: BlissVal) -> Option<BlissVal> {
        self.symbol_macros
            .borrow()
            .get(&symbol.as_symbol_index())
            .copied()
    }

    fn define_symbol_macro(&mut self, symbol: BlissVal, expansion: BlissVal) {
        self.symbol_macros_mut()
            .insert(symbol.as_symbol_index(), expansion);
    }

    fn define_local(&mut self, name: &str, val: BlissVal) {
        let mut frame = self.frame.borrow_mut();
        frame.vars.insert(name.to_string(), val);
        // Also bind by symbol index so the symbol-keyed lookup
        // (`lookup_var_symbol`) sees this binding. Otherwise a binding made only
        // in `vars` (e.g. an unsupplied &optional/&key parameter's NIL default)
        // fails to shadow a same-named binding in an enclosing frame's
        // `symbol_vars`, because the two lookups walk the frame chain
        // independently — the body would then see the caller's variable instead
        // of the parameter (bliss-lb6: an unsupplied `end` inheriting the
        // caller's `end` drove find/position off the end of a sequence).
        if let Some(idx) = bliss_rt::symbols::find_index(name) {
            frame.symbol_vars.insert(idx, val);
        }
    }

    fn define_local_symbol(&mut self, symbol: BlissVal, val: BlissVal) {
        let mut frame = self.frame.borrow_mut();
        frame.symbol_vars.insert(symbol.as_symbol_index(), val);
        frame.vars.insert(sym_name(symbol), val);
    }

    fn seed_standard_constant(&mut self, name: &str, val: BlissVal) {
        self.define_local(name, val);
        self.define_local(&format!("COMMON-LISP:{name}"), val);
        self.define_local(&format!("CL:{name}"), val);
    }

    fn lookup_frame(frame: &Rc<RefCell<EnvFrame>>, name: &str) -> Option<BlissVal> {
        let borrowed = frame.borrow();
        if let Some(val) = borrowed.vars.get(name) {
            return Some(*val);
        }
        let parent = borrowed.parent.clone();
        drop(borrowed);
        parent.and_then(|parent| Self::lookup_frame(&parent, name))
    }

    fn lookup_symbol_frame(frame: &Rc<RefCell<EnvFrame>>, symbol_index: u32) -> Option<BlissVal> {
        let borrowed = frame.borrow();
        if let Some(val) = borrowed.symbol_vars.get(&symbol_index) {
            return Some(*val);
        }
        let parent = borrowed.parent.clone();
        drop(borrowed);
        parent.and_then(|parent| Self::lookup_symbol_frame(&parent, symbol_index))
    }

    fn set_frame_var(frame: &Rc<RefCell<EnvFrame>>, name: &str, val: BlissVal) -> bool {
        {
            let mut borrowed = frame.borrow_mut();
            if borrowed.vars.contains_key(name) {
                borrowed.vars.insert(name.to_string(), val);
                if let Some(idx) = bliss_rt::symbols::find_index(name) {
                    borrowed.symbol_vars.insert(idx, val);
                }
                return true;
            }
            let parent = borrowed.parent.clone();
            drop(borrowed);
            if let Some(parent) = parent {
                return Self::set_frame_var(&parent, name, val);
            }
        }
        false
    }

    fn set_symbol_frame_var(
        frame: &Rc<RefCell<EnvFrame>>,
        symbol_index: u32,
        val: BlissVal,
    ) -> bool {
        {
            let mut borrowed = frame.borrow_mut();
            let name = sym_name(BlissVal::from_symbol_index(symbol_index));
            if borrowed.symbol_vars.contains_key(&symbol_index) || borrowed.vars.contains_key(&name)
            {
                borrowed.symbol_vars.insert(symbol_index, val);
                borrowed.vars.insert(name, val);
                return true;
            }
            let parent = borrowed.parent.clone();
            drop(borrowed);
            if let Some(parent) = parent {
                return Self::set_symbol_frame_var(&parent, symbol_index, val);
            }
        }
        false
    }

    fn clear_mv(&mut self) {
        self.mv.clear();
        self.mv_active = false;
    }

    fn set_mv(&mut self, values: Vec<BlissVal>) {
        self.mv = values;
        self.mv_active = true;
    }
}

// ── Live tree-walker environment roots (bliss-6b2.3) ─────────────

fn live_env_roots() -> &'static OrderedMutex<HashMap<ThreadId, Vec<usize>>> {
    static ROOTS: std::sync::OnceLock<OrderedMutex<HashMap<ThreadId, Vec<usize>>>> =
        std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            19,
            "live evaluator environment GC roots",
            HashMap::new(),
        )
    })
}

fn scan_live_env_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    let roots = live_env_roots().lock().unwrap_or_else(|e| e.into_inner());
    let mut visited = HashSet::new();
    for stack in roots.values() {
        for &address in stack {
            if visited.insert(address) {
                // SAFETY: guards register an Env only for the lexical extent in
                // which its originating &mut Env remains live and immobile.
                unsafe { (&mut *(address as *mut Env)).visit_gc_roots(visit) };
            }
        }
    }
}

fn install_live_env_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_live_env_roots));
}

struct LiveEnvRootGuard {
    thread: ThreadId,
    address: usize,
}

impl LiveEnvRootGuard {
    fn new(env: &mut Env) -> Self {
        install_live_env_scanner();
        let thread = std::thread::current().id();
        let address = env as *mut Env as usize;
        live_env_roots()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(thread)
            .or_default()
            .push(address);
        Self { thread, address }
    }
}

impl Drop for LiveEnvRootGuard {
    fn drop(&mut self) {
        assert_eq!(self.thread, std::thread::current().id());
        let mut roots = live_env_roots().lock().unwrap_or_else(|e| e.into_inner());
        let remove = {
            let stack = roots
                .get_mut(&self.thread)
                .expect("live Env root stack vanished before guard drop");
            assert_eq!(stack.pop(), Some(self.address));
            stack.is_empty()
        };
        if remove {
            roots.remove(&self.thread);
        }
    }
}

#[cfg(test)]
fn live_env_root_depth() -> usize {
    live_env_roots()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&std::thread::current().id())
        .map_or(0, Vec::len)
}

thread_local! {
    // Caller frames that `with_child_frame` has swapped OUT of `env.frame` while a
    // nested evaluation runs. For a lexical child the new frame's parent IS the
    // saved caller frame, so it stays reachable via `env.frame`; but a *function
    // call* installs the callee's captured frame as parent, taking the caller's
    // frame (and its locals — e.g. a `loop` iteration variable) off the scanned
    // `env.frame` chain. Those locals then survive only in this Rust-side stack,
    // invisible to the relocating GC. Rooting them here keeps them (and the values
    // they bind) live and relocated for the whole nested call (bliss-6b2 #2).
    static SUSPENDED_FRAMES: RefCell<Vec<Rc<RefCell<EnvFrame>>>> =
        const { RefCell::new(Vec::new()) };
}

fn scan_suspended_frames(visit: &mut dyn FnMut(*mut BlissVal)) {
    SUSPENDED_FRAMES.with(|s| {
        let frames = s.borrow();
        let mut state = EnvRootVisitState::default();
        for frame in frames.iter() {
            visit_env_frame_roots(frame, &mut state, visit);
        }
    });
}

fn install_suspended_frame_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_suspended_frames));
}

fn with_child_frame<T>(
    env: &mut Env,
    parent: Rc<RefCell<EnvFrame>>,
    f: impl FnOnce(&mut Env) -> Result<T, BlissError>,
) -> Result<T, BlissError> {
    install_suspended_frame_scanner();
    let saved_frame = Rc::clone(&env.frame);
    // Keep the swapped-out caller frame rooted for the extent of `f`: a function
    // call reparents `env.frame` to the callee's captured frame, so without this
    // the caller's locals would be unscanned during the call (bliss-6b2 #2).
    SUSPENDED_FRAMES.with(|s| s.borrow_mut().push(Rc::clone(&saved_frame)));
    env.frame = Rc::new(RefCell::new(EnvFrame {
        vars: HashMap::new(),
        symbol_vars: HashMap::new(),
        parent: Some(parent),
    }));
    let result = f(env);
    env.frame = saved_frame;
    SUSPENDED_FRAMES.with(|s| {
        s.borrow_mut().pop();
    });
    result
}

/// Run `f` inside an implicit `(block nil …)` so a `(return x)` in the body
/// exits the construct with `x`. Used by DOTIMES/DOLIST (and other iteration
/// macros) which ANSI specifies establish a NIL block.
fn with_block_nil(
    env: &mut Env,
    f: impl FnOnce(&mut Env) -> Result<BlissVal, BlissError>,
) -> Result<BlissVal, BlissError> {
    let token = next_control_token("__RETURN_FROM__");
    env.block_stack.push(("NIL".to_string(), token.clone()));
    let result = f(env);
    env.block_stack.pop();
    match result {
        Err(BlissError::Internal(msg)) if msg == token => Ok(take_control_value(&token)),
        other => other,
    }
}

fn eval_lambda_call(
    env: &mut Env,
    params_form: BlissVal,
    body: BlissVal,
    args: &[BlissVal],
    parent: Rc<RefCell<EnvFrame>>,
) -> Result<BlissVal, BlissError> {
    // The interim host-stack depth guard (commit ddba528) is retired (nmq.6):
    // with the bytecode backend the default, deep recursion runs on the
    // per-green-thread BlissStack and is bounded by BLISS_STACK_SIZE, raising a
    // catchable STORAGE-CONDITION (R2.20). This tree-walker path is now the
    // fallback for forms the compiler does not yet handle.
    let roots = bliss_rt::ShadowRootScope::new();
    let params_form = roots.root(params_form);
    let body = roots.root(body);
    let args = roots.root_values(args.iter().copied());
    with_child_frame(env, parent, |env| {
        let current_args: Vec<_> = args.iter().map(|arg| arg.get()).collect();
        bind_lambda_list(params_form.get(), &current_args, env)?;
        // Arguments are a single-value context; a producer evaluated as an
        // argument (or an &optional/&key default) must not leak its extra values
        // into the body. The body's tail form establishes this call's values.
        env.clear_mv();
        eval_progn(body.get(), env)
    })
}

fn with_eval_context<T>(
    env: &mut Env,
    context: EvalContext,
    f: impl FnOnce(&mut Env) -> Result<T, BlissError>,
) -> Result<T, BlissError> {
    let previous = env.eval_context;
    env.eval_context = context;
    let result = f(env);
    env.eval_context = previous;
    result
}

// ── BlissVal printer ──────────────────────────────────────────────
#[inline]
/// Render a form for the `BLISS_BFASL_FORM` compile-file diagnostic (truncated).
pub(super) fn fmt_form_debug(val: BlissVal) -> String {
    let mut s = String::new();
    print_val(val, &mut s);
    if s.len() > 300 {
        s.truncate(300);
        s.push_str("…");
    }
    s
}

fn print_val(val: BlissVal, out: &mut String) {
    if val.is_nil() {
        out.push_str("NIL");
    } else if val == T {
        out.push('T');
    } else if val == EOF {
        out.push_str("#<EOF>");
    } else if let Some(name) = package_object_name(val) {
        // A first-class package object (bliss-bhs) prints as #<PACKAGE name>.
        out.push_str("#<PACKAGE ");
        out.push_str(&name);
        out.push('>');
    } else if bliss_stdlib::is_instance(val) {
        // A user-defined `print-object` method wins when one applies (and a print
        // env is parked); otherwise CLOS instances are opaque handles printed as
        // #<CLASS-NAME>.
        if let Some(rendered) = dispatch_print_object(val, PRINT_ESCAPE.with(|c| c.get())) {
            out.push_str(&rendered);
            return;
        }
        let name = instance_class_hierarchy_names(val)
            .as_ref()
            .and_then(|names| names.first())
            .cloned()
            .unwrap_or_else(|| "INSTANCE".to_string());
        out.push_str("#<");
        out.push_str(&name);
        out.push('>');
    } else if val.is_fixnum() {
        out.push_str(&val.as_fixnum().to_string());
    } else if val.is_single_float() {
        let s = format!("{}", val.as_single_float());
        out.push_str(&s);
        if !s.contains('.') && !s.contains('e') {
            out.push_str(".0");
        }
    } else if val.is_character() {
        out.push_str("#\\");
        match val.as_char() {
            ' ' => out.push_str("Space"),
            '\n' => out.push_str("Newline"),
            '\t' => out.push_str("Tab"),
            '\r' => out.push_str("Return"),
            c => out.push(c),
        }
    } else if val.is_symbol() {
        let name = sym_name(val);
        if let Some(bare) = name.strip_prefix("KEYWORD:") {
            out.push(':');
            out.push_str(bare);
        } else {
            out.push_str(&name);
        }
    } else if val.is_cons() {
        out.push('(');
        print_list_body(val, out);
        out.push(')');
    } else if val.is_heap_object() {
        if let Some(s) = bliss_stdlib::registered_string(val) {
            out.push('"');
            for c in s.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            return;
        }
        // Pathnames are registry-backed pseudo-heap values: render via their
        // namestring rather than dereferencing them as a heap object (which
        // would crash for a pathname whose namestring is not separately
        // registered, e.g. a MERGE-PATHNAMES result). bliss-lb6.
        if bliss_stdlib::is_pathname(val) {
            if let Ok(ns) = bliss_stdlib::namestring(val) {
                let s = val_as_str(ns);
                out.push('"');
                for c in s.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
                return;
            }
        }
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            match hdr.type_id() {
                type_id::SIMPLE_BASE_STRING => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    let data = std::slice::from_raw_parts(ptr.add(16), len);
                    if let Ok(s) = std::str::from_utf8(data) {
                        out.push('"');
                        for c in s.chars() {
                            if c == '"' || c == '\\' {
                                out.push('\\');
                            }
                            out.push(c);
                        }
                        out.push('"');
                    } else {
                        out.push_str("#<string>");
                    }
                }
                type_id::SIMPLE_VECTOR => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    out.push_str("#(");
                    for i in 0..len {
                        if i > 0 {
                            out.push(' ');
                        }
                        print_val(*(ptr.add(16 + i * 8) as *const BlissVal), out);
                    }
                    out.push(')');
                }
                type_id::COMPLEX_ARRAY => {
                    // A fill-pointer / adjustable vector prints as #(...) over its
                    // active elements (0..fill-pointer) from the backing storage.
                    let fp = bliss_stdlib::cvec_fill_pointer(val);
                    let storage = *(ptr.add(8) as *const BlissVal);
                    let sptr = storage.as_ptr();
                    out.push_str("#(");
                    for i in 0..fp {
                        if i > 0 {
                            out.push(' ');
                        }
                        print_val(*(sptr.add(16 + i * 8) as *const BlissVal), out);
                    }
                    out.push(')');
                }
                type_id::RATIO => {
                    let num = *(ptr.add(8) as *const BlissVal);
                    let den = *(ptr.add(16) as *const BlissVal);
                    print_val(num, out);
                    out.push('/');
                    print_val(den, out);
                }
                type_id::BIGNUM => {
                    let sign = *(ptr.add(8) as *const i32);
                    let n = *(ptr.add(12) as *const u32) as usize;
                    let mut limbs: Vec<u64> = Vec::with_capacity(n);
                    for i in 0..n {
                        limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                    }
                    out.push_str(&bignum_to_decimal(sign, &limbs));
                }
                _ => out.push_str(&format!("#<heap-object type={}>", hdr.type_id())),
            }
        }
    } else {
        out.push_str(&format!("#<unknown {:#x}>", val.0));
    }
}

/// Render a bignum (little-endian base-2^64 limbs) as a decimal string.
/// Delegates to the stdlib renderer so the interpreter's printer and FORMAT
/// ~A/~S agree (bliss-axe).
fn bignum_to_decimal(sign: i32, limbs: &[u64]) -> String {
    bliss_stdlib::format::bignum_to_decimal(sign, limbs)
}

#[inline]
fn print_list_body(val: BlissVal, out: &mut String) {
    let mut cur = val;
    let mut first = true;
    while cur.is_cons() {
        if !first {
            out.push(' ');
        }
        first = false;
        unsafe {
            let c = cur.as_ptr() as *const ConsCell;
            print_val((*c).car, out);
            cur = (*c).cdr;
        }
    }
    if !cur.is_nil() {
        out.push_str(" . ");
        print_val(cur, out);
    }
}

#[inline]
fn format_val(val: BlissVal) -> String {
    let mut s = String::new();
    print_val(val, &mut s);
    s
}

fn format_control_message(control: &str, args: &[BlissVal]) -> Result<String, BlissError> {
    bliss_stdlib::format(NIL, control, args).map(val_as_str)
}

#[inline]
fn princ_val(val: BlissVal, out: &mut String) {
    if val.is_heap_object() {
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(ptr.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(ptr.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    out.push_str(s);
                    return;
                }
            }
        }
    }
    if val.is_character() {
        out.push(val.as_char());
        return;
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For ~A, print symbol name without package prefix
        let bare = name.trim_start_matches("KEYWORD:");
        out.push_str(bare);
        return;
    }
    print_val(val, out);
}

/// `princ_val` (unescaped), but env-aware: dispatches user `print-object`
/// methods with `*print-escape*` bound to NIL.
fn princ_val_env(val: BlissVal, env: &mut Env, out: &mut String) {
    let prev_env = PRINT_ENV.with(|c| c.replace(env as *mut Env));
    let prev_escape = PRINT_ESCAPE.with(|c| c.replace(false));
    princ_val(val, out);
    PRINT_ENV.with(|c| c.set(prev_env));
    PRINT_ESCAPE.with(|c| c.set(prev_escape));
}

// ── Symbol name lookup ────────────────────────────────────────────
fn sym_name(val: BlissVal) -> String {
    if val.is_nil() {
        return "NIL".into();
    }
    if val == T {
        return "T".into();
    }
    if !val.is_symbol() {
        return String::new();
    }
    let idx = val.as_symbol_index();
    // Use the reader's reverse symbol table for O(1) lookup
    if let Some(name) = reader::symbol_name(idx) {
        return name;
    }
    format!("SYM#{}", idx)
}

fn resolve_sym(name: &str) -> Option<BlissVal> {
    match reader::read_from_string(name) {
        Ok((sym, _)) if sym.is_symbol() => Some(sym),
        _ => None,
    }
}

thread_local! {
    // The QUOTE symbol is a constant, but `resolve_sym` runs the full reader to
    // produce it. `apply_function` did that on EVERY builtin application (to
    // synthesize `(name 'a 'b)`), so a fib-style hot loop re-parsed "QUOTE"
    // millions of times — a dominant cost in profiles of the bytecode/native
    // path. Cache it once per thread.
    static QUOTE_SYM: std::cell::Cell<BlissVal> = const { std::cell::Cell::new(NIL) };
}

/// The interned QUOTE symbol, resolved once per thread (see [`QUOTE_SYM`]).
fn quote_sym() -> BlissVal {
    QUOTE_SYM.with(|c| {
        let v = c.get();
        // NIL is itself a symbol, so the uncached sentinel (NIL) can't be
        // detected with is_symbol(); QUOTE is never NIL, so "cached" == non-NIL.
        if !v.is_nil() {
            return v;
        }
        let q = resolve_sym("QUOTE").unwrap_or(NIL);
        c.set(q);
        q
    })
}

/// Render a MAKE-PATHNAME `:directory` list — `(:absolute|:relative comp…)` —
/// to a physical namestring the pathname parser understands. Components are
/// strings or the `:up`/`:back`/`:wild`/`:wild-inferiors` keywords. Returns None
/// if the designator is not a list (leave it for the stdlib to handle).
fn directory_designator_to_namestring(dir: BlissVal) -> Option<String> {
    if !dir.is_cons() {
        return None;
    }
    let items = list_to_vec(dir);
    let mut out = String::new();
    let mut start = 0;
    if let Some(&first) = items.first() {
        if first.is_symbol() {
            match symbol_bare_name(&sym_name(first)).as_str() {
                "ABSOLUTE" => {
                    out.push('/');
                    start = 1;
                }
                "RELATIVE" => start = 1,
                _ => {}
            }
        }
    }
    for &item in &items[start..] {
        let part = if item.is_symbol() {
            match symbol_bare_name(&sym_name(item)).as_str() {
                "UP" | "BACK" => "..".to_string(),
                "WILD" => "*".to_string(),
                "WILD-INFERIORS" => "**".to_string(),
                other => other.to_string(),
            }
        } else {
            val_as_str(item)
        };
        out.push_str(&part);
        out.push('/');
    }
    Some(out)
}

/// STRINGP that is safe for every value. Some string values are registry-backed
/// sentinels — a string hash wearing `TAG_HEAP_OBJECT`, not a real heap pointer
/// — so the raw `BlissVal::is_string` would dereference garbage and segfault on
/// them (this is how ASDF's namestring comparisons crash). Recognise a
/// registry-backed string via the store first (no dereference); only then fall
/// back to reading a genuine heap-string header. Pathnames are a distinct type
/// and never strings.
fn is_string_value(v: BlissVal) -> bool {
    // Exclude pathnames FIRST: a pathname is a distinct type and never a string,
    // yet its value can also be present in the string registry (its namestring),
    // so a registry check ahead of this would wrongly report STRINGP true — which
    // made UIOP's ENSURE-DIRECTORY-PATHNAME recurse forever (bliss-lb6.15).
    if bliss_stdlib::is_pathname(v) {
        return false;
    }
    // A registry-backed string sentinel is a real string but NOT a valid heap
    // pointer (a string hash wearing TAG_HEAP_OBJECT), so recognise it via the
    // store before the raw is_string() would dereference garbage and segfault
    // (how ASDF's namestring comparisons crashed).
    if bliss_stdlib::registered_string(v).is_some() {
        return true;
    }
    v.is_string()
}

/// True if `v` is a registry-backed sentinel (a string/namestring hash wearing
/// TAG_HEAP_OBJECT) — a value that is NOT a real heap pointer and must never be
/// dereferenced via a raw `ObjectHeader` load.
fn is_registry_sentinel(v: BlissVal) -> bool {
    bliss_stdlib::registered_string(v).is_some() || bliss_stdlib::is_pathname(v)
}

/// Sentinel-safe VECTORP: a string is a vector, and registry sentinels/pathnames
/// are never real heap vectors. Guards the raw header load in
/// `bliss_rt::types::vectorp`, which would segfault on a non-pointer sentinel.
fn is_vector_value(v: BlissVal) -> bool {
    if is_string_value(v) {
        return true;
    }
    if is_registry_sentinel(v) {
        return false;
    }
    bliss_rt::types::vectorp(v)
}

/// Sentinel-safe HASH-TABLE-P.
fn is_hash_table_value(v: BlissVal) -> bool {
    bliss_stdlib::hash_table_p(v)
}

/// Sentinel-safe SIMPLE-VECTOR-P (excludes strings and sentinels).
fn is_simple_vector_value(v: BlissVal) -> bool {
    if is_string_value(v) || is_registry_sentinel(v) {
        return false;
    }
    bliss_rt::types::vectorp(v)
}

// ── Collect a list into a Vec of elements ─────────────────────────
fn list_to_vec(val: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut c = val;
    while c.is_cons() {
        let (car, cdr) = cp(c);
        result.push(car);
        c = cdr;
    }
    result
}

fn vec_to_list(elems: &[BlissVal]) -> BlissVal {
    let roots = bliss_rt::ShadowRootScope::new();
    let elems = roots.root_values(elems.iter().copied());
    let result = roots.root(NIL);
    for e in elems.iter().rev() {
        result.set(arena_cons(e.get(), result.get()));
    }
    result.get()
}

fn format_body_forms(forms: BlissVal) -> String {
    let parts = list_to_vec(forms)
        .into_iter()
        .map(format_val)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "NIL".to_string()
    } else {
        parts.join(" ")
    }
}

// ── Quasiquote expansion ─────────────────────────────────────────
/// Expand a quasiquote template, substituting BLISS::UNQUOTE forms with
/// their evaluated values and splicing BLISS::UNQUOTE-SPLICING forms.
fn eval_quasiquote(template: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    eval_quasiquote_depth(template, env, 1)
}

/// Expand a quasiquote template at nesting `depth` (1 = the outermost backquote).
/// A nested `` ` `` raises the depth; a `,`/`,@` lowers it. An unquote is only
/// evaluated when it brings the depth to 0 — otherwise its wrapper is preserved
/// and its content is processed one level shallower, so nested backquotes like
/// `` `(a `(b ,x)) `` keep the inner `,x` unevaluated (CLHS 2.4.6).
fn eval_quasiquote_depth(
    template: BlissVal,
    env: &mut Env,
    depth: u32,
) -> Result<BlissVal, BlissError> {
    if !template.is_cons() {
        return Ok(template);
    }
    let (car, cdr) = cp(template);
    let head = if car.is_symbol() {
        sym_name(car)
    } else {
        String::new()
    };

    // (BLISS::UNQUOTE expr): evaluate at depth 1, else keep wrapper at depth-1.
    if head == "BLISS::UNQUOTE" {
        let (expr, _) = cp(cdr);
        if depth == 1 {
            return eval_form(expr, env);
        }
        let inner = eval_quasiquote_depth(expr, env, depth - 1)?;
        return Ok(arena_cons(car, arena_cons(inner, NIL)));
    }
    // A bare ,@ template is an error at the outermost level; deeper, keep it.
    if head == "BLISS::UNQUOTE-SPLICING" {
        if depth == 1 {
            return Err(BlissError::Internal(",@ not inside a list".into()));
        }
        let (expr, _) = cp(cdr);
        let inner = eval_quasiquote_depth(expr, env, depth - 1)?;
        return Ok(arena_cons(car, arena_cons(inner, NIL)));
    }
    // A nested `` ` ``: preserve the wrapper, process its body one level deeper.
    if head == "BLISS::QUASIQUOTE" {
        let (inner_tmpl, _) = cp(cdr);
        let inner = eval_quasiquote_depth(inner_tmpl, env, depth + 1)?;
        return Ok(arena_cons(car, arena_cons(inner, NIL)));
    }

    // Process each element, honoring ,@ splicing only at depth 1.
    let mut result_elems: Vec<BlissVal> = Vec::new();
    // Root the accumulated elements across the element evaluations and the final
    // cons-up (bliss-6b2 #2): each eval_form / recursive expand / arena_cons can
    // relocate the earlier, Vec-resident elements.
    let _rg = VecRootGuard::new(&mut result_elems);
    let mut cur = template;
    while cur.is_cons() {
        let (elem, rest) = cp(cur);
        // A nested backquote as an element must not be flattened element-wise.
        if elem.is_cons() {
            let (ecar, ecdr) = cp(elem);
            let ecar_name = if ecar.is_symbol() {
                sym_name(ecar)
            } else {
                String::new()
            };
            if ecar_name == "BLISS::UNQUOTE-SPLICING" {
                let (splice_expr, _) = cp(ecdr);
                if depth == 1 {
                    let splice_val = eval_form(splice_expr, env)?;
                    result_elems.extend(list_to_vec(splice_val));
                } else {
                    let inner = eval_quasiquote_depth(splice_expr, env, depth - 1)?;
                    result_elems.push(arena_cons(ecar, arena_cons(inner, NIL)));
                }
                cur = rest;
                continue;
            }
            // `,,@x` — (UNQUOTE (UNQUOTE-SPLICING x)) as an element: the outer `,`
            // reduces the level, and at the enclosing level `,@x` splices. For the
            // common two-deep case (`` `(… `(… ,,@x)) ``), evaluate x now and wrap
            // each spliced element in a single UNQUOTE for the inner backquote.
            if ecar_name == "BLISS::UNQUOTE" && ecdr.is_cons() {
                let (inner_elem, _) = cp(ecdr);
                if inner_elem.is_cons() {
                    let (icar, icdr) = cp(inner_elem);
                    if icar.is_symbol() && sym_name(icar) == "BLISS::UNQUOTE-SPLICING" && depth == 2
                    {
                        let (splice_expr, _) = cp(icdr);
                        let splice_val = eval_form(splice_expr, env)?;
                        for e in list_to_vec(splice_val) {
                            result_elems.push(arena_cons(ecar, arena_cons(e, NIL)));
                        }
                        cur = rest;
                        continue;
                    }
                }
            }
        }
        let expanded = eval_quasiquote_depth(elem, env, depth)?;
        result_elems.push(expanded);
        cur = rest;
    }
    if !cur.is_nil() {
        let expanded_tail = eval_quasiquote_depth(cur, env, depth)?;
        let mut result = expanded_tail;
        for e in result_elems.iter().rev() {
            result = arena_cons(*e, result);
        }
        return Ok(result);
    }
    Ok(vec_to_list(&result_elems))
}

// ── Closure ID generation ────────────────────────────────────────
thread_local! {
    static NEXT_CLOSURE_ID: RefCell<u64> = const { RefCell::new(1) };
    static NEXT_STDLIB_CLASS_ID: RefCell<i64> = const { RefCell::new(300_000) };
}

fn next_closure_id() -> u64 {
    NEXT_CLOSURE_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    })
}

fn next_stdlib_class_id() -> BlissVal {
    NEXT_STDLIB_CLASS_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        // Opaque metaobject handle, not a fixnum: keeps class/method ids off the
        // fixnum tag so a plain integer can't collide with one. See bliss-dx6.
        BlissVal::from_meta_handle(v)
    })
}

fn register_declared_packages(source: &str) {
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    while pos < chars.len() {
        match chars[pos] {
            ';' => {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
            }
            '"' => {
                pos += 1;
                while pos < chars.len() {
                    if chars[pos] == '\\' {
                        pos += 2;
                    } else if chars[pos] == '"' {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            '(' => {
                pos += 1;
                let op = read_scan_token(&chars, &mut pos);
                if op.eq_ignore_ascii_case("DEFPACKAGE")
                    || op
                        .rsplit(':')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("DEFINE-PACKAGE"))
                {
                    let pkg = read_scan_token(&chars, &mut pos);
                    let pkg = pkg
                        .trim_start_matches(':')
                        .trim_start_matches("KEYWORD:")
                        .trim();
                    if !pkg.is_empty() {
                        reader::register_package(&pkg.to_uppercase());
                    }
                }
            }
            _ => pos += 1,
        }
    }
}

fn read_scan_token(chars: &[char], pos: &mut usize) -> String {
    while *pos < chars.len() && chars[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
    if *pos >= chars.len() {
        return String::new();
    }
    let start = *pos;
    while *pos < chars.len() {
        let ch = chars[*pos];
        if ch.is_ascii_whitespace() || matches!(ch, '(' | ')' | '"' | ';') {
            break;
        }
        *pos += 1;
    }
    chars[start..*pos].iter().collect()
}

// ── Minimal bootstrap evaluator ───────────────────────────────────
// ── Read-time evaluation (`#.`) ──────────────────────────────────
// `#.` must evaluate its form in the live load environment. The reader's
// read-eval hook is a bare `fn` pointer, so it reaches the current env through
// this thread-local. It is set to point at the loop's `&mut Env` only for the
// duration of each top-level read (during which the outer `env` binding is not
// otherwise touched) and cleared afterwards. Evaluation is single-threaded.
thread_local! {
    static READ_EVAL_ENV: std::cell::Cell<*mut Env> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    // Re-entrancy guard for the package-aware symbol resolver. Resolving a token
    // calls `intern_into_package`, which internally re-reads names via
    // `resolve_sym`/`read-from-string`; those nested reads must take the reader's
    // default name-keyed path, not recurse back into the resolver (bliss-lb6.12).
    static RESOLVING_SYMBOL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Printing an instance may dispatch a user `print-object` method, which needs
    // the live env. Like READ_EVAL_ENV, the print entry points park their `&mut
    // Env` here for the span of one print. PRINT_ESCAPE carries `*print-escape*`
    // (prin1/write => true, princ => false) to the dispatched method; the
    // PRINTING_OBJECT guard stops a method that itself prints another instance
    // from re-entering dispatch (which would alias the parked `&mut`), so nested
    // instances fall back to the `#<CLASS>` form.
    static PRINT_ENV: std::cell::Cell<*mut Env> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    static PRINT_ESCAPE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    static PRINTING_OBJECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Print `val` with the given `*print-escape*` value, dispatching user
/// `print-object` methods for instances by parking `env` for the printer. The
/// parked env is restored afterwards so nested prints compose.
fn print_val_env(val: BlissVal, env: &mut Env, escape: bool, out: &mut String) {
    let prev_env = PRINT_ENV.with(|c| c.replace(env as *mut Env));
    let prev_escape = PRINT_ESCAPE.with(|c| c.replace(escape));
    print_val(val, out);
    PRINT_ENV.with(|c| c.set(prev_env));
    PRINT_ESCAPE.with(|c| c.set(prev_escape));
}

/// `format_val`, but env-aware: dispatches user `print-object` methods.
fn format_val_env(val: BlissVal, env: &mut Env, escape: bool) -> String {
    let mut s = String::new();
    print_val_env(val, env, escape, &mut s);
    s
}

/// If `val` is an instance with an applicable user `print-object` method and a
/// print env is parked (and we are not already inside a print-object call),
/// invoke the method against a fresh string stream and return its output.
/// `escape` is the `*print-escape*` value the method should observe.
fn dispatch_print_object(val: BlissVal, escape: bool) -> Option<String> {
    let ptr = PRINT_ENV.with(|c| c.get());
    if ptr.is_null() || PRINTING_OBJECT.with(|c| c.get()) {
        return None;
    }
    // Safety: the parked pointer is the live `&mut Env` of the print entry point;
    // printing is single-threaded and the outer borrow is dormant for the span of
    // this call (mirrors READ_EVAL_ENV / read_time_eval).
    let env = unsafe { &mut *ptr };
    let stream = bliss_stdlib::make_string_output_stream(NIL).ok()?;
    if !has_applicable_method(env, "PRINT-OBJECT", &[val, stream]) {
        return None;
    }
    PRINTING_OBJECT.with(|c| c.set(true));
    // Bind *PRINT-ESCAPE* so the method's `(when *print-escape* …)` sees the
    // right mode; the guard is restored on every exit path.
    let result = (|| {
        let _esc = resolve_sym("*PRINT-ESCAPE*")
            .map(|s| DynBind::establish(s, if escape { T } else { NIL }));
        invoke_generic_function("PRINT-OBJECT", &[val, stream], env)
    })();
    PRINTING_OBJECT.with(|c| c.set(false));
    result.ok()?;
    let s = bliss_stdlib::get_output_stream_string(stream).ok()?;
    Some(val_as_str(s))
}

/// Hook installed into the stdlib formatter so its `~A`/`~S` render honour user
/// `print-object` methods too. Delegates to [`dispatch_print_object`], which
/// requires a print env to be parked (the FORMAT builtin parks it).
fn stdlib_print_object_hook(val: BlissVal, escape: bool) -> Option<String> {
    if let Some(name) = package_object_name(val) {
        return Some(format!("#<PACKAGE {name}>"));
    }
    // A user PRINT-OBJECT method wins if one applies.
    if let Some(s) = dispatch_print_object(val, escape) {
        return Some(s);
    }
    // `princ` / `~A` of a CONDITION prints its report string (CLHS 9.1); `~S`
    // keeps the default `#<TYPE …>`. Without this, an UNDEFINED-FUNCTION printed
    // as an opaque `#<UNDEFINED-FUNCTION>` with no name — making a failed load
    // (e.g. a missing builtin during an ASDF compile) undiagnosable.
    if !escape && bliss_stdlib::is_instance(val) && !PRINTING_OBJECT.with(|c| c.get()) {
        let ptr = PRINT_ENV.with(|c| c.get());
        if !ptr.is_null() {
            // Safety: mirrors dispatch_print_object — the parked pointer is the
            // live print-entry Env; printing is single-threaded.
            let env = unsafe { &*ptr };
            let is_condition = instance_class_hierarchy_names(val)
                .map(|ns| ns.iter().any(|n| n == "CONDITION"))
                .unwrap_or(false);
            if is_condition {
                PRINTING_OBJECT.with(|c| c.set(true));
                let report = condition_report_string(env, val);
                PRINTING_OBJECT.with(|c| c.set(false));
                if let Some(report) = report {
                    return Some(report);
                }
            }
        }
    }
    None
}

/// Reader hook: resolve a symbol token to the canonical symbol already
/// accessible in the reader's package context, so a bare read inside package P,
/// the qualified spelling `P:NAME`, and `FIND-SYMBOL` all converge on ONE symbol
/// with ONE value/function cell (bliss-lb6.12). `pkg` is the explicit package
/// designator (`None` = the current `*PACKAGE*`).
///
/// It resolves through the SAME read-only `find_symbol_in_package` that
/// `FIND-SYMBOL` uses, so the two can never disagree. Crucially it does NOT
/// intern/fabricate a symbol when the name is not yet accessible: it returns
/// `None`, deferring to the reader's default name-keyed interning. Fabricating
/// here would mint a package-qualified home symbol for names that are really
/// inherited from COMMON-LISP but not present in its symbol table — e.g. special
/// operators like `EVAL-WHEN` — and the interpreter dispatches those by bare
/// name, so a qualified spelling would break them. `None` is also returned when
/// no load environment is active (internal `read-from-string`) or for a bare
/// read whose current package is COMMON-LISP / COMMON-LISP-USER, where the bare
/// name is already the canonical key.
fn reader_symbol_resolver(pkg: Option<&str>, name: &str) -> Option<u32> {
    let ptr = READ_EVAL_ENV.with(|c| c.get());
    if ptr.is_null() || RESOLVING_SYMBOL.with(|c| c.get()) {
        return None;
    }
    // Safety: the load loop parks its live `&mut Env` here for exactly the span
    // of each top-level read; reads never run concurrently and the interpreter is
    // single-threaded, so no other `&mut Env` is live across this call.
    let env = unsafe { &*ptr };
    let pkg_name = match pkg {
        Some(p) => resolve_package_name(env, p),
        None => env.current_package.clone(),
    };
    if pkg.is_none() && (pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER") {
        return None;
    }
    // `find_symbol_in_package` may consult COMMON-LISP-USER / KEYWORD via
    // `resolve_sym`, which re-reads a name and could re-enter this hook; the guard
    // routes those nested reads to the reader's default path.
    RESOLVING_SYMBOL.with(|c| c.set(true));
    let found = find_symbol_in_package(env, &pkg_name, name);
    RESOLVING_SYMBOL.with(|c| c.set(false));
    found.and_then(|(sym, _)| sym.is_symbol().then(|| sym.as_symbol_index()))
}

/// Reader hook: build a `#P"…"` literal as the stdlib's registry-backed pathname
/// (the representation PATHNAMEP / NAMESTRING / LOAD understand), rather than the
/// reader's own minimal PATHNAME object which the rest of the system does not
/// recognise (bliss-lb6). Returns None on a malformed namestring so the reader
/// falls back to its default.
fn reader_pathname_constructor(namestring: BlissVal) -> Option<BlissVal> {
    bliss_stdlib::parse_namestring(namestring, None, None)
        .ok()
        .map(|(pathname, _)| pathname)
}

fn read_time_eval(form: BlissVal) -> Result<BlissVal, BlissError> {
    let ptr = READ_EVAL_ENV.with(|c| c.get());
    if ptr.is_null() {
        return Err(BlissError::StreamError(
            "#. read-eval used outside a load environment".into(),
        ));
    }
    // Safety: `ptr` refers to the load loop's live `&mut Env` for exactly the
    // span of the enclosing read, and reads never run concurrently.
    let env = unsafe { &mut *ptr };
    match eval_form(form, env) {
        Ok(value) => Ok(value),
        Err(err) => {
            if std::env::var_os("BLISS_BFASL_TRACE").is_some() {
                eprintln!("[bfasl] #. failed: {} => {}", format_val(form), err);
            }
            Err(err)
        }
    }
}

/// Read the next form from `chars` at `pos`, evaluating any `#.` read-eval forms
/// in `env`. Returns the absolute position just past the form. Restores the
/// read-eval env pointer afterwards so nested loads compose.
fn read_next_form_at(
    chars: &[char],
    pos: usize,
    env: &mut Env,
) -> Result<(BlissVal, usize), BlissError> {
    let prev = READ_EVAL_ENV.with(|c| c.replace(env as *mut Env));
    let result = reader::read_form_at(chars, pos, 10, true);
    READ_EVAL_ENV.with(|c| c.set(prev));
    result
}

/// `reader::read_from_string`, but with the current load environment parked in
/// `READ_EVAL_ENV` for the span of the read. This activates the package-aware
/// symbol resolver (and `#.` read-eval) so a runtime `READ` / `READ-FROM-STRING`
/// resolves a package-qualified token to the SAME symbol identity the file
/// reader would — otherwise `PKG:NAME` for an inherited/re-exported symbol (e.g.
/// `ASDF:FIND-SYSTEM`, homed in ASDF/SYSTEM) is mis-interned as a fresh symbol
/// homed in PKG, with an empty function cell (bliss).
fn read_from_string_in_env(source: &str, env: &mut Env) -> Result<(BlissVal, usize), BlissError> {
    // Honour the reader specials. `*READ-EVAL*` defaults to T (so `#.` works in a
    // runtime READ, as in CL), and `*READ-BASE*` to 10.
    let read_eval = env
        .lookup_var("*READ-EVAL*")
        .map(|v| !v.is_nil())
        .unwrap_or(true);
    let read_base = env
        .lookup_var("*READ-BASE*")
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum() as u32)
        .filter(|b| (2..=36).contains(b))
        .unwrap_or(10);
    let prev = READ_EVAL_ENV.with(|c| c.replace(env as *mut Env));
    let result = reader::read_from_string_with_base(source, read_base, read_eval);
    READ_EVAL_ENV.with(|c| c.set(prev));
    result
}

fn read_eval_all_env(source: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let _env_roots = LiveEnvRootGuard::new(env);
    reader::set_read_eval_hook(Some(read_time_eval));
    reader::set_symbol_resolver(Some(reader_symbol_resolver));
    reader::set_pathname_constructor(Some(reader_pathname_constructor));
    bliss_stdlib::format::set_print_object_hook(Some(stdlib_print_object_hook));
    register_declared_packages(source);
    let chars: Vec<char> = source.chars().collect();
    // Nesting is checked once for the whole buffer; each form is then read from
    // the shared slice at an advancing position, so loading is O(length) rather
    // than O(forms · length) — the quadratic re-scan that made large files
    // (lib/asdf.lisp) load in tens of seconds (bliss-lb6.5).
    reader::check_nesting(&chars)?;
    let mut pos = 0;
    let mut last = NIL;
    let trace = std::env::var("BLISS_LOAD_TRACE").is_ok();
    let timeit = std::env::var("BLISS_LOAD_TIMING").is_ok();
    let mut form_no = 0usize;
    let mut read_ns = 0u128;
    let mut eval_ns = 0u128;
    loop {
        while pos < chars.len() && chars[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            break;
        }
        if trace {
            form_no += 1;
            let snippet: String = chars[pos..].iter().take(70).collect();
            eprintln!("[LOADTRACE {form_no}] {}", snippet.replace('\n', " "));
        }
        let t0 = timeit.then(std::time::Instant::now);
        let (val, next_pos) = read_next_form_at(&chars, pos, env)?;
        if let Some(t0) = t0 {
            read_ns += t0.elapsed().as_nanos();
        }
        if val == EOF {
            break;
        }
        let t1 = timeit.then(std::time::Instant::now);
        last = bytecode::eval_toplevel(val, env)?;
        if let Some(t1) = t1 {
            eval_ns += t1.elapsed().as_nanos();
        }
        pos = next_pos;
    }
    if timeit {
        eprintln!(
            "[LOADTIMING] read={}ms eval={}ms",
            read_ns / 1_000_000,
            eval_ns / 1_000_000
        );
    }
    Ok(last)
}

fn read_forms_for_compile(source: &str, env: &mut Env) -> Result<Vec<BlissVal>, BlissError> {
    with_eval_context(env, EvalContext::CompileFile, |env| {
        reader::set_read_eval_hook(Some(read_time_eval));
        register_declared_packages(source);
        let chars: Vec<char> = source.chars().collect();
        let mut pos = 0;
        let mut forms = Vec::new();
        loop {
            while pos < chars.len() && chars[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos >= chars.len() {
                break;
            }
            let (val, next_pos) = read_next_form_at(&chars, pos, env).map_err(|e| {
                let line = chars[..pos].iter().filter(|&&ch| ch == '\n').count() + 1;
                BlissError::FileError(format!("compile-file read failed near line {line}: {e}"))
            })?;
            if val == EOF {
                break;
            }
            seed_compile_time_definitions(val, env);
            if let Err(e) = process_compile_toplevel_form(val, env) {
                if std::env::var_os("BLISS_BFASL_TRACE").is_some() {
                    let line = chars[..pos].iter().filter(|&&ch| ch == '\n').count() + 1;
                    eprintln!("[bfasl] ignored compile-time effect near line {line}: {e}");
                }
            }
            forms.extend(compile_file_load_forms(val, env));
            pos = next_pos;
        }
        Ok(forms)
    })
}

fn seed_compile_time_definitions(form: BlissVal, env: &mut Env) {
    if !form.is_cons() {
        return;
    }
    let (op, cdr) = cp(form);
    if op.is_symbol() {
        let op_name = sym_name(op);
        match symbol_leaf_name(&op_name) {
            "DEFVAR" => {
                let (symbol, rest) = cp(cdr);
                if symbol.is_symbol() && env.lookup_var_symbol(symbol).is_none() {
                    let value = if rest.is_cons() {
                        eval_form(cp(rest).0, env).unwrap_or(NIL)
                    } else {
                        NIL
                    };
                    seed_compile_time_binding(env, symbol, value);
                }
                return;
            }
            "DEFPARAMETER" | "DEFCONSTANT" => {
                let (symbol, rest) = cp(cdr);
                if symbol.is_symbol() {
                    let value = if rest.is_cons() {
                        eval_form(cp(rest).0, env).unwrap_or(NIL)
                    } else {
                        NIL
                    };
                    seed_compile_time_binding(env, symbol, value);
                }
                return;
            }
            "QUOTE" | "FUNCTION" => return,
            _ => {}
        }
    }
    let mut cursor = form;
    while cursor.is_cons() {
        let (item, rest) = cp(cursor);
        seed_compile_time_definitions(item, env);
        cursor = rest;
    }
}

fn seed_compile_time_binding(env: &mut Env, symbol: BlissVal, value: BlissVal) {
    env.set_var_symbol(symbol, value);
    let name = sym_name(symbol);
    env.set_var(&name, value);
    // Mirror to the bare name for package-qualified symbols, but never for
    // keywords (see lookup_var_symbol).
    let bare = symbol_bare_name(&name);
    if bare != name && !name.starts_with("KEYWORD:") {
        env.set_var(&bare, value);
    }
}

fn symbol_leaf_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn eval_when_has_situation(situations: BlissVal, target: &str) -> bool {
    list_to_vec(situations)
        .into_iter()
        .any(|situation| situation.is_symbol() && symbol_leaf_name(&sym_name(situation)) == target)
}

fn expand_compile_toplevel_form(mut form: BlissVal, env: &mut Env) -> BlissVal {
    for _ in 0..256 {
        if !form.is_cons() {
            return form;
        }
        let (op, rest) = cp(form);
        if !op.is_symbol() {
            return form;
        }
        let name = sym_name(op);
        let Some(mdef) = lookup_macro(env, &name) else {
            return form;
        };
        match expand_macro(&mdef, rest, env) {
            Ok(expanded) if expanded != form => form = expanded,
            _ => return form,
        }
    }
    form
}

/// Format a UNIX timestamp as SBCL's compile-note date, e.g. "16 AUG 2026
/// 01:04:28 AM" (UTC). Used only for the `*compile-verbose*` progress line, so
/// exact local-time/zone fidelity is not important. Uses the civil-from-days
/// algorithm (days since 1970-01-01).
fn format_compile_note_date(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let (days, sod) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!(
        "{:02} {} {} {:02}:{:02}:{:02} {}",
        d,
        MONTHS[(m - 1) as usize],
        year,
        h12,
        mi,
        s,
        ampm
    )
}

fn compile_file_load_forms(form: BlissVal, env: &mut Env) -> Vec<BlissVal> {
    // DEFINE-CONDITION has a dedicated structural BFASL lowering. Expanding it
    // here would flatten the boot macro into SETQ/DEFCLASS source forms and
    // discard the boundary the portable load action needs.
    if form.is_cons() {
        let (op, _) = cp(form);
        if op.is_symbol() && symbol_leaf_name(&sym_name(op)) == "DEFINE-CONDITION" {
            return vec![form];
        }
    }
    let form = expand_compile_toplevel_form(form, env);
    if !form.is_cons() {
        return vec![form];
    }
    let (op, cdr) = cp(form);
    if !op.is_symbol() {
        return vec![form];
    }
    let op_name = sym_name(op);
    match symbol_leaf_name(&op_name) {
        "PROGN" => list_to_vec(cdr)
            .into_iter()
            .flat_map(|f| compile_file_load_forms(f, env))
            .collect(),
        "EVAL-WHEN" => {
            let (situations, body) = cp(cdr);
            if eval_when_has_situation(situations, "LOAD-TOPLEVEL")
                || eval_when_has_situation(situations, "EXECUTE")
            {
                list_to_vec(body)
                    .into_iter()
                    .flat_map(|f| compile_file_load_forms(f, env))
                    .collect()
            } else {
                Vec::new()
            }
        }
        // A top-level MACROLET establishes local macros for its body, which are
        // then processed as top-level forms (CLHS 3.2.3.1). Register the local
        // macros lexically, expand/splice the body through the same pipeline, then
        // restore the previous bindings so the macros do not leak past the form.
        "MACROLET" => {
            let (defs_form, body) = cp(cdr);
            let mut saved: Vec<(String, Option<MacroDef>)> = Vec::new();
            for def in list_to_vec(defs_form) {
                if !def.is_cons() {
                    continue;
                }
                let (name_form, rest) = cp(def);
                if !name_form.is_symbol() || !rest.is_cons() {
                    continue;
                }
                let (params_form, macro_body) = cp(rest);
                let name = sym_name(name_form);
                let mdef = MacroDef {
                    params_form,
                    body: macro_body,
                    captured_frame: Rc::clone(&env.frame),
                    bytecode: None,
                };
                let prev = env.macros_mut().insert(name.clone(), mdef);
                saved.push((name, prev));
            }
            let result: Vec<BlissVal> = list_to_vec(body)
                .into_iter()
                .flat_map(|f| compile_file_load_forms(f, env))
                .collect();
            for (name, prev) in saved.into_iter().rev() {
                match prev {
                    Some(p) => {
                        env.macros_mut().insert(name, p);
                    }
                    None => {
                        env.macros_mut().remove(&name);
                    }
                }
            }
            result
        }
        _ => vec![form],
    }
}

fn compile_toplevel_form_has_effect(form: BlissVal, env: &Env) -> bool {
    if !form.is_cons() {
        return false;
    }
    let (op, _) = cp(form);
    if !op.is_symbol() {
        return false;
    }
    let name = sym_name(op);
    if macro_defined(env, &name) {
        return true;
    }
    matches!(
        symbol_leaf_name(&name),
        "EVAL-WHEN"
            | "PROGN"
            | "DEFUN"
            | "DEFMACRO"
            | "DEFINE-COMPILER-MACRO"
            | "DEFINE-SYMBOL-MACRO"
            | "DEFSETF"
            | "DEFINE-SETF-EXPANDER"
            | "GET-SETF-EXPANSION"
            | "DEFCLASS"
            | "DEFSTRUCT"
            | "DEFGENERIC"
            | "DEFMETHOD"
            | "DEFPACKAGE"
            | "DEFINE-PACKAGE"
            | "IN-PACKAGE"
            | "MAKE-PACKAGE"
            | "USE-PACKAGE"
            | "EXPORT"
            | "IMPORT"
            | "SHADOW"
            | "SHADOWING-IMPORT"
            | "SETQ"
            | "SETF"
    )
}

fn process_compile_toplevel_form(form: BlissVal, env: &mut Env) -> Result<(), BlissError> {
    if form.is_cons() {
        let (op, cdr) = cp(form);
        if op.is_symbol() && symbol_leaf_name(&sym_name(op)) == "DEFINE-PACKAGE" {
            eval_defpackage(cdr, env)?;
            return Ok(());
        }
    }
    if compile_toplevel_form_has_effect(form, env) {
        eval_form(form, env)?;
    }
    Ok(())
}

fn bundled_asdf_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/asdf.lisp")
        .to_string_lossy()
        .into_owned()
}

/// The bootstrap prelude, embedded at compile time so it is always available
/// regardless of where the binary runs from.
const EMBEDDED_BOOT_LISP: &str = include_str!("../../../lib/boot.lisp");

/// Source of the bootstrap prelude: the file named by BLISS_BOOT_FILE if set
/// (for testing alternate preludes), otherwise the embedded copy.
fn boot_prelude_source() -> Result<String, BlissError> {
    if let Ok(p) = std::env::var("BLISS_BOOT_FILE") {
        return std::fs::read_to_string(&p).map_err(|e| {
            BlissError::FileError(format!("cannot read BLISS_BOOT_FILE {}: {}", p, e))
        });
    }
    Ok(EMBEDDED_BOOT_LISP.to_string())
}

// Reserved for the stage-6 ASDF output-translations wiring; retained so the
// default cache location stays defined in one place until `require`/ASDF is live.
#[allow(dead_code)]
fn default_asdf_output_translations() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/.cache/bliss/asdf/")
}

fn normalize_package_name(name: &str) -> String {
    name.trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase()
}

/// The home package encoded in a symbol's arena key. Mirrors the logic of the
/// SYMBOL-PACKAGE builtin: `KEYWORD:x` → KEYWORD, `PKG::x`/`PKG:x` → PKG, and a
/// bare name → COMMON-LISP.
fn home_package_of_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("KEYWORD:") {
        let _ = rest;
        "KEYWORD".to_string()
    } else if let Some((pkg, _)) = name.rsplit_once("::") {
        pkg.to_string()
    } else if let Some((pkg, _)) = name.rsplit_once(':') {
        pkg.to_string()
    } else {
        "COMMON-LISP".to_string()
    }
}

/// True if some package OTHER than COMMON-LISP genuinely owns (homes) a symbol
/// with this bare name.
///
/// The reader interns every bare symbol under a package-less arena key, so the
/// COMMON-LISP package would otherwise appear to "contain" every bare symbol
/// ever read — including internal symbols of user packages such as UIOP, whose
/// names collide with nothing in ANSI CL but still get a bare arena entry.
/// A symbol is only truly owned by package P when P's own symbol table maps the
/// name to a symbol whose home package (per its arena key) is P itself; imported
/// or inherited symbols don't count. If any non-CL package owns the name, then
/// COMMON-LISP must NOT claim it — that is what keeps FIND-SYMBOL / DO-SYMBOLS
/// over COMMON-LISP from fabricating membership and breaking package algorithms
/// like UIOP's DEFINE-PACKAGE (which compares symbol home packages).
/// The set of bare names homed in some non-CL package — the batch form of
/// [`name_owned_by_noncl_package`], computed once so COMMON-LISP enumeration is
/// O(all-package-symbols) instead of O(symbols × packages) (bliss-gq5.5).
fn noncl_owned_names(_env: &Env) -> std::collections::HashSet<String> {
    let mut owned = std::collections::HashSet::new();
    for pkg in bliss_stdlib::list_all_packages() {
        let Some(pkg_name) = bliss_stdlib::package_name(pkg) else {
            continue;
        };
        if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
            continue;
        }
        for sym in bliss_stdlib::present_symbols(pkg) {
            let name = sym_name(sym);
            if home_package_of_name(&name) == pkg_name {
                owned.insert(symbol_bare_name(&name));
            }
        }
    }
    owned
}

fn name_owned_by_noncl_package(_env: &Env, bare_name: &str) -> bool {
    for pkg in bliss_stdlib::list_all_packages() {
        let Some(pkg_name) = bliss_stdlib::package_name(pkg) else {
            continue;
        };
        if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
            continue;
        }
        if let Some(sym) = bliss_stdlib::find_present_symbol(pkg, bare_name) {
            if home_package_of_name(&sym_name(sym)) == pkg_name {
                return true;
            }
        }
    }
    false
}

/// Resolve a package designator to a canonical package name, following
/// nicknames. Returns the canonical name of the registered package whose name or
/// nicknames match; if none match, returns the normalized designator unchanged
/// (so it can name a package about to be created).
fn resolve_package_name(_env: &Env, raw: &str) -> String {
    let normalized = normalize_package_name(raw);
    // Built-in nicknames that must resolve even before the registry is consulted.
    match normalized.as_str() {
        "CL" => return "COMMON-LISP".to_string(),
        "CL-USER" => return "COMMON-LISP-USER".to_string(),
        _ => {}
    }
    // The registry resolves a name OR nickname to the package; take its canonical
    // name. If unknown, return the normalized designator unchanged so it can name
    // a package about to be created.
    if let Some(pkg) = bliss_stdlib::find_package(&normalized) {
        if let Some(canonical) = bliss_stdlib::package_name(pkg) {
            return canonical;
        }
    }
    normalized
}

/// Short package name for the REPL prompt. Standard packages use their usual
/// CL nicknames (`CL-USER>`, `CL>`); user-defined packages show their full name.
fn prompt_package_name(name: &str) -> &str {
    match name {
        "COMMON-LISP-USER" => "CL-USER",
        "COMMON-LISP" => "CL",
        other => other,
    }
}

fn symbol_bare_name(name: &str) -> String {
    let without_keyword = name.trim_start_matches("KEYWORD:");
    let base = without_keyword
        .rsplit_once("::")
        .map(|(_, tail)| tail)
        .or_else(|| without_keyword.rsplit_once(':').map(|(_, tail)| tail))
        .unwrap_or(without_keyword);
    base.to_uppercase()
}

/// The bare `symbol-name` (CL `SYMBOL-NAME` / `STRING` of a symbol): strips any
/// package prefix but preserves case, unlike `symbol_bare_name` which upper-cases.
fn symbol_name_string(name: &str) -> String {
    let without_keyword = name.strip_prefix("KEYWORD:").unwrap_or(name);
    without_keyword
        .rsplit_once("::")
        .map(|(_, tail)| tail)
        .or_else(|| without_keyword.rsplit_once(':').map(|(_, tail)| tail))
        .unwrap_or(without_keyword)
        .to_string()
}

fn package_status_symbol(status: &str) -> BlissVal {
    resolve_sym(&format!(":{}", status)).unwrap_or(NIL)
}

fn symbol_for_package(pkg_name: &str, bare_name: &str) -> Option<BlissVal> {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    let candidates = if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
        vec![bare_name.clone()]
    } else if pkg_name == "KEYWORD" {
        vec![format!(":{}", bare_name)]
    } else {
        vec![
            format!("{pkg_name}:{bare_name}"),
            format!("{pkg_name}::{bare_name}"),
            bare_name.clone(),
        ]
    };
    candidates
        .into_iter()
        .find_map(|candidate| resolve_sym(&candidate))
}

fn intern_into_package(env: &mut Env, pkg_name: &str, bare_name: &str) -> BlissVal {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    // ANSI INTERN: if a symbol of this name is already accessible in the package
    // — present here, or inherited from a used package — return it unchanged. In
    // particular, an inherited symbol must NOT be re-homed into this package or
    // forked into a new same-named symbol, or a downstream package reaching it
    // via two :use paths sees two conflicting symbols (bliss-lb6.8).
    if let Some((sym, _)) = find_symbol_in_package(env, &pkg_name, &bare_name) {
        return sym;
    }
    let sym = symbol_for_package(&pkg_name, &bare_name)
        .or_else(|| resolve_sym(&bare_name))
        .unwrap_or_else(|| arena_str(&bare_name));
    ensure_package_available(env, &pkg_name, &[]);
    // Record the symbol's home package in its heap cell, pointing at the shared
    // bliss_rt PACKAGE object (bliss-jtc.6 Stage D). A no-op if the symbol is not
    // registry-resident.
    if sym.is_symbol() {
        let pkg = bliss_rt::packages::find_or_create(&pkg_name);
        bliss_rt::symbols::set_symbol_package(sym.as_symbol_index(), pkg);
    }
    // Record membership in the package's registry index (resolving a nickname to
    // the canonical package). New symbols start internal; EXPORT promotes them.
    if let Some(pkg) = bliss_stdlib::find_package(&pkg_name) {
        let _ = bliss_stdlib::add_symbol(pkg, &bare_name, sym, false);
    }
    sym
}

fn find_symbol_in_package(
    env: &Env,
    pkg_name: &str,
    bare_name: &str,
) -> Option<(BlissVal, &'static str)> {
    // Uppercase the name once, then walk the use-graph with a visited set. ASDF's
    // package graph is dense with diamonds (uiop is used by nearly everything and
    // itself uses ~15 uiop/* packages), so an un-memoised DFS re-walked shared
    // packages combinatorially — and FIND-SYMBOL is called once per inherited
    // symbol while a package is being defined. Memoising collapses each lookup
    // to O(reachable packages) (bliss-lb6.5).
    let bare_upper = bare_name.to_uppercase();
    let mut visited: HashSet<String> = HashSet::new();
    find_symbol_in_package_rec(env, pkg_name, &bare_upper, &mut visited)
}

fn find_symbol_in_package_rec(
    env: &Env,
    pkg_name: &str,
    bare_upper: &str,
    visited: &mut HashSet<String>,
) -> Option<(BlissVal, &'static str)> {
    let pkg_name = resolve_package_name(env, pkg_name);
    if !visited.insert(pkg_name.clone()) {
        // Already explored this package on another use-path.
        return None;
    }
    if pkg_name == "COMMON-LISP" {
        // COMMON-LISP owns a bare name only if it is an already-interned symbol
        // that no user package homes. Never intern here: FIND-SYMBOL must have
        // no side effects, and fabricating a symbol would make COMMON-LISP
        // appear to export every name ever read.
        if let Some(idx) = reader::find_symbol_index(bare_upper) {
            if !name_owned_by_noncl_package(env, bare_upper) {
                return Some((BlissVal::from_symbol_index(idx), "EXTERNAL"));
            }
        }
    } else if pkg_name == "COMMON-LISP-USER" {
        if let Some(sym) = resolve_sym(bare_upper) {
            return Some((sym, "EXTERNAL"));
        }
    }
    if pkg_name == "KEYWORD" {
        if let Some(sym) = resolve_sym(&format!(":{}", bare_upper)) {
            return Some((sym, "EXTERNAL"));
        }
    }
    // Borrow the package briefly: resolve the symbol directly, and only clone
    // the small `uses` list (package names, not the symbols map) if we must
    // recurse. Cloning the whole PackageDef here — its entire `symbols` HashMap
    // — on every lookup was a dominant cost while defining ASDF's packages
    // (bliss-gq5.2).
    let uses = {
        let pkg = bliss_stdlib::find_package(&pkg_name)?;
        if let Some(sym) = bliss_stdlib::find_present_symbol(pkg, bare_upper) {
            let status = if bliss_stdlib::is_external_symbol(pkg, bare_upper) {
                "EXTERNAL"
            } else {
                "INTERNAL"
            };
            return Some((sym, status));
        }
        bliss_stdlib::package_use_list(pkg)
            .into_iter()
            .filter_map(bliss_stdlib::package_name)
            .collect::<Vec<_>>()
    };
    for used in &uses {
        if let Some((sym, _)) = find_symbol_in_package_rec(env, used, bare_upper, visited) {
            return Some((sym, "INHERITED"));
        }
    }
    None
}

fn ensure_package_available(_env: &mut Env, name: &str, uses: &[&str]) {
    let pkg = match bliss_stdlib::find_package(name) {
        Some(p) => p,
        None => match bliss_stdlib::make_package(name, &[], &[]) {
            Ok(p) => p,
            Err(_) => return,
        },
    };
    // Record each used package on the use-list, creating a bare placeholder for
    // one not yet defined (the old name-keyed map recorded uses leniently, and
    // ASDF's package graph relies on forward `:use` references resolving later).
    for u in uses {
        let used = match bliss_stdlib::find_package(u) {
            Some(p) => p,
            None => match bliss_stdlib::make_package(u, &[], &[]) {
                Ok(p) => {
                    reader::register_package(u);
                    p
                }
                Err(_) => continue,
            },
        };
        let _ = bliss_stdlib::use_package(&[used], pkg);
    }
    reader::register_package(name);
}

fn plist_get(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, entry_vals) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(cp(entry_vals).0);
            }
        }
        cur = rest;
    }
    None
}

fn plist_entry(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, _) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(entry);
            }
        }
        cur = rest;
    }
    None
}

fn resolve_type_spec(env: &Env, type_spec: BlissVal) -> BlissVal {
    if type_spec.is_symbol() {
        let name = symbol_bare_name(&sym_name(type_spec));
        if let Some(expanded) =
            plist_get(env.lookup_var("*TYPE-DEFINITIONS*").unwrap_or(NIL), &name)
        {
            return expanded;
        }
    }
    type_spec
}

fn condition_definition_entry(env: &Env, type_name: &str) -> Option<BlissVal> {
    plist_entry(
        env.lookup_var("*CONDITION-DEFINITIONS*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    )
}

/// A condition definition: its parent type names and its `(slot, initarg)` slots.
type ConditionDefinition = (Vec<String>, Vec<(String, String)>);

fn builtin_condition_definition(type_name: &str) -> Option<ConditionDefinition> {
    match symbol_bare_name(type_name).as_str() {
        "CONDITION" => Some((vec![], vec![])),
        "SERIOUS-CONDITION" => Some((vec!["CONDITION".into()], vec![])),
        "ERROR" => Some((vec!["SERIOUS-CONDITION".into()], vec![])),
        "WARNING" => Some((vec!["CONDITION".into()], vec![])),
        "STYLE-WARNING" => Some((vec!["WARNING".into()], vec![])),
        "STORAGE-CONDITION" => Some((vec!["SERIOUS-CONDITION".into()], vec![])),
        "SIMPLE-CONDITION" => Some((
            vec!["CONDITION".into()],
            vec![
                ("FORMAT-CONTROL".into(), "FORMAT-CONTROL".into()),
                ("FORMAT-ARGUMENTS".into(), "FORMAT-ARGUMENTS".into()),
            ],
        )),
        "SIMPLE-ERROR" => Some((vec!["ERROR".into(), "SIMPLE-CONDITION".into()], vec![])),
        "SIMPLE-WARNING" => Some((vec!["WARNING".into(), "SIMPLE-CONDITION".into()], vec![])),
        "ARITHMETIC-ERROR" => Some((
            vec!["ERROR".into()],
            vec![
                ("OPERATION".into(), "OPERATION".into()),
                ("OPERANDS".into(), "OPERANDS".into()),
            ],
        )),
        "DIVISION-BY-ZERO" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-OVERFLOW" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-UNDERFLOW" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-INEXACT" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-INVALID-OPERATION" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "CELL-ERROR" => Some((vec!["ERROR".into()], vec![("NAME".into(), "NAME".into())])),
        "UNBOUND-VARIABLE" => Some((vec!["CELL-ERROR".into()], vec![])),
        "UNDEFINED-FUNCTION" => Some((vec!["CELL-ERROR".into()], vec![])),
        "UNBOUND-SLOT" => Some((
            vec!["CELL-ERROR".into()],
            vec![("INSTANCE".into(), "INSTANCE".into())],
        )),
        "TYPE-ERROR" => Some((
            vec!["ERROR".into()],
            vec![
                ("DATUM".into(), "DATUM".into()),
                ("EXPECTED-TYPE".into(), "EXPECTED-TYPE".into()),
            ],
        )),
        "SIMPLE-TYPE-ERROR" => Some((vec!["TYPE-ERROR".into(), "SIMPLE-CONDITION".into()], vec![])),
        "CONTROL-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "FILE-ERROR" => Some((
            vec!["ERROR".into()],
            vec![("PATHNAME".into(), "PATHNAME".into())],
        )),
        "PACKAGE-ERROR" => Some((
            vec!["ERROR".into()],
            vec![("PACKAGE".into(), "PACKAGE".into())],
        )),
        "PARSE-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "PRINT-NOT-READABLE" => Some((
            vec!["ERROR".into()],
            vec![("OBJECT".into(), "OBJECT".into())],
        )),
        "PROGRAM-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "STREAM-ERROR" => Some((
            vec!["ERROR".into()],
            vec![("STREAM".into(), "STREAM".into())],
        )),
        "END-OF-FILE" => Some((vec!["STREAM-ERROR".into()], vec![])),
        "READER-ERROR" => Some((vec!["STREAM-ERROR".into(), "PARSE-ERROR".into()], vec![])),
        _ => None,
    }
}

fn condition_slot_specs(env: &Env, type_name: &str) -> Vec<(String, String)> {
    let mut specs = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some((parents, own_slots)) = builtin_condition_definition(&type_name) {
        for parent in parents {
            specs.extend(condition_slot_specs(env, &parent));
        }
        specs.extend(own_slots);
        return specs;
    }

    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (slots_form, _) = cp(rest2);
        for parent in list_to_vec(parents_form) {
            specs.extend(condition_slot_specs(env, &sym_name(parent)));
        }
        for slot in list_to_vec(slots_form) {
            if slot.is_symbol() {
                let name = symbol_bare_name(&sym_name(slot));
                specs.push((name.clone(), name));
                continue;
            }
            if slot.is_cons() {
                let (slot_name_form, opts_form) = cp(slot);
                let slot_name = symbol_bare_name(&sym_name(slot_name_form));
                let mut initarg = slot_name.clone();
                let opts = list_to_vec(opts_form);
                let mut i = 0;
                while i + 1 < opts.len() {
                    let opt_name = symbol_bare_name(&sym_name(opts[i]));
                    if opt_name == "INITARG" {
                        initarg = symbol_bare_name(&sym_name(opts[i + 1]));
                    }
                    i += 2;
                }
                specs.push((slot_name, initarg));
            }
        }
    }

    specs
}

fn condition_default_initargs(env: &Env, type_name: &str) -> Vec<(String, BlissVal)> {
    let mut defaults = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (_, rest3) = cp(rest2);
        let (options_form, _) = cp(rest3);
        for parent in list_to_vec(parents_form) {
            defaults.extend(condition_default_initargs(env, &sym_name(parent)));
        }
        for option in list_to_vec(options_form) {
            if !option.is_cons() {
                continue;
            }
            let (name_form, values_form) = cp(option);
            if symbol_bare_name(&sym_name(name_form)) != "DEFAULT-INITARGS" {
                continue;
            }
            let values = list_to_vec(values_form);
            let mut i = 0;
            while i + 1 < values.len() {
                defaults.push((symbol_bare_name(&sym_name(values[i])), values[i + 1]));
                i += 2;
            }
        }
    }
    defaults
}

fn ensure_condition_class_registered(env: &Env, type_name: &str) -> Result<BlissVal, BlissError> {
    let type_sym = resolve_sym(&symbol_bare_name(type_name)).unwrap_or(NIL);
    if let Some(class) = bliss_stdlib::find_class(type_sym) {
        return Ok(class);
    }

    let mut parent_names = Vec::new();
    if let Some((builtin_parents, _)) = builtin_condition_definition(type_name) {
        parent_names.extend(builtin_parents);
    } else if let Some(entry) = condition_definition_entry(env, type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, _) = cp(rest);
        for parent in list_to_vec(parents_form) {
            parent_names.push(symbol_bare_name(&sym_name(parent)));
        }
    } else {
        parent_names.push("CONDITION".into());
    }

    let mut supers = Vec::new();
    for parent in parent_names {
        supers.push(ensure_condition_class_registered(env, &parent)?);
    }

    let slot_names: Vec<BlissVal> = condition_slot_specs(env, type_name)
        .into_iter()
        .map(|(slot_name, _)| resolve_sym(&slot_name).unwrap_or(NIL))
        .collect();
    let class = next_stdlib_class_id();
    bliss_stdlib::define_class(type_sym, class, &supers, &slot_names)?;
    Ok(class)
}

/// Class-precedence-list names for a genuine CLOS instance, most-specific-first.
/// Returns `None` for immediate values (fixnums, symbols, conses, …) so callers
/// can fall back to immediate-type handling. Used by `typep` to honour the CLOS
/// class hierarchy for user-defined classes and conditions alike.
fn instance_class_hierarchy_names(object: BlissVal) -> Option<Vec<String>> {
    if !bliss_stdlib::is_instance(object) {
        return None;
    }
    let class = bliss_stdlib::class_of(object);
    let cpl = bliss_stdlib::compute_class_precedence_list(class).ok()?;
    let mut names = Vec::new();
    for class in cpl {
        let name = bliss_stdlib::class_name(class);
        if name.is_symbol() {
            names.push(symbol_bare_name(&sym_name(name)));
        }
    }
    if names.is_empty() { None } else { Some(names) }
}

fn condition_type_hierarchy_names(cond: BlissVal) -> Option<Vec<String>> {
    let class = bliss_stdlib::class_of(cond);
    let cpl = bliss_stdlib::compute_class_precedence_list(class).ok()?;
    let mut names = Vec::new();
    for class in cpl {
        let name = bliss_stdlib::class_name(class);
        if name.is_symbol() {
            names.push(symbol_bare_name(&sym_name(name)));
        }
    }
    if names.iter().any(|name| name == "CONDITION") {
        Some(names)
    } else {
        None
    }
}

fn condition_supertypes(env: &Env, type_name: &str) -> Vec<String> {
    let mut supers = Vec::new();
    let mut cur = plist_get(
        env.lookup_var("*CONDITION-TYPES*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    );
    while let Some(list) = cur {
        for sup in list_to_vec(list) {
            supers.push(symbol_bare_name(&val_as_str(sup)));
        }
        cur = None;
    }
    supers
}

fn condition_type_matches(env: &Env, signaled_type: &str, handler_type: &str) -> bool {
    let signaled = symbol_bare_name(signaled_type);
    let handler = symbol_bare_name(handler_type);
    handler == "T"
        || signaled == handler
        || condition_supertypes(env, &signaled)
            .iter()
            .any(|sup| sup == &handler)
}

fn condition_matches_handler(env: &Env, condition: BlissVal, handler_type: &str) -> bool {
    let handler = symbol_bare_name(handler_type);
    if handler == "T" {
        return true;
    }
    if let Some(hierarchy) = condition_type_hierarchy_names(condition) {
        return hierarchy.iter().any(|name| name == &handler);
    }
    let signaled_type = if condition.is_symbol() {
        sym_name(condition)
    } else {
        val_as_str(condition)
    };
    condition_type_matches(env, &signaled_type, handler_type)
}

fn is_package_value(_env: &Env, value: BlissVal) -> bool {
    // `packagep` / `typep 'package`: strictly a first-class package object
    // (bliss-bhs, CLHS 11.1). A package NAME string is a STRING, not a PACKAGE.
    is_package_object(value)
}

/// The chain of built-in supertypes for a type name (including the type itself),
/// most-specific-first, used by SUBTYPEP. Returns `None` for names that are not
/// built-in atomic types.
fn builtin_supertypes(name: &str) -> Option<&'static [&'static str]> {
    let chain: &'static [&'static str] = match name {
        "FIXNUM" | "BIGNUM" => &["INTEGER", "RATIONAL", "REAL", "NUMBER", "ATOM", "T"],
        "INTEGER" => &["RATIONAL", "REAL", "NUMBER", "ATOM", "T"],
        "RATIO" => &["RATIONAL", "REAL", "NUMBER", "ATOM", "T"],
        "RATIONAL" => &["REAL", "NUMBER", "ATOM", "T"],
        "SINGLE-FLOAT" | "DOUBLE-FLOAT" | "SHORT-FLOAT" | "LONG-FLOAT" => {
            &["FLOAT", "REAL", "NUMBER", "ATOM", "T"]
        }
        "FLOAT" => &["REAL", "NUMBER", "ATOM", "T"],
        "REAL" => &["NUMBER", "ATOM", "T"],
        "NUMBER" => &["ATOM", "T"],
        "CHARACTER" => &["ATOM", "T"],
        "SYMBOL" => &["ATOM", "T"],
        "KEYWORD" => &["SYMBOL", "ATOM", "T"],
        "NULL" => &["SYMBOL", "LIST", "SEQUENCE", "ATOM", "T"],
        "CONS" => &["LIST", "SEQUENCE", "T"],
        "LIST" => &["SEQUENCE", "T"],
        "SIMPLE-STRING" | "BASE-STRING" => &["STRING", "VECTOR", "ARRAY", "SEQUENCE", "ATOM", "T"],
        "STRING" => &["VECTOR", "ARRAY", "SEQUENCE", "ATOM", "T"],
        "VECTOR" => &["ARRAY", "SEQUENCE", "ATOM", "T"],
        "ARRAY" => &["ATOM", "T"],
        "SEQUENCE" => &["T"],
        "HASH-TABLE" | "FUNCTION" | "PACKAGE" | "PATHNAME" | "STREAM" => &["ATOM", "T"],
        "STANDARD-OBJECT" => &["T"],
        "ATOM" => &["T"],
        "T" => &[],
        _ => return None,
    };
    Some(chain)
}

/// SUBTYPEP core: returns `(subtype-p, certain-p)`. Handles built-in atomic type
/// lattices and CLOS class subtyping via the class precedence list; returns
/// `(false, false)` — "unknown" — for relationships it cannot decide.
fn subtypep_relation(t1: BlissVal, t2: BlissVal) -> (bool, bool) {
    let n1 = symbol_bare_name(&sym_name(t1));
    let n2 = symbol_bare_name(&sym_name(t2));
    if n2 == "T" || n1 == "NIL" || n1 == n2 {
        return (true, true);
    }
    // Built-in atomic type lattice.
    if let Some(supers) = builtin_supertypes(&n1) {
        if supers.contains(&n2.as_str()) {
            return (true, true);
        }
        // Both are known built-ins with no relation → definitely not a subtype.
        if builtin_supertypes(&n2).is_some() {
            return (false, true);
        }
    }
    // CLOS class subtyping via the class precedence list.
    if let (Some(c1), Some(c2)) = (bliss_stdlib::find_class(t1), bliss_stdlib::find_class(t2)) {
        if let Ok(cpl) = bliss_stdlib::compute_class_precedence_list(c1) {
            if cpl.contains(&c2) {
                return (true, true);
            }
            return (false, true);
        }
    }
    // Every class is a subtype of STANDARD-OBJECT and T.
    if bliss_stdlib::find_class(t1).is_some() && (n2 == "STANDARD-OBJECT") {
        return (true, true);
    }
    (false, false)
}

/// True if `object`'s length satisfies a vector/array type's size argument list.
/// An empty list or a `*` wildcard matches any length; a fixnum must equal the
/// object's length.
fn vector_length_matches(size_args: &[BlissVal], object: BlissVal) -> bool {
    let Some(size) = size_args.first().copied() else {
        return true;
    };
    if size.is_symbol() && symbol_bare_name(&sym_name(size)) == "*" {
        return true;
    }
    if size.is_fixnum() {
        return bliss_stdlib::length(object)
            .map(|len| len as i64 == size.as_fixnum())
            .unwrap_or(false);
    }
    true
}

fn typep_matches(env: &mut Env, object: BlissVal, type_spec: BlissVal) -> Result<bool, BlissError> {
    let type_spec = resolve_type_spec(env, type_spec);
    if type_spec.is_symbol() {
        let type_name = symbol_bare_name(&sym_name(type_spec));
        // CLOS instances are represented internally as tagged fixnum ids, so the
        // immediate-type predicates below (INTEGER/FIXNUM/NUMBER, …) would alias
        // them. Route instances exclusively through their class hierarchy.
        if bliss_stdlib::is_instance(object) {
            if type_name == "T" {
                return Ok(true);
            }
            let hierarchy = instance_class_hierarchy_names(object);
            let in_hierarchy = hierarchy
                .as_ref()
                .map(|names| names.iter().any(|name| name == &type_name))
                .unwrap_or(false);
            let is_condition = hierarchy
                .as_ref()
                .map(|names| names.iter().any(|name| name == "CONDITION"))
                .unwrap_or(false);
            return Ok(in_hierarchy || (type_name == "STANDARD-OBJECT" && !is_condition));
        }
        let matches = match type_name.as_str() {
            "T" => true,
            "NIL" | "NULL" => object.is_nil(),
            "ATOM" => !object.is_cons(),
            "LIST" => object.is_list(),
            "CONS" => object.is_cons(),
            "SYMBOL" => object.is_symbol(),
            "KEYWORD" => is_keyword_arg(object),
            "STRING" | "SIMPLE-STRING" | "BASE-STRING" => is_string_value(object),
            "NUMBER" | "REAL" => object.is_fixnum() || object.is_single_float(),
            "INTEGER" | "FIXNUM" | "BIGNUM" | "RATIONAL" => object.is_fixnum(),
            // No ratios yet, so RATIONAL collapses to INTEGER above and RATIO
            // matches nothing.
            "RATIO" => false,
            // UNSIGNED-BYTE with no size == (integer 0 *); SIGNED-BYTE with no
            // size == any integer; BIT == (integer 0 1).
            "UNSIGNED-BYTE" => object.is_fixnum() && object.as_fixnum() >= 0,
            "SIGNED-BYTE" => object.is_fixnum(),
            "BIT" => object.is_fixnum() && matches!(object.as_fixnum(), 0 | 1),
            "FLOAT" | "SINGLE-FLOAT" => object.is_single_float(),
            "CHARACTER" => object.is_character(),
            "BOOLEAN" => object.is_nil() || object == T,
            "FUNCTION" | "COMPILED-FUNCTION" => is_function_value(object),
            "PACKAGE" => is_package_value(env, object),
            "HASH-TABLE" => bliss_stdlib::hash_table_count(object).is_ok(),
            "PATHNAME" => bliss_stdlib::namestring(object).is_ok(),
            "STREAM" | "FILE-STREAM" | "SYNONYM-STREAM" => is_stream(object),
            "SIMPLE-VECTOR" => is_simple_vector_value(object),
            // A string is a (vector character); an ARRAY includes both general
            // vectors and strings.
            "VECTOR" | "ARRAY" | "SIMPLE-ARRAY" => is_vector_value(object),
            "SEQUENCE" => object.is_list() || is_vector_value(object),
            other => {
                if let Some(hierarchy) = instance_class_hierarchy_names(object) {
                    hierarchy.iter().any(|name| name == other)
                        || (other == "STANDARD-OBJECT"
                            && !hierarchy.iter().any(|name| name == "CONDITION"))
                } else {
                    other == symbol_bare_name(&val_as_str(object))
                }
            }
        };
        return Ok(matches);
    }

    if !type_spec.is_cons() {
        return Ok(object == type_spec);
    }

    let (head, args) = cp(type_spec);
    let op = symbol_bare_name(&sym_name(head));
    match op.as_str() {
        "OR" => {
            for spec in list_to_vec(args) {
                if typep_matches(env, object, spec)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "AND" => {
            for spec in list_to_vec(args) {
                if !typep_matches(env, object, spec)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "QUOTE" => {
            // A DEFTYPE expander is stored as its body form, e.g. `'(integer 1 N)`
            // → (QUOTE (INTEGER 1 N)); resolve_type_spec hands back that quoted
            // form. Match against the quoted type spec (alexandria's
            // POSITIVE-FIXNUM etc., used by babel).
            let (inner, _) = cp(args);
            typep_matches(env, object, inner)
        }
        "SIMPLE-VECTOR" => {
            // (simple-vector size): a general vector whose length matches SIZE
            // (or `*`). Used e.g. by ASDF's MATCH-CONDITION-P etypecase.
            if !is_simple_vector_value(object) {
                return Ok(false);
            }
            Ok(vector_length_matches(&list_to_vec(args), object))
        }
        "VECTOR" | "SIMPLE-ARRAY" | "ARRAY" => {
            // (vector element-type size) / (array element-type dims): accept a
            // general vector or a string, checking the size/length when given.
            // Element-type is not tracked, so it is treated as wild.
            if !is_vector_value(object) {
                return Ok(false);
            }
            // The size/length is the LAST argument (element-type precedes it).
            let arg_vec = list_to_vec(args);
            let size_args = if arg_vec.len() >= 2 {
                arg_vec[1..].to_vec()
            } else if op == "VECTOR" {
                arg_vec.clone()
            } else {
                Vec::new()
            };
            Ok(vector_length_matches(&size_args, object))
        }
        "MEMBER" => Ok(list_to_vec(args)
            .into_iter()
            .any(|candidate| vals_equal(object, candidate))),
        "EQL" => {
            let (value, _) = cp(args);
            Ok(eql_values(object, value))
        }
        "INTEGER" => {
            if !object.is_fixnum() {
                return Ok(false);
            }
            let bounds = list_to_vec(args);
            let value = object.as_fixnum();
            // A bound is `*`/omitted (unbounded), an integer (inclusive), or a
            // one-element list `(n)` (exclusive). `is_lower` picks the
            // comparison direction. alexandria's ARRAY-INDEX is
            // `(integer 0 (array-dimension-limit))` — an exclusive upper bound,
            // so calling as_fixnum on the `(n)` cons used to panic.
            let bound_ok = |bound: BlissVal, is_lower: bool| -> bool {
                if bound.is_symbol() && symbol_bare_name(&sym_name(bound)) == "*" {
                    return true;
                }
                if bound.is_cons() {
                    let (b, _) = cp(bound);
                    if !b.is_fixnum() {
                        return true;
                    }
                    let n = b.as_fixnum();
                    return if is_lower { value > n } else { value < n };
                }
                if !bound.is_fixnum() {
                    return true;
                }
                let n = bound.as_fixnum();
                if is_lower { value >= n } else { value <= n }
            };
            let lower_ok = bounds
                .first()
                .copied()
                .map(|b| bound_ok(b, true))
                .unwrap_or(true);
            let upper_ok = bounds
                .get(1)
                .copied()
                .map(|b| bound_ok(b, false))
                .unwrap_or(true);
            Ok(lower_ok && upper_ok)
        }
        "UNSIGNED-BYTE" | "SIGNED-BYTE" | "MOD" => {
            if !object.is_fixnum() {
                return Ok(false);
            }
            let value = object.as_fixnum();
            let bounds = list_to_vec(args);
            // The size/modulus argument may be omitted or `*` (wild).
            let size = bounds.first().copied().and_then(|b| {
                if b.is_symbol() && symbol_bare_name(&sym_name(b)) == "*" {
                    None
                } else {
                    Some(b.as_fixnum())
                }
            });
            let ok = match op.as_str() {
                // (unsigned-byte s) == (integer 0 (2^s - 1))
                "UNSIGNED-BYTE" => value >= 0 && size.map(|s| value < (1i64 << s)).unwrap_or(true),
                // (signed-byte s) == (integer -2^(s-1) (2^(s-1) - 1))
                "SIGNED-BYTE" => size
                    .map(|s| {
                        let limit = 1i64 << (s - 1);
                        value >= -limit && value < limit
                    })
                    .unwrap_or(true),
                // (mod n) == (integer 0 (n - 1)); n is required.
                _ => size.map(|n| value >= 0 && value < n).unwrap_or(false),
            };
            Ok(ok)
        }
        "SATISFIES" => {
            let (predicate, _) = cp(args);
            let predicate_name = sym_name(predicate);
            if predicate_name == "FIND-PACKAGE" {
                let designator =
                    if object.is_character() || is_string_value(object) || object.is_symbol() {
                        val_as_str(object)
                    } else {
                        return Ok(false);
                    };
                let pkg_name = normalize_package_name(&designator);
                return Ok(bliss_stdlib::find_package(&pkg_name).is_some()
                    || matches!(
                        pkg_name.as_str(),
                        "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD" | "BLISS-EXT"
                    ));
            }
            Ok(!apply_function(predicate, &[object], env)?.is_nil())
        }
        _ => Ok(false),
    }
}

fn package_symbols(env: &Env, package_name: &str, include_inherited: bool) -> Vec<BlissVal> {
    let package_name = normalize_package_name(package_name);
    // Fast path (bliss-gq5.3): a normal package's own symbols with no
    // inheritance — what DO-EXTERNAL-SYMBOLS drives on every ASDF DEFINE-PACKAGE.
    // Return the values directly: no PackageDef clone, no dedup map, no per-name
    // String clone. (Names within one package are already unique.)
    if !include_inherited
        && package_name != "COMMON-LISP"
        && package_name != "COMMON-LISP-USER"
        && package_name != "KEYWORD"
    {
        return bliss_stdlib::find_package(&package_name)
            .map(bliss_stdlib::present_symbols)
            .unwrap_or_default();
    }
    let mut seen = HashMap::<String, BlissVal>::new();
    // Enumerate the whole interned table once (one lock, no fixed 4096 cap — the
    // old `0..4096` probe truncated large images and re-locked per index,
    // bliss-gq5.5). COMMON-LISP/-USER are the bare-named symbols; KEYWORD the
    // `:`-prefixed ones.
    if package_name == "COMMON-LISP" {
        // Only bare symbols that no user package homes belong to COMMON-LISP.
        // Precompute the set of names owned by a non-CL package ONCE, rather than
        // rescanning every package per symbol (was O(symbols × packages)).
        let owned = noncl_owned_names(env);
        for (idx, name) in bliss_rt::symbols::interned_names() {
            if !name.contains(':') && !owned.contains(&name) {
                seen.entry(name.clone())
                    .or_insert(BlissVal::from_symbol_index(idx));
            }
        }
    } else if package_name == "COMMON-LISP-USER" {
        for (idx, name) in bliss_rt::symbols::interned_names() {
            if !name.contains(':') {
                seen.entry(name.clone())
                    .or_insert(BlissVal::from_symbol_index(idx));
            }
        }
    } else if package_name == "KEYWORD" {
        for (idx, name) in bliss_rt::symbols::interned_names() {
            if name.starts_with(':') {
                seen.entry(name.clone())
                    .or_insert(BlissVal::from_symbol_index(idx));
            }
        }
    }
    // Borrow briefly: merge the package's own symbols into `seen`, and clone
    // only the small `uses` list for the inherited walk — never the whole
    // PackageDef (bliss-gq5.3).
    let uses = {
        if let Some(pkg) = bliss_stdlib::find_package(&package_name) {
            for sym in bliss_stdlib::present_symbols(pkg) {
                seen.entry(symbol_bare_name(&sym_name(sym))).or_insert(sym);
            }
            if include_inherited {
                bliss_stdlib::package_use_list(pkg)
                    .into_iter()
                    .filter_map(bliss_stdlib::package_name)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    };
    for used in &uses {
        for sym in package_symbols(env, used, false) {
            seen.entry(val_as_str(sym)).or_insert(sym);
        }
    }
    seen.into_values().collect()
}

fn resolve_load_path(path: &str) -> Result<String, BlissError> {
    let supplied = Path::new(path);
    if supplied.exists() {
        return Ok(path.to_string());
    }
    if supplied.extension().is_some() {
        return Err(BlissError::FileError(format!(
            "cannot read {}: file does not exist",
            path
        )));
    }

    let source = supplied.with_extension("lisp");
    let bfasl = supplied.with_extension("bfasl");
    let source_exists = source.exists();
    let bfasl_exists = bfasl.exists();

    match (source_exists, bfasl_exists) {
        (true, true) => {
            let source_modified = std::fs::metadata(&source).and_then(|m| m.modified());
            let bfasl_modified = std::fs::metadata(&bfasl).and_then(|m| m.modified());
            if let (Ok(source_modified), Ok(bfasl_modified)) = (source_modified, bfasl_modified) {
                if source_modified > bfasl_modified {
                    return Err(BlissError::FileError(format!(
                        "compiled file {} is older than source {}; recompile or load the source explicitly",
                        bfasl.display(),
                        source.display()
                    )));
                }
            }
            Ok(bfasl.to_string_lossy().into_owned())
        }
        (false, true) => Ok(bfasl.to_string_lossy().into_owned()),
        (true, false) => Ok(source.to_string_lossy().into_owned()),
        (false, false) => Err(BlissError::FileError(format!(
            "cannot read {}: file does not exist",
            path
        ))),
    }
}

fn load_path_into_env(path: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    // ANSI LOAD binds *PACKAGE* (and *READTABLE*) for the dynamic extent of the
    // load, so a file's IN-PACKAGE forms don't leak into the caller — e.g. after
    // `(load "lib/asdf.lisp")` the REPL returns to CL-USER, not ASDF/FOOTER. We
    // snapshot the current package and restore it on the way out (on success and
    // on error), which also keeps `current_package` — the reader's bare-symbol
    // resolution context (bliss-lb6.12) and the REPL prompt — consistent.
    let saved_package = env.current_package.clone();
    // ANSI LOAD also dynamically binds *LOAD-PATHNAME*/*LOAD-TRUENAME* to the
    // file being loaded.  The binding must live in the symbols' value cells:
    // functions called by the loaded file run in their captured lexical
    // environments and cannot see a binding inserted into this Env frame.
    // DynBind below gives nested loads the expected stack discipline and restores
    // both cells on success or error (bliss-lb6.23).
    // Sync the reader's package context to the *current dynamic* value of
    // *PACKAGE*. bliss's reader resolves bare symbols via env.current_package,
    // which a dynamic `(let ((*package* X)) (load …))` binding does not update —
    // so without this, ASDF's DEFINE-OP (which LET-binds *package* to :asdf-user
    // before loading a .asd) would read `defsystem` in the wrong package and hit
    // an undefined function (bliss-lb6.17).
    {
        // A special LET binding of *PACKAGE* (as ASDF's DEFINE-OP does) lives in
        // the symbol's dynamic value cell; the root-frame lexical copy shadows it
        // for name lookup, so consult the cell first, then fall back to the var.
        let cell_val =
            resolve_sym("*PACKAGE*").and_then(|s| global_value_cell(s.as_symbol_index()));
        let pkg_val = cell_val.or_else(|| env.lookup_var("*PACKAGE*"));
        if let Some(pkg_val) = pkg_val {
            let pkg_name = resolve_package_name(env, &val_as_str(pkg_val));
            if !pkg_name.is_empty() {
                env.current_package = pkg_name;
            }
        }
    }
    let result = (|| {
        // Resolve the load path (e.g. prefer a compiled .bfasl sibling, like SBCL)
        // before reading (bfasl-bytecode-unit).
        let resolved_path = resolve_load_path(path)?;
        // Bind *LOAD-PATHNAME*/*LOAD-TRUENAME* to an absolute pathname for the
        // file now that its real on-disk location is known. Keep the guards alive
        // through parsing and evaluation; dropping them restores the caller.
        let _load_path_bindings = load_pathname_value(&resolved_path).and_then(|pathname| {
            let load_pathname = resolve_sym("*LOAD-PATHNAME*")?;
            let load_truename = resolve_sym("*LOAD-TRUENAME*")?;
            Some((
                DynBind::establish(load_pathname, pathname),
                DynBind::establish(load_truename, pathname),
            ))
        });
        let bytes = std::fs::read(&resolved_path)
            .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", resolved_path, e)))?;
        // A Bliss FASL (.bfasl) starts with the BFASL magic — verify and load the
        // compiled unit (bliss-lb6.6); otherwise treat the file as source.
        if bytes.len() >= bliss_rt::bfasl::BFASL_MAGIC.len()
            && bytes[..bliss_rt::bfasl::BFASL_MAGIC.len()] == bliss_rt::bfasl::BFASL_MAGIC
        {
            return load_bfasl_into_env(&bytes, env);
        }
        let contents = String::from_utf8_lossy(&bytes).into_owned();
        with_eval_context(env, EvalContext::Load, |env| {
            read_eval_all_env(&contents, env)
        })
    })();
    if env.current_package != saved_package {
        env.current_package = saved_package.clone();
        env.define_local("*PACKAGE*", package_object(&saved_package));
    }
    result
}

/// Build an absolute pathname value for a file being loaded, for
/// *LOAD-PATHNAME*/*LOAD-TRUENAME*. Canonicalizes to an absolute path when the
/// file exists so ASDF can derive absolute system directories from it.
fn load_pathname_value(path: &str) -> Option<BlissVal> {
    let abs = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    bliss_stdlib::parse_namestring(arena_str(&abs), None, None)
        .ok()
        .map(|(pathname, _)| pathname)
}

/// Load a verified `.bfasl` compiled unit (spec §6.11). A BYTECODE_UNIT is
/// authoritative and executes without invoking the source reader or
/// macroexpander. TOPLEVEL_FORMS is accepted only for legacy artifacts that do
/// not contain a BBU; a malformed BBU never falls back to source.
fn load_bfasl_into_env(bytes: &[u8], env: &mut Env) -> Result<BlissVal, BlissError> {
    let unit = bliss_rt::bfasl::load(bytes)
        .map_err(|e| BlissError::FileError(format!("invalid .bfasl: {e}")))?;
    if let Some(bbu) = unit.section(bliss_rt::bfasl::section::BYTECODE_UNIT) {
        return with_eval_context(env, EvalContext::Load, |env| bytecode::load_bbu(bbu, env));
    }
    let forms = unit
        .section(bliss_rt::bfasl::section::TOPLEVEL_FORMS)
        .ok_or_else(|| {
            BlissError::FileError(
                "bfasl: missing authoritative BYTECODE_UNIT (and no legacy TOPLEVEL_FORMS)".into(),
            )
        })?;
    let src = String::from_utf8_lossy(forms).into_owned();
    with_eval_context(env, EvalContext::Load, |env| read_eval_all_env(&src, env))
}

/// Compile a source unit to a source-independent `.bfasl` byte image. New
/// writers emit only the authoritative BBU plus source/debug metadata; if any
/// load-time form cannot be represented as bytecode, compilation fails rather
/// than producing a hybrid artifact.
fn build_bfasl_from_source(
    source: &str,
    src_path: &str,
    env: &mut Env,
) -> Result<Vec<u8>, BlissError> {
    let forms = read_forms_for_compile(source, env)?;
    let bytecode_unit = bytecode::build_bbu_from_forms(&forms, src_path, source, env)?;
    Ok(bliss_rt::bfasl::BfaslBuilder::new()
        .content_hash(bliss_rt::bfasl::content_hash(source.as_bytes()))
        .section(bliss_rt::bfasl::section::BYTECODE_UNIT, bytecode_unit)
        .section(
            bliss_rt::bfasl::section::SOURCE_MAP,
            src_path.as_bytes().to_vec(),
        )
        .build())
}

/// True if `name` (a module string, matched case-insensitively) is already in
/// `*MODULES*`.
fn module_provided(env: &Env, name: &str) -> bool {
    list_to_vec(env.lookup_var("*MODULES*").unwrap_or(NIL))
        .iter()
        .any(|m| val_as_str(*m).eq_ignore_ascii_case(name))
}

/// Record `name` in `*MODULES*` (pushnew, case-insensitive) so a later REQUIRE is
/// a no-op.
fn record_module(env: &mut Env, name: &str) {
    if !module_provided(env, name) {
        let cur = env.lookup_var("*MODULES*").unwrap_or(NIL);
        env.set_var("*MODULES*", arena_cons(arena_str(name), cur));
    }
}

fn require_module(module: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let normalized = module
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .trim_matches('"')
        .to_uppercase();
    // ANSI: REQUIRE does nothing if the module is already present (whether loaded
    // by a prior REQUIRE or a plain LOAD that PROVIDEd it). Re-loading asdf.lisp
    // over an already-loaded asdf re-defines its packages and errors.
    if module_provided(env, &normalized) {
        return Ok(NIL);
    }
    if normalized == "ASDF" {
        load_path_into_env(&bundled_asdf_path(), env)?;
        // asdf.lisp PROVIDEs itself; belt-and-suspenders in case it did not.
        record_module(env, &normalized);
        return Ok(T);
    }

    let providers = list_to_vec(env.lookup_var("*MODULE-PROVIDER-FUNCTIONS*").unwrap_or(NIL));
    for provider in providers {
        let provided = apply_function(provider, &[arena_str(&normalized)], env)?;
        if !provided.is_nil() {
            record_module(env, &normalized);
            return Ok(provided);
        }
    }

    let candidate = format!("{}.lisp", normalized.to_ascii_lowercase());
    let result = load_path_into_env(&candidate, env)?;
    record_module(env, &normalized);
    Ok(result)
}

#[allow(dead_code)]
fn read_eval_all(source: &str) -> Result<BlissVal, BlissError> {
    let mut env = Env::new(false);
    read_eval_all_env(source, &mut env)
}

fn eval_form(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let form = roots.root(form);
    let form = form.get();
    if form.is_nil() || form == T {
        return Ok(form);
    }
    if form.is_fixnum() || form.is_single_float() || form.is_character() {
        return Ok(form);
    }
    if form.is_heap_object() {
        unsafe {
            let hdr = *(form.as_ptr() as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                bliss_stdlib::register_string(form, &val_as_str(form));
                return Ok(form);
            }
        }
    }
    if form.is_symbol() {
        if let Some(expansion) = env.lookup_symbol_macro(form) {
            if expansion == form {
                return Err(BlissError::Internal(format!(
                    "circular symbol macro expansion for {}",
                    sym_name(form)
                )));
            }
            return eval_form(expansion, env);
        }
        if let Some(val) = env.lookup_var_symbol(form) {
            return Ok(val);
        }
        let name = sym_name(form);
        // Keyword symbols are self-evaluating
        if name.starts_with("KEYWORD:") {
            return Ok(form);
        }
        // Check variable environment
        if let Some(val) = env.lookup_var(&name) {
            return Ok(val);
        }
        if std::env::var_os("BLISS_TRACE_UNDEF_FN").is_some() && form.is_symbol() {
            let idx = form.as_symbol_index();
            let obj = bliss_rt::symbols::symbol_object_ptr(idx);
            // Is `name` currently mapped in the registry to a DIFFERENT index
            // whose value IS bound? If so, `idx` is an orphan duplicate created by
            // a lookup that missed the existing entry — registry corruption.
            let canonical = bliss_rt::symbols::find_index(&name);
            let canonical_bound = canonical
                .map(|ci| bliss_rt::symbols::symbol_value(ci) != Some(bliss_rt::value::UNBOUND));
            eprintln!(
                "[unbound-var] symbol {:?} idx={} object_ptr={:?} value_cell_addr={:?} \
                 find_index={:?} canonical_bound={:?}",
                name,
                idx,
                obj.map(|p| p as *const u8),
                obj.map(|p| (p + 16) as *const u8),
                canonical,
                canonical_bound,
            );
            // Look for other interned symbols sharing this exact name: a bound
            // twin would prove duplicate-interning / registry corruption.
            let twins: Vec<(u32, bool)> = bliss_rt::symbols::interned_names()
                .into_iter()
                .filter(|(_, n)| *n == name)
                .map(|(i, _)| {
                    (
                        i,
                        bliss_rt::symbols::symbol_value(i) != Some(bliss_rt::value::UNBOUND),
                    )
                })
                .collect();
            eprintln!("[unbound-var]   twins(idx,bound) with same name = {twins:?}");
            if std::env::var_os("BLISS_TRACE_UNDEF_BT").is_some() {
                eprintln!("{}", std::backtrace::Backtrace::force_capture());
            }
        }
        return Err(BlissError::UnboundVariable(form));
    }
    if form.is_cons() {
        // Multiple values propagate only out of value-transparent forms (control
        // and binding special forms, and function calls) and genuine multiple-value
        // producers. Every other compound form is a single-value context: after it
        // computes its result, any extra values left in `env.mv` by a nested
        // producer (e.g. the second value of a GETHASH evaluated as an argument)
        // must be discarded so an enclosing multiple-value consumer does not see
        // them leak through. See bliss-2pt.12.
        let preserve = {
            let (car, _) = cp(form);
            if car.is_symbol() {
                mv_form_preserves_values(&sym_name(car), env)
            } else {
                // Lambda application `((lambda ...) ...)` — dispatches through
                // eval_lambda_call, which sets mv from the body's tail form.
                true
            }
        };
        let result = eval_list(form, env)?;
        if !preserve {
            env.clear_mv();
        }
        return Ok(result);
    }
    Ok(form)
}

/// Whether a compound form headed by `name` propagates multiple values (either
/// because it is value-transparent — its value is that of a tail sub-form — or
/// because it is a genuine multiple-value producer). Anything not covered here
/// is treated as a single-value context (see the `eval_form` cons arm).
///
/// The classification is deliberately conservative: over-including a form here
/// merely leaves a latent leak (the historical behaviour), whereas wrongly
/// truncating a producer would drop legitimate secondary values. Dynamically
/// defined functions/macros/generics/methods always preserve — a function's
/// return values come from its own body (eval_lambda_call clears the caller's
/// argument values before evaluating it), and a macro's from its expansion.
fn mv_form_preserves_values(name: &str, env: &Env) -> bool {
    let bare = name.rsplit(':').next().unwrap_or(name);
    if fn_bound(env, name)
        || macro_defined(env, name)
        || env.generics.borrow().contains_key(name)
        || env.methods.borrow().contains_key(name)
        || fn_bound(env, bare)
    {
        return true;
    }
    mv_operator_preserves(name) || mv_operator_preserves(bare)
}

/// The static set of built-in operators whose result carries multiple values.
fn mv_operator_preserves(name: &str) -> bool {
    matches!(
        name,
        // ── genuine multiple-value producers ──
        "VALUES"
            | "VALUES-LIST"
            | "GETHASH"
            | "FLOOR"
            | "CEILING"
            | "TRUNCATE"
            | "ROUND"
            | "FFLOOR"
            | "FCEILING"
            | "FTRUNCATE"
            | "FROUND"
            | "PARSE-NAMESTRING"
            | "PARSE-INTEGER"
            | "SUBTYPEP"
            | "READ-LINE"
            | "READ-FROM-STRING"
            | "INTERN"
            | "FIND-SYMBOL"
            | "MACROEXPAND"
            | "MACROEXPAND-1"
            | "GET-MACRO-CHARACTER"
            | "GET-PROPERTIES"
            | "GET-SETF-EXPANSION"
            | "BLISS-EXT:RUN-PROGRAM"
            | "COMPILE-FILE"
            | "ENSURE-DIRECTORIES-EXIST"
            | "RENAME-FILE"
            | "GET-DECODED-TIME"
            | "DECODE-UNIVERSAL-TIME"
            | "DECODE-FLOAT"
            | "INTEGER-DECODE-FLOAT"
            | "MULTIPLE-VALUE-PROG1"
            // ── value-transparent control / binding special forms ──
            | "IF"
            | "WHEN"
            | "UNLESS"
            | "COND"
            | "CASE"
            | "ECASE"
            | "CCASE"
            | "TYPECASE"
            | "ETYPECASE"
            | "CTYPECASE"
            | "AND"
            | "OR"
            | "PROGN"
            | "LOCALLY"
            | "THE"
            | "EVAL-WHEN"
            | "LET"
            | "LET*"
            | "FLET"
            | "LABELS"
            | "MACROLET"
            | "SYMBOL-MACROLET"
            | "BLOCK"
            | "CATCH"
            | "UNWIND-PROTECT"
            | "PROGV"
            | "TAGBODY"
            | "HANDLER-BIND"
            | "HANDLER-CASE"
            | "RESTART-BIND"
            | "RESTART-CASE"
            | "IGNORE-ERRORS"
            | "DESTRUCTURING-BIND"
            | "MULTIPLE-VALUE-BIND"
            | "MULTIPLE-VALUE-CALL"
            | "EVAL"
            | "FUNCALL"
            | "APPLY"
            | "CALL-NEXT-METHOD"
            | "LOOP"
    )
}

fn canonical_type_name(type_form: BlissVal) -> Result<String, BlissError> {
    if type_form.is_symbol() {
        return Ok(sym_name(type_form)
            .trim_start_matches("COMMON-LISP:")
            .to_string());
    }
    if type_form.is_cons() {
        let (head, rest) = cp(type_form);
        if head.is_symbol() && sym_name(head) == "QUOTE" {
            let (quoted, _) = cp(rest);
            if quoted.is_symbol() {
                return Ok(sym_name(quoted)
                    .trim_start_matches("COMMON-LISP:")
                    .to_string());
            }
        }
    }
    Err(BlissError::TypeError {
        datum: type_form,
        expected: "type specifier".into(),
    })
}

fn value_satisfies_declared_type(type_form: BlissVal, value: BlissVal) -> Result<bool, BlissError> {
    let type_name = canonical_type_name(type_form)?;
    Ok(match type_name.as_str() {
        "T" => true,
        "NIL" | "NULL" => value.is_nil(),
        "BOOLEAN" => value.is_nil() || value == T,
        "SYMBOL" => value.is_symbol(),
        "KEYWORD" => value.is_symbol() && sym_name(value).starts_with("KEYWORD:"),
        "CHARACTER" | "BASE-CHAR" | "STANDARD-CHAR" => value.is_character(),
        "STRING" | "SIMPLE-STRING" | "SIMPLE-BASE-STRING" => is_string_value(value),
        "INTEGER" | "FIXNUM" => value.is_fixnum(),
        "FLOAT" | "SINGLE-FLOAT" | "REAL" => value.is_single_float() || value.is_fixnum(),
        "RATIO" => ratio_parts_val(value).is_some(),
        "NUMBER" => {
            value.is_fixnum() || value.is_single_float() || ratio_parts_val(value).is_some()
        }
        "LIST" => value.is_list(),
        // FUNCTION must agree with FUNCTIONP/TYPEP: an interpreter closure is
        // the cons `(BLISS::CLOSURE . id)`, so a plain `value.is_cons()` check
        // would wrongly reject it. babel's string-to-octets funcalls through
        // `(the function (encoder mapping))`, where the encoder is exactly such
        // a closure.
        "FUNCTION" | "COMPILED-FUNCTION" => is_function_value(value),
        "CONS" => value.is_cons() && !is_function_value(value),
        "ATOM" => !value.is_cons() || is_function_value(value),
        other => {
            if value.is_heap_object() {
                let header = unsafe { *(value.as_ptr() as *const ObjectHeader) };
                match other {
                    "VECTOR" | "SIMPLE-VECTOR" => header.type_id() == type_id::SIMPLE_VECTOR,
                    "HASH-TABLE" => header.type_id() == type_id::HASH_TABLE,
                    _ => false,
                }
            } else {
                false
            }
        }
    })
}

fn eval_when_should_run(situations: BlissVal, env: &Env) -> bool {
    let has_situation = |target: &str| {
        list_to_vec(situations).into_iter().any(|situation| {
            situation.is_symbol() && symbol_leaf_name(&sym_name(situation)) == target
        })
    };

    match env.eval_context {
        EvalContext::CompileFile => has_situation("COMPILE-TOPLEVEL"),
        EvalContext::Load => has_situation("LOAD-TOPLEVEL") || has_situation("EXECUTE"),
        EvalContext::Eval | EvalContext::Repl => has_situation("EXECUTE"),
    }
}

fn eval_form_collecting_values(
    form: BlissVal,
    env: &mut Env,
) -> Result<(BlissVal, Vec<BlissVal>), BlissError> {
    env.clear_mv();
    let primary = eval_form(form, env)?;
    let values = if env.mv_active {
        env.mv.clone()
    } else {
        vec![primary]
    };
    Ok((primary, values))
}

fn cp(val: BlissVal) -> (BlissVal, BlissVal) {
    if !val.is_cons() {
        return (NIL, NIL);
    }
    unsafe {
        let c = val.as_ptr() as *const ConsCell;
        ((*c).car, (*c).cdr)
    }
}

/// A TAGBODY tag is a symbol or an integer. Return its canonical string key,
/// or None if the form is a statement (a cons or other non-tag object).
fn tag_key(form: BlissVal) -> Option<String> {
    if form.is_cons() {
        return None;
    }
    if form.is_symbol() {
        return Some(sym_name(form));
    }
    if form.is_fixnum() {
        return Some(format!("#{}", form.as_fixnum()));
    }
    None
}

fn eval_list(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // Root the operator and argument-list locals in place for the whole dispatch:
    // a relocating minor GC fired by any sub-form evaluation would otherwise leave
    // these (and every `cp(cdr)`-derived cursor read afterwards) dangling
    // (bliss-6b2 #2). StackRoot rewrites the locals in place, so all existing
    // reads keep working.
    let (mut car, mut cdr) = cp(form);
    let _car_root = bliss_rt::gc::StackRoot::new(&mut car);
    let _cdr_root = bliss_rt::gc::StackRoot::new(&mut cdr);
    if car.is_symbol() {
        let name = sym_name(car);

        // Check for macro expansion first (lexical MACROLET macro, else global).
        if let Some(mdef) = lookup_macro(env, &name) {
            let expanded = expand_macro(&mdef, cdr, env)?;
            return eval_form(expanded, env);
        }

        match name.as_str() {
            #[cfg(test)]
            "%FORCE-MINOR-GC-FOR-TEST" => {
                bliss_rt::collect_t0_minor()?;
                return Ok(NIL);
            }
            "QUOTE" => {
                let (q, _) = cp(cdr);
                return Ok(q);
            }
            "BLISS::QUASIQUOTE" => {
                let (template, _) = cp(cdr);
                return eval_quasiquote(template, env);
            }
            "IF" => {
                let (test, mut r) = cp(cdr);
                // `r` (the then/else tail) is held across the test's evaluation;
                // root it in place so a GC there can't dangle it (bliss-6b2 #2).
                let _r_root = bliss_rt::gc::StackRoot::new(&mut r);
                let tv = eval_form(test, env)?;
                let (then, er) = cp(r);
                return if !tv.is_nil() {
                    eval_form(then, env)
                } else if er.is_cons() {
                    let (ef, _) = cp(er);
                    eval_form(ef, env)
                } else {
                    Ok(NIL)
                };
            }
            "PROGN" => return eval_progn(cdr, env),
            // LOCALLY evaluates its body forms in sequence; declarations are
            // not yet honoured by the tree-walker, and leading `(declare …)`
            // forms evaluate to NIL harmlessly, so it reduces to PROGN.
            "LOCALLY" => return eval_progn(cdr, env),
            "DECLARE" => return Ok(NIL),
            "THE" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (type_form, r) = cp(cdr);
                let type_form = roots.root(type_form);
                let (val_form, _) = cp(r);
                let value = eval_form(val_form, env)?;
                let type_form = type_form.get();
                // The quick primitive check handles the common declared types;
                // fall back to full TYPEP so user DEFTYPEs (e.g. babel's
                // `(the array-index ...)` → alexandria's ARRAY-INDEX) and
                // bounded/compound specs are honoured rather than rejected.
                if value_satisfies_declared_type(type_form, value)?
                    || typep_matches(env, value, type_form)?
                {
                    return Ok(value);
                }
                return Err(BlissError::TypeError {
                    datum: value,
                    expected: canonical_type_name(type_form)?,
                });
            }
            "LOOP" => return eval_loop(cdr, env),
            "EVAL-WHEN" => {
                let (situations, body) = cp(cdr);
                return if eval_when_should_run(situations, env) {
                    eval_progn(body, env)
                } else {
                    Ok(NIL)
                };
            }
            "BLOCK" => {
                let (name_form, body) = cp(cdr);
                let name = sym_name(name_form);
                let token = next_control_token("__RETURN_FROM__");
                env.block_stack.push((name, token.clone()));
                let result = eval_progn(body, env);
                env.block_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "RETURN-FROM" => {
                let (name_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let name = sym_name(name_form);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == &name)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!(
                    "RETURN-FROM: no block named {} is currently visible",
                    name
                )));
            }
            "RETURN" => {
                let (val_form, _) = cp(cdr);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == "NIL")
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(
                    "RETURN: no block named NIL is currently visible".into(),
                ));
            }
            "CATCH" => {
                let (tag_form, body) = cp(cdr);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let token = next_control_token("__THROW__");
                env.catch_stack.push((tag, token.clone()));
                let result = eval_progn(body, env);
                env.catch_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "THROW" => {
                let (tag_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .catch_stack
                    .iter()
                    .rev()
                    .find(|(catch_tag, _)| catch_tag == &tag)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!("uncaught throw to {}", tag)));
            }
            "TAGBODY" => {
                // (tagbody {tag | statement}*)
                // Tags are symbols or integers; statements are forms evaluated in
                // order. GO transfers control to a tag; TAGBODY returns NIL.
                let items = list_to_vec(cdr);
                let token = next_control_token("__GO__");
                // Record tag name -> statement index (index just after the tag).
                let mut tag_index: HashMap<String, usize> = HashMap::new();
                let base = env.tag_stack.len();
                for (i, item) in items.iter().enumerate() {
                    if let Some(name) = tag_key(*item) {
                        tag_index.entry(name.clone()).or_insert(i);
                        env.tag_stack.push((name, token.clone()));
                    }
                }
                let mut pc = 0usize;
                let result: Result<(), BlissError> = loop {
                    if pc >= items.len() {
                        break Ok(());
                    }
                    let item = items[pc];
                    if tag_key(item).is_some() {
                        pc += 1;
                        continue;
                    }
                    match eval_form(item, env) {
                        Ok(_) => {
                            pc += 1;
                        }
                        Err(BlissError::Internal(msg)) if msg == token => {
                            let target = val_as_str(take_control_value(&token));
                            match tag_index.get(&target) {
                                Some(idx) => {
                                    pc = *idx;
                                }
                                None => break Err(BlissError::Internal(msg)),
                            }
                        }
                        Err(e) => break Err(e),
                    }
                };
                env.tag_stack.truncate(base);
                match result {
                    Ok(()) => return Ok(NIL),
                    Err(e) => return Err(e),
                }
            }
            "GO" => {
                // (go tag) — tag is not evaluated.
                let (tag_form, _) = cp(cdr);
                let name = match tag_key(tag_form) {
                    Some(n) => n,
                    None => {
                        return Err(BlissError::Internal(
                            "GO: tag must be a symbol or integer".into(),
                        ));
                    }
                };
                if let Some((_, token)) = env
                    .tag_stack
                    .iter()
                    .rev()
                    .find(|(tag_name, _)| tag_name == &name)
                {
                    let token = token.clone();
                    store_control_value(&token, arena_str(&name));
                    return Err(BlissError::Internal(token));
                }
                return Err(BlissError::Internal(format!("GO: no such tag {}", name)));
            }
            "UNWIND-PROTECT" => {
                // (unwind-protect protected cleanup...) — cleanup runs whether the
                // protected form returns normally or exits non-locally.
                let (protected, cleanup) = cp(cdr);
                let result = eval_form(protected, env);
                match result {
                    Ok(v) => {
                        let saved_mv = env.mv.clone();
                        let saved_mv_active = env.mv_active;
                        eval_progn(cleanup, env)?;
                        env.mv = saved_mv;
                        env.mv_active = saved_mv_active;
                        return Ok(v);
                    }
                    Err(e) => match eval_progn(cleanup, env) {
                        Ok(_) => return Err(e),
                        Err(cleanup_exit) => return Err(cleanup_exit),
                    },
                }
            }
            "PRINT" => {
                // (print object &optional stream): leading newline, then the
                // prin1 representation — routed through the output stream.
                // Preserves the historical trailing-newline behaviour that the
                // stage-0 gate depends on.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError("PRINT requires an object".into()));
                }
                // Render first (dispatching print-object allocates), then resolve
                // the stream from the rooted args so a GC in between cannot stale
                // the object or the stream (bliss-6b2 #2).
                let rendered = format_val_env(args[0], env, true);
                let stream = if args.len() > 1 { args[1] } else { NIL };
                let out = resolve_output_stream(stream, env);
                write_str_to(out, "\n")?;
                write_str_to(out, &rendered)?;
                write_str_to(out, "\n")?;
                return Ok(args[0]);
            }
            "PRINC" => {
                // (princ object &optional stream)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError("PRINC requires an object".into()));
                }
                let mut s = String::new();
                princ_val_env(args[0], env, &mut s);
                let stream = if args.len() > 1 { args[1] } else { NIL };
                let out = resolve_output_stream(stream, env);
                write_str_to(out, &s)?;
                return Ok(args[0]);
            }
            "PRIN1" => {
                // (prin1 object &optional stream): the escaped (readable)
                // representation, no surrounding newlines. Returns the object.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError("PRIN1 requires an object".into()));
                }
                let rendered = format_val_env(args[0], env, true);
                let stream = if args.len() > 1 { args[1] } else { NIL };
                let out = resolve_output_stream(stream, env);
                write_str_to(out, &rendered)?;
                return Ok(args[0]);
            }
            "WRITE" => {
                // (write object &key stream escape ...): render OBJECT honouring
                // :escape (default T → prin1-style; NIL → princ-style) to :stream
                // (default *standard-output*). Other keywords are accepted and
                // ignored. Returns the object.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError("WRITE requires an object".into()));
                }
                let mut stream_idx: Option<usize> = None;
                let mut escape = true;
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = symbol_bare_name(&sym_name(args[i]));
                    let val = args[i + 1];
                    match key.as_str() {
                        "STREAM" => stream_idx = Some(i + 1),
                        "ESCAPE" => escape = !val.is_nil(),
                        _ => {}
                    }
                    i += 2;
                }
                // :escape NIL is princ semantics (unquoted strings/chars);
                // otherwise prin1 (readable) semantics. Render before resolving the
                // stream from the rooted args so an allocating render cannot stale
                // it (bliss-6b2 #2).
                let rendered = if escape {
                    format_val_env(args[0], env, true)
                } else {
                    let mut s = String::new();
                    princ_val_env(args[0], env, &mut s);
                    s
                };
                let stream = stream_idx.map(|k| args[k]).unwrap_or(NIL);
                let out = resolve_output_stream(stream, env);
                write_str_to(out, &rendered)?;
                return Ok(args[0]);
            }
            "TERPRI" => {
                let args = list_to_vec(cdr);
                let stream = if args.is_empty() {
                    NIL
                } else {
                    eval_form(args[0], env)?
                };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    invoke_generic_function("STREAM-TERPRI", &[out], env)?;
                    return Ok(NIL);
                }
                bliss_stdlib::stream_terpri(out)?;
                return Ok(NIL);
            }
            "FRESH-LINE" => {
                let args = list_to_vec(cdr);
                let stream = if args.is_empty() {
                    NIL
                } else {
                    eval_form(args[0], env)?
                };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    let r = invoke_generic_function("STREAM-FRESH-LINE", &[out], env)?;
                    return Ok(if r.is_nil() { NIL } else { T });
                }
                let emitted = bliss_stdlib::stream_fresh_line(out)?;
                return Ok(if emitted { T } else { NIL });
            }
            // Buffered output is flushed eagerly, so these are effectively
            // no-ops on ordinary streams; Gray streams still get their protocol
            // method invoked. All three return NIL per ANSI.
            "FINISH-OUTPUT" | "FORCE-OUTPUT" | "CLEAR-OUTPUT" => {
                let args = list_to_vec(cdr);
                let stream = if args.is_empty() {
                    NIL
                } else {
                    eval_form(args[0], env)?
                };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    let gf = match name.as_str() {
                        "FINISH-OUTPUT" => "STREAM-FINISH-OUTPUT",
                        "FORCE-OUTPUT" => "STREAM-FORCE-OUTPUT",
                        _ => "STREAM-CLEAR-OUTPUT",
                    };
                    invoke_generic_function(gf, &[out], env)?;
                }
                return Ok(NIL);
            }
            "WRITE-CHAR" => {
                // (write-char character &optional stream)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "WRITE-CHAR requires a character".into(),
                    ));
                }
                let ch = args[0];
                let stream = if args.len() > 1 { args[1] } else { NIL };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    invoke_generic_function("STREAM-WRITE-CHAR", &[out, ch], env)?;
                } else {
                    bliss_stdlib::stream_write_char(out, ch)?;
                }
                return Ok(ch);
            }
            "MAKE-STRING" => {
                // (make-string size &key initial-element element-type) → a FRESH
                // (non-interned) mutable string, so (setf (char s i) c) / REPLACE
                // can mutate it without aliasing a shared literal.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("MAKE-STRING requires a size".into()));
                }
                let size = args[0];
                if !size.is_fixnum() || size.as_fixnum() < 0 {
                    return Err(BlissError::TypeError {
                        datum: size,
                        expected: "non-negative string size".into(),
                    });
                }
                let mut fill = ' ';
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = args[i];
                    let val = args[i + 1];
                    if key.is_symbol()
                        && symbol_bare_name(&sym_name(key)) == "INITIAL-ELEMENT"
                        && val.is_character()
                    {
                        fill = val.as_char();
                    }
                    i += 2;
                }
                let content: String = std::iter::repeat(fill)
                    .take(size.as_fixnum() as usize)
                    .collect();
                return Ok(bliss_stdlib::make_lisp_string_fresh(&content));
            }
            "MAKE-STRING-OUTPUT-STREAM" => {
                // (make-string-output-stream &key element-type)
                return bliss_stdlib::make_string_output_stream(NIL);
            }
            "CLOSE" => {
                // (close stream &key abort) → T. Closing a non-stream is a no-op.
                let args = eval_args(cdr, env)?;
                let stream = if args.is_empty() { NIL } else { args[0] };
                let mut abort = false;
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = args[i];
                    if key.is_symbol() && symbol_bare_name(&sym_name(key)) == "ABORT" {
                        abort = args[i + 1] != NIL;
                    }
                    i += 2;
                }
                if is_stream(stream) {
                    bliss_stdlib::close(stream, abort)?;
                }
                return Ok(T);
            }
            "GET-OUTPUT-STREAM-STRING" => {
                let (sf, _) = cp(cdr);
                let stream = eval_form(sf, env)?;
                return bliss_stdlib::get_output_stream_string(stream);
            }
            "MAKE-STRING-INPUT-STREAM" => {
                // (make-string-input-stream string &optional start end)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-STRING-INPUT-STREAM requires a string".into(),
                    ));
                }
                let string = args[0];
                let start = if args.len() > 1 && args[1].is_fixnum() {
                    args[1].as_fixnum() as usize
                } else {
                    0
                };
                let end = if args.len() > 2 && args[2].is_fixnum() {
                    Some(args[2].as_fixnum() as usize)
                } else {
                    None
                };
                return bliss_stdlib::make_string_input_stream(string, start, end);
            }
            "READ-CHAR" => {
                // (read-char &optional stream eof-error-p eof-value)
                let args = eval_args(cdr, env)?;
                let stream = if !args.is_empty() { args[0] } else { NIL };
                let eof_error_p = if args.len() > 1 { args[1] } else { T };
                let in_stream = resolve_input_stream(stream, env);
                let (result, at_eof) = if is_gray_stream(in_stream) {
                    let r = invoke_generic_function("STREAM-READ-CHAR", &[in_stream], env)?;
                    let eof = !r.is_character();
                    (r, eof)
                } else {
                    let r = bliss_stdlib::stream_read_char(in_stream)?;
                    let eof = r == EOF;
                    (r, eof)
                };
                if at_eof {
                    if eof_error_p.is_nil() {
                        // Re-read eof-value from the rooted args after the
                        // (allocating) read (bliss-6b2 #2).
                        return Ok(if args.len() > 2 { args[2] } else { NIL });
                    }
                    return Err(BlissError::StreamError("end of file on READ-CHAR".into()));
                }
                return Ok(result);
            }
            "READ" | "READ-PRESERVING-WHITESPACE" => {
                // (read &optional stream eof-error-p eof-value recursive-p)
                let args = eval_args(cdr, env)?;
                let stream = if !args.is_empty() { args[0] } else { NIL };
                let eof_error_p = if args.len() > 1 { args[1] } else { T };
                let in_stream = resolve_input_stream(stream, env);
                match read_one_form_from_stream(in_stream, env)? {
                    Some(form) => return Ok(form),
                    None => {
                        if eof_error_p.is_nil() {
                            // Re-read eof-value from the rooted args after the read.
                            return Ok(if args.len() > 2 { args[2] } else { NIL });
                        }
                        return Err(BlissError::StreamError("end of file on READ".into()));
                    }
                }
            }
            "READ-FROM-STRING" => {
                // (read-from-string string &optional eof-error-p eof-value
                //  &key (start 0) end preserve-whitespace) => object, position
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::ProgramError(
                        "READ-FROM-STRING requires a string".into(),
                    ));
                }
                let vals = eval_forms(args.iter().copied(), env)?;
                let s = val_as_str(vals[0]);
                let chars: Vec<char> = s.chars().collect();
                let eof_error_p = vals.get(1).copied().unwrap_or(T);
                let eof_value = vals.get(2).copied().unwrap_or(NIL);
                // Scan trailing keyword args for :start / :end.
                let mut start = 0usize;
                let mut end = chars.len();
                let mut i = 3;
                while i + 1 < vals.len() {
                    match symbol_bare_name(&sym_name(vals[i])).as_str() {
                        "START" => {
                            start = val_as_str(vals[i + 1])
                                .parse()
                                .ok()
                                .or_else(|| {
                                    vals[i + 1]
                                        .is_fixnum()
                                        .then(|| vals[i + 1].as_fixnum() as usize)
                                })
                                .unwrap_or(0)
                        }
                        "END" if !vals[i + 1].is_nil() => {
                            if vals[i + 1].is_fixnum() {
                                end = vals[i + 1].as_fixnum() as usize;
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
                let start = start.min(chars.len());
                let end = end.min(chars.len()).max(start);
                let sub: String = chars[start..end].iter().collect();
                match read_from_string_in_env(&sub, env) {
                    Ok((form, consumed)) => {
                        env.set_mv(vec![form, BlissVal::from_fixnum((start + consumed) as i64)]);
                        return Ok(form);
                    }
                    Err(_) if eof_error_p.is_nil() => {
                        env.set_mv(vec![eof_value, BlissVal::from_fixnum(end as i64)]);
                        return Ok(eof_value);
                    }
                    Err(e) => return Err(e),
                }
            }
            "UNREAD-CHAR" => {
                // (unread-char character &optional stream)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "UNREAD-CHAR requires a character".into(),
                    ));
                }
                let ch = args[0];
                let stream = if args.len() > 1 { args[1] } else { NIL };
                let in_stream = resolve_input_stream(stream, env);
                if is_gray_stream(in_stream) {
                    invoke_generic_function("STREAM-UNREAD-CHAR", &[in_stream, ch], env)?;
                } else {
                    bliss_stdlib::stream_unread_char(in_stream, ch)?;
                }
                return Ok(NIL);
            }
            "+" => return eval_arith(cdr, env, 0, 0.0, |a, b| a + b, bigrat_add, |a, b| a + b),
            "-" => return eval_arith_sub(cdr, env),
            "*" => return eval_arith(cdr, env, 1, 1.0, |a, b| a * b, bigrat_mul, |a, b| a * b),
            "/" => return eval_arith_div(cdr, env),
            "CONS" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                return Ok(arena_cons(a.get(), b));
            }
            "LIST" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let forms = roots.root(cdr);
                let mut elems = Vec::new();
                while forms.get().is_cons() {
                    let (ef, r) = cp(forms.get());
                    let rest = roots.root(r);
                    elems.push(roots.root(eval_form(ef, env)?));
                    forms.set(rest.get());
                }
                let elems: Vec<BlissVal> = elems.iter().map(|value| value.get()).collect();
                return Ok(vec_to_list(&elems));
            }
            "CAR" | "FIRST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (a, _) = cp(v);
                    return Ok(a);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "CDR" | "REST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    return Ok(d);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "SECOND" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    if d.is_cons() {
                        let (a, _) = cp(d);
                        return Ok(a);
                    }
                }
                return Ok(NIL);
            }
            "THIRD" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                let elems = list_to_vec(v);
                return Ok(if elems.len() >= 3 { elems[2] } else { NIL });
            }
            "ATOM" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { NIL } else { T });
            }
            "NULL" | "NOT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_nil() { T } else { NIL });
            }
            "CONSP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { T } else { NIL });
            }
            "LISTP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_list() { T } else { NIL });
            }
            "ENDP" => {
                // (endp list) — CLHS: T at the end of a proper list (NIL), NIL
                // for a cons, a type error for anything else.
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(T);
                } else if v.is_cons() {
                    return Ok(NIL);
                } else {
                    return Err(BlissError::TypeError {
                        datum: v,
                        expected: "list".into(),
                    });
                }
            }
            "NUMBERP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if is_number_value(v) { T } else { NIL });
            }
            "STRINGP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if is_string_value(v) { T } else { NIL });
            }
            "BOUNDP" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                return Ok(if env.lookup_var(&name).is_some() {
                    T
                } else {
                    NIL
                });
            }
            "SYMBOL-VALUE" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                return match env.lookup_var(&name) {
                    Some(v) => Ok(v),
                    None => Err(BlissError::UnboundVariable(sym)),
                };
            }
            "SYMBOLP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_symbol() { T } else { NIL });
            }
            "KEYWORDP" => {
                // A keyword is a symbol whose home package is KEYWORD. NIL is a
                // symbol but not a keyword.
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if is_keyword_arg(v) { T } else { NIL });
            }
            "CONSTANTP" => {
                // (constantp form &optional environment) — T if FORM always
                // evaluates to the same value: a self-evaluating object, a
                // constant variable (keyword / T / NIL), or a (quote …) form.
                // Conservative for other symbols/compound forms (NIL).
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let is_const = if v.is_nil() || v == T {
                    true
                } else if v.is_cons() {
                    let (h, _) = cp(v);
                    h.is_symbol() && symbol_bare_name(&sym_name(h)) == "QUOTE"
                } else if v.is_symbol() {
                    is_keyword_arg(v) || CONSTANT_VARS.with(|c| c.borrow().contains(&sym_name(v)))
                } else {
                    // Numbers, characters, strings, and other self-evaluating
                    // heap atoms are constant.
                    true
                };
                return Ok(if is_const { T } else { NIL });
            }
            "BLISS-INTERNAL::%MARK-CONSTANT"
            | "BLISS-INTERNAL:%MARK-CONSTANT"
            | "%MARK-CONSTANT" => {
                // (%mark-constant 'name) — record NAME as a DEFCONSTANT so
                // CONSTANTP recognises it. Called by the DEFCONSTANT macro.
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_symbol() {
                    CONSTANT_VARS.with(|c| c.borrow_mut().insert(sym_name(v)));
                }
                return Ok(v);
            }
            "BLISS-INTERNAL::%DEFINE-CONDITION-PORTABLE"
            | "BLISS-INTERNAL:%DEFINE-CONDITION-PORTABLE"
            | "%DEFINE-CONDITION-PORTABLE" => {
                let forms = list_to_vec(cdr);
                if forms.len() != 4 {
                    return Err(BlissError::ProgramError(
                        "%DEFINE-CONDITION-PORTABLE expects four arguments".into(),
                    ));
                }
                let values = eval_forms(forms, env)?;
                return define_condition_portable(values[0], values[1], values[2], values[3], env);
            }
            "DOCUMENTATION" => {
                // (documentation object &optional doc-type) — the interpreter
                // does not retain documentation strings; always NIL. Arguments
                // are still evaluated for their side effects/arity.
                let _ = eval_args(cdr, env)?;
                return Ok(NIL);
            }
            "FDEFINITION" | "SYMBOL-FUNCTION" => {
                let (sf, _) = cp(cdr);
                let spec = eval_form(sf, env)?;
                // A function name is a symbol or `(setf f)`. Return a callable
                // designator: the heap function object when one is bound (so it
                // is FUNCTIONP), otherwise the name itself (funcall/apply accept
                // a symbol / `(setf f)` designator).
                if spec.is_symbol() {
                    let n = sym_name(spec);
                    // A local flet/labels function shadows any global; return a
                    // closure so it survives the flet scope.
                    if let Some(c) = local_fn_closure(env, &n) {
                        return Ok(c);
                    }
                    if let Some(f) = global_fn(&n) {
                        return Ok(f);
                    }
                    if fn_bound(env, &n)
                        || env.methods.borrow().contains_key(&n)
                        || env.generics.borrow().contains_key(&n)
                        || macro_defined(env, &n)
                        || is_builtin_function(&symbol_bare_name(&n))
                    {
                        return Ok(spec);
                    }
                    return Err(BlissError::UndefinedFunction(spec));
                }
                return Ok(spec);
            }
            "FBOUNDP" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                // A name is fbound if it resolves as an ordinary function,
                // a generic function, or a macro.
                let bound = fn_bound(env, &name)
                    || env.methods.borrow().contains_key(&name)
                    || env.generics.borrow().contains_key(&name)
                    || macro_defined(env, &name)
                    || is_builtin_function(&symbol_bare_name(&name));
                return Ok(if bound { T } else { NIL });
            }
            "FMAKUNBOUND" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                // Clear the global heap function cell and the lexical/name-map
                // and macro entries, so the name is no longer fbound.
                if let Some(idx) = bliss_rt::symbols::find_index(&name) {
                    bliss_rt::symbols::set_symbol_function(idx, bliss_rt::value::UNBOUND);
                }
                env.funs_mut().remove(&name);
                env.macros_mut().remove(&name);
                global_macro_remove(&name);
                return Ok(sym);
            }
            "CHAR-CODE" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if !v.is_character() {
                    return Err(BlissError::TypeError {
                        datum: v,
                        expected: "character".into(),
                    });
                }
                return Ok(BlissVal::from_fixnum(v.as_char() as i64));
            }
            "CODE-CHAR" => {
                let (af, _) = cp(cdr);
                let code = num_val(eval_form(af, env)?)? as u32;
                return Ok(match char::from_u32(code) {
                    Some(c) => BlissVal::from_char(c),
                    None => NIL,
                });
            }
            "TYPEP" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (obj_form, r) = cp(cdr);
                let type_form = roots.root(cp(r).0);
                let obj = roots.root(eval_form(obj_form, env)?);
                let raw_type_spec = eval_form(type_form.get(), env)?;
                let matches = typep_matches(env, obj.get(), raw_type_spec)?;
                return Ok(if matches { T } else { NIL });
            }
            "EQ" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                return Ok(if a.get() == b { T } else { NIL });
            }
            "EQL" => {
                // EQL value-compares numbers of the same type, so two distinct
                // heap bignums/ratios with equal value are EQL (bliss-jtc.5).
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                return Ok(if eql_values(a.get(), b) { T } else { NIL });
            }
            "EQUAL" | "EQUALP" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                let eq = if name == "EQUALP" {
                    vals_equalp(a.get(), b)
                } else {
                    vals_equal(a.get(), b)
                };
                return Ok(if eq { T } else { NIL });
            }
            "=" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                return Ok(if numeric_cmp(a.get(), b)? == Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "<" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Less);
            }
            ">" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Greater);
            }
            "<=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Greater);
            }
            ">=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Less);
            }
            "/=" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                // Root the second arg FORM before evaluating the first: a young
                // arg list relocates under the first eval's GC, dangling a bare
                // `bf` (bliss-6b2 #2).
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                return Ok(if numeric_cmp(a.get(), b)? != Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "AND" => {
                // Root the source-form cursor across the sub-evaluations: a young
                // form list (e.g. a macroexpansion) relocates under a minor GC and
                // a bare cursor would dangle (bliss-6b2 #2).
                let roots = bliss_rt::ShadowRootScope::new();
                let mut result = T;
                let c = roots.root(cdr);
                while c.get().is_cons() {
                    let (f, r) = cp(c.get());
                    let rest = roots.root(r);
                    result = eval_form(f, env)?;
                    if result.is_nil() {
                        return Ok(NIL);
                    }
                    c.set(rest.get());
                }
                return Ok(result);
            }
            "OR" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let c = roots.root(cdr);
                while c.get().is_cons() {
                    let (f, r) = cp(c.get());
                    let rest = roots.root(r);
                    let v = eval_form(f, env)?;
                    if !v.is_nil() {
                        return Ok(v);
                    }
                    c.set(rest.get());
                }
                return Ok(NIL);
            }
            "WHEN" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if !tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "UNLESS" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "DESTRUCTURING-BIND" => {
                let (pattern, rest) = cp(cdr);
                let (value_form, body) = cp(rest);
                let value = eval_form(value_form, env)?;
                let parent = Rc::clone(&env.frame);
                return with_child_frame(env, parent, move |env| {
                    // Use the full destructuring binder so &optional/&rest/&key
                    // work in the pattern (not just plain structural matching).
                    bind_macro_param(pattern, value, env, None)?;
                    eval_progn(body, env)
                });
            }
            "COND" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let c = roots.root(cdr);
                while c.get().is_cons() {
                    let (clause, rest) = cp(c.get());
                    let (test, body) = cp(clause);
                    // Root `body` across the test evaluation (a young clause could
                    // relocate) — see AND/OR (bliss-6b2 #2).
                    let body = roots.root(body);
                    let rest = roots.root(rest);
                    let tv = eval_form(test, env)?;
                    if !tv.is_nil() {
                        if body.get().is_nil() {
                            return Ok(tv);
                        }
                        return eval_progn(body.get(), env);
                    }
                    c.set(rest.get());
                }
                return Ok(NIL);
            }
            "VALUES" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let remaining = roots.root(cdr);
                let mut rooted_vals = Vec::new();
                while remaining.get().is_cons() {
                    let (af, rest) = cp(remaining.get());
                    let rest = roots.root(rest);
                    rooted_vals.push(roots.root(eval_form(af, env)?));
                    remaining.set(rest.get());
                }
                let vals: Vec<_> = rooted_vals.iter().map(|value| value.get()).collect();
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "FORMAT" => return eval_format(cdr, env),
            "ERROR" => {
                // Root the evaluated datum and format args across the condition
                // construction, which allocates (bliss-6b2 #2). eval_args keeps
                // the whole arg vector rooted; a slice into it stays rooted too.
                let roots = bliss_rt::ShadowRootScope::new();
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("ERROR".into()));
                }
                let control = roots.root(args[0]);
                let format_args = &args[1..];
                let message = if is_string_value(control.get()) {
                    format_control_message(&val_as_str(control.get()), format_args)?
                } else {
                    val_as_str(control.get())
                };
                // (error datum &rest args): a condition instance is signalled as
                // is; a condition-type symbol is built via MAKE-CONDITION with the
                // remaining args as initargs; a format-control string becomes a
                // SIMPLE-ERROR.
                let condition = match coerce_condition_designator(env, control.get(), format_args)? {
                    Some(condition) => condition,
                    None => make_simple_condition("SIMPLE-ERROR", control.get(), format_args, env)?,
                };
                // The terminal message is the condition's REPORT (e.g. "The value X
                // is not of type Y"), not the bare designator/type name.
                let report = condition_report_string(env, condition).unwrap_or(message);
                match signal_condition_object(condition, env) {
                    Ok(_) => return Err(BlissError::Internal(format!("ERROR: {}", report))),
                    Err(error) => return Err(error),
                }
            }
            "LET" => return eval_let(cdr, env, false),
            "LET*" => return eval_let(cdr, env, true),
            "SETQ" => {
                // Root the pair cursor across value evaluation (a young form list
                // relocates under a minor GC; bliss-6b2 #2). Variable names are
                // symbols (immortal), so only the cursor needs rooting.
                let roots = bliss_rt::ShadowRootScope::new();
                let c = roots.root(cdr);
                let mut result = NIL;
                while c.get().is_cons() {
                    let (sym_form, r) = cp(c.get());
                    let (val_form, r2) = cp(r);
                    let r2 = roots.root(r2);
                    if sym_form.is_symbol() {
                        if let Some(expansion) = env.lookup_symbol_macro(sym_form) {
                            let setf_form = arena_cons(
                                resolve_sym("SETF").unwrap_or(NIL),
                                arena_cons(expansion, arena_cons(val_form, NIL)),
                            );
                            result = eval_form(setf_form, env)?;
                            c.set(r2.get());
                            continue;
                        }
                    }
                    let val = eval_form(val_form, env)?;
                    if sym_form.is_symbol() {
                        env.set_var_symbol(sym_form, val);
                    } else {
                        env.set_var(&sym_name(sym_form), val);
                    }
                    result = val;
                    c.set(r2.get());
                }
                return Ok(result);
            }
            "SETF" => {
                // (setf place value place value ...) — symbol places behave like
                // SETQ; a handful of common accessor places are supported by
                // mutating the target in place. Other places error clearly.
                let roots = bliss_rt::ShadowRootScope::new();
                let c = roots.root(cdr);
                let mut result = NIL;
                while c.get().is_cons() {
                    // Root the place form, value form, evaluated value, and cursor
                    // across the value evaluation and the place mutation (which
                    // itself evaluates sub-forms) — a young place/value relocates
                    // under a minor GC (bliss-6b2 #2).
                    let iroots = bliss_rt::ShadowRootScope::new();
                    let (place, r) = cp(c.get());
                    let (val_form, r2) = cp(r);
                    let place = iroots.root(place);
                    let val_form = iroots.root(val_form);
                    let r2 = iroots.root(r2);
                    let val = iroots.root(eval_form(val_form.get(), env)?);
                    if place.get().is_symbol() {
                        if let Some(expansion) = env.lookup_symbol_macro(place.get()) {
                            let setf_form = arena_cons(
                                resolve_sym("SETF").unwrap_or(NIL),
                                arena_cons(expansion, arena_cons(val_form.get(), NIL)),
                            );
                            result = eval_form(setf_form, env)?;
                            c.set(r2.get());
                            continue;
                        }
                        env.set_var_symbol(place.get(), val.get());
                    } else if place.get().is_cons() {
                        let (accessor, aargs) = cp(place.get());
                        let acc = if accessor.is_symbol() {
                            sym_name(accessor)
                        } else {
                            String::new()
                        };
                        // A user SETF-expander (DEFINE-SETF-EXPANDER / DEFSETF)
                        // takes precedence over the built-in place handling below.
                        if env.setf_expanders.borrow().contains_key(&acc) {
                            result = apply_setf_expansion(place.get(), val.get(), env)?;
                            c.set(r2.get());
                            continue;
                        }
                        let aargs = iroots.root(aargs);
                        let (tgt_form, _) = cp(aargs.get());
                        match acc.as_str() {
                            "VALUES" => {
                                // (setf (values p1 p2 …) form) — distribute the
                                // values FORM produced (captured in env.mv by the
                                // eval above) across the places, defaulting missing
                                // values to NIL.
                                let values = if env.mv_active {
                                    env.mv.clone()
                                } else {
                                    vec![val.get()]
                                };
                                let quote_sym = resolve_sym("QUOTE").unwrap_or(NIL);
                                for (i, pf) in list_to_vec(aargs.get()).into_iter().enumerate() {
                                    let v = values.get(i).copied().unwrap_or(NIL);
                                    let quoted = arena_cons(quote_sym, arena_cons(v, NIL));
                                    let setf_form = vec_to_list(&[
                                        resolve_sym("SETF").unwrap_or(NIL),
                                        pf,
                                        quoted,
                                    ]);
                                    eval_form(setf_form, env)?;
                                }
                            }
                            "CAR" | "FIRST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        // Route through the write barrier: mutating
                                        // an OLD cons's car to a YOUNG value must be
                                        // recorded in the remembered set, else the
                                        // minor GC frees the young referent
                                        // (bliss-6b2 root cause #2).
                                        let cell = tgt.as_ptr() as *mut ConsCell;
                                        bliss_rt::gc::store_ref(
                                            std::ptr::addr_of_mut!((*cell).car),
                                            val.get(),
                                        );
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "CDR" | "REST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        let cell = tgt.as_ptr() as *mut ConsCell;
                                        bliss_rt::gc::store_ref(
                                            std::ptr::addr_of_mut!((*cell).cdr),
                                            val.get(),
                                        );
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "GETHASH" => {
                                // (setf (gethash key table) val)
                                let key = eval_form(tgt_form, env)?;
                                let (tbl_form, _) = cp(cp(aargs.get()).1);
                                let tbl = eval_form(tbl_form, env)?;
                                bliss_stdlib::set_gethash(key, tbl, val.get())?;
                            }
                            "SLOT-VALUE" => {
                                // (setf (slot-value instance slot-name) val)
                                let instance = eval_form(tgt_form, env)?;
                                let (slot_form, _) = cp(cp(aargs.get()).1);
                                let slot = eval_form(slot_form, env)?;
                                write_slot_value(instance, slot, val.get(), env)?;
                            }
                            "SYMBOL-VALUE" => {
                                // (setf (symbol-value sym) val) — assign the
                                // symbol's dynamic value, like SETQ on the symbol.
                                let sym = eval_form(tgt_form, env)?;
                                if sym.is_symbol() {
                                    env.set_var_symbol(sym, val.get());
                                } else {
                                    let name = val_as_str(sym);
                                    env.set_var(&name, val.get());
                                }
                            }
                            "SYMBOL-FUNCTION" | "FDEFINITION" => {
                                // (setf (symbol-function sym) fn) / (setf
                                // (fdefinition sym) fn) — install fn as the
                                // symbol's global function definition. alexandria's
                                // sequences.lisp aliases functions this way
                                // (`(setf (symbol-function 'emptyp) …)`).
                                let sym = eval_form(tgt_form, env)?;
                                if !sym.is_symbol() {
                                    return Err(BlissError::Internal(format!(
                                        "SETF {}: expected a symbol name, got {}",
                                        acc,
                                        format_val(sym)
                                    )));
                                }
                                let fnval = coerce_installed_function(env, sym, val.get());
                                bliss_rt::symbols::set_symbol_function(
                                    sym.as_symbol_index(),
                                    fnval,
                                );
                            }
                            "CHAR" | "SCHAR" | "AREF" | "SVREF" | "ROW-MAJOR-AREF" | "ELT"
                            | "BIT" | "SBIT" => {
                                // (setf (char string index) val) and friends —
                                // mutate a string or vector element in place.
                                // BIT/SBIT index a bit array exactly like AREF.
                                let seq = eval_form(tgt_form, env)?;
                                let (idx_form, _) = cp(cp(aargs.get()).1);
                                let idx = eval_form(idx_form, env)?;
                                if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                                    return Err(BlissError::TypeError {
                                        datum: idx,
                                        expected: "non-negative sequence index".into(),
                                    });
                                }
                                let i = idx.as_fixnum() as usize;
                                if is_string_value(seq) {
                                    bliss_stdlib::string_set_char(seq, i, val.get())?;
                                } else if seq.is_cons() && acc == "ELT" {
                                    // (setf (elt list i) val)
                                    let mut cursor = seq;
                                    for _ in 0..i {
                                        cursor = cp(cursor).1;
                                    }
                                    if cursor.is_cons() {
                                        unsafe {
                                            let cell = cursor.as_ptr() as *mut ConsCell;
                                            bliss_rt::gc::store_ref(
                                                std::ptr::addr_of_mut!((*cell).car),
                                                val.get(),
                                            );
                                        }
                                    } else {
                                        return Err(BlissError::Internal(
                                            "SETF ELT: index past end of list".into(),
                                        ));
                                    }
                                } else {
                                    bliss_stdlib::set_elt(seq, i, val.get())?;
                                }
                            }
                            "FILL-POINTER" => {
                                // (setf (fill-pointer vector) n) — set the active
                                // length of a fill-pointer vector.
                                let vec = eval_form(tgt_form, env)?;
                                if !val.get().is_fixnum() || val.get().as_fixnum() < 0 {
                                    return Err(BlissError::TypeError {
                                        datum: val.get(),
                                        expected: "non-negative fill pointer".into(),
                                    });
                                }
                                bliss_stdlib::set_fill_pointer(vec, val.get().as_fixnum() as usize)?;
                            }
                            "DOCUMENTATION" => {
                                // (setf (documentation object doc-type) val) — the
                                // interpreter does not retain documentation
                                // strings, so accept and ignore. SETF still
                                // returns the assigned value.
                            }
                            other => {
                                let reader_slot = env.classes.borrow().values().find_map(|class| {
                                    class.slots.iter().find_map(|slot| {
                                        let matches_reader = slot
                                            .accessor
                                            .as_ref()
                                            .map(|acc| acc == other)
                                            .unwrap_or(false)
                                            || slot.readers.iter().any(|reader| reader == other)
                                            || slot.writers.iter().any(|writer| writer == other);
                                        if matches_reader {
                                            Some(slot.name.clone())
                                        } else {
                                            None
                                        }
                                    })
                                });
                                if let Some(slot_name) = reader_slot {
                                    let tgt = eval_form(tgt_form, env)?;
                                    write_slot_value(
                                        tgt,
                                        resolve_sym(&slot_name).unwrap_or(NIL),
                                        val.get(),
                                        env,
                                    )?;
                                } else if let Some(writer_sym) = resolve_sym(
                                    &setf_writer_symbol_name(other),
                                )
                                .filter(|s| bytecode::is_registered(s.as_symbol_index()))
                                {
                                    // A source-free `(defun (setf place) …)` writer
                                    // installed from a .bfasl: dispatch its
                                    // registered bytecode with the new value first,
                                    // then the place's subforms.
                                    let mut args = RootedVals::new(vec![val.get()]);
                                    for v in eval_args(aargs.get(), env)?.iter() {
                                        args.push(*v);
                                    }
                                    if let Some(res) = bytecode::call_registered(
                                        writer_sym.as_symbol_index(),
                                        &args,
                                        writer_sym,
                                        env,
                                    ) {
                                        res?;
                                    }
                                } else if let Some((params_form, body)) =
                                    callable_body(env, &format!("(SETF {})", other))
                                {
                                    // A user-defined writer: (defun (setf place) …).
                                    // Call it as (funcall #'(setf place) NEW args…):
                                    // the new value is the first argument, then the
                                    // place's own subforms (CLHS 5.1.2.9).
                                    let mut args = RootedVals::new(vec![val.get()]);
                                    for v in eval_args(aargs.get(), env)?.iter() {
                                        args.push(*v);
                                    }
                                    eval_lambda_call(
                                        env,
                                        params_form,
                                        body,
                                        &args,
                                        Rc::clone(&env.frame),
                                    )?;
                                } else if {
                                    let key = format!("(SETF {})", other);
                                    env.methods.borrow().contains_key(&key)
                                        || env.generics.borrow().contains_key(&key)
                                } {
                                    // A (setf place) *generic function* (defmethod
                                    // (setf place) …): dispatch it with the new
                                    // value first, then the place's subforms.
                                    let key = format!("(SETF {})", other);
                                    let mut args = RootedVals::new(vec![val.get()]);
                                    for v in eval_args(aargs.get(), env)?.iter() {
                                        args.push(*v);
                                    }
                                    invoke_generic_function(&key, &args, env)?;
                                } else {
                                    return Err(BlissError::Internal(format!(
                                        "SETF: unsupported place ({} ...)",
                                        other
                                    )));
                                }
                            }
                        }
                    }
                    result = val.get();
                    c.set(r2.get());
                }
                return Ok(result);
            }
            "DEFUN" => return eval_defun(cdr, env),
            "DEFSETF" => {
                // Short form (defsetf access-fn update-fn) registers an expander
                // that stores via (update-fn arg… new). The long form
                // (defsetf access-fn lambda-list (store) . body) is treated like a
                // DEFINE-SETF-EXPANDER whose body yields the store form.
                let (name_form, rest) = cp(cdr);
                let (second, more) = cp(rest);
                if second.is_symbol() {
                    // Short form: update-fn is a symbol.
                    let update_fn = second;
                    return eval_defsetf_short(name_form, update_fn, env);
                }
                // Long form: (defsetf name (args…) (store) body…). Build an
                // equivalent define-setf-expander.
                return eval_defsetf_long(name_form, second, more, env);
            }
            "DEFINE-SETF-EXPANDER" => return eval_define_setf_expander(cdr, env),
            "GET-SETF-EXPANSION" => {
                // (get-setf-expansion place &optional environment) → five values.
                let (place_form, _) = cp(cdr);
                let place = eval_form(place_form, env)?;
                let ex = get_setf_expansion(place, env)?;
                env.set_mv(vec![
                    vec_to_list(&ex.temps),
                    vec_to_list(&ex.vals),
                    vec_to_list(&ex.stores),
                    ex.store_form,
                    ex.access_form,
                ]);
                return Ok(vec_to_list(&ex.temps));
            }
            "FLET" | "LABELS" => return eval_flet(cdr, env),
            "DEFMACRO" => return eval_defmacro(cdr, env),
            "DEFINE-SYMBOL-MACRO" => return eval_define_symbol_macro(cdr, env),
            "DEFINE-COMPILER-MACRO" => return eval_define_compiler_macro(cdr, env),
            "MACROLET" => return eval_macrolet(cdr, env),
            "SYMBOL-MACROLET" => return eval_symbol_macrolet(cdr, env),
            "MACROEXPAND-1" => return eval_macroexpand(cdr, env, true),
            "MACROEXPAND" => return eval_macroexpand(cdr, env, false),
            "DEFCLASS" => return eval_defclass(cdr, env),
            "DEFSTRUCT" => return eval_defstruct(cdr, env),
            "DEFGENERIC" => return eval_defgeneric(cdr, env),
            "DEFMETHOD" => return eval_defmethod(cdr, env),
            "MAKE-INSTANCE" => return eval_make_instance(cdr, env),
            "FUNCTION" => {
                let (name_form, _) = cp(cdr);
                if name_form.is_symbol() {
                    let fn_name = sym_name(name_form);
                    // A local flet/labels function shadows any global; return a
                    // closure so it stays callable outside the flet scope.
                    if let Some(c) = local_fn_closure(env, &fn_name) {
                        return Ok(c);
                    }
                    if fn_bound(env, &fn_name) {
                        return Ok(name_form); // return the symbol as a function designator
                    }
                }
                // (function (lambda (params) body...)) — create a closure
                if name_form.is_cons() {
                    let (lh, lr) = cp(name_form);
                    if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
                        let (params_form, body) = cp(lr);
                        // Capture the current lexical environment
                        let closure = Closure {
                            params_form,
                            body,
                            captured_frame: Rc::clone(&env.frame),
                        };
                        let id = next_closure_id();
                        env.closures.borrow_mut().insert(id, closure);
                        // Return a tagged closure reference as (BLISS::CLOSURE . id)
                        let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                        return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
                    }
                }
                return Ok(name_form);
            }
            "LAMBDA" => {
                // Bare (lambda (params) body...) — create a closure
                let (params_form, body) = cp(cdr);
                let closure = Closure {
                    params_form,
                    body,
                    captured_frame: Rc::clone(&env.frame),
                };
                let id = next_closure_id();
                env.closures.borrow_mut().insert(id, closure);
                let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
            }
            "EVAL" => {
                // (eval form): evaluate the argument to obtain the form, then
                // evaluate that form. CL specifies the null lexical environment;
                // the tree-walker evaluates in the current env, which suffices
                // for the global/dynamic forms ASDF passes to EVAL.
                let (form_form, _) = cp(cdr);
                let form = eval_form(form_form, env)?;
                return eval_form(form, env);
            }
            "FUNCALL" => {
                let (fn_form, args_form) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let args = eval_args(args_form, env)?;
                return apply_function(fn_val, &args, env);
            }
            "APPLY" => {
                // Root the function value, the source spine, and the accumulated
                // arg vector across each evaluation (bliss-6b2 #2): every
                // eval_form allocates and can relocate the callee, the remaining
                // unread argument forms, and the already-collected args.
                let roots = bliss_rt::ShadowRootScope::new();
                let (fn_form, rest0) = cp(cdr);
                let rest = roots.root(rest0);
                let fn_val = roots.root(eval_form(fn_form, env)?);
                let mut args: Vec<BlissVal> = Vec::new();
                let _ag = VecRootGuard::new(&mut args);
                while rest.get().is_cons() {
                    let (item, r) = cp(rest.get());
                    let r = roots.root(r);
                    let is_last = !r.get().is_cons();
                    let v = eval_form(item, env)?;
                    rest.set(r.get());
                    if is_last {
                        // Last arg is a list to spread.
                        args.extend(list_to_vec(v));
                    } else {
                        args.push(v);
                    }
                }
                return apply_function(fn_val.get(), &args, env);
            }
            "LENGTH" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(BlissVal::from_fixnum(bliss_stdlib::length(v)? as i64));
            }
            "ELT" => {
                // (elt sequence index) — works on lists, vectors, and strings.
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "ELT requires a sequence and an index".into(),
                    ));
                }
                let seq = args[0];
                let idx = args[1];
                if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                    return Err(BlissError::TypeError {
                        datum: idx,
                        expected: "non-negative sequence index".into(),
                    });
                }
                return bliss_stdlib::elt(seq, idx.as_fixnum() as usize);
            }
            "AREF" | "SVREF" | "ROW-MAJOR-AREF" | "BIT" | "SBIT" => {
                // One-dimensional array/vector/string access — delegates to elt.
                // BIT/SBIT read a bit array exactly like AREF (rank-1 here).
                let args = eval_args(cdr, env)?;
                if args.len() != 2 {
                    return Err(BlissError::ProgramError(format!(
                        "{}: only one-dimensional arrays are supported",
                        name
                    )));
                }
                let arr = args[0];
                let idx = args[1];
                if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                    return Err(BlissError::TypeError {
                        datum: idx,
                        expected: "non-negative array index".into(),
                    });
                }
                return bliss_stdlib::elt(arr, idx.as_fixnum() as usize);
            }
            "VECTOR" => {
                // (vector &rest elements) → a fresh simple-vector.
                let elems = eval_args(cdr, env)?;
                return Ok(bliss_stdlib::build_simple_vector(&elems));
            }
            "BLISS-INTERNAL::%MAKE-COMPLEX-VECTOR"
            | "BLISS-INTERNAL:%MAKE-COMPLEX-VECTOR"
            | "%MAKE-COMPLEX-VECTOR" => {
                // (%make-complex-vector size fill-pointer adjustable-p &optional
                // initial-element) — build a rank-1 fill-pointer / adjustable
                // vector. Called by MAKE-ARRAY (boot.lisp) for the
                // :fill-pointer/:adjustable cases.
                let args = eval_args(cdr, env)?;
                if args.len() < 3 {
                    return Err(BlissError::Internal(
                        "%MAKE-COMPLEX-VECTOR requires size, fill-pointer, adjustable".into(),
                    ));
                }
                let size = if args[0].is_fixnum() {
                    args[0].as_fixnum().max(0) as usize
                } else {
                    0
                };
                let fp = if args[1].is_fixnum() {
                    args[1].as_fixnum().max(0) as usize
                } else {
                    // :fill-pointer T (or absent) ⇒ full length.
                    size
                };
                let adjustable = !args[2].is_nil();
                let iel = if args.len() > 3 { args[3] } else { NIL };
                let elems = vec![iel; size];
                return Ok(bliss_stdlib::build_complex_vector(
                    &elems, size, fp, adjustable,
                ));
            }
            "VECTOR-PUSH" => {
                // (vector-push new-element vector) → index used, or NIL if full.
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "VECTOR-PUSH requires an element and a vector".into(),
                    ));
                }
                let val = args[0];
                let vec = args[1];
                return bliss_stdlib::vector_push(vec, val);
            }
            "VECTOR-PUSH-EXTEND" => {
                // (vector-push-extend new-element vector &optional extension)
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "VECTOR-PUSH-EXTEND requires an element and a vector".into(),
                    ));
                }
                let val = args[0];
                let vec = args[1];
                let ext = if args.len() > 2 {
                    let e = args[2];
                    e.is_fixnum().then(|| e.as_fixnum().max(0) as usize)
                } else {
                    None
                };
                return bliss_stdlib::vector_push_extend(vec, val, ext);
            }
            "VECTOR-POP" => {
                // (vector-pop vector) → the element at the (decremented) fill ptr.
                let (vf, _) = cp(cdr);
                let vec = eval_form(vf, env)?;
                return bliss_stdlib::vector_pop(vec);
            }
            "FILL-POINTER" => {
                // (fill-pointer vector) → its fill pointer (an integer).
                let (vf, _) = cp(cdr);
                let vec = eval_form(vf, env)?;
                if bliss_stdlib::is_complex_vector(vec) {
                    return Ok(BlissVal::from_fixnum(
                        bliss_stdlib::cvec_fill_pointer(vec) as i64
                    ));
                }
                return Err(BlissError::TypeError {
                    datum: vec,
                    expected: "vector with a fill pointer".into(),
                });
            }
            "VECTORP" | "SIMPLE-VECTOR-P" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if is_vector_value(v) { T } else { NIL });
            }
            "ARRAYP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                // bliss arrays are simple-vectors and strings.
                return Ok(if is_vector_value(v) { T } else { NIL });
            }
            "ADJUSTABLE-ARRAY-P" => {
                // Only a COMPLEX_ARRAY created :adjustable is adjustable.
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let adj = bliss_stdlib::is_complex_vector(v) && bliss_stdlib::cvec_adjustable(v);
                return Ok(if adj { T } else { NIL });
            }
            "ARRAY-HAS-FILL-POINTER-P" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if bliss_stdlib::is_complex_vector(v) {
                    T
                } else {
                    NIL
                });
            }
            "ARRAY-DISPLACEMENT" => {
                // bliss has no displaced arrays: (values nil 0).
                let (af, _) = cp(cdr);
                eval_form(af, env)?;
                env.set_mv(vec![NIL, BlissVal::from_fixnum(0)]);
                return Ok(NIL);
            }
            "ARRAY-ELEMENT-TYPE" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                // Strings hold CHARACTER; simple-vectors hold T.
                let ty = if is_string_value(v) { "CHARACTER" } else { "T" };
                return Ok(resolve_sym(ty).unwrap_or(T));
            }
            "ARRAY-RANK" => {
                let (af, _) = cp(cdr);
                let _v = eval_form(af, env)?;
                // All bliss arrays are one-dimensional.
                return Ok(BlissVal::from_fixnum(1));
            }
            "ARRAY-DIMENSIONS" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let len = bliss_stdlib::length(v)? as i64;
                return Ok(vec_to_list(&[BlissVal::from_fixnum(len)]));
            }
            "APPEND" => {
                // Root the accumulated elements AND the source-form spine across
                // the arg-eval loop (bliss-6b2 #2): a later argument's evaluation
                // allocates and can relocate both the Vec-resident list elements
                // and the not-yet-evaluated source forms.
                let roots = bliss_rt::ShadowRootScope::new();
                let mut all: Vec<bliss_rt::ShadowRoot> = Vec::new();
                if !cdr.is_cons() {
                    return Ok(NIL);
                }
                let c = roots.root(cdr);
                while c.get().is_cons() {
                    let (item_form, rest) = cp(c.get());
                    let rest = roots.root(rest);
                    let is_last = !rest.get().is_cons();
                    let v = eval_form(item_form, env)?;
                    c.set(rest.get());
                    if is_last && v.is_nil() {
                        continue;
                    }
                    all.extend(list_to_vec(v).into_iter().map(|e| roots.root(e)));
                }
                let all: Vec<BlissVal> = all.iter().map(|r| r.get()).collect();
                return Ok(vec_to_list(&all));
            }
            "REVERSE" => {
                // Delegate to the stdlib so lists, vectors, and strings all
                // reverse with the correct result type (AGENTS.md: don't
                // reimplement sequence ops in the interpreter).
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return bliss_stdlib::reverse(v);
            }
            "NTH" => {
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("NTH requires an index and a list".into()));
                }
                let idx = num_val(args[0])? as usize;
                let elems = list_to_vec(args[1]);
                return Ok(if idx < elems.len() { elems[idx] } else { NIL });
            }
            "MAKE-HASH-TABLE" => {
                // (make-hash-table &key test size ...) — honor :test, evaluate
                // and ignore the rest. Backed by the real stdlib hash table.
                let mut test = bliss_stdlib::HashTest::Eql;
                let roots = bliss_rt::ShadowRootScope::new();
                let c = roots.root(cdr);
                while c.get().is_cons() {
                    let (kw_form, r) = cp(c.get());
                    if !r.is_cons() {
                        break;
                    }
                    let (vf, r2) = cp(r);
                    let vf = roots.root(vf);
                    let r2 = roots.root(r2);
                    // Evaluate the keyword too: called directly the keyword is a
                    // self-evaluating symbol, but reached through apply_function's
                    // synthesize path (funcall/apply/bytecode CallNamed) every
                    // argument arrives quoted as `(quote :test)` (bliss-x5y.2).
                    let kw = eval_form(kw_form, env)?;
                    let v = eval_form(vf.get(), env)?;
                    if kw.is_symbol() {
                        let kn = sym_name(kw);
                        if kn.strip_prefix("KEYWORD:").unwrap_or(&kn) == "TEST" {
                            let tn = sym_name(v);
                            let tb = tn.strip_prefix("KEYWORD:").unwrap_or(&tn).to_uppercase();
                            test = match tb.as_str() {
                                "EQ" => bliss_stdlib::HashTest::Eq,
                                "EQUAL" => bliss_stdlib::HashTest::Equal,
                                "EQUALP" => bliss_stdlib::HashTest::Equalp,
                                _ => bliss_stdlib::HashTest::Eql,
                            };
                        }
                    }
                    c.set(r2.get());
                }
                let opts = bliss_stdlib::MakeHashTableOptions {
                    test,
                    ..Default::default()
                };
                return bliss_stdlib::make_hash_table(&opts);
            }
            "GETHASH" => {
                // (gethash key table &optional default) -> value; sets the
                // second value to the present-p flag.
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("GETHASH requires a key and a table".into()));
                }
                let key = args[0];
                let tbl = args[1];
                let default = if args.len() >= 3 { args[2] } else { NIL };
                let (val, present) = bliss_stdlib::gethash(key, tbl, default)?;
                env.set_mv(vec![val, if present { T } else { NIL }]);
                return Ok(val);
            }
            "BLISS::PUT-GETHASH" => {
                // Store primitive for bytecode-lowered `(setf (gethash key table)
                // value)` (bliss-x5y.2). Arguments arrive already evaluated —
                // value, key, table, in the SETF handler's value-first order —
                // through apply_function's synthesize path. Returns the value.
                let args = eval_args(cdr, env)?;
                let val = args.first().copied().unwrap_or(NIL);
                let key = args.get(1).copied().unwrap_or(NIL);
                let tbl = args.get(2).copied().unwrap_or(NIL);
                bliss_stdlib::set_gethash(key, tbl, val)?;
                // Re-read from the rooted args: set_gethash may rehash/allocate.
                return Ok(args.first().copied().unwrap_or(NIL));
            }
            "BLISS::SET-AREF" => {
                // Store primitive for bytecode-lowered `(setf (aref|svref|char|
                // schar|row-major-aref|elt seq index) value)`. Arguments arrive
                // already evaluated (seq, index, value); the element is mutated in
                // place and the value returned (SETF semantics).
                let args = eval_args(cdr, env)?;
                let seq = args.first().copied().unwrap_or(NIL);
                let idx = args.get(1).copied().unwrap_or(NIL);
                let val = args.get(2).copied().unwrap_or(NIL);
                if !idx.is_fixnum() || idx.as_fixnum() < 0 {
                    return Err(BlissError::TypeError {
                        datum: idx,
                        expected: "non-negative sequence index".into(),
                    });
                }
                let i = idx.as_fixnum() as usize;
                if is_string_value(seq) {
                    bliss_stdlib::string_set_char(seq, i, val)?;
                } else if seq.is_cons() {
                    let mut cursor = seq;
                    for _ in 0..i {
                        cursor = cp(cursor).1;
                    }
                    if cursor.is_cons() {
                        unsafe {
                            let cell = cursor.as_ptr() as *mut ConsCell;
                            bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).car), val);
                        }
                    }
                } else {
                    bliss_stdlib::set_elt(seq, i, val)?;
                }
                // Re-read from the rooted args: the store may have allocated.
                return Ok(args.get(2).copied().unwrap_or(NIL));
            }
            "BLISS::SET-SYMBOL-FUNCTION" => {
                // Store primitive for bytecode-lowered `(setf (symbol-function|
                // fdefinition sym) fn)`. Args arrive evaluated (sym, value);
                // install fn as the symbol's global function and return it.
                let args = eval_args(cdr, env)?;
                let sym = args.first().copied().unwrap_or(NIL);
                let val = args.get(1).copied().unwrap_or(NIL);
                if !sym.is_symbol() {
                    return Err(BlissError::Internal(format!(
                        "SET-SYMBOL-FUNCTION: expected a symbol, got {}",
                        format_val(sym)
                    )));
                }
                let fnval = coerce_installed_function(env, sym, val);
                bliss_rt::symbols::set_symbol_function(sym.as_symbol_index(), fnval);
                return Ok(val);
            }
            "BLISS::SET-CAR" | "BLISS::SET-CDR" => {
                // Store primitives for bytecode-lowered `(setf (car|cdr place)
                // value)`. Arguments arrive already evaluated (cons, value)
                // through apply_function's synthesize path; the target's car/cdr
                // is written in place and the value is returned (SETF semantics).
                let set_car = name == "BLISS::SET-CAR";
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("SET-CAR/SET-CDR needs cons and value".into()));
                }
                let target = args[0];
                let val = args[1];
                if !target.is_cons() {
                    return Err(BlissError::TypeError {
                        datum: target,
                        expected: "cons".into(),
                    });
                }
                unsafe {
                    let cell = target.as_ptr() as *mut ConsCell;
                    if set_car {
                        bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).car), val);
                    } else {
                        bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).cdr), val);
                    }
                }
                return Ok(val);
            }
            "REMHASH" => {
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("REMHASH requires a key and a table".into()));
                }
                let removed = bliss_stdlib::remhash(args[0], args[1])?;
                return Ok(if removed { T } else { NIL });
            }
            "CLRHASH" => {
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                bliss_stdlib::clrhash(tbl)?;
                return Ok(tbl);
            }
            "HASH-TABLE-COUNT" => {
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                let n = bliss_stdlib::hash_table_count(tbl)?;
                return Ok(BlissVal::from_fixnum(n as i64));
            }
            "HASH-TABLE-P" => {
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                return Ok(if is_hash_table_value(tbl) { T } else { NIL });
            }
            "HASH-TABLE-KEYS" | "HASH-TABLE-VALUES" => {
                // alexandria's HASH-TABLE-KEYS/VALUES (used pervasively — babel's
                // instantiate-concrete-mappings iterates the encoding keys). These
                // were listed as builtins but never dispatched (undefined at call).
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                let entries = bliss_stdlib::hash_table_entries(tbl)?;
                let items: Vec<BlissVal> = if name == "HASH-TABLE-KEYS" {
                    entries.into_iter().map(|(k, _)| k).collect()
                } else {
                    entries.into_iter().map(|(_, v)| v).collect()
                };
                return Ok(vec_to_list(&items));
            }
            "MAPHASH" => {
                // (maphash function hash-table): call FUNCTION on each key/value
                // pair through the unified function protocol (bliss-jtc.8) — any
                // callable (lambda, closure, heap function object, builtin), not
                // only a native pointer. Iterates a snapshot so the table may be
                // mutated (per-key) during the walk. Returns NIL.
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("MAPHASH requires a function and a table".into()));
                }
                let roots = bliss_rt::ShadowRootScope::new();
                let function = roots.root(args[0]);
                // Root the entry snapshot: each apply_function runs user code that
                // can relocate the still-pending Vec-resident keys/values (#2).
                let entries: Vec<(bliss_rt::ShadowRoot, bliss_rt::ShadowRoot)> =
                    bliss_stdlib::hash_table_entries(args[1])?
                        .into_iter()
                        .map(|(k, v)| (roots.root(k), roots.root(v)))
                        .collect();
                for (key, value) in &entries {
                    apply_function(function.get(), &[key.get(), value.get()], env)?;
                }
                return Ok(NIL);
            }
            "SXHASH" => {
                // (sxhash object): a hash code such that equal objects hash equal
                // (ANSI); routed to the stdlib hash (bliss-jtc.8).
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(bliss_stdlib::sxhash(v));
            }
            "HASH-TABLE-ENTRIES" => {
                // Bliss helper: a fresh list of (key . value) pairs. Backs
                // WITH-HASH-TABLE-ITERATOR (bliss-jtc.8).
                let (tf, _) = cp(cdr);
                let tbl = eval_form(tf, env)?;
                let pairs: Vec<BlissVal> = bliss_stdlib::hash_table_entries(tbl)?
                    .into_iter()
                    .map(|(k, v)| arena_cons(k, v))
                    .collect();
                return Ok(vec_to_list(&pairs));
            }
            "MAPCAR" => {
                // (mapcar fn list1 list2 ...) — apply fn to successive tuples,
                // stopping at the shortest list.
                // Root the source spine, the input lists' elements, and the
                // accumulated results across the loops: each eval_form /
                // apply_function allocates and can relocate the earlier, bare
                // `Vec`-resident values and the not-yet-walked spine under a minor
                // GC (bliss-6b2 #2).
                let roots = bliss_rt::ShadowRootScope::new();
                let (fn_form, r0) = cp(cdr);
                let r = roots.root(r0);
                let fn_val = roots.root(eval_form(fn_form, env)?);
                let mut lists: Vec<Vec<bliss_rt::ShadowRoot>> = Vec::new();
                while r.get().is_cons() {
                    let (list_form, rest) = cp(r.get());
                    let rest = roots.root(rest);
                    let elems = list_to_vec(eval_form(list_form, env)?);
                    lists.push(elems.into_iter().map(|v| roots.root(v)).collect());
                    r.set(rest.get());
                }
                let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                let mut results: Vec<bliss_rt::ShadowRoot> = Vec::with_capacity(n);
                for i in 0..n {
                    let args: Vec<BlissVal> = lists.iter().map(|l| l[i].get()).collect();
                    results.push(roots.root(apply_function(fn_val.get(), &args, env)?));
                }
                let results: Vec<BlissVal> = results.iter().map(|v| v.get()).collect();
                return Ok(vec_to_list(&results));
            }
            "MAP" => {
                // Root the source-form spine as well as the result-type/function
                // and every sequence element and result across the apply loop
                // (bliss-6b2 #2): each eval_form / apply_function allocates and can
                // relocate these Rust-heap Vec-resident values and the not-yet-
                // evaluated source forms.
                let roots = bliss_rt::ShadowRootScope::new();
                let src = roots.root_values(list_to_vec(cdr));
                if src.len() < 3 {
                    return Err(BlissError::Internal(
                        "MAP requires a result type, function, and sequence".into(),
                    ));
                }
                let result_type = roots.root(eval_form(src[0].get(), env)?);
                let fn_val = roots.root(eval_form(src[1].get(), env)?);
                let mut seqs: Vec<Vec<bliss_rt::ShadowRoot>> = Vec::new();
                for seq_form in &src[2..] {
                    let seq = eval_form(seq_form.get(), env)?;
                    let elems: Vec<BlissVal> = if is_string_value(seq) {
                        val_as_str(seq).chars().map(BlissVal::from_char).collect()
                    } else {
                        list_to_vec(seq)
                    };
                    seqs.push(elems.into_iter().map(|v| roots.root(v)).collect());
                }
                let len = seqs.iter().map(Vec::len).min().unwrap_or(0);
                let mut results: Vec<bliss_rt::ShadowRoot> = Vec::with_capacity(len);
                for i in 0..len {
                    let call_args: Vec<BlissVal> = seqs.iter().map(|seq| seq[i].get()).collect();
                    results.push(roots.root(apply_function(fn_val.get(), &call_args, env)?));
                }
                let results: Vec<BlissVal> = results.iter().map(|v| v.get()).collect();
                let result_type = result_type.get();
                let result_name = if result_type.is_symbol() {
                    symbol_bare_name(&sym_name(result_type))
                } else {
                    val_as_str(result_type).to_uppercase()
                };
                return Ok(match result_name.as_str() {
                    "NIL" => NIL,
                    "LIST" => vec_to_list(&results),
                    "STRING" | "SIMPLE-STRING" | "BASE-STRING" => {
                        let s = results.iter().map(|v| v.as_char()).collect::<String>();
                        arena_str(&s)
                    }
                    _ => vec_to_list(&results),
                });
            }
            // FIND / POSITION / COUNT are defined in lib/boot.lisp over
            // ELT/LENGTH/FUNCALL so their :key/:test can be any interpreter
            // function.  The stdlib helpers only understood a fixed set of
            // sentinel keys and panicked (aborting the process) on a real
            // function — see bliss-0l1.  No builtin arm here means these names
            // fall through to the user/boot function table below.
            "MEMBER" => {
                // (member item list &key key test test-not)
                let (item_f, r) = cp(cdr);
                let (list_f, kwrest) = cp(r);
                let item = eval_form(item_f, env)?;
                let list = eval_form(list_f, env)?;
                let kwargs = eval_args(kwrest, env)?;
                let key_fn = find_key_arg(&kwargs, "KEY");
                let test_fn = find_key_arg(&kwargs, "TEST");
                let test_not_fn = find_key_arg(&kwargs, "TEST-NOT");
                let mut c = list;
                while c.is_cons() {
                    let (car, cdr_val) = cp(c);
                    let probe = match key_fn {
                        Some(kf) if kf != NIL => apply_function(kf, &[car], env)?,
                        _ => car,
                    };
                    let matched = if let Some(tf) = test_fn {
                        apply_function(tf, &[item, probe], env)? != NIL
                    } else if let Some(tnf) = test_not_fn {
                        apply_function(tnf, &[item, probe], env)? == NIL
                    } else {
                        vals_equal(probe, item)
                    };
                    if matched {
                        return Ok(c);
                    }
                    c = cdr_val;
                }
                return Ok(NIL);
            }
            "ASSOC" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (key_f, r) = cp(cdr);
                let alist_f = roots.root(cp(r).0);
                let key = roots.root(eval_form(key_f, env)?);
                let alist = eval_form(alist_f.get(), env)?;
                let key = key.get();
                let mut c = alist;
                while c.is_cons() {
                    let (pair, rest) = cp(c);
                    if pair.is_cons() {
                        let (k, _) = cp(pair);
                        if vals_equal(k, key) {
                            return Ok(pair);
                        }
                    }
                    c = rest;
                }
                return Ok(NIL);
            }
            "CONCATENATE" => {
                // Delegate to stdlib so CLI sequence behavior matches the
                // same implementation used by lower-level stage-3 tests.
                let (type_form, rest) = cp(cdr);
                let result_type = eval_form(type_form, env)?;
                let sequences = eval_args(rest, env)?;
                return bliss_stdlib::concatenate(result_type, &sequences);
            }
            "SUBSEQ" => {
                let (seq_form, rest) = cp(cdr);
                let (start_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let start = num_val(eval_form(start_form, env)?)? as usize;
                let end = if rest2.is_cons() {
                    Some(num_val(eval_form(cp(rest2).0, env)?)? as usize)
                } else {
                    None
                };
                return bliss_stdlib::subseq(seq, start, end);
            }
            "SOME" | "EVERY" | "NOTANY" | "NOTEVERY" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(format!(
                        "{} requires a predicate",
                        name
                    )));
                }
                // Root predicate + all sequence elements across the apply loop
                // (bliss-6b2 #2): apply_function runs user code that can relocate
                // these Vec-resident values.
                let roots = bliss_rt::ShadowRootScope::new();
                let pred = roots.root(args[0]);
                let mut seqs: Vec<Vec<bliss_rt::ShadowRoot>> = Vec::with_capacity(args.len() - 1);
                for s in &args[1..] {
                    seqs.push(seq_elements(*s)?.into_iter().map(|e| roots.root(e)).collect());
                }
                let minlen = seqs.iter().map(Vec::len).min().unwrap_or(0);
                for i in 0..minlen {
                    let call_args: Vec<BlissVal> = seqs.iter().map(|s| s[i].get()).collect();
                    let r = apply_function(pred.get(), &call_args, env)?;
                    match name.as_str() {
                        "SOME" if !r.is_nil() => return Ok(r),
                        "EVERY" if r.is_nil() => return Ok(NIL),
                        "NOTANY" if !r.is_nil() => return Ok(NIL),
                        "NOTEVERY" if r.is_nil() => return Ok(T),
                        _ => {}
                    }
                }
                return Ok(match name.as_str() {
                    "SOME" => NIL,
                    "EVERY" | "NOTANY" => T,
                    _ => NIL, // NOTEVERY
                });
            }
            "COERCE" => {
                let (val_form, rest) = cp(cdr);
                let (type_form, _) = cp(rest);
                let value = eval_form(val_form, env)?;
                let type_val = eval_form(type_form, env)?;
                return coerce_value(value, type_val);
            }
            "SORT" => {
                let (seq_form, rest) = cp(cdr);
                let (pred_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let predicate = eval_form(pred_form, env)?;
                let key = if rest2.is_cons() {
                    let (kw_form, rest3) = cp(rest2);
                    if kw_form.is_symbol()
                        && symbol_bare_name(&sym_name(kw_form)).eq_ignore_ascii_case("KEY")
                        && rest3.is_cons()
                    {
                        Some(eval_form(cp(rest3).0, env)?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return sort_sequence(seq, predicate, key, env);
            }
            "STABLE-SORT" => {
                let (seq_form, rest) = cp(cdr);
                let (pred_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let predicate = eval_form(pred_form, env)?;
                let key = if rest2.is_cons() {
                    let (kw_form, rest3) = cp(rest2);
                    if kw_form.is_symbol()
                        && symbol_bare_name(&sym_name(kw_form)).eq_ignore_ascii_case("KEY")
                        && rest3.is_cons()
                    {
                        Some(eval_form(cp(rest3).0, env)?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return sort_sequence(seq, predicate, key, env);
            }
            "PATHNAMEP" => {
                let (thing_form, _) = cp(cdr);
                let thing = eval_form(thing_form, env)?;
                return Ok(if bliss_stdlib::is_pathname(thing) {
                    T
                } else {
                    NIL
                });
            }
            "MAKE-PATHNAME" => {
                // (make-pathname &key host device directory name type version defaults)
                let args = eval_args(cdr, env)?;
                // Track supplied-p per component so an explicit `:name nil`
                // (override) is distinguished from an unsupplied component (which
                // is taken from :defaults, per ANSI).
                let (mut host, mut device, mut directory) = (None, None, None);
                let (mut name_c, mut type_c, mut version) = (None, None, None);
                let mut defaults: Option<BlissVal> = None;
                let mut i = 0;
                while i + 1 < args.len() {
                    let key = symbol_bare_name(&sym_name(args[i]));
                    let val = args[i + 1];
                    match key.as_str() {
                        "HOST" => host = Some(val),
                        "DEVICE" => device = Some(val),
                        // A directory given as (:absolute|:relative comp…) uses
                        // reader keywords the stdlib can't match by hash; render
                        // it to a namestring the stdlib parser accepts.
                        "DIRECTORY" => {
                            directory = Some(match directory_designator_to_namestring(val) {
                                Some(s) => arena_str(&s),
                                None => val,
                            });
                        }
                        "NAME" => name_c = Some(val),
                        "TYPE" => type_c = Some(val),
                        "VERSION" => version = Some(val),
                        "DEFAULTS" => defaults = Some(val),
                        _ => {}
                    }
                    i += 2;
                }
                // Components not explicitly supplied are taken from :defaults
                // (ANSI). `pathname_directory` returns the directory as a
                // namestring, which is what make_pathname expects. With no valid
                // :defaults, unsupplied components stay NIL.
                let d = defaults.filter(|v| bliss_stdlib::is_pathname(*v));
                let resolve = |supplied: Option<BlissVal>, from: fn(BlissVal) -> BlissVal| {
                    supplied.unwrap_or_else(|| d.map(from).unwrap_or(NIL))
                };
                return bliss_stdlib::make_pathname(
                    resolve(host, bliss_stdlib::pathname_host),
                    resolve(device, bliss_stdlib::pathname_device),
                    resolve(directory, bliss_stdlib::pathname_directory),
                    resolve(name_c, bliss_stdlib::pathname_name),
                    resolve(type_c, bliss_stdlib::pathname_type),
                    resolve(version, bliss_stdlib::pathname_version),
                );
            }
            "USER-HOMEDIR-PATHNAME" => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
                let dir = if home.ends_with('/') {
                    home
                } else {
                    format!("{home}/")
                };
                let (pathname, _) = bliss_stdlib::parse_namestring(arena_str(&dir), None, None)?;
                return Ok(pathname);
            }
            "PARSE-NAMESTRING" => {
                let (thing_form, rest) = cp(cdr);
                let thing = eval_form(thing_form, env)?;
                let host = if rest.is_cons() {
                    Some(eval_form(cp(rest).0, env)?)
                } else {
                    None
                };
                let (pathname, position) = bliss_stdlib::parse_namestring(thing, host, None)?;
                env.set_mv(vec![pathname, BlissVal::from_fixnum(position as i64)]);
                return Ok(pathname);
            }
            "NAMESTRING" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return bliss_stdlib::namestring(pathname);
            }
            "PROBE-FILE" => {
                // (probe-file pathspec) — truename if the file exists, else NIL.
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::probe_file(pathname)?.unwrap_or(NIL));
            }
            "DELETE-FILE" => {
                // (delete-file pathspec) — delete the file, returning T.
                let (path_form, _) = cp(cdr);
                let pathspec = eval_form(path_form, env)?;
                bliss_stdlib::delete_file(pathspec)?;
                return Ok(T);
            }
            "RENAME-FILE" => {
                // (rename-file filespec new-name) → (values new-truename old-truename
                // new-truename). We surface the primary (new) pathname.
                let (from_form, rest) = cp(cdr);
                let (to_form, _) = cp(rest);
                let from = eval_form(from_form, env)?;
                let to = eval_form(to_form, env)?;
                let (defaulted, old_true, new_true) = bliss_stdlib::rename_file(from, to)?;
                env.set_mv(vec![defaulted, old_true, new_true]);
                return Ok(defaulted);
            }
            "FILE-WRITE-DATE" => {
                // (file-write-date pathspec) — the file's last-modified time as a
                // CL universal time (seconds since 1900-01-01). ASDF uses it for
                // staleness checks (bliss-lb6.14).
                let (path_form, _) = cp(cdr);
                let pathspec = eval_form(path_form, env)?;
                let path = path_designator_to_string(pathspec)?;
                let secs = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map_err(|e| BlissError::FileError(format!("{}: {}", path, e)))?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| BlissError::FileError(e.to_string()))?
                    .as_secs();
                // Universal time epoch (1900) precedes the Unix epoch (1970) by
                // 2208988800 seconds.
                return Ok(BlissVal::from_fixnum(secs as i64 + 2_208_988_800));
            }
            "GET-UNIVERSAL-TIME" => {
                return Ok(BlissVal::from_fixnum(
                    bliss_stdlib::time::get_universal_time(),
                ));
            }
            "ENCODE-UNIVERSAL-TIME" => {
                // (encode-universal-time second minute hour date month year
                //  &optional time-zone)
                let args = eval_args(cdr, env)?;
                if args.len() < 6 {
                    return Err(BlissError::ProgramError(
                        "ENCODE-UNIVERSAL-TIME requires at least 6 arguments".into(),
                    ));
                }
                let n = |v: BlissVal| -> Result<i64, BlissError> { Ok(num_val(v)? as i64) };
                let (second, minute, hour) = (n(args[0])?, n(args[1])?, n(args[2])?);
                let (date, month, mut year) = (n(args[3])?, n(args[4])?, n(args[5])?);
                // CLHS 25.1.4: a two-digit year is relative to a 50-year window
                // around the current year.
                if (0..=99).contains(&year) {
                    let current = bliss_stdlib::time::decode_universal_time(
                        bliss_stdlib::time::get_universal_time(),
                        Some(0),
                    )
                    .5;
                    let base = current - 50;
                    year = base + (year - base).rem_euclid(100);
                }
                // Time zone is hours west of GMT; NIL / omitted means local,
                // which we model as GMT (see time.rs).
                let time_zone = match args.get(6) {
                    Some(v) if !v.is_nil() => Some(num_val(*v)? as i64),
                    _ => None,
                };
                return Ok(BlissVal::from_fixnum(
                    bliss_stdlib::time::encode_universal_time(
                        second, minute, hour, date, month, year, time_zone,
                    ),
                ));
            }
            "DECODE-UNIVERSAL-TIME" => {
                // (decode-universal-time universal-time &optional time-zone)
                //   => second, minute, hour, date, month, year,
                //      day-of-week, daylight-p, zone
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError(
                        "DECODE-UNIVERSAL-TIME requires a universal time".into(),
                    ));
                }
                let universal = num_val(args[0])? as i64;
                let time_zone = match args.get(1) {
                    Some(v) if !v.is_nil() => Some(num_val(*v)? as i64),
                    _ => None,
                };
                let (sec, min, hour, date, month, year, dow, dst, zone) =
                    bliss_stdlib::time::decode_universal_time(universal, time_zone);
                let values = vec![
                    BlissVal::from_fixnum(sec),
                    BlissVal::from_fixnum(min),
                    BlissVal::from_fixnum(hour),
                    BlissVal::from_fixnum(date),
                    BlissVal::from_fixnum(month),
                    BlissVal::from_fixnum(year),
                    BlissVal::from_fixnum(dow),
                    if dst { T } else { NIL },
                    BlissVal::from_fixnum(zone),
                ];
                let first = values[0];
                env.set_mv(values);
                return Ok(first);
            }
            "DIRECTORY" => {
                // (directory pathspec &key …) — list pathnames matching a
                // (possibly wild) pathname. Extra keyword args (e.g. UIOP's
                // :resolve-symlinks) are accepted and ignored. Needed by ASDF's
                // source-registry directory walk (bliss-lb6.14).
                let (path_form, _) = cp(cdr);
                let pathspec = eval_form(path_form, env)?;
                let pathname = if bliss_stdlib::is_pathname(pathspec) {
                    pathspec
                } else {
                    bliss_stdlib::parse_namestring(pathspec, None, None)?.0
                };
                let entries = bliss_stdlib::directory(pathname)?;
                return Ok(vec_to_list(&entries));
            }
            "TRUENAME" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return bliss_stdlib::truename(pathname);
            }
            "PATHNAME" => {
                // Coerce a pathname designator to a pathname: an existing
                // pathname passes through; anything else is parsed as a
                // namestring. Check is_pathname FIRST — pathnames are registry-
                // backed pseudo-heap values, so calling is_string on one (as the
                // string branch would) dereferences a bogus pointer and crashes.
                let (thing_form, _) = cp(cdr);
                let thing = eval_form(thing_form, env)?;
                if bliss_stdlib::is_pathname(thing) {
                    return Ok(thing);
                }
                let (pathname, _) = bliss_stdlib::parse_namestring(thing, None, None)?;
                return Ok(pathname);
            }
            "MERGE-PATHNAMES" => {
                // (merge-pathnames pathname &optional default-pathname default-version)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::ProgramError(
                        "MERGE-PATHNAMES requires a pathname".into(),
                    ));
                }
                // Coerce a pathname designator (string / pathname) to a pathname.
                let to_pathname = |v: BlissVal| -> Result<BlissVal, BlissError> {
                    if bliss_stdlib::is_pathname(v) {
                        Ok(v)
                    } else {
                        Ok(bliss_stdlib::parse_namestring(v, None, None)?.0)
                    }
                };
                // Root the intermediate pathnames: to_pathname/lookup allocate and
                // can relocate an earlier result (bliss-6b2 #2).
                let roots = bliss_rt::ShadowRootScope::new();
                let pathname = roots.root(to_pathname(args[0])?);
                let default = if args.len() > 1 {
                    roots.root(to_pathname(args[1])?)
                } else {
                    // ANSI default is *default-pathname-defaults*.
                    roots.root(match env.lookup_var("*DEFAULT-PATHNAME-DEFAULTS*") {
                        Some(v) if !v.is_nil() => to_pathname(v)?,
                        _ => to_pathname(bliss_stdlib::make_lisp_string("./"))?,
                    })
                };
                let default_version = if args.len() > 2 { args[2] } else { NIL };
                return bliss_stdlib::merge_pathnames(
                    pathname.get(),
                    default.get(),
                    default_version,
                );
            }
            "PATHNAME-NAME" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_name(pathname));
            }
            "PATHNAME-TYPE" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_type(pathname));
            }
            "PATHNAME-DIRECTORY" => {
                let (pathname_form, _) = cp(cdr);
                let mut pathname = eval_form(pathname_form, env)?;
                // Coerce a namestring designator to a pathname first (ANSI).
                if !bliss_stdlib::is_pathname(pathname) {
                    pathname = bliss_stdlib::parse_namestring(pathname, None, None)?.0;
                }
                // ANSI PATHNAME-DIRECTORY returns a list (:absolute|:relative
                // comp…), not a namestring — UIOP does directory-list arithmetic
                // on it (bliss-lb6). Build the keywords with the interpreter's
                // interner so they are EQ to the reader's :absolute / :wild / ….
                match bliss_stdlib::pathname_directory_components(pathname) {
                    Some((absolute, comps)) => {
                        let kw = |s: &str| resolve_sym(s).unwrap_or(NIL);
                        let mut elems = Vec::with_capacity(comps.len() + 1);
                        elems.push(kw(if absolute { ":ABSOLUTE" } else { ":RELATIVE" }));
                        for c in comps {
                            elems.push(match c {
                                bliss_stdlib::PathDirComp::Name(s) => {
                                    bliss_stdlib::make_lisp_string(&s)
                                }
                                bliss_stdlib::PathDirComp::Up => kw(":UP"),
                                bliss_stdlib::PathDirComp::Wild => kw(":WILD"),
                                bliss_stdlib::PathDirComp::WildInferiors => kw(":WILD-INFERIORS"),
                            });
                        }
                        return Ok(vec_to_list(&elems));
                    }
                    None => return Ok(NIL),
                }
            }
            "PATHNAME-HOST" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_host(pathname));
            }
            "PATHNAME-DEVICE" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_device(pathname));
            }
            "PATHNAME-VERSION" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_version(pathname));
            }
            "WILD-PATHNAME-P" => {
                // (wild-pathname-p pathname &optional field-key)
                let args = eval_args(cdr, env)?;
                let pathname = if args.is_empty() { NIL } else { args[0] };
                let field = if args.len() > 1 { Some(args[1]) } else { None };
                return Ok(if bliss_stdlib::wild_pathname_p(pathname, field) {
                    T
                } else {
                    NIL
                });
            }
            "PATHNAME-MATCH-P" => {
                // (pathname-match-p pathname wildcard)
                let args = eval_args(cdr, env)?;
                let pn = args.first().copied().unwrap_or(NIL);
                let wc = args.get(1).copied().unwrap_or(NIL);
                return Ok(if bliss_stdlib::pathname_match_p(pn, wc)? {
                    T
                } else {
                    NIL
                });
            }
            "TRANSLATE-PATHNAME" => {
                // (translate-pathname source from-wildcard to-wildcard)
                let (src_form, rest) = cp(cdr);
                let (from_form, rest2) = cp(rest);
                let (to_form, _) = cp(rest2);
                let src = eval_form(src_form, env)?;
                let from = eval_form(from_form, env)?;
                let to = eval_form(to_form, env)?;
                return bliss_stdlib::translate_pathname(src, from, to);
            }
            "ENSURE-DIRECTORIES-EXIST" => {
                // (ensure-directories-exist pathspec &key verbose) → pathspec plus
                // a second value that is true if any directory was created.
                let (ps_form, _) = cp(cdr);
                let pathspec = eval_form(ps_form, env)?;
                let (pn, created) = bliss_stdlib::ensure_directories_exist(pathspec)?;
                env.set_mv(vec![pn, if created { T } else { NIL }]);
                return Ok(pn);
            }
            "STRING" => {
                // (string x): a string is returned as-is; a symbol yields its
                // bare SYMBOL-NAME (no package prefix); a character yields a
                // one-character string.
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let s = if is_string_value(v) {
                    val_as_str(v)
                } else if v.is_character() {
                    v.as_char().to_string()
                } else if v.is_symbol() || v.is_nil() || v == T {
                    symbol_name_string(&sym_name(v))
                } else {
                    val_as_str(v)
                };
                return Ok(arena_str(&s));
            }
            "WRITE-TO-STRING" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(arena_str(&format_val_env(v, env, true)));
            }
            "1+" | "1-" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let delta = if name == "1+" { 1 } else { -1 };
                if v.is_fixnum() {
                    return Ok(BlissVal::from_fixnum(v.as_fixnum() + delta));
                }
                if v.is_single_float() {
                    return Ok(BlissVal::from_single_float(
                        v.as_single_float() + delta as f32,
                    ));
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "number".into(),
                });
            }
            "ZEROP" | "PLUSP" | "MINUSP" => {
                let (af, _) = cp(cdr);
                let n = num_val(eval_form(af, env)?)?;
                let result = match name.as_str() {
                    "ZEROP" => n == 0.0,
                    "PLUSP" => n > 0.0,
                    _ => n < 0.0,
                };
                return Ok(if result { T } else { NIL });
            }
            "EVENP" | "ODDP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let n = if v.is_fixnum() {
                    v.as_fixnum()
                } else {
                    num_val(v)? as i64
                };
                let even = n % 2 == 0;
                return Ok(if even == (name == "EVENP") { T } else { NIL });
            }
            "ABS" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_fixnum() {
                    return Ok(BlissVal::from_fixnum(v.as_fixnum().abs()));
                }
                if v.is_single_float() {
                    return Ok(BlissVal::from_single_float(v.as_single_float().abs()));
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "number".into(),
                });
            }
            "MIN" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MIN requires at least one argument".into(),
                    ));
                }
                let mut min = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v < min {
                        min = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(min as f32)
                } else {
                    BlissVal::from_fixnum(min as i64)
                });
            }
            "MAX" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAX requires at least one argument".into(),
                    ));
                }
                let mut max = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v > max {
                        max = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(max as f32)
                } else {
                    BlissVal::from_fixnum(max as i64)
                });
            }
            "FLOOR" => return eval_floor(cdr, env),
            "REM" => {
                // REM: remainder of TRUNCATE — the result takes the sign of the
                // dividend (Rust `%`).
                let roots = bliss_rt::ShadowRootScope::new();
                let (af, r) = cp(cdr);
                let bf = roots.root(cp(r).0);
                let a = roots.root(eval_form(af, env)?);
                let b = eval_form(bf.get(), env)?;
                let av = num_val(a.get())? as i64;
                let bv = num_val(b)? as i64;
                if bv == 0 {
                    return Err(BlissError::ArithmeticError("division by zero".into()));
                }
                return Ok(BlissVal::from_fixnum(av % bv));
            }
            "MOD" => {
                // MOD: remainder of FLOOR — the result takes the sign of the
                // *divisor* (ANSI), so `(mod -7 3)` = 2, not -1.
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("MOD requires two arguments".into()));
                }
                let av = num_val(args[0])? as i64;
                let bv = num_val(args[1])? as i64;
                if bv == 0 {
                    return Err(BlissError::ArithmeticError("division by zero".into()));
                }
                return Ok(BlissVal::from_fixnum(((av % bv) + bv) % bv));
            }
            "TRUNCATE" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("TRUNCATE requires an argument".into()));
                }
                let a = args[0];
                let av = num_val(a)?;
                if args.len() >= 2 {
                    let b = args[1];
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).trunc() as i64;
                    let rem = integer_or_float_remainder(a, b, q, av, bv);
                    env.set_mv(vec![BlissVal::from_fixnum(q), rem]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av as i64));
            }
            "CEILING" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("CEILING requires an argument".into()));
                }
                let a = args[0];
                let av = num_val(a)?;
                if args.len() >= 2 {
                    let b = args[1];
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).ceil() as i64;
                    let rem = integer_or_float_remainder(a, b, q, av, bv);
                    env.set_mv(vec![BlissVal::from_fixnum(q), rem]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av.ceil() as i64));
            }
            "ROUND" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("ROUND requires an argument".into()));
                }
                let a = args[0];
                let av = num_val(a)?;
                if args.len() >= 2 {
                    let b = args[1];
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = round_half_even(av / bv);
                    let rem = integer_or_float_remainder(a, b, q, av, bv);
                    env.set_mv(vec![BlissVal::from_fixnum(q), rem]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(round_half_even(av)));
            }
            "EXPT" => {
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal("EXPT requires two arguments".into()));
                }
                let a = args[0];
                let b = args[1];
                // Exact result when the base is rational and the exponent is an
                // integer: an exact rational/bignum, promoting past i64 range.
                if !a.is_single_float() && b.is_fixnum() {
                    if let Some(base) = as_bigrat(a) {
                        let e = b.as_fixnum();
                        if e >= 0 {
                            return Ok(bigrat_pow(&base, e as u64).to_val());
                        }
                        if base.num.is_zero() {
                            return Err(BlissError::ArithmeticError("division by zero".into()));
                        }
                        // negative exponent: reciprocal of base^|e|
                        let p = bigrat_pow(&base, e.unsigned_abs());
                        return Ok(bigrat_div(&BigRat::from_i64(1), &p).to_val());
                    }
                }
                let av = num_val(a)?;
                let bv = num_val(b)?;
                return Ok(BlissVal::from_single_float(av.powf(bv) as f32));
            }
            "SQRT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let nv = num_val(v)?;
                return Ok(BlissVal::from_single_float(nv.sqrt() as f32));
            }
            "RANDOM" => {
                // (random limit &optional random-state) — a value in [0, limit)
                // of the same type as LIMIT. The optional random-state arg is
                // evaluated (for effect) but the shared generator is used.
                let (limit_form, rest) = cp(cdr);
                let limit = eval_form(limit_form, env)?;
                if rest.is_cons() {
                    eval_form(cp(rest).0, env)?;
                }
                if limit.is_fixnum() {
                    let bound = limit.as_fixnum();
                    if bound <= 0 {
                        return Err(BlissError::ProgramError(
                            "RANDOM limit must be a positive number".into(),
                        ));
                    }
                    let r = (next_random_u64() % bound as u64) as i64;
                    return Ok(BlissVal::from_fixnum(r));
                }
                if limit.is_single_float() {
                    let bound = limit.as_single_float();
                    if bound <= 0.0 {
                        return Err(BlissError::ProgramError(
                            "RANDOM limit must be a positive number".into(),
                        ));
                    }
                    // 24 random mantissa bits give a uniform unit float in [0,1).
                    let unit = (next_random_u64() >> 40) as f32 / (1u64 << 24) as f32;
                    return Ok(BlissVal::from_single_float(unit * bound));
                }
                return Err(BlissError::TypeError {
                    datum: limit,
                    expected: "positive integer or float".into(),
                });
            }
            "NTH-VALUE" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let (nf, r) = cp(cdr);
                let form = roots.root(cp(r).0);
                let n = eval_form(nf, env)?;
                let idx = num_val(n)? as usize;
                let (_, values) = eval_form_collecting_values(form.get(), env)?;
                return Ok(values.get(idx).copied().unwrap_or(NIL));
            }
            "MULTIPLE-VALUE-BIND" => return eval_multiple_value_bind(cdr, env),
            "MULTIPLE-VALUE-CALL" => {
                // (multiple-value-call function form*) — gather all the values
                // produced by each form into a single argument list.
                let roots = bliss_rt::ShadowRootScope::new();
                let (fn_form, forms) = cp(cdr);
                // Root the source cursor and the function value across the loop
                // (bliss-6b2 #2): each form's evaluation allocates and can relocate.
                let c = roots.root(forms);
                let fn_val = roots.root(eval_form(fn_form, env)?);
                let mut args = Vec::new();
                let _ag = VecRootGuard::new(&mut args);
                while c.get().is_cons() {
                    let (af, r) = cp(c.get());
                    let (_, values) = eval_form_collecting_values(af, env)?;
                    args.extend(values);
                    c.set(r);
                }
                return apply_function(fn_val.get(), &args, env);
            }
            "MULTIPLE-VALUE-PROG1" => {
                let (first_form, rest_forms) = cp(cdr);
                let (primary, saved_values) = eval_form_collecting_values(first_form, env)?;
                // Root the saved values across the body's allocating forms (bliss-6b2 #2).
                let saved_values = {
                    let mut sv = saved_values;
                    let _sg = VecRootGuard::new(&mut sv);
                    let _ = eval_progn(rest_forms, env)?;
                    sv
                };
                env.set_mv(saved_values);
                return Ok(primary);
            }
            "MULTIPLE-VALUE-SETQ" => {
                let (vars_form, rest) = cp(cdr);
                let (values_form, _) = cp(rest);
                let vars = list_to_vec(vars_form);
                let (_, values) = eval_form_collecting_values(values_form, env)?;
                for (index, var_form) in vars.iter().enumerate() {
                    let name = sym_name(*var_form);
                    env.set_var(&name, values.get(index).copied().unwrap_or(NIL));
                }
                if values.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(values.clone());
                return Ok(values[0]);
            }
            "MULTIPLE-VALUE-LIST" => {
                // (multiple-value-list form) — a list of all the values of form.
                let (form, _) = cp(cdr);
                let (_, values) = eval_form_collecting_values(form, env)?;
                return Ok(vec_to_list(&values));
            }
            "VALUES-LIST" => {
                // (values-list list) — return the elements of list as values.
                let (form, _) = cp(cdr);
                let lst = eval_form(form, env)?;
                let vals = list_to_vec(lst);
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "HANDLER-CASE" => return eval_handler_case(cdr, env),
            "HANDLER-BIND" => return eval_handler_bind(cdr, env),
            "RESTART-BIND" => return eval_restart_bind(cdr, env),
            "RESTART-CASE" => return eval_restart_case(cdr, env),
            "SIGNAL" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("SIGNAL requires an argument".into()));
                }
                let datum = roots.root(args[0]);
                let initargs = &args[1..];
                // (signal datum &rest args): a condition-type symbol is built into
                // an instance so handler type-matching runs against the real CLOS
                // class hierarchy.
                let cond =
                    coerce_condition_designator(env, datum.get(), initargs)?.unwrap_or(datum.get());
                return signal_condition_object(cond, env);
            }
            "WARN" => {
                let roots = bliss_rt::ShadowRootScope::new();
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal("WARN requires an argument".into()));
                }
                let datum = roots.root(args[0]);
                let rest_args = &args[1..];
                // (warn datum &rest args): a warning-type symbol or condition is
                // used directly; a format-control string becomes a SIMPLE-WARNING.
                let message = if is_string_value(datum.get()) {
                    format_control_message(&val_as_str(datum.get()), rest_args)?
                } else {
                    val_as_str(datum.get())
                };
                let condition = match coerce_condition_designator(env, datum.get(), rest_args)? {
                    Some(condition) => condition,
                    None => make_simple_warning_condition(datum.get(), rest_args, env)?,
                };
                // Establish a MUFFLE-WARNING restart for the dynamic extent of the
                // signal so a handler can suppress the default warning message.
                let base_len = env.restarts.len();
                env.restarts.push(RestartEntry {
                    name: "MUFFLE-WARNING".to_string(),
                    function: RestartFunction::ContinueNil,
                    interactive_function: None,
                    test_function: None,
                    unwind_on_invoke: true,
                });
                let result = signal_condition_object(condition, env);
                env.restarts.truncate(base_len);
                match result {
                    Ok(_) => {
                        // Unhandled (or handler declined): print the warning per
                        // R5.104 and return NIL. Warnings never enter the debugger.
                        eprintln!("WARNING: {}", message);
                        return Ok(NIL);
                    }
                    Err(error) => {
                        if restart_invoked_name(&error).as_deref() == Some("MUFFLE-WARNING") {
                            return Ok(NIL);
                        }
                        return Err(error);
                    }
                }
            }
            "MAKE-CONDITION" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-CONDITION requires a type".into(),
                    ));
                }
                let type_val = args[0];
                let type_name = if type_val.is_symbol() {
                    sym_name(type_val)
                } else {
                    val_as_str(type_val)
                };
                // Initarg key/value pairs (drop an odd trailing arg, as the
                // original loop did) are already evaluated and rooted in `args`.
                let pair_count = (args.len().saturating_sub(1)) & !1;
                let initarg_pairs = &args[1..1 + pair_count];
                return build_condition_instance(env, &type_name, initarg_pairs);
            }
            "SLOT-VALUE" => {
                let args = eval_args(cdr, env)?;
                let instance = args.first().copied().unwrap_or(NIL);
                let slot = args.get(1).copied().unwrap_or(NIL);
                return slot_value_or_signal(instance, slot, env);
            }
            "SLOT-BOUNDP" => {
                let args = eval_args(cdr, env)?;
                let instance = args.first().copied().unwrap_or(NIL);
                let slot = args.get(1).copied().unwrap_or(NIL);
                return Ok(if slot_is_bound(instance, slot, env)? {
                    T
                } else {
                    NIL
                });
            }
            "CLASS-OF" => {
                let (object_form, _) = cp(cdr);
                let object = eval_form(object_form, env)?;
                return Ok(bliss_stdlib::class_of(object));
            }
            "FIND-CLASS" => {
                // (find-class name &optional (errorp t) environment) — return the
                // class metaobject, or (when errorp is NIL) NIL if none is found.
                let (name_form, rest) = cp(cdr);
                let name = eval_form(name_form, env)?;
                // errorp defaults to T when the argument is omitted.
                let errorp = if rest.is_cons() {
                    let (errorp_form, _) = cp(rest);
                    eval_form(errorp_form, env)? != NIL
                } else {
                    true
                };
                if let Some(class) = bliss_stdlib::find_class(name) {
                    return Ok(class);
                }
                if let Ok(class) = resolve_class_metaobject(env, name)
                    && class != name
                {
                    return Ok(class);
                }
                if !errorp {
                    return Ok(NIL);
                }
                let name_str = if name.is_symbol() {
                    sym_name(name)
                } else {
                    val_as_str(name)
                };
                let msg = format!("there is no class named {}", name_str);
                let condition = make_simple_error_condition(arena_str(&msg), env)?;
                return Err(signal_and_raise(env, condition, msg));
            }
            "CLASS-NAME" => {
                let (class_form, _) = cp(cdr);
                let class = eval_form(class_form, env)?;
                return Ok(bliss_stdlib::class_name(class));
            }
            "SUBTYPEP" => {
                // (subtypep type1 type2) → two values: subtype-p and certain-p.
                let (t1_form, rest) = cp(cdr);
                let (t2_form, _) = cp(rest);
                let t1 = eval_form(t1_form, env)?;
                let t2 = eval_form(t2_form, env)?;
                let t1 = resolve_type_spec(env, t1);
                let t2 = resolve_type_spec(env, t2);
                let (subtype_p, certain_p) = subtypep_relation(t1, t2);
                let subp = if subtype_p { T } else { NIL };
                let certainp = if certain_p { T } else { NIL };
                env.set_mv(vec![subp, certainp]);
                return Ok(subp);
            }
            "CLASS-PRECEDENCE-LIST" | "COMPUTE-CLASS-PRECEDENCE-LIST" => {
                let (class_form, _) = cp(cdr);
                let class_input = eval_form(class_form, env)?;
                let class = resolve_class_metaobject(env, class_input)?;
                let cpl = bliss_stdlib::compute_class_precedence_list(class)?;
                return Ok(vec_to_list(&cpl));
            }
            "SLOT-MAKUNBOUND" => {
                let (instance_form, rest) = cp(cdr);
                let (slot_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let slot = eval_form(slot_form, env)?;
                bliss_stdlib::slot_makunbound(instance, slot)?;
                return Ok(instance);
            }
            "SLOT-EXISTS-P" => {
                let (instance_form, rest) = cp(cdr);
                let (slot_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let slot = eval_form(slot_form, env)?;
                let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let slot_name = symbol_bare_name(&sym_name(slot));
                let exists = lookup_slot_def(env, &class_name, &slot_name).is_some();
                return Ok(if exists { T } else { NIL });
            }
            "NEXT-METHOD-P" => {
                let has_next = env
                    .method_context
                    .last()
                    .map(method_context_has_next)
                    .unwrap_or(false);
                return Ok(if has_next { T } else { NIL });
            }
            "INITIALIZE-INSTANCE" => {
                let (instance_form, init_args) = cp(cdr);
                let instance = eval_form(instance_form, env)?;
                let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let initargs = evaluated_initargs(&class_name, init_args, env)?;
                let explicit_slots: Vec<String> = initargs
                    .chunks_exact(2)
                    .map(|pair| symbol_bare_name(&sym_name(pair[0])))
                    .collect();
                let (instance_initargs, class_initargs) =
                    split_initargs_for_class(env, &class_name, &initargs);
                bliss_stdlib::initialize_instance(instance, &instance_initargs)?;
                for (slot_name, value) in class_initargs {
                    write_class_slot_value(env, &class_name, &slot_name, Some(value));
                }
                apply_class_initforms(instance, &class_name, env, None, &explicit_slots)?;
                return Ok(instance);
            }
            "REINITIALIZE-INSTANCE" => {
                let (instance_form, init_args) = cp(cdr);
                let instance = eval_form(instance_form, env)?;
                let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let initargs = evaluated_initargs(&class_name, init_args, env)?;
                let (instance_initargs, class_initargs) =
                    split_initargs_for_class(env, &class_name, &initargs);
                bliss_stdlib::reinitialize_instance(instance, &instance_initargs)?;
                for (slot_name, value) in class_initargs {
                    write_class_slot_value(env, &class_name, &slot_name, Some(value));
                }
                return Ok(instance);
            }
            "CHANGE-CLASS" => {
                let (instance_form, rest) = cp(cdr);
                let (class_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let old_class_name =
                    class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let class_input = eval_form(class_form, env)?;
                let class = resolve_class_metaobject(env, class_input)?;
                bliss_stdlib::change_class(instance, class)?;
                let new_class_name = class_name_for_instance_class(class);
                // Newly-added slots are those in the new class's *effective*
                // (inherited + direct) slot set that were not effective slots of
                // the old class. Using effective slots — not just direct slots —
                // is essential: e.g. change-class to a class that inherits a slot
                // (parent) which the old class lacked must still apply that slot's
                // initform (CLOS change-class / update-instance-for-different-class).
                let old_slot_names: std::collections::HashSet<String> =
                    effective_slots_for_class(env, &old_class_name)
                        .into_iter()
                        .map(|slot| slot.name)
                        .collect();
                let added_slots = effective_slots_for_class(env, &new_class_name)
                    .into_iter()
                    .filter(|slot| !old_slot_names.contains(&slot.name))
                    .map(|slot| slot.name)
                    .collect::<Vec<_>>();
                apply_class_initforms(instance, &new_class_name, env, Some(&added_slots), &[])?;
                return Ok(instance);
            }
            "CALL-NEXT-METHOD" => {
                let context = env.method_context.last().cloned().ok_or_else(|| {
                    BlissError::UndefinedFunction(resolve_sym("CALL-NEXT-METHOD").unwrap_or(NIL))
                })?;
                let args = if cdr.is_nil() {
                    RootedVals::new(context.args.clone())
                } else {
                    eval_args(cdr, env)?
                };
                return invoke_next_method(env, &context, &args);
            }
            "CERROR" => return eval_cerror(cdr, env),
            "COMPUTE-RESTARTS" => {
                let args = list_to_vec(cdr);
                let condition = if args.is_empty() {
                    None
                } else {
                    Some(eval_form(args[0], env)?)
                };
                let mut restarts = Vec::new();
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if restart_applies(&restart, condition, env)? {
                        restarts.push(resolve_sym(&restart.name).unwrap_or(NIL));
                    }
                }
                return Ok(vec_to_list(&restarts));
            }
            "FIND-RESTART" => {
                // Root the source cursor and the evaluated condition across the
                // evaluations and the applies-loop (bliss-6b2 #2).
                let roots = bliss_rt::ShadowRootScope::new();
                let (name_form, rest0) = cp(cdr);
                let rest = roots.root(rest0);
                let restart_name = symbol_bare_name(&val_as_str(eval_form(name_form, env)?));
                let condition = if rest.get().is_cons() {
                    Some(roots.root(eval_form(cp(rest.get()).0, env)?))
                } else {
                    None
                };
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if symbol_bare_name(&restart.name) == restart_name
                        && restart_applies(&restart, condition.as_ref().map(|c| c.get()), env)?
                    {
                        return Ok(resolve_sym(&restart.name).unwrap_or(NIL));
                    }
                }
                return Ok(NIL);
            }
            "INVOKE-RESTART" => {
                // Root the source-arg spine across the name evaluation (bliss-6b2 #2).
                let roots = bliss_rt::ShadowRootScope::new();
                let (name_form, rest0) = cp(cdr);
                let rest_args = roots.root(rest0);
                let name_val = eval_form(name_form, env)?;
                let restart_name = val_as_str(name_val).to_uppercase();
                let args = eval_args(rest_args.get(), env)?;
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if restart.name == restart_name {
                        let result = invoke_restart_function(&restart.function, &args, env)?;
                        if restart.unwind_on_invoke {
                            store_control_value(&format!("RESTART-RESULT:{restart_name}"), result);
                            return Err(BlissError::Internal(format!(
                                "__RESTART_INVOKED__:{}",
                                restart_name
                            )));
                        }
                        return Ok(result);
                    }
                }
                return Err(BlissError::Internal(format!(
                    "Restart {} not found",
                    restart_name
                )));
            }
            "INVOKE-RESTART-INTERACTIVELY" => {
                let (restart_form, _) = cp(cdr);
                let restart = eval_form(restart_form, env)?;
                let restart_name = symbol_bare_name(&val_as_str(restart)).to_uppercase();
                let Some(entry) = env
                    .restarts
                    .iter()
                    .rev()
                    .find(|entry| entry.name == restart_name)
                    .cloned()
                else {
                    return Err(BlissError::Internal(format!(
                        "Restart {} not found",
                        restart_name
                    )));
                };
                let interactive_args = if let Some(interactive) = &entry.interactive_function {
                    let value = invoke_restart_function(interactive, &[], env)?;
                    if value.is_nil() {
                        Vec::new()
                    } else if value.is_cons() {
                        list_to_vec(value)
                    } else {
                        vec![value]
                    }
                } else {
                    Vec::new()
                };
                let result = invoke_restart_function(&entry.function, &interactive_args, env)?;
                if entry.unwind_on_invoke {
                    store_control_value(&format!("RESTART-RESULT:{restart_name}"), result);
                    return Err(BlissError::Internal(format!(
                        "__RESTART_INVOKED__:{}",
                        restart_name
                    )));
                }
                return Ok(result);
            }
            "WITH-OPEN-FILE" => return eval_with_open_file(cdr, env),
            "LOAD" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                // LOAD accepts a pathname designator — a namestring OR a pathname
                // object (e.g. `#P"…"`, common in a ~/.blissrc). `val_as_str` on a
                // pathname yields its debug repr, so coerce via its namestring.
                let path = path_designator_to_string(path_val)?;
                // ANSI LOAD returns a generalized boolean (T on success); the
                // last top-level form's value is not the result.
                load_path_into_env(&path, env)?;
                return Ok(T);
            }
            "COMPILE-FILE" => {
                // (compile-file source &key output-file &allow-other-keys) — compile
                // SOURCE to a .bfasl (bliss-lb6.6). Returns three values per ANSI:
                // output-truename, warnings-p, failure-p. ASDF passes the target as
                // an :output-file keyword (not positional).
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "COMPILE-FILE requires a source".into(),
                    ));
                }
                let src_path = path_designator_to_string(args[0])?;
                let mut out_path: Option<String> = None;
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = args[i];
                    let val = args[i + 1];
                    if key.is_symbol()
                        && symbol_bare_name(&sym_name(key)) == "OUTPUT-FILE"
                        && val != NIL
                    {
                        out_path = Some(path_designator_to_string(val)?);
                    }
                    i += 2;
                }
                let out_path = out_path.unwrap_or_else(|| {
                    let stem = src_path.strip_suffix(".lisp").unwrap_or(&src_path);
                    format!("{stem}.bfasl")
                });
                let source = std::fs::read_to_string(&src_path).map_err(|e| {
                    BlissError::FileError(format!("compile-file: cannot read {src_path}: {e}"))
                })?;
                // Per-file progress, gated on *COMPILE-VERBOSE* (default T, like
                // SBCL). Only fires when compilation actually happens — ASDF
                // calls COMPILE-FILE only for a missing/stale fasl — so warm
                // loads stay silent.
                let compile_verbose = env
                    .lookup_var("*COMPILE-VERBOSE*")
                    .map(|v| !v.is_nil())
                    .unwrap_or(true);
                let compile_start = std::time::Instant::now();
                if compile_verbose {
                    let written = std::fs::metadata(&src_path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| format!(" (written {})", format_compile_note_date(d.as_secs())))
                        .unwrap_or_default();
                    println!("; compiling file \"{src_path}\"{written}:");
                }
                // ANSI COMPILE-FILE binds *PACKAGE* (and *READTABLE*) for the
                // dynamic extent of the compilation (CLHS 3.2.1), so a file's
                // IN-PACKAGE forms don't leak into the caller — after
                // `(compile-file "lib/asdf.lisp")` the REPL stays in CL-USER, not
                // ASDF/FOOTER. Snapshot the current package and restore it once
                // compilation is done (mirrors LOAD in load_path_into_env).
                let saved_package = env.current_package.clone();
                let image = build_bfasl_from_source(&source, &src_path, env);
                if env.current_package != saved_package {
                    env.current_package = saved_package.clone();
                    env.define_local("*PACKAGE*", package_object(&saved_package));
                }
                let image = image?;
                // Create the output directory if needed. ASDF's output-translations
                // route fasls into a per-implementation cache tree whose directories
                // may not exist yet; real CL relies on ASDF pre-creating them, but
                // creating them here is harmless and avoids a spurious file error.
                if let Some(parent) = std::path::Path::new(&out_path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&out_path, &image).map_err(|e| {
                    BlissError::FileError(format!("compile-file: cannot write {out_path}: {e}"))
                })?;
                if compile_verbose {
                    let el = compile_start.elapsed();
                    let secs = el.as_secs();
                    println!("; wrote {out_path}");
                    println!(
                        "; compilation finished in {}:{:02}:{:02}.{:03}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60,
                        el.subsec_millis()
                    );
                }
                let (out_pn, _) = bliss_stdlib::parse_namestring(arena_str(&out_path), None, None)?;
                env.set_mv(vec![out_pn, NIL, NIL]);
                return Ok(out_pn);
            }
            "COMPILE-FILE-PATHNAME" => {
                // (compile-file-pathname input-file &key output-file &allow-other-keys)
                // Return the pathname COMPILE-FILE would write. With an explicit
                // :output-file, return that (as a pathname); otherwise the input
                // with a "fasl" type. ASDF calls this to compute output-files.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "compile-file-pathname: missing input-file".into(),
                    ));
                }
                // Scan &key args for :output-file.
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = args[i];
                    let val = args[i + 1];
                    if key.is_symbol() && symbol_bare_name(&sym_name(key)) == "OUTPUT-FILE" {
                        if val != NIL {
                            let (pn, _) = bliss_stdlib::parse_namestring(
                                arena_str(&path_designator_to_string(val)?),
                                None,
                                None,
                            )?;
                            return Ok(pn);
                        }
                    }
                    i += 2;
                }
                let src = path_designator_to_string(args[0])?;
                let stem = src
                    .strip_suffix(".lisp")
                    .or_else(|| src.strip_suffix(".lsp"))
                    .unwrap_or(&src);
                let (pn, _) =
                    bliss_stdlib::parse_namestring(arena_str(&format!("{stem}.fasl")), None, None)?;
                return Ok(pn);
            }
            "REQUIRE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                return require_module(&val_as_str(module_val), env);
            }
            "PROVIDE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                env.define_local("*LAST-PROVIDED-MODULE*", module_val);
                // Register in *MODULES* so a later (require ...) is a no-op (ANSI).
                let name = val_as_str(module_val).to_uppercase();
                record_module(env, &name);
                return Ok(module_val);
            }
            "BLISS-EXT:GETENV" => {
                let (name_form, _) = cp(cdr);
                let name = val_as_str(eval_form(name_form, env)?);
                return Ok(std::env::var(&name)
                    .ok()
                    .map(|value| arena_str(&value))
                    .unwrap_or(NIL));
            }
            // bliss-jtc.10: Lisp-visible tiering introspection. These make the
            // hotspot engine observable through the real binary — a test (or a
            // user) can watch a hot loop's counters climb and its tier promote,
            // then confirm results are identical across tiers (the S5 gate).
            // Each takes a function designator (a symbol or #'fn) and returns a
            // fixnum; an unrecognised / non-tiered designator yields NIL.
            "BLISS-EXT:FUNCTION-TIER" => {
                let (f_form, _) = cp(cdr);
                let d = eval_form(f_form, env)?;
                bytecode::poll_background_compilation();
                return Ok(resolve_tiered_fn(d)
                    .map(|f| BlissVal::from_fixnum(bliss_rt::function::tier(f) as i64))
                    .unwrap_or(NIL));
            }
            "BLISS-EXT:FUNCTION-INVOKE-COUNT" => {
                let (f_form, _) = cp(cdr);
                let d = eval_form(f_form, env)?;
                return Ok(resolve_tiered_fn(d)
                    .map(|f| BlissVal::from_fixnum(bliss_rt::function::invoke_count(f) as i64))
                    .unwrap_or(NIL));
            }
            "BLISS-EXT:FUNCTION-BACK-EDGE-COUNT" => {
                let (f_form, _) = cp(cdr);
                let d = eval_form(f_form, env)?;
                return Ok(resolve_tiered_fn(d)
                    .map(|f| BlissVal::from_fixnum(bliss_rt::function::back_edge_count(f) as i64))
                    .unwrap_or(NIL));
            }
            // Process-wide count of T1 speculative deoptimizations (bliss-jtc.27):
            // observability for the S5 gate's second half — a failed speculation
            // deoptimizes to the interpreter and still returns the correct value.
            "BLISS-EXT:DEOPT-COUNT" => {
                return Ok(BlissVal::from_fixnum(bytecode::deopt_count() as i64));
            }
            // Compile-coverage diagnostic (bliss-x5y.1): the histogram of why the
            // bytecode lowerer bailed to the tree-walker, printed most-frequent
            // first (requires BLISS_BAIL_TRACE=1 when compiling). Returns the
            // number of distinct reasons. Use it to find which constructs keep
            // real code (e.g. uiop:ensure-package) off the bytecode/T1 path.
            "BLISS-EXT:BAIL-REPORT" => {
                let report = bytecode::bail_report();
                for (reason, count) in &report {
                    println!("{count:>7}  {reason}");
                }
                return Ok(BlissVal::from_fixnum(report.len() as i64));
            }
            "BLISS-EXT:GETCWD" => {
                // Current working directory as a namestring with a trailing
                // slash (a directory namestring), for ASDF's getcwd (#+bliss).
                return Ok(std::env::current_dir()
                    .ok()
                    .map(|p| {
                        let mut s = p.to_string_lossy().into_owned();
                        if !s.ends_with('/') {
                            s.push('/');
                        }
                        arena_str(&s)
                    })
                    .unwrap_or(NIL));
            }
            "BLISS-EXT:RUN-PROGRAM" => {
                // (bliss-ext:run-program command) — run a subprocess synchronously,
                // capturing stdout/stderr. COMMAND is a list of strings (program +
                // args, executed directly) or a string (run via `/bin/sh -c`).
                // Returns (values exit-code stdout-string stderr-string). UIOP's
                // RUN-PROGRAM builds on this for #+bliss.
                if env.sandbox {
                    return Err(BlissError::SandboxViolation(
                        "subprocess execution denied in sandbox mode".into(),
                    ));
                }
                let (cmd_form, _) = cp(cdr);
                let cmd_val = eval_form(cmd_form, env)?;
                let mut command = if is_string_value(cmd_val) {
                    let mut c = std::process::Command::new("/bin/sh");
                    c.arg("-c").arg(val_as_str(cmd_val));
                    c
                } else {
                    let parts: Vec<String> = list_to_vec(cmd_val)
                        .iter()
                        .map(|v| val_as_str(*v))
                        .collect();
                    if parts.is_empty() {
                        return Err(BlissError::Internal("run-program: empty command".into()));
                    }
                    let mut c = std::process::Command::new(&parts[0]);
                    c.args(&parts[1..]);
                    c
                };
                match command.output() {
                    Ok(out) => {
                        let code = out.status.code().unwrap_or(-1);
                        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                        env.set_mv(vec![
                            BlissVal::from_fixnum(code as i64),
                            arena_str(&stdout),
                            arena_str(&stderr),
                        ]);
                        return Ok(BlissVal::from_fixnum(code as i64));
                    }
                    Err(e) => {
                        return Err(BlissError::FileError(format!("run-program: {e}")));
                    }
                }
            }
            "BLISS-EXT:RAW-COMMAND-LINE-ARGUMENTS" => {
                // The process argv as a list of strings (program name first),
                // matching SBCL's sb-ext:*posix-argv*, for ASDF (#+bliss).
                let argv: Vec<BlissVal> = std::env::args().map(|a| arena_str(&a)).collect();
                return Ok(vec_to_list(&argv));
            }
            "READ-LINE" => {
                // (read-line &optional stream eof-error-p eof-value)
                let args = list_to_vec(cdr);
                let stream = if !args.is_empty() {
                    eval_form(args[0], env)?
                } else {
                    NIL
                };
                let inp = resolve_input_stream(stream, env);
                if is_gray_stream(inp) {
                    // The Gray stream-read-line returns (values string eof-p);
                    // invoke_generic_function yields the primary value (the line).
                    let line = invoke_generic_function("STREAM-READ-LINE", &[inp], env)?;
                    return Ok(line);
                }
                let (line_val, missing_newline) = bliss_stdlib::stream_read_line(inp)?;
                if line_val == EOF {
                    // At end of input: honour the eof designator like the old
                    // behaviour did — return NIL rather than signalling.
                    return Ok(NIL);
                }
                env.set_mv(vec![line_val, if missing_newline { T } else { NIL }]);
                return Ok(line_val);
            }
            "READ-SEQUENCE" => {
                // (read-sequence sequence stream &key start end) — destructively
                // fill SEQUENCE from STREAM; returns the index of the first element
                // not read into (start + number of elements actually read).
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "READ-SEQUENCE requires a sequence and a stream".into(),
                    ));
                }
                let vals = eval_forms(args.iter().copied(), env)?;
                let seq = vals[0];
                let inp = resolve_input_stream(vals[1], env);
                let seq_len = bliss_stdlib::length(seq)?;
                let (start, end) = read_start_end_keys(&vals[2..], seq_len);
                let count = end - start;
                let mut pos = start;
                if is_gray_stream(inp) {
                    for _ in 0..count {
                        let c = invoke_generic_function("STREAM-READ-CHAR", &[inp], env)?;
                        if c.is_nil() || c == EOF {
                            break;
                        }
                        seq_set_elt(seq, pos, c)?;
                        pos += 1;
                    }
                } else {
                    for el in bliss_stdlib::stream_read_sequence(inp, count)? {
                        seq_set_elt(seq, pos, el)?;
                        pos += 1;
                    }
                }
                return Ok(BlissVal::from_fixnum(pos as i64));
            }
            "WRITE-SEQUENCE" => {
                // (write-sequence sequence stream &key start end) — write the
                // subsequence to STREAM; returns SEQUENCE.
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "WRITE-SEQUENCE requires a sequence and a stream".into(),
                    ));
                }
                let vals = eval_forms(args.iter().copied(), env)?;
                let seq = vals[0];
                let out = resolve_output_stream(vals[1], env);
                let seq_len = bliss_stdlib::length(seq)?;
                let (start, end) = read_start_end_keys(&vals[2..], seq_len);
                let elems: Vec<BlissVal> = (start..end)
                    .map(|i| bliss_stdlib::elt(seq, i))
                    .collect::<Result<_, _>>()?;
                if is_gray_stream(out) {
                    for el in &elems {
                        let gf = if el.is_character() {
                            "STREAM-WRITE-CHAR"
                        } else {
                            "STREAM-WRITE-BYTE"
                        };
                        invoke_generic_function(gf, &[out, *el], env)?;
                    }
                } else {
                    bliss_stdlib::stream_write_sequence(out, &elems)?;
                }
                return Ok(seq);
            }
            "WRITE-STRING" => {
                // (write-string string &optional stream &key start end)
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "WRITE-STRING requires an argument".into(),
                    ));
                }
                let s = val_as_str(args[0]);
                // A second positional argument is the stream designator, unless
                // it is a keyword (the start of &key start/end options).
                let stream = if args.len() > 1 && !is_keyword_arg(args[1]) {
                    args[1]
                } else {
                    NIL
                };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    // Dispatch to the Gray stream-write-string generic (start 0,
                    // end nil → whole string).
                    invoke_generic_function(
                        "STREAM-WRITE-STRING",
                        &[out, args[0], BlissVal::from_fixnum(0), NIL],
                        env,
                    )?;
                    return Ok(args[0]);
                }
                write_str_to(out, &s)?;
                return Ok(args[0]);
            }
            "WRITE-LINE" => {
                // (write-line string &optional stream &key start end) — WRITE-STRING
                // followed by a newline; returns the string.
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "WRITE-LINE requires an argument".into(),
                    ));
                }
                let s = val_as_str(args[0]);
                let stream = if args.len() > 1 && !is_keyword_arg(args[1]) {
                    args[1]
                } else {
                    NIL
                };
                let out = resolve_output_stream(stream, env);
                if is_gray_stream(out) {
                    invoke_generic_function(
                        "STREAM-WRITE-STRING",
                        &[out, args[0], BlissVal::from_fixnum(0), NIL],
                        env,
                    )?;
                    invoke_generic_function("STREAM-TERPRI", &[out], env)?;
                    return Ok(args[0]);
                }
                write_str_to(out, &s)?;
                bliss_stdlib::stream_terpri(out)?;
                return Ok(args[0]);
            }
            "FILE-LENGTH" => {
                // (file-length stream) — length in elements of an open file stream.
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal("FILE-LENGTH requires a stream".into()));
                }
                let stream = eval_form(args[0], env)?;
                return bliss_stdlib::file_length_fn(stream);
            }
            "SAVE-IMAGE" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                let path = val_as_str(path_val);
                // Save a minimal image: serialize function definitions — the
                // lexical/name-map ones plus the global ones now in symbol
                // function cells (bliss-jtc.6.8).
                let mut image_data = String::new();
                for (name, fdef) in env.funs.borrow().iter() {
                    let params_str = fdef.params.join(" ");
                    let body_str = format_body_forms(fdef.body);
                    image_data
                        .push_str(&format!("(defun {} ({}) {})\n", name, params_str, body_str));
                }
                bliss_rt::symbols::for_each_bound_function(|_idx, name, func| {
                    if bliss_rt::function::is_interpreted_function(func) {
                        let params_str = format_body_forms(bliss_rt::function::lambda_list(func));
                        let body_str = format_body_forms(bliss_rt::function::body(func));
                        image_data.push_str(&format!("(defun {name} {params_str} {body_str})\n"));
                    }
                });
                std::fs::write(&path, &image_data)
                    .map_err(|e| BlissError::FileError(format!("save-image: {}", e)))?;
                return Ok(T);
            }
            "DEFPACKAGE" => return eval_defpackage(cdr, env),
            "IN-PACKAGE" => {
                let (pkg_form, _) = cp(cdr);
                // IN-PACKAGE's argument is a package designator and is NOT
                // evaluated (CLHS): a symbol (interned or uninterned) or string
                // whose name is used.
                let raw = if pkg_form.is_symbol() {
                    symbol_bare_name(&sym_name(pkg_form))
                } else if is_string_value(pkg_form) {
                    val_as_str(pkg_form)
                } else {
                    val_as_str(eval_form(pkg_form, env)?)
                };
                let pkg_name = raw
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':')
                    .to_uppercase();
                env.current_package = pkg_name;
                env.define_local("*PACKAGE*", package_object(&env.current_package));
                return Ok(T);
            }
            "MAKE-PACKAGE" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-PACKAGE requires a package designator".into(),
                    ));
                }
                let pkg_name = normalize_package_name(&val_as_str(args[0]));
                // Parse :nicknames and :use keyword options.
                let mut nicknames = Vec::new();
                let mut uses: Vec<String> = Vec::new();
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = symbol_bare_name(&sym_name(args[i]));
                    let value = args[i + 1];
                    match key.as_str() {
                        "NICKNAMES" => {
                            for nick in list_to_vec(value) {
                                nicknames.push(normalize_package_name(&val_as_str(nick)));
                            }
                        }
                        "USE" => {
                            for used in list_to_vec(value) {
                                uses.push(resolve_package_name(env, &val_as_str(used)));
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
                let use_refs: Vec<&str> = uses.iter().map(String::as_str).collect();
                ensure_package_available(env, &pkg_name, &use_refs);
                if !nicknames.is_empty() {
                    if let Some(pkg) = bliss_stdlib::find_package(&pkg_name) {
                        for nick in nicknames {
                            // Register the nickname with the reader too, so a
                            // package-qualified symbol written with the nickname
                            // (e.g. `uiop:foo`, UIOP being a nickname of
                            // UIOP/DRIVER) resolves at read time (bliss-lb6).
                            reader::register_package(&nick);
                            let _ = bliss_stdlib::add_nickname(pkg, &nick);
                        }
                    }
                }
                return Ok(package_object(&pkg_name));
            }
            "FIND-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let raw = val_as_str(eval_form(args[0], env)?);
                let pkg_name = resolve_package_name(env, &raw);
                return Ok(
                    if bliss_stdlib::find_package(&pkg_name).is_some()
                        || matches!(
                            pkg_name.as_str(),
                            "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD"
                        )
                    {
                        package_object(&pkg_name)
                    } else {
                        NIL
                    },
                );
            }
            "PACKAGEP" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let value = eval_form(args[0], env)?;
                return Ok(if is_package_value(env, value) { T } else { NIL });
            }
            "PACKAGE-NAME" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = val_as_str(eval_form(args[0], env)?);
                return Ok(if pkg.is_empty() {
                    NIL
                } else {
                    arena_str(&resolve_package_name(env, &pkg))
                });
            }
            "PACKAGE-NAMES" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                return Ok(vec_to_list(&[arena_str(&pkg)]));
            }
            "PACKAGE-NICKNAMES" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let raw = val_as_str(eval_form(args[0], env)?);
                let pkg_name = resolve_package_name(env, &raw);
                let nicks: Vec<BlissVal> = bliss_stdlib::find_package(&pkg_name)
                    .map(|pkg| {
                        bliss_stdlib::package_nicknames(pkg)
                            .iter()
                            .map(|n| arena_str(n))
                            .collect()
                    })
                    .unwrap_or_default();
                return Ok(vec_to_list(&nicks));
            }
            "PACKAGE-SHADOWING-SYMBOLS" => {
                return Ok(NIL);
            }
            "PACKAGE-USED-BY-LIST" => {
                // The packages that :USE the given package. Was stubbed to NIL,
                // which broke UIOP's ensure-package reconciliation: ensure-exported
                // walks (package-used-by-list from-package) to propagate an export
                // into inheriting packages, so a NIL result left inheritors in an
                // inconsistent state that ASDF then re-processed (bliss-nad).
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let raw = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let target = resolve_package_name(env, &raw);
                // Resolve the target to its canonical handle once, then match
                // use-list entries by identity — comparing package *handles* is
                // far cheaper than resolving and string-comparing each entry's
                // name (this runs O(all-packages) per call, and ASDF calls it
                // once per exported symbol during reconciliation).
                let target_pkg = match bliss_stdlib::find_package(&target) {
                    Some(p) => p,
                    None => return Ok(NIL),
                };
                let mut result = Vec::new();
                for p in bliss_stdlib::list_all_packages() {
                    let uses_target = bliss_stdlib::package_use_list(p)
                        .iter()
                        .any(|u| *u == target_pkg);
                    if uses_target {
                        result.push(p);
                    }
                }
                return Ok(vec_to_list(&result));
            }
            "PACKAGE-USE-LIST" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let uses = bliss_stdlib::find_package(&pkg_name)
                    .map(bliss_stdlib::package_use_list)
                    .unwrap_or_default();
                return Ok(vec_to_list(&uses));
            }
            "LIST-ALL-PACKAGES" => {
                return Ok(vec_to_list(&bliss_stdlib::list_all_packages()));
            }
            "BLISS-INTERNAL::PACKAGE-SYMBOLS" | "BLISS-INTERNAL:PACKAGE-SYMBOLS" => {
                let args = eval_args(cdr, env)?;
                let package = if args.is_empty() {
                    env.current_package.clone()
                } else {
                    normalize_package_name(&val_as_str(args[0]))
                };
                let include_inherited = if let Some(arg) = args.get(1) {
                    !arg.is_nil()
                } else {
                    false
                };
                return Ok(vec_to_list(&package_symbols(
                    env,
                    &package,
                    include_inherited,
                )));
            }
            "USE-PACKAGE" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Ok(T);
                }
                let mut names = Vec::new();
                let pkgs_val = args[0];
                if pkgs_val.is_cons() {
                    for pkg in list_to_vec(pkgs_val) {
                        names.push(resolve_package_name(env, &val_as_str(pkg)));
                    }
                } else {
                    names.push(resolve_package_name(env, &val_as_str(pkgs_val)));
                }
                let target = if args.len() > 1 {
                    let raw = val_as_str(args[1]);
                    resolve_package_name(env, &raw)
                } else {
                    env.current_package.clone()
                };
                // ensure_package_available creates the target if needed and adds
                // each named package to its use-list (creating a placeholder for
                // any not yet defined).
                let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                ensure_package_available(env, &target, &name_refs);
                return Ok(T);
            }
            "UNUSE-PACKAGE" => return Ok(T),
            "RENAME-PACKAGE" => {
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "RENAME-PACKAGE requires package and new name".into(),
                    ));
                }
                let old_raw = val_as_str(args[0]);
                let old_name = resolve_package_name(env, &old_raw);
                let new_name = normalize_package_name(&val_as_str(args[1]));
                // Optional new nicknames (3rd arg); default: keep the package's
                // current nicknames (bliss has always preserved them on rename).
                if let Some(pkg) = bliss_stdlib::find_package(&old_name) {
                    let new_nicks: Vec<String> = if args.len() > 2 {
                        list_to_vec(args[2])
                            .iter()
                            .map(|n| normalize_package_name(&val_as_str(*n)))
                            .collect()
                    } else {
                        bliss_stdlib::package_nicknames(pkg)
                    };
                    let nick_refs: Vec<&str> = new_nicks.iter().map(String::as_str).collect();
                    let _ = bliss_stdlib::rename_package(pkg, &new_name, &nick_refs);
                    for nick in &new_nicks {
                        reader::register_package(nick);
                    }
                }
                reader::register_package(&new_name);
                return Ok(package_object(&new_name));
            }
            "DELETE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let del_raw = val_as_str(eval_form(args[0], env)?);
                let pkg_name = resolve_package_name(env, &del_raw);
                let _ = bliss_stdlib::delete_package(&pkg_name);
                return Ok(T);
            }
            "INTERN" => {
                let args = eval_args(cdr, env)?;
                let name_val = args.first().copied().unwrap_or(NIL);
                let name_str = symbol_bare_name(&val_as_str(name_val));
                let pkg_name = if args.len() > 1 {
                    let raw = val_as_str(args[1]);
                    resolve_package_name(env, &raw)
                } else {
                    env.current_package.clone()
                };
                let sym = intern_into_package(env, &pkg_name, &name_str);
                env.set_mv(vec![
                    sym,
                    if sym.is_symbol() {
                        package_status_symbol("INTERNAL")
                    } else {
                        NIL
                    },
                ]);
                return Ok(sym);
            }
            "FIND-SYMBOL" => {
                let args = eval_args(cdr, env)?;
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "FIND-SYMBOL requires a name and package".into(),
                    ));
                }
                let name = symbol_bare_name(&val_as_str(args[0]));
                let pkg_raw = val_as_str(args[1]);
                let pkg_name = resolve_package_name(env, &pkg_raw);
                if let Some((sym, status)) = find_symbol_in_package(env, &pkg_name, &name) {
                    env.set_mv(vec![sym, package_status_symbol(status)]);
                    return Ok(sym);
                }
                env.set_mv(vec![NIL, NIL]);
                return Ok(NIL);
            }
            "EXPORT" | "IMPORT" | "SHADOWING-IMPORT" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Ok(T);
                }
                let symbols = args[0];
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(args[1]))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let sym_vals: Vec<BlissVal> = if symbols.is_cons() {
                    list_to_vec(symbols)
                } else if symbols.is_nil() {
                    Vec::new()
                } else {
                    vec![symbols]
                };
                let export_mode = car.is_symbol() && sym_name(car) == "EXPORT";
                // Preserve the identity of the PASSED symbol: importing or
                // (re-)exporting a symbol must make THAT symbol present in the
                // package, not fork a fresh same-named one — otherwise a
                // downstream package that inherits it sees a conflict with the
                // imported one (bliss-lb6). Only a non-symbol designator (a bare
                // string name) falls back to interning by name.
                let mut resolved = Vec::with_capacity(sym_vals.len());
                for sv in sym_vals {
                    let name = symbol_bare_name(&val_as_str(sv));
                    let sym = if sv.is_symbol() {
                        sv
                    } else {
                        intern_into_package(env, &pkg_name, &name)
                    };
                    resolved.push((name, sym));
                }
                // Make each symbol present in the package (EXPORT → external, so
                // used packages inherit it; IMPORT/SHADOWING-IMPORT → internal).
                if let Some(pkg) = bliss_stdlib::find_package(&pkg_name) {
                    for (name, sym) in resolved {
                        let _ = bliss_stdlib::add_symbol(pkg, &name, sym, export_mode);
                    }
                }
                return Ok(T);
            }
            "UNEXPORT" => return Ok(T),
            "SHADOW" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Ok(T);
                }
                let names_val = args[0];
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(args[1]))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let mut names = Vec::new();
                if names_val.is_cons() {
                    for name in list_to_vec(names_val) {
                        names.push(symbol_bare_name(&val_as_str(name)));
                    }
                } else {
                    names.push(symbol_bare_name(&val_as_str(names_val)));
                }
                for name in names {
                    intern_into_package(env, &pkg_name, &name);
                }
                return Ok(T);
            }
            "UNINTERN" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Ok(NIL);
                }
                let symbol = args[0];
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(args[1]))
                } else {
                    env.current_package.clone()
                };
                let name = symbol_bare_name(&val_as_str(symbol));
                let Some(pkg) = bliss_stdlib::find_package(&pkg_name) else {
                    return Ok(NIL);
                };
                let removed = match bliss_stdlib::find_present_symbol(pkg, &name) {
                    Some(sym) => bliss_stdlib::unintern(sym, pkg).unwrap_or(false),
                    None => false,
                };
                return Ok(if removed { T } else { NIL });
            }
            "SYMBOL-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let sym = eval_form(args[0], env)?;
                if !sym.is_symbol() {
                    return Ok(NIL);
                }
                let name = sym_name(sym);
                let package = if name.starts_with("KEYWORD:") {
                    Some("KEYWORD".to_string())
                } else if let Some((pkg, _)) = name.rsplit_once("::") {
                    Some(pkg.to_string())
                } else if let Some((pkg, _)) = name.rsplit_once(':') {
                    Some(pkg.to_string())
                } else {
                    Some("COMMON-LISP".to_string())
                };
                return Ok(package
                    .map(|pkg| package_object(&resolve_package_name(env, &pkg)))
                    .unwrap_or(NIL));
            }
            "GENSYM" => {
                thread_local! { static COUNTER: RefCell<u64> = const { RefCell::new(0) }; }
                let n = COUNTER.with(|c| {
                    let v = *c.borrow();
                    *c.borrow_mut() = v + 1;
                    v
                });
                let name = format!("G{}", n);
                match resolve_sym(&name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(&name)),
                }
            }
            "MAKE-SYMBOL" => {
                // (make-symbol name) — a fresh uninterned symbol with that name.
                let (name_form, _) = cp(cdr);
                let name = val_as_str(eval_form(name_form, env)?);
                return Ok(reader::make_uninterned_symbol(&name));
            }
            "COPY-SYMBOL" => {
                // (copy-symbol sym) — a fresh uninterned symbol with the same name.
                let (sym_form, _) = cp(cdr);
                let sym = eval_form(sym_form, env)?;
                let name = symbol_name_string(&sym_name(sym));
                return Ok(reader::make_uninterned_symbol(&name));
            }
            "DOTIMES" => {
                // (dotimes (var count [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (count_form, result_rest) = cp(br);
                let roots = bliss_rt::ShadowRootScope::new();
                let var_form = roots.root(var_form);
                let count_form = roots.root(count_form);
                let result_rest = roots.root(result_rest);
                let body = roots.root(body);
                let count = eval_form(count_form.get(), env)?;
                let n = num_val(count)? as i64;
                // Establish a fresh variable frame (so the loop variable shadows
                // outer bindings and does not leak) while keeping the shared
                // global tables — packages, functions, … — mutable in place, so
                // definitions made in the body persist. `with_child_frame` swaps
                // only the frame on the same env; `env.child()` would clone the
                // copy-on-write tables and lose those mutations.
                let parent = Rc::clone(&env.frame);
                return with_block_nil(env, move |env| {
                    with_child_frame(env, parent, |env| {
                        for i in 0..n {
                            env.define_local_symbol(var_form.get(), BlissVal::from_fixnum(i));
                            eval_progn(body.get(), env)?;
                        }
                        if result_rest.get().is_cons() {
                            let (result_form, _) = cp(result_rest.get());
                            env.define_local_symbol(var_form.get(), BlissVal::from_fixnum(n));
                            return eval_form(result_form, env);
                        }
                        Ok(NIL)
                    })
                });
            }
            "DOLIST" => {
                // (dolist (var list [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (list_form, result_rest) = cp(br);
                let roots = bliss_rt::ShadowRootScope::new();
                let var_form = roots.root(var_form);
                let list_form = roots.root(list_form);
                let result_rest = roots.root(result_rest);
                let body = roots.root(body);
                let list = eval_form(list_form.get(), env)?;
                let elems = list_to_vec(list);
                let elems = roots.root_values(elems);
                let parent = Rc::clone(&env.frame);
                return with_block_nil(env, move |env| {
                    with_child_frame(env, parent, |env| {
                        for e in &elems {
                            env.define_local_symbol(var_form.get(), e.get());
                            eval_progn(body.get(), env)?;
                        }
                        if result_rest.get().is_cons() {
                            let (result_form, _) = cp(result_rest.get());
                            env.define_local_symbol(var_form.get(), NIL);
                            return eval_form(result_form, env);
                        }
                        Ok(NIL)
                    })
                });
            }
            "STRING=" => {
                let (a, b) = {
                    // Root both args across evaluation so a young arg list can't
                    // dangle either (bliss-6b2 #2).
                    let roots = bliss_rt::ShadowRootScope::new();
                    let (af, r) = cp(cdr);
                    let bf = roots.root(cp(r).0);
                    let a = roots.root(eval_form(af, env)?);
                    let b = eval_form(bf.get(), env)?;
                    (a.get(), b)
                };
                return Ok(if val_as_str(a) == val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "STRING<" => {
                // ANSI: the mismatch index if string1 < string2, else NIL.
                let (a, b) = {
                    // Root both args across evaluation so a young arg list can't
                    // dangle either (bliss-6b2 #2).
                    let roots = bliss_rt::ShadowRootScope::new();
                    let (af, r) = cp(cdr);
                    let bf = roots.root(cp(r).0);
                    let a = roots.root(eval_form(af, env)?);
                    let b = eval_form(bf.get(), env)?;
                    (a.get(), b)
                };
                let (i, ord) = string_mismatch(&val_as_str(a), &val_as_str(b));
                return Ok(if ord == std::cmp::Ordering::Less {
                    BlissVal::from_fixnum(i as i64)
                } else {
                    NIL
                });
            }
            "STRING>" => {
                // ANSI: the mismatch index if string1 > string2, else NIL.
                let (a, b) = {
                    // Root both args across evaluation so a young arg list can't
                    // dangle either (bliss-6b2 #2).
                    let roots = bliss_rt::ShadowRootScope::new();
                    let (af, r) = cp(cdr);
                    let bf = roots.root(cp(r).0);
                    let a = roots.root(eval_form(af, env)?);
                    let b = eval_form(bf.get(), env)?;
                    (a.get(), b)
                };
                let (i, ord) = string_mismatch(&val_as_str(a), &val_as_str(b));
                return Ok(if ord == std::cmp::Ordering::Greater {
                    BlissVal::from_fixnum(i as i64)
                } else {
                    NIL
                });
            }
            "TYPE-OF" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                // CLOS instances: TYPE-OF returns the direct class name, not the
                // representation type (previously "FIXNUM"). See bliss-2ke.
                if bliss_stdlib::is_instance(v) {
                    if let Some(name) = instance_class_hierarchy_names(v)
                        .as_ref()
                        .and_then(|n| n.first())
                    {
                        return match resolve_sym(name) {
                            Some(sym) => Ok(sym),
                            None => Ok(arena_str(name)),
                        };
                    }
                }
                let type_name = if v.is_nil() {
                    "NULL"
                } else if v == T {
                    "BOOLEAN"
                } else if v.is_fixnum() {
                    "FIXNUM"
                } else if v.is_single_float() {
                    "SINGLE-FLOAT"
                } else if v.is_character() {
                    "CHARACTER"
                } else if v.is_symbol() {
                    "SYMBOL"
                } else if v.is_cons() {
                    "CONS"
                } else if is_string_value(v) {
                    "SIMPLE-BASE-STRING"
                } else {
                    "T"
                };
                match resolve_sym(type_name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(type_name)),
                }
            }
            _ => {}
        }

        // Check user-defined functions: lexical (FLET/LABELS/`(setf f)`) then the
        // global function cell (bliss-jtc.6.8).
        if let Some((params_form, body)) = callable_body(env, &name) {
            let roots = bliss_rt::ShadowRootScope::new();
            // Root the callee's params/body BEFORE evaluating arguments (which
            // allocates and can trigger a relocating minor GC). Held as bare
            // locals, these copies would go stale when the GC moves the callee's
            // body tree — the local still points at the pre-move location — and
            // eval_lambda_call would then walk a freed body (bliss-6b2 #2).
            let params_form = roots.root(params_form);
            let body = roots.root(body);
            let forms = roots.root(cdr);
            let mut rooted_args = Vec::new();
            while forms.get().is_cons() {
                let (af, r) = cp(forms.get());
                let rest = roots.root(r);
                rooted_args.push(roots.root(eval_form(af, env)?));
                forms.set(rest.get());
            }
            // T0→T1 tiering: run a GLOBAL compiled callee through the promoting
            // bytecode/native path so hot functions promote even when called from
            // tree-walked code (e.g. a `loop`, which never compiles to bytecode).
            // Guard on no lexical shadow — never redirect an FLET/LABELS binding
            // to the global registry entry of the same name.
            if !env.funs.borrow().contains_key(&name) {
                // Dispatch through the resolved function object's OWN name, not
                // the called name. For an aliased installation — `(setf
                // (symbol-function 'p) (symbol-function 'q))`, `… #'q`, or a
                // source-free `(lambda …)` whose real code is registered under a
                // fresh gensym — the alias `p` carries no registry entry and its
                // interpreted body may be an empty shell, so keying dispatch on
                // `p` ran the empty body and returned NIL. The object's `name`
                // cell is the symbol its bytecode is registered under (bliss-57m).
                let dispatch_sym = global_fn(&name)
                    .map(bliss_rt::function::name)
                    .filter(|n| n.is_symbol())
                    .or_else(|| resolve_sym(&name));
                if let Some(sym) = dispatch_sym {
                    let args: Vec<BlissVal> = rooted_args.iter().map(|a| a.get()).collect();
                    if let Some(res) =
                        bytecode::call_registered(sym.as_symbol_index(), &args, sym, env)
                    {
                        return res;
                    }
                }
            }
            // Re-read from the roots only now: call_registered may have collected.
            let args: Vec<BlissVal> = rooted_args.iter().map(|a| a.get()).collect();
            return eval_lambda_call(
                env,
                params_form.get(),
                body.get(),
                &args,
                Rc::clone(&env.frame),
            );
        }

        // Check accessor functions (from DEFCLASS). Match by full name or bare
        // name so a package-qualified call resolves to a reader stored under its
        // defining-package spelling (bliss-lb6.14).
        let mut accessor_slot_name: Option<String> = None;
        let bare_op = symbol_bare_name(&name);
        for class in env.classes.borrow().values() {
            for slot in &class.slots {
                let reader_match = slot
                    .accessor
                    .as_ref()
                    .map(|acc| acc == &name || symbol_bare_name(acc) == bare_op)
                    .unwrap_or(false)
                    || slot
                        .readers
                        .iter()
                        .any(|reader| reader == &name || symbol_bare_name(reader) == bare_op);
                if reader_match {
                    accessor_slot_name = Some(slot.name.clone());
                }
            }
        }
        if let Some(slot_name) = accessor_slot_name {
            let (inst_form, _) = cp(cdr);
            let inst = eval_form(inst_form, env)?;
            // A reader on a non-instance defers to an applicable explicit method
            // (e.g. ASDF's system-source-file on STRING/SYMBOL); read_slot_value
            // itself yields NIL for a non-instance rather than crashing.
            if !bliss_stdlib::is_instance(inst) {
                let args = [inst];
                if has_applicable_method(env, &name, &args) {
                    return invoke_generic_function(&name, &args, env);
                }
            }
            return read_slot_value(inst, resolve_sym(&slot_name).unwrap_or(NIL), env);
        }

        // Check methods
        if env.generics.borrow().contains_key(&name) || env.methods.borrow().contains_key(&name) {
            let args = eval_args(cdr, env)?;
            return invoke_generic_function(&name, &args, env);
        }

        // Check for package-qualified symbols (e.g., TEST-PKG:HELLO)
        if name.contains(':') {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            if parts.len() == 2 {
                let _pkg_name = parts[0];
                let fn_name = parts[1].trim_start_matches(':');
                if let Some((params_form, body)) = callable_body(env, fn_name) {
                    let args = eval_args(cdr, env)?;
                    return eval_lambda_call(env, params_form, body, &args, Rc::clone(&env.frame));
                }
            }
        }
    }

    // Lambda application: ((lambda (params) body) args...)
    if car.is_cons() {
        let (lh, lr) = cp(car);
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body_rest) = cp(lr);
            let args = eval_args(cdr, env)?;
            return eval_lambda_call(env, params_form, body_rest, &args, Rc::clone(&env.frame));
        }
    }

    // CL:DISASSEMBLE (spec §6) — render the callee's current tier: annotated
    // bytecode while interpreted (T0), decoded x86-64 once native (T1).
    if car.is_symbol() && symbol_bare_name(&sym_name(car)) == "DISASSEMBLE" {
        let arg = if cdr.is_cons() {
            eval_form(cp(cdr).0, env)?
        } else {
            NIL
        };
        let listing = if arg.is_symbol() {
            bytecode::disassemble_by_symbol(arg.as_symbol_index())
        } else {
            None
        };
        match listing {
            Some(s) => print!("{s}"),
            None => println!(
                "; DISASSEMBLE: {arg:?} is not a compiled Bliss function (a builtin or interpreted closure)"
            ),
        }
        return Ok(NIL);
    }

    if std::env::var_os("BLISS_TRACE_UNDEF_FN").is_some() && car.is_symbol() {
        let idx = car.as_symbol_index();
        let obj = bliss_rt::symbols::symbol_object_ptr(idx);
        eprintln!(
            "[undef-fn] symbol {:?} idx={} object_ptr={:?} function_cell_addr={:?}",
            sym_name(car),
            idx,
            obj.map(|p| p as *const u8),
            obj.map(|p| (p + 24) as *const u8),
        );
    }
    Err(BlissError::UndefinedFunction(car))
}

fn eval_progn(forms: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let remaining = roots.root(forms);
    let mut r = NIL;
    while remaining.get().is_cons() {
        let (f, rest) = cp(remaining.get());
        let rest = roots.root(rest);
        r = eval_form(f, env)?;
        remaining.set(rest.get());
    }
    Ok(r)
}

// ── LOOP (extended, bootstrap subset) ─────────────────────────────
//
// Supports the clauses ASDF's load path exercises:
//   :with var = init [:and var = init]*
//   :for pat :in list        (pat may be a destructuring pattern, e.g. (k . v))
//   :for pat :on list
//   :initially form*   :finally form*   (a (return X) form ends the loop with X)
//   :when/:if/:unless test <clause> [:and <clause>]* [:else <clause>+] [:end]
//   :collect/:append expr [:into var]
//   :do form*          :return expr
// A "simple" LOOP (no leading keyword) runs its body repeatedly until a
// top-level (return X); nested returns are not detected (not needed at load
// time) and a hard iteration cap guards against runaway loops.

fn is_loop_keyword(bare: &str) -> bool {
    matches!(
        bare,
        "WITH"
            | "AND"
            | "FOR"
            | "AS"
            | "IN"
            | "ON"
            | "BEING"
            | "THE"
            | "OF"
            | "THEN"
            | "WHEN"
            | "IF"
            | "UNLESS"
            | "ELSE"
            | "END"
            | "COLLECT"
            | "COLLECTING"
            | "APPEND"
            | "APPENDING"
            | "NCONC"
            | "NCONCING"
            | "DO"
            | "DOING"
            | "RETURN"
            | "THEREIS"
            | "INTO"
            | "FINALLY"
            | "INITIALLY"
            | "WHILE"
            | "UNTIL"
            | "REPEAT"
            // Numeric-iteration and across sub-keywords for `for var ...`.
            | "FROM"
            | "UPFROM"
            | "DOWNFROM"
            | "TO"
            | "UPTO"
            | "DOWNTO"
            | "BELOW"
            | "ABOVE"
            | "BY"
            | "ACROSS"
            // Accumulation clause keywords.
            | "SUM"
            | "SUMMING"
            | "COUNT"
            | "COUNTING"
            | "MAXIMIZE"
            | "MAXIMIZING"
            | "MINIMIZE"
            | "MINIMIZING"
            | "ALWAYS"
            | "NEVER"
    )
}

#[derive(Clone)]
enum LoopClause {
    Do(Vec<BlissVal>),
    Collect(BlissVal, Option<String>),
    Append(BlissVal, Option<String>),
    Sum(BlissVal, Option<String>),
    Count(BlissVal, Option<String>),
    Maximize(BlissVal, Option<String>),
    Minimize(BlissVal, Option<String>),
    Always(BlissVal),
    Never(BlissVal),
    Return(BlissVal),
    ThereIs(BlissVal),
    // `:while`/`:until` are body clauses: they test at their lexical position,
    // so an `:until` written after `:collect` terminates only after the collect
    // has run for the final iteration. Terminating clears no accumulation.
    While(BlissVal),
    Until(BlissVal),
    Cond {
        test: BlissVal,
        negate: bool,
        then: Vec<LoopClause>,
        els: Vec<LoopClause>,
    },
}

struct LoopParser {
    toks: Vec<BlissVal>,
    pos: usize,
}

impl LoopParser {
    fn peek(&self) -> Option<BlissVal> {
        self.toks.get(self.pos).copied()
    }
    fn advance(&mut self) -> Option<BlissVal> {
        let v = self.peek();
        if v.is_some() {
            self.pos += 1;
        }
        v
    }
    /// Bare (package-stripped, upcased) name of the next token, but only when
    /// it is a recognised LOOP keyword — otherwise None (it's an expression).
    fn peek_kw(&self) -> Option<String> {
        let v = self.peek()?;
        if !v.is_symbol() {
            return None;
        }
        let n = sym_name(v);
        let bare = n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string();
        if is_loop_keyword(&bare) {
            Some(bare)
        } else {
            None
        }
    }
    fn at_kw(&self, k: &str) -> bool {
        self.peek_kw().as_deref() == Some(k)
    }
    /// True if the next token is the symbol with the given bare name (e.g. "="),
    /// regardless of package. Does not consume.
    fn at_sym(&self, name: &str) -> bool {
        match self.peek() {
            Some(v) if v.is_symbol() => symbol_bare_name(&sym_name(v)) == name,
            _ => false,
        }
    }
    fn read_form(&mut self) -> Result<BlissVal, BlissError> {
        self.advance()
            .ok_or_else(|| BlissError::Internal("LOOP: unexpected end of clauses".into()))
    }
    /// Read one-or-more forms up to the next LOOP keyword (for :do/:finally).
    fn read_forms(&mut self) -> Vec<BlissVal> {
        let mut forms = Vec::new();
        while self.pos < self.toks.len() && self.peek_kw().is_none() {
            forms.push(self.advance().unwrap());
        }
        forms
    }
    fn read_into(&mut self) -> Result<Option<String>, BlissError> {
        if self.at_kw("INTO") {
            self.advance();
            Ok(Some(sym_name(self.read_form()?)))
        } else {
            Ok(None)
        }
    }
    /// Consume an optional trailing `of-type <type>` on an accumulation or
    /// iteration clause (e.g. `sum 1 into n of-type fixnum`). `OF-TYPE` is not
    /// a LOOP keyword, so without this the main dispatch would silently stop at
    /// it and drop every clause that follows.
    fn skip_of_type(&mut self) -> Result<(), BlissError> {
        if self.at_sym("OF-TYPE") {
            self.advance();
            self.read_form()?;
        }
        Ok(())
    }
    fn parse_clause(&mut self) -> Result<LoopClause, BlissError> {
        let kw = self
            .peek_kw()
            .ok_or_else(|| BlissError::Internal("LOOP: expected a clause keyword".into()))?;
        self.advance();
        match kw.as_str() {
            "COLLECT" | "COLLECTING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Collect(e, into))
            }
            "APPEND" | "APPENDING" | "NCONC" | "NCONCING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Append(e, into))
            }
            "SUM" | "SUMMING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Sum(e, into))
            }
            "COUNT" | "COUNTING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Count(e, into))
            }
            "MAXIMIZE" | "MAXIMIZING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Maximize(e, into))
            }
            "MINIMIZE" | "MINIMIZING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                self.skip_of_type()?;
                Ok(LoopClause::Minimize(e, into))
            }
            "ALWAYS" => Ok(LoopClause::Always(self.read_form()?)),
            "NEVER" => Ok(LoopClause::Never(self.read_form()?)),
            "WHILE" => Ok(LoopClause::While(self.read_form()?)),
            "UNTIL" => Ok(LoopClause::Until(self.read_form()?)),
            "DO" | "DOING" => Ok(LoopClause::Do(self.read_forms())),
            "RETURN" => Ok(LoopClause::Return(self.read_form()?)),
            "THEREIS" => Ok(LoopClause::ThereIs(self.read_form()?)),
            "WHEN" | "IF" => self.parse_cond(false),
            "UNLESS" => self.parse_cond(true),
            other => Err(BlissError::Internal(format!(
                "LOOP: unsupported clause `{}`",
                other
            ))),
        }
    }
    fn parse_cond(&mut self, negate: bool) -> Result<LoopClause, BlissError> {
        let test = self.read_form()?;
        let mut then = vec![self.parse_clause()?];
        while self.at_kw("AND") {
            self.advance();
            then.push(self.parse_clause()?);
        }
        let mut els = Vec::new();
        if self.at_kw("ELSE") {
            self.advance();
            els.push(self.parse_clause()?);
            while self.at_kw("AND") {
                self.advance();
                els.push(self.parse_clause()?);
            }
        }
        if self.at_kw("END") {
            self.advance();
        }
        Ok(LoopClause::Cond {
            test,
            negate,
            then,
            els,
        })
    }
}

/// A scalar LOOP accumulator (sum / count / maximize / minimize).
#[derive(Clone)]
enum NumAcc {
    Sum(BlissVal),
    Count(i64),
    Max(Option<BlissVal>),
    Min(Option<BlissVal>),
}

impl NumAcc {
    fn finalize(&self) -> BlissVal {
        match self {
            NumAcc::Sum(v) => *v,
            NumAcc::Count(c) => BlissVal::from_fixnum(*c),
            NumAcc::Max(v) | NumAcc::Min(v) => v.unwrap_or(NIL),
        }
    }
}

#[derive(Default)]
struct LoopAccs {
    map: std::collections::HashMap<Option<String>, Vec<BlissVal>>,
    nums: std::collections::HashMap<Option<String>, NumAcc>,
    /// Default result for a boolean loop (ALWAYS/NEVER): T unless short-circuited.
    bool_default: Option<BlissVal>,
}

impl LoopAccs {
    fn collect(&mut self, key: Option<String>, v: BlissVal) {
        self.map.entry(key).or_default().push(v);
    }
    fn append(&mut self, key: Option<String>, v: BlissVal) {
        let items = list_to_vec(v);
        self.map.entry(key).or_default().extend(items);
    }
    fn sum(&mut self, key: Option<String>, v: BlissVal) -> Result<(), BlissError> {
        let entry = self
            .nums
            .entry(key)
            .or_insert(NumAcc::Sum(BlissVal::from_fixnum(0)));
        if let NumAcc::Sum(acc) = entry {
            *acc = loop_add_numbers(*acc, v)?;
        }
        Ok(())
    }
    fn count(&mut self, key: Option<String>, truthy: bool) {
        let entry = self.nums.entry(key).or_insert(NumAcc::Count(0));
        if let NumAcc::Count(c) = entry {
            if truthy {
                *c += 1;
            }
        }
    }
    fn maximize(&mut self, key: Option<String>, v: BlissVal) -> Result<(), BlissError> {
        let entry = self.nums.entry(key).or_insert(NumAcc::Max(None));
        if let NumAcc::Max(cur) = entry {
            let take = match cur {
                None => true,
                Some(c) => num_val(v)? > num_val(*c)?,
            };
            if take {
                *cur = Some(v);
            }
        }
        Ok(())
    }
    fn minimize(&mut self, key: Option<String>, v: BlissVal) -> Result<(), BlissError> {
        let entry = self.nums.entry(key).or_insert(NumAcc::Min(None));
        if let NumAcc::Min(cur) = entry {
            let take = match cur {
                None => true,
                Some(c) => num_val(v)? < num_val(*c)?,
            };
            if take {
                *cur = Some(v);
            }
        }
        Ok(())
    }
}

impl LoopAccs {
    /// Visit every `BlissVal` this accumulator holds (bliss-6b2 #2). The values
    /// live in Rust-heap `Vec`/map buffers the collector can't see, yet persist
    /// across the whole loop body's allocating iterations — so they must be
    /// scanned as roots (relocated in place) or a minor/major GC would free or
    /// move their referents out from under the stored copies.
    fn visit_roots(&mut self, visit: &mut dyn FnMut(*mut BlissVal)) {
        for vals in self.map.values_mut() {
            for v in vals.iter_mut() {
                visit(v as *mut BlissVal);
            }
        }
        for n in self.nums.values_mut() {
            match n {
                NumAcc::Sum(v) => visit(v as *mut BlissVal),
                NumAcc::Max(Some(v)) | NumAcc::Min(Some(v)) => visit(v as *mut BlissVal),
                _ => {}
            }
        }
        if let Some(v) = self.bool_default.as_mut() {
            visit(v as *mut BlissVal);
        }
    }
}

impl ForState {
    /// Visit every `BlissVal` this for-clause cursor holds (bliss-6b2 #2), for the
    /// same reason as [`LoopAccs::visit_roots`] — `items`/`tail`/`current`/… span
    /// the loop's allocating body.
    fn visit_roots(&mut self, visit: &mut dyn FnMut(*mut BlissVal)) {
        match self {
            ForState::In { pat, items, .. }
            | ForState::Across { pat, items, .. }
            | ForState::Being { pat, items, .. } => {
                visit(pat as *mut BlissVal);
                for it in items.iter_mut() {
                    visit(it as *mut BlissVal);
                }
            }
            ForState::On { pat, tail, step } => {
                visit(pat as *mut BlissVal);
                visit(tail as *mut BlissVal);
                if let Some(s) = step.as_mut() {
                    visit(s as *mut BlissVal);
                }
            }
            ForState::Eq { pat, init, then } => {
                visit(pat as *mut BlissVal);
                visit(init as *mut BlissVal);
                if let Some(t) = then.as_mut() {
                    visit(t as *mut BlissVal);
                }
            }
            ForState::From {
                pat,
                current,
                step,
                limit,
            } => {
                visit(pat as *mut BlissVal);
                visit(current as *mut BlissVal);
                visit(step as *mut BlissVal);
                if let Some((_, v)) = limit.as_mut() {
                    visit(v as *mut BlissVal);
                }
            }
        }
    }
}

thread_local! {
    /// Active extended-LOOP root sources (accumulators + for-clause cursors) on
    /// this thread, scanned by [`scan_loop_roots`] during every GC (bliss-6b2 #2).
    static LOOP_ROOTS: RefCell<Vec<(*mut LoopAccs, *mut Vec<ForState>)>> =
        const { RefCell::new(Vec::new()) };
}

fn scan_loop_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    LOOP_ROOTS.with(|r| {
        for &(accs, states) in r.borrow().iter() {
            // SAFETY: guards register these only for the lexical extent in which
            // the pointed-to locals remain live and immobile (they are not moved
            // while a guard is on the stack).
            unsafe {
                (*accs).visit_roots(visit);
                for st in (*states).iter_mut() {
                    st.visit_roots(visit);
                }
            }
        }
    });
}

fn install_loop_root_scanner() {
    static INIT: Once = Once::new();
    INIT.call_once(|| bliss_rt::gc::register_root_scanner(scan_loop_roots));
}

/// Registers a loop's `accs`/`states` as GC roots for its lexical extent.
struct LoopRootGuard;

impl LoopRootGuard {
    fn new(accs: &mut LoopAccs, states: &mut Vec<ForState>) -> Self {
        install_loop_root_scanner();
        let accs = accs as *mut LoopAccs;
        let states = states as *mut Vec<ForState>;
        LOOP_ROOTS.with(|r| r.borrow_mut().push((accs, states)));
        LoopRootGuard
    }
}

impl Drop for LoopRootGuard {
    fn drop(&mut self) {
        LOOP_ROOTS.with(|r| {
            r.borrow_mut().pop();
        });
    }
}

thread_local! {
    /// `Vec<BlissVal>` buffers registered as GC roots for a lexical extent
    /// (bliss-6b2 #2) — for accumulators/scratch the collector can't otherwise
    /// see (they live on the Rust heap). Scanned in place so the GC relocates
    /// their contents.
    static VEC_ROOTS: RefCell<Vec<*mut Vec<BlissVal>>> = const { RefCell::new(Vec::new()) };
}

fn scan_vec_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    VEC_ROOTS.with(|r| {
        for &v in r.borrow().iter() {
            // SAFETY: registered only for the extent its owning local is live.
            unsafe {
                for e in (*v).iter_mut() {
                    visit(e as *mut BlissVal);
                }
            }
        }
    });
}

fn install_vec_root_scanner() {
    static INIT: Once = Once::new();
    INIT.call_once(|| bliss_rt::gc::register_root_scanner(scan_vec_roots));
}

/// Registers a `Vec<BlissVal>` as a GC root for its lexical extent.
struct VecRootGuard(*mut Vec<BlissVal>);

impl VecRootGuard {
    fn new(v: &mut Vec<BlissVal>) -> Self {
        install_vec_root_scanner();
        let p = v as *mut Vec<BlissVal>;
        VEC_ROOTS.with(|r| r.borrow_mut().push(p));
        VecRootGuard(p)
    }
}

impl Drop for VecRootGuard {
    fn drop(&mut self) {
        VEC_ROOTS.with(|r| {
            let mut v = r.borrow_mut();
            if let Some(pos) = v.iter().rposition(|&p| p == self.0) {
                v.remove(pos);
            }
        });
    }
}

/// A `Vec<BlissVal>` kept GC-rooted for its ENTIRE lifetime (bliss-6b2 #2).
///
/// `eval_args`/`eval_forms` return this instead of a bare `Vec` so evaluated
/// arguments stay rooted while the CALLER uses them — e.g. across an
/// `apply_function` / `eval_lambda_call` / `invoke_generic_function` whose callee
/// allocates and can relocate them — not merely while they are being evaluated.
/// (Rooting that ended at the evaluator's return left every caller unprotected.)
/// The buffer is boxed so it has a stable address the guard can point at even as
/// the `RootedVals` value is moved (returned) around. Derefs to `[BlissVal]`.
struct RootedVals {
    vals: Box<Vec<BlissVal>>,
    _guard: VecRootGuard,
}

impl RootedVals {
    fn new(vals: Vec<BlissVal>) -> Self {
        let mut vals = Box::new(vals);
        let guard = VecRootGuard::new(&mut vals);
        RootedVals { vals, _guard: guard }
    }
    fn push(&mut self, v: BlissVal) {
        self.vals.push(v);
    }
}

impl std::ops::Deref for RootedVals {
    type Target = [BlissVal];
    fn deref(&self) -> &[BlissVal] {
        &self.vals
    }
}

/// Bind a (possibly destructuring / dotted) pattern against a value.
fn loop_bind(pattern: BlissVal, value: BlissVal, env: &mut Env) {
    if pattern.is_symbol() {
        if sym_name(pattern) != "NIL" {
            // Bind through the symbol-indexed store as well, so the loop
            // variable properly shadows any outer lexical binding of the same
            // name (symbol lookup consults symbol_vars before the name map).
            env.define_local_symbol(pattern, value);
        }
    } else if pattern.is_cons() {
        let (pcar, pcdr) = cp(pattern);
        let (vcar, vcdr) = if value.is_cons() {
            cp(value)
        } else {
            (NIL, NIL)
        };
        loop_bind(pcar, vcar, env);
        loop_bind(pcdr, vcdr, env);
    }
}

/// True if any clause is a `:while`/`:until` terminator (recursing into
/// conditional branches). Such a clause is a terminating driver, so a loop that
/// has one is neither run-once nor subject to the runaway-iteration cap.
fn loop_body_has_terminator(clauses: &[LoopClause]) -> bool {
    clauses.iter().any(|c| match c {
        LoopClause::While(_) | LoopClause::Until(_) => true,
        LoopClause::Cond { then, els, .. } => {
            loop_body_has_terminator(then) || loop_body_has_terminator(els)
        }
        _ => false,
    })
}

/// Collect every `:into` accumulator name so they can be bound to NIL up
/// front — LOOP guarantees accumulators are bound even if never accumulated,
/// and :finally clauses read them.
fn loop_collect_intos(clauses: &[LoopClause], out: &mut Vec<String>) {
    for c in clauses {
        match c {
            LoopClause::Collect(_, Some(n))
            | LoopClause::Append(_, Some(n))
            | LoopClause::Sum(_, Some(n))
            | LoopClause::Count(_, Some(n))
            | LoopClause::Maximize(_, Some(n))
            | LoopClause::Minimize(_, Some(n)) => {
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
            LoopClause::Cond { then, els, .. } => {
                loop_collect_intos(then, out);
                loop_collect_intos(els, out);
            }
            _ => {}
        }
    }
}

fn loop_exec_clauses(
    clauses: &[LoopClause],
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
    terminate: &mut bool,
) -> Result<(), BlissError> {
    for c in clauses {
        if ret.is_some() || *terminate {
            break;
        }
        loop_exec_clause(c, env, accs, ret, terminate)?;
    }
    Ok(())
}

/// Evaluate a LOOP clause value-form, honouring the `it` anaphor (CLHS 6.1.5):
/// inside the selected branch of a `when`/`if`/`unless`, the bare token `it`
/// (symbol `it` or keyword `:it` — LOOP matches anaphora by name) stands for the
/// value of the conditional test, which the `Cond` executor has bound to the
/// lexical variable `IT`. A nested `it` (e.g. `collect (list it)`) resolves via
/// that same binding through the normal evaluator.
fn loop_it_eval(expr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    if expr.is_symbol() && symbol_bare_name(&sym_name(expr)) == "IT" {
        if let Some(v) = env.lookup_var("IT") {
            return Ok(v);
        }
    }
    eval_form(expr, env)
}

fn loop_exec_clause(
    c: &LoopClause,
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
    terminate: &mut bool,
) -> Result<(), BlissError> {
    match c {
        LoopClause::Do(forms) => {
            for f in forms {
                if ret.is_some() {
                    break;
                }
                loop_it_eval(*f, env)?;
            }
        }
        LoopClause::Return(e) => {
            *ret = Some(loop_it_eval(*e, env)?);
        }
        LoopClause::ThereIs(e) => {
            let v = loop_it_eval(*e, env)?;
            if !v.is_nil() {
                *ret = Some(v);
            }
        }
        LoopClause::Always(e) => {
            accs.bool_default.get_or_insert(T);
            if loop_it_eval(*e, env)?.is_nil() {
                *ret = Some(NIL);
            }
        }
        LoopClause::Never(e) => {
            accs.bool_default.get_or_insert(T);
            if !loop_it_eval(*e, env)?.is_nil() {
                *ret = Some(NIL);
            }
        }
        LoopClause::Sum(e, into) => {
            let v = loop_it_eval(*e, env)?;
            accs.sum(into.clone(), v)?;
        }
        LoopClause::Count(e, into) => {
            let truthy = !loop_it_eval(*e, env)?.is_nil();
            accs.count(into.clone(), truthy);
        }
        LoopClause::Maximize(e, into) => {
            let v = loop_it_eval(*e, env)?;
            accs.maximize(into.clone(), v)?;
        }
        LoopClause::Minimize(e, into) => {
            let v = loop_it_eval(*e, env)?;
            accs.minimize(into.clone(), v)?;
        }
        LoopClause::Collect(e, into) => {
            let v = loop_it_eval(*e, env)?;
            accs.collect(into.clone(), v);
        }
        LoopClause::Append(e, into) => {
            let v = loop_it_eval(*e, env)?;
            accs.append(into.clone(), v);
        }
        LoopClause::While(e) => {
            if eval_form(*e, env)?.is_nil() {
                *terminate = true;
            }
        }
        LoopClause::Until(e) => {
            if !eval_form(*e, env)?.is_nil() {
                *terminate = true;
            }
        }
        LoopClause::Cond {
            test,
            negate,
            then,
            els,
        } => {
            let t = eval_form(*test, env)?;
            let take = !t.is_nil() ^ *negate;
            if take {
                // Bind the `it` anaphor (CLHS 6.1.5) to the test value for the
                // selected branch, so `when TEST collect it` collects TEST's
                // value (and a nested `it` resolves through this same binding).
                env.define_local("IT", t);
                loop_exec_clauses(then, env, accs, ret, terminate)?;
            } else {
                loop_exec_clauses(els, env, accs, ret, terminate)?;
            }
        }
    }
    Ok(())
}

/// If `form` is (RETURN x) or (RETURN-FROM nil x), return Some(x-form).
fn loop_return_target(form: BlissVal) -> Option<BlissVal> {
    if !form.is_cons() {
        return None;
    }
    let (head, rest) = cp(form);
    if !head.is_symbol() {
        return None;
    }
    match sym_name(head).as_str() {
        "RETURN" => Some(cp(rest).0),
        "RETURN-FROM" => {
            let (_blk, r) = cp(rest);
            Some(cp(r).0)
        }
        _ => None,
    }
}

/// A parsed `:for` iteration clause.
enum ForClause {
    In {
        pat: BlissVal,
        list_form: BlissVal,
        /// Optional `by` step function form (default: `cdr`).
        step: Option<BlissVal>,
    },
    On {
        pat: BlissVal,
        list_form: BlissVal,
        /// Optional `by` step function form (default: `cdr`).
        step: Option<BlissVal>,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        start: BlissVal,
        step: Option<BlissVal>,
        limit: Option<(LoopForLimit, BlissVal)>,
        /// `:downfrom` counts downward: default step is -1 and a plain `:to` /
        /// `:below` limit is read as its descending counterpart.
        descending: bool,
    },
    Across {
        pat: BlissVal,
        seq_form: BlissVal,
    },
    Being {
        pat: BlissVal,
        source: LoopBeingSource,
    },
}

/// Runtime cursor for a `:for` clause.
enum ForState {
    In {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    On {
        pat: BlissVal,
        tail: BlissVal,
        /// Evaluated `by` step function (None → step by cdr).
        step: Option<BlissVal>,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        current: BlissVal,
        step: BlissVal,
        limit: Option<(LoopForLimit, BlissVal)>,
    },
    Across {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    Being {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
}

#[derive(Clone, Copy)]
enum LoopForLimit {
    Below,
    To,
    Upto,
    Above,
    Downto,
}

enum LoopBeingSource {
    /// `:being :the :symbols :in pkg` — all accessible symbols (incl. inherited).
    Symbols(BlissVal),
    /// `:being :the {:external-symbols | :present-symbols} :in pkg` — the package's
    /// own symbols (matching DO-EXTERNAL-SYMBOLS; ASDF DEFINE-PACKAGE exports what
    /// it homes here).
    OwnSymbols(BlissVal),
    HashKeys(BlissVal),
    HashValues(BlissVal),
}

fn eval_loop(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // Run the loop in a fresh variable frame (for iteration variables and
    // accumulators) while keeping the shared global tables mutable in place, so
    // definitions made in the loop body (intern, use-package, defun, …) persist.
    let parent = Rc::clone(&env.frame);
    // LOOP establishes an implicit `block nil`, so a bare `(return x)` in the
    // body (e.g. simple loops) exits with x, alongside LOOP's own RETURN clause.
    with_block_nil(env, move |env| {
        with_child_frame(env, parent, |env| eval_loop_inner(cdr, env))
    })
}

fn eval_loop_inner(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let toks = list_to_vec(cdr);

    // Simple LOOP: no leading keyword -> repeat body until a top-level return.
    let starts_with_kw = toks
        .first()
        .and_then(|v| {
            if v.is_symbol() {
                let n = sym_name(*v);
                Some(is_loop_keyword(n.strip_prefix("KEYWORD:").unwrap_or(&n)))
            } else {
                None
            }
        })
        .unwrap_or(false);
    if !starts_with_kw {
        // Walk the LIVE body cons list through a rooted cursor each iteration
        // rather than a stale `toks` snapshot (bliss-6b2 #2): the simple loop
        // runs unbounded, and each body form's evaluation can fire a GC that
        // relocates the remaining, not-yet-evaluated body forms. Two reused root
        // slots (never grown in the loop) keep the head and cursor relocated.
        let roots = bliss_rt::ShadowRootScope::new();
        let body = roots.root(cdr);
        let c = roots.root(cdr);
        let mut guard: u64 = 0;
        loop {
            c.set(body.get());
            while c.get().is_cons() {
                let f = cp(c.get()).0;
                if let Some(tgt) = loop_return_target(f) {
                    return eval_form(tgt, env);
                }
                eval_form(f, env)?;
                // Re-read the tail from the rooted cursor after evaluation: a GC
                // fired inside eval_form would have staled a plain `rest` local.
                c.set(cp(c.get()).1);
            }
            guard += 1;
            if guard > 10_000_000 {
                return Err(BlissError::Internal(
                    "LOOP: simple loop exceeded iteration cap".into(),
                ));
            }
        }
    }

    // Extended LOOP.
    let mut p = LoopParser { toks, pos: 0 };
    let mut with_bindings: Vec<(BlissVal, BlissVal)> = Vec::new();
    let mut for_clauses: Vec<ForClause> = Vec::new();
    let mut initially: Vec<BlissVal> = Vec::new();
    let mut finally: Vec<BlissVal> = Vec::new();
    let mut body: Vec<LoopClause> = Vec::new();
    // `repeat N`: run the body at most N times.
    let mut repeat_form: Option<BlissVal> = None;

    while let Some(kw) = p.peek_kw() {
        match kw.as_str() {
            "WITH" => {
                p.advance();
                loop {
                    let var = p.read_form()?;
                    // Optional `of-type <type>` is accepted and ignored.
                    if p.at_sym("OF-TYPE") {
                        p.advance();
                        p.read_form()?;
                    } else if p.toks.get(p.pos + 1).is_some_and(|next| {
                        next.is_symbol() && symbol_bare_name(&sym_name(*next)) == "="
                    }) {
                        // CLHS 6.1.1.7 also permits a bare type specifier
                        // between the WITH variable and `=`.  Babel's generated
                        // counters use `with noctets fixnum = 0`.
                        p.advance();
                    }
                    // `= init` is optional; a bare `:with var` binds var to NIL.
                    let init = if p.at_sym("=") {
                        p.advance();
                        p.read_form()?
                    } else {
                        NIL
                    };
                    with_bindings.push((var, init));
                    if p.at_kw("AND") {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            // `AS` is a standard synonym for `FOR` (CLHS 6.1.2.1).
            "FOR" | "AS" => {
                p.advance();
                // `for a ... and b ...` chains parallel iteration clauses that
                // step together (babel's encoders use `for i fixnum from s
                // below e and di fixnum from d`). We collect each into
                // for_clauses; for the independent counters seen in practice
                // parallel and sequential stepping coincide.
                loop {
                    let pat = p.read_form()?;
                    // Optional `:of-type <type>` type declaration is accepted and ignored.
                    if p.at_sym("OF-TYPE") {
                        p.advance();
                        p.read_form()?;
                    } else if p.at_sym("FIXNUM")
                        || p.at_sym("FLOAT")
                        || p.at_sym("T")
                        || p.at_sym("NIL")
                    {
                        // CLHS 6.1.1.7: a bare simple-type-spec (fixnum | float |
                        // t | nil) may follow the loop var, e.g. `for i fixnum from
                        // 0 below n` (babel's encoders use this). Declarations have
                        // no bearing on the tree-walker, so skip it.
                        p.advance();
                    }
                    match p.peek_kw().as_deref() {
                        Some("IN") => {
                            p.advance();
                            let list_form = p.read_form()?;
                            let step = if p.at_kw("BY") {
                                p.advance();
                                Some(p.read_form()?)
                            } else {
                                None
                            };
                            for_clauses.push(ForClause::In {
                                pat,
                                list_form,
                                step,
                            });
                        }
                        Some("ON") => {
                            p.advance();
                            let list_form = p.read_form()?;
                            let step = if p.at_kw("BY") {
                                p.advance();
                                Some(p.read_form()?)
                            } else {
                                None
                            };
                            for_clauses.push(ForClause::On {
                                pat,
                                list_form,
                                step,
                            });
                        }
                        Some("ACROSS") => {
                            p.advance();
                            let seq_form = p.read_form()?;
                            for_clauses.push(ForClause::Across { pat, seq_form });
                        }
                        Some("BEING") => {
                            p.advance();
                            if p.at_kw("THE") {
                                p.advance();
                            }
                            let kind = p.read_form()?;
                            let kind_name = if kind.is_symbol() {
                                sym_name(kind)
                            } else {
                                String::new()
                            };
                            let kind_bare =
                                kind_name.strip_prefix("KEYWORD:").unwrap_or(&kind_name);
                            let source = match kind_bare {
                                "SYMBOLS" | "EXTERNAL-SYMBOLS" | "PRESENT-SYMBOLS" => {
                                    // The connective is :in or :of (ANSI accepts both).
                                    let conn = p.read_form()?;
                                    let conn_name = if conn.is_symbol() {
                                        sym_name(conn)
                                    } else {
                                        String::new()
                                    };
                                    let conn_bare =
                                        conn_name.strip_prefix("KEYWORD:").unwrap_or(&conn_name);
                                    if conn_bare != "IN" && conn_bare != "OF" {
                                        return Err(BlissError::Internal(
                                            "LOOP :for ... :being <symbols> expects :in or :of"
                                                .into(),
                                        ));
                                    }
                                    let pkg = p.read_form()?;
                                    if kind_bare == "SYMBOLS" {
                                        LoopBeingSource::Symbols(pkg)
                                    } else {
                                        LoopBeingSource::OwnSymbols(pkg)
                                    }
                                }
                                "HASH-KEYS" => {
                                    let of_kw = p.read_form()?;
                                    let of_name = if of_kw.is_symbol() {
                                        sym_name(of_kw)
                                    } else {
                                        String::new()
                                    };
                                    if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF"
                                    {
                                        return Err(BlissError::Internal(
                                            "LOOP :for ... :being :the :hash-keys expects :of"
                                                .into(),
                                        ));
                                    }
                                    LoopBeingSource::HashKeys(p.read_form()?)
                                }
                                "HASH-VALUES" => {
                                    let of_kw = p.read_form()?;
                                    let of_name = if of_kw.is_symbol() {
                                        sym_name(of_kw)
                                    } else {
                                        String::new()
                                    };
                                    if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF"
                                    {
                                        return Err(BlissError::Internal(
                                            "LOOP :for ... :being :the :hash-values expects :of"
                                                .into(),
                                        ));
                                    }
                                    LoopBeingSource::HashValues(p.read_form()?)
                                }
                                _ => {
                                    return Err(BlissError::Internal(
                                    "LOOP :for ... :being supports :symbols / :external-symbols / :present-symbols / :hash-keys / :hash-values in the bootstrap"
                                        .into(),
                                ));
                                }
                            };
                            for_clauses.push(ForClause::Being { pat, source });
                        }
                        // `:from`/`:upfrom` count up; `:downfrom` counts down. All
                        // share the same `[:by step] [limit]` tail parsing.
                        Some("FROM") | Some("UPFROM") | Some("DOWNFROM") => {
                            let descending = p.peek_kw().as_deref() == Some("DOWNFROM");
                            p.advance();
                            let start = p.read_form()?;
                            let mut step = None;
                            let mut limit = None;
                            loop {
                                match p.peek_kw().as_deref() {
                                    Some("BY") => {
                                        p.advance();
                                        step = Some(p.read_form()?);
                                    }
                                    Some("BELOW") => {
                                        p.advance();
                                        limit = Some((LoopForLimit::Below, p.read_form()?));
                                    }
                                    Some("TO") => {
                                        p.advance();
                                        limit = Some((LoopForLimit::To, p.read_form()?));
                                    }
                                    Some("UPTO") => {
                                        p.advance();
                                        limit = Some((LoopForLimit::Upto, p.read_form()?));
                                    }
                                    Some("ABOVE") => {
                                        p.advance();
                                        limit = Some((LoopForLimit::Above, p.read_form()?));
                                    }
                                    Some("DOWNTO") => {
                                        p.advance();
                                        limit = Some((LoopForLimit::Downto, p.read_form()?));
                                    }
                                    _ => break,
                                }
                            }
                            for_clauses.push(ForClause::From {
                                pat,
                                start,
                                step,
                                limit,
                                descending,
                            });
                        }
                        _ => {
                            // :for var = init [:then step]. Both `=` and the
                            // keyword `:=` are valid LOOP stepping tokens.
                            let eq = p.read_form()?;
                            if !(eq.is_symbol() && symbol_bare_name(&sym_name(eq)) == "=") {
                                let found = if eq.is_symbol() {
                                    sym_name(eq)
                                } else {
                                    format!("{:?}", eq)
                                };
                                return Err(BlissError::Internal(format!(
                                    "LOOP :for supports :in / :on / :across / :being / :from / = in the bootstrap (got {})",
                                    found
                                )));
                            }
                            let init = p.read_form()?;
                            let then = if p.at_kw("THEN") {
                                p.advance();
                                Some(p.read_form()?)
                            } else {
                                None
                            };
                            for_clauses.push(ForClause::Eq { pat, init, then });
                        }
                    }
                    if p.at_kw("AND") {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            "INITIALLY" => {
                p.advance();
                initially = p.read_forms();
            }
            "FINALLY" => {
                p.advance();
                finally = p.read_forms();
            }
            "WHILE" => {
                p.advance();
                body.push(LoopClause::While(p.read_form()?));
            }
            "UNTIL" => {
                p.advance();
                body.push(LoopClause::Until(p.read_form()?));
            }
            "REPEAT" => {
                p.advance();
                repeat_form = Some(p.read_form()?);
            }
            _ => body.push(p.parse_clause()?),
        }
    }

    // Establish :with bindings (sequential, LET*-style).
    for (var, init) in &with_bindings {
        let v = eval_form(*init, env)?;
        loop_bind(*var, v, env);
    }

    // Bind all :into accumulators to NIL up front.
    let mut into_names = Vec::new();
    loop_collect_intos(&body, &mut into_names);
    for n in &into_names {
        env.define_local(n, NIL);
    }

    let mut accs = LoopAccs::default();
    let mut ret: Option<BlissVal> = None;

    for f in &initially {
        eval_form(*f, env)?;
    }

    // `repeat N`: evaluate the count once (negative/NIL → 0 iterations).
    let mut repeat_remaining: Option<u64> = match repeat_form {
        Some(f) => Some(num_val(eval_form(f, env)?)?.max(0.0) as u64),
        None => None,
    };

    let has_terminator = loop_body_has_terminator(&body);
    // Root the accumulators and for-clause cursors for the whole loop: they hold
    // BlissVals in Rust-heap buffers across the body's allocating iterations
    // (bliss-6b2 #2). `states` is declared here (outside the branch) so one guard
    // covers both the no-driver and driver paths.
    let mut states: Vec<ForState> = Vec::with_capacity(for_clauses.len());
    let _loop_roots = LoopRootGuard::new(&mut accs, &mut states);
    if for_clauses.is_empty() && !has_terminator && repeat_remaining.is_none() {
        // No driver at all: run the body once (when/collect-only loops).
        let mut terminate = false;
        loop_exec_clauses(&body, env, &mut accs, &mut ret, &mut terminate)?;
    } else {
        // Build cursors, evaluating each list form once.
        let mut has_stepping_driver = false;
        for fc in &for_clauses {
            match fc {
                ForClause::In {
                    pat,
                    list_form,
                    step,
                } => {
                    let list = eval_form(*list_form, env)?;
                    let items = match step {
                        // `for x in list by fn`: x takes the car of each stepped
                        // tail (list, (fn list), (fn (fn list)), …).
                        Some(step_form) => {
                            let step_fn = eval_form(*step_form, env)?;
                            let mut items = Vec::new();
                            let mut tail = list;
                            while tail.is_cons() {
                                items.push(cp(tail).0);
                                tail = apply_function(step_fn, &[tail], env)?;
                            }
                            items
                        }
                        None => list_to_vec(list),
                    };
                    states.push(ForState::In {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::On {
                    pat,
                    list_form,
                    step,
                } => {
                    let list = eval_form(*list_form, env)?;
                    let step = match step {
                        Some(step_form) => Some(eval_form(*step_form, env)?),
                        None => None,
                    };
                    states.push(ForState::On {
                        pat: *pat,
                        tail: list,
                        step,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Eq { pat, init, then } => states.push(ForState::Eq {
                    pat: *pat,
                    init: *init,
                    then: *then,
                }),
                ForClause::From {
                    pat,
                    start,
                    step,
                    limit,
                    descending,
                } => {
                    let current = eval_form(*start, env)?;
                    // `:downfrom`, or a DOWNTO/ABOVE limit, counts down by default.
                    let down = *descending
                        || matches!(
                            limit,
                            Some((LoopForLimit::Downto, _)) | Some((LoopForLimit::Above, _))
                        );
                    // `:by` is a positive step magnitude; a descending loop
                    // applies it as a decrement, so negate an explicit step (and
                    // default to -1) when counting down.
                    let step = match step {
                        Some(expr) => {
                            let s = eval_form(*expr, env)?;
                            if down { loop_negate_number(s)? } else { s }
                        }
                        None => BlissVal::from_fixnum(if down { -1 } else { 1 }),
                    };
                    let limit = match limit {
                        Some((kind, expr)) => {
                            // Under `:downfrom`, a plain ascending limit is read as
                            // its descending counterpart (`to`→`downto`,
                            // `below`→`above`) so termination compares downward.
                            let kind = if *descending {
                                match kind {
                                    LoopForLimit::To | LoopForLimit::Upto => LoopForLimit::Downto,
                                    LoopForLimit::Below => LoopForLimit::Above,
                                    other => *other,
                                }
                            } else {
                                *kind
                            };
                            Some((kind, eval_form(*expr, env)?))
                        }
                        None => None,
                    };
                    states.push(ForState::From {
                        pat: *pat,
                        current,
                        step,
                        limit,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Across { pat, seq_form } => {
                    let seq = eval_form(*seq_form, env)?;
                    // `:across` walks any vector — a string yields its characters,
                    // a general vector its elements. `seq_elements` routes through
                    // the stdlib's ELT/LENGTH, so a non-string vector is iterated
                    // by element rather than mis-read as its printed string form.
                    let items = seq_elements(seq)?;
                    states.push(ForState::Across {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Being { pat, source } => {
                    let items = match source {
                        LoopBeingSource::Symbols(pkg_form) => {
                            let name =
                                normalize_package_name(&val_as_str(eval_form(*pkg_form, env)?));
                            package_symbols(env, &name, true)
                        }
                        LoopBeingSource::OwnSymbols(pkg_form) => {
                            let name =
                                normalize_package_name(&val_as_str(eval_form(*pkg_form, env)?));
                            package_symbols(env, &name, false)
                        }
                        LoopBeingSource::HashKeys(table_form) => {
                            let table = eval_form(*table_form, env)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(key, _)| key)
                                .collect()
                        }
                        LoopBeingSource::HashValues(table_form) => {
                            let table = eval_form(*table_form, env)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(_, value)| value)
                                .collect()
                        }
                    };
                    states.push(ForState::Being {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
            }
        }
        // repeat/while/until are terminating drivers too, so the runaway cap
        // (which only guards driverless loops) should not misfire on them.
        if repeat_remaining.is_some() || has_terminator {
            has_stepping_driver = true;
        }
        let mut first = true;
        let mut guard: u64 = 0;
        loop {
            if ret.is_some() {
                break;
            }
            // Step every driver in order; later clauses see earlier bindings.
            let mut exhausted = false;
            for st in &mut states {
                match st {
                    ForState::In { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], env);
                        *idx += 1;
                    }
                    ForState::On { pat, tail, step } => {
                        if !tail.is_cons() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, *tail, env);
                        *tail = match step {
                            Some(f) => apply_function(*f, &[*tail], env)?,
                            None => cp(*tail).1,
                        };
                    }
                    ForState::Eq { pat, init, then } => {
                        let f = if first { *init } else { then.unwrap_or(*init) };
                        let v = eval_form(f, env)?;
                        loop_bind(*pat, v, env);
                    }
                    ForState::From {
                        pat,
                        current,
                        step,
                        limit,
                    } => {
                        // The arithmetic iteration variable is visible to
                        // FINALLY with the value that failed the limit test.
                        // For `from 0 below 2`, that final value is 2.
                        loop_bind(*pat, *current, env);
                        if loop_from_exhausted(*current, limit.as_ref())? {
                            exhausted = true;
                            break;
                        }
                        *current = loop_add_numbers(*current, *step)?;
                    }
                    ForState::Across { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], env);
                        *idx += 1;
                    }
                    ForState::Being { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], env);
                        *idx += 1;
                    }
                }
            }
            if exhausted {
                break;
            }
            // `repeat N`: stop after N body executions.
            if let Some(rem) = repeat_remaining {
                if rem == 0 {
                    break;
                }
                repeat_remaining = Some(rem - 1);
            }
            // Run the body. `:while`/`:until` are body clauses evaluated in
            // lexical order (after driver stepping, so an interleaved
            // `for … while …` sees the current binding); a triggered one sets
            // `terminate` and ends the loop after the clauses before it have run.
            let mut terminate = false;
            loop_exec_clauses(&body, env, &mut accs, &mut ret, &mut terminate)?;
            if terminate {
                break;
            }
            first = false;
            if !has_stepping_driver {
                guard += 1;
                if guard > 10_000_000 {
                    return Err(BlissError::Internal(
                        "LOOP: exceeded iteration cap (no terminating driver)".into(),
                    ));
                }
            }
        }
    }

    // Publish named accumulators so :finally can read them.
    let named: Vec<(String, Vec<BlissVal>)> = accs
        .map
        .iter()
        .filter_map(|(k, v)| k.as_ref().map(|name| (name.clone(), v.clone())))
        .collect();
    for (name, items) in named {
        env.define_local(&name, vec_to_list(&items));
    }
    // Numeric named accumulators (sum/count/maximize/minimize INTO var).
    let named_nums: Vec<(String, BlissVal)> = accs
        .nums
        .iter()
        .filter_map(|(k, v)| k.as_ref().map(|name| (name.clone(), v.finalize())))
        .collect();
    for (name, val) in named_nums {
        env.define_local(&name, val);
    }

    // :finally — an embedded (return X) ends the loop with X.
    for f in &finally {
        if ret.is_some() {
            break;
        }
        if let Some(tgt) = loop_return_target(*f) {
            ret = Some(eval_form(tgt, env)?);
        } else {
            eval_form(*f, env)?;
        }
    }

    if let Some(r) = ret {
        return Ok(r);
    }
    // Result precedence for the anonymous accumulator: scalar (sum/count/max/min),
    // then boolean (always/never), then the collected list, else NIL.
    if let Some(n) = accs.nums.get(&None) {
        return Ok(n.finalize());
    }
    if let Some(b) = accs.bool_default {
        return Ok(b);
    }
    if let Some(items) = accs.map.get(&None) {
        return Ok(vec_to_list(items));
    }
    Ok(NIL)
}

fn loop_add_numbers(lhs: BlissVal, rhs: BlissVal) -> Result<BlissVal, BlissError> {
    if lhs.is_fixnum() && rhs.is_fixnum() {
        return Ok(BlissVal::from_fixnum(lhs.as_fixnum() + rhs.as_fixnum()));
    }
    let sum = num_val(lhs)? + num_val(rhs)?;
    Ok(BlissVal::from_single_float(sum as f32))
}

/// Negate a LOOP step value, preserving fixnum vs float (bliss-lb6): a
/// `:downfrom … :by n` decrements by `n`.
fn loop_negate_number(v: BlissVal) -> Result<BlissVal, BlissError> {
    if v.is_fixnum() {
        return Ok(BlissVal::from_fixnum(-v.as_fixnum()));
    }
    Ok(BlissVal::from_single_float(-(num_val(v)? as f32)))
}

fn loop_from_exhausted(
    current: BlissVal,
    limit: Option<&(LoopForLimit, BlissVal)>,
) -> Result<bool, BlissError> {
    let Some((kind, limit)) = limit else {
        return Ok(false);
    };
    let current = num_val(current)?;
    let limit = num_val(*limit)?;
    Ok(match kind {
        LoopForLimit::Below => current >= limit,
        LoopForLimit::To | LoopForLimit::Upto => current > limit,
        LoopForLimit::Above => current <= limit,
        LoopForLimit::Downto => current < limit,
    })
}

// ── Exact numeric tower: fixnum <-> bignum <-> ratio ──────────────
//
// Integer arithmetic promotes to arbitrary precision (BIGNUM, §1.8.1) on
// i64 overflow rather than silently wrapping. Exact rationals are carried as
// a pair of BigInts (denominator kept positive). Floats remain inexact and
// contaminate any operation they take part in, per CL contagion rules.

/// The 61-bit fixnum range (values outside it become BIGNUMs), matching the
/// reader's `fits_fixnum`.
const FIXNUM_MAX: i64 = (1 << 60) - 1;
const FIXNUM_MIN: i64 = -(1 << 60);

/// Allocate a `total_size`-byte object (header + body) of `type_id` on the shared
/// GC heap and return a pointer to the object header (bliss-jtc.5). The allocator
/// writes the header; the caller fills body fields at offsets ≥ 8.
fn gc_alloc_obj(total_size: usize, type_id: u8) -> *mut u8 {
    let hdr = std::mem::size_of::<ObjectHeader>();
    let body_size = total_size.saturating_sub(hdr).max(1);
    match bliss_rt::gc::alloc_typed(body_size, type_id) {
        // SAFETY: alloc_typed returns a pointer past an 8-byte header.
        Some(body) => unsafe { body.sub(hdr) },
        None => std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(total_size.max(hdr), hdr).unwrap(),
        ),
    }
}

/// Allocate a RATIO heap object (numerator/denominator are integers) on the
/// shared GC heap, using the spec RatioData layout (bliss-jtc.5).
pub(super) fn alloc_ratio_cli(num: BlissVal, den: BlissVal) -> BlissVal {
    let roots = bliss_rt::ShadowRootScope::new();
    let num = roots.root(num);
    let den = roots.root(den);
    let ptr = gc_alloc_obj(std::mem::size_of::<RatioData>(), type_id::RATIO) as *mut RatioData;
    unsafe {
        (*ptr).numerator = num.get();
        (*ptr).denominator = den.get();
        BlissVal::from_heap_ptr(ptr as *mut u8)
    }
}

/// Allocate a BIGNUM heap object (§1.8.1) on the shared GC heap: header + sign +
/// n_limbs + limbs. Layout mirrors the reader so `print_val` renders it correctly.
fn alloc_bignum_cli(sign: i32, limbs: &[u64]) -> BlissVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let ptr = gc_alloc_obj(total_size, type_id::BIGNUM);
    unsafe {
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

/// If `v` is a heap-allocated number, return its numeric `type_id`
/// (BIGNUM/RATIO/COMPLEX/DOUBLE_FLOAT). Fixnums and single-floats are immediates,
/// not heap objects, so they return None here (bliss-jtc.5).
fn heap_numeric_type_id(v: BlissVal) -> Option<u8> {
    if !v.is_heap_object() {
        return None;
    }
    // SAFETY: `v` is a heap object; reading its header type_id is the same
    // pattern the numeric paths already use (e.g. ratio_parts_val).
    let tid = unsafe { (*(v.as_ptr() as *const ObjectHeader)).type_id() };
    matches!(
        tid,
        type_id::BIGNUM | type_id::RATIO | type_id::COMPLEX | type_id::DOUBLE_FLOAT
    )
    .then_some(tid)
}

/// True if `v` is any kind of number: a fixnum, a single-float, or a heap
/// numeric object (bignum/ratio/complex/double-float) (bliss-jtc.5).
fn is_number_value(v: BlissVal) -> bool {
    v.is_fixnum() || v.is_single_float() || heap_numeric_type_id(v).is_some()
}

/// CL EQL: identical objects, or two numbers of the same type with the same
/// value. Fixnums and single-floats are immediates (handled by `==`); heap
/// numbers of the same type are compared by value on the shared representation.
fn eql_values(a: BlissVal, b: BlissVal) -> bool {
    if a == b {
        return true;
    }
    match (heap_numeric_type_id(a), heap_numeric_type_id(b)) {
        (Some(ta), Some(tb)) if ta == tb => numeric_cmp(a, b)
            .map(|o| o == Ordering::Equal)
            .unwrap_or(false),
        _ => false,
    }
}

/// Extract (numerator, denominator) BlissVals from a RATIO heap object.
pub(super) fn ratio_parts_val(v: BlissVal) -> Option<(BlissVal, BlissVal)> {
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let hdr = *(ptr as *const ObjectHeader);
        if hdr.type_id() != type_id::RATIO {
            return None;
        }
        let num = *(ptr.add(8) as *const BlissVal);
        let den = *(ptr.add(16) as *const BlissVal);
        Some((num, den))
    }
}

// ── Arbitrary-precision integers (little-endian base-2^64 magnitude) ──

fn mag_trim(mut m: Vec<u64>) -> Vec<u64> {
    while !m.is_empty() && *m.last().unwrap() == 0 {
        m.pop();
    }
    m
}

fn mag_cmp(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry: u128 = 0;
    for i in 0..a.len().max(b.len()) {
        let av = *a.get(i).unwrap_or(&0) as u128;
        let bv = *b.get(i).unwrap_or(&0) as u128;
        let s = av + bv + carry;
        out.push(s as u64);
        carry = s >> 64;
    }
    if carry != 0 {
        out.push(carry as u64);
    }
    mag_trim(out)
}

/// `a - b`, requiring `a >= b`.
fn mag_sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow: i128 = 0;
    for (i, &ai) in a.iter().enumerate() {
        let av = ai as i128;
        let bv = *b.get(i).unwrap_or(&0) as i128;
        let mut d = av - bv - borrow;
        if d < 0 {
            d += 1i128 << 64;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u64);
    }
    mag_trim(out)
}

fn mag_mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &av) in a.iter().enumerate() {
        let av = av as u128;
        let mut carry: u128 = 0;
        for (j, &bv) in b.iter().enumerate() {
            let cur = out[i + j] as u128 + av * bv as u128 + carry;
            out[i + j] = cur as u64;
            carry = cur >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = out[k] as u128 + carry;
            out[k] = cur as u64;
            carry = cur >> 64;
            k += 1;
        }
    }
    mag_trim(out)
}

fn mag_bit(m: &[u64], idx: usize) -> u64 {
    (m[idx / 64] >> (idx % 64)) & 1
}

fn mag_bitlen(m: &[u64]) -> usize {
    if m.is_empty() {
        return 0;
    }
    let top = m.len() - 1;
    64 * top + (64 - m[top].leading_zeros() as usize)
}

fn mag_shl1(m: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(m.len() + 1);
    let mut carry = 0u64;
    for &x in m {
        out.push((x << 1) | carry);
        carry = x >> 63;
    }
    if carry != 0 {
        out.push(carry);
    }
    mag_trim(out)
}

fn mag_set_bit(m: &mut Vec<u64>, idx: usize) {
    let w = idx / 64;
    while m.len() <= w {
        m.push(0);
    }
    m[w] |= 1u64 << (idx % 64);
}

/// Truncating division of magnitudes: returns (quotient, remainder). `b` must
/// be nonzero. Bit-by-bit long division — O(bits · limbs), fine at test sizes.
fn mag_divmod(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let n = mag_bitlen(a);
    let mut rem: Vec<u64> = Vec::new();
    let mut quot: Vec<u64> = Vec::new();
    for i in (0..n).rev() {
        rem = mag_shl1(&rem);
        if mag_bit(a, i) == 1 {
            if rem.is_empty() {
                rem.push(1);
            } else {
                rem[0] |= 1;
            }
        }
        if mag_cmp(&rem, b) != Ordering::Less {
            rem = mag_sub(&rem, b);
            mag_set_bit(&mut quot, i);
        }
    }
    (mag_trim(quot), mag_trim(rem))
}

#[derive(Clone, PartialEq, Eq)]
struct BigInt {
    sign: i8,      // -1, 0, or 1
    mag: Vec<u64>, // little-endian; no trailing zero limbs; empty iff sign == 0
}

impl BigInt {
    fn zero() -> BigInt {
        BigInt {
            sign: 0,
            mag: Vec::new(),
        }
    }

    fn one() -> BigInt {
        BigInt {
            sign: 1,
            mag: vec![1],
        }
    }

    fn from_i64(n: i64) -> BigInt {
        if n == 0 {
            return BigInt::zero();
        }
        BigInt {
            sign: if n < 0 { -1 } else { 1 },
            mag: vec![n.unsigned_abs()],
        }
    }

    fn from_mag(sign: i8, mag: Vec<u64>) -> BigInt {
        let mag = mag_trim(mag);
        if mag.is_empty() {
            BigInt::zero()
        } else {
            BigInt {
                sign: if sign < 0 { -1 } else { 1 },
                mag,
            }
        }
    }

    fn from_parts(sign: i32, limbs: &[u64]) -> BigInt {
        BigInt::from_mag(if sign < 0 { -1 } else { 1 }, limbs.to_vec())
    }

    fn is_zero(&self) -> bool {
        self.sign == 0
    }

    fn to_f64(&self) -> f64 {
        let mut f = 0.0f64;
        for &limb in self.mag.iter().rev() {
            f = f * 18446744073709551616.0 + limb as f64; // * 2^64
        }
        if self.sign < 0 { -f } else { f }
    }

    /// Canonicalize: a fixnum when it fits the 61-bit range, else a BIGNUM.
    fn to_val(&self) -> BlissVal {
        if self.is_zero() {
            return BlissVal::from_fixnum(0);
        }
        if self.mag.len() == 1 {
            let m = self.mag[0];
            if self.sign > 0 {
                if m <= FIXNUM_MAX as u64 {
                    return BlissVal::from_fixnum(m as i64);
                }
            } else if m <= FIXNUM_MIN.unsigned_abs() {
                return BlissVal::from_fixnum(-(m as i64));
            }
        }
        alloc_bignum_cli(self.sign as i32, &self.mag)
    }
}

fn big_neg(a: &BigInt) -> BigInt {
    BigInt {
        sign: -a.sign,
        mag: a.mag.clone(),
    }
}

fn big_add(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() {
        return b.clone();
    }
    if b.is_zero() {
        return a.clone();
    }
    if a.sign == b.sign {
        BigInt::from_mag(a.sign, mag_add(&a.mag, &b.mag))
    } else {
        match mag_cmp(&a.mag, &b.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::from_mag(a.sign, mag_sub(&a.mag, &b.mag)),
            Ordering::Less => BigInt::from_mag(b.sign, mag_sub(&b.mag, &a.mag)),
        }
    }
}

fn big_sub(a: &BigInt, b: &BigInt) -> BigInt {
    big_add(a, &big_neg(b))
}

fn big_mul(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() || b.is_zero() {
        return BigInt::zero();
    }
    BigInt::from_mag(a.sign * b.sign, mag_mul(&a.mag, &b.mag))
}

fn big_cmp(a: &BigInt, b: &BigInt) -> Ordering {
    if a.sign != b.sign {
        return a.sign.cmp(&b.sign);
    }
    match a.sign {
        0 => Ordering::Equal,
        1 => mag_cmp(&a.mag, &b.mag),
        _ => mag_cmp(&b.mag, &a.mag),
    }
}

/// Truncating (toward zero) division: (quotient, remainder). `b` nonzero.
fn big_divmod(a: &BigInt, b: &BigInt) -> (BigInt, BigInt) {
    let (q, r) = mag_divmod(&a.mag, &b.mag);
    (
        BigInt::from_mag(a.sign * b.sign, q),
        BigInt::from_mag(a.sign, r), // remainder takes the dividend's sign
    )
}

/// Exact division, assuming `g` divides `a` with no remainder.
fn big_divexact(a: &BigInt, g: &BigInt) -> BigInt {
    big_divmod(a, g).0
}

/// Non-negative gcd of two integers (gcd(0,0) == 0).
fn big_gcd(x: &BigInt, y: &BigInt) -> BigInt {
    let mut a = BigInt::from_mag(1, x.mag.clone());
    let mut b = BigInt::from_mag(1, y.mag.clone());
    while !b.is_zero() {
        let (_, r) = big_divmod(&a, &b);
        a = b;
        b = BigInt::from_mag(1, r.mag);
    }
    a
}

/// Read a fixnum or BIGNUM into a BigInt. `None` for any other value.
fn bigint_from_val(v: BlissVal) -> Option<BigInt> {
    if v.is_fixnum() {
        return Some(BigInt::from_i64(v.as_fixnum()));
    }
    if v.is_heap_object() {
        unsafe {
            let ptr = v.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::BIGNUM {
                let sign = *(ptr.add(8) as *const i32);
                let n = *(ptr.add(12) as *const u32) as usize;
                let mut limbs = Vec::with_capacity(n);
                for i in 0..n {
                    limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                }
                return Some(BigInt::from_parts(sign, &limbs));
            }
        }
    }
    None
}

/// An exact rational: denominator kept positive and reduced to lowest terms.
#[derive(Clone)]
struct BigRat {
    num: BigInt,
    den: BigInt,
}

impl BigRat {
    fn from_i64(n: i64) -> BigRat {
        BigRat {
            num: BigInt::from_i64(n),
            den: BigInt::one(),
        }
    }

    fn from_bigint(n: BigInt) -> BigRat {
        BigRat {
            num: n,
            den: BigInt::one(),
        }
    }

    /// Build a reduced rational; `den` must be nonzero.
    fn new(mut num: BigInt, mut den: BigInt) -> BigRat {
        if den.sign < 0 {
            num = big_neg(&num);
            den = big_neg(&den);
        }
        if num.is_zero() {
            return BigRat {
                num: BigInt::zero(),
                den: BigInt::one(),
            };
        }
        let g = big_gcd(&num, &den);
        if big_cmp(&g, &BigInt::one()) != Ordering::Equal {
            num = big_divexact(&num, &g);
            den = big_divexact(&den, &g);
        }
        BigRat { num, den }
    }

    fn is_integer(&self) -> bool {
        big_cmp(&self.den, &BigInt::one()) == Ordering::Equal
    }

    fn to_f64(&self) -> f64 {
        self.num.to_f64() / self.den.to_f64()
    }

    fn to_val(&self) -> BlissVal {
        if self.is_integer() {
            self.num.to_val()
        } else {
            alloc_ratio_cli(self.num.to_val(), self.den.to_val())
        }
    }
}

fn bigrat_neg(a: &BigRat) -> BigRat {
    BigRat {
        num: big_neg(&a.num),
        den: a.den.clone(),
    }
}

fn bigrat_add(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_add(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_sub(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_sub(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_mul(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.num), big_mul(&a.den, &b.den))
}

/// `a / b`; `b` must be nonzero (the caller checks for zero divisors).
fn bigrat_div(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.den), big_mul(&a.den, &b.num))
}

fn bigrat_cmp(a: &BigRat, b: &BigRat) -> Ordering {
    // Denominators are positive, so comparing cross-products is order-preserving.
    big_cmp(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den))
}

/// Raise an exact rational to a non-negative integer power.
fn bigrat_pow(base: &BigRat, mut e: u64) -> BigRat {
    let mut result = BigRat::from_i64(1);
    let mut b = base.clone();
    while e > 0 {
        if e & 1 == 1 {
            result = bigrat_mul(&result, &b);
        }
        e >>= 1;
        if e > 0 {
            b = bigrat_mul(&b, &b);
        }
    }
    result
}

/// A fixnum, BIGNUM, or RATIO as an exact rational; `None` otherwise.
fn as_bigrat(v: BlissVal) -> Option<BigRat> {
    if let Some(n) = bigint_from_val(v) {
        return Some(BigRat::from_bigint(n));
    }
    if let Some((nv, dv)) = ratio_parts_val(v) {
        let n = bigint_from_val(nv)?;
        let d = bigint_from_val(dv)?;
        if d.is_zero() {
            return None;
        }
        return Some(BigRat::new(n, d));
    }
    None
}

fn num_val(v: BlissVal) -> Result<f64, BlissError> {
    if v.is_single_float() {
        Ok(v.as_single_float() as f64)
    } else if let Some(r) = as_bigrat(v) {
        Ok(r.to_f64())
    } else {
        Err(BlissError::TypeError {
            datum: v,
            expected: "number".into(),
        })
    }
}

/// Exact-where-possible numeric comparison. Floats force inexact comparison
/// (CL contagion); otherwise operands compare as exact rationals.
fn numeric_cmp(a: BlissVal, b: BlissVal) -> Result<Ordering, BlissError> {
    // Fixnum fast path (bliss-x5y.10): the overwhelmingly common case is two
    // fixnums; compare their untagged i64s directly instead of allocating two
    // heap BigRats. This removes the per-comparison bignum allocation that
    // dominated arithmetic-heavy code in profiles.
    if a.is_fixnum() && b.is_fixnum() {
        return Ok(a.as_fixnum().cmp(&b.as_fixnum()));
    }
    if a.is_single_float() || b.is_single_float() {
        let av = num_val(a)?;
        let bv = num_val(b)?;
        return Ok(av.partial_cmp(&bv).unwrap_or(Ordering::Equal));
    }
    match (as_bigrat(a), as_bigrat(b)) {
        (Some(ra), Some(rb)) => Ok(bigrat_cmp(&ra, &rb)),
        (None, _) => Err(BlissError::TypeError {
            datum: a,
            expected: "number".into(),
        }),
        (_, None) => Err(BlissError::TypeError {
            datum: b,
            expected: "number".into(),
        }),
    }
}

fn eval_arith(
    args: BlissVal,
    env: &mut Env,
    init_i: i64,
    init_f: f64,
    op_f: fn(f64, f64) -> f64,
    op_r: fn(&BigRat, &BigRat) -> BigRat,
    op_i: fn(i128, i128) -> i128,
) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let remaining = roots.root(args);
    let mut rooted_vals = Vec::new();
    while remaining.get().is_cons() {
        let (af, rest) = cp(remaining.get());
        let rest = roots.root(rest);
        rooted_vals.push(roots.root(eval_form(af, env)?));
        remaining.set(rest.get());
    }
    let vals: Vec<_> = rooted_vals.iter().map(|value| value.get()).collect();
    fold_arith_vals(&vals, init_i, init_f, op_f, op_r, op_i)
}

/// The `+`/`*` fold over already-evaluated operands (int/rational with float
/// contagion). Shared by operator-position [`eval_arith`] and the direct
/// builtin dispatch in [`apply_function`], so both produce bit-identical
/// results — including the fixnum-overflow→bignum promotion (`op_r` on
/// `BigRat`) that the float-based `apply_builtin` got wrong (bliss-x5y.9).
fn fold_arith_vals(
    vals: &[BlissVal],
    init_i: i64,
    init_f: f64,
    op_f: fn(f64, f64) -> f64,
    op_r: fn(&BigRat, &BigRat) -> BigRat,
    op_i: fn(i128, i128) -> i128,
) -> Result<BlissVal, BlissError> {
    // Fixnum fast path (bliss-x5y.10): fold in i128 while every operand is a
    // fixnum and the running result stays in fixnum range, allocating no
    // BigRat. i128 cannot overflow here — one op on fixnum-range values is at
    // most ~2^120. A non-fixnum operand or an out-of-range result switches to
    // the exact int/rational/float fold for the remaining operands, seeded from
    // the fixnum accumulator.
    let mut acc_i = init_i as i128;
    let mut idx = 0;
    while idx < vals.len() {
        let v = vals[idx];
        if !v.is_fixnum() {
            break;
        }
        let r = op_i(acc_i, v.as_fixnum() as i128);
        if !(FIXNUM_MIN as i128..=FIXNUM_MAX as i128).contains(&r) {
            break;
        }
        acc_i = r;
        idx += 1;
    }
    if idx == vals.len() {
        return Ok(BlissVal::from_fixnum(acc_i as i64));
    }
    let mut acc = BigRat::from_i64(acc_i as i64);
    let mut acc_f = init_f;
    let mut is_float = false;
    for &v in &vals[idx..] {
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f = op_f(acc_f, v.as_single_float() as f64);
        } else if let Some(rv) = as_bigrat(v) {
            if is_float {
                acc_f = op_f(acc_f, rv.to_f64());
            } else {
                acc = op_r(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: v,
                expected: "number".into(),
            });
        }
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_sub(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let remaining = roots.root(args);
    let mut rooted_vals = Vec::new();
    while remaining.get().is_cons() {
        let (af, rest) = cp(remaining.get());
        let rest = roots.root(rest);
        rooted_vals.push(roots.root(eval_form(af, env)?));
        remaining.set(rest.get());
    }
    let vals: Vec<_> = rooted_vals.iter().map(|value| value.get()).collect();
    sub_vals(&vals)
}

/// The `-` fold (negate for one arg, left-fold subtraction otherwise) over
/// already-evaluated operands. Shared by [`eval_arith_sub`] and the direct
/// builtin dispatch so both tiers agree (bliss-x5y.9).
fn sub_vals(vals: &[BlissVal]) -> Result<BlissVal, BlissError> {
    if vals.is_empty() {
        return Ok(BlissVal::from_fixnum(0));
    }
    // Fixnum fast path (bliss-x5y.10): all-fixnum subtraction (or 1-arg negate)
    // in i128 with a range check — no BigRat allocation. i128 cannot overflow
    // for any realistic operand count (each |operand| <= 2^60). If the result
    // leaves fixnum range (e.g. negating FIXNUM_MIN), fall through to the exact
    // bignum path below.
    if vals.iter().all(|v| v.is_fixnum()) {
        let mut acc = vals[0].as_fixnum() as i128;
        if vals.len() == 1 {
            acc = -acc;
        } else {
            for v in &vals[1..] {
                acc -= v.as_fixnum() as i128;
            }
        }
        if (FIXNUM_MIN as i128..=FIXNUM_MAX as i128).contains(&acc) {
            return Ok(BlissVal::from_fixnum(acc as i64));
        }
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            return Ok(BlissVal::from_single_float(-vals[0].as_single_float()));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            return Ok(bigrat_neg(&r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f -= v.as_single_float() as f64;
        } else if let Some(rv) = as_bigrat(*v) {
            if is_float {
                acc_f -= rv.to_f64();
            } else {
                acc = bigrat_sub(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_div(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let remaining = roots.root(args);
    let mut rooted_vals = Vec::new();
    while remaining.get().is_cons() {
        let (af, rest) = cp(remaining.get());
        let rest = roots.root(rest);
        rooted_vals.push(roots.root(eval_form(af, env)?));
        remaining.set(rest.get());
    }
    let vals: Vec<_> = rooted_vals.iter().map(|value| value.get()).collect();
    if vals.is_empty() {
        return Err(BlissError::ArithmeticError(
            "/ requires at least one argument".into(),
        ));
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            let v = vals[0].as_single_float();
            if v == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            return Ok(BlissVal::from_single_float(1.0 / v));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            if r.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            // reciprocal: den/num
            return Ok(bigrat_div(&BigRat::from_i64(1), &r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            let dv = v.as_single_float() as f64;
            if dv == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f /= dv;
        } else if let Some(rv) = as_bigrat(*v) {
            if rv.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if is_float {
                acc_f /= rv.to_f64();
            } else {
                acc = bigrat_div(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    if is_float {
        Ok(BlissVal::from_single_float(acc_f as f32))
    } else {
        Ok(acc.to_val())
    }
}

fn eval_cmp(
    args: BlissVal,
    env: &mut Env,
    pred: fn(Ordering) -> bool,
) -> Result<BlissVal, BlissError> {
    let roots = bliss_rt::ShadowRootScope::new();
    let (af, r) = cp(args);
    // Root the second arg form before evaluating the first (bliss-6b2 #2).
    let bf = roots.root(cp(r).0);
    let a = roots.root(eval_form(af, env)?);
    let b = eval_form(bf.get(), env)?;
    Ok(if pred(numeric_cmp(a.get(), b)?) {
        T
    } else {
        NIL
    })
}

/// Evaluate each form in `forms`, keeping the results GC-rooted across the whole
/// evaluation, then return them as a fresh `Vec`. The intermediate results live
/// in a `Vec<BlissVal>` whose backing buffer is on the Rust heap — invisible to
/// the collector — so without rooting, evaluating a later form could trigger a
/// relocating minor GC that leaves the earlier results dangling (bliss-6b2 #2).
/// Callers hold the returned (bare) `Vec` only briefly; native-stack roots are
/// covered conservatively by the collector.
fn eval_forms(
    forms: impl IntoIterator<Item = BlissVal>,
    env: &mut Env,
) -> Result<RootedVals, BlissError> {
    // Root the input forms first (they may be young source conses reachable only
    // through a caller's Rust local), then evaluate into a self-rooted result
    // that stays rooted for the caller (bliss-6b2 #2).
    let roots = bliss_rt::ShadowRootScope::new();
    let forms: Vec<bliss_rt::ShadowRoot> = forms.into_iter().map(|f| roots.root(f)).collect();
    let mut result = RootedVals::new(Vec::new());
    for f in &forms {
        let v = eval_form(f.get(), env)?;
        result.push(v);
    }
    Ok(result)
}

fn eval_args(args: BlissVal, env: &mut Env) -> Result<RootedVals, BlissError> {
    // Evaluate a cons list of argument forms into a `RootedVals` — kept rooted for
    // the whole time the CALLER holds it (across the ensuing apply/dispatch, whose
    // callee allocates), not just during evaluation (bliss-6b2 #2). This is the
    // shared argument evaluator for many builtins.
    let roots = bliss_rt::ShadowRootScope::new();
    let cursor = roots.root(args);
    let mut result = RootedVals::new(Vec::new());
    while cursor.get().is_cons() {
        let (af, r) = cp(cursor.get());
        let rest = roots.root(r);
        let v = eval_form(af, env)?;
        result.push(v);
        cursor.set(rest.get());
    }
    Ok(result)
}

// ── COERCE ───────────────────────────────────────────────────────
/// Extract a sequence (list, vector, or string) into a Vec of its elements.
fn seq_elements(seq: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    if seq.is_nil() {
        return Ok(Vec::new());
    }
    if seq.is_cons() {
        return Ok(list_to_vec(seq));
    }
    let n = bliss_stdlib::length(seq)?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(bliss_stdlib::elt(seq, i)?);
    }
    Ok(out)
}

/// CL COERCE for the type specifiers that actually occur in practice. The
/// result-type is reduced to the head symbol of the spec (e.g. `(vector t)` →
/// VECTOR). Unknown specifiers pass the value through unchanged.
fn coerce_value(value: BlissVal, type_val: BlissVal) -> Result<BlissVal, BlissError> {
    // Reduce the type spec to a bare head-symbol name.
    let head = if type_val.is_cons() {
        cp(type_val).0
    } else {
        type_val
    };
    if head.is_nil() {
        return Ok(value);
    }
    let tname = symbol_bare_name(&sym_name(head));
    match tname.as_str() {
        "T" => Ok(value),
        "LIST" => {
            if value.is_cons() || value.is_nil() {
                Ok(value)
            } else {
                Ok(vec_to_list(&seq_elements(value)?))
            }
        }
        "VECTOR" | "SIMPLE-VECTOR" | "ARRAY" | "SIMPLE-ARRAY" => {
            Ok(bliss_stdlib::build_simple_vector(&seq_elements(value)?))
        }
        "STRING" | "SIMPLE-STRING" | "BASE-STRING" | "SIMPLE-BASE-STRING" => {
            if bliss_stdlib::registered_string(value).is_some() {
                return Ok(value);
            }
            let mut s = String::new();
            for e in seq_elements(value)? {
                if e.is_character() {
                    s.push(e.as_char());
                }
            }
            Ok(arena_str(&s))
        }
        "CHARACTER" => {
            if value.is_character() {
                return Ok(value);
            }
            let s = val_as_str(value);
            match s.chars().next() {
                Some(c) => Ok(BlissVal::from_char(c)),
                None => Err(BlissError::TypeError {
                    datum: value,
                    expected: "character".into(),
                }),
            }
        }
        "FLOAT" | "SINGLE-FLOAT" | "DOUBLE-FLOAT" | "SHORT-FLOAT" | "LONG-FLOAT" => {
            Ok(BlissVal::from_single_float(num_val(value)? as f32))
        }
        // FUNCTION: bliss symbols and closures are already callable via funcall.
        "FUNCTION" => Ok(value),
        // Unknown / identity specifiers: pass through unchanged.
        _ => Ok(value),
    }
}

// ── SORT / STABLE-SORT ───────────────────────────────────────────
// Sort by actually applying the predicate (and optional key) — the stdlib
// helper only guessed a direction from the predicate symbol and compared raw
// bits, so `#'string<` and other real predicates produced wrong orders. This
// runs in the evaluator, where functions can be called. Both SORT and
// STABLE-SORT use this stable merge sort.
fn sort_less(
    predicate: BlissVal,
    key: Option<BlissVal>,
    a: BlissVal,
    b: BlissVal,
    env: &mut Env,
) -> Result<bool, BlissError> {
    let ka = match key {
        Some(k) if !k.is_nil() => apply_function(k, &[a], env)?,
        _ => a,
    };
    let kb = match key {
        Some(k) if !k.is_nil() => apply_function(k, &[b], env)?,
        _ => b,
    };
    Ok(!apply_function(predicate, &[ka, kb], env)?.is_nil())
}

fn merge_sort_pred(
    elems: &mut [BlissVal],
    predicate: BlissVal,
    key: Option<BlissVal>,
    env: &mut Env,
) -> Result<(), BlissError> {
    let n = elems.len();
    if n <= 1 {
        return Ok(());
    }
    let mid = n / 2;
    let mut left = elems[..mid].to_vec();
    let mut right = elems[mid..].to_vec();
    // Root the split copies across the predicate/key calls (bliss-6b2 #2): each
    // comparison runs user code that can relocate these Vec-resident elements.
    let _lg = VecRootGuard::new(&mut left);
    let _rg = VecRootGuard::new(&mut right);
    merge_sort_pred(&mut left, predicate, key, env)?;
    merge_sort_pred(&mut right, predicate, key, env)?;
    let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
    while i < left.len() && j < right.len() {
        // Stable: keep left before right unless right is strictly less.
        if sort_less(predicate, key, right[j], left[i], env)? {
            elems[k] = right[j];
            j += 1;
        } else {
            elems[k] = left[i];
            i += 1;
        }
        k += 1;
    }
    while i < left.len() {
        elems[k] = left[i];
        i += 1;
        k += 1;
    }
    while j < right.len() {
        elems[k] = right[j];
        j += 1;
        k += 1;
    }
    Ok(())
}

fn sort_sequence(
    seq: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    if seq.is_nil() {
        return Ok(NIL);
    }
    let is_list = seq.is_cons();
    let mut elems: Vec<BlissVal> = if is_list {
        list_to_vec(seq)
    } else {
        let n = bliss_stdlib::length(seq)?;
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            v.push(bliss_stdlib::elt(seq, i)?);
        }
        v
    };
    let _eg = VecRootGuard::new(&mut elems);
    merge_sort_pred(&mut elems, predicate, key, env)?;
    if is_list {
        Ok(vec_to_list(&elems))
    } else {
        for (i, e) in elems.iter().enumerate() {
            bliss_stdlib::set_elt(seq, i, *e)?;
        }
        Ok(seq)
    }
}

// ── LET / LET* binding ───────────────────────────────────────────
// Bind lexically by pushing a fresh frame onto the SAME env (via
// `with_child_frame`), never by forking a whole `Env` with `env.child()`.
// Forking would copy the Rc-shared global tables (packages, funs, macros,
// classes, methods, …) and any mutation inside the body — e.g. a DEFPACKAGE,
// DEFUN, or DEFMETHOD nested in a LET — would copy-on-write into the discarded
// child and never reach the caller. Keeping one env also lets multiple values
// and dynamic state flow out of the body naturally.
/// True if `sym` names a special (dynamically-scoped) variable. bliss follows
/// the universal earmuff convention — a name spelled `*…*` is special — which
/// covers the standard special variables (`*standard-output*`, `*package*`, …)
/// and library specials like ASDF's `*asdf-session*`. A `let` on such a name
/// must establish a DYNAMIC binding (visible to called functions), not a lexical
/// one (bliss-lb6.14: ASDF's session cache is a `let`-bound special read by
/// helper functions).
fn is_special_var(sym: BlissVal) -> bool {
    if !sym.is_symbol() {
        return false;
    }
    let bare = symbol_bare_name(&sym_name(sym));
    let b = bare.as_bytes();
    b.len() > 2 && b[0] == b'*' && b[b.len() - 1] == b'*'
}

/// RAII guard for a dynamic (special-variable) binding: it saves the symbol's
/// current global value cell and restores it on drop, so the binding is undone
/// on every exit path from the `let` — normal return or an error unwinding
/// through `?`.
struct DynBind {
    idx: u32,
    saved: BlissVal,
}

impl DynBind {
    /// Establish a dynamic binding of `sym` to `val`, returning the guard.
    fn establish(sym: BlissVal, val: BlissVal) -> Self {
        let idx = sym.as_symbol_index();
        let saved = bliss_rt::symbols::symbol_value(idx).unwrap_or(bliss_rt::value::UNBOUND);
        bliss_rt::symbols::set_symbol_value(idx, val);
        DynBind { idx, saved }
    }
}

impl Drop for DynBind {
    fn drop(&mut self) {
        bliss_rt::symbols::set_symbol_value(self.idx, self.saved);
    }
}

fn eval_let(cdr: BlissVal, env: &mut Env, sequential: bool) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let parent = Rc::clone(&env.frame);

    if sequential {
        // let*: one child frame; each init sees the bindings established before it.
        return with_child_frame(env, parent, move |env| {
            // Special bindings are dynamic (global cell); the guards restore on
            // scope exit. Held for the whole body so later inits see earlier
            // special bindings, matching lexical ones.
            let mut dyn_binds: Vec<DynBind> = Vec::new();
            let mut c = bindings_form;
            while c.is_cons() {
                let (binding, rest) = cp(c);
                if binding.is_cons() {
                    let (var_form, val_rest) = cp(binding);
                    let (val_form, _) = cp(val_rest);
                    let val = eval_form(val_form, env)?;
                    if is_special_var(var_form) {
                        dyn_binds.push(DynBind::establish(var_form, val));
                    } else if var_form.is_symbol() {
                        env.define_local_symbol(var_form, val);
                    } else {
                        env.define_local(&sym_name(var_form), val);
                    }
                } else if binding.is_symbol() {
                    if is_special_var(binding) {
                        dyn_binds.push(DynBind::establish(binding, NIL));
                    } else {
                        env.define_local_symbol(binding, NIL);
                    }
                }
                c = rest;
            }
            eval_progn(body, env)
        });
    }

    // let: every init is evaluated in the outer frame before any binding is
    // visible. The evaluated values must be GC-rooted across the loop: a later
    // init allocates and can trigger a relocating minor GC that would leave the
    // earlier (bare `Vec`-resident) values dangling before they are bound into
    // the — scanned — child frame (bliss-6b2 #2).
    let roots = bliss_rt::ShadowRootScope::new();
    let mut evaluated: Vec<(BlissVal, bliss_rt::ShadowRoot)> = Vec::new();
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        if binding.is_cons() {
            let (var_form, val_rest) = cp(binding);
            let (val_form, _) = cp(val_rest);
            evaluated.push((var_form, roots.root(eval_form(val_form, env)?)));
        } else if binding.is_symbol() {
            evaluated.push((binding, roots.root(NIL)));
        }
        c = rest;
    }

    with_child_frame(env, parent, move |env| {
        // Special bindings are dynamic (global cell) with RAII restore; the rest
        // are lexical frame bindings.
        let mut dyn_binds: Vec<DynBind> = Vec::new();
        for (symbol, val) in &evaluated {
            let val = val.get();
            if is_special_var(*symbol) {
                dyn_binds.push(DynBind::establish(*symbol, val));
            } else if symbol.is_symbol() {
                env.define_local_symbol(*symbol, val);
            } else {
                env.define_local(&sym_name(*symbol), val);
            }
        }
        eval_progn(body, env)
    })
}

// ── DEFUN ────────────────────────────────────────────────────────
fn eval_defun(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    // A defun body is wrapped in an implicit block named after the function
    // (ANSI 3.1.2.1), so `(return-from NAME ...)` works from anywhere in the
    // body — including inside nested flet/loop/etypecase forms. For `(setf x)`
    // the block is named `x`.
    let block_name = if name_form.is_symbol() {
        Some(name_form)
    } else if name_form.is_cons() {
        let (_head, tail) = cp(name_form);
        if tail.is_cons() {
            Some(cp(tail).0)
        } else {
            None
        }
    } else {
        None
    };
    let body = match (block_name, resolve_sym("BLOCK")) {
        (Some(bn), Some(block_sym)) => {
            let block_form = arena_cons(block_sym, arena_cons(bn, body));
            arena_cons(block_form, NIL)
        }
        _ => body,
    };
    // Macroexpand the body once, now, so the tree-walker never re-expands (and
    // re-gensyms) it on every call (bliss-lb6.11). This is a best-effort,
    // conservative pass: anything it leaves unexpanded is still handled by the
    // lazy expansion in `eval_list`, so it can only ever under-expand.
    let body = mx_each(body, env, 0);
    if name_form.is_symbol() {
        // Ordinary global function → the symbol's heap function cell
        // (bliss-jtc.6.8). Redefinition updates the existing function object in
        // place so its identity (and any attached tiering state) is stable.
        let idx = name_form.as_symbol_index();
        match bliss_rt::symbols::symbol_function(idx) {
            Some(existing) if bliss_rt::function::is_interpreted_function(existing) => {
                // SAFETY: `existing` is an interpreted-function object.
                unsafe { bliss_rt::function::redefine(existing, params_form, body, NIL) };
            }
            _ => {
                let f = bliss_rt::function::alloc_interpreted(params_form, body, NIL, name_form);
                bliss_rt::symbols::set_symbol_function(idx, f);
            }
        }
    } else {
        // Non-symbol names, e.g. `(setf foo)`: store GLOBALLY under the canonical
        // "(SETF FOO)" key so SETF finds the writer function from any Env — in
        // particular from a different file loaded later (bliss-d0b). A per-Env
        // `funs` entry was lost across the throwaway compile/load Envs.
        let name = function_name_key(name_form);
        let params = extract_params(params_form);
        GLOBAL_SETF_FNS.with(|m| {
            m.borrow_mut().insert(
                name,
                FunDef {
                    params,
                    params_form,
                    body,
                },
            )
        });
    }
    Ok(name_form)
}

// ── FLET / LABELS local function binding ────────────────────────
// (flet ((name (lambda-list) body...) ...) body...)
// (labels ...) has the same shape but the local functions are mutually
// recursive. Both bind the named functions in a fresh child environment and
// evaluate the body there; because named functions in this evaluator resolve
// their names through the current environment's function table at call time,
// LABELS-style mutual recursion works naturally, and FLET-bound functions are
// visible only within the FLET/LABELS body.
/// If `name` names a LOCAL `flet`/`labels` function (present in `env.funs`),
/// build a closure over the current lexical environment and return it, so that
/// `#'localfn` / `(function localfn)` / `(fdefinition 'localfn)` yields a value
/// that is still callable after the `flet` scope exits — e.g. passed to MAPCAR
/// or stored. Returning the bare symbol (as the code used to) lost the local
/// binding, so a later `funcall` resolved the name globally and failed with
/// "undefined function" (notably `#'<gensym>` from library macros — babel's
/// encoders). Returns `None` for non-local names.
fn local_fn_closure(env: &mut Env, name: &str) -> Option<BlissVal> {
    let (params_form, body) = env
        .funs
        .borrow()
        .get(name)
        .map(|f| (f.params_form, f.body))?;
    let closure = Closure {
        params_form,
        body,
        captured_frame: Rc::clone(&env.frame),
    };
    let id = next_closure_id();
    env.closures.borrow_mut().insert(id, closure);
    let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
    Some(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)))
}

fn eval_flet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (defs_form, body) = cp(cdr);
    let mut child_env = env.child();
    let mut c = defs_form;
    while c.is_cons() {
        let (def, rest) = cp(c);
        if def.is_cons() {
            let (name_form, def_rest) = cp(def);
            let (params_form, fbody) = cp(def_rest);
            let name = sym_name(name_form);
            let params = extract_params(params_form);
            // A local function's body is wrapped in an implicit block named after
            // the function (ANSI 3.1.2.1 / 6.1 flet-labels), so `(return-from
            // NAME ...)` inside it works. For a `(setf foo)` name the block is
            // named `foo`.
            let block_name = if name_form.is_symbol() {
                Some(name_form)
            } else if name_form.is_cons() {
                let (_head, tail) = cp(name_form);
                if tail.is_cons() {
                    Some(cp(tail).0)
                } else {
                    None
                }
            } else {
                None
            };
            let fbody = match (block_name, resolve_sym("BLOCK")) {
                (Some(bn), Some(block_sym)) => {
                    let block_form = arena_cons(block_sym, arena_cons(bn, fbody));
                    arena_cons(block_form, NIL)
                }
                _ => fbody,
            };
            child_env.funs_mut().insert(
                name,
                FunDef {
                    params,
                    params_form,
                    body: fbody,
                },
            );
        }
        c = rest;
    }
    // Register the forked child Env as a GC root for the extent of the body:
    // its `funs` map holds the local functions' body cons trees, which are
    // otherwise unreachable from any scanned root and would be freed by a
    // relocating minor GC mid-body (bliss-6b2 #2). `env.child()` forks a whole
    // Env struct, so — unlike `with_child_frame`, which mutates the already-
    // registered env in place — the child needs its own guard.
    let _child_root = LiveEnvRootGuard::new(&mut child_env);
    let __r = eval_progn(body, &mut child_env);
    // Multiple values produced in the child body must propagate to the caller;
    // env.child() forks the value registers (bliss-lb6.22).
    env.mv = std::mem::take(&mut child_env.mv);
    env.mv_active = child_env.mv_active;
    __r
}

// ── Extract parameter names from a lambda list ──────────────────
fn extract_params(params_form: BlissVal) -> Vec<String> {
    let mut params = Vec::new();
    let mut c = params_form;
    while c.is_cons() {
        let (p, rest) = cp(c);
        if p.is_symbol() {
            let pname = sym_name(p);
            // Skip lambda-list keywords
            if !pname.starts_with('&') {
                params.push(pname);
            }
        }
        c = rest;
    }
    params
}

// ── Ordinary lambda-list binding ─────────────────────────────────
// Binds required, &optional (with defaults + supplied-p), &rest/&body,
// &key (with defaults, supplied-p, and ((:kw var) ...) form),
// &allow-other-keys (accepted, not enforced), and &aux. Defaults are
// evaluated left-to-right in `env` so they can see earlier parameters.

/// Bare (KEYWORD:-stripped) name of a symbol.
fn key_bare(sym: BlissVal) -> String {
    let n = sym_name(sym);
    n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string()
}

/// Parse an &optional/&aux element: `var` | `(var [default [supplied-p]])`.
fn parse_var_spec(elem: BlissVal) -> (String, BlissVal, Option<String>) {
    if elem.is_symbol() {
        return (sym_name(elem), NIL, None);
    }
    if elem.is_cons() {
        let (var, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        return (sym_name(var), default, supp);
    }
    (String::new(), NIL, None)
}

/// Parse a &key element: `var` | `(var [default [supp]])` | `((:kw var) [default [supp]])`.
/// Returns (keyword-bare-name, var-name, default-form, supplied-p-var).
fn parse_key_spec(elem: BlissVal) -> (String, String, BlissVal, Option<String>) {
    // For a `&key var` (or `(var …)`) spec the keyword name is `var`'s *bare*
    // name (ANSI: the indicator is `(intern (symbol-name var) :keyword)`),
    // independent of `var`'s home package — so a param whose symbol resolves
    // package-qualified (e.g. an exported symbol read inside its own package,
    // bliss-lb6.12) still matches a bare `:var` keyword at the call site. The
    // bound variable name stays the full symbol name so the body's references
    // (which resolve to the same symbol) find the binding.
    if elem.is_symbol() {
        let var = sym_name(elem);
        return (symbol_bare_name(&var), var, NIL, None);
    }
    if elem.is_cons() {
        let (head, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        if head.is_symbol() {
            let var = sym_name(head);
            return (symbol_bare_name(&var), var, default, supp);
        }
        if head.is_cons() {
            let (kw_sym, r3) = cp(head);
            let var = if r3.is_cons() {
                sym_name(cp(r3).0)
            } else {
                String::new()
            };
            return (key_bare(kw_sym), var, default, supp);
        }
    }
    (String::new(), String::new(), NIL, None)
}

/// Look up a keyword's value in a `key value key value ...` argument tail.
fn find_key_arg(plist: &[BlissVal], kw_bare: &str) -> Option<BlissVal> {
    let mut i = 0;
    while i + 1 < plist.len() {
        if plist[i].is_symbol() && key_bare(plist[i]) == kw_bare {
            return Some(plist[i + 1]);
        }
        i += 2;
    }
    None
}

fn bind_lambda_list(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<(), BlissError> {
    bind_lambda_list_ex(params_form, args, env, false)
}

/// Bind a lambda list to `args`. `force_allow_other_keys` makes an unrecognised
/// keyword be ignored rather than a PROGRAM-ERROR: used when binding a CLOS
/// method, because per CLHS 7.6.5 the *generic function* accepts the union of
/// all applicable methods' keyword parameters, so an individual method must not
/// reject a keyword another applicable method declares (bliss-lb6.14: ASDF's
/// OPERATE :around declares :verbose, the primary method does not).
fn bind_lambda_list_ex(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
    force_allow_other_keys: bool,
) -> Result<(), BlissError> {
    #[derive(PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }
    let mut mode = Mode::Req;
    let mut arg_i = 0usize;
    let mut key_start: Option<usize> = None;
    let mut rest_bound = false;
    let mut saw_key = false;
    let mut allow_other_keys = force_allow_other_keys;
    // Root the source spine and the args snapshot across &optional/&key/&aux
    // default evaluation (bliss-6b2 #2): each eval_form(default) can relocate
    // both the params cursor and the remaining unread args. The deferred &key
    // default forms are held in a separately-rooted Vec so they survive from
    // collection to the post-loop keyword pass.
    let roots = bliss_rt::ShadowRootScope::new();
    let mut args_owned: Vec<BlissVal> = args.to_vec();
    let _args_guard = VecRootGuard::new(&mut args_owned);
    let args: &[BlissVal] = &args_owned;
    let mut key_default_forms: Vec<BlissVal> = Vec::new();
    let _key_defaults_guard = VecRootGuard::new(&mut key_default_forms);
    let mut key_specs: Vec<(String, String, usize, Option<String>)> = Vec::new();

    let c = roots.root(params_form);
    while c.get().is_cons() {
        let (elem, rest) = cp(c.get());
        c.set(rest);
        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    saw_key = true;
                    key_start.get_or_insert(arg_i);
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => {
                    allow_other_keys = true;
                    continue;
                }
                _ => {}
            }
        }
        match mode {
            Mode::Req => {
                let v = args.get(arg_i).copied().ok_or_else(|| {
                    BlissError::ProgramError(format!(
                        "too few arguments for lambda list: missing value for {}",
                        sym_name(elem)
                    ))
                })?;
                arg_i += 1;
                env.define_local_symbol(elem, v);
            }
            Mode::Opt => {
                let (var, default_form, supp) = parse_var_spec(elem);
                if arg_i < args.len() {
                    env.define_local(&var, args[arg_i]);
                    arg_i += 1;
                    if let Some(sp) = supp {
                        env.define_local(&sp, T);
                    }
                } else {
                    let dv = if default_form == NIL {
                        NIL
                    } else {
                        eval_form(default_form, env)?
                    };
                    env.define_local(&var, dv);
                    if let Some(sp) = supp {
                        env.define_local(&sp, NIL);
                    }
                }
            }
            Mode::Rest => {
                if rest_bound {
                    return Err(BlissError::Internal(
                        "malformed lambda list: multiple &rest/&body variables".into(),
                    ));
                }
                let remaining = args.get(arg_i..).unwrap_or(&[]);
                env.define_local(&sym_name(elem), vec_to_list(remaining));
                key_start.get_or_insert(arg_i);
                rest_bound = true;
            }
            Mode::Key => {
                let (kw_bare, var, default_form, supp) = parse_key_spec(elem);
                let idx = key_default_forms.len();
                key_default_forms.push(default_form);
                key_specs.push((kw_bare, var, idx, supp));
            }
            Mode::Aux => {
                let (var, default_form, _) = parse_var_spec(elem);
                let dv = if default_form == NIL {
                    NIL
                } else {
                    eval_form(default_form, env)?
                };
                env.define_local(&var, dv);
            }
        }
    }

    if saw_key {
        let start = key_start.unwrap_or(arg_i);
        let tail = args.get(start..).unwrap_or(&[]);
        if tail.len() % 2 != 0 {
            return Err(BlissError::Internal(
                "keyword arguments must appear in key/value pairs".into(),
            ));
        }

        let mut call_allows_other_keys = false;
        for pair in tail.chunks(2) {
            let key = pair[0];
            if !key.is_symbol() {
                return Err(BlissError::TypeError {
                    datum: key,
                    expected: "keyword".into(),
                });
            }
            let bare = key_bare(key);
            if bare == "ALLOW-OTHER-KEYS" && !pair[1].is_nil() {
                call_allows_other_keys = true;
            }
        }

        for (kw_bare, var, default_idx, supp) in &key_specs {
            if let Some(v) = find_key_arg(tail, kw_bare) {
                env.define_local(var, v);
                if let Some(sp) = supp {
                    env.define_local(sp, T);
                }
            } else {
                let df = key_default_forms[*default_idx];
                let dv = if df == NIL {
                    NIL
                } else {
                    eval_form(df, env)?
                };
                env.define_local(var, dv);
                if let Some(sp) = supp {
                    env.define_local(sp, NIL);
                }
            }
        }

        if !(allow_other_keys || call_allows_other_keys) {
            for pair in tail.chunks(2) {
                let bare = key_bare(pair[0]);
                if bare == "ALLOW-OTHER-KEYS" {
                    continue;
                }
                if !key_specs.iter().any(|(kw, _, _, _)| kw == &bare) {
                    // Unknown keyword to a function is an ANSI PROGRAM-ERROR
                    // (catchable), not an internal/uncatchable failure.
                    return Err(BlissError::ProgramError(format!(
                        "unexpected keyword argument: {}",
                        bare
                    )));
                }
            }
        }
    } else if !rest_bound && arg_i < args.len() {
        return Err(BlissError::ProgramError(format!(
            "too many arguments for lambda list: expected {}, got {}",
            arg_i,
            args.len()
        )));
    }

    Ok(())
}

fn bind_pattern_value(pattern: BlissVal, value: BlissVal, env: &mut Env) -> Result<(), BlissError> {
    if pattern.is_nil() {
        if value.is_nil() {
            return Ok(());
        }
        return Err(BlissError::ProgramError(format!(
            "destructuring mismatch: expected NIL, got {}",
            format_val(value)
        )));
    }

    if pattern.is_symbol() {
        env.define_local_symbol(pattern, value);
        return Ok(());
    }

    if !pattern.is_cons() {
        return Err(BlissError::ProgramError(format!(
            "invalid destructuring pattern: {}",
            format_val(pattern)
        )));
    }

    if !value.is_cons() {
        return Err(BlissError::ProgramError(format!(
            "destructuring mismatch: expected list for pattern {}, got {}",
            format_val(pattern),
            format_val(value)
        )));
    }

    let (pcar, pcdr) = cp(pattern);
    let (vcar, vcdr) = cp(value);
    bind_pattern_value(pcar, vcar, env)?;
    bind_pattern_value(pcdr, vcdr, env)
}

/// True if a nested macro pattern is a destructuring lambda list — it contains a
/// lambda-list keyword (`&optional`/`&rest`/`&key`/…) — as opposed to a plain
/// structural pattern like `(a b)` or a dotted `(k . v)`.
fn contains_lambda_list_keyword(pattern: BlissVal) -> bool {
    let mut c = pattern;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        if elem.is_symbol()
            && matches!(
                sym_name(elem).as_str(),
                "&OPTIONAL"
                    | "&REST"
                    | "&BODY"
                    | "&KEY"
                    | "&AUX"
                    | "&WHOLE"
                    | "&ENVIRONMENT"
                    | "&ALLOW-OTHER-KEYS"
            )
        {
            return true;
        }
        c = rest;
    }
    false
}

/// Bind one macro parameter pattern against a value. A symbol binds directly; a
/// nested pattern that is itself a destructuring lambda list recurses through the
/// full macro lambda-list binder so `&optional`/`&rest`/`&key` work at any depth
/// (this is what lets e.g. ASDF's `(defmacro with-upgradability ((&optional) &body body) …)`
/// expand). Every other cons pattern uses plain structural destructuring.
fn bind_macro_param(
    pattern: BlissVal,
    value: BlissVal,
    env: &mut Env,
    macroexpand_env: Option<&MacroexpandEnv>,
) -> Result<(), BlissError> {
    if pattern.is_nil() {
        if value.is_nil() {
            return Ok(());
        }
        return Err(BlissError::ProgramError(format!(
            "destructuring mismatch: expected NIL, got {}",
            format_val(value)
        )));
    }
    if pattern.is_symbol() {
        env.define_local_symbol(pattern, value);
        return Ok(());
    }
    if !pattern.is_cons() {
        return Err(BlissError::ProgramError(format!(
            "invalid destructuring pattern: {}",
            format_val(pattern)
        )));
    }
    // A sub-pattern that is a destructuring lambda list at *this* level goes to
    // the lambda-list binder (handles &optional/&rest/&key).
    if contains_lambda_list_keyword(pattern) {
        let sub_args = list_to_vec(value);
        return bind_macro_lambda_list(pattern, &sub_args, env, macroexpand_env);
    }
    // Otherwise destructure structurally, recursing through this function so a
    // keyword-bearing lambda list nested *deeper* is still detected. A symbol in
    // the cdr position binds the rest (dotted patterns).
    if !value.is_cons() {
        return Err(BlissError::ProgramError(format!(
            "destructuring mismatch: expected list for pattern {}, got {}",
            format_val(pattern),
            format_val(value)
        )));
    }
    let (pcar, pcdr) = cp(pattern);
    let (vcar, vcdr) = cp(value);
    bind_macro_param(pcar, vcar, env, macroexpand_env)?;
    bind_macro_param(pcdr, vcdr, env, macroexpand_env)
}

fn bind_macro_lambda_list(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
    macroexpand_env: Option<&MacroexpandEnv>,
) -> Result<(), BlissError> {
    #[derive(PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }

    let mut mode = Mode::Req;
    let mut arg_i = 0usize;
    let mut key_start: Option<usize> = None;
    let mut rest_bound = false;
    let mut saw_key = false;
    let mut allow_other_keys = false;
    let mut key_specs: Vec<(String, BlissVal, BlissVal, Option<String>)> = Vec::new();
    let mut whole_var: Option<BlissVal> = None;
    let whole_form = vec_to_list(args);

    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        c = rest;

        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&WHOLE" => {
                    let (var, rest_after_var) = cp(c);
                    whole_var = Some(var);
                    c = rest_after_var;
                    continue;
                }
                "&ENVIRONMENT" => {
                    let (var, rest_after_var) = cp(c);
                    let env_value = macroexpand_env
                        .cloned()
                        .map(store_macroexpand_environment)
                        .unwrap_or(NIL);
                    bind_pattern_value(var, env_value, env)?;
                    c = rest_after_var;
                    continue;
                }
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    saw_key = true;
                    key_start.get_or_insert(arg_i);
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => {
                    allow_other_keys = true;
                    continue;
                }
                _ => {}
            }
        }

        match mode {
            Mode::Req => {
                let v = args.get(arg_i).copied().ok_or_else(|| {
                    BlissError::ProgramError(format!(
                        "too few arguments for macro lambda list: missing value for {}",
                        format_val(elem)
                    ))
                })?;
                arg_i += 1;
                bind_macro_param(elem, v, env, macroexpand_env)?;
            }
            Mode::Opt => {
                let (pattern, default_form, supp) = if elem.is_symbol() {
                    (elem, NIL, None)
                } else if elem.is_cons() {
                    let (pat, r) = cp(elem);
                    let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    let supp = if r2.is_cons() {
                        Some(sym_name(cp(r2).0))
                    } else {
                        None
                    };
                    (pat, default, supp)
                } else {
                    (elem, NIL, None)
                };

                if arg_i < args.len() {
                    bind_macro_param(pattern, args[arg_i], env, macroexpand_env)?;
                    arg_i += 1;
                    if let Some(sp) = supp {
                        env.define_local(&sp, T);
                    }
                } else {
                    let dv = if default_form == NIL {
                        NIL
                    } else {
                        eval_form(default_form, env)?
                    };
                    bind_macro_param(pattern, dv, env, macroexpand_env)?;
                    if let Some(sp) = supp {
                        env.define_local(&sp, NIL);
                    }
                }
            }
            Mode::Rest => {
                if rest_bound {
                    return Err(BlissError::Internal(
                        "malformed macro lambda list: multiple &rest/&body variables".into(),
                    ));
                }
                let remaining = vec_to_list(args.get(arg_i..).unwrap_or(&[]));
                bind_macro_param(elem, remaining, env, macroexpand_env)?;
                key_start.get_or_insert(arg_i);
                rest_bound = true;
            }
            Mode::Key => {
                let (kw_bare, pattern, default_form, supp) = if elem.is_symbol() {
                    let bare = sym_name(elem);
                    (bare, elem, NIL, None)
                } else if elem.is_cons() {
                    let (head, r) = cp(elem);
                    let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    let supp = if r2.is_cons() {
                        Some(sym_name(cp(r2).0))
                    } else {
                        None
                    };
                    if head.is_symbol() {
                        (sym_name(head), head, default, supp)
                    } else if head.is_cons() {
                        let (kw_sym, r3) = cp(head);
                        let pattern = if r3.is_cons() { cp(r3).0 } else { NIL };
                        (key_bare(kw_sym), pattern, default, supp)
                    } else {
                        (String::new(), head, default, supp)
                    }
                } else {
                    (String::new(), elem, NIL, None)
                };
                key_specs.push((kw_bare, pattern, default_form, supp));
            }
            Mode::Aux => {
                let (pattern, default_form) = if elem.is_symbol() {
                    (elem, NIL)
                } else if elem.is_cons() {
                    let (pat, r) = cp(elem);
                    let (default, _) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    (pat, default)
                } else {
                    (elem, NIL)
                };
                let dv = if default_form == NIL {
                    NIL
                } else {
                    eval_form(default_form, env)?
                };
                bind_pattern_value(pattern, dv, env)?;
            }
        }
    }

    if let Some(var) = whole_var {
        bind_pattern_value(var, whole_form, env)?;
    }

    if saw_key {
        let start = key_start.unwrap_or(arg_i);
        let tail = args.get(start..).unwrap_or(&[]);
        if tail.len() % 2 != 0 {
            return Err(BlissError::Internal(
                "macro keyword arguments must appear in key/value pairs".into(),
            ));
        }

        let mut call_allows_other_keys = false;
        for pair in tail.chunks(2) {
            let key = pair[0];
            if !key.is_symbol() {
                return Err(BlissError::TypeError {
                    datum: key,
                    expected: "keyword".into(),
                });
            }
            let bare = key_bare(key);
            if bare == "ALLOW-OTHER-KEYS" && !pair[1].is_nil() {
                call_allows_other_keys = true;
            }
        }

        for (kw_bare, pattern, default_form, supp) in &key_specs {
            if let Some(v) = find_key_arg(tail, kw_bare) {
                bind_macro_param(*pattern, v, env, macroexpand_env)?;
                if let Some(sp) = supp {
                    env.define_local(sp, T);
                }
            } else {
                let dv = if *default_form == NIL {
                    NIL
                } else {
                    eval_form(*default_form, env)?
                };
                bind_macro_param(*pattern, dv, env, macroexpand_env)?;
                if let Some(sp) = supp {
                    env.define_local(sp, NIL);
                }
            }
        }

        if !(allow_other_keys || call_allows_other_keys) {
            for pair in tail.chunks(2) {
                let bare = key_bare(pair[0]);
                if bare == "ALLOW-OTHER-KEYS" {
                    continue;
                }
                if !key_specs.iter().any(|(kw, _, _, _)| kw == &bare) {
                    return Err(BlissError::Internal(format!(
                        "unexpected macro keyword argument: {}",
                        bare
                    )));
                }
            }
        }
    } else if !rest_bound && arg_i < args.len() {
        return Err(BlissError::ProgramError(format!(
            "too many arguments for macro lambda list: expected {}, got {}",
            arg_i,
            args.len()
        )));
    }

    Ok(())
}

// ── DEFMACRO ─────────────────────────────────────────────────────
fn eval_defmacro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);

    // A top-level DEFMACRO is GLOBAL (CLHS): register it in the global macro
    // table so it survives the throwaway child Envs used while compiling/loading
    // other files. Also clear any stale same-named global function so the name
    // resolves as a macro (bliss-lb6.22).
    global_macro_insert(
        name,
        MacroDef {
            params_form,
            body,
            captured_frame: Rc::clone(&env.frame),
            bytecode: None,
        },
    );
    Ok(name_form)
}

fn eval_define_setf_expander(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // (define-setf-expander access-fn lambda-list . body) — register a macro-like
    // expander keyed by ACCESS-FN. It is called with the place's subforms and
    // returns the five setf-expansion values.
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);
    env.setf_expanders.borrow_mut().insert(
        name,
        SetfExpander::Expander(MacroDef {
            params_form,
            body,
            captured_frame: Rc::clone(&env.frame),
            bytecode: None,
        }),
    );
    Ok(name_form)
}

fn eval_defsetf_short(
    name_form: BlissVal,
    update_fn: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    env.setf_expanders
        .borrow_mut()
        .insert(sym_name(name_form), SetfExpander::ShortUpdate(update_fn));
    Ok(name_form)
}

fn eval_defsetf_long(
    name_form: BlissVal,
    _lambda_list: BlissVal,
    _rest: BlissVal,
    _env: &mut Env,
) -> Result<BlissVal, BlissError> {
    // The long form `(defsetf access-fn (args…) (store) body…)` is accepted but
    // not registered; `(setf (access-fn …) …)` then falls through to the default
    // `(setf access-fn)` writer path. DEFINE-SETF-EXPANDER covers the cases we
    // actually need (e.g. alexandria's assoc-value).
    Ok(name_form)
}

/// A fresh uninterned symbol for use as a setf-expansion temporary, so it can
/// never capture a variable in the caller's code.
fn gensym_symbol(prefix: &str) -> BlissVal {
    thread_local! { static COUNTER: RefCell<u64> = const { RefCell::new(0) }; }
    let n = COUNTER.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    });
    reader::make_uninterned_symbol(&format!("#:{prefix}-SETF-{n}"))
}

/// The five values of a setf-expansion (CLHS 5.1.2.3): temporary variables,
/// their value forms, the store variables, the storing form, and the accessing
/// form.
struct SetfExpansion {
    temps: Vec<BlissVal>,
    vals: Vec<BlissVal>,
    stores: Vec<BlissVal>,
    store_form: BlissVal,
    access_form: BlissVal,
}

/// Compute the setf-expansion of PLACE (CLHS GET-SETF-EXPANSION).
fn get_setf_expansion(place: BlissVal, env: &mut Env) -> Result<SetfExpansion, BlissError> {
    // A variable (or symbol-macro) place: no temporaries; store via SETQ.
    if place.is_symbol() {
        if let Some(expansion) = env.lookup_symbol_macro(place) {
            return get_setf_expansion(expansion, env);
        }
        let store = gensym_symbol("NEW");
        let setq = vec_to_list(&[resolve_sym("SETQ").unwrap_or(NIL), place, store]);
        return Ok(SetfExpansion {
            temps: Vec::new(),
            vals: Vec::new(),
            stores: vec![store],
            store_form: setq,
            access_form: place,
        });
    }
    if place.is_cons() {
        let (accessor, args) = cp(place);
        let acc = if accessor.is_symbol() {
            sym_name(accessor)
        } else {
            String::new()
        };
        // A user-defined expander wins.
        let expander = env.setf_expanders.borrow().get(&acc).cloned();
        if let Some(expander) = expander {
            match expander {
                SetfExpander::Expander(mdef) => {
                    // Apply it to the place's subforms and read back the five
                    // values it returns via (values …). The expander body runs in
                    // a CHILD env, so the multiple values land on that child's mv —
                    // read them there, not on the caller's env.
                    let mut child = env.child_with_parent(Rc::clone(&mdef.captured_frame));
                    let arg_list = list_to_vec(args);
                    let first = if let Some(function) = &mdef.bytecode {
                        // A source-free bytecode expander (loaded from a .bfasl):
                        // run it like a macro; its (values …) land on the child mv.
                        bytecode::run_macro(
                            Rc::new(function.borrow().clone()),
                            &arg_list,
                            &mut child,
                        )?
                    } else {
                        let macroexpand_env = if params_form_uses_environment(mdef.params_form) {
                            Some(macroexpand_environment_from_cli(env))
                        } else {
                            None
                        };
                        bind_macro_lambda_list(
                            mdef.params_form,
                            &arg_list,
                            &mut child,
                            macroexpand_env.as_ref(),
                        )?;
                        eval_progn(mdef.body, &mut child)?
                    };
                    let mut values = if child.mv_active {
                        std::mem::take(&mut child.mv)
                    } else {
                        vec![first]
                    };
                    while values.len() < 5 {
                        values.push(NIL);
                    }
                    return Ok(SetfExpansion {
                        temps: list_to_vec(values[0]),
                        vals: list_to_vec(values[1]),
                        stores: list_to_vec(values[2]),
                        store_form: values[3],
                        access_form: values[4],
                    });
                }
                SetfExpander::ShortUpdate(update_fn) => {
                    // (setf (name arg…) new) => (update-fn arg… new).
                    let arg_forms = list_to_vec(args);
                    let temps: Vec<BlissVal> =
                        (0..arg_forms.len()).map(|_| gensym_symbol("A")).collect();
                    let store = gensym_symbol("NEW");
                    let mut access_items = vec![accessor];
                    access_items.extend_from_slice(&temps);
                    let mut store_items = vec![update_fn];
                    store_items.extend_from_slice(&temps);
                    store_items.push(store);
                    return Ok(SetfExpansion {
                        temps,
                        vals: arg_forms,
                        stores: vec![store],
                        store_form: vec_to_list(&store_items),
                        access_form: vec_to_list(&access_items),
                    });
                }
            }
        }
        // Default expansion for a function place with a `(setf f)` writer: bind
        // each argument to a temporary, then store via `((setf f) new t1 t2 …)`
        // and access via `(f t1 t2 …)`.
        let arg_forms = list_to_vec(args);
        let temps: Vec<BlissVal> = (0..arg_forms.len()).map(|_| gensym_symbol("A")).collect();
        let store = gensym_symbol("NEW");
        let mut access_items = vec![accessor];
        access_items.extend_from_slice(&temps);
        let access_form = vec_to_list(&access_items);
        let setf_fn = vec_to_list(&[resolve_sym("SETF").unwrap_or(NIL), accessor]);
        let mut store_items = vec![resolve_sym("FUNCALL").unwrap_or(NIL), setf_fn, store];
        store_items.extend_from_slice(&temps);
        let store_form = vec_to_list(&store_items);
        return Ok(SetfExpansion {
            temps,
            vals: arg_forms,
            stores: vec![store],
            store_form,
            access_form,
        });
    }
    Err(BlissError::Internal(format!(
        "GET-SETF-EXPANSION: not a place: {}",
        format_val(place)
    )))
}

/// Store NEW_VALUE into PLACE using its setf-expansion: bind the temporaries to
/// their value forms (sequentially, like LET*), bind the store variable to
/// NEW_VALUE, then evaluate the storing form. Returns NEW_VALUE.
fn apply_setf_expansion(
    place: BlissVal,
    new_value: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let ex = get_setf_expansion(place, env)?;
    let parent = Rc::clone(&env.frame);
    with_child_frame(env, parent, move |env| {
        for (temp, val_form) in ex.temps.iter().zip(ex.vals.iter()) {
            let v = eval_form(*val_form, env)?;
            if temp.is_symbol() {
                env.define_local_symbol(*temp, v);
            }
        }
        if let Some(store) = ex.stores.first() {
            if store.is_symbol() {
                env.define_local_symbol(*store, new_value);
            }
        }
        eval_form(ex.store_form, env)?;
        Ok(new_value)
    })
}

fn eval_define_symbol_macro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (symbol, rest) = cp(cdr);
    let (expansion, _) = cp(rest);
    if !symbol.is_symbol() {
        return Err(BlissError::Internal(
            "DEFINE-SYMBOL-MACRO: name must be a symbol".into(),
        ));
    }
    env.define_symbol_macro(symbol, expansion);
    Ok(symbol)
}

fn eval_define_compiler_macro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    if !name_form.is_symbol() {
        // A compiler macro is always optional (CLHS 3.2.2.1.3: the compiler is
        // not required to use one), and our registry keys expanders by symbol.
        // For a `(setf f)` function name — e.g. ASDF's WITH-DEPRECATION
        // instrumenting a `(defmethod (setf foo) …)` — just skip defining the
        // compiler macro; the underlying function/method still works.
        return Ok(name_form);
    }

    // The expander is stored in bliss-compiler's global registry as an
    // Arc<dyn Fn + Send + Sync>, so it must own a Send snapshot of the defining
    // lexical frame rather than share the live Rc chain. See FrozenEnvFrame.
    let capture = register_frozen_macro_capture(FrozenMacroCapture {
        params_form,
        body,
        captured_frame: freeze_env_frame(&env.frame),
        funs: env.funs.borrow().clone(),
        classes: env.classes.borrow().clone(),
        methods: env.methods.borrow().clone(),
        symbol_macros: env.symbol_macros.borrow().clone(),
    });
    let current_package = env.current_package.clone();
    let sandbox = env.sandbox;
    let eval_context = env.eval_context;

    compiler_macroexpand::define_compiler_macro(
        name_form,
        Arc::new(move |form, _macro_env| {
            let (params_form, body, captured_frame, funs, classes, methods, symbol_macros) = {
                let capture = capture
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (
                    capture.params_form,
                    capture.body,
                    Arc::clone(&capture.captured_frame),
                    capture.funs.clone(),
                    capture.classes.clone(),
                    capture.methods.clone(),
                    capture.symbol_macros.clone(),
                )
            };
            let (_, args) = cp(form);
            let mut macro_env = Env::new_for_macro_expansion(sandbox);
            macro_env.frame = thaw_env_frame(&captured_frame);
            macro_env.funs = Rc::new(RefCell::new(funs.clone()));
            macro_env.macros = Rc::new(RefCell::new(HashMap::new()));
            macro_env.symbol_macros = Rc::new(RefCell::new(symbol_macros.clone()));
            macro_env.classes = Rc::new(RefCell::new(classes.clone()));
            macro_env.methods = Rc::new(RefCell::new(methods.clone()));
            macro_env.current_package = current_package.clone();
            macro_env.eval_context = eval_context;
            // Register the fresh macro-expansion Env as a GC root *before*
            // binding: it holds the macro parameters (e.g. `&body body`) and
            // every variable the body binds (e.g. a `case`/`typecase` gensym), on
            // a frame chain a relocating minor GC would otherwise never scan. The
            // guard must precede bind_macro_lambda_list — building a `&rest`/
            // `&body` list allocates (arena_cons), which can fire a GC that would
            // free the already-bound parameters if the env were still
            // unregistered (bliss-6b2 #2). Same guard as eval_flet / eval_macrolet.
            let _macro_env_root = LiveEnvRootGuard::new(&mut macro_env);
            bind_macro_lambda_list(
                params_form,
                &list_to_vec(args),
                &mut macro_env,
                Some(_macro_env),
            )?;
            eval_progn(body, &mut macro_env)
        }),
    );

    Ok(name_form)
}

/// True if a macro lambda list references `&ENVIRONMENT` — the only reason
/// `expand_macro` needs the (expensive to build) macroexpand environment.
fn params_form_uses_environment(params_form: BlissVal) -> bool {
    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        if elem.is_symbol() && sym_name(elem) == "&ENVIRONMENT" {
            return true;
        }
        if elem.is_cons() && params_form_uses_environment(elem) {
            return true;
        }
        c = rest;
    }
    false
}

/// Depth cap for the definition-time macroexpansion pass. Beyond it we stop
/// expanding and leave the form to the lazy `eval_list` path — a backstop
/// against a runaway (self-referential) macro, never hit by real code.
const MACROEXPAND_ALL_MAX_DEPTH: u32 = 400;

/// Macroexpand each element of the (proper-spine) list `list` independently as
/// code, rebuilding the list. Symbols (tags, keywords, binding names) and other
/// atoms pass through untouched; only genuine sub-forms are expanded. The list
/// spine is walked iteratively so a long body cannot overflow the Rust stack;
/// only *nesting* recurses (bounded by `MACROEXPAND_ALL_MAX_DEPTH`).
fn mx_each(list: BlissVal, env: &mut Env, depth: u32) -> BlissVal {
    let mut items = Vec::new();
    let mut c = list;
    while c.is_cons() {
        let (h, t) = cp(c);
        items.push(h);
        c = t;
    }
    // `c` is the (usually NIL) tail; preserve it so dotted lists round-trip.
    let mut out = c;
    for &it in items.iter().rev() {
        out = arena_cons(macroexpand_all(it, env, depth), out);
    }
    out
}

/// Expand every macro call reachable in *evaluated position* within `form`,
/// once (a conservative macroexpand-all / minimal compilation — CLHS 3.2.2.2).
///
/// Safety principle: only expand where the position is certainly code. Quoted
/// data, type specifiers, local-macro scopes (MACROLET/SYMBOL-MACROLET), the
/// LOOP sublanguage, and binding *names* are left verbatim; a form we are unsure
/// about is returned unchanged and handled by the lazy expansion in `eval_list`
/// at call time. An expander error also leaves the form verbatim. Thus the pass
/// can only ever *under*-expand, never miscompile.
fn macroexpand_all(form: BlissVal, env: &mut Env, depth: u32) -> BlissVal {
    if depth >= MACROEXPAND_ALL_MAX_DEPTH || !form.is_cons() {
        return form;
    }
    let d = depth + 1;
    let (car, cdr) = cp(form);

    // `((lambda (..) ..) args..)` — expand the operator form and the arguments.
    if car.is_cons() {
        return arena_cons(macroexpand_all(car, env, d), mx_each(cdr, env, d));
    }
    if !car.is_symbol() {
        return form;
    }
    let name = sym_name(car);

    // 1. Macro call → expand once, then recurse into the expansion. On any
    //    expander error, leave the original form for the lazy path.
    if let Some(mdef) = lookup_macro(env, &name) {
        return match expand_macro(&mdef, cdr, env) {
            Ok(expanded) => macroexpand_all(expanded, env, d),
            Err(_) => form,
        };
    }

    // 2. Special forms that must NOT be walked generically.
    match name.as_str() {
        // Data / sublanguages / local-macro scopes: leave the whole form.
        "QUOTE" | "BLISS::QUASIQUOTE" | "MACROLET" | "SYMBOL-MACROLET" | "LOOP" | "DECLARE"
        | "GO" => form,

        // Binding forms: expand init-forms and bodies but preserve the bound
        // names (a variable named like a macro must not be expanded away).
        "LET" | "LET*" => {
            let (bindings, body) = cp(cdr);
            let new_bindings = mx_bindings(bindings, env, d);
            arena_cons(car, arena_cons(new_bindings, mx_each(body, env, d)))
        }
        "LAMBDA" => {
            // (lambda lambda-list body...) — keep the lambda list, expand body.
            let (ll, body) = cp(cdr);
            arena_cons(car, arena_cons(ll, mx_each(body, env, d)))
        }
        "FLET" | "LABELS" => {
            // (flet ((name lambda-list fbody...) ...) body...)
            let (defs, body) = cp(cdr);
            let new_defs = mx_local_fns(defs, env, d);
            arena_cons(car, arena_cons(new_defs, mx_each(body, env, d)))
        }
        "MULTIPLE-VALUE-BIND" => {
            // (multiple-value-bind (vars) value-form body...)
            let (vars, rest) = cp(cdr);
            let (value_form, body) = cp(rest);
            arena_cons(
                car,
                arena_cons(
                    vars,
                    arena_cons(macroexpand_all(value_form, env, d), mx_each(body, env, d)),
                ),
            )
        }
        "DESTRUCTURING-BIND" => {
            // (destructuring-bind pattern value-form body...)
            let (pattern, rest) = cp(cdr);
            let (value_form, body) = cp(rest);
            arena_cons(
                car,
                arena_cons(
                    pattern,
                    arena_cons(macroexpand_all(value_form, env, d), mx_each(body, env, d)),
                ),
            )
        }
        "COND" => {
            // (cond (test body...) ...) — each clause element is a standalone
            // form (the clause car is a *test*, not an operator), so expand the
            // clause element-wise rather than as one call form.
            let mut clauses = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (clause, rest) = cp(c);
                clauses.push(if clause.is_cons() {
                    mx_each(clause, env, d)
                } else {
                    clause
                });
                c = rest;
            }
            let mut out = c;
            for &cl in clauses.iter().rev() {
                out = arena_cons(cl, out);
            }
            arena_cons(car, out)
        }
        "FUNCTION" => {
            // (function (lambda ...)) — expand the lambda; (function name) — leave.
            let (target, _) = cp(cdr);
            if target.is_cons() {
                arena_cons(car, arena_cons(macroexpand_all(target, env, d), NIL))
            } else {
                form
            }
        }

        // Everything else — other special forms whose arguments are all code
        // (PROGN, WHEN, IF, AND, OR, TAGBODY, BLOCK, SETQ, SETF, CATCH, THE,
        // RETURN-FROM, UNWIND-PROTECT, EVAL-WHEN, …) and ordinary function
        // calls — expand each argument independently. Symbols in argument
        // position (tags, block names, setq/setf place symbols) pass through.
        _ => arena_cons(car, mx_each(cdr, env, d)),
    }
}

/// Expand the init-forms of a LET/LET* binding list, preserving each binding's
/// variable name. A binding is `name`, `(name)`, or `(name init)`.
fn mx_bindings(bindings: BlissVal, env: &mut Env, depth: u32) -> BlissVal {
    let mut out_items = Vec::new();
    let mut c = bindings;
    while c.is_cons() {
        let (b, rest) = cp(c);
        let nb = if b.is_cons() {
            let (var, init) = cp(b);
            // Keep `var`; expand every init-form after it.
            arena_cons(var, mx_each(init, env, depth))
        } else {
            b
        };
        out_items.push(nb);
        c = rest;
    }
    let mut out = c;
    for &it in out_items.iter().rev() {
        out = arena_cons(it, out);
    }
    out
}

/// Expand the bodies of FLET/LABELS local functions, preserving each name and
/// lambda list: `(name lambda-list fbody...)`.
fn mx_local_fns(defs: BlissVal, env: &mut Env, depth: u32) -> BlissVal {
    let mut out_items = Vec::new();
    let mut c = defs;
    while c.is_cons() {
        let (def, rest) = cp(c);
        let nd = if def.is_cons() {
            let (fname, after_name) = cp(def);
            if after_name.is_cons() {
                let (ll, fbody) = cp(after_name);
                arena_cons(fname, arena_cons(ll, mx_each(fbody, env, depth)))
            } else {
                def
            }
        } else {
            def
        };
        out_items.push(nd);
        c = rest;
    }
    let mut out = c;
    for &it in out_items.iter().rev() {
        out = arena_cons(it, out);
    }
    out
}

fn expand_macro(mdef: &MacroDef, args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut child_env = env.child_with_parent(Rc::clone(&mdef.captured_frame));
    let arg_list = list_to_vec(args);
    if let Some(function) = &mdef.bytecode {
        return bytecode::run_macro(
            Rc::new(function.borrow().clone()),
            &arg_list,
            &mut child_env,
        );
    }
    // Building the macroexpand environment walks every frame and re-registers
    // *all* global macros into bliss-compiler's macro table (with fresh handles
    // and frozen frame snapshots). That is only needed to satisfy an
    // `&ENVIRONMENT` parameter, which almost no macro has — doing it on every
    // expansion made loading macro-heavy files (lib/asdf.lisp) blow up to
    // multi-GB and never finish. Build it only when the lambda list uses it.
    let macroexpand_env = if params_form_uses_environment(mdef.params_form) {
        Some(macroexpand_environment_from_cli(env))
    } else {
        None
    };
    // Register the forked expansion Env as a GC root for the whole expansion:
    // `env.child_with_parent` forks a whole Env, so the macro parameters and
    // every variable the body binds (e.g. a `loop`/`case` iteration var inside a
    // quasiquote — as in asdf's `with-upgradability`) live on a frame chain a
    // relocating minor GC would otherwise never scan, freeing them mid-expansion
    // (bliss-6b2 #2). The guard must precede the bind: building a `&rest`/`&body`
    // list allocates and can fire a GC before the body even runs. Same guard as
    // eval_flet / eval_macrolet.
    let _child_root = LiveEnvRootGuard::new(&mut child_env);
    bind_macro_lambda_list(
        mdef.params_form,
        &arg_list,
        &mut child_env,
        macroexpand_env.as_ref(),
    )?;
    eval_progn(mdef.body, &mut child_env)
}

fn macroexpand_environment_from_cli(env: &Env) -> MacroexpandEnv {
    fn collect_frames(frame: &Rc<RefCell<EnvFrame>>, frames: &mut Vec<Rc<RefCell<EnvFrame>>>) {
        let parent = frame.borrow().parent.clone();
        if let Some(parent) = parent {
            collect_frames(&parent, frames);
        }
        frames.push(Rc::clone(frame));
    }

    let mut macro_env = MacroexpandEnv::null();

    let mut global_symbol_macros = Vec::new();
    for (&symbol_index, &expansion) in env.symbol_macros.borrow().iter() {
        global_symbol_macros.push((
            BlissVal::from_symbol_index(symbol_index),
            VariableInfo::SymbolMacro(expansion),
        ));
    }
    if !global_symbol_macros.is_empty() {
        macro_env = macro_env.augment_environment(global_symbol_macros, Vec::new(), Vec::new());
    }

    let mut frames = Vec::new();
    collect_frames(&env.frame, &mut frames);
    for frame in frames {
        let borrowed = frame.borrow();
        let mut variables = Vec::new();
        for &symbol_index in borrowed.symbol_vars.keys() {
            variables.push((
                BlissVal::from_symbol_index(symbol_index),
                VariableInfo::Lexical,
            ));
        }
        if !variables.is_empty() {
            macro_env = macro_env.augment_environment(variables, Vec::new(), Vec::new());
        }
    }

    // Expose both global (top-level DEFMACRO) and lexical (MACROLET) macros to
    // the bytecode compiler; a lexical macro of the same name shadows the global.
    let mut all_macros: HashMap<String, MacroDef> = GLOBAL_MACROS.with(|m| m.borrow().clone());
    for (name, def) in env.macros.borrow().iter() {
        all_macros.insert(name.clone(), def.clone());
    }
    for (name, macro_def) in all_macros.iter() {
        let params_bits = macro_def.params_form.0;
        let body_bits = macro_def.body.0;
        let frame_ptr = Rc::as_ptr(&macro_def.captured_frame) as usize;
        let bytecode_ptr = macro_def
            .bytecode
            .as_ref()
            .map_or(0, |function| Rc::as_ptr(function) as usize);

        // Reuse the cached registration if this exact macro definition was
        // already registered (bliss-gq5.8) — the common case across the many
        // forms compiled during a load. Only a redefinition (changed params,
        // body, or captured frame) falls through to re-register.
        let cached = MACRO_FN_CACHE.with(|c| c.borrow().get(name).copied());
        let (handle, symbol) = match cached {
            Some(e)
                if e.params_bits == params_bits
                    && e.body_bits == body_bits
                    && e.frame_ptr == frame_ptr
                    && e.bytecode_ptr == bytecode_ptr =>
            {
                (e.handle, e.symbol)
            }
            _ => {
                let handle = next_macro_function_handle();
                if let Some(function) = &macro_def.bytecode {
                    let function = Arc::new(Mutex::new(function.borrow().clone()));
                    LOADED_COMPILER_MACRO_FUNCTIONS
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(Arc::downgrade(&function));
                    compiler_macroexpand::register_macro_function(
                        handle,
                        Arc::new(move |form, _call_macro_env| {
                            let (_, args) = cp(form);
                            let function = function
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .clone();
                            let mut macro_env = Env::new_for_macro_expansion(false);
                            bytecode::run_macro(
                                Rc::new(function),
                                &list_to_vec(args),
                                &mut macro_env,
                            )
                        }),
                    );
                    let symbol = resolve_sym(name).unwrap_or(NIL);
                    MACRO_FN_CACHE.with(|c| {
                        c.borrow_mut().insert(
                            name.clone(),
                            MacroFnCacheEntry {
                                params_bits,
                                body_bits,
                                frame_ptr,
                                bytecode_ptr,
                                handle,
                                symbol,
                            },
                        );
                    });
                    if !symbol.is_nil() {
                        macro_env = macro_env.augment_function(symbol, FunctionInfo::Macro(handle));
                    }
                    continue;
                }
                // Registered into bliss-compiler's global Send + Sync macro
                // table, so the closure owns a frozen snapshot instead of the
                // live Rc frame. See FrozenEnvFrame. (The ordinary expand_macro
                // path shares the live frame.)
                let capture = register_frozen_macro_capture(FrozenMacroCapture {
                    params_form: macro_def.params_form,
                    body: macro_def.body,
                    captured_frame: freeze_env_frame(&macro_def.captured_frame),
                    funs: HashMap::new(),
                    classes: HashMap::new(),
                    methods: HashMap::new(),
                    symbol_macros: HashMap::new(),
                });
                compiler_macroexpand::register_macro_function(
                    handle,
                    Arc::new(move |form, call_macro_env| {
                        let (params_form, body, captured_frame) = {
                            let capture = capture
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            (
                                capture.params_form,
                                capture.body,
                                Arc::clone(&capture.captured_frame),
                            )
                        };
                        let (_, args) = cp(form);
                        let mut macro_env = Env::new_for_macro_expansion(false);
                        macro_env.frame = thaw_env_frame(&captured_frame);
                        // The expander body finds other global macros via
                        // GLOBAL_MACROS (lookup_macro), so the reconstructed Env
                        // needs no macro table.
                        // Root the fresh expansion Env before binding: it holds
                        // the macro parameters (e.g. `&body body`) and every
                        // variable the body binds (loop vars, case gensyms) on a
                        // frame chain the relocating minor GC would otherwise never
                        // scan — freeing them mid-expansion (bliss-6b2 #2). This is
                        // the path asdf's `with-upgradability` takes.
                        let _macro_env_root = LiveEnvRootGuard::new(&mut macro_env);
                        bind_macro_lambda_list(
                            params_form,
                            &list_to_vec(args),
                            &mut macro_env,
                            Some(call_macro_env),
                        )?;
                        eval_progn(body, &mut macro_env)
                    }),
                );
                let symbol = resolve_sym(name).unwrap_or(NIL);
                MACRO_FN_CACHE.with(|c| {
                    c.borrow_mut().insert(
                        name.clone(),
                        MacroFnCacheEntry {
                            params_bits,
                            body_bits,
                            frame_ptr,
                            bytecode_ptr,
                            handle,
                            symbol,
                        },
                    );
                });
                (handle, symbol)
            }
        };
        if !symbol.is_nil() {
            macro_env = macro_env.augment_function(symbol, FunctionInfo::Macro(handle));
        }
    }

    macro_env
}

fn eval_macroexpand(
    cdr: BlissVal,
    env: &mut Env,
    single_step: bool,
) -> Result<BlissVal, BlissError> {
    let (form_expr, rest) = cp(cdr);
    let form = eval_form(form_expr, env)?;
    let macro_env = if rest.is_cons() {
        let (env_expr, _) = cp(rest);
        let env_value = eval_form(env_expr, env)?;
        load_macroexpand_environment(env_value).ok_or_else(|| {
            BlissError::Internal("MACROEXPAND: invalid lexical environment".into())
        })?
    } else {
        macroexpand_environment_from_cli(env)
    };
    let (expanded, expanded_p) = if single_step {
        compiler_macroexpand::macroexpand_1(form, &macro_env)?
    } else {
        compiler_macroexpand::macroexpand(form, &macro_env)?
    };
    env.set_mv(vec![expanded, if expanded_p { T } else { NIL }]);
    Ok(expanded)
}

fn eval_macrolet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (defs_form, body) = cp(cdr);
    let mut child_env = env.child();
    for def in list_to_vec(defs_form) {
        if !def.is_cons() {
            continue;
        }
        let (name_form, rest) = cp(def);
        let (params_form, macro_body) = cp(rest);
        let name = sym_name(name_form);
        child_env.macros_mut().insert(
            name,
            MacroDef {
                params_form,
                body: macro_body,
                captured_frame: Rc::clone(&env.frame),
                bytecode: None,
            },
        );
    }
    // Register the forked child Env as a GC root for the extent of the body:
    // `env.child()` forks a whole Env, so variables bound *inside* the macrolet
    // body (e.g. a `case`/`typecase` gensym `(let ((g key)) …)`) live on
    // child_env's frame chain, which — unlike an in-place `with_child_frame` —
    // is otherwise invisible to the relocating minor GC and would be freed
    // mid-body (bliss-6b2 #2). Same guard as eval_flet.
    let _child_root = LiveEnvRootGuard::new(&mut child_env);
    let __r = eval_progn(body, &mut child_env);
    // Multiple values produced in the child body must propagate to the caller;
    // env.child() forks the value registers (bliss-lb6.22).
    env.mv = std::mem::take(&mut child_env.mv);
    env.mv_active = child_env.mv_active;
    __r
}

fn eval_symbol_macrolet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let mut child_env = env.child();
    for binding in list_to_vec(bindings_form) {
        if !binding.is_cons() {
            continue;
        }
        let (symbol, expansion_rest) = cp(binding);
        if !symbol.is_symbol() {
            continue;
        }
        let (expansion, _) = cp(expansion_rest);
        child_env.define_symbol_macro(symbol, expansion);
    }
    // Root the forked child Env for the body's extent so variables bound inside
    // the symbol-macrolet body survive a relocating minor GC (bliss-6b2 #2).
    let _child_root = LiveEnvRootGuard::new(&mut child_env);
    let __r = eval_progn(body, &mut child_env);
    // Multiple values produced in the child body must propagate to the caller;
    // env.child() forks the value registers (bliss-lb6.22).
    env.mv = std::mem::take(&mut child_env.mv);
    env.mv_active = child_env.mv_active;
    __r
}

// ── DEFCLASS ─────────────────────────────────────────────────────
fn eval_defclass(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (supers_form, rest2) = cp(rest);
    let (slots_form, _) = cp(rest2);

    let name = sym_name(name_form);

    // Parse superclasses
    let mut supers = Vec::new();
    let super_list = list_to_vec(supers_form);
    for s in &super_list {
        supers.push(sym_name(*s));
    }

    // Parse slots
    let mut slots = Vec::new();
    let slot_list = list_to_vec(slots_form);
    for slot_form in &slot_list {
        if slot_form.is_cons() {
            let (slot_name_form, slot_opts) = cp(*slot_form);
            let slot_name = sym_name(slot_name_form);
            let mut initargs: Vec<String> = Vec::new();
            let mut accessor = None;
            let mut readers = Vec::new();
            let mut writers = Vec::new();
            let mut initform = None;
            let mut allocation = SlotAllocation::Instance;

            // Parse slot options
            let opts = list_to_vec(slot_opts);
            let mut i = 0;
            while i < opts.len() {
                let opt_name = sym_name(opts[i]);
                let opt_bare = opt_name
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':');
                if opt_bare == "INITARG" {
                    if i + 1 < opts.len() {
                        let ia = sym_name(opts[i + 1]);
                        // A slot may declare more than one :initarg — collect them
                        // all so make-instance accepts any (e.g. :licence/:license).
                        initargs.push(
                            ia.trim_start_matches("KEYWORD:")
                                .trim_start_matches(':')
                                .to_string(),
                        );
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "ACCESSOR" {
                    if i + 1 < opts.len() {
                        let accessor_name = sym_name(opts[i + 1]);
                        accessor = Some(accessor_name.clone());
                        readers.push(accessor_name);
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "READER" {
                    if i + 1 < opts.len() {
                        readers.push(sym_name(opts[i + 1]));
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "WRITER" {
                    if i + 1 < opts.len() {
                        if opts[i + 1].is_symbol() {
                            writers.push(sym_name(opts[i + 1]));
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "INITFORM" {
                    if i + 1 < opts.len() {
                        initform = Some(opts[i + 1]);
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "ALLOCATION" {
                    if i + 1 < opts.len() {
                        let allocation_name = symbol_bare_name(&sym_name(opts[i + 1]));
                        if allocation_name == "CLASS" {
                            allocation = SlotAllocation::Class;
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "TYPE" || opt_bare == "DOCUMENTATION" {
                    i += 2;
                } else {
                    i += 1;
                }
            }

            slots.push(SlotDef {
                name: slot_name,
                initargs,
                accessor,
                readers,
                writers,
                initform,
                allocation,
            });
        } else if slot_form.is_symbol() {
            slots.push(SlotDef {
                name: sym_name(*slot_form),
                initargs: Vec::new(),
                accessor: None,
                readers: Vec::new(),
                writers: Vec::new(),
                initform: None,
                allocation: SlotAllocation::Instance,
            });
        }
    }

    let mut class_slot_values = HashMap::new();
    for slot in &slots {
        if slot.allocation == SlotAllocation::Class {
            class_slot_values.insert(slot.name.clone(), None);
        }
    }

    env.classes.borrow_mut().insert(
        name.clone(),
        ClassDef {
            name: name.clone(),
            supers,
            slots,
            class_slot_values: Arc::new(Mutex::new(class_slot_values)),
        },
    );
    let direct_supers: Result<Vec<BlissVal>, BlissError> = super_list
        .iter()
        .map(|super_name| resolve_class_metaobject(env, *super_name))
        .collect();
    // Only :instance-allocated slots get an inline cell in the heap-object
    // instance layout; :class-allocated slots live in ClassDef.class_slot_values.
    let slot_names: Vec<BlissVal> = env.classes.borrow()[&name]
        .slots
        .iter()
        .filter(|slot| slot.allocation == SlotAllocation::Instance)
        .map(|slot| resolve_sym(&slot.name).unwrap_or(NIL))
        .collect();
    bliss_stdlib::define_class(name_form, name_form, &direct_supers?, &slot_names)?;
    Ok(name_form)
}

/// Install the structural payload of a source-free DEFINE-CONDITION load
/// action. This mirrors boot.lisp's macro without invoking the reader or macro
/// compiler from a BFASL load.
fn define_condition_portable(
    name: BlissVal,
    parents: BlissVal,
    slots: BlissVal,
    options: BlissVal,
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let effective_parents = if parents.is_nil() {
        vec_to_list(&[resolve_sym("CONDITION").unwrap_or(NIL)])
    } else {
        parents
    };
    for (variable, entry) in [
        ("*CONDITION-TYPES*", vec_to_list(&[name, effective_parents])),
        (
            "*CONDITION-DEFINITIONS*",
            vec_to_list(&[name, effective_parents, slots, options]),
        ),
    ] {
        let symbol = resolve_sym(variable).unwrap_or(NIL);
        let old = env
            .lookup_var_symbol(symbol)
            .or_else(|| env.lookup_var(variable))
            .unwrap_or(NIL);
        env.set_var_symbol(symbol, arena_cons(entry, old));
    }

    eval_defclass(vec_to_list(&[name, effective_parents, slots]), env)?;

    // DEFINE-CONDITION also defines requested slot readers/accessors. Reuse the
    // boot helper that computes those DEFUN forms from the structural slots.
    if let Some(helper) = resolve_sym("%DEFINE-CONDITION-READER-DEFS") {
        if fn_bound(env, &sym_name(helper)) {
            let definitions = apply_function(helper, &[slots], env)?;
            for definition in list_to_vec(definitions) {
                eval_form(definition, env)?;
            }
        }
    }
    Ok(name)
}

// ── DEFSTRUCT ────────────────────────────────────────────────────
/// A minimal `defstruct` implemented on top of CLOS: it expands to a `defclass`
/// plus a `make-NAME` keyword constructor, a `NAME-P` predicate, a `copy-NAME`
/// copier, and `NAME-slot` accessors, then evaluates those forms. Structure
/// options (e.g. `:conc-name`, `:constructor`) are accepted but ignored; the
/// standard default names are used.
fn eval_defstruct(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_spec, slots_form) = cp(cdr);
    let name_sym = if name_spec.is_cons() {
        cp(name_spec).0
    } else {
        name_spec
    };
    let name_str = symbol_bare_name(&sym_name(name_sym));

    // Parse each slot into (slot-symbol, default-form, accessor-symbol, initarg-keyword).
    struct StructSlot {
        slot_sym: BlissVal,
        default: BlissVal,
        accessor: BlissVal,
        initarg: BlissVal,
    }
    let mut slots: Vec<StructSlot> = Vec::new();
    for slot_form in list_to_vec(slots_form) {
        let (slot_sym, default) = if slot_form.is_cons() {
            let (sn, rest) = cp(slot_form);
            (sn, if rest.is_cons() { cp(rest).0 } else { NIL })
        } else {
            (slot_form, NIL)
        };
        if !slot_sym.is_symbol() {
            continue;
        }
        let slot_str = symbol_bare_name(&sym_name(slot_sym));
        slots.push(StructSlot {
            slot_sym,
            default,
            accessor: resolve_sym(&format!("{}-{}", name_str, slot_str)).unwrap_or(NIL),
            initarg: resolve_sym(&format!(":{}", slot_str)).unwrap_or(NIL),
        });
    }

    let sym = |name: &str| resolve_sym(name).unwrap_or(NIL);
    let quote = |value: BlissVal| vec_to_list(&[sym("QUOTE"), value]);
    let obj = sym("%STRUCT-OBJECT%");

    // (defclass NAME () ((slot :initarg :slot :accessor NAME-slot) ...))
    let slot_clauses: Vec<BlissVal> = slots
        .iter()
        .map(|s| {
            vec_to_list(&[
                s.slot_sym,
                sym(":INITARG"),
                s.initarg,
                sym(":ACCESSOR"),
                s.accessor,
            ])
        })
        .collect();
    let defclass_form = vec_to_list(&[sym("DEFCLASS"), name_sym, NIL, vec_to_list(&slot_clauses)]);
    eval_form(defclass_form, env)?;

    // (defun make-NAME (&key (slot default) ...) (make-instance 'NAME :slot slot ...))
    let mut ctor_params = vec![sym("&KEY")];
    for s in &slots {
        ctor_params.push(vec_to_list(&[s.slot_sym, s.default]));
    }
    let mut make_call = vec![sym("MAKE-INSTANCE"), quote(name_sym)];
    for s in &slots {
        make_call.push(s.initarg);
        make_call.push(s.slot_sym);
    }
    let ctor_defun = vec_to_list(&[
        sym("DEFUN"),
        sym(&format!("MAKE-{}", name_str)),
        vec_to_list(&ctor_params),
        vec_to_list(&make_call),
    ]);
    eval_form(ctor_defun, env)?;

    // (defun NAME-P (o) (typep o 'NAME))
    let pred_defun = vec_to_list(&[
        sym("DEFUN"),
        sym(&format!("{}-P", name_str)),
        vec_to_list(&[obj]),
        vec_to_list(&[sym("TYPEP"), obj, quote(name_sym)]),
    ]);
    eval_form(pred_defun, env)?;

    // (defun copy-NAME (o) (make-NAME :slot (NAME-slot o) ...))
    let mut copy_call = vec![sym(&format!("MAKE-{}", name_str))];
    for s in &slots {
        copy_call.push(s.initarg);
        copy_call.push(vec_to_list(&[s.accessor, obj]));
    }
    let copy_defun = vec_to_list(&[
        sym("DEFUN"),
        sym(&format!("COPY-{}", name_str)),
        vec_to_list(&[obj]),
        vec_to_list(&copy_call),
    ]);
    eval_form(copy_defun, env)?;

    Ok(name_sym)
}

// ── DEFGENERIC ───────────────────────────────────────────────────
fn eval_defgeneric(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, options) = cp(cdr);
    let name = function_name_key(name_form);
    let mut combination = bliss_stdlib::MethodCombinationType::Standard;
    // `(:method qualifier* specialized-lambda-list body...)` options each define a
    // method; collect their tails so they can be registered after the generic
    // function exists (with its method combination already known).
    let mut method_options: Vec<BlissVal> = Vec::new();
    let mut opts = options;
    while opts.is_cons() {
        let (option, rest) = cp(opts);
        if option.is_cons() {
            let (option_name, option_rest) = cp(option);
            if option_name.is_symbol() {
                match symbol_bare_name(&sym_name(option_name)).as_str() {
                    "METHOD-COMBINATION" => {
                        let method_combination = cp(option_rest).0;
                        combination = method_combination_from_name(&sym_name(method_combination))
                            .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
                    }
                    "METHOD" => method_options.push(option_rest),
                    _ => {}
                }
            }
        }
        opts = rest;
    }
    let generic_function = bliss_stdlib::make_generic_function(name_form, NIL)?;
    env.generics.borrow_mut().insert(
        name.clone(),
        GenericDef {
            generic_function,
            combination,
        },
    );
    env.methods.borrow_mut().entry(name).or_default();

    // Register each :method option by delegating to DEFMETHOD: the option tail
    // `(qualifier* specialized-lambda-list body...)` is exactly a DEFMETHOD cdr
    // once the generic-function name is consed on the front.
    for method_option in method_options {
        let defmethod_cdr = arena_cons(name_form, method_option);
        eval_defmethod(defmethod_cdr, env)?;
    }
    Ok(name_form)
}

// ── DEFMETHOD ────────────────────────────────────────────────────
fn eval_defmethod(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    // A `(setf place)` method name is a cons; key it as "(SETF PLACE)" so SETF
    // can find the writer generic (bliss-lb6.14).
    let name = function_name_key(name_form);
    let combination = env
        .generics
        .borrow()
        .get(&name)
        .map(|generic| generic.combination)
        .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
    let mut cursor = rest;
    let mut qualifier = bliss_stdlib::MethodQualifier::Primary;
    while cursor.is_cons() {
        let (head, tail) = cp(cursor);
        if head.is_cons() {
            cursor = arena_cons(head, tail);
            break;
        }
        if !head.is_symbol() {
            break;
        }
        match symbol_bare_name(&sym_name(head)).as_str() {
            "AROUND" => qualifier = bliss_stdlib::MethodQualifier::Around,
            "BEFORE" => qualifier = bliss_stdlib::MethodQualifier::Before,
            "AFTER" => qualifier = bliss_stdlib::MethodQualifier::After,
            qualifier_name
                if combination != bliss_stdlib::MethodCombinationType::Standard
                    && method_combination_from_name(qualifier_name) == Some(combination) => {}
            _ => break,
        }
        cursor = tail;
    }
    let (spec_params_form, body) = cp(cursor);
    let method_id = next_stdlib_class_id();

    // Parse the specialized lambda list. Required parameters (before any
    // lambda-list keyword) may carry specializers `(var class)` / `(var (eql v))`;
    // once a keyword such as &optional/&rest/&key/&aux is seen, the remaining
    // parameters are ordinary (unspecialized) and are passed through verbatim so
    // the standard lambda-list binder handles them.
    let params_list = list_to_vec(spec_params_form);
    let mut specializers = Vec::new();
    let mut plain_params: Vec<BlissVal> = Vec::new();
    let mut past_required = false;

    for p in &params_list {
        if p.is_symbol() {
            let bare = symbol_bare_name(&sym_name(*p));
            if bare.starts_with('&') {
                past_required = true;
                plain_params.push(*p);
                continue;
            }
            plain_params.push(*p);
            if !past_required {
                specializers.push(MethodSpecializer::Any);
            }
        } else if p.is_cons() {
            if past_required {
                // &optional/&key parameter with a default form, e.g. (y 10).
                plain_params.push(*p);
                continue;
            }
            let (var_form, rest_p) = cp(*p);
            plain_params.push(var_form);
            let (class_form, _) = cp(rest_p);
            if class_form.is_cons() {
                let (head, value_rest) = cp(class_form);
                if head.is_symbol() && symbol_bare_name(&sym_name(head)) == "EQL" {
                    specializers.push(MethodSpecializer::Eql(eval_form(cp(value_rest).0, env)?));
                } else {
                    specializers.push(MethodSpecializer::Class(sym_name(class_form)));
                }
            } else {
                let specializer_name = sym_name(class_form);
                if symbol_bare_name(&specializer_name) == "T" {
                    specializers.push(MethodSpecializer::Any);
                } else {
                    specializers.push(MethodSpecializer::Class(specializer_name));
                }
            }
        }
    }
    let lambda_list = vec_to_list(&plain_params);

    let generic_function = if let Some(generic) = env.generics.borrow().get(&name).cloned() {
        generic.generic_function
    } else {
        let gf = bliss_stdlib::make_generic_function(name_form, NIL)?;
        env.generics.borrow_mut().insert(
            name.clone(),
            GenericDef {
                generic_function: gf,
                combination: bliss_stdlib::MethodCombinationType::Standard,
            },
        );
        gf
    };
    bliss_stdlib::clos::add_method(generic_function, method_id)?;
    bliss_stdlib::set_method_specializers(method_id, vec![], qualifier);

    env.methods
        .borrow_mut()
        .entry(name.clone())
        .or_default()
        .push(MethodDef {
            method_id,
            specializers,
            lambda_list,
            qualifier,
            body,
        });
    Ok(name_form)
}

// ── MAKE-INSTANCE ────────────────────────────────────────────────
fn eval_make_instance(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (class_form, init_args) = cp(cdr);
    let class_input = eval_form(class_form, env)?;
    let class = resolve_class_metaobject(env, class_input)?;
    let class_name = class_name_for_instance_class(class);
    let mut initargs = evaluated_initargs(&class_name, init_args, env)?;
    // Keep initargs rooted across make_instance, the initforms, and the user
    // :after methods below — all of which allocate (bliss-6b2 #2).
    let _ig = VecRootGuard::new(&mut initargs);
    let explicit_slots: Vec<String> = initargs
        .chunks_exact(2)
        .map(|pair| symbol_bare_name(&sym_name(pair[0])))
        .collect();
    let (instance_initargs, class_initargs) = split_initargs_for_class(env, &class_name, &initargs);
    let instance = bliss_stdlib::make_instance(class, &instance_initargs)?;
    for (slot_name, value) in class_initargs {
        write_class_slot_value(env, &class_name, &slot_name, Some(value));
    }
    apply_class_initforms(instance, &class_name, env, None, &explicit_slots)?;

    // Run user-defined :after methods on the initialization protocol so the
    // canonical `(defmethod initialize-instance :after ...)` hook fires. Per
    // ANSI, shared-initialize's :after methods run inside the initialize-instance
    // primary, hence before initialize-instance's own :after methods.
    // initialize-instance is called as (instance &rest initargs); shared-initialize
    // as (instance slot-names &rest initargs) with slot-names = T (all slots).
    let mut ii_args = Vec::with_capacity(initargs.len() + 1);
    ii_args.push(instance);
    ii_args.extend_from_slice(&initargs);
    let _iig = VecRootGuard::new(&mut ii_args);
    let mut si_args = Vec::with_capacity(initargs.len() + 2);
    si_args.push(instance);
    si_args.push(T);
    si_args.extend_from_slice(&initargs);
    let _sig = VecRootGuard::new(&mut si_args);
    run_initialization_aux_methods(
        env,
        "SHARED-INITIALIZE",
        &si_args,
        bliss_stdlib::MethodQualifier::After,
    )?;
    run_initialization_aux_methods(
        env,
        "INITIALIZE-INSTANCE",
        &ii_args,
        bliss_stdlib::MethodQualifier::After,
    )?;
    Ok(instance)
}

// ── Apply function (lambda or named) ─────────────────────────────
/// Faithful direct dispatch of the hot numeric/comparison builtins on
/// already-evaluated args, reusing the SAME cores as operator-position dispatch
/// (`fold_arith_vals`, `sub_vals`, `numeric_cmp`). Returns `None` for any name
/// outside this set so behaviour is unchanged for everything else. This lets a
/// bytecode/native function's `+`/`-`/`<`/… calls (routed through c2i →
/// apply_function) skip the synthesize-`(name 'a 'b)`-and-re-evaluate detour
/// (bliss-x5y.8) without the float-arithmetic divergence that ruled out
/// `apply_builtin` (bliss-x5y.9).
fn apply_numeric_op(name: &str, args: &[BlissVal]) -> Option<Result<BlissVal, BlissError>> {
    // Comparisons are binary in bliss's operator dispatch (eval_cmp / `=`); only
    // fast-path the 2-arg shape so other arities match the general path exactly.
    let cmp = |pred: fn(Ordering) -> bool| -> Result<BlissVal, BlissError> {
        Ok(if pred(numeric_cmp(args[0], args[1])?) {
            T
        } else {
            NIL
        })
    };
    Some(match name {
        "+" => fold_arith_vals(args, 0, 0.0, |a, b| a + b, bigrat_add, |a, b| a + b),
        "*" => fold_arith_vals(args, 1, 1.0, |a, b| a * b, bigrat_mul, |a, b| a * b),
        "-" => sub_vals(args),
        "<" if args.len() == 2 => cmp(|o| o == Ordering::Less),
        ">" if args.len() == 2 => cmp(|o| o == Ordering::Greater),
        "<=" if args.len() == 2 => cmp(|o| o != Ordering::Greater),
        ">=" if args.len() == 2 => cmp(|o| o != Ordering::Less),
        "=" if args.len() == 2 => cmp(|o| o == Ordering::Equal),
        "/=" if args.len() == 2 => cmp(|o| o != Ordering::Equal),
        _ => return None,
    })
}

fn apply_function(
    fn_val: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    // Function could be a lambda form, a symbol naming a function, or a closure
    if fn_val.is_symbol() {
        let name = sym_name(fn_val);
        if let Some((params_form, body)) = callable_body(env, &name) {
            // Mirror the operator-position path: a GLOBAL function with a
            // registered bytecode body (e.g. one installed from a `.bfasl`, whose
            // interpreted-function object carries a NIL body) must dispatch
            // through the promoting bytecode/native path, not run its empty
            // fallback body. Dispatch through the resolved object's OWN name so
            // an aliased installation — `(setf (symbol-function 'p) #'q)` — routes
            // to q's registered bytecode rather than the alias p, which carries no
            // registry entry and whose interpreted body is the empty shell
            // (bliss-57m). Guard on no lexical FLET/LABELS shadow so a local
            // binding is never redirected to the global registry entry.
            if !env.funs.borrow().contains_key(&name) {
                let dispatch_sym = global_fn(&name)
                    .map(bliss_rt::function::name)
                    .filter(|n| n.is_symbol())
                    .or_else(|| resolve_sym(&name));
                if let Some(sym) = dispatch_sym {
                    if let Some(res) =
                        bytecode::call_registered(sym.as_symbol_index(), args, sym, env)
                    {
                        return res;
                    }
                }
            }
            return eval_lambda_call(env, params_form, body, args, Rc::clone(&env.frame));
        }
        if env.generics.borrow().contains_key(&name) || env.methods.borrow().contains_key(&name) {
            return invoke_generic_function(&name, args, env);
        }
        // Faithful fast path: the hot numeric/comparison builtins dispatch
        // directly on the evaluated args through the SAME cores as operator
        // position (bliss-x5y.8). This is what every +/-/< a bytecode/native
        // function calls through c2i takes, avoiding the synthesize-and-
        // re-evaluate detour below. Unlike `apply_builtin` it is bit-identical
        // to the tree-walker (bliss-x5y.9), so tiers stay consistent.
        if let Some(res) = apply_numeric_op(&name, args) {
            // These builtins yield exactly one value; reset the multiple-values
            // state so a caller's stale MV (e.g. from an arg that was `(values
            // …)`) does not leak, matching the operator-position path the old
            // synthesize-and-eval detour went through (bliss-x5y.8).
            env.clear_mv();
            return res;
        }
        // These structural primitives already have evaluated arguments here.
        // Dispatch them directly instead of allocating quoted argument forms
        // and re-entering eval_form. The old bridge allocated four temporary
        // conses for every unary call, which made shallow recursive list walks
        // consume memory in proportion to total calls (bliss-jql).
        if matches!(
            name.as_str(),
            "CAR" | "FIRST" | "CDR" | "REST" | "NULL" | "NOT" | "CONSP" | "ATOM" | "LISTP"
        ) {
            env.clear_mv();
            return apply_builtin(&name, args, env);
        }
        // Builtin: synthesize `(name 'arg1 'arg2 ...)` and evaluate it so the
        // full operator-position builtin set (not just apply_builtin's subset)
        // is reachable through funcall/apply/mapcar.
        let roots = bliss_rt::ShadowRootScope::new();
        let fn_val = roots.root(fn_val);
        let args = roots.root_values(args.iter().copied());
        let quote_sym = quote_sym();
        let mut items = Vec::with_capacity(args.len() + 1);
        items.push(roots.root(fn_val.get()));
        for arg in &args {
            // Keywords are self-evaluating and several operator-position
            // builtin parsers inspect the raw argument form to recognize &key
            // pairs (SORT :KEY, MEMBER :TEST, MAKE-PATHNAME, ...). Quoting a
            // keyword produced `(QUOTE :KEY)`, which evaluates equivalently but
            // hid it from those parsers. Preserve keyword forms directly while
            // quoting every other already-evaluated value.
            if is_keyword_arg(arg.get()) {
                items.push(roots.root(arg.get()));
            } else {
                let quoted_arg = roots.root(arena_cons(arg.get(), NIL));
                items.push(roots.root(arena_cons(quote_sym, quoted_arg.get())));
            }
        }
        let form_items: Vec<_> = items.iter().map(|item| item.get()).collect();
        let form = vec_to_list(&form_items);
        return eval_form(form, env);
    }
    if fn_val.is_cons() {
        let (lh, lr) = cp(fn_val);
        // Check for closure: (BLISS::CLOSURE . id)
        if lh.is_symbol() && sym_name(lh) == "BLISS::CLOSURE" && lr.is_fixnum() {
            let id = lr.as_fixnum() as u64;
            let closure = { env.closures.borrow().get(&id).cloned() };
            if let Some(closure) = closure {
                return eval_lambda_call(
                    env,
                    closure.params_form,
                    closure.body,
                    args,
                    Rc::clone(&closure.captured_frame),
                );
            }
        }
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body) = cp(lr);
            return eval_lambda_call(env, params_form, body, args, Rc::clone(&env.frame));
        }
    }
    // A heap interpreted-function object, e.g. from FDEFINITION / SYMBOL-FUNCTION
    // or a #' on a global defun. Call it by its own lambda list and body.
    if fn_val.is_heap_object() && bliss_rt::function::is_interpreted_function(fn_val) {
        bliss_rt::function::record_invocation(fn_val);
        // Function-object calls participate in the same tiering dispatch as
        // operator-position symbol calls. ASDF commonly retains a function
        // object and invokes it through FUNCALL/MAP; merely bumping FnMeta here
        // left such hot functions permanently at tier 0 even when bytecode was
        // registered for their global name.
        let function_name = bliss_rt::function::name(fn_val);
        if function_name.is_symbol() {
            if let Some(result) =
                bytecode::call_registered(function_name.as_symbol_index(), args, fn_val, env)
            {
                return result;
            }
        }
        let params_form = bliss_rt::function::lambda_list(fn_val);
        let body = bliss_rt::function::body(fn_val);
        return eval_lambda_call(env, params_form, body, args, Rc::clone(&env.frame));
    }
    Err(BlissError::Internal(format!("Cannot apply: {:?}", fn_val)))
}

/// True if `name` (a bare, upcased function name) denotes a standard function
/// bliss implements as a builtin operator. Used by FBOUNDP/FDEFINITION so a
/// builtin like FUNCALL is reported bound and `(fdefinition 'funcall)` returns a
/// callable designator — ASDF's ENSURE-FUNCTION relies on this. Special
/// operators and macros are intentionally excluded (they are not functions).
fn is_builtin_function(name: &str) -> bool {
    matches!(
        name,
        // Introspection / devtools
        "DISASSEMBLE"
            // Control / function application
            | "FUNCALL" | "APPLY" | "VALUES" | "VALUES-LIST" | "IDENTITY" | "COMPLEMENT"
            | "CONSTANTLY" | "NOT" | "EQ" | "EQL" | "EQUAL" | "EQUALP"
            // Conses / lists
            | "CONS" | "CAR" | "CDR" | "FIRST" | "REST" | "SECOND" | "THIRD" | "FOURTH"
            | "FIFTH" | "LAST" | "LIST" | "LIST*" | "APPEND" | "NCONC" | "REVERSE"
            | "NREVERSE" | "NTH" | "NTHCDR" | "CAAR" | "CADR" | "CDAR" | "CDDR"
            | "CADDR" | "CADDDR" | "COPY-LIST" | "COPY-TREE" | "LDIFF" | "TAILP"
            | "CONSP" | "ATOM" | "LISTP" | "NULL" | "ENDP" | "ACONS" | "ASSOC"
            | "RASSOC" | "MEMBER" | "MEMBER-IF" | "ASSOC-IF" | "GETF" | "GET"
            | "SUBST" | "PAIRLIST" | "PAIRLIS" | "REVAPPEND" | "NRECONC" | "BUTLAST"
            | "NBUTLAST" | "MAPCAR" | "MAPC" | "MAPCAN" | "MAPCON" | "MAPLIST" | "MAPL"
            | "SET-DIFFERENCE" | "UNION" | "INTERSECTION" | "ADJOIN"
            // Sequences
            | "ELT" | "LENGTH" | "SUBSEQ" | "COPY-SEQ" | "AREF" | "SVREF" | "ROW-MAJOR-AREF"
            | "BIT" | "SBIT"
            | "MAP" | "MAP-INTO" | "REDUCE" | "COUNT" | "COUNT-IF" | "FIND" | "FIND-IF"
            | "POSITION" | "POSITION-IF" | "REMOVE" | "REMOVE-IF" | "REMOVE-IF-NOT"
            | "REMOVE-DUPLICATES" | "DELETE" | "DELETE-IF" | "DELETE-DUPLICATES"
            | "SUBSTITUTE" | "SUBSTITUTE-IF" | "FILL" | "SORT" | "STABLE-SORT" | "MERGE"
            | "SEARCH" | "MISMATCH" | "CONCATENATE" | "EVERY" | "SOME" | "NOTEVERY"
            | "NOTANY" | "VECTOR" | "MAKE-ARRAY" | "MAKE-LIST" | "MAKE-SEQUENCE"
            | "VECTORP" | "SIMPLE-VECTOR-P" | "ARRAYP" | "ARRAY-DIMENSIONS"
            | "ARRAY-DIMENSION" | "ARRAY-TOTAL-SIZE" | "VECTOR-PUSH" | "VECTOR-PUSH-EXTEND"
            | "VECTOR-POP" | "FILL-POINTER" | "%MAKE-COMPLEX-VECTOR"
            | "ADJUSTABLE-ARRAY-P" | "ARRAY-HAS-FILL-POINTER-P" | "ARRAY-DISPLACEMENT"
            // Numbers
            | "+" | "-" | "*" | "/" | "1+" | "1-" | "=" | "/=" | "<" | ">" | "<=" | ">="
            | "MIN" | "MAX" | "ABS" | "MOD" | "REM" | "FLOOR" | "CEILING" | "TRUNCATE"
            | "ROUND" | "GCD" | "LCM" | "EXPT" | "SQRT" | "ISQRT" | "SIGNUM" | "FLOAT"
            | "ZEROP" | "PLUSP" | "MINUSP" | "ODDP" | "EVENP" | "NUMBERP" | "INTEGERP"
            | "FLOATP" | "RATIONALP" | "REALP" | "NUMERATOR" | "DENOMINATOR"
            | "LOGAND" | "LOGIOR" | "LOGXOR" | "LOGNOT" | "ASH" | "LOGBITP" | "BOOLE"
            | "INTEGER-LENGTH" | "RANDOM" | "EXP" | "LOG" | "SIN" | "COS" | "TAN"
            // Characters
            | "CHAR" | "CHAR-CODE" | "CODE-CHAR" | "CHAR-UPCASE" | "CHAR-DOWNCASE"
            | "CHARACTERP" | "CHAR=" | "CHAR<" | "CHAR>" | "CHAR<=" | "CHAR>=" | "CHAR/="
            | "ALPHA-CHAR-P" | "DIGIT-CHAR-P" | "ALPHANUMERICP" | "UPPER-CASE-P"
            | "LOWER-CASE-P" | "CHAR-EQUAL" | "DIGIT-CHAR" | "CHAR-INT"
            // Strings
            | "STRING" | "STRING=" | "STRING<" | "STRING>" | "STRING<=" | "STRING>="
            | "STRING/=" | "STRING-EQUAL" | "STRING-UPCASE" | "STRING-DOWNCASE"
            | "STRING-CAPITALIZE" | "STRING-TRIM" | "STRING-LEFT-TRIM" | "STRING-RIGHT-TRIM"
            | "STRINGP" | "CHAR-NAME" | "NAME-CHAR" | "PARSE-INTEGER" | "MAKE-STRING"
            | "STRING-TO-LIST"
            // Symbols / packages
            | "SYMBOLP" | "KEYWORDP" | "CONSTANTP" | "%MARK-CONSTANT"
            | "%DEFINE-CONDITION-PORTABLE" | "SYMBOL-NAME"
            | "SYMBOL-VALUE" | "SYMBOL-FUNCTION"
            | "SYMBOL-PACKAGE" | "SYMBOL-PLIST" | "MAKE-SYMBOL" | "GENSYM" | "GENTEMP"
            | "INTERN" | "FIND-SYMBOL" | "FIND-PACKAGE" | "PACKAGE-NAME" | "PACKAGEP"
            | "MAKE-PACKAGE" | "PACKAGE-NAMES" | "PACKAGE-NICKNAMES"
            | "PACKAGE-SHADOWING-SYMBOLS" | "PACKAGE-USED-BY-LIST" | "PACKAGE-USE-LIST"
            | "PACKAGE-SYMBOLS"
            | "USE-PACKAGE" | "UNUSE-PACKAGE" | "RENAME-PACKAGE" | "DELETE-PACKAGE"
            | "EXPORT" | "UNEXPORT" | "IMPORT" | "SHADOWING-IMPORT" | "SHADOW"
            | "UNINTERN"
            | "BOUNDP" | "FBOUNDP" | "FDEFINITION" | "MAKUNBOUND" | "FMAKUNBOUND"
            | "SET" | "FUNCTIONP" | "COMPILED-FUNCTION-P" | "SPECIAL-OPERATOR-P"
            | "COERCE" | "TYPE-OF" | "TYPEP" | "SUBTYPEP" | "DOCUMENTATION"
            // Hash tables
            | "MAKE-HASH-TABLE" | "GETHASH" | "REMHASH" | "CLRHASH" | "MAPHASH"
            | "HASH-TABLE-COUNT" | "HASH-TABLE-P" | "HASH-TABLE-KEYS" | "HASH-TABLE-VALUES"
            // Pathnames / files
            | "PATHNAME" | "NAMESTRING" | "MERGE-PATHNAMES" | "MAKE-PATHNAME"
            | "PATHNAME-NAME" | "PATHNAME-TYPE" | "PATHNAME-DIRECTORY" | "PATHNAME-HOST"
            | "PATHNAME-DEVICE" | "PATHNAME-VERSION" | "PATHNAMEP" | "PARSE-NAMESTRING"
            | "PROBE-FILE" | "TRUENAME" | "DIRECTORY" | "WILD-PATHNAME-P"
            | "PATHNAME-MATCH-P" | "TRANSLATE-PATHNAME" | "ENSURE-DIRECTORIES-EXIST"
            | "FILE-NAMESTRING" | "DIRECTORY-NAMESTRING" | "ENOUGH-NAMESTRING"
            | "COMPILE-FILE" | "COMPILE-FILE-PATHNAME" | "FILE-WRITE-DATE"
            // Time
            | "GET-UNIVERSAL-TIME" | "ENCODE-UNIVERSAL-TIME" | "DECODE-UNIVERSAL-TIME"
            // I/O
            | "PRINT" | "PRIN1" | "PRINC" | "WRITE" | "WRITE-STRING" | "WRITE-LINE"
            | "WRITE-CHAR" | "TERPRI" | "FRESH-LINE" | "READ" | "READ-LINE" | "READ-CHAR"
            | "READ-FROM-STRING" | "FORMAT" | "PRIN1-TO-STRING" | "PRINC-TO-STRING"
            | "WRITE-TO-STRING" | "FORCE-OUTPUT" | "FINISH-OUTPUT" | "CLEAR-OUTPUT"
            | "FILE-LENGTH" | "READ-SEQUENCE" | "WRITE-SEQUENCE"
            // Misc
            | "ERROR" | "WARN" | "SIGNAL" | "CERROR" | "MAKE-CONDITION" | "MUFFLE-WARNING"
            | "INVOKE-RESTART" | "FIND-RESTART" | "COMPUTE-RESTARTS" | "ABORT" | "CONTINUE"
            | "CLASS-OF" | "CLASS-NAME" | "FIND-CLASS" | "SLOT-VALUE" | "SLOT-BOUNDP"
            | "MAKE-INSTANCE" | "COPY-STRUCTURE"
    )
}

fn apply_builtin(name: &str, args: &[BlissVal], _env: &mut Env) -> Result<BlissVal, BlissError> {
    match name {
        // CL:DISASSEMBLE — show the function's current tier: annotated bytecode
        // while interpreted (T0), decoded x86-64 once promoted to native (T1).
        "DISASSEMBLE" => {
            let listing = args.first().and_then(|a| {
                if a.is_symbol() {
                    bytecode::disassemble_by_symbol(a.as_symbol_index())
                } else {
                    None
                }
            });
            match listing {
                Some(s) => print!("{s}"),
                None => println!(
                    "; DISASSEMBLE: not a compiled Bliss function (a builtin or interpreted closure)"
                ),
            }
            Ok(NIL)
        }
        // Use the shared int/rational cores, NOT an f64 accumulator: the old
        // float arithmetic here lost precision above 2^53 and never promoted to
        // bignum, diverging from operator-position dispatch (bliss-x5y.9).
        "+" => fold_arith_vals(args, 0, 0.0, |a, b| a + b, bigrat_add, |a, b| a + b),
        "-" => sub_vals(args),
        "*" => fold_arith_vals(args, 1, 1.0, |a, b| a * b, bigrat_mul, |a, b| a * b),
        "CONS" => {
            if args.len() >= 2 {
                Ok(arena_cons(args[0], args[1]))
            } else {
                Err(BlissError::Internal("CONS requires 2 arguments".into()))
            }
        }
        "CAR" | "FIRST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (a, _) = cp(v);
                return Ok(a);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        "CDR" | "REST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (_, d) = cp(v);
                return Ok(d);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        "NULL" | "NOT" => Ok(if args.first().copied().unwrap_or(NIL).is_nil() {
            T
        } else {
            NIL
        }),
        "CONSP" => Ok(if args.first().copied().unwrap_or(NIL).is_cons() {
            T
        } else {
            NIL
        }),
        "ENDP" => {
            // (endp list) — T at the end of a proper list (NIL), NIL for a cons,
            // and a type error for a non-list (CLHS: endp is defined only on
            // lists).
            let arg = args.first().copied().unwrap_or(NIL);
            if arg.is_nil() {
                Ok(T)
            } else if arg.is_cons() {
                Ok(NIL)
            } else {
                Err(BlissError::TypeError {
                    datum: arg,
                    expected: "list".into(),
                })
            }
        }
        "ATOM" => Ok(if args.first().copied().unwrap_or(NIL).is_cons() {
            NIL
        } else {
            T
        }),
        "LISTP" => Ok(if args.first().copied().unwrap_or(NIL).is_list() {
            T
        } else {
            NIL
        }),
        _ => Err(BlissError::UndefinedFunction(
            resolve_sym(name).unwrap_or(NIL),
        )),
    }
}

// ── FLOOR ────────────────────────────────────────────────────────
/// The remainder for a division whose quotient is `q`: an exact integer when
/// both operands are fixnums (ANSI: integer args give an integer remainder),
/// otherwise a float. Fixes the FLOOR/CEILING/TRUNCATE/ROUND second value.
fn integer_or_float_remainder(a: BlissVal, b: BlissVal, q: i64, av: f64, bv: f64) -> BlissVal {
    if a.is_fixnum() && b.is_fixnum() {
        BlissVal::from_fixnum(a.as_fixnum() - q * b.as_fixnum())
    } else {
        BlissVal::from_single_float((av - (q as f64) * bv) as f32)
    }
}

/// First character index at which strings `a` and `b` differ, plus which is
/// greater there. If one is a proper prefix of the other, the index is the
/// shorter length; `Equal` means identical. Char-based (not byte-based) so it is
/// correct for non-ASCII. Used by STRING</STRING> to return the ANSI mismatch
/// index rather than a boolean.
fn string_mismatch(a: &str, b: &str) -> (usize, std::cmp::Ordering) {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let n = av.len().min(bv.len());
    for i in 0..n {
        if av[i] != bv[i] {
            return (i, av[i].cmp(&bv[i]));
        }
    }
    (n, av.len().cmp(&bv.len()))
}

/// Round to nearest integer, ties to even — ANSI CL ROUND semantics, unlike
/// Rust's `f64::round` (ties away from zero). `(round 5 2)` = 2, `(round 7 2)` = 4.
fn round_half_even(x: f64) -> i64 {
    if (x - x.trunc()).abs() == 0.5 {
        let lower = x.floor() as i64;
        if lower % 2 == 0 { lower } else { lower + 1 }
    } else {
        x.round() as i64
    }
}

fn eval_floor(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let args = eval_args(cdr, env)?;
    if args.is_empty() {
        return Err(BlissError::Internal("FLOOR requires an argument".into()));
    }
    let a = args[0];
    let av = num_val(a)?;
    if args.len() >= 2 {
        let b = args[1];
        let bv = num_val(b)?;
        if bv == 0.0 {
            return Err(BlissError::ArithmeticError("division by zero".into()));
        }
        let q = (av / bv).floor() as i64;
        let rem = integer_or_float_remainder(a, b, q, av, bv);
        env.set_mv(vec![BlissVal::from_fixnum(q), rem]);
        return Ok(BlissVal::from_fixnum(q));
    }
    let q = av.floor() as i64;
    let rem = av - q as f64;
    env.set_mv(vec![
        BlissVal::from_fixnum(q),
        BlissVal::from_single_float(rem as f32),
    ]);
    Ok(BlissVal::from_fixnum(q))
}

// ── MULTIPLE-VALUE-BIND ─────────────────────────────────────────
fn eval_multiple_value_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (vars_form, rest) = cp(cdr);
    let (values_form, body) = cp(rest);

    // Evaluate the values form
    env.clear_mv();
    let primary = eval_form(values_form, env)?;
    let mv = env.mv.clone();

    // Bind variables in a fresh frame on the SAME env (see eval_let) so that
    // global definitions in the body — e.g. INTERN inside UIOP's ENSURE-SYMBOL,
    // which runs under two nested MULTIPLE-VALUE-BINDs — persist to the caller.
    let var_names: Vec<String> = list_to_vec(vars_form)
        .iter()
        .map(|v| sym_name(*v))
        .collect();
    let parent = Rc::clone(&env.frame);
    with_child_frame(env, parent, move |env| {
        for (i, var_name) in var_names.iter().enumerate() {
            let val = if i == 0 {
                primary
            } else if i < mv.len() {
                mv[i]
            } else {
                NIL
            };
            env.define_local(var_name, val);
        }
        eval_progn(body, env)
    })
}

// ── HANDLER-CASE ────────────────────────────────────────────────
fn eval_handler_case(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (protected_form, clauses) = cp(cdr);
    let base_len = env.handlers.len();
    let mut installed = Vec::new();
    let mut c = clauses;
    while c.is_cons() {
        let (clause, rest) = cp(c);
        let (type_form, clause_rest) = cp(clause);
        let (bind_list, handler_body) = cp(clause_rest);
        let token = next_control_token("handler-case");
        let var_name = if bind_list.is_cons() {
            Some(sym_name(cp(bind_list).0))
        } else {
            None
        };
        let entry = HandlerEntry {
            type_name: sym_name(type_form),
            handler: HandlerImpl::HandlerCase {
                token: token.clone(),
                var_name: var_name.clone(),
                body: handler_body,
                captured_frame: Rc::clone(&env.frame),
            },
        };
        installed.push(entry);
        c = rest;
    }

    // One HANDLER-CASE form is one cluster, its clauses held in source order.
    // SIGNAL tries a cluster's entries front-to-back, so the first matching
    // clause wins (ANSI CL) — no reverse needed.
    env.handlers.push(HandlerCluster {
        entries: installed.clone(),
    });

    let result = eval_form(protected_form, env);
    env.handlers.truncate(base_len);

    match result {
        Ok(val) => Ok(val),
        Err(error) => {
            // A condition signalled through HANDLER-CASE's own handlers arrives as
            // a control token naming the selected clause.
            if let Some(token) = handler_case_token(&error) {
                let condition = take_control_value(&token);
                for handler in installed {
                    if let HandlerImpl::HandlerCase {
                        token: entry_token,
                        var_name,
                        body,
                        captured_frame,
                    } = handler.handler
                    {
                        if entry_token == token {
                            let mut handler_env = env.child_with_parent(captured_frame);
                            if let Some(name) = var_name {
                                handler_env.define_local(&name, condition);
                            }
                            return eval_progn(body, &mut handler_env);
                        }
                    }
                }
                return Err(error);
            }
            // A raw runtime error (TYPE-ERROR, UNBOUND-VARIABLE,
            // UNDEFINED-FUNCTION, arithmetic, …) is a signalable CL condition
            // too. Build the condition and let the first clause whose type
            // matches handle it, so HANDLER-CASE catches system errors — not only
            // those raised through SIGNAL/ERROR. Errors that are not conditions
            // (control-flow tokens, Shutdown) yield None and propagate unchanged.
            if let Ok(Some(condition)) = bliss_error_to_condition(env, &error) {
                for handler in installed {
                    if let HandlerImpl::HandlerCase {
                        var_name,
                        body,
                        captured_frame,
                        ..
                    } = handler.handler
                    {
                        if condition_matches_handler(env, condition, &handler.type_name) {
                            let mut handler_env = env.child_with_parent(captured_frame);
                            if let Some(name) = var_name {
                                handler_env.define_local(&name, condition);
                            }
                            return eval_progn(body, &mut handler_env);
                        }
                    }
                }
            }
            Err(error)
        }
    }
}

// ── HANDLER-BIND ────────────────────────────────────────────────
fn eval_handler_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let base_len = env.handlers.len();

    // Parse all bindings into one cluster (this HANDLER-BIND form), in source
    // order. Establishing the cluster is a single push.
    let mut entries = Vec::new();
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        let (type_form, handler_rest) = cp(binding);
        let (handler_form, _) = cp(handler_rest);
        // Per ANSI, each handler spec is a FORM evaluated (in this lexical
        // environment) to produce the handler function — e.g. `#'(lambda (c) …)`
        // must become a closure that captures its surroundings, not the raw
        // `(FUNCTION (LAMBDA …))` list (which apply cannot call). Evaluate it now.
        let handler_fn = eval_form(handler_form, env)?;
        entries.push(HandlerEntry {
            type_name: sym_name(type_form),
            handler: HandlerImpl::Function(handler_fn),
        });
        c = rest;
    }
    env.handlers.push(HandlerCluster { entries });
    let cluster_index = base_len;

    let result = eval_progn(body, env);

    // Conditions raised through SIGNAL/ERROR already ran the handler stack at
    // signal time (signal_condition_object). Raw evaluator errors — TYPE-ERROR
    // from (car 5), UNBOUND-VARIABLE, PROGRAM-ERROR, arithmetic, … — do not pass
    // through that machinery, so give the handlers this HANDLER-BIND established
    // their turn now, on the unwind: build the condition the error denotes and
    // run our handlers newest-first. A handler that declines (returns) lets the
    // original error keep propagating, so enclosing frames still see it; a
    // handler that transfers control (INVOKE-RESTART, non-local exit) surfaces as
    // a different error, which we propagate instead. Non-condition errors
    // (control tokens, Shutdown) convert to None and are left untouched.
    //
    // Because a raw error unwinds the interpreter stack before reaching here, a
    // handler can only invoke restarts that ENCLOSE this HANDLER-BIND; a restart
    // established inside its body is already gone. The fully general fix is to
    // signal raw errors at generation time (bliss-qry) — a runtime-model change.
    let outcome = match result {
        Ok(value) => Ok(value),
        Err(error) => match bliss_error_to_condition(env, &error) {
            Ok(Some(condition)) => match run_handler_bind_handlers(env, condition, cluster_index) {
                Ok(()) => Err(error),
                Err(transfer) => Err(transfer),
            },
            _ => Err(error),
        },
    };

    env.handlers.truncate(base_len);
    outcome
}

fn parse_restart_options(
    option_forms: BlissVal,
    captured_frame: &Rc<RefCell<EnvFrame>>,
) -> (Option<RestartFunction>, Option<RestartFunction>) {
    let options = list_to_vec(option_forms);
    let mut interactive_function = None;
    let mut test_function = None;
    let mut index = 0;
    while index + 1 < options.len() {
        let key = options[index];
        let value = options[index + 1];
        if key.is_symbol() {
            match symbol_bare_name(&sym_name(key)).as_str() {
                "INTERACTIVE-FUNCTION" => {
                    interactive_function = Some(RestartFunction::FunctionForm {
                        function_form: value,
                        captured_frame: captured_frame.clone(),
                    });
                }
                "TEST-FUNCTION" => {
                    test_function = Some(RestartFunction::FunctionForm {
                        function_form: value,
                        captured_frame: captured_frame.clone(),
                    });
                }
                _ => {}
            }
        }
        index += 2;
    }
    (interactive_function, test_function)
}

// ── RESTART-BIND ────────────────────────────────────────────────
fn eval_restart_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let base_len = env.restarts.len();
    let captured_frame = Rc::clone(&env.frame);
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        let (name_form, binding_rest) = cp(binding);
        let (function_form, option_forms) = cp(binding_rest);
        let (interactive_function, test_function) =
            parse_restart_options(option_forms, &captured_frame);
        env.restarts.push(RestartEntry {
            name: sym_name(name_form).to_uppercase(),
            function: RestartFunction::FunctionForm {
                function_form,
                captured_frame: captured_frame.clone(),
            },
            interactive_function,
            test_function,
            unwind_on_invoke: false,
        });
        c = rest;
    }

    let result = eval_progn(body, env);
    env.restarts.truncate(base_len);
    result
}

// ── RESTART-CASE ────────────────────────────────────────────────
fn eval_restart_case(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (restartable_form, clauses) = cp(cdr);
    let base_len = env.restarts.len();
    let captured_frame = Rc::clone(&env.frame);
    let mut c = clauses;
    while c.is_cons() {
        let (clause, rest) = cp(c);
        let (name_form, clause_rest) = cp(clause);
        let (params_form, body) = cp(clause_rest);
        env.restarts.push(RestartEntry {
            name: sym_name(name_form).to_uppercase(),
            function: RestartFunction::FunctionForm {
                function_form: arena_cons(
                    resolve_sym("LAMBDA").unwrap_or(NIL),
                    arena_cons(params_form, body),
                ),
                captured_frame: captured_frame.clone(),
            },
            interactive_function: None,
            test_function: None,
            unwind_on_invoke: true,
        });
        c = rest;
    }

    let result = eval_form(restartable_form, env);
    env.restarts.truncate(base_len);
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(name) = restart_invoked_name(&error) {
                return Ok(take_control_value(&format!("RESTART-RESULT:{name}")));
            }
            Err(error)
        }
    }
}

// ── CERROR ───────────────────────────────────────────────────────
fn eval_cerror(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (_continue_form, rest) = cp(cdr);
    let (datum_form, arg_forms) = cp(rest);
    let datum = eval_form(datum_form, env)?;
    let args = eval_args(arg_forms, env)?;
    // (cerror continue-control datum &rest args): datum may be a condition
    // instance, a condition-type symbol (built via MAKE-CONDITION), or a
    // format-control string (→ SIMPLE-ERROR).
    let message = if is_string_value(datum) {
        format_control_message(&val_as_str(datum), &args)?
    } else {
        val_as_str(datum)
    };
    let condition = match coerce_condition_designator(env, datum, &args)? {
        Some(condition) => condition,
        None => make_simple_condition("SIMPLE-ERROR", datum, &args, env)?,
    };

    let base_len = env.restarts.len();
    env.restarts.push(RestartEntry {
        name: "CONTINUE".to_string(),
        function: RestartFunction::ContinueNil,
        interactive_function: None,
        test_function: None,
        unwind_on_invoke: true,
    });

    let result = signal_condition_object(condition, env);
    env.restarts.truncate(base_len);
    match result {
        Ok(_) => Err(BlissError::Internal(format!("ERROR: {}", message))),
        Err(error) => {
            if restart_invoked_name(&error).as_deref() == Some("CONTINUE") {
                return Ok(NIL);
            }
            Err(error)
        }
    }
}

// ── WITH-OPEN-FILE ──────────────────────────────────────────────
fn eval_with_open_file(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (binding, body) = cp(cdr);
    let (var_form, rest) = cp(binding);
    let (path_form, opts_rest) = cp(rest);

    let var_name = sym_name(var_form);
    let path_val = eval_form(path_form, env)?;
    let path = val_as_str(path_val);

    // Check sandbox mode
    if env.sandbox {
        return Err(BlissError::SandboxViolation(format!(
            "File access denied in sandbox mode: {}",
            path
        )));
    }

    // Parse options. WITH-OPEN-FILE is a macro whose option VALUES are ordinary
    // forms (e.g. ASDF passes `:direction direction`, a variable), so each value
    // must be EVALUATED — not read as a literal keyword.
    let opts = list_to_vec(opts_rest);
    let mut direction = bliss_stdlib::StreamDirection::Input;
    let mut element_type = T;
    let mut if_exists = T;
    let mut if_does_not_exist = NIL;
    let mut if_dne_supplied = false;
    let mut i = 0;
    while i + 1 < opts.len() {
        let opt_bare = symbol_bare_name(&sym_name(opts[i]));
        let value = eval_form(opts[i + 1], env)?;
        match opt_bare.as_str() {
            "DIRECTION" => match symbol_bare_name(&sym_name(value)).as_str() {
                "OUTPUT" => direction = bliss_stdlib::StreamDirection::Output,
                "IO" => direction = bliss_stdlib::StreamDirection::Io,
                "INPUT" => direction = bliss_stdlib::StreamDirection::Input,
                _ => {}
            },
            "ELEMENT-TYPE" => {
                // (unsigned-byte 8) or the fixnum 8 selects a byte stream.
                let is_byte = value == BlissVal::from_fixnum(8)
                    || (value.is_cons()
                        && symbol_bare_name(&sym_name(cp(value).0)) == "UNSIGNED-BYTE");
                element_type = if is_byte { BlissVal::from_fixnum(8) } else { T };
            }
            "IF-EXISTS" => if_exists = value,
            "IF-DOES-NOT-EXIST" => {
                if_does_not_exist = value;
                if_dne_supplied = true;
            }
            _ => {}
        }
        i += 2;
    }
    // CLHS defaults for `:if-does-not-exist` when unsupplied: `:error` for input
    // (and for output with `:if-exists :overwrite`/`:append`), `:create`
    // otherwise. The stdlib `open` treats any non-NIL value as "signal", and
    // creates missing files on output regardless — so the only default that
    // matters here is input, where NIL must NOT be passed (it would suppress the
    // error and hand the body a NIL stream). Represent `:error` as T.
    if !if_dne_supplied && matches!(direction, bliss_stdlib::StreamDirection::Input) {
        if_does_not_exist = T;
    }

    // Open the file via the standard-library stream machinery.
    let stream_val = bliss_stdlib::open(
        path_val,
        direction,
        element_type,
        if_exists,
        if_does_not_exist,
        bliss_stdlib::ExternalFormat::Utf8,
    )?;

    let parent = Rc::clone(&env.frame);
    // Evaluate body in a fresh frame on the same env, then close the stream
    // (unwind-protect style) whether the body returned or unwound.
    let result = with_child_frame(env, parent, move |env| {
        env.define_local(&var_name, stream_val);
        eval_progn(body, env)
    });
    // `:if-does-not-exist nil` yields a NIL "stream"; there is nothing to close.
    if !stream_val.is_nil() {
        bliss_stdlib::close(stream_val, false)?;
    }
    result
}

// ── DEFPACKAGE ──────────────────────────────────────────────────
fn eval_defpackage(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, opts) = cp(cdr);
    // DEFPACKAGE's name is a package DESIGNATOR and is NOT evaluated (CLHS): a
    // string, or a symbol (interned OR uninterned, e.g. `#:ocicl-runtime`) whose
    // name is used. Evaluating it would look an uninterned symbol up as a
    // variable and signal UNBOUND-VARIABLE.
    let name_raw = if name_form.is_symbol() {
        symbol_bare_name(&sym_name(name_form))
    } else if is_string_value(name_form) {
        val_as_str(name_form)
    } else {
        val_as_str(eval_form(name_form, env)?)
    };
    let pkg_name = name_raw
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase();
    let mut exports = Vec::new();
    let mut uses = Vec::new();
    let mut nicknames = Vec::new();
    let mut interns: Vec<String> = Vec::new();
    // (from-package, symbol-name) pairs to import into this package.
    let mut import_from: Vec<(String, String)> = Vec::new();

    let mut c = opts;
    while c.is_cons() {
        let (opt, rest) = cp(c);
        if opt.is_cons() {
            let (key, val_list) = cp(opt);
            let key_name = sym_name(key);
            let key_bare = key_name
                .trim_start_matches("KEYWORD:")
                .trim_start_matches(':');
            match key_bare {
                "USE" | "USE-REEXPORT" | "MIX" | "MIX-REEXPORT" => {
                    for v in list_to_vec(val_list) {
                        uses.push(resolve_package_name(env, &val_as_str(v)));
                    }
                }
                "REEXPORT" => {
                    for v in list_to_vec(val_list) {
                        let from = resolve_package_name(env, &val_as_str(v));
                        uses.push(from.clone());
                        if let Some(pkg) = bliss_stdlib::find_package(&from) {
                            for sym in bliss_stdlib::external_symbols_of(pkg) {
                                exports.push(symbol_bare_name(&sym_name(sym)));
                            }
                        }
                    }
                }
                "EXPORT" => {
                    for v in list_to_vec(val_list) {
                        exports.push(symbol_bare_name(&sym_name(v)));
                    }
                }
                "NICKNAMES" => {
                    for v in list_to_vec(val_list) {
                        nicknames.push(normalize_package_name(&val_as_str(v)));
                    }
                }
                "INTERN" | "SHADOW" => {
                    for v in list_to_vec(val_list) {
                        interns.push(symbol_bare_name(&sym_name(v)));
                    }
                }
                "IMPORT-FROM" | "SHADOWING-IMPORT-FROM" => {
                    let vals = list_to_vec(val_list);
                    if let Some((pkg, syms)) = vals.split_first() {
                        let from = resolve_package_name(env, &val_as_str(*pkg));
                        for s in syms {
                            import_from.push((from.clone(), symbol_bare_name(&sym_name(*s))));
                        }
                    }
                }
                "RECYCLE" | "UNINTERN" | "DOCUMENTATION" | "LOCAL-NICKNAMES" => {}
                _ => {}
            }
        }
        c = rest;
    }

    // Register the canonical name and every nickname with the reader, so
    // package-qualified symbols written with either resolve at read time.
    reader::register_package(&pkg_name);
    for nick in &nicknames {
        reader::register_package(nick);
    }
    // Create (or reuse) the package in the registry, linking its use-list
    // (creating placeholders for forward `:use` references), then its nicknames.
    let use_refs: Vec<&str> = uses.iter().map(String::as_str).collect();
    ensure_package_available(env, &pkg_name, &use_refs);
    if let Some(pkg) = bliss_stdlib::find_package(&pkg_name) {
        for nick in &nicknames {
            let _ = bliss_stdlib::add_nickname(pkg, nick);
        }
    }

    // Import named symbols so they are accessible (and identical) in this
    // package; fall back to a fresh internal symbol if the source lacks it.
    for (from, sym_name_str) in &import_from {
        let found = find_symbol_in_package(env, from, sym_name_str).map(|(sym, _)| sym);
        match found {
            Some(sym) => {
                if let Some(pkg) = bliss_stdlib::find_package(&pkg_name) {
                    let _ = bliss_stdlib::add_symbol(pkg, sym_name_str, sym, false);
                }
            }
            None => {
                intern_into_package(env, &pkg_name, sym_name_str);
            }
        }
    }
    // Intern :intern/:shadow symbols as internal symbols.
    for name in &interns {
        intern_into_package(env, &pkg_name, name);
    }
    // Make each exported name present + external. If a symbol of that name is
    // already accessible in the package — in particular inherited from a used
    // package — export THAT symbol (preserving its identity and home package)
    // rather than forking a fresh same-named one. Re-exporting an inherited
    // symbol must keep it EQ to the original, or a downstream package that uses
    // both paths sees two conflicting symbols (bliss-lb6.8).
    for name in &exports {
        let Some(pkg) = bliss_stdlib::find_package(&pkg_name) else {
            continue;
        };
        let sym = bliss_stdlib::find_present_symbol(pkg, name)
            .or_else(|| find_symbol_in_package(env, &pkg_name, name).map(|(s, _)| s))
            .unwrap_or_else(|| intern_into_package(env, &pkg_name, name));
        let _ = bliss_stdlib::add_symbol(pkg, name, sym, true);
    }

    // DEFPACKAGE returns the package object (CLHS), not T (bliss-bhs).
    Ok(package_object(&pkg_name))
}

// ── FORMAT ───────────────────────────────────────────────────────
fn eval_format(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // Root the source cursor, the evaluated destination, and the format-arg
    // spine across each evaluation (bliss-6b2 #2): a young form list relocates
    // under a minor GC fired inside eval_form.
    let roots = bliss_rt::ShadowRootScope::new();
    let (df, r) = cp(args);
    let r = roots.root(r);
    let dest = roots.root(eval_form(df, env)?);
    let (ff, fa) = cp(r.get());
    let fa = roots.root(fa);
    let fv = eval_form(ff, env)?;
    let fs = val_as_str(fv);
    let av = eval_args(fa.get(), env)?;
    // Park the env so the formatter's `~A`/`~S` can dispatch user print-object
    // methods (via stdlib_print_object_hook → dispatch_print_object).
    let prev_env = PRINT_ENV.with(|c| c.replace(env as *mut Env));
    let result = bliss_stdlib::format(dest.get(), &fs, &av);
    PRINT_ENV.with(|c| c.set(prev_env));
    result
}

fn val_as_str(val: BlissVal) -> String {
    // A package object stringifies to its name, so the many package builtins that
    // resolve a designator via `val_as_str` accept a package object transparently
    // (bliss-bhs).
    if let Some(name) = package_object_name(val) {
        return name;
    }
    if let Some(s) = bliss_stdlib::registered_string(val) {
        return s;
    }
    if val.is_heap_object() {
        unsafe {
            let p = val.as_ptr();
            let h = *(p as *const ObjectHeader);
            if h.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(p.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(p.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    return s.to_string();
                }
            }
        }
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For symbols used as keyword args, strip package prefix
        return name;
    }
    format_val(val)
}

/// Coerce a pathname designator — a namestring string or a pathname object — to
/// a filesystem path string. `val_as_str` on a pathname returns its debug repr
/// rather than the namestring, so callers that take file paths (LOAD, etc.) must
/// resolve pathnames through `namestring` first. bliss-lb6.
fn path_designator_to_string(v: BlissVal) -> Result<String, BlissError> {
    if bliss_stdlib::is_pathname(v) {
        return Ok(val_as_str(bliss_stdlib::namestring(v)?));
    }
    Ok(val_as_str(v))
}

/// Draw the next 64 random bits from a per-thread SplitMix64 generator, seeded
/// once from OS entropy via `RandomState` (no external RNG dependency). Backs the
/// RANDOM builtin; a fresh, unreproducible sequence per process, like a default
/// CL *RANDOM-STATE*.
fn next_random_u64() -> u64 {
    use std::cell::Cell;
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    thread_local! {
        static STATE: Cell<u64> = Cell::new(RandomState::new().build_hasher().finish() | 1);
    }
    STATE.with(|s| {
        let mut z = s.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        s.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    })
}

fn vals_equal(a: BlissVal, b: BlissVal) -> bool {
    if a == b {
        return true;
    }
    if a.is_fixnum() && b.is_fixnum() {
        return a.as_fixnum() == b.as_fixnum();
    }
    if a.is_single_float() && b.is_single_float() {
        return a.as_single_float() == b.as_single_float();
    }
    if is_string_value(a) && is_string_value(b) {
        return val_as_str(a) == val_as_str(b);
    }
    // Pathnames: EQUAL when their components match. Comparing namestrings is the
    // faithful proxy and matches the EQUAL hash-table's `cl_equal`. ASDF compares
    // pathnames with EQUAL pervasively (find-system caching, output-file dedup,
    // and — via actions keyed on components — plan-traversal cycle detection);
    // returning NIL for equal pathnames made those caches never hit, so
    // asdf:load-system re-traversed forever (bliss-nad).
    if bliss_stdlib::is_pathname(a) && bliss_stdlib::is_pathname(b) {
        return match (bliss_stdlib::namestring(a), bliss_stdlib::namestring(b)) {
            (Ok(na), Ok(nb)) => val_as_str(na) == val_as_str(nb),
            _ => false,
        };
    }
    if a.is_cons() && b.is_cons() {
        let (a_car, a_cdr) = cp(a);
        let (b_car, b_cdr) = cp(b);
        return vals_equal(a_car, b_car) && vals_equal(a_cdr, b_cdr);
    }
    false
}

/// CL `EQUALP`: like `EQUAL` but numbers compare by value across types
/// (`1` equalp `1.0`), characters and strings compare case-insensitively, and
/// vectors/arrays compare element-wise (same length, elements `EQUALP`).
fn vals_equalp(a: BlissVal, b: BlissVal) -> bool {
    if a == b {
        return true;
    }
    if is_number_value(a) && is_number_value(b) {
        return numeric_cmp(a, b)
            .map(|o| o == Ordering::Equal)
            .unwrap_or(false);
    }
    if a.is_character() && b.is_character() {
        return a.as_char().eq_ignore_ascii_case(&b.as_char());
    }
    if is_string_value(a) && is_string_value(b) {
        return val_as_str(a).eq_ignore_ascii_case(&val_as_str(b));
    }
    if a.is_cons() && b.is_cons() {
        let (ac, ad) = cp(a);
        let (bc, bd) = cp(b);
        return vals_equalp(ac, bc) && vals_equalp(ad, bd);
    }
    // Hash tables: same test, same count, and every entry's value EQUALP.
    if is_hash_table_value(a) && is_hash_table_value(b) {
        let counts_match = matches!(
            (bliss_stdlib::hash_table_count(a), bliss_stdlib::hash_table_count(b)),
            (Ok(x), Ok(y)) if x == y
        );
        let tests_match = matches!(
            (bliss_stdlib::hash_table_test(a), bliss_stdlib::hash_table_test(b)),
            (Ok(x), Ok(y)) if x == y
        );
        if !counts_match || !tests_match {
            return false;
        }
        return match bliss_stdlib::hash_table_entries(a) {
            Ok(entries) => entries.into_iter().all(|(k, va)| {
                matches!(bliss_stdlib::gethash(k, b, NIL), Ok((vb, true)) if vals_equalp(va, vb))
            }),
            Err(_) => false,
        };
    }
    // Vectors / arrays (simple and fill-pointer): same length, EQUALP elements.
    if is_vector_value(a) && is_vector_value(b) && !is_string_value(a) && !is_string_value(b) {
        let la = bliss_stdlib::length(a).unwrap_or(usize::MAX);
        let lb = bliss_stdlib::length(b).unwrap_or(usize::MAX);
        if la == usize::MAX || la != lb {
            return false;
        }
        for i in 0..la {
            match (bliss_stdlib::elt(a, i), bliss_stdlib::elt(b, i)) {
                (Ok(x), Ok(y)) if vals_equalp(x, y) => {}
                _ => return false,
            }
        }
        return true;
    }
    false
}

/// Load the user init file at startup (after the bootstrap prelude, before any
/// --eval/--load/--script/REPL processing). The path is `$BLISS_INIT_FILE` if
/// set, otherwise `~/.blissrc`. A missing file is normal and skipped silently;
/// an error while evaluating the init file is reported to stderr but is
/// non-fatal, so a broken init file never blocks startup. The caller gates this
/// on `--no-init` / `--script`.
fn load_init_file(env: &mut Env) {
    let path = match std::env::var("BLISS_INIT_FILE") {
        Ok(p) => p,
        Err(_) => match std::env::var("HOME") {
            Ok(home) => format!("{}/.blissrc", home),
            Err(_) => return,
        },
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        // No init file present — the normal case.
        return;
    };
    if let Err(e) = read_eval_all_env(&contents, env) {
        eprintln!("; error loading init file {}: {}", path, describe_err(&e));
    }
}

// ── CLI driver ─────────────────────────────────────────────────────
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    let ca = CliArgs::parse(args)?;
    if ca.help {
        print_help();
        return Ok(0);
    }
    if ca.version {
        print_version();
        return Ok(0);
    }

    let mut env = Env::new(ca.sandbox);

    // Set *command-line-args* (issue #4)
    if !ca.cl_args.is_empty() {
        let args_list: Vec<BlissVal> = ca.cl_args.iter().map(|s| arena_str(s)).collect();
        let args_val = vec_to_list(&args_list);
        env.define_local("*COMMAND-LINE-ARGS*", args_val);
    } else {
        env.define_local("*COMMAND-LINE-ARGS*", NIL);
    }

    // Load the Lisp bootstrap prelude. It defines standard-CL forms (defvar,
    // push, incf, ...) that are part of the language, not an optional feature,
    // so it loads unconditionally — before any init file, --eval, --load, or
    // script — for every mode. `--no-bootstrap` opts out (raw evaluator);
    // `--bootstrap` is now the default and kept only for compatibility. A
    // failure here is fatal: the prelude is core.
    if !ca.no_bootstrap {
        let contents = boot_prelude_source()?;
        read_eval_all_env(&contents, &mut env)?;
    }

    // Load the user init file (~/.blissrc, or $BLISS_INIT_FILE) when starting an
    // interactive REPL. Batch modes (--eval, --load, a script) run without it, so
    // they stay hermetic and reproducible (spec: an explicit --eval overrides
    // init-file discovery); --no-init opts the REPL out too (issue #8). A missing
    // file is normal; a broken init file is reported but non-fatal.
    if !ca.no_init && ca.eval.is_none() && ca.load.is_none() && ca.script.is_none() {
        load_init_file(&mut env);
    }

    // Load image if specified (issue #8)
    if let Some(ref image_path) = ca.image {
        if let Ok(contents) = std::fs::read_to_string(image_path) {
            read_eval_all_env(&contents, &mut env)?;
        }
    }

    if let Some(ref path) = ca.load_report {
        return run_load_report(path, &mut env);
    }
    if let Some(ref expr) = ca.eval {
        return run_eval_env(expr, &mut env);
    }
    if let Some(ref path) = ca.load {
        return run_load_env(path, &mut env);
    }
    if let Some(ref script) = ca.script {
        return run_script_env(script, &mut env);
    }
    run_repl_env(&mut env)
}

/// Collapse a form's source to a single-line, length-capped preview keyed by its
/// operator, for the punch-list.
fn preview_form(src: &str) -> String {
    // Drop leading comment lines / blank lines the reader skipped, so the
    // preview starts at the actual form.
    let body: String = src
        .lines()
        .skip_while(|line| {
            let t = line.trim_start();
            t.is_empty() || t.starts_with(';')
        })
        .collect::<Vec<_>>()
        .join(" ");
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 96 {
        let head: String = collapsed.chars().take(93).collect();
        format!("{}...", head)
    } else {
        collapsed
    }
}

/// Bucket an error message into a category. Undefined functions/variables keep
/// their name so the report shows exactly which symbols are missing.
fn categorize_error(desc: &str) -> String {
    if let Some(rest) = desc.strip_prefix("undefined function: ") {
        return format!("undefined-fn: {}", rest);
    }
    if let Some(rest) = desc.strip_prefix("unbound variable: ") {
        return format!("unbound-var: {}", rest);
    }
    if desc.contains("CHECK-TYPE") {
        return "check-type-failure".to_string();
    }
    if desc.contains("LOOP ") {
        return "loop-unsupported-clause".to_string();
    }
    if desc.contains("SETF: unsupported place") {
        return "setf-unsupported-place".to_string();
    }
    if desc.contains("not an instance") {
        return "not-an-instance".to_string();
    }
    let cleaned = desc.strip_prefix("internal error: ").unwrap_or(desc);
    let cleaned = cleaned.strip_prefix("ERROR: ").unwrap_or(cleaned);
    cleaned.chars().take(60).collect()
}

/// From `pos`, find the index of the next top-level form (a `(` at the start of
/// a line). Used to resync after a reader error.
fn resync_to_next_toplevel(chars: &[char], pos: usize) -> usize {
    let mut i = pos;
    while i + 1 < chars.len() {
        if chars[i] == '\n' && chars[i + 1] == '(' {
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

/// `--load-report`: evaluate every top-level form in a file, continue past
/// errors, and print a categorized punch-list of the failures.
fn run_load_report(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    use std::collections::BTreeMap;
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    register_declared_packages(&contents);
    let chars: Vec<char> = contents.chars().collect();
    let mut pos = 0usize;
    let mut form_index = 0usize;
    let mut ok = 0usize;
    let mut failures: Vec<(usize, String, String)> = Vec::new();
    let mut category_counts: BTreeMap<String, usize> = BTreeMap::new();

    with_eval_context(env, EvalContext::Load, |env| {
        loop {
            while pos < chars.len() && chars[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos >= chars.len() {
                break;
            }
            let remaining: String = chars[pos..].iter().collect();
            let (val, consumed) = match reader::read_from_string(&remaining) {
                Ok(pair) => pair,
                Err(e) => {
                    *category_counts
                        .entry("reader-error".to_string())
                        .or_insert(0) += 1;
                    failures.push((form_index + 1, "<reader>".into(), describe_err(&e)));
                    let next = resync_to_next_toplevel(&chars, pos);
                    if next <= pos {
                        break;
                    }
                    pos = next;
                    continue;
                }
            };
            if val == EOF {
                break;
            }
            let form_src: String = chars[pos..pos + consumed].iter().collect();
            pos += consumed;
            form_index += 1;
            match eval_form(val, env) {
                Ok(_) => ok += 1,
                Err(e) => {
                    let desc = describe_err(&e);
                    *category_counts.entry(categorize_error(&desc)).or_insert(0) += 1;
                    failures.push((form_index, preview_form(&form_src), desc));
                }
            }
        }
        Ok(NIL)
    })?;

    println!(
        "== {} ==\n{} top-level forms: {} ok, {} failed\n",
        path,
        form_index,
        ok,
        failures.len()
    );

    let mut cats: Vec<(&String, &usize)> = category_counts.iter().collect();
    cats.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("== Failure categories (by count) ==");
    for (cat, count) in &cats {
        println!("  {:>4}  {}", count, cat);
    }

    println!("\n== First failing forms (up to 80) ==");
    for (idx, form, err) in failures.iter().take(80) {
        println!("  [{:>4}] {}\n         → {}", idx, form, err);
    }
    Ok(0)
}

fn run_eval_env(expr: &str, env: &mut Env) -> Result<i32, BlissError> {
    match with_eval_context(env, EvalContext::Eval, |env| read_eval_all_env(expr, env)) {
        Ok(result) => {
            println!("{}", format_val_env(result, env, true));
            // When the top-level form yielded multiple values, echo the
            // secondary values too (one per line). env.mv holds the full value
            // list only when the last form actually produced multiple values;
            // guard on mv[0] == result so a stale channel from a nested form
            // does not leak extra output.
            let mv = env.mv.clone();
            if mv.len() > 1 && mv[0] == result {
                for v in &mv[1..] {
                    println!("{}", format_val(*v));
                }
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

/// Render an error for display, resolving symbol indices to their names so
/// messages read `undefined function: FOO` instead of `... Symbol(147)`.
pub fn describe_err(e: &BlissError) -> String {
    match e {
        BlissError::UndefinedFunction(s) => format!("undefined function: {}", sym_name(*s)),
        BlissError::UnboundVariable(s) => format!("unbound variable: {}", sym_name(*s)),
        _ => format!("{}", e),
    }
}

fn run_load_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    match load_path_into_env(path, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

fn run_script_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    match read_eval_all_env(&contents, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

// Keep standalone versions for backward compatibility
#[allow(dead_code)]
fn run_eval(expr: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_eval_env(expr, &mut env)
}

#[allow(dead_code)]
fn run_load(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_load_env(path, &mut env)
}

#[allow(dead_code)]
fn run_script(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_script_env(path, &mut env)
}

pub fn help_text() -> &'static str {
    concat!(
        "Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]\n",
        "\n",
        "Bliss Common Lisp\n",
        "\n",
        "Options:\n",
        "  --help               Print this help message and exit\n",
        "  --version            Print version information and exit\n",
        "  --eval, -e EXPR      Evaluate EXPR and exit\n",
        "  --load FILE          Load FILE and exit\n",
        "  --image FILE         Path to the boot image\n",
        "  --no-image           Start without loading an image\n",
        "  --bootstrap          Deprecated; the prelude now loads by default\n",
        "  --no-bootstrap       Skip the bootstrap prelude (raw evaluator)\n",
        "  --workers N          Number of worker threads\n",
        "  --heap-size SIZE     Heap size (e.g. 512M, 1G)\n",
        "  --tlab-size SIZE     Per-thread TLAB size\n",
        "  --nursery-size SIZE  Nursery size\n",
        "  --stack-size SIZE    CL stack size per green thread\n",
        "  --gc-log FILE        Write GC logs to FILE\n",
        "  --jit-dump           Emit jitdump metadata\n",
        "  --log-level LEVEL    Set log level (error|warn|info|debug|trace)\n",
        "  --sandbox            Enable sandbox mode\n",
        "  --no-init            Skip loading the init file\n",
        "\n",
        "Arguments after -- are passed through to CL as *command-line-args*.\n",
    )
}

pub fn print_help() {
    print!("{}", help_text());
}

pub fn print_version() {
    println!("bliss {}", env!("CARGO_PKG_VERSION"));
}

// ── REPL driver ────────────────────────────────────────────────────
pub fn run_repl() -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_repl_env(&mut env)
}

fn run_repl_env(env: &mut Env) -> Result<i32, BlissError> {
    let stdin = std::io::stdin();
    run_repl_reader(env, &mut stdin.lock())
}

/// Run the REPL against an explicit line reader instead of the process stdin.
///
/// Intended for tests and embedding: passing an empty/closed reader yields an
/// immediate EOF so the caller never blocks on the process's real stdin (which
/// makes `cargo test` hang in an interactive terminal). See issue bliss-z57.
pub fn run_repl_with_reader<R: std::io::BufRead>(reader: &mut R) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_repl_reader(&mut env, reader)
}

/// The read/eval/print loop over an arbitrary line source. Real runs pass
/// `stdin().lock()`; tests inject an empty reader (immediate EOF).
fn run_repl_reader<R: std::io::BufRead>(env: &mut Env, reader: &mut R) -> Result<i32, BlissError> {
    let _config = ReplConfig::default();
    println!("Bliss Common Lisp {}", env!("CARGO_PKG_VERSION"));
    println!("Type (quit) to exit.");
    println!();
    // Promote initial allocations (env setup, image load) to permanent
    ARENA.with(|a| a.borrow_mut().promote_all());
    let mut input = String::new();
    // Debugger state (nesting level, history) carried across debugger entries (A6.02).
    let mut repl_state = bliss_stdlib::ReplState::new();
    loop {
        // Prompt reflects the current package, e.g. `CL-USER> ` (R6.08).
        eprint!("{}> ", prompt_package_name(&env.current_package));
        input.clear();
        match reader.read_line(&mut input) {
            Ok(0) => {
                println!();
                return Ok(0);
            }
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "(quit)" || trimmed == "(exit)" {
                    return Ok(0);
                }
                match read_eval_all_env(trimmed, env) {
                    Ok(result) => {
                        println!("{}", format_val_env(result, env, true));
                        // Promote allocations from this eval (they may be stored in env)
                        ARENA.with(|a| a.borrow_mut().promote_all());
                    }
                    Err(e) => {
                        eprintln!("ERROR: {}", describe_err(&e));
                        // Enter the full devtools debugger (R6.13, A6.02): supports
                        // step/next/out/continue, backtrace, frame, eval, restart, and
                        // help. It runs its own read loop and returns once the user
                        // continues, aborts, or requests stepping. In non-interactive
                        // sessions (piped stdin/scripts) it returns immediately so the
                        // REPL keeps consuming input rather than blocking.
                        let condition = arena_str(&format!("{}", e));
                        let _ = bliss_stdlib::invoke_debugger_ui(condition, &mut repl_state);
                        // On error, temporary allocations can be freed
                        ARENA.with(|a| {
                            let mut arena = a.borrow_mut();
                            if arena.should_compact() {
                                arena.compact();
                            }
                        });
                    }
                }
            }
            Err(e) => return Err(BlissError::Internal(format!("read error: {}", e))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplConfig {
    pub history_file: String,
    pub history_size: usize,
    pub syntax_highlighting: bool,
}
impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            history_file: "~/.bliss/repl-history".into(),
            history_size: 1000,
            syntax_highlighting: true,
        }
    }
}

// ── T0 objects on the shared GC heap (bliss-jtc.1) ────────────────

/// Serialize tests that walk the shared per-process GC heap: `walk_heap` reads
/// object headers, so it must not run while a sibling test is allocating.
#[cfg(test)]
fn heap_test_lock() -> &'static std::sync::Mutex<()> {
    static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod env_gc_root_tests {
    use super::*;

    const BASE: i64 = 900_000_000;
    const ROOT_COUNT: i64 = 36;
    const RELOCATION_DELTA: i64 = 10_000;

    fn marker(offset: i64) -> BlissVal {
        BlissVal::from_fixnum(BASE + offset)
    }

    fn method(offset: i64) -> MethodDef {
        MethodDef {
            method_id: marker(offset),
            specializers: vec![MethodSpecializer::Eql(marker(offset + 1))],
            lambda_list: marker(offset + 2),
            qualifier: bliss_stdlib::MethodQualifier::Primary,
            body: marker(offset + 3),
        }
    }

    /// The visitor exposes real mutable storage for every category of value an
    /// Env can retain. Reusing one captured parent frame through several
    /// definitions also verifies that shared frame slots are yielded once.
    #[test]
    fn env_root_visitor_rewrites_all_retained_value_slots() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        let parent = Rc::new(RefCell::new(EnvFrame::default()));

        env.frame.borrow_mut().vars.insert("ROOT".into(), marker(0));
        env.frame.borrow_mut().symbol_vars.insert(1, marker(1));
        env.frame.borrow_mut().parent = Some(Rc::clone(&parent));
        parent.borrow_mut().vars.insert("PARENT".into(), marker(2));
        parent.borrow_mut().symbol_vars.insert(2, marker(3));

        env.funs.borrow_mut().insert(
            "LOCAL-FUN".into(),
            FunDef {
                params: Vec::new(),
                params_form: marker(4),
                body: marker(5),
            },
        );
        env.macros.borrow_mut().insert(
            "LOCAL-MACRO".into(),
            MacroDef {
                params_form: marker(6),
                body: marker(7),
                captured_frame: Rc::clone(&parent),
                bytecode: None,
            },
        );
        env.setf_expanders.borrow_mut().insert(
            "LONG-SETF".into(),
            SetfExpander::Expander(MacroDef {
                params_form: marker(8),
                body: marker(9),
                captured_frame: Rc::clone(&parent),
                bytecode: None,
            }),
        );
        env.setf_expanders
            .borrow_mut()
            .insert("SHORT-SETF".into(), SetfExpander::ShortUpdate(marker(10)));
        env.symbol_macros.borrow_mut().insert(3, marker(11));

        let mut class_values = HashMap::new();
        class_values.insert("SHARED".into(), Some(marker(13)));
        env.classes.borrow_mut().insert(
            "ROOT-CLASS".into(),
            ClassDef {
                name: "ROOT-CLASS".into(),
                supers: Vec::new(),
                slots: vec![SlotDef {
                    name: "SLOT".into(),
                    initargs: Vec::new(),
                    accessor: None,
                    readers: Vec::new(),
                    writers: Vec::new(),
                    initform: Some(marker(12)),
                    allocation: SlotAllocation::Instance,
                }],
                class_slot_values: Arc::new(Mutex::new(class_values)),
            },
        );
        env.generics.borrow_mut().insert(
            "ROOT-GENERIC".into(),
            GenericDef {
                generic_function: marker(14),
                combination: bliss_stdlib::MethodCombinationType::Standard,
            },
        );
        env.methods
            .borrow_mut()
            .insert("ROOT-METHOD".into(), vec![method(15)]);

        env.restarts.push(RestartEntry {
            name: "ROOT-RESTART".into(),
            function: RestartFunction::FunctionForm {
                function_form: marker(19),
                captured_frame: Rc::clone(&parent),
            },
            interactive_function: Some(RestartFunction::FunctionForm {
                function_form: marker(20),
                captured_frame: Rc::clone(&parent),
            }),
            test_function: Some(RestartFunction::FunctionForm {
                function_form: marker(21),
                captured_frame: Rc::clone(&parent),
            }),
            unwind_on_invoke: false,
        });
        env.handlers.push(HandlerCluster {
            entries: vec![
                HandlerEntry {
                    type_name: "T".into(),
                    handler: HandlerImpl::Function(marker(22)),
                },
                HandlerEntry {
                    type_name: "T".into(),
                    handler: HandlerImpl::HandlerCase {
                        token: "ROOT-HANDLER".into(),
                        var_name: None,
                        body: marker(23),
                        captured_frame: Rc::clone(&parent),
                    },
                },
            ],
        });
        env.mv.push(marker(24));
        env.closures.borrow_mut().insert(
            1,
            Closure {
                params_form: marker(25),
                body: marker(26),
                captured_frame: Rc::clone(&parent),
            },
        );
        env.method_context.push(MethodContext {
            args: vec![marker(27)],
            next: NextMethod::Standard {
                around: vec![method(28)],
                before: Vec::new(),
                primary: Vec::new(),
                after: Vec::new(),
            },
        });
        env.method_context.push(MethodContext {
            args: Vec::new(),
            next: NextMethod::Primary {
                primary: vec![method(32)],
            },
        });

        let expected: HashSet<i64> = (BASE..BASE + ROOT_COUNT).collect();
        let mut rewritten = HashSet::new();
        env.visit_gc_roots(&mut |slot| unsafe {
            let value = &mut *slot;
            if value.is_fixnum() {
                let n = value.as_fixnum();
                if expected.contains(&n) {
                    assert!(rewritten.insert(n), "root slot {n} was visited twice");
                    *value = BlissVal::from_fixnum(n + RELOCATION_DELTA);
                }
            }
        });
        assert_eq!(rewritten, expected, "every seeded Env root must be yielded");

        let relocated_expected: HashSet<i64> =
            expected.iter().map(|n| n + RELOCATION_DELTA).collect();
        let mut relocated = HashSet::new();
        env.visit_gc_roots(&mut |slot| unsafe {
            let value = &*slot;
            if value.is_fixnum() && relocated_expected.contains(&value.as_fixnum()) {
                assert!(relocated.insert(value.as_fixnum()));
            }
        });
        assert_eq!(
            relocated, relocated_expected,
            "the visitor must expose the mutable slots, not temporary copies"
        );
    }

    /// Child environments inherit lexical definitions by value, so mutating a
    /// FLET/MACROLET/SYMBOL-MACROLET table in a child cannot leak to its parent.
    #[test]
    fn mutable_lexical_root_maps_remain_child_local() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let parent = Env::new(false);
        parent.funs.borrow_mut().insert(
            "PARENT".into(),
            FunDef {
                params: Vec::new(),
                params_form: marker(0),
                body: marker(1),
            },
        );
        parent.symbol_macros.borrow_mut().insert(99, marker(2));
        parent.macros.borrow_mut().insert(
            "PARENT-MACRO".into(),
            MacroDef {
                params_form: marker(3),
                body: marker(4),
                captured_frame: Rc::clone(&parent.frame),
                bytecode: None,
            },
        );

        let mut child = parent.child();
        child.funs_mut().insert(
            "CHILD".into(),
            FunDef {
                params: Vec::new(),
                params_form: marker(5),
                body: marker(6),
            },
        );
        child.symbol_macros_mut().insert(100, marker(7));
        let child_frame = Rc::clone(&child.frame);
        child.macros_mut().insert(
            "CHILD-MACRO".into(),
            MacroDef {
                params_form: marker(8),
                body: marker(9),
                captured_frame: child_frame,
                bytecode: None,
            },
        );

        assert!(child.funs.borrow().contains_key("PARENT"));
        assert!(!parent.funs.borrow().contains_key("CHILD"));
        assert!(!parent.symbol_macros.borrow().contains_key(&100));
        assert!(!parent.macros.borrow().contains_key("CHILD-MACRO"));
    }
}

#[cfg(test)]
mod jtc1_heap_tests {
    use super::*;

    /// Objects allocated by the tree-walk evaluator live on the shared GC heap
    /// and are visible to heap walking (bliss-jtc.1).
    #[test]
    fn evaluator_objects_live_on_the_gc_heap_and_are_walkable() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        // WRITE-TO-STRING allocates its result through the evaluator's arena
        // (now the GC heap); the distinctive digits are unlikely to collide with
        // any other live string. (A source string *literal* would be allocated by
        // the reader — jtc.15 — so we use an evaluator-produced string here.)
        let marker = "918273645";
        let src = "(cons (write-to-string 918273645) (cons 1 (cons 2 nil)))";
        read_eval_all(src).expect("eval");

        let mut cons_count = 0usize;
        let mut found_marker = false;
        bliss_rt::walk_heap(|ptr, tid, _size| {
            if tid == type_id::CONS {
                cons_count += 1;
            } else if tid == type_id::SIMPLE_BASE_STRING {
                // String body layout: [len:u64 | bytes].
                unsafe {
                    let len = *(ptr as *const u64) as usize;
                    if len == marker.len() {
                        let bytes = std::slice::from_raw_parts(ptr.add(8), len);
                        found_marker |= bytes == marker.as_bytes();
                    }
                }
            }
            true
        })
        .expect("walk_heap");

        assert!(
            cons_count > 0,
            "evaluator cons cells must be walkable on the GC heap"
        );
        assert!(
            found_marker,
            "the evaluator-allocated string must be walkable on the GC heap"
        );
    }
}

#[cfg(test)]
mod transient_shadow_root_tests {
    use super::*;

    /// A collection in the third argument moves the vector and cons produced
    /// by the first two arguments. VALUES must retain both through precise
    /// argument roots and publish their rewritten addresses afterward.
    #[test]
    fn nested_evaluation_rewrites_scalar_and_argument_vector_temporaries() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        let primary = read_eval_all_env(
            "(values (vector 10 20 30) (cons 111 222) (%force-minor-gc-for-test))",
            &mut env,
        )
        .expect("nested evaluation across forced minor GC");

        assert_eq!(bliss_stdlib::length(primary).unwrap(), 3);
        assert_eq!(bliss_stdlib::elt(primary, 0).unwrap().as_fixnum(), 10);
        assert_eq!(bliss_stdlib::elt(primary, 2).unwrap().as_fixnum(), 30);
        assert_eq!(env.mv.len(), 3);
        let (car, cdr) = cp(env.mv[1]);
        assert_eq!(car.as_fixnum(), 111);
        assert_eq!(cdr.as_fixnum(), 222);
        assert!(env.mv[2].is_nil());

        let after = read_eval_all_env("(cons 333 444)", &mut env)
            .expect("T0 allocator must reacquire a valid TLAB after collection");
        let (car, cdr) = cp(after);
        assert_eq!(car.as_fixnum(), 333);
        assert_eq!(cdr.as_fixnum(), 444);
    }

    #[test]
    fn live_env_registration_rewrites_lexical_values_and_unwinds() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        assert_eq!(live_env_root_depth(), 0);

        let outer = LiveEnvRootGuard::new(&mut env);
        assert_eq!(live_env_root_depth(), 1);
        {
            let _inner = LiveEnvRootGuard::new(&mut env);
            assert_eq!(live_env_root_depth(), 2);
        }
        assert_eq!(live_env_root_depth(), 1);

        let value = read_eval_all_env(
            "(let ((held (vector 71 72 73))) (%force-minor-gc-for-test) held)",
            &mut env,
        )
        .expect("lexical Env root must survive forced minor GC");
        assert_eq!(bliss_stdlib::length(value).unwrap(), 3);
        assert_eq!(bliss_stdlib::elt(value, 1).unwrap().as_fixnum(), 72);
        assert_eq!(live_env_root_depth(), 1);
        drop(outer);
        assert_eq!(live_env_root_depth(), 0);

        assert!(read_eval_all_env("missing-env-root-test-variable", &mut env).is_err());
        assert_eq!(live_env_root_depth(), 0, "error unwind must unregister Env");
    }

    #[test]
    fn evaluator_global_scanner_rewrites_macro_definition_slots() {
        const GLOBAL_BASE: i64 = 910_000_000;
        const DELTA: i64 = 10_000;
        let marker = |offset| BlissVal::from_fixnum(GLOBAL_BASE + offset);
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        GLOBAL_MACROS.with(|macros| macros.borrow_mut().clear());
        let frame = Rc::new(RefCell::new(EnvFrame::default()));
        frame.borrow_mut().vars.insert("CAPTURED".into(), marker(2));
        GLOBAL_MACROS.with(|macros| {
            macros.borrow_mut().insert(
                "GC-RETAINED-GLOBAL-MACRO".into(),
                MacroDef {
                    params_form: marker(0),
                    body: marker(1),
                    captured_frame: Rc::clone(&frame),
                    bytecode: None,
                },
            );
        });

        scan_evaluator_global_roots(&mut |slot| unsafe {
            let value = &mut *slot;
            if value.is_fixnum() && (GLOBAL_BASE..GLOBAL_BASE + 3).contains(&value.as_fixnum()) {
                *value = BlissVal::from_fixnum(value.as_fixnum() + DELTA);
            }
        });
        let definition = GLOBAL_MACROS.with(|macros| {
            macros
                .borrow()
                .get("GC-RETAINED-GLOBAL-MACRO")
                .cloned()
                .expect("global macro remains registered")
        });
        assert_eq!(definition.params_form.as_fixnum(), GLOBAL_BASE + DELTA);
        assert_eq!(definition.body.as_fixnum(), GLOBAL_BASE + 1 + DELTA);
        assert_eq!(
            frame.borrow().vars["CAPTURED"].as_fixnum(),
            GLOBAL_BASE + 2 + DELTA
        );
    }
}

// ── Numeric heap objects on the shared GC heap (bliss-jtc.5) ───────

#[cfg(test)]
mod jtc5_numeric_tests {
    use super::*;

    /// Fixnum overflow (bignum) and division (ratio) allocate on the shared GC
    /// heap using the spec layouts, so heap walking sees them.
    #[test]
    fn bignum_and_ratio_live_on_the_gc_heap() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        read_eval_all("(* 1000000000000000000 1000000000000000000)").expect("bignum");
        read_eval_all("(/ 3 7)").expect("ratio");

        let mut bignum = false;
        let mut ratio = false;
        bliss_rt::walk_heap(|_p, tid, _s| {
            if tid == type_id::BIGNUM {
                bignum = true;
            } else if tid == type_id::RATIO {
                ratio = true;
            }
            true
        })
        .expect("walk_heap");
        assert!(bignum, "overflow bignum must be on the GC heap");
        assert!(ratio, "ratio must be on the GC heap");
    }

    /// Numeric type and equality predicates operate on the shared heap
    /// representation: bignums/ratios are numbers, and EQL value-compares
    /// same-type heap numbers while EQ stays identity.
    #[test]
    fn numeric_predicates_use_the_shared_representation() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            read_eval_all("(numberp (* 999999999999 999999999999))").unwrap(),
            T
        );
        assert_eq!(read_eval_all("(numberp (/ 1 3))").unwrap(), T);
        assert_eq!(read_eval_all("(numberp \"x\")").unwrap(), NIL);
        assert_eq!(read_eval_all("(eql 1/3 1/3)").unwrap(), T);
        assert_eq!(
            read_eval_all("(eql (* 10000000000 10000000000) (* 10000000000 10000000000))").unwrap(),
            T
        );
        assert_eq!(read_eval_all("(eq 1/3 1/3)").unwrap(), NIL);
        assert_eq!(read_eval_all("(= (/ 1 2) (/ 2 4))").unwrap(), T);
    }
}

#[cfg(test)]
mod jtc6c2_binding_cell_tests {
    use super::*;

    /// bliss-jtc.6 Stage C2: a global (non-lexical) variable's value lives in the
    /// symbol's heap value cell, and normal references read it back through the
    /// cell fallback — lexical LET bindings are unaffected.
    #[test]
    fn global_value_is_authoritative_in_the_symbol_cell() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);

        // A top-level assignment writes the global value cell.
        read_eval_all_env("(setq *c2-global* 123)", &mut env).expect("setq");
        let idx = bliss_rt::symbols::intern("*C2-GLOBAL*");
        assert_eq!(
            bliss_rt::symbols::symbol_value(idx),
            Some(BlissVal::from_fixnum(123)),
            "global value must be stored in the symbol's value cell"
        );

        // A normal reference reads it back through the cell.
        assert_eq!(
            read_eval_all_env("*c2-global*", &mut env).expect("ref"),
            BlissVal::from_fixnum(123)
        );

        // A lexical LET shadows the global without disturbing the cell.
        assert_eq!(
            read_eval_all_env("(let ((*c2-global* 9)) *c2-global*)", &mut env).expect("let"),
            BlissVal::from_fixnum(9)
        );
        assert_eq!(
            bliss_rt::symbols::symbol_value(idx),
            Some(BlissVal::from_fixnum(123)),
            "the LET binding must not overwrite the global cell"
        );
    }
}

#[cfg(test)]
mod jtc6_8_function_object_tests {
    use super::*;

    /// bliss-jtc.6.8: DEFUN of an ordinary symbol installs a heap interpreted-
    /// function object in the symbol's function cell (with zeroed FnMeta), the
    /// call path resolves through it, and redefinition updates the object in
    /// place — preserving identity — while changing behaviour.
    #[test]
    fn defun_installs_identity_stable_function_object_in_the_cell() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);

        read_eval_all_env("(defun c2b-op (x y) (+ x y))", &mut env).expect("defun");
        let idx = bliss_rt::symbols::intern("C2B-OP");
        let f = bliss_rt::symbols::symbol_function(idx).expect("function cell must be bound");
        assert!(
            bliss_rt::function::is_interpreted_function(f),
            "DEFUN must store a heap interpreted-function object in the function cell"
        );
        assert_eq!(
            bliss_rt::function::tier(f),
            0,
            "a fresh function starts at tier 0"
        );

        // The call path resolves through the cell.
        assert_eq!(
            read_eval_all_env("(c2b-op 2 3)", &mut env).expect("call"),
            BlissVal::from_fixnum(5)
        );

        // Redefinition preserves object identity (tiering/IC/deopt key off it)
        // while changing behaviour.
        read_eval_all_env("(defun c2b-op (x y) (* x y))", &mut env).expect("redefun");
        let f2 = bliss_rt::symbols::symbol_function(idx).expect("still bound");
        assert_eq!(
            f2, f,
            "redefinition must reuse the same function object (stable identity)"
        );
        assert_eq!(
            read_eval_all_env("(c2b-op 2 3)", &mut env).expect("call2"),
            BlissVal::from_fixnum(6)
        );
    }

    /// The function-object invoke counter (FnMeta substrate) advances when the
    /// tree-walker resolves a global call through the cell.
    #[test]
    fn tree_walker_call_bumps_the_invoke_counter() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        read_eval_all_env("(defun c2b-counted () 42)", &mut env).expect("defun");
        let idx = bliss_rt::symbols::intern("C2B-COUNTED");
        let f = bliss_rt::symbols::symbol_function(idx).unwrap();
        let before = bliss_rt::function::invoke_count(f);
        read_eval_all_env("(c2b-counted)", &mut env).expect("call");
        assert!(
            bliss_rt::function::invoke_count(f) > before,
            "a resolved global call must bump the FnMeta invoke counter"
        );
    }

    /// bliss-jtc.10.1: a hot loop inside a function bumps that function object's
    /// back-edge counter once per iteration, and the count scales with the trip
    /// count — the profiling signal the tier scheduler reads to find hot loops.
    #[test]
    fn hot_loop_bumps_the_back_edge_counter() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        // A plain tagbody/go loop counting down from N — the canonical shape
        // LOOP/DO/DOTIMES all lower to.
        read_eval_all_env(
            "(defun c2b-spin (n) \
               (block done \
                 (tagbody \
                  top (when (<= n 0) (return-from done nil)) \
                      (setq n (- n 1)) \
                      (go top))))",
            &mut env,
        )
        .expect("defun");
        let idx = bliss_rt::symbols::intern("C2B-SPIN");
        let f = bliss_rt::symbols::symbol_function(idx).unwrap();

        let before = bliss_rt::function::back_edge_count(f);
        read_eval_all_env("(c2b-spin 100)", &mut env).expect("spin 100");
        let after_100 = bliss_rt::function::back_edge_count(f);
        assert!(
            after_100 >= before + 100,
            "a 100-iteration loop must record >=100 back-edges (before={before}, after={after_100})"
        );

        // A longer trip count records proportionally more back-edges.
        read_eval_all_env("(c2b-spin 500)", &mut env).expect("spin 500");
        let after_500 = bliss_rt::function::back_edge_count(f);
        assert!(
            after_500 >= after_100 + 500,
            "back-edge count must scale with trip count (after_100={after_100}, after_500={after_500})"
        );
    }
}

#[cfg(test)]
mod jtc8_hashtable_tests {
    use super::*;

    /// bliss-jtc.8: MAPHASH invokes any callable — here an interpreted lambda —
    /// through the unified function protocol, not just native pointers.
    #[test]
    fn maphash_invokes_interpreted_functions() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let out = read_eval_all(
            "(let ((h (make-hash-table)) (s 0)) \
               (setf (gethash :a h) 10) (setf (gethash :b h) 20) \
               (maphash (lambda (k v) k (setq s (+ s v))) h) s)",
        )
        .expect("maphash");
        assert_eq!(out, BlissVal::from_fixnum(30));
    }

    /// MAPHASH also dispatches a named global function (its heap function object).
    #[test]
    fn maphash_invokes_named_global_functions() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let out = read_eval_all(
            "(let ((acc nil)) \
               (defun c8-collect (k v) k (setq acc (cons v acc))) \
               (let ((h (make-hash-table))) \
                 (setf (gethash :x h) 1) \
                 (maphash #'c8-collect h)) \
               (length acc))",
        )
        .expect("maphash named");
        assert_eq!(out, BlissVal::from_fixnum(1));
    }

    /// SXHASH: EQUAL objects hash equal (ANSI).
    #[test]
    fn sxhash_equal_objects_hash_equal() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            read_eval_all("(eql (sxhash \"abc\") (sxhash \"abc\"))").unwrap(),
            T
        );
        assert_eq!(
            read_eval_all("(eql (sxhash (list 1 2 3)) (sxhash (list 1 2 3)))").unwrap(),
            T
        );
    }

    // WITH-HASH-TABLE-ITERATOR is a boot.lisp macro, so it is covered by a
    // subprocess test (tests/hashtable_cli.rs) that loads the bootstrap, not by
    // the bootstrap-free read_eval_all helper.
}

#[cfg(test)]
mod jtc3_unified_tiering_tests {
    use super::*;

    /// bliss-jtc.3: the heap function object is the single tiering record. Every
    /// invocation on the hot (bytecode) dispatch path accumulates on the object's
    /// FnMeta invoke counter, which drives T0→T1 promotion — no separate
    /// per-symbol counter map for named functions.
    #[test]
    fn function_object_accumulates_all_invocations() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        read_eval_all_env("(defun c2b-hot (x) (+ x 1))", &mut env).expect("defun");
        let idx = bliss_rt::symbols::intern("C2B-HOT");
        let f = bliss_rt::symbols::symbol_function(idx).expect("function cell bound");

        for _ in 0..15 {
            read_eval_all_env("(c2b-hot 1)", &mut env).expect("call");
        }
        assert!(
            bliss_rt::function::invoke_count(f) >= 15,
            "all invocations accumulate on the function object (the tiering record)"
        );
        // Tier is recorded on the object (0 at T0; promoted to 1 once hot on
        // backends/arches where native compilation succeeds — see t1_native).
        assert!(bliss_rt::function::tier(f) <= 2);
    }
}

#[cfg(test)]
mod jtc5mf_storage_condition_pool_tests {
    use super::*;

    /// bliss-5mf: after Env::new reseeds the STORAGE-CONDITION pool with
    /// CLI-native instances, a preallocated pool condition carries the *same*
    /// condition class the CLI builds for `(make-condition 'storage-condition)`.
    /// Because TYPE-OF and HANDLER-CASE type matching both derive from that
    /// class, the pooled instance is recognized identically — it reports
    /// STORAGE-CONDITION and is caught by (storage-condition ...) / (condition
    /// ...) handler clauses, rather than the stdlib's differently-symboled class
    /// that the CLI reader would read back as an unrelated name.
    #[test]
    fn pooled_storage_condition_shares_the_cli_condition_class() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        // Env::new reseeds the thread-local pool with CLI-native instances.
        let mut env = Env::new(false);

        let pooled = bliss_stdlib::acquire_preallocated_storage_condition()
            .expect("pool must yield a preallocated STORAGE-CONDITION");
        let built = build_condition_instance(&mut env, "STORAGE-CONDITION", &[])
            .expect("CLI must build a STORAGE-CONDITION");

        assert_eq!(
            bliss_stdlib::clos::class_of(pooled),
            bliss_stdlib::clos::class_of(built),
            "pooled STORAGE-CONDITION must share the CLI-recognized condition class"
        );
    }

    /// The reseeded pool instances survive a GC (they are pinned + immortal),
    /// and re-acquire keeps returning class-correct instances.
    #[test]
    fn pooled_storage_condition_survives_gc_and_stays_recognized() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        let expected_class = bliss_stdlib::clos::class_of(
            build_condition_instance(&mut env, "STORAGE-CONDITION", &[]).expect("build"),
        );

        // Churn the heap and force collection; pinned pool instances must persist.
        for _ in 0..64 {
            let _ = read_eval_all_env("(list 1 2 3 4 5)", &mut env);
        }
        let _ = bliss_rt::gc::full_gc();

        let pooled =
            bliss_stdlib::acquire_preallocated_storage_condition().expect("pool must survive GC");
        assert_eq!(
            bliss_stdlib::clos::class_of(pooled),
            expected_class,
            "pooled STORAGE-CONDITION must remain CLI-recognized after GC"
        );
    }
}
