//! CL Reader — converts character streams into Lisp objects.
//!
//! Implements the CLHS §2.2 reader algorithm. See spec §4.1.

use bliss_rt::error::BlissError;
use bliss_rt::object::{
    ComplexData, ConsCell, ElementTypeTag, ObjectHeader, PathnameData, RatioData, ReadtableData,
    type_id,
};
use bliss_rt::value::{BlissVal, EOF, MISSING, NIL, T, TAG_HEAP_OBJECT};
use std::collections::HashMap;
use std::sync::Mutex;

type MacroCharTable = HashMap<(u64, char), (BlissVal, bool)>;
type DispatchCharTable = HashMap<(u64, char), bool>;
type DispatchSubCharTable = HashMap<(u64, char, char), BlissVal>;
type TokenChars = Vec<(char, bool)>;
const MAX_READER_NESTING: usize = 4096;

// ── Global symbol table ───────────────────────────────────────────
//
// The canonical symbol registry now lives in `bliss_rt::symbols`, where each
// interned symbol is a heap-resident `SymbolData` object (bliss-jtc.6 Stage A).
// The reader's public entrypoints delegate to it so the reader, interpreter,
// stdlib, and GC share one symbol identity space rather than parallel name
// tables. Package existence is likewise delegated to the shared bliss_rt package
// registry of heap PACKAGE objects (bliss-jtc.6 Stage D).

pub fn intern_symbol(name: &str) -> u32 {
    bliss_rt::symbols::intern(name)
}

/// Look up an already-interned symbol index by name WITHOUT interning it.
/// Unlike `intern_symbol`, this never creates a new entry, so it can be used
/// to answer "does a symbol with this name exist?" without side effects — which
/// is what a correct FIND-SYMBOL needs (FIND-SYMBOL must not intern).
pub fn find_symbol_index(name: &str) -> Option<u32> {
    bliss_rt::symbols::find_index(name)
}

/// Look up the name of a symbol by its index.
/// Returns None if the index is not in the global symbol table.
pub fn symbol_name(idx: u32) -> Option<String> {
    bliss_rt::symbols::symbol_name(idx)
}

pub fn register_package(name: &str) {
    bliss_rt::packages::register(name);
}

fn package_exists(name: &str) -> bool {
    bliss_rt::packages::exists(name)
}

/// Create a fresh uninterned symbol with the given name. Each call yields a
/// distinct symbol (a unique high-range index) with a real heap object, but the
/// name is deliberately not added to the name→index map, so the symbol remains
/// uninterned and is not found by `intern`/`find-symbol`. `symbol_name` can
/// still resolve it via its heap name cell.
pub fn make_uninterned_symbol(name: &str) -> BlissVal {
    bliss_rt::symbols::make_uninterned(name)
}

// ── Global macro character tables ─────────────────────────────────
static MACRO_CHARS: Mutex<Option<MacroCharTable>> = Mutex::new(None);
static DISPATCH_CHARS: Mutex<Option<DispatchCharTable>> = Mutex::new(None);
static DISPATCH_SUB_CHARS: Mutex<Option<DispatchSubCharTable>> = Mutex::new(None);
type ReadEvalHook = fn(BlissVal) -> Result<BlissVal, BlissError>;
static READ_EVAL_HOOK: Mutex<Option<ReadEvalHook>> = Mutex::new(None);

pub fn set_read_eval_hook(hook: Option<ReadEvalHook>) {
    let mut guard = READ_EVAL_HOOK.lock().unwrap();
    *guard = hook;
}

fn read_eval_hook() -> Option<ReadEvalHook> {
    *READ_EVAL_HOOK.lock().unwrap()
}

fn readtable_key(readtable: BlissVal) -> u64 {
    readtable.0 & !bliss_rt::value::TAG_MASK
}

fn lookup_custom_macro(readtable: BlissVal, ch: char) -> Option<(BlissVal, bool)> {
    let key = readtable_key(readtable);
    let guard = MACRO_CHARS.lock().unwrap();
    guard
        .as_ref()
        .and_then(|table| table.get(&(key, ch)).copied())
}

fn lookup_custom_dispatch(
    readtable: BlissVal,
    disp_char: char,
    sub_char: char,
) -> Option<BlissVal> {
    let key = readtable_key(readtable);
    let guard = DISPATCH_SUB_CHARS.lock().unwrap();
    guard
        .as_ref()
        .and_then(|table| table.get(&(key, disp_char, sub_char)).copied())
}

fn dispatch_is_registered(readtable: BlissVal, ch: char) -> bool {
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
    readtable: BlissVal,
) -> Option<Result<(BlissVal, usize), BlissError>> {
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
    labels: HashMap<u32, BlissVal>,
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
    match bliss_rt::gc::alloc_typed(body_size, type_id) {
        // SAFETY: alloc_typed returns a pointer past an 8-byte header.
        Some(body) => unsafe { body.sub(hdr) },
        None => std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(total_size.max(hdr), hdr).unwrap(),
        ),
    }
}

fn alloc_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    // A headered GC object whose body (car@0, cdr@8) is what `from_cons_ptr`
    // points at — the same representation the T0 evaluator uses.
    let body = match bliss_rt::gc::alloc_typed(16, type_id::CONS) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(std::alloc::Layout::new::<ConsCell>()),
    };
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = car;
        (*cell).cdr = cdr;
        BlissVal::from_cons_ptr(body)
    }
}

