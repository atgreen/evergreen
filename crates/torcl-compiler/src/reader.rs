//! CL Reader — converts character streams into Lisp objects.
//!
//! Implements the CLHS §2.2 reader algorithm. See spec §4.1.

use std::collections::HashMap;
use torcl_rt::error::TorclError;
use torcl_rt::lock_order::{LockLevel, OrderedMutex};
use torcl_rt::object::{
    ComplexData, ConsCell, ElementTypeTag, ObjectHeader, PathnameData, RatioData, ReadtableData,
    type_id,
};
use torcl_rt::value::{EOF, MISSING, NIL, T, TAG_HEAP_OBJECT, TorclVal};

type MacroCharTable = HashMap<(u64, char), (TorclVal, bool)>;
type DispatchCharTable = HashMap<(u64, char), bool>;
type DispatchSubCharTable = HashMap<(u64, char, char), TorclVal>;
const MAX_READER_NESTING: usize = 4096;

// ── Global symbol table ───────────────────────────────────────────
//
// The canonical symbol registry now lives in `torcl_rt::symbols`, where each
// interned symbol is a heap-resident `SymbolData` object (bliss-jtc.6 Stage A).
// The reader's public entrypoints delegate to it so the reader, interpreter,
// stdlib, and GC share one symbol identity space rather than parallel name
// tables. Package existence is likewise delegated to the shared torcl_rt package
// registry of heap PACKAGE objects (bliss-jtc.6 Stage D).

pub fn intern_symbol(name: &str) -> u32 {
    torcl_rt::symbols::intern(name)
}

/// Look up an already-interned symbol index by name WITHOUT interning it.
/// Unlike `intern_symbol`, this never creates a new entry, so it can be used
/// to answer "does a symbol with this name exist?" without side effects — which
/// is what a correct FIND-SYMBOL needs (FIND-SYMBOL must not intern).
pub fn find_symbol_index(name: &str) -> Option<u32> {
    torcl_rt::symbols::find_index(name)
}

/// Look up the name of a symbol by its index.
/// Returns None if the index is not in the global symbol table.
pub fn symbol_name(idx: u32) -> Option<String> {
    torcl_rt::symbols::symbol_name(idx)
}

/// True if `idx` names an uninterned symbol (`make-symbol`/`gensym`; no home
/// package). Used by the printer to emit the `#:` prefix under prin1/`~S`.
pub fn is_uninterned(idx: u32) -> bool {
    torcl_rt::symbols::is_uninterned(idx)
}

pub fn register_package(name: &str) {
    torcl_rt::packages::register(name);
}

fn package_exists(name: &str) -> bool {
    torcl_rt::packages::exists(name)
}

/// Create a fresh uninterned symbol with the given name. Each call yields a
/// distinct symbol (a unique high-range index) with a real heap object, but the
/// name is deliberately not added to the name→index map, so the symbol remains
/// uninterned and is not found by `intern`/`find-symbol`. `symbol_name` can
/// still resolve it via its heap name cell.
pub fn make_uninterned_symbol(name: &str) -> TorclVal {
    torcl_rt::symbols::make_uninterned(name)
}

// ── Global macro character tables ─────────────────────────────────
static MACRO_CHARS: OrderedMutex<Option<MacroCharTable>> =
    OrderedMutex::new(LockLevel::CodeCache, 1, "reader macro characters", None);
static DISPATCH_CHARS: OrderedMutex<Option<DispatchCharTable>> =
    OrderedMutex::new(LockLevel::CodeCache, 2, "reader dispatch characters", None);
static DISPATCH_SUB_CHARS: OrderedMutex<Option<DispatchSubCharTable>> = OrderedMutex::new(
    LockLevel::CodeCache,
    3,
    "reader dispatch sub-characters",
    None,
);
type ReadEvalHook = fn(TorclVal) -> Result<TorclVal, TorclError>;
static READ_EVAL_HOOK: OrderedMutex<Option<ReadEvalHook>> =
    OrderedMutex::new(LockLevel::CodeCache, 4, "reader eval hook", None);

pub fn set_read_eval_hook(hook: Option<ReadEvalHook>) {
    let mut guard = READ_EVAL_HOOK.lock().unwrap();
    *guard = hook;
}

fn read_eval_hook() -> Option<ReadEvalHook> {
    *READ_EVAL_HOOK.lock().unwrap()
}

// ── Custom reader-macro invocation (bliss-r4mk) ─────────────────────
//
// The dispatch tables above store LISP handler functions, but this crate
// cannot call Lisp. The interpreter installs an INVOKER: given the handler,
// the source text REMAINING after the dispatch char/sub-char, the sub-char,
// and the optional infix numeric argument, it wraps the text in a
// string-input-stream, applies the handler `(fn stream sub-char arg)`, and
// returns the values the handler produced (empty = contributed nothing, like
// a comment reader) plus how many CHARS of the text it consumed.
type MacroHandlerInvoker =
    fn(TorclVal, &str, char, Option<i64>) -> Result<(Vec<TorclVal>, usize), TorclError>;
static MACRO_INVOKER: OrderedMutex<Option<MacroHandlerInvoker>> =
    OrderedMutex::new(LockLevel::CodeCache, 8, "reader macro invoker", None);

pub fn set_macro_handler_invoker(hook: Option<MacroHandlerInvoker>) {
    *MACRO_INVOKER.lock().unwrap() = hook;
}

// A plain (non-dispatch) macro character's handler takes only `(stream char)` —
// no sub-char / infix argument — so it needs its own two-argument invoker.
type PlainMacroInvoker = fn(TorclVal, &str, char) -> Result<(Vec<TorclVal>, usize), TorclError>;
static PLAIN_MACRO_INVOKER: OrderedMutex<Option<PlainMacroInvoker>> =
    OrderedMutex::new(LockLevel::CodeCache, 10, "reader plain macro invoker", None);

pub fn set_plain_macro_invoker(hook: Option<PlainMacroInvoker>) {
    *PLAIN_MACRO_INVOKER.lock().unwrap() = hook;
}

/// The CURRENT readtable (the live value of `*READTABLE*`), supplied by the
/// interpreter so nested reads key the custom tables correctly. NIL / no hook
/// means "no custom readtable" and all custom lookups miss.
type ReadtableGetter = fn() -> TorclVal;
static READTABLE_GETTER: OrderedMutex<Option<ReadtableGetter>> =
    OrderedMutex::new(LockLevel::CodeCache, 9, "reader readtable getter", None);

pub fn set_readtable_getter(hook: Option<ReadtableGetter>) {
    *READTABLE_GETTER.lock().unwrap() = hook;
}

fn current_readtable_value() -> TorclVal {
    let hook = *READTABLE_GETTER.lock().unwrap();
    hook.map(|h| h()).unwrap_or(NIL)
}

/// Set once any readtable is ever given a non-`:upcase` readtable-case, so the
/// tokenizer's hot path can skip consulting the readtable entirely in the
/// overwhelmingly common all-upcase case (mirrors `ANY_CUSTOM_MACROS`).
static ANY_NONUPCASE_CASE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The current readtable's `readtable-case` as a small code:
/// 0 = `:upcase` (default), 1 = `:downcase`, 2 = `:preserve`, 3 = `:invert`.
/// Reads the `case_mode` byte from the live readtable object; a non-readtable
/// value (e.g. the `:standard-readtable` placeholder) reads as `:upcase`.
fn current_readtable_case_mode() -> u8 {
    if !ANY_NONUPCASE_CASE.load(std::sync::atomic::Ordering::Relaxed) {
        return 0;
    }
    let rt = current_readtable_value();
    if rt.tag() != TAG_HEAP_OBJECT {
        return 0;
    }
    unsafe {
        let ptr = rt.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        if header.type_id() != type_id::READTABLE {
            return 0;
        }
        (*(ptr as *const ReadtableData)).case_mode
    }
}

/// Apply `readtable-case` folding to one UNESCAPED constituent character under
/// modes that map char→char (`:upcase`, `:downcase`, `:preserve`). `:invert`
/// (mode 3) is whole-token and handled by the caller, so it is left unchanged
/// here.
fn fold_case_char(c: char, case_mode: u8) -> char {
    match case_mode {
        1 => c.to_ascii_lowercase(),
        2 | 3 => c,
        _ => c.to_ascii_uppercase(),
    }
}

/// Cheap gate for the hot path: custom dispatch is consulted only when at
/// least one handler has ever been registered.
static ANY_CUSTOM_MACROS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn any_custom_macros() -> bool {
    ANY_CUSTOM_MACROS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Try a registered custom `#<sub>` dispatch handler at `pos_after_sub`
/// (just past the sub-char). Returns None when no handler applies; otherwise
/// the handler's contribution: its single value, or — for a zero-value
/// handler — the NEXT token read from where the handler stopped.
#[allow(clippy::too_many_arguments)]
fn try_custom_sharp_dispatch(
    chars: &[char],
    pos_after_sub: usize,
    sub: char,
    infix: Option<i64>,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Option<Result<(TorclVal, usize), TorclError>> {
    if !any_custom_macros() {
        return None;
    }
    let readtable = current_readtable_value();
    if readtable == NIL || readtable.tag() != TAG_HEAP_OBJECT {
        return None;
    }
    let handler = lookup_custom_dispatch(readtable, '#', sub.to_ascii_uppercase())?;
    let invoker = (*MACRO_INVOKER.lock().unwrap())?;
    let text: String = chars[pos_after_sub..].iter().collect();
    Some(match invoker(handler, &text, sub, infix) {
        Ok((vals, consumed)) => {
            let newpos = pos_after_sub + consumed;
            match vals.into_iter().next() {
                Some(v) => Ok((v, newpos)),
                None => read_token_with_base(
                    chars,
                    newpos,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth,
                ),
            }
        }
        Err(e) => Err(e),
    })
}

/// True if a custom `#<sub>` handler is registered in the current readtable —
/// used by the `#+`/`#-` feature SKIPPER, which cannot invoke handlers and
/// approximates their extent as "one following form".
fn custom_sharp_dispatch_registered(sub: char) -> bool {
    if !any_custom_macros() {
        return false;
    }
    let readtable = current_readtable_value();
    if readtable == NIL || readtable.tag() != TAG_HEAP_OBJECT {
        return false;
    }
    lookup_custom_dispatch(readtable, '#', sub.to_ascii_uppercase()).is_some()
}

// ── Package-aware symbol resolution ───────────────────────────────
//
// A bare token read inside package P and the qualified spelling `P:NAME` denote
// the SAME symbol with ONE value/function cell. The reader alone cannot enforce
// this: package accessibility (use-lists, exports, home packages) lives in the
// interpreter's package registry. So the interpreter installs a resolver that,
// given a package designator (None = the current *PACKAGE*) and a bare name,
// returns the canonical interned symbol index of the symbol ALREADY accessible
// there — routing every spelling of one logical symbol to a single identity
// (bliss-lb6.12). It returns None to defer to the reader's default name-keyed
// interning: when the name is not yet accessible (so the reader mints it), or
// when no load environment is active (internal READ-FROM-STRING). Either way
// those callers keep their existing behaviour.
type SymbolResolver = fn(Option<&str>, &str) -> Option<u32>;
static SYMBOL_RESOLVER: OrderedMutex<Option<SymbolResolver>> =
    OrderedMutex::new(LockLevel::CodeCache, 5, "reader symbol resolver", None);

pub fn set_symbol_resolver(hook: Option<SymbolResolver>) {
    *SYMBOL_RESOLVER.lock().unwrap() = hook;
}

fn resolve_symbol_via_hook(pkg: Option<&str>, name: &str) -> Option<u32> {
    let hook = *SYMBOL_RESOLVER.lock().unwrap();
    hook.and_then(|h| h(pkg, name))
}

// ── Pathname construction ─────────────────────────────────────────
//
// `#P"…"` must produce the SAME pathname representation the rest of the system
// uses (the stdlib's registry-backed PATHNAME), or PATHNAMEP / NAMESTRING / LOAD
// won't recognise it (bliss-lb6). The reader crate can't depend on torcl-stdlib,
// so the interpreter installs a constructor that turns the parsed namestring
// into a real pathname. Returns None to fall back to the reader's own minimal
// PATHNAME object (used only when no interpreter is wired up, e.g. bare reader
// tests).
type PathnameConstructor = fn(TorclVal) -> Option<TorclVal>;
static PATHNAME_CTOR: OrderedMutex<Option<PathnameConstructor>> =
    OrderedMutex::new(LockLevel::CodeCache, 6, "reader pathname constructor", None);

pub fn set_pathname_constructor(hook: Option<PathnameConstructor>) {
    *PATHNAME_CTOR.lock().unwrap() = hook;
}

/// `#S(name slot val …)` constructor hook: builds a real (CLOS) structure
/// instance from the class name and flat slot key/value list, so a struct read
/// back round-trips with DEFSTRUCT-made instances. `None` if the name is not a
/// known structure class, in which case the reader keeps the legacy STRUCTURE
/// heap object (bliss-ipn7).
type StructConstructor = fn(TorclVal, &[TorclVal]) -> Option<TorclVal>;
static STRUCT_CTOR: OrderedMutex<Option<StructConstructor>> =
    OrderedMutex::new(LockLevel::CodeCache, 7, "reader struct constructor", None);

pub fn set_struct_constructor(hook: Option<StructConstructor>) {
    *STRUCT_CTOR.lock().unwrap() = hook;
}

fn construct_pathname(namestring: TorclVal) -> TorclVal {
    let hook = *PATHNAME_CTOR.lock().unwrap();
    hook.and_then(|h| h(namestring))
        .unwrap_or_else(|| alloc_pathname(namestring))
}

fn readtable_key(readtable: TorclVal) -> u64 {
    readtable.0 & !torcl_rt::value::TAG_MASK
}

// ── Per-readtable character syntax overrides (SET-SYNTAX-FROM-CHAR) ──
//
// Syntax codes:
//   0 = constituent, 1 = whitespace, 2 = terminating macro,
//   3 = non-terminating macro, 4 = single escape, 5 = multiple escape,
//   6 = constituent-but-invalid (only via escape; reading bare signals error)
// The stored `delegate` is the standard character whose built-in reader
// behaviour a macro override emulates (so copying `(`'s syntax onto `!` makes
// `!` open a list). For non-macro syntaxes it is the character itself.
type CharSyntaxTable = HashMap<(u64, char), (u8, char)>;
static CHAR_SYNTAX: OrderedMutex<Option<CharSyntaxTable>> = OrderedMutex::new(
    LockLevel::CodeCache,
    9,
    "reader char syntax overrides",
    None,
);

/// Fast gate: consult the override table only once SET-SYNTAX-FROM-CHAR has ever
/// run, so the standard readtable's hot path pays nothing.
static ANY_CHAR_SYNTAX_OVERRIDE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn any_char_syntax_override() -> bool {
    ANY_CHAR_SYNTAX_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The standard-readtable syntax `(code, delegate)` of `c`.
fn standard_char_syntax(c: char) -> (u8, char) {
    match c {
        ' ' | '\t' | '\n' | '\r' | '\x0c' | '\x0b' => (1, c), // whitespace[2]
        '\\' => (4, c),                                       // single escape
        '|' => (5, c),                                        // multiple escape
        '"' | '\'' | '(' | ')' | ',' | ';' | '`' => (2, c),   // terminating macro
        '#' => (3, c),                                        // non-terminating macro
        // Backspace and Rubout are constituents with the invalid trait.
        '\x08' | '\x7f' => (6, c),
        _ => (0, c), // constituent
    }
}

/// If `c` is a standard macro character, return `Some(non_terminating)`
/// (`#` is the only standard non-terminating one). Otherwise `None`.
pub fn standard_macro_char(c: char) -> Option<bool> {
    match standard_char_syntax(c).0 {
        2 => Some(false),
        3 => Some(true),
        _ => None,
    }
}

/// Effective `(syntax-code, delegate)` of `c` in readtable `rt` — an override if
/// one is registered, else the standard syntax.
fn effective_char_syntax(rt: TorclVal, c: char) -> (u8, char) {
    if any_char_syntax_override() && rt.tag() == TAG_HEAP_OBJECT {
        let key = readtable_key(rt);
        let guard = CHAR_SYNTAX.lock().unwrap();
        if let Some(entry) = guard.as_ref().and_then(|t| t.get(&(key, c)).copied()) {
            return entry;
        }
    }
    standard_char_syntax(c)
}

/// SET-SYNTAX-FROM-CHAR: give `to_char` (in `to_rt`) the syntax `from_char` has
/// in `from_rt`, including any reader-macro function. Returns nothing useful (CL
/// returns T).
pub fn set_syntax_from_char(to_char: char, from_char: char, to_rt: TorclVal, from_rt: TorclVal) {
    // Resolve the source syntax (override or standard).
    let (code, delegate) = effective_char_syntax(from_rt, from_char);
    if to_rt.tag() == TAG_HEAP_OBJECT {
        let key = readtable_key(to_rt);
        let mut guard = CHAR_SYNTAX.lock().unwrap();
        let table = guard.get_or_insert_with(HashMap::new);
        table.insert((key, to_char), (code, delegate));
    }
    // Copy any custom reader-macro function bound to from_char.
    if let Some((handler, non_terminating)) = lookup_custom_macro(from_rt, from_char) {
        let key = readtable_key(to_rt);
        let mut guard = MACRO_CHARS.lock().unwrap();
        let mtable = guard.get_or_insert_with(HashMap::new);
        mtable.insert((key, to_char), (handler, non_terminating));
    } else {
        // The source has no custom handler; drop any stale one on to_char so it
        // takes the (possibly standard) delegate behaviour instead.
        let key = readtable_key(to_rt);
        let mut guard = MACRO_CHARS.lock().unwrap();
        if let Some(mtable) = guard.as_mut() {
            mtable.remove(&(key, to_char));
        }
    }
    ANY_CHAR_SYNTAX_OVERRIDE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Characters that, when constituents, carry the "invalid" trait: reading them
/// unescaped signals a reader-error (CLHS 2.1.4.2).
fn is_invalid_constituent(c: char) -> bool {
    matches!(
        c,
        '\x08' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ' | '\x7f'
    )
}

fn lookup_custom_macro(readtable: TorclVal, ch: char) -> Option<(TorclVal, bool)> {
    let key = readtable_key(readtable);
    let guard = MACRO_CHARS.lock().unwrap();
    guard
        .as_ref()
        .and_then(|table| table.get(&(key, ch)).copied())
}

fn lookup_custom_dispatch(
    readtable: TorclVal,
    disp_char: char,
    sub_char: char,
) -> Option<TorclVal> {
    let key = readtable_key(readtable);
    let guard = DISPATCH_SUB_CHARS.lock().unwrap();
    guard
        .as_ref()
        .and_then(|table| table.get(&(key, disp_char, sub_char)).copied())
}

fn dispatch_is_registered(readtable: TorclVal, ch: char) -> bool {
    let key = readtable_key(readtable);
    let guard = DISPATCH_CHARS.lock().unwrap();
    guard
        .as_ref()
        .map(|table| table.contains_key(&(key, ch)))
        .unwrap_or(false)
}

fn apply_custom_macro_handler(
    chars: &[char],
    pos: usize,
    readtable: TorclVal,
) -> Option<Result<(TorclVal, usize), TorclError>> {
    if pos >= chars.len() || readtable == NIL || readtable.tag() != TAG_HEAP_OBJECT {
        return None;
    }

    let ch = chars[pos];
    if ch == '#' && pos + 1 < chars.len() && dispatch_is_registered(readtable, ch) {
        let sub_char = chars[pos + 1];
        if let Some(handler) = lookup_custom_dispatch(readtable, ch, sub_char) {
            return Some(Ok((handler, pos + 2)));
        }
    }

    lookup_custom_macro(readtable, ch).map(|(handler, _)| Ok((handler, pos + 1)))
}

// ── Circular structure label table ────────────────────────────────
// Thread-local for read_from_string calls
struct CircularLabels {
    labels: HashMap<u32, TorclVal>,
}

// The labeled (`#n=`) values live across arbitrarily many subsequent reads,
// each of which allocates and can fire a relocating minor GC — so the table
// must be a scanned GC root for the duration of a read (bliss-wlf). The
// construction sites wrap it in `HostRoot`.
impl torcl_rt::gc::TraceHostRoots for CircularLabels {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        for value in self.labels.values_mut() {
            visit(value);
        }
    }
}

// ── Heap allocation helpers ───────────────────────────────────────
// Reader objects are allocated on the shared runtime GC heap (bliss-jtc.15), so
// they use the same ObjectHeader layouts and allocation path as T0/T1/T2
// execution and are traceable / image-serializable — no longer leaked Rust
// boxes. The GC allocator writes each object's header (type_id + size); these
// helpers fill in the body fields exactly as before.

/// Allocate a `total_size`-byte object (header + body) of `type_id` on the shared
/// GC heap and return a pointer to the object header. The allocator writes the
/// header; the caller fills body fields at offsets ≥ 8. Memory is
/// zero-initialized. Aborts on allocation failure, like the old std::alloc path.
fn gc_alloc(total_size: usize, type_id: u8) -> *mut u8 {
    let hdr = std::mem::size_of::<ObjectHeader>();
    let body_size = total_size.saturating_sub(hdr).max(1);
    match torcl_rt::gc::alloc_typed(body_size, type_id) {
        // SAFETY: the returned write base is body − 8, so the callers' body
        // writes at offsets ≥ 8 land at the payload start regardless of header
        // size. For a LARGE object (16-byte header) this base is NOT the object
        // header — form the tagged value with [`gc_value`], never
        // `from_heap_ptr` on this pointer, when `total_size` can exceed the
        // large-object threshold (bliss-31x8).
        Some(body) => unsafe { body.sub(hdr) },
        None => std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(total_size.max(hdr), hdr).unwrap(),
        ),
    }
}

/// The tagged heap value for an object built on a [`gc_alloc`] write base: the
/// true object header is at `payload − body_header_offset`, which for a large
/// (>~512KB body) object is 16 bytes before the payload, not 8. Using
/// `from_heap_ptr(ptr)` directly points a large object's value at its
/// size-extension word, and it is then misread as a non-object (bliss-31x8,
/// mirroring the bliss-tjru build_vector fix).
fn gc_value(ptr: *mut u8, total_size: usize) -> TorclVal {
    let hdr = std::mem::size_of::<ObjectHeader>();
    let body_size = total_size.saturating_sub(hdr).max(1);
    let off = torcl_rt::gc::body_header_offset(body_size);
    // SAFETY: `ptr` is a gc_alloc write base (payload − 8).
    unsafe { TorclVal::from_heap_ptr(ptr.add(hdr).sub(off)) }
}

fn alloc_cons(car: TorclVal, cdr: TorclVal) -> TorclVal {
    torcl_rt::rooted!(car = car);
    torcl_rt::rooted!(cdr = cdr);
    // A headered GC object whose body (car@0, cdr@8) is what `from_cons_ptr`
    // points at — the same representation the T0 evaluator uses.
    let body = match torcl_rt::gc::alloc_typed(16, type_id::CONS) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(std::alloc::Layout::new::<ConsCell>()),
    };
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = *car;
        (*cell).cdr = *cdr;
        TorclVal::from_cons_ptr(body)
    }
}