fn alloc_string(s: &str) -> BlissVal {
    // Layout: ObjectHeader (8 bytes) + length (u64, 8 bytes) + bytes
    let bytes = s.as_bytes();
    let total_size = 8 + 8 + bytes.len();
    let ptr = gc_alloc(total_size, type_id::SIMPLE_BASE_STRING);
    unsafe {
        *(ptr.add(8) as *mut u64) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(16), bytes.len());
        BlissVal::from_heap_ptr(ptr)
    }
}

fn alloc_vector(elements: &[BlissVal]) -> BlissVal {
    let total_size = 8 + 8 + elements.len() * 8;
    let ptr = gc_alloc(total_size, type_id::SIMPLE_VECTOR);
    unsafe {
        *(ptr.add(8) as *mut u64) = elements.len() as u64;
        for (i, &elem) in elements.iter().enumerate() {
            *(ptr.add(16 + i * 8) as *mut BlissVal) = elem;
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

fn alloc_ratio(num: BlissVal, den: BlissVal) -> BlissVal {
    let ptr = gc_alloc(std::mem::size_of::<RatioData>(), type_id::RATIO) as *mut RatioData;
    unsafe {
        (*ptr).numerator = num;
        (*ptr).denominator = den;
        BlissVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_complex(real: BlissVal, imag: BlissVal) -> BlissVal {
    let ptr = gc_alloc(std::mem::size_of::<ComplexData>(), type_id::COMPLEX) as *mut ComplexData;
    unsafe {
        (*ptr).realpart = real;
        (*ptr).imagpart = imag;
        BlissVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_bit_vector(bits: &[u8]) -> BlissVal {
    // Layout: ObjectHeader (8) + element_type_tag byte + padding (7) + length (8) + data
    let data_bytes = bits.len().div_ceil(8);
    let total_size = 8 + 8 + 8 + data_bytes;
    let ptr = gc_alloc(total_size, type_id::SIMPLE_ARRAY);
    unsafe {
        // Element type tag at first byte after header
        *ptr.add(8) = ElementTypeTag::Bit as u8;
        // Length stored after the element-type word
        *(ptr.add(16) as *mut u64) = bits.len() as u64;
        // Pack bits
        for (i, &b) in bits.iter().enumerate() {
            if b != 0 {
                let byte_idx = i / 8;
                let bit_idx = i % 8;
                *ptr.add(24 + byte_idx) |= 1 << bit_idx;
            }
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

fn alloc_readtable() -> BlissVal {
    let ptr = gc_alloc(std::mem::size_of::<ReadtableData>(), type_id::READTABLE) as *mut ReadtableData;
    unsafe {
        (*ptr).case_mode = 0; // :upcase
        (*ptr)._pad = [0; 7];
        (*ptr).char_table = NIL;
        (*ptr).extended_table = NIL;
        (*ptr).macro_table = NIL;
        (*ptr).dispatch_table = NIL;
        BlissVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_pathname(namestring: BlissVal) -> BlissVal {
    let ptr = gc_alloc(std::mem::size_of::<PathnameData>(), type_id::PATHNAME) as *mut PathnameData;
    unsafe {
        (*ptr).host = NIL;
        (*ptr).device = NIL;
        (*ptr).directory = NIL;
        (*ptr).name = namestring;
        (*ptr).type_field = NIL;
        (*ptr).version = NIL;
        BlissVal::from_heap_ptr(ptr as *mut u8)
    }
}

fn alloc_structure(name: BlissVal, slots: &[BlissVal]) -> BlissVal {
    // Layout: ObjectHeader (8) + name (8) + n_slots (8) + slot data
    let total_size = 8 + 8 + 8 + slots.len() * 8;
    let ptr = gc_alloc(total_size, type_id::STRUCTURE);
    unsafe {
        *(ptr.add(8) as *mut BlissVal) = name;
        *(ptr.add(16) as *mut u64) = slots.len() as u64;
        for (i, &slot) in slots.iter().enumerate() {
            *(ptr.add(24 + i * 8) as *mut BlissVal) = slot;
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

/// Build a proper list from elements: (a b c) = cons(a, cons(b, cons(c, NIL)))
fn make_list(elems: &[BlissVal]) -> BlissVal {
    let mut result = NIL;
    for &e in elems.iter().rev() {
        result = alloc_cons(e, result);
    }
    result
}

// ── Reader state ──────────────────────────────────────────────────

/// Reader state bundle. Holds all per-read configuration.
pub struct ReaderState {
    input: BlissVal,
    readtable: BlissVal,
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
    pub fn set_input(&mut self, stream: BlissVal) {
        self.input = stream;
    }
    pub fn set_readtable(&mut self, readtable: BlissVal) {
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

pub fn read(state: &mut ReaderState) -> Result<BlissVal, BlissError> {
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
            if header.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(ptr.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(ptr.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    let chars: Vec<char> = s.chars().collect();
                    ensure_nesting_within_limit(&chars)?;
                    let mut labels = CircularLabels {
                        labels: HashMap::new(),
                    };

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
            return Err(BlissError::StreamError(format!(
                "unsupported stream type for read (type_id={})",
                header.type_id()
            )));
        }
    }
    // Non-heap, non-NIL input — cannot read from it
    Err(BlissError::StreamError(
        "unsupported input type for read".into(),
    ))
}

pub fn read_from_string(s: &str) -> Result<(BlissVal, usize), BlissError> {
    read_from_string_with_base(s, 10, false)
}

pub fn read_from_string_with_base(
    s: &str,
    read_base: u32,
    read_eval: bool,
) -> Result<(BlissVal, usize), BlissError> {
    let chars: Vec<char> = s.chars().collect();
    ensure_nesting_within_limit(&chars)?;
    let mut labels = CircularLabels {
        labels: HashMap::new(),
    };
    let mut pos = 0;
    loop {
        let (val, next) = read_token_with_base(
            &chars,
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
        pos = skip_whitespace_and_comments(&chars, next);
        if pos >= chars.len() {
            return Ok((EOF, pos));
        }
    }
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
) -> Result<(BlissVal, usize), BlissError> {
    read_token_with_base(chars, pos, labels, 10, false, true, 0)
}

fn read_token_with_base(
    chars: &[char],
    mut pos: usize,
    labels: &mut CircularLabels,
    read_base: u32,
    read_eval: bool,
    read_circular: bool,
    depth: usize,
) -> Result<(BlissVal, usize), BlissError> {
    if depth > MAX_READER_NESTING {
        return Err(BlissError::StreamError(
            "reader nesting limit exceeded".into(),
        ));
    }
    // Skip whitespace and line comments
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Ok((EOF, pos));
    }
    let ch = chars[pos];
    match ch {
        '(' => read_list_with_base(
            chars,
            pos + 1,
            labels,
            read_base,
            read_eval,
            read_circular,
            depth + 1,
        ),
        ')' => Err(BlissError::StreamError("unexpected ')'".into())),
        '"' => read_string(chars, pos + 1),
        '\'' => {
            let (val, p) = read_token_with_base(
                chars,
                pos + 1,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            let quote_sym = BlissVal::from_symbol_index(intern_symbol("QUOTE"));
            Ok((make_list(&[quote_sym, val]), p))
        }
        '`' => {
            let (val, p) = read_token_with_base(
                chars,
                pos + 1,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            let qq_sym = BlissVal::from_symbol_index(intern_symbol("BLISS::QUASIQUOTE"));
            Ok((make_list(&[qq_sym, val]), p))
        }
        ',' => {
            if pos + 1 < chars.len() && chars[pos + 1] == '@' {
                let (val, p) = read_token_with_base(
                    chars,
                    pos + 2,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                let uqs_sym = BlissVal::from_symbol_index(intern_symbol("BLISS::UNQUOTE-SPLICING"));
                Ok((make_list(&[uqs_sym, val]), p))
            } else {
                let (val, p) = read_token_with_base(
                    chars,
                    pos + 1,
                    labels,
                    read_base,
                    read_eval,
                    read_circular,
                    depth + 1,
                )?;
                let uq_sym = BlissVal::from_symbol_index(intern_symbol("BLISS::UNQUOTE"));
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

fn ensure_nesting_within_limit(chars: &[char]) -> Result<(), BlissError> {
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
                    return Err(BlissError::StreamError(
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
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    let mut elements: Vec<BlissVal> = Vec::new();
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unterminated list".into()));
        }
        if chars[pos] == ')' {
            return Ok((make_list(&elements), pos + 1));
        }
        if chars[pos] == '.' {
            // Check if it's a dot token (followed by whitespace or delimiter)
            if pos + 1 >= chars.len() || is_delimiter(chars[pos + 1]) {
                if elements.is_empty() {
                    return Err(BlissError::StreamError("dot at start of list".into()));
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
                    return Err(BlissError::StreamError("multiple objects after dot".into()));
                }
                // Build dotted list
                let mut result = cdr_val;
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

fn read_string(chars: &[char], mut pos: usize) -> Result<(BlissVal, usize), BlissError> {
    let mut s = String::new();
    loop {
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unterminated string".into()));
        }
        match chars[pos] {
            '"' => return Ok((alloc_string(&s), pos + 1)),
            '\\' => {
                pos += 1;
                if pos >= chars.len() {
                    return Err(BlissError::StreamError("unterminated string escape".into()));
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
fn read_atom(chars: &[char], pos: usize) -> Result<(BlissVal, usize), BlissError> {
    read_atom_with_base(chars, pos, 10)
}

fn read_atom_with_base(
    chars: &[char],
    pos: usize,
    read_base: u32,
) -> Result<(BlissVal, usize), BlissError> {
    let (token, end, has_escape) = collect_token(chars, pos)?;
    parse_token_with_base(&token, has_escape, read_base).map(|v| (v, end))
}

/// Collect a token respecting single-escape (\) and multiple-escape (|...|).
/// Returns (token_chars_with_case_info, end_position, had_any_escape).
fn collect_token(chars: &[char], mut pos: usize) -> Result<(TokenChars, usize, bool), BlissError> {
    // Each element is (char, escaped) where escaped means preserve case
    let mut token: TokenChars = Vec::new();
    let mut in_multiple_escape = false;
    let mut had_escape = false;

    while pos < chars.len() {
        let c = chars[pos];
        if in_multiple_escape {
            if c == '|' {
                in_multiple_escape = false;
                pos += 1;
                continue;
            }
            token.push((c, true));
            pos += 1;
            continue;
        }
        match c {
            '\\' => {
                had_escape = true;
                pos += 1;
                if pos >= chars.len() {
                    return Err(BlissError::StreamError("trailing single escape".into()));
                }
                token.push((chars[pos], true));
                pos += 1;
            }
            '|' => {
                had_escape = true;
                in_multiple_escape = true;
                pos += 1;
            }
            c if is_delimiter(c) => break,
            c => {
                token.push((c, false));
                pos += 1;
            }
        }
    }
    if in_multiple_escape {
        return Err(BlissError::StreamError(
            "unterminated multiple escape".into(),
        ));
    }
    Ok((token, pos, had_escape))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn parse_token(token: &[(char, bool)], has_escape: bool) -> Result<BlissVal, BlissError> {
    parse_token_with_base(token, has_escape, 10)
}

fn parse_token_with_base(
    token: &[(char, bool)],
    has_escape: bool,
    read_base: u32,
) -> Result<BlissVal, BlissError> {
    // Build the upcased name (upcased for non-escaped chars)
    let name: String = token
        .iter()
        .map(
            |&(c, escaped)| {
                if escaped { c } else { c.to_ascii_uppercase() }
            },
        )
        .collect();

    if name.is_empty() {
        return Err(BlissError::StreamError("empty token".into()));
    }

    // Don't try numeric interpretation if there are escape chars
    if !has_escape {
        // Check for package-qualified symbols first
        if let Some(result) = try_package_qualified(&name)? {
            return Ok(result);
        }
        // Check for keyword symbols
        if let Some(kw_name) = name.strip_prefix(':') {
            if kw_name.is_empty() {
                return Err(BlissError::StreamError("empty keyword".into()));
            }
            let full = format!("KEYWORD:{}", kw_name);
            let idx = intern_symbol(&full);
            return Ok(BlissVal::from_symbol_index(idx));
        }
        // Try numeric parse
        match try_parse_number_with_base(&name, read_base) {
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
    let idx = intern_symbol(&name);
    Ok(BlissVal::from_symbol_index(idx))
}

fn try_package_qualified(name: &str) -> Result<Option<BlissVal>, BlissError> {
    // Check for PKG::SYM or PKG:SYM (but not :keyword which starts with :)
    if name.starts_with(':') {
        return Ok(None);
    }
    if let Some(colon_pos) = name.find(':') {
        let pkg = &name[..colon_pos];
        let rest = &name[colon_pos + 1..];
        let (sym_name, _internal) = if let Some(stripped) = rest.strip_prefix(':') {
            (stripped, true)
        } else {
            (rest, false)
        };
        // Known packages: CL, KEYWORD, BLISS, COMMON-LISP
        match pkg {
            "CL" | "COMMON-LISP" => {
                if sym_name == "NIL" {
                    return Ok(Some(NIL));
                }
                if sym_name == "T" {
                    return Ok(Some(T));
                }
                let idx = intern_symbol(sym_name);
                Ok(Some(BlissVal::from_symbol_index(idx)))
            }
            "KEYWORD" => {
                let full = format!("KEYWORD:{}", sym_name);
                let idx = intern_symbol(&full);
                Ok(Some(BlissVal::from_symbol_index(idx)))
            }
            "BLISS" => {
                let full = format!("BLISS::{}", sym_name);
                let idx = intern_symbol(&full);
                Ok(Some(BlissVal::from_symbol_index(idx)))
            }
            _ if package_exists(pkg) => {
                let full = format!("{}:{}", pkg, sym_name);
                let idx = intern_symbol(&full);
                Ok(Some(BlissVal::from_symbol_index(idx)))
            }
            _ => Err(BlissError::StreamError(format!(
                "package not found: {}",
                pkg
            ))),
        }
    } else {
        Ok(None)
    }
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn try_parse_number(s: &str) -> Result<Option<BlissVal>, BlissError> {
    try_parse_number_with_base(s, 10)
}

fn try_parse_number_with_base(s: &str, read_base: u32) -> Result<Option<BlissVal>, BlissError> {
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
                    return Err(BlissError::ArithmeticError(
                        "division by zero in ratio".into(),
                    ));
                }
                return Ok(Some(alloc_ratio(
                    BlissVal::from_fixnum(n),
                    BlissVal::from_fixnum(d),
                )));
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
        if let Some(f) = parse_decimal_float(s) {
            return Ok(Some(BlissVal::from_single_float(f)));
        }
    }
    // Integer with read_base. Integers that fit the 61-bit fixnum range are
    // immediates; anything larger (including values that overflow i64) becomes
    // a bignum (§1.8.1) rather than silently degrading to a symbol.
    let trimmed = s.trim_start_matches('+');
    if let Ok(n) = i64::from_str_radix(trimmed, read_base) {
        if fits_fixnum(n) {
            return Ok(Some(BlissVal::from_fixnum(n)));
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

/// True when `n` fits the 61-bit signed fixnum range.
fn fits_fixnum(n: i64) -> bool {
    const MAX: i64 = (1 << 60) - 1;
    const MIN: i64 = -(1 << 60);
    (MIN..=MAX).contains(&n)
}

/// Parse a base-10 float literal following CL float syntax, normalizing any
/// exponent marker (e/s/f/d/l) to `e`. Returns `None` for non-floats.
fn parse_decimal_float(s: &str) -> Option<f32> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    let mut has_digit = false;
    let mut has_dot = false;
    let mut has_exp = false;
    let mut prev_digit = false;

    if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
        out.push(chars[i]);
        i += 1;
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            has_digit = true;
            prev_digit = true;
            out.push(c);
            i += 1;
        } else if c == '.' && !has_dot && !has_exp {
            has_dot = true;
            prev_digit = false;
            out.push('.');
            i += 1;
        } else if !has_exp
            && prev_digit
            && matches!(c, 'e' | 'E' | 's' | 'S' | 'f' | 'F' | 'd' | 'D' | 'l' | 'L')
        {
            has_exp = true;
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
    out.parse::<f32>().ok()
}

/// Allocate a bignum from an i64 that does not fit the fixnum range.
fn alloc_bignum_from_i64(n: i64) -> BlissVal {
    let sign = if n < 0 { -1 } else { 1 };
    let mag = n.unsigned_abs();
    alloc_bignum(sign, &[mag])
}

/// Parse a (possibly signed) integer literal in `base` into a bignum.
/// Returns `None` if the token is not a valid integer in that base.
fn parse_bignum(s: &str, base: u32) -> Option<BlissVal> {
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
fn alloc_bignum(sign: i32, limbs: &[u64]) -> BlissVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let ptr = gc_alloc(total_size, type_id::BIGNUM);
    unsafe {
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        BlissVal::from_heap_ptr(ptr)
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
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    if pos >= chars.len() {
        return Err(BlissError::StreamError("unexpected end after #".into()));
    }
    // Check for #nR, #n=, #n#
    if chars[pos].is_ascii_digit() {
        let start = pos;
        while pos < chars.len() && chars[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unexpected end after #n".into()));
        }
        let num: u32 = chars[start..pos]
            .iter()
            .collect::<String>()
            .parse()
            .unwrap();
        match chars[pos].to_ascii_uppercase() {
            'R' => {
                pos += 1;
                return read_radix_integer(chars, pos, num);
            }
            '=' => {
                if !read_circular {
                    return Err(BlissError::StreamError(
                        "circular reader notation is disabled".into(),
                    ));
                }
                pos += 1;
                // Pre-allocate a placeholder cons cell for circular references
                let placeholder = alloc_cons(NIL, NIL);
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
                        let ph_ptr = placeholder.as_ptr() as *mut ConsCell;
                        let val_ptr = val.as_ptr() as *const ConsCell;
                        (*ph_ptr).car = (*val_ptr).car;
                        (*ph_ptr).cdr = (*val_ptr).cdr;
                    }
                    return Ok((placeholder, p));
                }
                // For non-cons values, just update the label
                labels.labels.insert(num, val);
                return Ok((val, p));
            }
            '#' => {
                if !read_circular {
                    return Err(BlissError::StreamError(
                        "circular reader notation is disabled".into(),
                    ));
                }
                pos += 1;
                if let Some(&val) = labels.labels.get(&num) {
                    return Ok((val, pos));
                }
                return Err(BlissError::StreamError(format!("undefined label #{}", num)));
            }
            _ => {
                return Err(BlissError::StreamError(format!(
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
            let (val, p) = read_token_with_base(
                chars,
                pos,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )?;
            let func_sym = BlissVal::from_symbol_index(intern_symbol("FUNCTION"));
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
            // Block comment #| ... |# — possibly nested
            let p = skip_block_comment(chars, pos)?;
            read_token_with_base(
                chars,
                p,
                labels,
                read_base,
                read_eval,
                read_circular,
                depth + 1,
            )
        }
        ':' => {
            // Uninterned symbol
            let (token, end, _) = collect_token(chars, pos)?;
            let name: String = token
                .iter()
                .map(|&(c, esc)| if esc { c } else { c.to_ascii_uppercase() })
                .collect();
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
        '<' => Err(BlissError::StreamError("unreadable object #<".into())),
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
                return Err(BlissError::StreamError("*READ-EVAL* is false".into()));
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
        _ => Err(BlissError::StreamError(format!(
            "unknown # dispatch: {}",
            dispatch
        ))),
    }
}

fn read_char_literal(chars: &[char], pos: usize) -> Result<(BlissVal, usize), BlissError> {
    if pos >= chars.len() {
        return Err(BlissError::StreamError("unexpected end after #\\".into()));
    }
    // Collect char name
    let start = pos;
    let mut end = pos + 1;
    // If first char is alphabetic, read the full name
    if chars[pos].is_ascii_alphabetic() {
        while end < chars.len() && chars[end].is_ascii_alphabetic() {
            end += 1;
        }
    }
    if end - start > 1 {
        let name: String = chars[start..end].iter().collect();
        match name.to_lowercase().as_str() {
            "space" => return Ok((BlissVal::from_char(' '), end)),
            "newline" => return Ok((BlissVal::from_char('\n'), end)),
            "tab" => return Ok((BlissVal::from_char('\t'), end)),
            "return" => return Ok((BlissVal::from_char('\r'), end)),
            "backspace" => return Ok((BlissVal::from_char('\u{08}'), end)),
            "rubout" | "delete" => return Ok((BlissVal::from_char('\u{7F}'), end)),
            "page" => return Ok((BlissVal::from_char('\u{0C}'), end)),
            "linefeed" => return Ok((BlissVal::from_char('\n'), end)),
            "nul" | "null" => return Ok((BlissVal::from_char('\0'), end)),
            _ => {
                if name.len() == 1 {
                    return Ok((BlissVal::from_char(name.chars().next().unwrap()), end));
                }
                return Err(BlissError::StreamError(format!(
                    "unknown character name: {}",
                    name
                )));
            }
        }
    }
    Ok((BlissVal::from_char(chars[pos]), end))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_vector_literal(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    let mut elements = Vec::new();
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unterminated vector".into()));
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
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() || chars[pos] != '(' {
        return Err(BlissError::StreamError("expected ( after #C".into()));
    }
    pos += 1;
    pos = skip_whitespace_and_comments(chars, pos);
    let (real, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth + 1,
    )?;
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
        return Err(BlissError::StreamError(
            "expected ) after #C(real imag".into(),
        ));
    }
    Ok((alloc_complex(real, imag), pos + 1))
}

fn read_bit_vector(chars: &[char], mut pos: usize) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    let start = pos;
    let negative = if pos < chars.len() && (chars[pos] == '+' || chars[pos] == '-') {
        let neg = chars[pos] == '-';
        pos += 1;
        neg
    } else {
        false
    };
    while pos < chars.len() && chars[pos].is_ascii_alphanumeric() && !is_delimiter(chars[pos]) {
        pos += 1;
    }
    let digits: String = chars[start..pos].iter().collect();
    let digits = digits.trim_start_matches('+').trim_start_matches('-');
    let n = i64::from_str_radix(digits, radix)
        .map_err(|_| BlissError::StreamError(format!("invalid radix-{} integer", radix)))?;
    Ok((BlissVal::from_fixnum(if negative { -n } else { n }), pos))
}

fn skip_block_comment(chars: &[char], mut pos: usize) -> Result<usize, BlissError> {
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
    Err(BlissError::StreamError("unterminated block comment".into()))
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
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
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

fn eval_feature_expression(feature: BlissVal) -> bool {
    if feature == NIL {
        return false;
    }
    if feature.is_symbol() {
        let name = feature_symbol_name(feature);
        let bare = name
            .trim_start_matches("KEYWORD:")
            .trim_start_matches(':')
            .trim();
        return matches!(bare, "BLISS");
    }
    if !feature.is_cons() {
        return false;
    }

    let (op, args) = cons_parts(feature);
    if !op.is_symbol() {
        return false;
    }

    match feature_symbol_name(op).as_str() {
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

fn feature_symbol_name(val: BlissVal) -> String {
    match val {
        NIL => "NIL".to_string(),
        T => "T".to_string(),
        _ if val.tag() == bliss_rt::value::TAG_SYMBOL => symbol_name(val.as_symbol_index())
            .unwrap_or_else(|| format!("SYM#{}", val.as_symbol_index())),
        _ => String::new(),
    }
}

fn cons_parts(val: BlissVal) -> (BlissVal, BlissVal) {
    assert!(val.is_cons(), "cons_parts called on non-cons");
    unsafe {
        let cell = val.as_ptr() as *const ConsCell;
        ((*cell).car, (*cell).cdr)
    }
}

fn skip_form(chars: &[char], pos: usize, depth: usize) -> Result<usize, BlissError> {
    if depth > MAX_READER_NESTING {
        return Err(BlissError::StreamError(
            "reader nesting limit exceeded".into(),
        ));
    }
    let pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Ok(pos);
    }

    match chars[pos] {
        '(' => skip_list(chars, pos + 1, depth + 1),
        '"' => skip_string(chars, pos + 1),
        '\'' | '`' => skip_form(chars, pos + 1, depth + 1),
        ',' => {
            if pos + 1 < chars.len() && chars[pos + 1] == '@' {
                skip_form(chars, pos + 2, depth + 1)
            } else {
                skip_form(chars, pos + 1, depth + 1)
            }
        }
        '#' => skip_sharpsign_form(chars, pos + 1, depth + 1),
        _ => skip_atom(chars, pos),
    }
}

fn skip_list(chars: &[char], mut pos: usize, depth: usize) -> Result<usize, BlissError> {
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unterminated list".into()));
        }
        if chars[pos] == ')' {
            return Ok(pos + 1);
        }
        pos = skip_form(chars, pos, depth + 1)?;
    }
}

fn skip_string(chars: &[char], mut pos: usize) -> Result<usize, BlissError> {
    while pos < chars.len() {
        match chars[pos] {
            '\\' => pos += 2,
            '"' => return Ok(pos + 1),
            _ => pos += 1,
        }
    }
    Err(BlissError::StreamError("unterminated string".into()))
}

fn skip_atom(chars: &[char], pos: usize) -> Result<usize, BlissError> {
    let (_token, end, _escaped) = collect_token(chars, pos)?;
    Ok(end)
}

fn skip_sharpsign_form(chars: &[char], mut pos: usize, depth: usize) -> Result<usize, BlissError> {
    if pos >= chars.len() {
        return Err(BlissError::StreamError("unexpected end after #".into()));
    }

    if chars[pos].is_ascii_digit() {
        while pos < chars.len() && chars[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unexpected end after #n".into()));
        }
        return match chars[pos] {
            '=' => skip_form(chars, pos + 1, depth + 1),
            '#' => Ok(pos + 1),
            other => Err(BlissError::StreamError(format!(
                "unknown # dispatch #{}",
                other
            ))),
        };
    }

    let dispatch = chars[pos];
    match dispatch {
        '\'' | '+' | '-' | '.' => skip_form(chars, pos + 1, depth + 1),
        '\\' | ':' | 'b' | 'B' | 'o' | 'O' | 'x' | 'X' => skip_atom(chars, pos + 1),
        '(' => skip_list(chars, pos + 1, depth + 1),
        'C' | 'c' => skip_form(chars, pos + 1, depth + 1),
        '*' => skip_atom(chars, pos + 1),
        'P' | 'p' | 'S' | 's' => skip_form(chars, pos + 1, depth + 1),
        '<' => skip_atom(chars, pos + 1),
        '|' => skip_block_comment(chars, pos + 1),
        _ => Err(BlissError::StreamError(format!(
            "unknown # dispatch: {}",
            dispatch
        ))),
    }
}

/// Coerce a numeric BlissVal to f64 for mixed-type arithmetic.
fn numeric_to_f64(v: BlissVal) -> Option<f64> {
    if v.is_fixnum() {
        Some(v.as_fixnum() as f64)
    } else if v.is_single_float() {
        Some(v.as_single_float() as f64)
    } else {
        None
    }
}

fn eval_read_time_form_with_hook(form: BlissVal) -> Result<BlissVal, BlissError> {
    if let Some(hook) = read_eval_hook() {
        return hook(form);
    }
    eval_read_time_form(form)
}

pub fn eval_read_time_form(form: BlissVal) -> Result<BlissVal, BlissError> {
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
            return Err(BlissError::UnboundVariable(form));
        }
        if form.is_heap_object() {
            unsafe {
                let ptr = form.as_ptr();
                let header = *(ptr as *const ObjectHeader);
                if header.type_id() == type_id::SIMPLE_BASE_STRING {
                    return Ok(form);
                }
            }
        }
        return Err(BlissError::TypeError {
            datum: form,
            expected: "read-time evaluable form".into(),
        });
    }

    let (operator, args) = cons_parts(form);
    if !operator.is_symbol() {
        return Err(BlissError::TypeError {
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
                .ok_or_else(|| BlissError::Internal("QUOTE: missing argument".into()))
        }
        "IF" => {
            let argv = list_to_vec(args)?;
            if argv.is_empty() {
                return Err(BlissError::Internal("IF: missing test".into()));
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
                return Err(BlissError::Internal("CAR: expected 1 argument".into()));
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
                return Err(BlissError::Internal("CDR: expected 1 argument".into()));
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
                return Err(BlissError::Internal("CONS: expected 2 arguments".into()));
            }
            Ok(alloc_cons(argv[0], argv[1]))
        }
        "LIST" => Ok(make_list(&eval_read_time_args(args)?)),
        "EQ" => {
            let argv = eval_read_time_args(args)?;
            if argv.len() != 2 {
                return Err(BlissError::Internal("EQ: expected 2 arguments".into()));
            }
            Ok(if argv[0] == argv[1] { T } else { NIL })
        }
        _ => Err(BlissError::StreamError(format!(
            "read-eval not supported for {}",
            operator_name
        ))),
    }
}

fn list_to_vec(list: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    let mut result = Vec::new();
    let mut current = list;
    while current.is_cons() {
        let (car, cdr) = cons_parts(current);
        result.push(car);
        current = cdr;
    }
    if current != NIL {
        return Err(BlissError::TypeError {
            datum: list,
            expected: "proper list".into(),
        });
    }
    Ok(result)
}

fn eval_read_time_args(args: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    list_to_vec(args)?
        .into_iter()
        .map(eval_read_time_form)
        .collect()
}

fn eval_read_time_arithmetic(op: &str, args: BlissVal) -> Result<BlissVal, BlissError> {
    let argv = eval_read_time_args(args)?;
    if argv.is_empty() {
        return Ok(match op {
            "+" | "-" => BlissVal::from_fixnum(0),
            "*" => BlissVal::from_fixnum(1),
            _ => unreachable!(),
        });
    }

    if argv.iter().all(|value| value.is_fixnum()) {
        let mut iter = argv.iter().map(|value| value.as_fixnum());
        let first = iter
            .next()
            .ok_or_else(|| BlissError::Internal("missing arithmetic operand".into()))?;
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
        return Ok(BlissVal::from_fixnum(total));
    }

    let mut iter = argv.into_iter();
    let first_value = iter
        .next()
        .ok_or_else(|| BlissError::Internal("missing arithmetic operand".into()))?;
    let first = numeric_to_f64(first_value).ok_or_else(|| BlissError::TypeError {
        datum: first_value,
        expected: "number".into(),
    })?;

    let total = match op {
        "+" => iter.try_fold(first, |acc, value| {
            numeric_to_f64(value)
                .map(|number| acc + number)
                .ok_or_else(|| BlissError::TypeError {
                    datum: value,
                    expected: "number".into(),
                })
        })?,
        "*" => iter.try_fold(first, |acc, value| {
            numeric_to_f64(value)
                .map(|number| acc * number)
                .ok_or_else(|| BlissError::TypeError {
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
                        .ok_or_else(|| BlissError::TypeError {
                            datum: value,
                            expected: "number".into(),
                        })
                })?
            }
        }
        _ => unreachable!(),
    };

    Ok(BlissVal::from_single_float(total as f32))
}

fn read_pathname_literal(chars: &[char], pos: usize) -> Result<(BlissVal, usize), BlissError> {
    // #P"string" — parse the string that follows
    if pos >= chars.len() || chars[pos] != '"' {
        return Err(BlissError::StreamError("expected string after #P".into()));
    }
    let (string_val, end) = read_string(chars, pos + 1)?;
    Ok((alloc_pathname(string_val), end))
}

#[expect(
    dead_code,
    reason = "kept for bootstrap reader entrypoints not yet wired through public APIs"
)]
fn read_struct_literal(
    chars: &[char],
    pos: usize,
    labels: &mut CircularLabels,
) -> Result<(BlissVal, usize), BlissError> {
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
) -> Result<(BlissVal, usize), BlissError> {
    // #S(name slot-key slot-value ...) — parse struct literal
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() || chars[pos] != '(' {
        return Err(BlissError::StreamError("expected ( after #S".into()));
    }
    pos += 1;
    pos = skip_whitespace_and_comments(chars, pos);
    if pos >= chars.len() {
        return Err(BlissError::StreamError("unterminated #S literal".into()));
    }
    if chars[pos] == ')' {
        return Err(BlissError::StreamError(
            "#S() requires a struct name".into(),
        ));
    }
    // Read struct name
    let (name_val, p) = read_token_with_base(
        chars,
        pos,
        labels,
        read_base,
        read_eval,
        read_circular,
        depth + 1,
    )?;
    pos = p;
    // Read remaining slot key-value pairs as a flat list
    let mut slots = Vec::new();
    loop {
        pos = skip_whitespace_and_comments(chars, pos);
        if pos >= chars.len() {
            return Err(BlissError::StreamError("unterminated #S literal".into()));
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

pub fn make_readtable(from: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    let rt = alloc_readtable();
    if let Some(src) = from {
        if src.tag() == TAG_HEAP_OBJECT {
            // Copy macro char settings from src to rt
            let src_key = src.0 & !bliss_rt::value::TAG_MASK;
            let rt_key = rt.0 & !bliss_rt::value::TAG_MASK;
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

pub fn copy_readtable(from: BlissVal, to: Option<BlissVal>) -> Result<BlissVal, BlissError> {
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
    Ok(dest)
}

pub fn set_macro_character(
    readtable: BlissVal,
    ch: char,
    function: BlissVal,
    non_terminating: bool,
) -> Result<(), BlissError> {
    let key = readtable.0 & !bliss_rt::value::TAG_MASK;
    let mut guard = MACRO_CHARS.lock().unwrap();
    let table = guard.get_or_insert_with(HashMap::new);
    table.insert((key, ch), (function, non_terminating));
    Ok(())
}

pub fn get_macro_character(
    readtable: BlissVal,
    ch: char,
) -> Result<(Option<BlissVal>, bool), BlissError> {
    let key = readtable.0 & !bliss_rt::value::TAG_MASK;
    let guard = MACRO_CHARS.lock().unwrap();
    if let Some(table) = guard.as_ref() {
        if let Some(&(func, nt)) = table.get(&(key, ch)) {
            return Ok((Some(func), nt));
        }
    }
    Ok((None, false))
}

pub fn set_dispatch_macro_character(
    readtable: BlissVal,
    disp_char: char,
    sub_char: char,
    function: BlissVal,
) -> Result<(), BlissError> {
    let key = readtable.0 & !bliss_rt::value::TAG_MASK;
    let mut guard = DISPATCH_SUB_CHARS.lock().unwrap();
    let table = guard.get_or_insert_with(HashMap::new);
    table.insert((key, disp_char, sub_char), function);
    Ok(())
}

pub fn get_dispatch_macro_character(
    readtable: BlissVal,
    disp_char: char,
    sub_char: char,
) -> Result<Option<BlissVal>, BlissError> {
    let key = readtable.0 & !bliss_rt::value::TAG_MASK;
    let guard = DISPATCH_SUB_CHARS.lock().unwrap();
    if let Some(table) = guard.as_ref() {
        if let Some(&func) = table.get(&(key, disp_char, sub_char)) {
            return Ok(Some(func));
        }
    }
    Ok(None)
}

pub fn make_dispatch_macro_character(
    readtable: BlissVal,
    ch: char,
    non_terminating: bool,
) -> Result<(), BlissError> {
    let key = readtable.0 & !bliss_rt::value::TAG_MASK;
    let mut guard = DISPATCH_CHARS.lock().unwrap();
    let table = guard.get_or_insert_with(HashMap::new);
    table.insert((key, ch), non_terminating);
    // Also register as a macro char
    set_macro_character(readtable, ch, T, non_terminating)?;
    Ok(())
}