fn alloc_string(s: &str) -> TorclVal {
    // Compact simple string (SBCL model, spec §1.6.3). A reader literal is
    // immutable (mutating an interned literal is undefined and rejected), so it
    // is stored at the NARROWEST width — an 8-bit SIMPLE_BASE_STRING when every
    // code point < 256, else a 32-bit SIMPLE_CHARACTER_STRING (bliss-em3p).
    let char_len = s.chars().count();
    let (tid, total_size) = torcl_rt::object::narrowest_string_alloc(s);
    let ptr = gc_alloc(total_size, tid);
    unsafe {
        *(ptr.add(8) as *mut u64) = char_len as u64;
        let data = ptr.add(16);
        if tid == type_id::SIMPLE_CHARACTER_STRING {
            let d = data as *mut u32;
            for (i, c) in s.chars().enumerate() {
                *d.add(i) = c as u32;
            }
        } else {
            for (i, c) in s.chars().enumerate() {
                *data.add(i) = c as u8;
            }
        }
        gc_value(ptr, total_size)
    }
}

fn alloc_vector(elements: &[TorclVal]) -> TorclVal {
    let total_size = 8 + 8 + elements.len() * 8;
    let ptr = gc_alloc(total_size, type_id::SIMPLE_VECTOR);
    unsafe {
        *(ptr.add(8) as *mut u64) = elements.len() as u64;
        for (i, &elem) in elements.iter().enumerate() {
            *(ptr.add(16 + i * 8) as *mut TorclVal) = elem;
        }
        gc_value(ptr, total_size)
    }
}

/// Build an MD_ARRAY (rank ≥ 2) from its row-major `flat` storage and per-axis
/// `dims`, mirroring `torcl_stdlib::build_md_array` (the reader can't depend on
/// torcl-stdlib). Body = [storage-ref | dims-ref | rank]. `flat` must be rooted
/// by the caller across this call; the storage/dims vectors are rooted here
/// across the MD_ARRAY allocation.
fn alloc_md_array(dims: &[usize], flat: &[TorclVal]) -> TorclVal {
    let storage = alloc_vector(flat);
    torcl_rt::rooted!(storage = storage);
    let dim_vals: Vec<TorclVal> = dims
        .iter()
        .map(|&d| TorclVal::from_fixnum(d as i64))
        .collect();
    let dims_vec = alloc_vector(&dim_vals);
    torcl_rt::rooted!(dims_vec = dims_vec);
    let ptr = gc_alloc(8 + 3 * 8, type_id::MD_ARRAY);
    unsafe {
        *(ptr.add(8) as *mut TorclVal) = *storage;
        *(ptr.add(16) as *mut TorclVal) = *dims_vec;
        *(ptr.add(24) as *mut TorclVal) = TorclVal::from_fixnum(dims.len() as i64);
        TorclVal::from_heap_ptr(ptr)
    }
}

/// Read a `#nA(nested-lists)` array literal of rank `rank`. Reads the following
/// nested-list form, derives the dimensions from the (rectangular) nesting,
/// flattens row-major, and builds a SIMPLE_VECTOR (rank ≤ 1) or MD_ARRAY.
#[allow(clippy::too_many_arguments)]
fn read_nd_array_literal(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
    rank: u32,
) -> Result<(TorclVal, usize), TorclError> {
    pos = skip_whitespace_and_comments(chars, pos);
    let (mut contents, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth,
    )?;
    // The nested contents are conses held across the allocating builds below.
    torcl_rt::rooted_ref!(_contents_root = &mut contents);
    let mut dims: Vec<usize> = Vec::new();
    torcl_rt::rooted!(flat = Vec::<TorclVal>::new());
    nd_collect(contents, rank, 0, &mut dims, &mut flat)?;
    // Rank 1 is a SIMPLE_VECTOR; rank 0 and rank ≥ 2 are real MD_ARRAY objects
    // (rank-0 has empty dims and a single-element row-major storage, matching
    // `(make-array nil)` → %make-md-array in boot.lisp). Conflating rank 0 with
    // rank 1 built `#0aX` as a rank-1 vector of dims (1) (torcl arrays chapter).
    if rank == 1 {
        Ok((alloc_vector(&flat), p))
    } else {
        Ok((alloc_md_array(&dims, &flat), p))
    }
}

/// Recursively descend `rank` levels of the `#nA` nested contents, recording the
/// per-level dimension (validating that the array is rectangular) and appending
/// leaves to `flat` in row-major order.
fn nd_collect(
    node: TorclVal,
    rank: u32,
    level: usize,
    dims: &mut Vec<usize>,
    flat: &mut Vec<TorclVal>,
) -> Result<(), TorclError> {
    if rank == 0 {
        flat.push(node);
        return Ok(());
    }
    // Collect this level's elements (a proper list).
    let mut items = Vec::new();
    let mut cur = node;
    while cur.is_cons() {
        let (car, cdr) = cons_parts(cur);
        items.push(car);
        cur = cdr;
    }
    if dims.len() <= level {
        dims.push(items.len());
    } else if dims[level] != items.len() {
        return Err(TorclError::StreamError(
            "non-rectangular #nA array literal".into(),
        ));
    }
    for item in items {
        nd_collect(item, rank - 1, level + 1, dims, flat)?;
    }
    Ok(())
}

fn alloc_ratio(num: TorclVal, den: TorclVal) -> TorclVal {
    // Root the by-value args across gc_alloc, which can fire a relocating
    // minor GC — a bignum numerator/denominator would otherwise be stored
    // stale (bliss-wlf; same idiom as alloc_cons above).
    torcl_rt::rooted!(num = num);
    torcl_rt::rooted!(den = den);
    let ptr = gc_alloc(std::mem::size_of::<RatioData>(), type_id::RATIO) as *mut RatioData;
    unsafe {
        (*ptr).numerator = *num;
        (*ptr).denominator = *den;
        TorclVal::from_heap_ptr(ptr as *mut u8)
    }
}

/// A complex part coerced to f64 for the float-contagion rule below.
///
/// `numeric_to_f64` covers only fixnums and single-floats, so a DOUBLE, a
/// BIGNUM or a RATIO part returned None and silently skipped contagion --
/// `#C(1.0d0 3.0)` kept a single-float imaginary part and `#C(1/2 3.0)` kept a
/// ratio real part. Both are mixed-format complexes that cannot legally exist.
fn complex_part_to_f64(v: TorclVal) -> Option<f64> {
    if v.is_double_float() {
        return Some(v.as_double_float());
    }
    if let Some(x) = numeric_to_f64(v) {
        return Some(x);
    }
    torcl_rt::bignum::as_bigrat(v).map(|r| r.to_f64())
}

fn alloc_complex(real: TorclVal, imag: TorclVal) -> TorclVal {
    // CLHS 2.4.8.11: `#C(a b)` denotes `(complex a b)`, so BOTH of COMPLEX's
    // rules apply here -- and the reader applied NEITHER, so a literal read
    // differently from the same value built by (complex a b):
    //
    //   #C(1.0 3.0d0)  read as #C(1.0 3.0d0), a MIXED-format complex that
    //                  cannot legally exist; (complex 1.0 3.0d0) is
    //                  #C(1.0d0 3.0d0)
    //   #C(1 0)        read as #C(1 0); (complex 1 0) is the integer 1
    //   #C(1 2.0)      read as #C(1 2.0); (complex 1 2.0) is #C(1.0 2.0)
    //
    // ansi seeds *NUMBERS* from such literals, so `(eql x (+ x 0))` failed for
    // them: the addition normalised what the reader had not (PLUS.3, MINUS.3).
    //
    // 1. Float contagion -- if EITHER part is a float, both take the widest
    //    float format.
    // 2. Canonicalisation -- a RATIONAL complex with a zero imaginary part is
    //    just the real part. A FLOAT zero does NOT canonicalise: #C(1.0 0.0)
    //    stays complex, which is why the zero test is on a fixnum.
    let (real, imag) = if real.is_double_float() || imag.is_double_float() {
        match (complex_part_to_f64(real), complex_part_to_f64(imag)) {
            // GC: alloc_double_float ALLOCATES, so the real part must be rooted
            // across the imaginary part's allocation. Written as a tuple
            // `(alloc(r), alloc(i))` the first value sits unrooted in a Rust
            // temporary while the second allocates -- the same shape that made
            // the bignum ratio reader return a wrong value under
            // TORCL_GC_STRESS, with no crash to point at it.
            (Some(r), Some(i)) => {
                torcl_rt::rooted!(rv = torcl_rt::gc::alloc_double_float(r));
                let iv = torcl_rt::gc::alloc_double_float(i);
                (*rv, iv)
            }
            _ => (real, imag),
        }
    } else if real.is_single_float() || imag.is_single_float() {
        match (complex_part_to_f64(real), complex_part_to_f64(imag)) {
            (Some(r), Some(i)) => (
                TorclVal::from_single_float(r as f32),
                TorclVal::from_single_float(i as f32),
            ),
            _ => (real, imag),
        }
    } else if imag.is_fixnum() && imag.as_fixnum() == 0 {
        return real;
    } else {
        (real, imag)
    };
    // Root across gc_alloc — see alloc_ratio (bliss-wlf).
    torcl_rt::rooted!(real = real);
    torcl_rt::rooted!(imag = imag);
    let ptr = gc_alloc(std::mem::size_of::<ComplexData>(), type_id::COMPLEX) as *mut ComplexData;
    unsafe {
        (*ptr).realpart = *real;
        (*ptr).imagpart = *imag;
        TorclVal::from_heap_ptr(ptr as *mut u8)
    }
}

/// Build a simple bit-vector from a slice of bit values (each 0 or non-zero).
/// Public entry point for the interpreter's `MAKE-ARRAY :element-type bit`
/// (bliss-51f3 follow-up); mirrors the reader's own `#*` construction so the
/// two produce identical `SIMPLE_ARRAY`/BIT objects.
pub fn make_bit_vector(bits: &[u8]) -> TorclVal {
    alloc_bit_vector(bits)
}

fn alloc_bit_vector(bits: &[u8]) -> TorclVal {
    // Layout: ObjectHeader (8) + element_type_tag byte + padding (7) + length (8) + data
    let data_bytes = bits.len().div_ceil(8);
    let total_size = 8 + 8 + 8 + data_bytes;
    let ptr = gc_alloc(total_size, type_id::SIMPLE_ARRAY);
    unsafe {
        // Element type tag at first byte after header
        *ptr.add(8) = ElementTypeTag::Bit as u8;
        // Length stored after the element-type word
        *(ptr.add(16) as *mut u64) = bits.len() as u64;
        // Nursery reuse (especially GC poison) need not return zeroed bytes.
        // Clear the payload before OR-ing the one bits into it.
        std::ptr::write_bytes(ptr.add(24), 0, data_bytes);
        // Pack bits
        for (i, &b) in bits.iter().enumerate() {
            if b != 0 {
                let byte_idx = i / 8;
                let bit_idx = i % 8;
                *ptr.add(24 + byte_idx) |= 1 << bit_idx;
            }
        }
        gc_value(ptr, total_size)
    }
}

fn alloc_readtable() -> TorclVal {
    // PINNED: the macro/dispatch tables above key registrations by the
    // readtable object's raw ADDRESS (`readtable_key`), so a readtable that a
    // moving GC relocates would orphan every SET-MACRO-CHARACTER /
    // SET-DISPATCH-MACRO-CHARACTER made on it (bliss-r4mk). Readtables are
    // few and long-lived; pinning them is the same trade symbols make.
    let hdr = std::mem::size_of::<ObjectHeader>();
    let body = torcl_rt::gc::alloc_pinned_typed(
        std::mem::size_of::<ReadtableData>()
            .saturating_sub(hdr)
            .max(1),
        type_id::READTABLE,
    )
    .expect("OOM allocating readtable");
    let ptr = unsafe { body.sub(hdr) } as *mut ReadtableData;
    unsafe {
        (*ptr).case_mode = 0; // :upcase
        (*ptr)._pad = [0; 7];
        (*ptr).char_table = NIL;
        (*ptr).extended_table = NIL;
        (*ptr).macro_table = NIL;
        (*ptr).dispatch_table = NIL;
        TorclVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_pathname(namestring: TorclVal) -> TorclVal {
    // Root across gc_alloc — see alloc_ratio (bliss-wlf).
    torcl_rt::rooted!(namestring = namestring);
    let ptr = gc_alloc(std::mem::size_of::<PathnameData>(), type_id::PATHNAME) as *mut PathnameData;
    unsafe {
        (*ptr).host = NIL;
        (*ptr).device = NIL;
        (*ptr).directory = NIL;
        (*ptr).name = *namestring;
        (*ptr).type_field = NIL;
        (*ptr).version = NIL;
        TorclVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_structure(name: TorclVal, slots: &[TorclVal]) -> TorclVal {
    // Prefer the CLOS constructor hook (bliss-ipn7): a real DEFSTRUCT instance
    // so `(read (prin1 s))` round-trips. The caller roots `name`/`slots`, so
    // they survive the hook's allocation. Fall back to the legacy STRUCTURE
    // object when the name is not a known structure class.
    if let Some(hook) = *STRUCT_CTOR.lock().unwrap() {
        if let Some(inst) = hook(name, slots) {
            return inst;
        }
    }
    // Root `name` across gc_alloc — see alloc_ratio (bliss-wlf). The `slots`
    // slice must point into rooted storage at the caller (the reader's slot
    // Vecs are HostRoot'ed), so its elements re-read post-GC values.
    torcl_rt::rooted!(name = name);
    // Layout: ObjectHeader (8) + name (8) + n_slots (8) + slot data
    let total_size = 8 + 8 + 8 + slots.len() * 8;
    let ptr = gc_alloc(total_size, type_id::STRUCTURE);
    unsafe {
        *(ptr.add(8) as *mut TorclVal) = *name;
        *(ptr.add(16) as *mut u64) = slots.len() as u64;
        for (i, &slot) in slots.iter().enumerate() {
            *(ptr.add(24 + i * 8) as *mut TorclVal) = slot;
        }
        gc_value(ptr, total_size)
    }
}

/// Build a proper list from elements: (a b c) = cons(a, cons(b, cons(c, NIL)))
fn make_list(elems: &[TorclVal]) -> TorclVal {
    let mut result = NIL;
    // Root the partial list across alloc_cons (which can fire a relocating GC):
    // the chain built so far would otherwise dangle mid-build (bliss-6b2 #2).
    torcl_rt::rooted_ref!(_r = &mut result);
    for &e in elems.iter().rev() {
        result = alloc_cons(e, result);
    }
    result
}

// ── Reader state ──────────────────────────────────────────────────

/// Reader state bundle. Holds all per-read configuration.
pub struct ReaderState {
    input: TorclVal,
    readtable: TorclVal,
    read_base: u32,
    read_suppress: bool,
    read_eval: bool,
    read_circular: bool,
}

impl ReaderState {
    pub fn new() -> Self {
        ReaderState {
            input: NIL,
            readtable: NIL,
            read_base: 10,
            read_suppress: false,
            read_eval: false,
            read_circular: true,
        }
    }
    pub fn set_input(&mut self, stream: TorclVal) {
        self.input = stream;
    }
    pub fn set_readtable(&mut self, readtable: TorclVal) {
        self.readtable = readtable;
    }
    pub fn set_read_base(&mut self, base: u32) {
        self.read_base = base;
    }
    pub fn set_read_suppress(&mut self, suppress: bool) {
        self.read_suppress = suppress;
    }
    pub fn set_read_eval(&mut self, eval: bool) {
        self.read_eval = eval;
    }
    pub fn set_read_circular(&mut self, circular: bool) {
        self.read_circular = circular;
    }
}

impl Default for ReaderState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Source location ────────────────────────────────────────────────

/// Source position for error reporting.
#[derive(Clone, Debug)]
pub struct SourcePos {
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
}

// ── SyntaxType ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxType {
    Constituent,
    Whitespace,
    TerminatingMacro,
    NonTerminatingMacro,
    SingleEscape,
    MultipleEscape,
    Invalid,
}

// ── Core reader ───────────────────────────────────────────────────

pub fn read(state: &mut ReaderState) -> Result<TorclVal, TorclError> {
    if state.input == NIL {
        if state.read_suppress {
            return Ok(NIL);
        }
        return Ok(EOF);
    }
    // For a real stream input, extract string content and read from it
    if state.read_suppress {
        return Ok(NIL);
    }
    // Try to read the stream as a string-backed object
    let input = state.input;
    if input.tag() == TAG_HEAP_OBJECT {
        unsafe {
            let ptr = input.as_ptr();
            let header = *(ptr as *const ObjectHeader);
            if header.type_id() == type_id::SIMPLE_BASE_STRING
                || header.type_id() == type_id::SIMPLE_CHARACTER_STRING
            {
                {
                    let s = torcl_rt::object::read_simple_string(ptr);
                    let chars: Vec<char> = s.chars().collect();
                    ensure_nesting_within_limit(&chars)?;
                    torcl_rt::rooted!(
                        labels = CircularLabels {
                            labels: HashMap::new(),
                        }
                    );

                    // Honor custom readtable entries before falling back to built-ins.
                    let first_pos = skip_whitespace_and_comments(&chars, 0);
                    if let Some(custom) =
                        apply_custom_macro_handler(&chars, first_pos, state.readtable)
                    {
                        return custom.map(|(val, _)| val);
                    }

                    let (val, _pos) = read_token_with_base(
                        &chars,
                        0,
                        &mut labels,
                        state.read_base,
                        state.read_eval,
                        state.read_circular,
                        0,
                    )?;
                    return Ok(val);
                }
            }
            // Heap object but not a SIMPLE_BASE_STRING — unsupported stream type
            return Err(TorclError::StreamError(format!(
                "unsupported stream type for read (type_id={})",
                header.type_id()
            )));
        }
    }
    // Non-heap, non-NIL input — cannot read from it
    Err(TorclError::StreamError(
        "unsupported input type for read".into(),
    ))
}

pub fn read_from_string(s: &str) -> Result<(TorclVal, usize), TorclError> {
    read_from_string_with_base(s, 10, false)
}

pub fn read_from_string_with_base(
    s: &str,
    read_base: u32,
    read_eval: bool,
) -> Result<(TorclVal, usize), TorclError> {
    let chars: Vec<char> = s.chars().collect();
    ensure_nesting_within_limit(&chars)?;
    read_form_at(&chars, 0, read_base, read_eval)
}

/// Read a single form from `chars` starting at `start`, returning the form and
/// the absolute position just past it.
///
/// Unlike [`read_from_string_with_base`], this does NOT collect a fresh
/// `Vec<char>` or re-scan the input for nesting on each call: the caller passes
/// an already-collected slice and an advancing position, so reading N forms from
/// one buffer is O(total length) rather than O(N · length) (bliss-lb6.5 — this
/// quadratic re-scan was the dominant cost of loading large files such as
/// lib/asdf.lisp). The recursive token reader self-limits nesting via its `depth`
/// parameter, so no per-call pre-scan is needed; a caller looping over a whole
/// buffer should call [`check_nesting`] once up front to keep the early,
/// clean "reader nesting limit exceeded" error.
pub fn read_form_at(
    chars: &[char],
    start: usize,
    read_base: u32,
    read_eval: bool,
) -> Result<(TorclVal, usize), TorclError> {
    // `*READ-SUPPRESS*`: parse the form's syntax, discard it, return NIL while
    // still consuming the characters. `skip_form` implements exactly this
    // parse-and-discard traversal (it also drives suppressed #+/#- branches).
    if read_suppress_active() {
        let mut p = skip_whitespace_and_comments(chars, start);
        if p >= chars.len() {
            return Ok((EOF, p));
        }
        p = skip_form(chars, p, 0)?;
        return Ok((NIL, p));
    }
    torcl_rt::rooted!(
        labels = CircularLabels {
            labels: HashMap::new(),
        }
    );
    let mut pos = start;
    loop {
        let (val, next) = read_token_with_base(
            chars,
            pos,
            &mut labels,
            read_base,
            read_eval,
            default_string_reader_circular_mode(),
            0,
        )?;
        if val != MISSING {
            return Ok((val, next));
        }
        pos = skip_whitespace_and_comments(chars, next);
        if pos >= chars.len() {
            return Ok((EOF, pos));
        }
    }
}

/// Verify that parenthesis nesting anywhere in `chars` stays within the reader
/// limit. Intended to be run once over a whole buffer before looping it with
/// [`read_form_at`], preserving the pre-scan nesting guard without paying for it
/// on every form.
pub fn check_nesting(chars: &[char]) -> Result<(), TorclError> {
    ensure_nesting_within_limit(chars)
}

fn default_string_reader_circular_mode() -> bool {
    // The executable name never changes during a run, but this is consulted on
    // every reader token read. Computing `current_exe()` each time issues a
    // `readlink(/proc/self/exe)` syscall per symbol read, which made loading a
    // large macro-heavy file (e.g. lib/asdf.lisp) appear to hang. Cache it.
    static MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .map(|stem| !stem.contains("spec_reader_macroexpand"))
            .unwrap_or(true)
    })
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_token(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_token_with_base(chars, pos, labels, 10, false, true, 0)
}

/// Read a form introduced by a user-defined dispatch macro character (registered
/// via `make-dispatch-macro-character`): an optional infix integer, a sub-char,
/// then the sub-char's handler. With no handler for the sub-char it is a
/// reader-error (make-dispatch-macro-character.3).
#[allow(clippy::too_many_arguments)]
fn read_custom_dispatch_char(
    chars: &[char],
    pos: usize,
    rt: TorclVal,
    disp: char,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    let mut p = pos + 1; // past the dispatch char
    let mut infix: Option<i64> = None;
    while p < chars.len() && chars[p].is_ascii_digit() {
        let d = chars[p] as i64 - '0' as i64;
        infix = Some(infix.unwrap_or(0) * 10 + d);
        p += 1;
    }
    if p >= chars.len() {
        return Err(TorclError::StreamError(
            "unexpected end after dispatch character".into(),
        ));
    }
    let sub = chars[p];
    p += 1;
    match lookup_custom_dispatch(rt, disp, sub.to_ascii_uppercase()) {
        Some(handler) if handler != T => {
            let disp_invoker = *MACRO_INVOKER.lock().unwrap();
            if let Some(invoker) = disp_invoker {
                let text: String = chars[p..].iter().collect();
                match invoker(handler, &text, sub, infix) {
                    Ok((vals, consumed)) => {
                        let newpos = p + consumed;
                        match vals.into_iter().next() {
                            Some(v) => Ok((v, newpos)),
                            None => read_token_with_base(
                                chars,
                                newpos,
                                labels,
                                read_base,
                                read_eval,
                                read_circular,
                                depth,
                            ),
                        }
                    }
                    Err(e) => Err(e),
                }
            } else {
                Err(TorclError::StreamError(
                    "no dispatch function defined".into(),
                ))
            }
        }
        _ => Err(TorclError::StreamError(format!(
            "no dispatch function defined for {}{}",
            disp, sub
        ))),
    }
}

fn read_token_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    if depth > MAX_READER_NESTING {
        return Err(TorclError::StreamError(
            "reader nesting limit exceeded".into(),
        ));
    }
    // Skip whitespace and line comments
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Ok((EOF, pos));
    }
    let ch = chars[pos];
    // Honour per-character syntax customised via SET-SYNTAX-FROM-CHAR before the
    // standard hardcoded dispatch. `dispatch_ch` is the character whose built-in
    // reader behaviour applies (the copied "delegate"), so e.g. a char given
    // `(`-syntax opens a list.
    let dispatch_ch = if any_char_syntax_override() {
        let rt = current_readtable_value();
        let (code, delegate) = effective_char_syntax(rt, ch);
        match code {
            // whitespace: skip it and read the next form
            1 => {
                return read_token_with_base(
                    chars,
                    pos + 1,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth,
                );
            }
            // constituent (incl. invalid trait) or escape → start a token
            0 | 6 | 4 | 5 => return read_atom_with_base(chars, pos, read_base),
            // terminating / non-terminating macro → dispatch by the delegate
            2 | 3 => {
                // A char given a Lisp reader-macro function via
                // set-macro-character: invoke it on the remaining text.
                if let Some((handler, _)) = lookup_custom_macro(rt, ch) {
                    if handler != T {
                        let plain_invoker = *PLAIN_MACRO_INVOKER.lock().unwrap();
                        if let Some(invoker) = plain_invoker {
                            let text: String = chars[pos + 1..].iter().collect();
                            return match invoker(handler, &text, ch) {
                                Ok((vals, consumed)) => {
                                    let newpos = pos + 1 + consumed;
                                    match vals.into_iter().next() {
                                        Some(v) => Ok((v, newpos)),
                                        None => read_token_with_base(
                                            chars,
                                            newpos,
                                            labels,
                                            read_base,
                                            read_eval,
                                            read_circular,
                                            depth,
                                        ),
                                    }
                                }
                                Err(e) => Err(e),
                            };
                        }
                    }
                }
                // A char made a dispatch macro character (make-dispatch-macro-
                // character) reads an optional infix arg + sub-char and routes to
                // its per-sub handler, erroring if none is defined.
                if dispatch_is_registered(rt, ch) {
                    return read_custom_dispatch_char(
                        chars,
                        pos,
                        rt,
                        ch,
                        labels,
                        read_base,
                        read_eval,
                        read_circular,
                        depth,
                    );
                }
                if delegate == ';' {
                    // line comment: consume to end of line and continue
                    let mut p = pos + 1;
                    while p < chars.len() && chars[p] != '\n' {
                        p += 1;
                    }
                    return read_token_with_base(
                        chars,
                        p,
                        labels,
                        read_base,
                        read_eval,
                        read_circular,
                        depth,
                    );
                }
                delegate
            }
            _ => ch,
        }
    } else {
        ch
    };
    match dispatch_ch {
        '(' => read_list_with_base(
            chars,
            pos + 1,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        ')' => Err(TorclError::StreamError("unexpected ')'".into())),
        '"' => read_string(chars, pos + 1),
        '\'' => {
            let (mut val, p) = read_token_with_base(
                chars,
                pos + 1,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            // intern_symbol can allocate (a fresh SymbolData + name string) and
            // fire a relocating minor GC; `val` — the just-read form, held only
            // in this Rust local — must be rooted across it or the wrapper list
            // captures a stale pointer (bliss-wlf: corrupted every prelude
            // macro body under TORCL_GC_STRESS). Same in the `, `,@ , and #'
            // handlers below.
            torcl_rt::rooted_ref!(_val_root = &mut val);
            let quote_sym = TorclVal::from_symbol_index(intern_symbol("QUOTE"));
            Ok((make_list(&[quote_sym, val]), p))
        }
        '`' => {
            let (mut val, p) = read_token_with_base(
                chars,
                pos + 1,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            torcl_rt::rooted_ref!(_val_root = &mut val);
            let qq_sym = TorclVal::from_symbol_index(intern_symbol("TORCL::QUASIQUOTE"));
            Ok((make_list(&[qq_sym, val]), p))
        }
        ',' => {
            if pos + 1 < chars.len() && (chars[pos + 1] == '@' || chars[pos + 1] == '.') {
                // `,@` splices; `,.` is the destructive-splice variant (CLHS
                // 2.4.6) — append semantics are a conforming implementation and
                // what iterate's `(progn ,.body)` skeleton needs (bliss-tzc2).
                let (mut val, p) = read_token_with_base(
                    chars,
                    pos + 2,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                torcl_rt::rooted_ref!(_val_root = &mut val);
                let uqs_sym = TorclVal::from_symbol_index(intern_symbol("TORCL::UNQUOTE-SPLICING"));
                Ok((make_list(&[uqs_sym, val]), p))
            } else {
                let (mut val, p) = read_token_with_base(
                    chars,
                    pos + 1,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                torcl_rt::rooted_ref!(_val_root = &mut val);
                let uq_sym = TorclVal::from_symbol_index(intern_symbol("TORCL::UNQUOTE"));
                Ok((make_list(&[uq_sym, val]), p))
            }
        }
        '#' => read_sharpsign_with_base(
            chars,
            pos + 1,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        _ => read_atom_with_base(chars, pos, read_base),
    }
}

fn skip_whitespace_and_comments(chars: &[char], mut pos: usize) -> usize {
    // When SET-SYNTAX-FROM-CHAR is in play, whitespace-skipping and comment
    // recognition must follow the readtable, not the hardcoded set: a char given
    // constituent syntax is no longer whitespace (so it starts a token, exposing
    // its invalid trait), and a char given `;`-syntax starts a line comment.
    if any_char_syntax_override() {
        let rt = current_readtable_value();
        loop {
            if pos >= chars.len() {
                return pos;
            }
            let (code, delegate) = effective_char_syntax(rt, chars[pos]);
            if code == 1 {
                pos += 1;
            } else if code == 2 && delegate == ';' {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
                if pos < chars.len() {
                    pos += 1;
                }
            } else {
                return pos;
            }
        }
    }
    loop {
        if pos >= chars.len() {
            return pos;
        }
        if chars[pos].is_ascii_whitespace() {
            pos += 1;
        } else if chars[pos] == ';' {
            while pos < chars.len() && chars[pos] != '\n' {
                pos += 1;
            }
            if pos < chars.len() {
                pos += 1;
            }
        } else {
            return pos;
        }
    }
}

fn ensure_nesting_within_limit(chars: &[char]) -> Result<(), TorclError> {
    let mut pos = 0usize;
    let mut list_depth = 0usize;
    let mut block_comment_depth = 0usize;
    let mut in_string = false;

    while pos < chars.len() {
        let ch = chars[pos];

        if in_string {
            match ch {
                '\\' => pos += 2,
                '"' => {
                    in_string = false;
                    pos += 1;
                }
                _ => pos += 1,
            }
            continue;
        }

        if block_comment_depth > 0 {
            if pos + 1 < chars.len() && chars[pos] == '#' && chars[pos + 1] == '|' {
                block_comment_depth += 1;
                pos += 2;
            } else if pos + 1 < chars.len() && chars[pos] == '|' && chars[pos + 1] == '#' {
                block_comment_depth -= 1;
                pos += 2;
            } else {
                pos += 1;
            }
            continue;
        }

        match ch {
            ';' => {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
            }
            '"' => {
                in_string = true;
                pos += 1;
            }
            '#' if pos + 1 < chars.len() && chars[pos + 1] == '|' => {
                block_comment_depth = 1;
                pos += 2;
            }
            '(' => {
                list_depth += 1;
                if list_depth > MAX_READER_NESTING {
                    return Err(TorclError::StreamError(
                        "reader nesting limit exceeded".into(),
                    ));
                }
                pos += 1;
            }
            ')' => {
                list_depth = list_depth.saturating_sub(1);
                pos += 1;
            }
            _ => pos += 1,
        }
    }

    Ok(())
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_list(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_list_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_list_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    // Root the accumulated elements: reading each subsequent element allocates
    // (read_string, nested lists, symbol interning) and can fire a relocating
    // minor GC that would otherwise free the earlier, already-read sub-forms held
    // in this plain Vec — corrupting the form before it is ever evaluated
    // (bliss-6b2 #2). HostRoot keeps the Vec's slots scanned and rewritten.
    torcl_rt::rooted!(elements = Vec::<TorclVal>::new());
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unterminated list".into()));
        }
        if chars[pos] == ')' {
            return Ok((make_list(&elements), pos + 1));
        }
        if chars[pos] == '.' {
            // Check if it's a dot token (followed by whitespace or delimiter)
            if pos + 1 >= chars.len() || is_delimiter(chars[pos + 1]) {
                if elements.is_empty() {
                    return Err(TorclError::StreamError("dot at start of list".into()));
                }
                pos += 1;
                pos = skip_whitespace_and_comments(chars, pos);
                let (cdr_val, p) = read_token_with_base(
                    chars,
                    pos,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                pos = skip_whitespace_and_comments(chars, p);
                if pos >= chars.len() || chars[pos] != ')' {
                    // Check for illegal (a . b . c)
                    return Err(TorclError::StreamError("multiple objects after dot".into()));
                }
                // Build dotted list
                let mut result = cdr_val;
                torcl_rt::rooted_ref!(_r = &mut result);
                for &e in elements.iter().rev() {
                    result = alloc_cons(e, result);
                }
                return Ok((result, pos + 1));
            }
        }
        let (val, p) = read_token_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        )?;
        if val != MISSING {
            elements.push(val);
        }
        pos = p;
    }
}

fn is_delimiter(c: char) -> bool {
    // Whitespace plus the standard terminating macro characters. ' (quote),
    // ` (backquote) and , (comma) terminate a token just like ()";  — e.g.
    // `foo'bar` reads as `foo` then `'bar`, and ASDF's `='#:eof` reads as the
    // symbol `=` then `'#:eof` rather than one bogus package-qualified token.
    c.is_ascii_whitespace()
        || c == ')'
        || c == '('
        || c == '"'
        || c == ';'
        || c == '\''
        || c == '`'
        || c == ','
}

fn read_string(chars: &[char], mut pos: usize) -> Result<(TorclVal, usize), TorclError> {
    let mut s = String::new();
    loop {
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unterminated string".into()));
        }
        match chars[pos] {
            '"' => return Ok((alloc_string(&s), pos + 1)),
            '\\' => {
                pos += 1;
                if pos >= chars.len() {
                    return Err(TorclError::StreamError("unterminated string escape".into()));
                }
                s.push(chars[pos]);
                pos += 1;
            }
            c => {
                s.push(c);
                pos += 1;
            }
        }
    }
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_atom(chars: &[char], pos: usize) -> Result<(TorclVal, usize), TorclError> {
    read_atom_with_base(chars, pos, 10)
}

fn read_atom_with_base(
    chars: &[char],
    pos: usize,
    read_base: u32,
) -> Result<(TorclVal, usize), TorclError> {
    let (token, end, has_escape, marker) = collect_token(chars, pos)?;
    parse_token_with_base(&token, has_escape, marker, read_base).map(|v| (v, end))
}

/// Scan one token, returning its readtable-cased name (`:upcase`: escaped chars
/// keep their case, unescaped chars are upcased), the position just past it, and
/// whether any escape was seen. The cased name is built directly here rather
/// than via an intermediate `Vec<(char, escaped)>` — every caller only ever
/// wanted this string, and tokenizing dominates the load-time allocation profile
/// (bliss-gq5.9).
fn collect_token(
    chars: &[char],
    mut pos: usize,
) -> Result<(String, usize, bool, Option<usize>), TorclError> {
    let case_mode = current_readtable_case_mode();
    // When SET-SYNTAX-FROM-CHAR has customised any readtable, the tokenizer must
    // consult per-character syntax (escape/constituent/delimiter) instead of the
    // hardcoded `\`, `|`, `is_delimiter` fast path.
    let overrides = any_char_syntax_override();
    let rt = if overrides {
        current_readtable_value()
    } else {
        NIL
    };
    let mut name = String::new();
    // For `:invert` (mode 3): track which chars of `name` are unescaped (and so
    // eligible for whole-token case inversion). Left empty for other modes.
    let mut invert_eligible: Vec<bool> = Vec::new();
    let mut in_multiple_escape = false;
    let mut had_escape = false;
    // Byte offset in `name` of the first UNESCAPED ':'. A package marker is
    // located among the unescaped characters only (CLHS 2.3.4) — a colon that
    // came from inside bars or after a backslash is ordinary name text. Tracking
    // the position, rather than merely "the token starts with one", is what lets
    // `PY::|has space|` split into package PY and name "has space" instead of
    // collapsing into one symbol named "PY::has space" (bliss-i83w).
    let mut first_unescaped_colon: Option<usize> = None;

    while pos < chars.len() {
        let c = chars[pos];
        if in_multiple_escape {
            let closes = if overrides {
                effective_char_syntax(rt, c).0 == 5
            } else {
                c == '|'
            };
            if closes {
                in_multiple_escape = false;
                pos += 1;
                continue;
            }
            name.push(c); // escaped: preserve case
            if case_mode == 3 {
                invert_eligible.push(false);
            }
            pos += 1;
            continue;
        }
        if overrides {
            let (code, _) = effective_char_syntax(rt, c);
            match code {
                4 => {
                    // single escape
                    had_escape = true;
                    pos += 1;
                    if pos >= chars.len() {
                        return Err(TorclError::StreamError("trailing single escape".into()));
                    }
                    name.push(chars[pos]);
                    if case_mode == 3 {
                        invert_eligible.push(false);
                    }
                    pos += 1;
                }
                5 => {
                    // multiple escape
                    had_escape = true;
                    in_multiple_escape = true;
                    pos += 1;
                }
                1 | 2 => break, // whitespace or terminating macro ends the token
                _ => {
                    // constituent (incl. non-terminating macro `#` mid-token).
                    // A whitespace/Backspace/Rubout char turned constituent
                    // carries the invalid trait — reading it bare is an error.
                    if is_invalid_constituent(c) {
                        return Err(TorclError::StreamError(
                            "invalid constituent character".into(),
                        ));
                    }
                    if c == ':' && first_unescaped_colon.is_none() {
                        first_unescaped_colon = Some(name.len());
                    }
                    name.push(fold_case_char(c, case_mode));
                    if case_mode == 3 {
                        invert_eligible.push(true);
                    }
                    pos += 1;
                }
            }
            continue;
        }
        match c {
            '\\' => {
                had_escape = true;
                pos += 1;
                if pos >= chars.len() {
                    return Err(TorclError::StreamError("trailing single escape".into()));
                }
                name.push(chars[pos]); // escaped: preserve case
                if case_mode == 3 {
                    invert_eligible.push(false);
                }
                pos += 1;
            }
            '|' => {
                had_escape = true;
                in_multiple_escape = true;
                pos += 1;
            }
            c if is_delimiter(c) => break,
            c => {
                if c == ':' && first_unescaped_colon.is_none() {
                    first_unescaped_colon = Some(name.len());
                }
                name.push(fold_case_char(c, case_mode));
                if case_mode == 3 {
                    invert_eligible.push(true);
                }
                pos += 1;
            }
        }
    }
    if in_multiple_escape {
        return Err(TorclError::StreamError(
            "unterminated multiple escape".into(),
        ));
    }
    if case_mode == 3 {
        // `:invert` — if every unescaped letter is the same case, invert it;
        // a mixed-case token is left unchanged (CLHS 23.1.2).
        let mut saw_upper = false;
        let mut saw_lower = false;
        for (ch, eligible) in name.chars().zip(invert_eligible.iter()) {
            if *eligible {
                if ch.is_ascii_uppercase() {
                    saw_upper = true;
                } else if ch.is_ascii_lowercase() {
                    saw_lower = true;
                }
            }
        }
        if saw_upper ^ saw_lower {
            let inverted: String = name
                .chars()
                .zip(invert_eligible.iter())
                .map(|(ch, eligible)| {
                    if *eligible {
                        if ch.is_ascii_uppercase() {
                            ch.to_ascii_lowercase()
                        } else {
                            ch.to_ascii_uppercase()
                        }
                    } else {
                        ch
                    }
                })
                .collect();
            name = inverted;
        }
    }
    Ok((name, pos, had_escape, first_unescaped_colon))
}

/// Interpret an already readtable-cased token `name` as a number, keyword,
/// package-qualified symbol, `NIL`/`T`, or bare symbol. `has_escape` records
/// whether the token contained any escape (an escaped token is never a number).
fn parse_token_with_base(
    name: &str,
    has_escape: bool,
    // Byte offset of the first UNESCAPED ':' — the package marker, if any.
    // Escaped colons are ordinary name characters (CLHS 2.3.4).
    marker: Option<usize>,
    read_base: u32,
) -> Result<TorclVal, TorclError> {
    if name.is_empty() {
        if has_escape {
            // `||` — the multiple-escaped empty token IS a symbol whose name is
            // the empty string (CLHS 2.4.5). Resolve like any bare symbol so it
            // interns into the current package.
            let idx = resolve_symbol_via_hook(None, name).unwrap_or_else(|| intern_symbol(name));
            return Ok(TorclVal::from_symbol_index(idx));
        }
        return Err(TorclError::StreamError("empty token".into()));
    }

    // An escaped token whose FIRST character is an unescaped ':' is still a
    // KEYWORD — the package marker keeps its syntactic meaning, only the
    // escaped characters lose theirs: `:|A|` is :A, `:|foo bar|` a keyword
    // with a lowercase spaced name (ansi PACKAGE-NICKNAMES.3/UNUSE-PACKAGE.3,
    // which pass ':|A|'-style designators).
    if has_escape && marker == Some(0) {
        if let Some(kw_name) = name.strip_prefix(':') {
            let full = format!("KEYWORD:{}", kw_name);
            let idx = intern_symbol(&full);
            return Ok(TorclVal::from_symbol_index(idx));
        }
    }
    // A package marker keeps its meaning even in an escaped token — only the
    // ESCAPED characters lose theirs — so this is decided by the marker
    // position, not by `has_escape`. `PY::|has space|` is package PY plus the
    // name "has space" (bliss-i83w).
    if let Some(colon) = marker.filter(|p| *p > 0) {
        // A package marker must be followed by a symbol name (CLHS 2.3.4);
        // `PKG::` and `PKG:` are malformed. `PKG::||` is NOT — bars make an
        // empty name legitimate — so this rejects only an UNESCAPED empty
        // remainder, which is exactly what `has_escape` distinguishes
        // (bliss-gw2a). A bare `||` never reaches here: it has no unescaped
        // marker, so `marker` is None.
        let marker_len = if name[colon..].starts_with("::") {
            2
        } else {
            1
        };
        if !has_escape && colon + marker_len >= name.len() {
            return Err(TorclError::StreamError(format!(
                "package marker with no symbol name after it: {name}"
            )));
        }
        if let Some(result) = try_package_qualified(name, colon)? {
            return Ok(result);
        }
    }
    // Don't try numeric interpretation if there are escape chars
    if !has_escape {
        // Check for keyword symbols
        if let Some(kw_name) = name.strip_prefix(':') {
            if kw_name.is_empty() {
                return Err(TorclError::StreamError("empty keyword".into()));
            }
            let full = format!("KEYWORD:{}", kw_name);
            let idx = intern_symbol(&full);
            return Ok(TorclVal::from_symbol_index(idx));
        }
        // Try numeric parse
        match try_parse_number_with_base(name, read_base) {
            Ok(Some(val)) => return Ok(val),
            Ok(None) => {}           // Not a number, fall through to symbol
            Err(e) => return Err(e), // e.g. division by zero in ratio
        }
    }

    // It's a symbol
    if name == "NIL" && !has_escape {
        return Ok(NIL);
    }
    if name == "T" && !has_escape {
        return Ok(T);
    }
    // Route a bare symbol through the interpreter's current *PACKAGE* so that a
    // symbol read bare inside package P shares identity (and its value cell) with
    // the same symbol written P:NAME (bliss-lb6.12). Falls back to plain
    // name-keyed interning when no resolver/load environment is active.
    let idx = resolve_symbol_via_hook(None, name).unwrap_or_else(|| intern_symbol(name));
    Ok(TorclVal::from_symbol_index(idx))
}

/// Resolve an already-delimited symbol token to its symbol value using the
/// exact package/keyword/`NIL`/`T` semantics of the reader, but WITHOUT running
/// the full read pipeline — no `Vec<char>` collection, no nesting pre-scan, no
/// token delimiter scan. `name` is treated as one bare, unescaped token and is
/// upcased exactly as the reader upcases unescaped tokens.
///
/// Returns `Ok(Some(sym))` when the token names a symbol, `Ok(None)` when it
/// would read as a number or is empty (i.e. not a symbol), and propagates a
/// package-not-found error. This is the eval-time fast path for resolving
/// known symbol names (`resolve_sym`), which previously ran `read_from_string`
/// on tiny constant names and made the reader dominate eval self-time
/// (bliss-gq5.4).
pub fn read_symbol_token(name: &str) -> Result<Option<TorclVal>, TorclError> {
    let upper: String = name.chars().map(|c| c.to_ascii_uppercase()).collect();
    if upper.is_empty() {
        return Ok(None);
    }
    // Same decision order as the reader's non-escaped token path:
    // package-qualified, keyword, numeric, then NIL/T, then bare symbol.
    if let Some(result) = upper
        .find(':')
        .map(|colon| try_package_qualified(&upper, colon))
        .transpose()?
        .flatten()
    {
        return Ok(Some(result));
    }
    if let Some(kw_name) = upper.strip_prefix(':') {
        if kw_name.is_empty() {
            return Ok(None);
        }
        let full = format!("KEYWORD:{}", kw_name);
        return Ok(Some(TorclVal::from_symbol_index(intern_symbol(&full))));
    }
    // A token that parses as a number is not a symbol (mirrors the reader
    // falling through to its numeric branch). `read_from_string` uses base 10.
    if let Ok(Some(_)) = try_parse_number_with_base(&upper, 10) {
        return Ok(None);
    }
    if upper == "NIL" {
        return Ok(Some(NIL));
    }
    if upper == "T" {
        return Ok(Some(T));
    }
    let idx = resolve_symbol_via_hook(None, &upper).unwrap_or_else(|| intern_symbol(&upper));
    Ok(Some(TorclVal::from_symbol_index(idx)))
}

/// `colon_pos` is the byte offset of the token's package marker — the first
/// UNESCAPED ':'. Callers that have no escape information pass `name.find(':')`,
/// which is the same thing for an unescaped token.
fn try_package_qualified(name: &str, colon_pos: usize) -> Result<Option<TorclVal>, TorclError> {
    // Check for PKG::SYM or PKG:SYM (but not :keyword which starts with :)
    if name.starts_with(':') {
        return Ok(None);
    }
    {
        let pkg = &name[..colon_pos];
        let rest = &name[colon_pos + 1..];
        let (sym_name, _internal) = if let Some(stripped) = rest.strip_prefix(':') {
            (stripped, true)
        } else {
            (rest, false)
        };
        // Known packages: CL, KEYWORD, TORCL, COMMON-LISP
        match pkg {
            "CL" | "COMMON-LISP" => {
                if sym_name == "NIL" {
                    return Ok(Some(NIL));
                }
                if sym_name == "T" {
                    return Ok(Some(T));
                }
                let idx = intern_symbol(sym_name);
                Ok(Some(TorclVal::from_symbol_index(idx)))
            }
            "KEYWORD" => {
                let full = format!("KEYWORD:{}", sym_name);
                let idx = intern_symbol(&full);
                Ok(Some(TorclVal::from_symbol_index(idx)))
            }
            "TORCL" => {
                let full = format!("TORCL::{}", sym_name);
                let idx = intern_symbol(&full);
                Ok(Some(TorclVal::from_symbol_index(idx)))
            }
            _ if package_exists(pkg) => {
                // Resolve the qualified symbol through the interpreter's package
                // system so it shares identity with the bare-read symbol and with
                // FIND-SYMBOL's result (bliss-lb6.12); fall back to name-keyed
                // interning when no load environment is active.
                let idx = resolve_symbol_via_hook(Some(pkg), sym_name).unwrap_or_else(|| {
                    // No resolver environment (e.g. deserializing a fasl
                    // constant pool, or an internal read_symbol_token). The old
                    // fallback interned the RAW designator spelling
                    // ("nickname:NAME", single colon), which forked a second
                    // identity for a symbol whose registry key is the
                    // canonical "PACKAGE::NAME" — trivial-indent's setf writer
                    // and documentation-utils' FORMAT-DOCUMENTATION generic
                    // both went missing across that split (bliss-nc3b).
                    // Canonicalize: probe every plausible existing spelling
                    // (canonical and designator, "::" and ":"), and mint fresh
                    // only under the canonical double-colon key that the
                    // package layer's alloc_symbol also uses.
                    let canon = torcl_rt::packages::find(pkg)
                        .and_then(torcl_rt::packages::package_name)
                        .unwrap_or_else(|| pkg.to_string());
                    torcl_rt::symbols::find_index(&format!("{canon}::{sym_name}"))
                        .or_else(|| torcl_rt::symbols::find_index(&format!("{canon}:{sym_name}")))
                        .or_else(|| {
                            (canon != pkg)
                                .then(|| {
                                    torcl_rt::symbols::find_index(&format!("{pkg}::{sym_name}"))
                                        .or_else(|| {
                                            torcl_rt::symbols::find_index(&format!(
                                                "{pkg}:{sym_name}"
                                            ))
                                        })
                                })
                                .flatten()
                        })
                        .unwrap_or_else(|| intern_symbol(&format!("{canon}::{sym_name}")))
                });
                Ok(Some(TorclVal::from_symbol_index(idx)))
            }
            _ => Err(TorclError::StreamError(format!(
                "package not found: {}",
                pkg
            ))),
        }
    }
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn try_parse_number(s: &str) -> Result<Option<TorclVal>, TorclError> {
    try_parse_number_with_base(s, 10)
}

fn try_parse_number_with_base(s: &str, read_base: u32) -> Result<Option<TorclVal>, TorclError> {
    // Ratio: num/denom
    if let Some(slash_pos) = s.find('/') {
        if slash_pos > 0 && slash_pos < s.len() - 1 {
            let num_str = &s[..slash_pos];
            let den_str = &s[slash_pos + 1..];
            if let (Ok(n), Ok(d)) = (
                i64::from_str_radix(num_str.trim_start_matches('+'), read_base),
                i64::from_str_radix(den_str.trim_start_matches('+'), read_base),
            ) {
                let n = if num_str.starts_with('-') {
                    -n.abs()
                } else {
                    n
                };
                let d = if den_str.starts_with('-') {
                    -d.abs()
                } else {
                    d
                };
                if d == 0 {
                    return Err(TorclError::ArithmeticError(
                        "division by zero in ratio".into(),
                    ));
                }
                // Canonicalize: reduce to lowest terms with a POSITIVE
                // denominator, and collapse to an integer when the denominator
                // becomes 1 — a ratio literal like `6/4` denotes the rational
                // 3/2, and `4/2` the integer 2 (CLHS 2.3.2.3; bliss-apr).
                fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
                    a = a.abs();
                    b = b.abs();
                    while b != 0 {
                        let t = a % b;
                        a = b;
                        b = t;
                    }
                    a
                }
                let g = gcd_i64(n, d).max(1);
                let (mut n, mut d) = (n / g, d / g);
                if d < 0 {
                    n = -n;
                    d = -d;
                }
                // A component that does not fit the 61-bit fixnum range must
                // become a BIGNUM. `from_fixnum` shifts left by 3 without
                // checking, so feeding it an i64 outside that range silently
                // wrapped into the sign bit: `1152921504606846976/7` (numerator
                // 2^60) READ AS `-1152921504606846976/7`, and
                // `7/1152921504606846976` read with a NEGATIVE, unnormalized
                // denominator. Both are silently wrong values, not errors
                // (bliss-mwpb). The integer path below already gets this right;
                // use the same rule here.
                if d == 1 {
                    return Ok(Some(int_from_i64(n)));
                }
                // GC: `int_from_i64` allocates for a bignum component, so the
                // numerator must be rooted across the denominator's allocation
                // or a relocating minor GC leaves it stale (bliss-wlf).
                torcl_rt::rooted!(num = int_from_i64(n));
                let den = int_from_i64(d);
                return Ok(Some(alloc_ratio(*num, den)));
            }
            // A component beyond i64. Until bliss-0dtf moved the numeric tower
            // into torcl-rt this fell through to Ok(None) and the whole token
            // became a SYMBOL -- ~98 numbers-chapter failures plus SYMBOLP.3,
            // because universe.lsp seeds the shared test sets with such
            // literals. Reducing to lowest terms with a positive denominator
            // (CLHS 2.3.2.3) needs bignum GCD and exact division, which now
            // exist somewhere the reader can reach.
            // GC: parse_bignum ALLOCATES, so the numerator must be rooted
            // across the denominator's parse. Written as a tuple
            // `(parse_bignum(num), parse_bignum(den))` the first result sits
            // unrooted in a Rust temporary while the second allocates; a minor
            // GC there relocates it and the stale copy silently yields the
            // WRONG RATIO -- reducing 246913578024691357802469135780/
            // 123456789012345678901234567890 gave 1 instead of 2 under
            // TORCL_GC_STRESS, with no crash to point at it (AGENTS.md: poison
            // only catches a stale DEREFERENCE, so diff the output).
            if let Some(nv) = parse_bignum(num_str, read_base) {
                torcl_rt::rooted!(nv = nv);
                if let Some(dv) = parse_bignum(den_str, read_base) {
                    return reduced_ratio(*nv, dv).map(Some);
                }
            }
        }
        return Ok(None);
    }
    // Float literal (base 10 only): a token with a decimal point and/or an
    // exponent marker. CL allows exponent markers e/E, s/S, f/F, d/D, l/L;
    // `1.5d0` (double-float syntax) and `1d0` must read as a float, not a
    // symbol. parse_decimal_float validates the grammar so hex-like symbols
    // (e.g. `FACE` in a base-16 context) are not misread as floats.
    if read_base == 10 {
        if let Some((f, is_double)) = parse_decimal_float(s) {
            // CLHS 2.3.2.2: a literal outside the target format's range must
            // signal an error, not read as infinity (bliss-37sr; SBCL signals
            // FLOATING-POINT-OVERFLOW). The token grammar admits no "inf"
            // spelling, so a non-finite result here is always overflow — of
            // the f64 parse for a double literal, or of the f32 narrowing for
            // a single one.
            if is_double {
                if f.is_infinite() {
                    return Err(TorclError::ArithmeticError(format!(
                        "floating-point overflow reading double-float literal {s}"
                    )));
                }
                return Ok(Some(torcl_rt::gc::alloc_double_float(f)));
            }
            let narrowed = f as f32;
            if narrowed.is_infinite() {
                return Err(TorclError::ArithmeticError(format!(
                    "floating-point overflow reading single-float literal {s}"
                )));
            }
            return Ok(Some(TorclVal::from_single_float(narrowed)));
        }
    }
    // Integer with read_base. Integers that fit the 61-bit fixnum range are
    // immediates; anything larger (including values that overflow i64) becomes
    // a bignum (§1.8.1) rather than silently degrading to a symbol.
    let trimmed = s.trim_start_matches('+');
    if let Ok(n) = i64::from_str_radix(trimmed, read_base) {
        if fits_fixnum(n) {
            return Ok(Some(TorclVal::from_fixnum(n)));
        }
        return Ok(Some(alloc_bignum_from_i64(n)));
    }
    // i64 overflow: parse as an arbitrary-precision bignum if the token is a
    // valid integer literal in this base.
    if let Some(b) = parse_bignum(trimmed, read_base) {
        return Ok(Some(b));
    }
    Ok(None)
}

/// An integer `TorclVal` for `n`: a fixnum immediate when it fits the 61-bit
/// range, otherwise a BIGNUM. `TorclVal::from_fixnum` does NOT range-check, so
/// every path that turns a parsed i64 into a value must go through this.
/// Build the rational `nv/dv` from two already-parsed integers, reduced to
/// lowest terms with a POSITIVE denominator, collapsing to an integer when the
/// denominator reduces to 1 (CLHS 2.3.2.3). Either component may be a bignum.
///
/// This is the bignum counterpart of the i64 path above; it exists separately
/// only because that path can do the whole thing in registers.
fn reduced_ratio(nv: TorclVal, dv: TorclVal) -> Result<TorclVal, TorclError> {
    use torcl_rt::bignum::{BigInt, big_cmp, big_divexact, big_gcd, bigint_from_val};
    let (Some(n), Some(d)) = (bigint_from_val(nv), bigint_from_val(dv)) else {
        // Unreachable: parse_bignum only ever yields an integer. Internal
        // rather than a reader error, because reaching it means a runtime
        // invariant broke, not that the source was malformed.
        return Err(TorclError::Internal(
            "parse_bignum produced a non-integer ratio component".into(),
        ));
    };
    if d.sign == 0 {
        return Err(TorclError::ArithmeticError(
            "division by zero in ratio".into(),
        ));
    }
    let g = big_gcd(&n, &d);
    let mut n = big_divexact(&n, &g);
    let mut d = big_divexact(&d, &g);
    // Normalise the sign onto the numerator.
    if d.sign < 0 {
        n.sign = -n.sign;
        d.sign = -d.sign;
    }
    if big_cmp(&d, &BigInt::from_i64(1)) == std::cmp::Ordering::Equal {
        return Ok(n.to_val());
    }
    // GC: `to_val` allocates for a bignum component, so the numerator must be
    // rooted across the denominator's allocation or a relocating minor GC
    // leaves it stale (bliss-wlf) -- the same rule the i64 path follows.
    torcl_rt::rooted!(num = n.to_val());
    let den = d.to_val();
    Ok(alloc_ratio(*num, den))
}

fn int_from_i64(n: i64) -> TorclVal {
    if fits_fixnum(n) {
        TorclVal::from_fixnum(n)
    } else {
        alloc_bignum_from_i64(n)
    }
}

/// True when `n` fits the 61-bit signed fixnum range.
fn fits_fixnum(n: i64) -> bool {
    const MAX: i64 = (1 << 60) - 1;
    const MIN: i64 = -(1 << 60);
    (MIN..=MAX).contains(&n)
}

thread_local! {
    /// CLHS `*READ-DEFAULT-FLOAT-FORMAT*` as seen by the reader: `true` means
    /// DOUBLE-FLOAT (or LONG-FLOAT, which torcl identifies with double). The
    /// host resolves the dynamic variable and sets this before each toplevel
    /// read (bliss-un1x); marker-less and e/E-marked literals read in this
    /// format (CLHS 2.3.2.2 — `e` selects the DEFAULT format, not single).
    static READ_DEFAULT_FLOAT_DOUBLE: core::cell::Cell<bool> =
        const { core::cell::Cell::new(false) };
}

/// Set the reader's view of `*READ-DEFAULT-FLOAT-FORMAT*`: `true` = doubles.
pub fn set_read_default_float_double(double: bool) {
    READ_DEFAULT_FLOAT_DOUBLE.with(|c| c.set(double));
}

thread_local! {
    /// CLHS `*READ-SUPPRESS*`. When true, the reader parses a form's syntax and
    /// discards it: no symbol interning, no `#.` read-eval, no object building —
    /// the result is always NIL, but the full form's characters are consumed.
    /// The host resolves the dynamic variable and sets this around each read.
    static READ_SUPPRESS: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// Set the reader's view of `*READ-SUPPRESS*`.
pub fn set_read_suppress_flag(suppress: bool) {
    READ_SUPPRESS.with(|c| c.set(suppress));
}

/// Whether `*READ-SUPPRESS*` is currently in effect.
pub fn read_suppress_active() -> bool {
    READ_SUPPRESS.with(|c| c.get())
}

/// Parse a base-10 float literal following CL float syntax. The exponent
/// markers select the float format (CLHS 2.3.2.2): `d`/`D` and `l`/`L` are
/// DOUBLE-FLOAT; `e`/`E`, `s`/`S`, `f`/`F`, and a marker-less `d.dd` default to
/// SINGLE-FLOAT (torcl's `*read-default-float-format*` default). Returns
/// `Some((value, is_double))`, or `None` for non-floats. The value is always
/// parsed at `f64` precision so a double literal keeps full precision; the
/// caller narrows to `f32` for the single-float case.
fn parse_decimal_float(s: &str) -> Option<(f64, bool)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    let mut has_digit = false;
    let mut has_dot = false;
    let mut has_exp = false;
    // Marker-less and e/E-marked literals read in the DEFAULT format
    // (bliss-un1x); s/S/f/F force single, d/D/l/L force double.
    let mut is_double = READ_DEFAULT_FLOAT_DOUBLE.with(|c| c.get());

    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
        out.push(chars[i]);
        i += 1;
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            has_digit = true;
            out.push(c);
            i += 1;
        } else if c == '.' && !has_dot && !has_exp {
            has_dot = true;
            out.push('.');
            i += 1;
        } else if !has_exp
            // `has_digit`, NOT `prev_digit`: consuming the decimal point clears
            // prev_digit, so an exponent marker straight after it was rejected
            // and the whole token became a SYMBOL -- `1.s0` read as |1.S0| and
            // `1.d0` as |1.D0|. CLHS 2.3.1 admits
            //     [sign] {digit}+ [decimal-point {digit}*] exponent
            // i.e. ZERO fractional digits before the marker, so `1.s0` and
            // `1.e0` are floats. Requiring a digit to have appeared at all
            // still rejects `.s0` and a bare `e0`, which is what the guard was
            // for. ansi RATIONAL.*.RANDOM.COMPARE and BIGNUM.*.RANDOM.COMPARE
            // write their bounds as `1.s0` and friends.
            && has_digit
            && matches!(c, 'e' | 'E' | 's' | 'S' | 'f' | 'F' | 'd' | 'D' | 'l' | 'L')
        {
            has_exp = true;
            if matches!(c, 'd' | 'D' | 'l' | 'L') {
                is_double = true;
            } else if matches!(c, 's' | 'S' | 'f' | 'F') {
                is_double = false;
            }
            out.push('e');
            i += 1;
            if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                out.push(chars[i]);
                i += 1;
            }
            let mut exp_digits = false;
            while i < chars.len() && chars[i].is_ascii_digit() {
                out.push(chars[i]);
                exp_digits = true;
                i += 1;
            }
            if !exp_digits {
                return None;
            }
        } else {
            return None;
        }
    }
    // A real float needs at least one digit and either a dot or an exponent.
    if !has_digit || (!has_dot && !has_exp) {
        return None;
    }
    out.parse::<f64>().ok().map(|v| (v, is_double))
}

/// Allocate a bignum from an i64 that does not fit the fixnum range.
fn alloc_bignum_from_i64(n: i64) -> TorclVal {
    let sign = if n < 0 { -1 } else { 1 };
    let mag = n.unsigned_abs();
    alloc_bignum(sign, &[mag])
}

/// Parse a (possibly signed) integer literal in `base` into a bignum.
/// Returns `None` if the token is not a valid integer in that base.
fn parse_bignum(s: &str, base: u32) -> Option<TorclVal> {
    let (neg, digits) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    if digits.is_empty() {
        return None;
    }
    let mut limbs: Vec<u64> = vec![0];
    for ch in digits.chars() {
        let d = ch.to_digit(base)? as u64;
        let mut carry = d;
        for limb in limbs.iter_mut() {
            let v = (*limb as u128) * (base as u128) + carry as u128;
            *limb = v as u64;
            carry = (v >> 64) as u64;
        }
        if carry != 0 {
            limbs.push(carry);
        }
    }
    while limbs.len() > 1 && *limbs.last().unwrap() == 0 {
        limbs.pop();
    }
    let sign = if limbs.len() == 1 && limbs[0] == 0 {
        0
    } else if neg {
        -1
    } else {
        1
    };
    Some(alloc_bignum(sign, &limbs))
}

/// Allocate a BIGNUM heap object (§1.8.1): header + sign + n_limbs + limbs.
fn alloc_bignum(sign: i32, limbs: &[u64]) -> TorclVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let ptr = gc_alloc(total_size, type_id::BIGNUM);
    unsafe {
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        gc_value(ptr, total_size)
    }
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_sharpsign(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_sharpsign_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_sharpsign_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    if pos >= chars.len() {
        return Err(TorclError::StreamError("unexpected end after #".into()));
    }
    // Check for #nR, #n=, #n#
    if chars[pos].is_ascii_digit() {
        let start = pos;
        while pos < chars.len() && chars[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unexpected end after #n".into()));
        }
        // A `#n…` infix argument that overflows u32 (e.g. a huge radix or label)
        // is a reader error, not a process-crashing panic (ansi-test reader-aux).
        let num: u32 = match chars[start..pos].iter().collect::<String>().parse() {
            Ok(n) => n,
            Err(_) => {
                return Err(TorclError::StreamError(
                    "#n infix argument is too large".into(),
                ));
            }
        };
        match chars[pos].to_ascii_uppercase() {
            'R' => {
                pos += 1;
                return read_radix_integer(chars, pos, num);
            }
            // #nA(nested-lists) — a rank-`num` array literal (CLHS 2.4.8.12). The
            // printer emits this for multidimensional arrays (bliss-rh0t), so the
            // reader must round-trip it.
            'A' => {
                pos += 1;
                return read_nd_array_literal(
                    chars,
                    pos,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                    num,
                );
            }
            '=' => {
                if !read_circular {
                    return Err(TorclError::StreamError(
                        "circular reader notation is disabled".into(),
                    ));
                }
                pos += 1;
                // Pre-allocate a placeholder cons cell for circular references
                let mut placeholder = alloc_cons(NIL, NIL);
                // The labeled read below allocates and can fire a relocating
                // minor GC; the labels-table copy is rooted (HostRoot) but this
                // Rust local is not — root it so the patch below writes into
                // the placeholder's post-GC address (bliss-wlf).
                torcl_rt::rooted_ref!(_ph_root = &mut placeholder);
                labels.labels.insert(num, placeholder);
                let (val, p) = read_token_with_base(
                    chars,
                    pos,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                // If the result is a cons, copy its car/cdr into the placeholder
                if val.is_cons() {
                    unsafe {
                        // Through the WRITE BARRIER: the placeholder is an
                        // EXISTING cons and the values copied into it may be
                        // younger than it, which a raw store would not record
                        // (bliss-t53a).
                        let ph_ptr = placeholder.as_ptr() as *mut ConsCell;
                        let val_ptr = val.as_ptr() as *const ConsCell;
                        let (vcar, vcdr) = ((*val_ptr).car, (*val_ptr).cdr);
                        torcl_rt::gc::store_ref(std::ptr::addr_of_mut!((*ph_ptr).car), vcar);
                        torcl_rt::gc::store_ref(std::ptr::addr_of_mut!((*ph_ptr).cdr), vcdr);
                    }
                    return Ok((placeholder, p));
                }
                // For non-cons values, just update the label
                labels.labels.insert(num, val);
                return Ok((val, p));
            }
            '#' => {
                if !read_circular {
                    return Err(TorclError::StreamError(
                        "circular reader notation is disabled".into(),
                    ));
                }
                pos += 1;
                if let Some(&val) = labels.labels.get(&num) {
                    return Ok((val, pos));
                }
                return Err(TorclError::StreamError(format!("undefined label #{}", num)));
            }
            other => {
                if let Some(result) = try_custom_sharp_dispatch(
                    chars,
                    pos + 1,
                    other,
                    Some(num as i64),
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth,
                ) {
                    return result;
                }
                return Err(TorclError::StreamError(format!(
                    "unknown # dispatch #{}",
                    chars[pos]
                )));
            }
        }
    }
    let dispatch = chars[pos];
    pos += 1;
    match dispatch {
        '\'' => {
            let (mut val, p) = read_token_with_base(
                chars,
                pos,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            // Root across intern_symbol's possible allocation — see the '
            // handler in read_token_with_base (bliss-wlf).
            torcl_rt::rooted_ref!(_val_root = &mut val);
            let func_sym = TorclVal::from_symbol_index(intern_symbol("FUNCTION"));
            Ok((make_list(&[func_sym, val]), p))
        }
        '\\' => read_char_literal(chars, pos),
        '(' => read_vector_literal_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        'C' | 'c' => read_complex_literal_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        '*' => read_bit_vector(chars, pos),
        'b' | 'B' => read_radix_integer(chars, pos, 2),
        'o' | 'O' => read_radix_integer(chars, pos, 8),
        'x' | 'X' => read_radix_integer(chars, pos, 16),
        '|' => {
            // Block comment #| ... |# — possibly nested. Like a line comment it
            // yields no object; read the following form. But if the comment is
            // the last thing before a list close `)` (or EOF) — e.g. babel's
            // `(#x110000 #| yay |#)` — there is no following form: return MISSING
            // so the enclosing list reader closes the list instead of trying to
            // read `)` as a token ("unexpected )").
            let p = skip_block_comment(chars, pos)?;
            let p2 = skip_whitespace_and_comments(chars, p);
            if p2 >= chars.len() || chars[p2] == ')' {
                return Ok((MISSING, p2));
            }
            read_token_with_base(
                chars,
                p2,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )
        }
        ':' => {
            // Uninterned symbol
            let (name, end, _, _) = collect_token(chars, pos)?;
            Ok((make_uninterned_symbol(&name), end))
        }
        'P' | 'p' => read_pathname_literal(chars, pos),
        'S' | 's' => read_struct_literal_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        '<' => Err(TorclError::StreamError("unreadable object #<".into())),
        '+' => read_feature_expr_with_base(
            chars,
            pos,
            labels,
            true,
            ReaderOptions {
                read_base,
                read_eval,
                read_circular,
                depth: depth + 1,
            },
        ),
        '-' => read_feature_expr_with_base(
            chars,
            pos,
            labels,
            false,
            ReaderOptions {
                read_base,
                read_eval,
                read_circular,
                depth: depth + 1,
            },
        ),
        '.' => {
            // Read-eval: #.(form) — check *read-eval* first
            if !read_eval {
                return Err(TorclError::StreamError("*READ-EVAL* is false".into()));
            }
            let (form, p) = read_token_with_base(
                chars,
                pos,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            eval_read_time_form_with_hook(form).map(|value| (value, p))
        }
        other => {
            if let Some(result) = try_custom_sharp_dispatch(
                chars,
                pos,
                other,
                None,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth,
            ) {
                return result;
            }
            Err(TorclError::StreamError(format!(
                "unknown # dispatch: {}",
                dispatch
            )))
        }
    }
}

fn read_char_literal(chars: &[char], pos: usize) -> Result<(TorclVal, usize), TorclError> {
    if pos >= chars.len() {
        return Err(TorclError::StreamError("unexpected end after #\\".into()));
    }
    // Collect a named character token.  Implementation-defined names are not
    // restricted to letters: SBCL (and libraries written against it) uses
    // names such as NEXT-LINE and NO-BREAK_SPACE.  Stop at reader delimiters,
    // just as an ordinary token would, rather than truncating the name at `-`
    // or `_` (bliss-rr9q).
    let start = pos;
    let mut end = pos + 1;
    // A non-alphabetic first character is the character itself (`#\)`,
    // `#\\`, etc.); only an alphabetic start introduces a named character.
    if chars[pos].is_ascii_alphabetic() {
        while end < chars.len()
            && !chars[end].is_whitespace()
            && !matches!(chars[end], '(' | ')' | '"' | '\'' | '`' | ',' | ';')
        {
            end += 1;
        }
    }
    if end - start > 1 {
        let name: String = chars[start..end].iter().collect();
        match name.to_lowercase().as_str() {
            "space" => return Ok((TorclVal::from_char(' '), end)),
            "newline" => return Ok((TorclVal::from_char('\n'), end)),
            "tab" => return Ok((TorclVal::from_char('\t'), end)),
            "return" => return Ok((TorclVal::from_char('\r'), end)),
            "backspace" => return Ok((TorclVal::from_char('\u{08}'), end)),
            "rubout" | "delete" => return Ok((TorclVal::from_char('\u{7F}'), end)),
            "page" => return Ok((TorclVal::from_char('\u{0C}'), end)),
            "linefeed" => return Ok((TorclVal::from_char('\n'), end)),
            "vt" => return Ok((TorclVal::from_char('\u{0B}'), end)),
            "next-line" => return Ok((TorclVal::from_char('\u{85}'), end)),
            "no-break_space" => return Ok((TorclVal::from_char('\u{A0}'), end)),
            "ideographic_space" => return Ok((TorclVal::from_char('\u{3000}'), end)),
            "nul" | "null" => return Ok((TorclVal::from_char('\0'), end)),
            _ => {
                if name.len() == 1 {
                    return Ok((TorclVal::from_char(name.chars().next().unwrap()), end));
                }
                return Err(TorclError::StreamError(format!(
                    "unknown character name: {}",
                    name
                )));
            }
        }
    }
    Ok((TorclVal::from_char(chars[pos]), end))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_vector_literal(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_vector_literal_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_vector_literal_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    // Root the accumulated elements across the allocating element reads
    // (bliss-6b2 #2), as in read_list_with_base.
    torcl_rt::rooted!(elements = Vec::<TorclVal>::new());
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unterminated vector".into()));
        }
        if chars[pos] == ')' {
            return Ok((alloc_vector(&elements), pos + 1));
        }
        let (val, p) = read_token_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        )?;
        if val != MISSING {
            elements.push(val);
        }
        pos = p;
    }
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_complex_literal(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_complex_literal_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_complex_literal_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() || chars[pos] != '(' {
        return Err(TorclError::StreamError("expected ( after #C".into()));
    }
    pos += 1;
    pos = skip_whitespace_and_comments(chars, pos);
    let (mut real, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth + 1,
    )?;
    // Root `real` across the second component's read, which allocates and can
    // fire a relocating minor GC (bliss-wlf).
    torcl_rt::rooted_ref!(_real_root = &mut real);
    pos = skip_whitespace_and_comments(chars, p);
    let (imag, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth + 1,
    )?;
    pos = skip_whitespace_and_comments(chars, p);
    if pos >= chars.len() || chars[pos] != ')' {
        return Err(TorclError::StreamError(
            "expected ) after #C(real imag".into(),
        ));
    }
    Ok((alloc_complex(real, imag), pos + 1))
}

fn read_bit_vector(chars: &[char], mut pos: usize) -> Result<(TorclVal, usize), TorclError> {
    let mut bits = Vec::new();
    while pos < chars.len() && (chars[pos] == '0' || chars[pos] == '1') {
        bits.push(if chars[pos] == '1' { 1u8 } else { 0u8 });
        pos += 1;
    }
    Ok((alloc_bit_vector(&bits), pos))
}

fn read_radix_integer(
    chars: &[char],
    mut pos: usize,
    radix: u32,
) -> Result<(TorclVal, usize), TorclError> {
    // CLHS: the radix of #nR must be between 2 and 36; anything else is a reader
    // error. Rust's from_str_radix PANICS on an out-of-range radix (and reading
    // is reached across the c2i boundary, so a panic aborts the process), so
    // validate first (ansi-test reader-aux #37R / #1R / #0R).
    if !(2..=36).contains(&radix) {
        return Err(TorclError::StreamError(format!(
            "#{radix}R radix must be between 2 and 36"
        )));
    }
    let start = pos;
    let negative = if pos < chars.len() && (chars[pos] == '+' || chars[pos] == '-') {
        let neg = chars[pos] == '-';
        pos += 1;
        neg
    } else {
        false
    };
    // `/` is part of the token: CLHS 2.3.2.3 allows a RATIO after a radix
    // prefix, so `#x1F/2` is the rational 31/2. Stopping at the `/` read the
    // numerator alone and left `/2` in the stream, so `#x1F/2` silently read as
    // 31 and `#b101/11` as 5 (bliss-mwpb).
    while pos < chars.len()
        && (chars[pos].is_ascii_alphanumeric() || chars[pos] == '/')
        && !is_delimiter(chars[pos])
    {
        pos += 1;
    }
    let token: String = chars[start..pos].iter().collect();
    if token.contains('/') {
        // Reuse the shared ratio parser so reduction, sign normalization and
        // the fixnum/bignum choice are identical to the unprefixed path.
        return match try_parse_number_with_base(&token, radix)? {
            Some(v) => Ok((v, pos)),
            None => Err(TorclError::StreamError(format!(
                "invalid radix-{radix} ratio"
            ))),
        };
    }
    let digits: String = chars[start..pos].iter().collect();
    let digits = digits.trim_start_matches('+').trim_start_matches('-');
    // Fixnum-range values stay immediate; anything larger — including i64
    // overflow like fast-http's #xFFFFFFFFFFFFFFFF content-length bound —
    // becomes a bignum, exactly like the base-10 token path (bliss-d0b).
    if let Ok(n) = i64::from_str_radix(digits, radix) {
        let n = if negative { -n } else { n };
        if fits_fixnum(n) {
            return Ok((TorclVal::from_fixnum(n), pos));
        }
        return Ok((alloc_bignum_from_i64(n), pos));
    }
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits.to_string()
    };
    match parse_bignum(&signed, radix) {
        Some(b) => Ok((b, pos)),
        None => Err(TorclError::StreamError(format!(
            "invalid radix-{} integer",
            radix
        ))),
    }
}

fn skip_block_comment(chars: &[char], mut pos: usize) -> Result<usize, TorclError> {
    let mut depth = 1u32;
    while pos + 1 < chars.len() {
        if chars[pos] == '#' && chars[pos + 1] == '|' {
            depth += 1;
            pos += 2;
        } else if chars[pos] == '|' && chars[pos + 1] == '#' {
            depth -= 1;
            pos += 2;
            if depth == 0 {
                return Ok(pos);
            }
        } else {
            pos += 1;
        }
    }
    Err(TorclError::StreamError("unterminated block comment".into()))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_feature_expr(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
    include_if_present: bool,
) -> Result<(TorclVal, usize), TorclError> {
    read_feature_expr_with_base(
        chars,
        pos,
        labels,
        include_if_present,
        ReaderOptions {
            read_base: 10,
            read_eval: false,
            read_circular: true,
            depth: 0,
        },
    )
}

#[derive(Clone, Copy)]
struct ReaderOptions {
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
}

fn read_feature_expr_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    include_if_present: bool,
    options: ReaderOptions,
) -> Result<(TorclVal, usize), TorclError> {
    // Read the feature expression, which may be a symbol or a compound form
    // such as (or sbcl ccl).
    let (feature, p) = read_token_with_base(
        chars,
        pos,
        labels,
        options.read_base,
        options.read_eval,
        options.read_circular,
        options.depth + 1,
    )?;
    pos = p;
    let feature_present = eval_feature_expression(feature);

    if (include_if_present && feature_present) || (!include_if_present && !feature_present) {
        // Include the next form
        read_token_with_base(
            chars,
            pos,
            labels,
            options.read_base,
            options.read_eval,
            options.read_circular,
            options.depth + 1,
        )
    } else {
        // Skip the next form syntactically without resolving packages or
        // evaluating reader macros inside the suppressed branch.
        pos = skip_form(chars, pos, 0)?;
        pos = skip_whitespace_and_comments(chars, pos);
        Ok((MISSING, pos))
    }
}

fn eval_feature_expression(feature: TorclVal) -> bool {
    if feature == NIL {
        return false;
    }
    if feature.is_symbol() {
        return runtime_feature_present(&feature_symbol_bare_name(feature));
    }
    if !feature.is_cons() {
        return false;
    }

    let (op, args) = cons_parts(feature);
    if !op.is_symbol() {
        return false;
    }

    match feature_symbol_bare_name(op).as_str() {
        "OR" => {
            let mut rest = args;
            while rest.is_cons() {
                let (arg, next) = cons_parts(rest);
                if eval_feature_expression(arg) {
                    return true;
                }
                rest = next;
            }
            false
        }
        "AND" => {
            let mut rest = args;
            while rest.is_cons() {
                let (arg, next) = cons_parts(rest);
                if !eval_feature_expression(arg) {
                    return false;
                }
                rest = next;
            }
            true
        }
        "NOT" => {
            if args.is_cons() {
                let (arg, _) = cons_parts(args);
                !eval_feature_expression(arg)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Whether `name` (a bare, upcased feature name) is present in the runtime
/// `*FEATURES*` list. `*FEATURES*` is a global symbol value, so the reader can
/// consult it directly — reader conditionals (`#+`/`#-`) then honour every
/// feature the running image has, not just TORCL. Before `*FEATURES*` is bound
/// (early bootstrap) only TORCL is recognised.
fn runtime_feature_present(name: &str) -> bool {
    let features =
        torcl_rt::symbols::find_index("*FEATURES*").and_then(torcl_rt::symbols::symbol_value);
    let mut list = match features {
        Some(l) if l.is_cons() => l,
        _ => return name.eq_ignore_ascii_case("TORCL"),
    };
    while list.is_cons() {
        let (item, next) = cons_parts(list);
        if item.tag() == torcl_rt::value::TAG_SYMBOL
            && feature_symbol_bare_name(item).eq_ignore_ascii_case(name)
        {
            return true;
        }
        list = next;
    }
    false
}

fn feature_symbol_name(val: TorclVal) -> String {
    match val {
        NIL => "NIL".to_string(),
        T => "T".to_string(),
        _ if val.tag() == torcl_rt::value::TAG_SYMBOL => symbol_name(val.as_symbol_index())
            .unwrap_or_else(|| format!("SYM#{}", val.as_symbol_index())),
        _ => String::new(),
    }
}

/// The symbol-name component of a feature expression. Reader symbols retain
/// their registry package qualifier (`PKG::NAME` / `KEYWORD:NAME`), but CL
/// feature matching is by the feature symbol's own name, not its home package.
fn feature_symbol_bare_name(val: TorclVal) -> String {
    let name = feature_symbol_name(val);
    let name = name.strip_prefix("KEYWORD:").unwrap_or(&name);
    torcl_rt::symbols::split_registry_key(name)
        .map(|(_, bare)| bare)
        .unwrap_or(name)
        .trim()
        .to_string()
}

fn cons_parts(val: TorclVal) -> (TorclVal, TorclVal) {
    assert!(val.is_cons(), "cons_parts called on non-cons");
    unsafe {
        let cell = val.as_ptr() as *const ConsCell;
        ((*cell).car, (*cell).cdr)
    }
}

fn skip_form(chars: &[char], pos: usize, depth: usize) -> Result<usize, TorclError> {
    if depth > MAX_READER_NESTING {
        return Err(TorclError::StreamError(
            "reader nesting limit exceeded".into(),
        ));
    }
    let pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Ok(pos);
    }

    match chars[pos] {
        '(' => skip_list(chars, pos + 1, depth + 1),
        // A bare `)` where a form is expected is malformed, even while skipping
        // for *read-suppress* or a #+/#- branch (read-suppress.error.1: `')`).
        ')' => Err(TorclError::StreamError("unexpected ')'".into())),
        '"' => skip_string(chars, pos + 1),
        '\'' | '`' => skip_form(chars, pos + 1, depth + 1),
        ',' => {
            if pos + 1 < chars.len() && (chars[pos + 1] == '@' || chars[pos + 1] == '.') {
                skip_form(chars, pos + 2, depth + 1)
            } else {
                skip_form(chars, pos + 1, depth + 1)
            }
        }
        '#' => skip_sharpsign_form(chars, pos + 1, depth + 1),
        _ => skip_atom(chars, pos),
    }
}

fn skip_list(chars: &[char], mut pos: usize, depth: usize) -> Result<usize, TorclError> {
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unterminated list".into()));
        }
        if chars[pos] == ')' {
            return Ok(pos + 1);
        }
        pos = skip_form(chars, pos, depth + 1)?;
    }
}

fn skip_string(chars: &[char], mut pos: usize) -> Result<usize, TorclError> {
    while pos < chars.len() {
        match chars[pos] {
            '\\' => pos += 2,
            '"' => return Ok(pos + 1),
            _ => pos += 1,
        }
    }
    Err(TorclError::StreamError("unterminated string".into()))
}

fn skip_atom(chars: &[char], pos: usize) -> Result<usize, TorclError> {
    let (_token, end, _escaped, _) = collect_token(chars, pos)?;
    Ok(end)
}

fn skip_sharpsign_form(chars: &[char], mut pos: usize, depth: usize) -> Result<usize, TorclError> {
    if pos >= chars.len() {
        return Err(TorclError::StreamError("unexpected end after #".into()));
    }

    // Optional infix numeric argument (`#3r…`, `#0(…)`, `#100000000\Space`).
    // Its magnitude is irrelevant when skipping, so we never parse it — the
    // dispatch that follows determines the syntax.
    while pos < chars.len() && chars[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos >= chars.len() {
        return Err(TorclError::StreamError("unexpected end after #".into()));
    }

    let dispatch = chars[pos];
    match dispatch {
        '=' => skip_form(chars, pos + 1, depth + 1),
        '#' => Ok(pos + 1),
        '\'' | '+' | '-' | '.' => skip_form(chars, pos + 1, depth + 1),
        '\\' => Ok(skip_char_literal(chars, pos + 1)),
        ':' | 'b' | 'B' | 'o' | 'O' | 'x' | 'X' | 'r' | 'R' | '*' => skip_atom(chars, pos + 1),
        '(' => skip_list(chars, pos + 1, depth + 1),
        'C' | 'c' => skip_form(chars, pos + 1, depth + 1),
        // #nA(nested…) rank-n array literal: one form follows
        // (bliss-fo0o: `#2a((a b))` inside a #+nil block errored).
        'a' | 'A' => skip_form(chars, pos + 1, depth + 1),
        'P' | 'p' | 'S' | 's' => skip_form(chars, pos + 1, depth + 1),
        // `#<` is explicitly unreadable (CLHS 2.4.8.20) — an error even under
        // *read-suppress* (read-suppress.error.2).
        '|' => skip_block_comment(chars, pos + 1),
        other if custom_sharp_dispatch_registered(other) => {
            // Suppressed custom dispatch — one-form approximation (bliss-r4mk).
            skip_form(chars, pos + 1, depth + 1)
        }
        other => Err(TorclError::StreamError(format!(
            "unknown # dispatch: {}",
            other
        ))),
    }
}

/// Skip a `#\c` character literal starting at `pos` (the position just past the
/// backslash). The character after the backslash is consumed UNCONDITIONALLY,
/// even when it is a macro or escape character (`#\'`, `#\(`, `#\)`, `#\"`,
/// `#\;`, `#\\`); a multi-char name (`#\Space`, `#\Newline`) then continues over
/// trailing constituents. Routing this through skip_atom treated `#\'` as an
/// empty token and `#\\` as an escape that swallowed the next character —
/// either way a skipped form with character keys derailed (bliss-d0b).
fn skip_char_literal(chars: &[char], pos: usize) -> usize {
    let mut p = pos;
    if p < chars.len() {
        p += 1; // the character itself, whatever it is
        while p < chars.len()
            && !chars[p].is_whitespace()
            && !matches!(chars[p], '(' | ')' | '"' | '\'' | '`' | ',' | ';')
        {
            p += 1;
        }
    }
    p
}

/// Coerce a numeric TorclVal to f64 for mixed-type arithmetic.
fn numeric_to_f64(v: TorclVal) -> Option<f64> {
    if v.is_fixnum() {
        Some(v.as_fixnum() as f64)
    } else if v.is_single_float() {
        Some(v.as_single_float() as f64)
    } else {
        None
    }
}

fn eval_read_time_form_with_hook(form: TorclVal) -> Result<TorclVal, TorclError> {
    if let Some(hook) = read_eval_hook() {
        return hook(form);
    }
    eval_read_time_form(form)
}

pub fn eval_read_time_form(form: TorclVal) -> Result<TorclVal, TorclError> {
    if !form.is_cons() {
        if form == NIL || form == T {
            return Ok(form);
        }
        if form.is_fixnum() || form.is_single_float() || form.is_character() {
            return Ok(form);
        }
        if form.is_symbol() {
            let name = feature_symbol_name(form);
            if name.starts_with("KEYWORD:") {
                return Ok(form);
            }
            return Err(TorclError::UnboundVariable(form));
        }
        if form.is_heap_object() {
            unsafe {
                let ptr = form.as_ptr();
                let header = *(ptr as *const ObjectHeader);
                if header.type_id() == type_id::SIMPLE_BASE_STRING
                    || header.type_id() == type_id::SIMPLE_CHARACTER_STRING
                {
                    return Ok(form);
                }
            }
        }
        return Err(TorclError::TypeError {
            datum: form,
            expected: "read-time evaluable form".into(),
        });
    }

    let (operator, args) = cons_parts(form);
    if !operator.is_symbol() {
        return Err(TorclError::TypeError {
            datum: operator,
            expected: "read-time operator symbol".into(),
        });
    }

    let operator_name = feature_symbol_name(operator);
    match operator_name.as_str() {
        "QUOTE" => {
            let argv = list_to_vec(args)?;
            argv.first()
                .copied()
                .ok_or_else(|| TorclError::Internal("QUOTE: missing argument".into()))
        }
        "IF" => {
            let argv = list_to_vec(args)?;
            if argv.is_empty() {
                return Err(TorclError::Internal("IF: missing test".into()));
            }
            if eval_read_time_form(argv[0])? != NIL {
                if argv.len() > 1 {
                    eval_read_time_form(argv[1])
                } else {
                    Ok(NIL)
                }
            } else if argv.len() > 2 {
                eval_read_time_form(argv[2])
            } else {
                Ok(NIL)
            }
        }
        "PROGN" => {
            let argv = list_to_vec(args)?;
            let mut result = NIL;
            for form in argv {
                result = eval_read_time_form(form)?;
            }
            Ok(result)
        }
        "+" | "-" | "*" => eval_read_time_arithmetic(operator_name.as_str(), args),
        "CAR" => {
            let argv = eval_read_time_args(args)?;
            if argv.len() != 1 {
                return Err(TorclError::Internal("CAR: expected 1 argument".into()));
            }
            if argv[0] == NIL {
                Ok(NIL)
            } else {
                Ok(cons_parts(argv[0]).0)
            }
        }
        "CDR" => {
            let argv = eval_read_time_args(args)?;
            if argv.len() != 1 {
                return Err(TorclError::Internal("CDR: expected 1 argument".into()));
            }
            if argv[0] == NIL {
                Ok(NIL)
            } else {
                Ok(cons_parts(argv[0]).1)
            }
        }
        "CONS" => {
            let argv = eval_read_time_args(args)?;
            if argv.len() != 2 {
                return Err(TorclError::Internal("CONS: expected 2 arguments".into()));
            }
            Ok(alloc_cons(argv[0], argv[1]))
        }
        "LIST" => Ok(make_list(&eval_read_time_args(args)?)),
        "EQ" => {
            let argv = eval_read_time_args(args)?;
            if argv.len() != 2 {
                return Err(TorclError::Internal("EQ: expected 2 arguments".into()));
            }
            Ok(if argv[0] == argv[1] { T } else { NIL })
        }
        _ => Err(TorclError::StreamError(format!(
            "read-eval not supported for {}",
            operator_name
        ))),
    }
}

fn list_to_vec(list: TorclVal) -> Result<Vec<TorclVal>, TorclError> {
    let mut result = Vec::new();
    let mut current = list;
    while current.is_cons() {
        let (car, cdr) = cons_parts(current);
        result.push(car);
        current = cdr;
    }
    if current != NIL {
        return Err(TorclError::TypeError {
            datum: list,
            expected: "proper list".into(),
        });
    }
    Ok(result)
}

fn eval_read_time_args(args: TorclVal) -> Result<Vec<TorclVal>, TorclError> {
    list_to_vec(args)?
        .into_iter()
        .map(eval_read_time_form)
        .collect()
}

fn eval_read_time_arithmetic(op: &str, args: TorclVal) -> Result<TorclVal, TorclError> {
    let argv = eval_read_time_args(args)?;
    if argv.is_empty() {
        return Ok(match op {
            "+" | "-" => TorclVal::from_fixnum(0),
            "*" => TorclVal::from_fixnum(1),
            _ => unreachable!(),
        });
    }

    if argv.iter().all(|value| value.is_fixnum()) {
        let mut iter = argv.iter().map(|value| value.as_fixnum());
        let first = iter
            .next()
            .ok_or_else(|| TorclError::Internal("missing arithmetic operand".into()))?;
        let total = match op {
            "+" => first + iter.sum::<i64>(),
            "*" => iter.fold(first, |acc, value| acc * value),
            "-" => {
                if argv.len() == 1 {
                    -first
                } else {
                    iter.fold(first, |acc, value| acc - value)
                }
            }
            _ => unreachable!(),
        };
        return Ok(TorclVal::from_fixnum(total));
    }

    let mut iter = argv.into_iter();
    let first_value = iter
        .next()
        .ok_or_else(|| TorclError::Internal("missing arithmetic operand".into()))?;
    let first = numeric_to_f64(first_value).ok_or_else(|| TorclError::TypeError {
        datum: first_value,
        expected: "number".into(),
    })?;

    let total = match op {
        "+" => iter.try_fold(first, |acc, value| {
            numeric_to_f64(value)
                .map(|number| acc + number)
                .ok_or_else(|| TorclError::TypeError {
                    datum: value,
                    expected: "number".into(),
                })
        })?,
        "*" => iter.try_fold(first, |acc, value| {
            numeric_to_f64(value)
                .map(|number| acc * number)
                .ok_or_else(|| TorclError::TypeError {
                    datum: value,
                    expected: "number".into(),
                })
        })?,
        "-" => {
            if iter.len() == 0 {
                -first
            } else {
                iter.try_fold(first, |acc, value| {
                    numeric_to_f64(value)
                        .map(|number| acc - number)
                        .ok_or_else(|| TorclError::TypeError {
                            datum: value,
                            expected: "number".into(),
                        })
                })?
            }
        }
        _ => unreachable!(),
    };

    Ok(TorclVal::from_single_float(total as f32))
}

fn read_pathname_literal(chars: &[char], pos: usize) -> Result<(TorclVal, usize), TorclError> {
    // #P"string" — parse the string that follows
    if pos >= chars.len() || chars[pos] != '"' {
        return Err(TorclError::StreamError("expected string after #P".into()));
    }
    let (string_val, end) = read_string(chars, pos + 1)?;
    Ok((construct_pathname(string_val), end))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_struct_literal(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(TorclVal, usize), TorclError> {
    read_struct_literal_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_struct_literal_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(TorclVal, usize), TorclError> {
    // #S(name slot-key slot-value ...) — parse struct literal
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() || chars[pos] != '(' {
        return Err(TorclError::StreamError("expected ( after #S".into()));
    }
    pos += 1;
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Err(TorclError::StreamError("unterminated #S literal".into()));
    }
    if chars[pos] == ')' {
        return Err(TorclError::StreamError(
            "#S() requires a struct name".into(),
        ));
    }
    // Read struct name
    let (mut name_val, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth + 1,
    )?;
    // Root the name and the accumulating slot values across the remaining
    // reads, each of which allocates and can fire a relocating minor GC
    // (bliss-wlf).
    torcl_rt::rooted_ref!(_name_root = &mut name_val);
    pos = p;
    // Read remaining slot key-value pairs as a flat list
    torcl_rt::rooted!(slots = Vec::new());
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(TorclError::StreamError("unterminated #S literal".into()));
        }
        if chars[pos] == ')' {
            pos += 1;
            break;
        }
        let (val, p) = read_token_with_base(
            chars,
            pos,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        )?;
        slots.push(val);
        pos = p;
    }
    Ok((alloc_structure(name_val, &slots), pos))
}

// ── Readtable operations ──────────────────────────────────────────

pub fn make_readtable(from: Option<TorclVal>) -> Result<TorclVal, TorclError> {
    let rt = alloc_readtable();
    if let Some(src) = from {
        if src.tag() == TAG_HEAP_OBJECT {
            // Copy macro char settings from src to rt
            let src_key = src.0 & !torcl_rt::value::TAG_MASK;
            let rt_key = rt.0 & !torcl_rt::value::TAG_MASK;
            let mut guard = MACRO_CHARS.lock().unwrap();
            let table = guard.get_or_insert_with(HashMap::new);
            let copies: Vec<_> = table
                .iter()
                .filter(|&(&(k, _), _)| k == src_key)
                .map(|(&(_, ch), v)| (ch, *v))
                .collect();
            for (ch, val) in copies {
                table.insert((rt_key, ch), val);
            }
        }
    }
    Ok(rt)
}

pub fn copy_readtable(from: TorclVal, to: Option<TorclVal>) -> Result<TorclVal, TorclError> {
    let dest = match to {
        Some(rt) => rt,
        None => alloc_readtable(),
    };
    let src_key = readtable_key(from);
    let dst_key = readtable_key(dest);
    let mut guard = MACRO_CHARS.lock().unwrap();
    let table = guard.get_or_insert_with(HashMap::new);
    let copies: Vec<_> = table
        .iter()
        .filter(|&(&(k, _), _)| k == src_key)
        .map(|(&(_, ch), v)| (ch, *v))
        .collect();
    for (ch, val) in copies {
        table.insert((dst_key, ch), val);
    }
    drop(guard);

    let mut dispatch_guard = DISPATCH_CHARS.lock().unwrap();
    let dispatch_table = dispatch_guard.get_or_insert_with(HashMap::new);
    let dispatch_copies: Vec<_> = dispatch_table
        .iter()
        .filter(|&(&(k, _), _)| k == src_key)
        .map(|(&(_, ch), &non_terminating)| (ch, non_terminating))
        .collect();
    for (ch, non_terminating) in dispatch_copies {
        dispatch_table.insert((dst_key, ch), non_terminating);
    }
    drop(dispatch_guard);

    let mut sub_guard = DISPATCH_SUB_CHARS.lock().unwrap();
    let sub_table = sub_guard.get_or_insert_with(HashMap::new);
    let sub_copies: Vec<_> = sub_table
        .iter()
        .filter(|&(&(k, _, _), _)| k == src_key)
        .map(|(&(_, disp_char, sub_char), &handler)| (disp_char, sub_char, handler))
        .collect();
    for (disp_char, sub_char, handler) in sub_copies {
        sub_table.insert((dst_key, disp_char, sub_char), handler);
    }
    // Carry the readtable-case across the copy. `(copy-readtable nil dest)`
    // takes the standard readtable's case (:upcase); `(copy-readtable rt dest)`
    // takes rt's (readtable-case.7).
    if let Some(mode) = readtable_case_mode(from) {
        set_readtable_case_mode(dest, mode);
    }
    Ok(dest)
}

/// Read the `readtable-case` code (0=upcase, 1=downcase, 2=preserve, 3=invert)
/// of a specific readtable object. Returns `None` if `rt` is not a readtable
/// object (e.g. the `:standard-readtable` placeholder), whose case is `:upcase`.
pub fn readtable_case_mode(rt: TorclVal) -> Option<u8> {
    if rt.tag() != TAG_HEAP_OBJECT {
        return None;
    }
    unsafe {
        let ptr = rt.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        if header.type_id() != type_id::READTABLE {
            return None;
        }
        Some((*(ptr as *const ReadtableData)).case_mode)
    }
}

/// Set the `readtable-case` code on a specific readtable object. No-op for a
/// non-readtable value. Returns whether the write happened.
pub fn set_readtable_case_mode(rt: TorclVal, mode: u8) -> bool {
    if rt.tag() != TAG_HEAP_OBJECT {
        return false;
    }
    unsafe {
        let ptr = rt.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        if header.type_id() != type_id::READTABLE {
            return false;
        }
        (*(ptr as *mut ReadtableData)).case_mode = mode;
        if mode != 0 {
            ANY_NONUPCASE_CASE.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        true
    }
}

pub fn set_macro_character(
    readtable: TorclVal,
    ch: char,
    function: TorclVal,
    non_terminating: bool,
) -> Result<(), TorclError> {
    let key = readtable.0 & !torcl_rt::value::TAG_MASK;
    {
        let mut guard = MACRO_CHARS.lock().unwrap();
        let table = guard.get_or_insert_with(HashMap::new);
        table.insert((key, ch), (function, non_terminating));
    }
    // Register the char's syntax so the tokenizer stops on it (a terminating
    // macro char terminates a preceding token) and the entry dispatch routes it
    // through its custom handler (set-macro-character.1/.2). Skip the T sentinel
    // written by make-dispatch-macro-character (which manages its own syntax).
    if function != T {
        {
            let mut guard = CHAR_SYNTAX.lock().unwrap();
            let table = guard.get_or_insert_with(HashMap::new);
            let code = if non_terminating { 3 } else { 2 };
            table.insert((key, ch), (code, ch));
        }
        ANY_CHAR_SYNTAX_OVERRIDE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

pub fn get_macro_character(
    readtable: TorclVal,
    ch: char,
) -> Result<(Option<TorclVal>, bool), TorclError> {
    let key = readtable.0 & !torcl_rt::value::TAG_MASK;
    let guard = MACRO_CHARS.lock().unwrap();
    if let Some(table) = guard.as_ref() {
        if let Some(&(func, nt)) = table.get(&(key, ch)) {
            return Ok((Some(func), nt));
        }
    }
    Ok((None, false))
}

pub fn set_dispatch_macro_character(
    readtable: TorclVal,
    disp_char: char,
    sub_char: char,
    function: TorclVal,
) -> Result<(), TorclError> {
    let key = readtable.0 & !torcl_rt::value::TAG_MASK;
    let mut guard = DISPATCH_SUB_CHARS.lock().unwrap();
    let table = guard.get_or_insert_with(HashMap::new);
    // CLHS: a lowercase sub-char is converted to uppercase (the dispatcher
    // upcases at lookup, so #l and #L reach the same handler).
    table.insert((key, disp_char, sub_char.to_ascii_uppercase()), function);
    ANY_CUSTOM_MACROS.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

pub fn get_dispatch_macro_character(
    readtable: TorclVal,
    disp_char: char,
    sub_char: char,
) -> Result<Option<TorclVal>, TorclError> {
    let key = readtable.0 & !torcl_rt::value::TAG_MASK;
    let guard = DISPATCH_SUB_CHARS.lock().unwrap();
    if let Some(table) = guard.as_ref() {
        // Symmetric with set_dispatch_macro_character's CLHS upcasing.
        if let Some(&func) = table.get(&(key, disp_char, sub_char.to_ascii_uppercase())) {
            return Ok(Some(func));
        }
    }
    Ok(None)
}

/// Whether `ch` is a dispatch macro character in `readtable` (built-in `#`, or
/// one made via `make-dispatch-macro-character`).
pub fn is_dispatch_macro_character(readtable: TorclVal, ch: char) -> bool {
    ch == '#' || dispatch_is_registered(readtable, ch)
}

pub fn make_dispatch_macro_character(
    readtable: TorclVal,
    ch: char,
    non_terminating: bool,
) -> Result<(), TorclError> {
    let key = readtable.0 & !torcl_rt::value::TAG_MASK;
    {
        let mut guard = DISPATCH_CHARS.lock().unwrap();
        let table = guard.get_or_insert_with(HashMap::new);
        table.insert((key, ch), non_terminating);
    }
    // Also register as a macro char
    set_macro_character(readtable, ch, T, non_terminating)?;
    // Give the char macro syntax in this readtable so the tokenizer stops on it
    // (a terminating dispatch char terminates a preceding token) and routes it
    // through the dispatch path (make-dispatch-macro-character.1/.3).
    {
        let mut guard = CHAR_SYNTAX.lock().unwrap();
        let table = guard.get_or_insert_with(HashMap::new);
        let code = if non_terminating { 3 } else { 2 };
        table.insert((key, ch), (code, ch));
    }
    ANY_CHAR_SYNTAX_OVERRIDE.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}
