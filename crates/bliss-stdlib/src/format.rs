//! FORMAT and pretty-printer.
//!
//! See spec §5.9.

use bliss_rt::error::BlissError;
use bliss_rt::lock_order::{LockLevel, OrderedMutex};
use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, T};

// ── User print-object hook ────────────────────────────────────────
//
// FORMAT's `~A`/`~S` must honour user-defined CLOS `print-object` methods, but
// method dispatch lives in the interpreter, which this crate cannot call
// directly. The interpreter installs a hook that, given an instance and the
// `*print-escape*` mode, returns the method's rendering (or None to fall back to
// the built-in `#<CLASS>` form). Mirrors the reader's hook pattern.
type PrintObjectHook = fn(BlissVal, bool) -> Option<String>;
static PRINT_OBJECT_HOOK: OrderedMutex<Option<PrintObjectHook>> =
    OrderedMutex::new(LockLevel::CodeCache, 30, "print-object hook", None);

pub fn set_print_object_hook(hook: Option<PrintObjectHook>) {
    *PRINT_OBJECT_HOOK.lock().unwrap() = hook;
}

thread_local! {
    /// Structural nesting depth for *PRINT-LEVEL* (0 at the top level).
    /// Incremented around each list body; balanced, so it returns to 0.
    static PRINT_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn dispatch_print_object(v: BlissVal, escapep: bool) -> Option<String> {
    let hook = *PRINT_OBJECT_HOOK.lock().unwrap();
    hook.and_then(|h| h(v, escapep))
}

// ── String allocation ─────────────────────────────────────────────

/// Allocate a BlissVal string using the interned string table from the
/// streams module.  This ensures that format-produced strings compare
/// pointer-equal to strings created via `make_lisp_string()`.
fn make_bliss_string(s: &str) -> BlissVal {
    crate::streams::make_lisp_string(s)
}

// ── String extraction ────────────────────────────────────────────

/// Extract Rust string from a BlissString heap object.
/// Returns None if v is not a string-typed heap object.
fn extract_bliss_string(v: BlissVal) -> Option<String> {
    if let Some(s) = crate::pathnames::registered_string(v) {
        return Some(s);
    }
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let tid = header.type_id();
        if tid != type_id::SIMPLE_BASE_STRING && tid != type_id::SIMPLE_CHARACTER_STRING {
            return None;
        }
        Some(bliss_rt::object::read_simple_string(ptr))
    }
}

/// The current `*PRINT-BASE*` radix (CLHS 22.1.1.1), read directly from the
/// special's global value cell — which a dynamic `(let ((*print-base* r)) …)`
/// save/restores — so BOTH printers (the interpreter's `print_val` and this
/// stdlib printer) honour it identically, keeping tree-walked and compiled
/// integer output the same (bliss-82lz). Clamped to `[2, 36]`; defaults to 10
/// when unbound or out of range. Reads via `find_index` (the reliable registry
/// lookup), NOT `resolve_sym`, which mints a distinct symbol whose cell stays at
/// the default.
/// Apply `*PRINT-CASE*` to an (internally upper-case) symbol name for output.
/// `:UPCASE` (the default) leaves it unchanged; `:DOWNCASE` lower-cases; and
/// `:CAPITALIZE` title-cases each alphanumeric word. CLHS 22.1.3.3.
pub fn apply_print_case(name: &str) -> String {
    let case = bliss_rt::symbols::find_index("*PRINT-CASE*")
        .and_then(bliss_rt::symbols::symbol_value)
        .filter(|v| v.is_symbol())
        .and_then(|v| bliss_compiler::reader::symbol_name(v.as_symbol_index()));
    match case.as_deref() {
        Some("KEYWORD:DOWNCASE") => name.to_lowercase(),
        Some("KEYWORD:CAPITALIZE") => {
            let mut out = String::with_capacity(name.len());
            let mut word_start = true;
            for c in name.chars() {
                if c.is_alphanumeric() {
                    if word_start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    word_start = false;
                } else {
                    out.push(c);
                    word_start = true;
                }
            }
            out
        }
        _ => name.to_string(), // :UPCASE or unset
    }
}

/// `*PRINT-LENGTH*`: the max number of elements of a list/vector to print before
/// `...`, or `None` (unbounded) when the variable is NIL/unset.
pub fn print_length() -> Option<usize> {
    bliss_rt::symbols::find_index("*PRINT-LENGTH*")
        .and_then(bliss_rt::symbols::symbol_value)
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum().max(0) as usize)
}

/// `*PRINT-LEVEL*`: the max nesting depth to print before `#`, or `None`
/// (unbounded) when the variable is NIL/unset.
pub fn print_level() -> Option<usize> {
    bliss_rt::symbols::find_index("*PRINT-LEVEL*")
        .and_then(bliss_rt::symbols::symbol_value)
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum().max(0) as usize)
}

/// `*PRINT-CIRCLE*`: when non-NIL, shared and circular structure is printed
/// with `#N=`/`#N#` labels instead of looping forever.
pub fn print_circle_active() -> bool {
    bliss_rt::symbols::find_index("*PRINT-CIRCLE*")
        .and_then(bliss_rt::symbols::symbol_value)
        .map(|v| !v.is_nil())
        .unwrap_or(false)
}

/// `*PRINT-GENSYM*` (default T): when true, an uninterned symbol prints with the
/// `#:` prefix under escape printing so it reads back as a fresh symbol; NIL
/// suppresses the prefix (CLHS 22.1.3.3).
pub fn print_gensym() -> bool {
    bliss_rt::symbols::find_index("*PRINT-GENSYM*")
        .and_then(bliss_rt::symbols::symbol_value)
        .map(|v| !v.is_nil())
        .unwrap_or(true)
}

// ── *PRINT-CIRCLE* support (bliss-dlil) ──────────────────────────────────
//
// A two-pass scheme shared by BOTH printers (cli.rs `print_val` and this
// module's `blissval_to_print_string`), so `princ`/`print`/`prin1` and
// `prin1-to-string`/`format ~S` behave identically:
//
//   Pass 1 (`CircleTable::build`) walks the structure once, counting how many
//   times each cons is reached by object identity (its `as_ptr()` address).
//   Conses reached ≥2 times are "shared" (this includes a circular back-edge,
//   which reaches the head a second time). Cycles can't diverge because the
//   scan stops recursing the moment a cons is seen again.
//
//   Pass 2 is the normal recursive print, but each printer asks the table,
//   per cons, whether to emit a `#N=` label (first visit of a shared node),
//   a `#N#` back-reference (subsequent visit — do NOT recurse), or nothing.
//
// The table keys on raw addresses, holds no `BlissVal` roots, and is only
// valid while a single print is in flight — printing plain lists/vectors does
// not allocate, so nothing moves. (A user `print-object` method that both
// allocates and is reached under `*print-circle*` t could invalidate an
// address; that exotic combination is out of scope and matches the pre-existing
// assumption that the printers deref cons pointers directly.)

/// What to emit for a given cons under `*print-circle*`.
pub enum CircleMark {
    /// Not shared — print the cons normally.
    NotShared,
    /// First time this shared node is printed: emit `#N=` then its contents.
    First(u32),
    /// Already printed once: emit `#N#` and do not recurse.
    Repeat(u32),
}

struct CircleTable {
    /// address → times reached during the scan (retained only for count ≥ 2).
    counts: std::collections::HashMap<usize, u32>,
    /// address → label number, assigned lazily in first-print order.
    labels: std::collections::HashMap<usize, u32>,
    next_label: u32,
}

impl CircleTable {
    fn build(root: BlissVal) -> Option<CircleTable> {
        if !print_circle_active() {
            return None;
        }
        let mut t = CircleTable {
            counts: std::collections::HashMap::new(),
            labels: std::collections::HashMap::new(),
            next_label: 1,
        };
        t.scan(root);
        // Keep only genuinely shared/circular conses.
        t.counts.retain(|_, c| *c >= 2);
        if t.counts.is_empty() {
            None
        } else {
            Some(t)
        }
    }

    /// Count cons multiplicities. Recurses on cars (bounded by nesting depth)
    /// and iterates the cdr spine (bounded by nothing, but adds no stack), and
    /// stops at any cons seen a second time so cycles terminate.
    fn scan(&mut self, root: BlissVal) {
        let mut cur = root;
        loop {
            if !cur.is_cons() {
                return;
            }
            let addr = unsafe { cur.as_ptr() } as usize;
            let c = self.counts.entry(addr).or_insert(0);
            *c += 1;
            if *c > 1 {
                return;
            }
            unsafe {
                let ptr = cur.as_ptr() as *const bliss_rt::object::ConsCell;
                self.scan((*ptr).car);
                cur = (*ptr).cdr;
            }
        }
    }

    fn is_shared(&self, v: BlissVal) -> bool {
        v.is_cons() && self.counts.contains_key(&(unsafe { v.as_ptr() } as usize))
    }

    fn visit(&mut self, v: BlissVal) -> CircleMark {
        if !v.is_cons() {
            return CircleMark::NotShared;
        }
        let addr = unsafe { v.as_ptr() } as usize;
        if !self.counts.contains_key(&addr) {
            return CircleMark::NotShared;
        }
        if let Some(&n) = self.labels.get(&addr) {
            CircleMark::Repeat(n)
        } else {
            let n = self.next_label;
            self.next_label += 1;
            self.labels.insert(addr, n);
            CircleMark::First(n)
        }
    }
}

thread_local! {
    static CIRCLE: std::cell::RefCell<Option<CircleTable>> =
        const { std::cell::RefCell::new(None) };
    /// Re-entrancy depth so the table is built once at the outermost print and
    /// torn down when it returns, even though the printers recurse through the
    /// same entry points.
    static CIRCLE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Enter a (possibly nested) print. At the outermost level, build the circle
/// table from `root` if `*print-circle*` is active. Pair with [`circle_exit`].
pub fn circle_enter(root: BlissVal) {
    let d = CIRCLE_DEPTH.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    if d == 1 {
        let table = CircleTable::build(root);
        CIRCLE.with(|c| *c.borrow_mut() = table);
    }
}

/// Leave a print level; drop the table when the outermost level returns.
pub fn circle_exit() {
    let d = CIRCLE_DEPTH.with(|c| {
        let n = c.get().saturating_sub(1);
        c.set(n);
        n
    });
    if d == 0 {
        CIRCLE.with(|c| *c.borrow_mut() = None);
    }
}

/// True when a circle table is active (there is shared/circular structure).
pub fn circle_active() -> bool {
    CIRCLE.with(|c| c.borrow().is_some())
}

/// True when `v` is a shared/circular cons (used to force a dotted tail).
pub fn circle_is_shared(v: BlissVal) -> bool {
    CIRCLE.with(|c| c.borrow().as_ref().is_some_and(|t| t.is_shared(v)))
}

/// Register a visit to cons `v`, assigning a label on first sight.
pub fn circle_visit(v: BlissVal) -> CircleMark {
    CIRCLE.with(|c| {
        let mut b = c.borrow_mut();
        match b.as_mut() {
            Some(t) => t.visit(v),
            None => CircleMark::NotShared,
        }
    })
}

pub fn print_base() -> u32 {
    bliss_rt::symbols::find_index("*PRINT-BASE*")
        .and_then(bliss_rt::symbols::symbol_value)
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum())
        .filter(|&b| (2..=36).contains(&b))
        .map(|b| b as u32)
        .unwrap_or(10)
}

/// Render a fixnum in `radix` (2..=36) with a leading `-` for negatives and
/// upper-cased digits — the plain-integer form used by `~A`/`~S`/PRINT under
/// `*print-base*`. Shared by this printer and the interpreter's `print_val` so
/// both tiers agree (bliss-82lz).
pub fn fixnum_to_radix(n: i64, radix: u32) -> String {
    format_integer(n, radix, false, false, 0, ' ', ',', 3)
}

thread_local! {
    // Set while printing a rational's numerator/denominator so those integer
    // components are NOT individually radix-decorated: a ratio takes a single
    // leading radix specifier around the whole `num/den`, not a per-part
    // decoration (CLHS 22.1.3.1.1; bliss-6i2z).
    static SUPPRESS_INT_RADIX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The current `*PRINT-RADIX*` flag, read from the value cell via `find_index`
/// (like [`print_base`]). Defaults to NIL. bliss-6i2z.
fn print_radix() -> bool {
    bliss_rt::symbols::find_index("*PRINT-RADIX*")
        .and_then(bliss_rt::symbols::symbol_value)
        .map(|v| !v.is_nil())
        .unwrap_or(false)
}

/// True when an integer should be decorated with a `*print-radix*` specifier
/// right now: `*print-radix*` is set AND we are not inside a ratio's part-print
/// (which suppresses per-part decoration). Both printers gate on this.
pub fn print_radix_active() -> bool {
    print_radix() && !SUPPRESS_INT_RADIX.with(|c| c.get())
}

/// Enter/leave the ratio-part context; returns the previous value so the caller
/// can restore it after printing the numerator and denominator.
pub fn suppress_integer_radix(v: bool) -> bool {
    SUPPRESS_INT_RADIX.with(|c| c.replace(v))
}

/// Decorate an integer's already-rendered `digits` (in `base`, sign included)
/// with its `*print-radix*` specifier (CLHS 22.1.3.1.1): base 10 → trailing
/// `.`; base 2/8/16 → leading `#b`/`#o`/`#x`; else `#<base>r`. The caller applies
/// this only when [`print_radix_active`] is true.
pub fn decorate_integer_radix(digits: String, base: u32) -> String {
    match base {
        10 => format!("{}.", digits),
        2 => format!("#b{}", digits),
        8 => format!("#o{}", digits),
        16 => format!("#x{}", digits),
        b => format!("#{}r{}", b, digits),
    }
}

/// The leading radix specifier for a RATIO under `*print-radix*`: `#b`/`#o`/`#x`
/// for base 2/8/16, else `#<base>r` (base 10 → `#10r`, per CLHS — ratios use a
/// leading specifier, not the integer's trailing dot). bliss-6i2z.
pub fn ratio_radix_prefix(base: u32) -> String {
    match base {
        2 => "#b".into(),
        8 => "#o".into(),
        16 => "#x".into(),
        b => format!("#{}r", b),
    }
}

/// Render a bignum (little-endian base-2^64 limbs) in `radix` (2..=36), digits
/// upper-cased. `radix == 10` delegates to the chunked [`bignum_to_decimal`]
/// (faster, and the `~D`/comma path); other radices use straight long division
/// so `*print-base*` is honoured for heap integers too (bliss-82lz).
pub fn bignum_to_radix(sign: i32, limbs: &[u64], radix: u32) -> String {
    if radix == 10 {
        return bignum_to_decimal(sign, limbs);
    }
    if sign == 0 || limbs.iter().all(|&l| l == 0) {
        return "0".into();
    }
    let mut work = limbs.to_vec();
    let mut digits: Vec<char> = Vec::new();
    loop {
        let mut rem: u128 = 0;
        for limb in work.iter_mut().rev() {
            let cur = (rem << 64) | (*limb as u128);
            *limb = (cur / radix as u128) as u64;
            rem = cur % radix as u128;
        }
        digits.push(
            char::from_digit(rem as u32, radix)
                .unwrap()
                .to_ascii_uppercase(),
        );
        while work.len() > 1 && *work.last().unwrap() == 0 {
            work.pop();
        }
        if work.len() == 1 && work[0] == 0 {
            break;
        }
    }
    let mut s = String::new();
    if sign < 0 {
        s.push('-');
    }
    s.extend(digits.iter().rev());
    s
}

/// Render a bignum (little-endian base-2^64 limbs) as a decimal string.
/// Shared with the interpreter's printer so `~A`/`~S`, PRINT, and the REPL all
/// render heap integers the same way (bliss-axe).
pub fn bignum_to_decimal(sign: i32, limbs: &[u64]) -> String {
    if sign == 0 || limbs.iter().all(|&l| l == 0) {
        return "0".into();
    }
    const D: u128 = 1_000_000_000;
    let mut work = limbs.to_vec();
    let mut chunks: Vec<u32> = Vec::new();
    loop {
        let mut rem: u128 = 0;
        for limb in work.iter_mut().rev() {
            let cur = (rem << 64) | (*limb as u128);
            *limb = (cur / D) as u64;
            rem = cur % D;
        }
        chunks.push(rem as u32);
        while work.len() > 1 && *work.last().unwrap() == 0 {
            work.pop();
        }
        if work.len() == 1 && work[0] == 0 {
            break;
        }
    }
    let mut s = String::new();
    if sign < 0 {
        s.push('-');
    }
    for (i, chunk) in chunks.iter().rev().enumerate() {
        if i == 0 {
            s.push_str(&chunk.to_string());
        } else {
            s.push_str(&format!("{:09}", chunk));
        }
    }
    s
}

/// If `v` is a heap-allocated number (bignum or ratio), render it to decimal —
/// otherwise None. Bignums render as digits; ratios as `num/den` (each part is
/// itself a fixnum or bignum). Reads the same object layout the interpreter's
/// numeric tower writes (bliss-axe: previously these fell through to the generic
/// `#<heap-object>` in FORMAT ~A/~S).
fn heap_number_string(v: BlissVal, escapep: bool) -> Option<String> {
    if !v.is_heap_object() {
        return None;
    }
    // Registry-backed pseudo-heap values (registered strings, pathnames) have a
    // sentinel `as_ptr()` that must never be dereferenced.
    if crate::pathnames::registered_string(v).is_some() || crate::pathnames::is_pathname(v) {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        match (*(ptr as *const ObjectHeader)).type_id() {
            type_id::BIGNUM => {
                let off = object_payload_offset(ptr);
                let sign = *(ptr.add(off) as *const i32);
                let n = *(ptr.add(off + 4) as *const u32) as usize;
                let mut limbs = Vec::with_capacity(n);
                for i in 0..n {
                    limbs.push(*(ptr.add(off + 8 + i * 8) as *const u64));
                }
                let base = print_base();
                let digits = bignum_to_radix(sign, &limbs, base);
                Some(if print_radix_active() {
                    decorate_integer_radix(digits, base)
                } else {
                    digits
                })
            }
            type_id::RATIO => {
                let num = *(ptr.add(8) as *const BlissVal);
                let den = *(ptr.add(16) as *const BlissVal);
                let base = print_base();
                let radix = print_radix_active();
                // Print num/den WITHOUT per-part radix decoration; the ratio
                // takes a single leading specifier (bliss-6i2z).
                let prev = suppress_integer_radix(true);
                let body = format!(
                    "{}/{}",
                    blissval_to_print_string(num, escapep),
                    blissval_to_print_string(den, escapep)
                );
                suppress_integer_radix(prev);
                Some(if radix {
                    format!("{}{}", ratio_radix_prefix(base), body)
                } else {
                    body
                })
            }
            type_id::COMPLEX => {
                let rp = *(ptr.add(8) as *const BlissVal);
                let ip = *(ptr.add(16) as *const BlissVal);
                Some(format!(
                    "#C({} {})",
                    blissval_to_print_string(rp, escapep),
                    blissval_to_print_string(ip, escapep)
                ))
            }
            _ => None,
        }
    }
}

/// The payload offset of a heap object: 8, or 16 for a large (>~512KB body)
/// object whose 16-byte header carries a size-extension word at +8. Fixed-
/// offset reads misread large objects — the extension word masquerades as the
/// first payload field (bliss-e9hb, same family as bliss-tjru/bliss-31x8).
///
/// # Safety
/// `ptr` must point at a live object header.
unsafe fn object_payload_offset(ptr: *const u8) -> usize {
    if unsafe { *(ptr as *const ObjectHeader) }.is_large_object() {
        16
    } else {
        8
    }
}

/// Render a heap vector — simple-vector or complex (fill-pointer / adjustable) —
/// as `#(e0 e1 …)`. `None` for non-vector heap objects. A complex vector shows
/// only its active elements (0..fill-pointer).
fn heap_vector_string(v: BlissVal, escapep: bool) -> Option<String> {
    if !v.is_heap_object() {
        return None;
    }
    if crate::pathnames::registered_string(v).is_some() || crate::pathnames::is_pathname(v) {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        // Multidimensional array → `#<rank>A(nested row-major lists)`.
        if (*(ptr as *const ObjectHeader)).type_id() == type_id::MD_ARRAY {
            let storage = *(ptr.add(8) as *const BlissVal);
            let dims_vec = *(ptr.add(16) as *const BlissVal);
            let rank = (*(ptr.add(24) as *const BlissVal)).as_fixnum().max(0) as usize;
            let dbase = dims_vec.as_ptr();
            let doff = object_payload_offset(dbase);
            let dn = *(dbase.add(doff) as *const u64) as usize;
            let mut dims = Vec::with_capacity(dn);
            for k in 0..dn {
                dims.push(
                    (*(dbase.add(doff + 8 + k * 8) as *const BlissVal))
                        .as_fixnum()
                        .max(0) as usize,
                );
            }
            let (nested, _) = md_render(storage.as_ptr(), &dims, 0, escapep);
            return Some(format!("#{rank}A{nested}"));
        }
        let (base, count) = match (*(ptr as *const ObjectHeader)).type_id() {
            type_id::SIMPLE_VECTOR => (
                ptr,
                *(ptr.add(object_payload_offset(ptr)) as *const u64) as usize,
            ),
            type_id::COMPLEX_ARRAY => {
                let storage = *(ptr.add(8) as *const BlissVal);
                (storage.as_ptr(), crate::sequences::cvec_fill_pointer(v))
            }
            _ => return None,
        };
        let boff = object_payload_offset(base);
        let mut s = String::from("#(");
        for i in 0..count {
            if i > 0 {
                s.push(' ');
            }
            let e = *(base.add(boff + 8 + i * 8) as *const BlissVal);
            s.push_str(&blissval_to_print_string(e, escapep));
        }
        s.push(')');
        Some(s)
    }
}

/// Render one axis of a multidimensional array's row-major storage as nested
/// parenthesised lists (CLHS `#nA` syntax). `base` is the storage SIMPLE_VECTOR
/// object pointer, `dims` the remaining axes, `start` the flat offset of this
/// subtree. Returns the rendered subtree and the number of leaf elements it
/// consumed.
unsafe fn md_render(base: *const u8, dims: &[usize], start: usize, escapep: bool) -> (String, usize) {
    if dims.len() <= 1 {
        let n = dims.first().copied().unwrap_or(0);
        let boff = unsafe { object_payload_offset(base) };
        let mut s = String::from("(");
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            let e = unsafe { *(base.add(boff + 8 + (start + i) * 8) as *const BlissVal) };
            s.push_str(&blissval_to_print_string(e, escapep));
        }
        s.push(')');
        (s, n)
    } else {
        let mut s = String::from("(");
        let mut consumed = 0;
        for i in 0..dims[0] {
            if i > 0 {
                s.push(' ');
            }
            let (sub, c) = unsafe { md_render(base, &dims[1..], start + consumed, escapep) };
            s.push_str(&sub);
            consumed += c;
        }
        s.push(')');
        (s, consumed)
    }
}

/// True iff `v` is the interpreter closure representation `(BLISS::CLOSURE . id)`
/// — a callable that must print as `#<FUNCTION>`. Reads only a genuine cons's
/// car (no sentinel dereference).
fn is_closure_cons(v: BlissVal) -> bool {
    if !v.is_cons() {
        return false;
    }
    let car = unsafe { (*(v.as_ptr() as *const bliss_rt::object::ConsCell)).car };
    // NIL and T report is_symbol() == true but have no symbol-table index
    // (as_symbol_index panics on them), so exclude them before indexing.
    if car == NIL || car == T || !car.is_symbol() {
        return false;
    }
    bliss_compiler::reader::symbol_name(car.as_symbol_index())
        .map(|n| n == "BLISS::CLOSURE")
        .unwrap_or(false)
}

/// Ensure the mantissa of a `{:E}`-formatted float carries a decimal point so it
/// reads back as a float (CL ~E always shows one): "1E-3" → "1.0E-3";
/// "1.2345E3" is unchanged.
/// Round/pad a plain decimal string to exactly `d` fraction digits.
///
/// `~F` must round the float's SHORTEST ROUND-TRIPPING DECIMAL, which is the
/// float's printed value, not its exact binary value. Doing the rounding in
/// f64 is not good enough at large magnitudes: the f32 3.4028235e38 has the
/// shortest decimal 340282350000000000000000000000000000000, and neither that
/// decimal nor the original binary value is exactly representable in f64, so
/// `{:.3}` produced 340282349999999991754788743781432688640.000. Operating on
/// the digit string sidesteps binary representation entirely.
///
/// `s` is a Rust `{}`-formatted float: never exponential, optional leading `-`,
/// optional single `.`. Ties round away from zero.
fn round_decimal_string(s: &str, d: usize) -> String {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let sign = if neg { "-" } else { "" };

    if frac_part.len() <= d {
        let pad = "0".repeat(d - frac_part.len());
        return format!("{sign}{int_part}.{frac_part}{pad}");
    }

    // A leading sentinel digit gives a carry out of the most significant place
    // somewhere to land (9.6 -> 10 at d=0).
    let mut digits: Vec<u8> = std::iter::once(0u8)
        .chain(int_part.bytes().map(|b| b - b'0'))
        .chain(frac_part.bytes().map(|b| b - b'0'))
        .collect();
    let int_len = int_part.len() + 1;
    let cut = int_len + d;
    let round_up = digits[cut] >= 5;
    digits.truncate(cut);
    if round_up {
        let mut i = cut;
        while i > 0 {
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }

    let to_str = |ds: &[u8]| -> String { ds.iter().map(|d| (d + b'0') as char).collect() };
    let int_s = to_str(&digits[..int_len]);
    let int_s = int_s.trim_start_matches('0');
    let int_s = if int_s.is_empty() { "0" } else { int_s };
    format!("{sign}{int_s}.{}", to_str(&digits[int_len..]))
}

fn exp_with_decimal_point(s: &str, exp_char: char) -> String {
    let Some((mant, exp)) = s.split_once('E') else {
        return s.to_string();
    };
    // CL always shows a digit on each side of the point, and the exponent
    // ALWAYS carries its sign (CLHS 22.3.3.2) — Rust's `{:E}` writes neither,
    // so `1E10` has to become `1.0e+10`. The marker case comes from the
    // directive's exponent-char parameter (default lowercase `e`), not from
    // Rust's formatter.
    let mant = if mant.contains('.') {
        mant.to_string()
    } else {
        format!("{mant}.0")
    };
    let exp = if exp.starts_with('-') || exp.starts_with('+') {
        exp.to_string()
    } else {
        format!("+{exp}")
    };
    format!("{mant}{exp_char}{exp}")
}

/// CL `~E` with an explicit fraction-digit count `d` (CLHS 22.3.3.2).
/// `k` is the scale factor (default 1: `k` significant digits before the decimal
/// point, `d` after); `e` zero-pads the exponent to that many digits. The
/// exponent always carries a sign. Numbers are computed in f64 and rounded to `d`
/// fraction digits, so binary32 imprecision beyond `d` is discarded (no bliss-8zrb
/// regression on this path).
fn format_e_fixed(f: f64, d: usize, e: Option<usize>, k: i64, exp_char: char) -> String {
    let neg = f.is_sign_negative() && f != 0.0;
    let af = f.abs();
    let factor = 10f64.powi(d as i32);
    let (mant, exp) = if af == 0.0 {
        (0.0f64, 0i64)
    } else {
        // Exponent so the mantissa has `k` integer digits (k>=1): mantissa in
        // [10^(k-1), 10^k). Scale factors <=0 shift the point left of the digits.
        let mut exp = af.log10().floor() as i64 - (k - 1);
        let mut mant = (af / 10f64.powi(exp as i32) * factor).round() / factor;
        // A round-up can carry past the k-digit window (e.g. 9.99 -> 10.0): shift.
        if k >= 1 && mant >= 10f64.powi(k as i32) {
            mant = (mant / 10.0 * factor).round() / factor;
            exp += 1;
        }
        (mant, exp)
    };
    let mant_str = format!("{:.*}", d, mant);
    let exp_sign = if exp < 0 { '-' } else { '+' };
    let exp_digits = match e {
        Some(ee) => format!("{:0width$}", exp.unsigned_abs(), width = ee),
        None => format!("{}", exp.unsigned_abs()),
    };
    format!(
        "{}{mant_str}{exp_char}{exp_sign}{exp_digits}",
        if neg { "-" } else { "" }
    )
}

/// Walk a cons-cell linked list and collect all car values into a Vec.
fn cons_list_to_vec(v: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut current = v;
    while current.is_cons() {
        unsafe {
            let ptr = current.as_ptr() as *const bliss_rt::object::ConsCell;
            result.push((*ptr).car);
            current = (*ptr).cdr;
        }
    }
    result
}

// ── Helpers ───────────────────────────────────────────────────────

/// Escape a string body for prin1/~S (`*print-escape*` true): precede each `"`
/// and `\` with `\` so the printed `"…"` reads back as the same string
/// (CLHS 22.1.3.4). Matches cli::print_val's SIMPLE_BASE_STRING escaping — the
/// stdlib printer (prin1-to-string / write-to-string / FORMAT ~S) previously
/// emitted the raw body, so `(prin1-to-string "a\"b")` did not round-trip
/// (bliss-str-esc).
fn escape_string_body(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn blissval_to_print_string(v: BlissVal, escapep: bool) -> String {
    // Establish (at the outermost level) the *print-circle* label table, so
    // shared/circular structure prints with #N=/#N# instead of looping.
    circle_enter(v);
    let s = blissval_to_print_inner(v, escapep);
    circle_exit();
    s
}

/// Print a single-float the way CL requires (CLHS 22.1.3.1.3): free format
/// while `10^-3 <= |x| < 10^7`, exponential notation outside that band, and
/// always with a decimal point so the result reads back as a float.
///
/// Rust's `{}` never switches to an exponent, so bliss used to print
/// `10000000000.0` for `1.0e10` and `0.0000000001` for `1.0e-10`. Both read
/// back correctly, but neither is the printed representation CL specifies.
///
/// This is the single implementation for BOTH printers — `format`'s
/// `blissval_to_print_inner` here and the interpreter's `print_val` — which
/// have drifted before (bliss-i608).
pub fn single_float_to_string(x: f32) -> String {
    if x.is_nan() || x.is_infinite() {
        // Leave the existing non-finite spellings alone; they are not CL
        // external representations and nothing reads them back.
        return format!("{x}");
    }
    let a = x.abs();
    if a != 0.0 && !(1e-3..1e7).contains(&a) {
        // `{:e}` gives the shortest round-tripping mantissa, but spells a whole
        // mantissa without a point ("1e10"); CL wants a digit on each side.
        let s = format!("{x:e}");
        return match s.split_once('e') {
            Some((mantissa, exp)) if !mantissa.contains('.') => {
                format!("{mantissa}.0e{exp}")
            }
            _ => s,
        };
    }
    let s = format!("{x}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// Print a DOUBLE-FLOAT in CL external syntax. Because bliss's
/// `*read-default-float-format*` is SINGLE-FLOAT, a double always carries an
/// explicit `d` exponent marker so it reads back as a double (CLHS 22.1.3.1.3):
/// `1.0d0`, `3.141592653589793d0`, `1.5d-10`. Kept in lockstep with
/// [`single_float_to_string`] so `prin1`/`~S` and the interpreter's `print_val`
/// spell doubles identically.
pub fn double_float_to_string(x: f64) -> String {
    if x.is_nan() || x.is_infinite() {
        return format!("{x}");
    }
    let a = x.abs();
    if a != 0.0 && !(1e-3..1e7).contains(&a) {
        let s = format!("{x:e}");
        return match s.split_once('e') {
            Some((mantissa, exp)) if !mantissa.contains('.') => {
                format!("{mantissa}.0d{exp}")
            }
            Some((mantissa, exp)) => format!("{mantissa}d{exp}"),
            None => s,
        };
    }
    let s = format!("{x}");
    if s.contains('.') {
        format!("{s}d0")
    } else {
        format!("{s}.0d0")
    }
}

fn blissval_to_print_inner(v: BlissVal, escapep: bool) -> String {
    if v.is_nil() {
        return "NIL".into();
    }
    if v == T {
        return "T".into();
    }
    // CLOS instances are heap objects; detect them via the liveness registry
    // before the generic heap-object branch. A condition prints as its report
    // message (~A/princ semantics); any other instance prints as #<CLASS-NAME>.
    if crate::clos::is_instance(v) {
        // A user `print-object` method wins when one applies; else fall back.
        if let Some(s) = dispatch_print_object(v, escapep) {
            return s;
        }
        return format_instance(v);
    }
    if v.is_fixnum() {
        let base = print_base();
        let digits = if base == 10 {
            format!("{}", v.as_fixnum())
        } else {
            fixnum_to_radix(v.as_fixnum(), base)
        };
        return if print_radix_active() {
            decorate_integer_radix(digits, base)
        } else {
            digits
        };
    }
    if v.is_character() {
        let c = v.as_char();
        return if escapep {
            // prin1/~S names the non-graphic characters so they read back
            // (CLHS 22.1.3.2) — must match cli print_val, which was correct while
            // the stdlib printer emitted `#\` + the literal char.
            let mut s = String::from("#\\");
            match c {
                ' ' => s.push_str("Space"),
                '\n' => s.push_str("Newline"),
                '\t' => s.push_str("Tab"),
                '\r' => s.push_str("Return"),
                other => s.push(other),
            }
            s
        } else {
            format!("{}", c)
        };
    }
    if v.is_single_float() {
        return single_float_to_string(v.as_single_float());
    }
    if v.is_double_float() {
        return double_float_to_string(v.as_double_float());
    }
    if v.is_symbol() {
        let idx = v.as_symbol_index();
        let name =
            bliss_compiler::reader::symbol_name(idx).unwrap_or_else(|| format!("SYM#{}", idx));
        // A keyword's registry name is `KEYWORD:FOO`; it must PRINT as `:FOO`
        // (both prin1/~S and princ/~A), which is what a reader — including a
        // SLIME/slynk client parsing the wire — round-trips back to the keyword.
        if let Some(bare) = name
            .strip_prefix("KEYWORD::")
            .or_else(|| name.strip_prefix("KEYWORD:"))
        {
            // prin1/~S prints the readable `:FOO`; princ/~A drops the marker.
            let bare = apply_print_case(bare);
            return if escapep {
                format!(":{}", bare)
            } else {
                bare
            };
        }
        // An uninterned symbol (make-symbol/gensym, no home package) prints as
        // `#:NAME` under prin1/~S so it reads back as a fresh uninterned symbol
        // (CLHS 22.1.3.3, *print-gensym* default T); princ/~A drops the marker.
        // slynk's UNPARSE-NAME relies on this: `(subseq (prin1-to-string
        // (make-symbol s)) 2)` strips the `#:` — without it that subseq errors.
        if bliss_compiler::reader::is_uninterned(idx) {
            let name = apply_print_case(&name);
            return if escapep && print_gensym() {
                format!("#:{}", name)
            } else {
                name
            };
        }
        return apply_print_case(name.trim_start_matches("KEYWORD:"));
    }
    // An interpreter closure `(BLISS::CLOSURE . id)` is a function, not the data
    // list it is structurally — print it as #<FUNCTION> (matches cli print_val).
    if is_closure_cons(v) {
        return "#<FUNCTION>".to_string();
    }
    if v.is_cons() {
        // *PRINT-LEVEL*: past the depth limit, a nested list prints as `#`.
        let depth = PRINT_DEPTH.with(|d| d.get());
        if print_level().is_some_and(|lvl| depth >= lvl) {
            return "#".to_string();
        }
        PRINT_DEPTH.with(|d| d.set(depth + 1));
        let s = format_cons(v, escapep);
        PRINT_DEPTH.with(|d| d.set(depth));
        return s;
    }
    if v.is_heap_object() {
        // Pathnames are registry-backed pseudo-heap values: render via their
        // namestring rather than falling through to `#<heap-object>` (which is
        // what a MERGE-PATHNAMES / MAKE-PATHNAME result — not separately string-
        // interned — used to show, e.g. ocicl's "; loading ~A" messages).
        if crate::pathnames::is_pathname(v) {
            if let Ok(ns) = crate::pathnames::namestring(v) {
                if let Some(s) = extract_bliss_string(ns) {
                    // Match cli's print_val: quoted (and body-escaped)
                    // namestring under prin1/~S, bare namestring under princ/~A.
                    return if escapep {
                        format!("\"{}\"", escape_string_body(&s))
                    } else {
                        s
                    };
                }
            }
        }
        // Check if it's a string and extract its content. This runs BEFORE the
        // number check because a registered-string sentinel is a pseudo-heap
        // value whose `as_ptr()` must not be dereferenced (extract_bliss_string
        // resolves it via the registry); a real string is consumed here too.
        if let Some(s) = extract_bliss_string(v) {
            return if escapep {
                format!("\"{}\"", escape_string_body(&s))
            } else {
                s
            };
        }
        // Heap-allocated numbers (bignum, ratio) render as digits, not
        // #<heap-object> (bliss-axe). Only real GC heap objects reach here now.
        if let Some(s) = heap_number_string(v, escapep) {
            return s;
        }
        // A character-typed (fill-pointer / adjustable) array is a string and
        // prints as one, not as #(...) (bliss-9q4).
        if let Some(s) = crate::sequences::cvec_char_contents(v) {
            return if escapep {
                format!("\"{}\"", escape_string_body(&s))
            } else {
                s
            };
        }
        // A bit-vector renders in `#*bits` syntax, not `#(…)` (bliss-zg9). Checked
        // before the general vector path since it is also a rank-1 array.
        if bliss_rt::types::bit_vector_p(v) {
            let len = bliss_rt::types::bit_vector_len(v).unwrap_or(0);
            let mut s = String::with_capacity(2 + len);
            s.push_str("#*");
            for i in 0..len {
                s.push(if bliss_rt::types::bit_vector_ref(v, i) == Some(1) {
                    '1'
                } else {
                    '0'
                });
            }
            return s;
        }
        // Vectors (simple and complex/fill-pointer) render as #(...).
        if let Some(s) = heap_vector_string(v, escapep) {
            return s;
        }
        return format!("#<heap-object {:?}>", v);
    }
    // A non-heap opaque value (e.g. a first-class package object, a meta-handle):
    // let the interpreter's print hook name it (#<PACKAGE …>) before the generic
    // fallback (bliss-bhs).
    if let Some(s) = dispatch_print_object(v, escapep) {
        return s;
    }
    format!("#<object {:?}>", v)
}

/// Print a CLOS instance. Conditions carrying a `format-control` slot render as
/// their report message (the standard `princ`/`~A` behaviour for conditions);
/// every other instance renders as `#<CLASS-NAME>`. Guarded against unbounded
/// re-entry in case a report control string references its own condition.
fn format_instance(v: BlissVal) -> String {
    use std::cell::Cell;
    thread_local! {
        static DEPTH: Cell<u32> = const { Cell::new(0) };
    }
    if DEPTH.with(|d| d.get()) > 8 {
        return instance_class_tag(v);
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    // A DEFSTRUCT instance prints in the readable #S(NAME :slot val …) syntax
    // (CLHS 22.1.3.12), not the generic #<NAME> (bliss-i1i9).
    let class = crate::clos::class_of(v);
    if crate::clos::is_structure_class(class) {
        let result = format_struct(v, class);
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        return result;
    }
    let control_sym =
        BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol("FORMAT-CONTROL"));
    let result = match crate::clos::slot_value(v, control_sym) {
        Ok(control) => match extract_bliss_string(control) {
            Some(control_str) => {
                let args_sym = BlissVal::from_symbol_index(bliss_compiler::reader::intern_symbol(
                    "FORMAT-ARGUMENTS",
                ));
                let args = crate::clos::slot_value(v, args_sym)
                    .ok()
                    .map(cons_list_to_vec)
                    .unwrap_or_default();
                if args.is_empty() {
                    control_str
                } else {
                    match format(NIL, &control_str, &args) {
                        Ok(formatted) => extract_bliss_string(formatted).unwrap_or(control_str),
                        Err(_) => control_str,
                    }
                }
            }
            None => instance_class_tag(v),
        },
        Err(_) => instance_class_tag(v),
    };
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

/// Render a DEFSTRUCT instance as `#S(NAME :slot val :slot val …)` — the
/// readable structure syntax. Slot names print as keywords; slot values print
/// escaped (prin1-style) so the form round-trips. Slots are emitted in the
/// class's slot order (CLHS 22.1.3.12; bliss-i1i9).
fn format_struct(v: BlissVal, class: BlissVal) -> String {
    let name = crate::clos::class_name(class);
    let mut out = String::from("#S(");
    out.push_str(&blissval_to_print_string(name, false));
    for slot in crate::clos::effective_slots(class) {
        if let Ok(val) = crate::clos::slot_value(v, slot) {
            out.push(' ');
            out.push(':');
            out.push_str(&blissval_to_print_string(slot, false));
            out.push(' ');
            out.push_str(&blissval_to_print_string(val, true));
        }
    }
    out.push(')');
    out
}

/// `#<CLASS-NAME>` tag for an instance with no printable report.
fn instance_class_tag(v: BlissVal) -> String {
    let class = crate::clos::class_of(v);
    let name = crate::clos::class_name(class);
    format!("#<{}>", blissval_to_print_string(name, false))
}

/// Print a cons/list the way `~A`/`~S` should: `(a b c)`, nested lists
/// recursively, and dotted tails as `(a . b)`. `escapep` propagates so `~S`
/// escapes strings and characters inside the list.
fn format_cons(v: BlissVal, escapep: bool) -> String {
    // *PRINT-LENGTH*: after this many elements, print `...` and stop.
    let limit = print_length();
    let circle = circle_active();
    let mut out = String::new();
    // *PRINT-CIRCLE* head: a `#N#` back-reference replaces the whole list; a
    // `#N=` label prefixes it on first sight.
    if circle {
        match circle_visit(v) {
            CircleMark::Repeat(n) => return format!("#{n}#"),
            CircleMark::First(n) => {
                out.push('#');
                out.push_str(&n.to_string());
                out.push('=');
            }
            CircleMark::NotShared => {}
        }
    }
    out.push('(');
    let mut current = v;
    let mut count = 0usize;
    let mut first = true;
    loop {
        if current.is_cons() {
            // A shared/circular cons reached in the cdr position prints as a
            // dotted tail so its own #N=/#N# label appears (the head cons,
            // `first`, was already labelled above).
            if !first && circle && circle_is_shared(current) {
                out.push_str(" . ");
                out.push_str(&blissval_to_print_string(current, escapep));
                break;
            }
            if limit.is_some_and(|n| count >= n) {
                if count > 0 {
                    out.push(' ');
                }
                out.push_str("...");
                break;
            }
            unsafe {
                let ptr = current.as_ptr() as *const bliss_rt::object::ConsCell;
                if count > 0 {
                    out.push(' ');
                }
                out.push_str(&blissval_to_print_string((*ptr).car, escapep));
                current = (*ptr).cdr;
            }
            count += 1;
            first = false;
        } else if current.is_nil() {
            break;
        } else {
            out.push_str(" . ");
            out.push_str(&blissval_to_print_string(current, escapep));
            break;
        }
    }
    out.push(')');
    out
}

#[allow(clippy::too_many_arguments)]
fn format_integer(
    n: i64,
    radix: u32,
    colon: bool,
    at_sign: bool,
    mincol: usize,
    padchar: char,
    commachar: char,
    comma_interval: usize,
) -> String {
    let negative = n < 0;
    let abs = if n == i64::MIN {
        (n as u128).wrapping_neg() as u64
    } else {
        n.unsigned_abs()
    };
    let digits = if abs == 0 {
        "0".to_string()
    } else {
        let mut d = String::new();
        let mut v = abs;
        while v > 0 {
            let rem = (v % radix as u64) as u32;
            d.push(char::from_digit(rem, radix).unwrap().to_ascii_uppercase());
            v /= radix as u64;
        }
        d.chars().rev().collect()
    };
    format_integer_parts(
        negative,
        &digits,
        radix,
        colon,
        at_sign,
        mincol,
        padchar,
        commachar,
        comma_interval,
    )
}

/// Format an already-rendered magnitude (`digits`, no sign) in a `~D`/`~B`/`~O`/
/// `~X` field: optional comma grouping (radix 10 + `:`), a `-`/`+` sign, and left
/// padding to `mincol`. Shared by the fixnum path and the bignum path so both
/// honor the same parameters (bliss-1kib).
#[allow(clippy::too_many_arguments)]
fn format_integer_parts(
    negative: bool,
    digits: &str,
    radix: u32,
    colon: bool,
    at_sign: bool,
    mincol: usize,
    padchar: char,
    commachar: char,
    comma_interval: usize,
) -> String {
    let with_commas = if colon && radix == 10 {
        insert_commas(digits, commachar, comma_interval)
    } else {
        digits.to_string()
    };
    let sign = if negative {
        "-"
    } else if at_sign {
        "+"
    } else {
        ""
    };
    let result = format!("{}{}", sign, with_commas);
    if result.len() < mincol {
        let pad: String = std::iter::repeat_n(padchar, mincol - result.len()).collect();
        format!("{}{}", pad, result)
    } else {
        result
    }
}

/// Extract `(negative, magnitude_digits)` from an integer BlissVal (fixnum or
/// bignum) in `radix`, or `None` if `val` is not an integer. Lets FORMAT ~D/~B/
/// ~O/~X render bignums, which used to type-error (only `is_fixnum()` was
/// accepted) even though ~A/PRINT already print them (bliss-1kib).
fn integer_magnitude(val: BlissVal, radix: u32) -> Option<(bool, String)> {
    if val.is_fixnum() {
        let n = val.as_fixnum();
        let negative = n < 0;
        let abs = if n == i64::MIN {
            (n as u128).wrapping_neg() as u64
        } else {
            n.unsigned_abs()
        };
        let digits = if abs == 0 {
            "0".to_string()
        } else {
            let mut d = String::new();
            let mut v = abs;
            while v > 0 {
                let rem = (v % radix as u64) as u32;
                d.push(char::from_digit(rem, radix).unwrap().to_ascii_uppercase());
                v /= radix as u64;
            }
            d.chars().rev().collect()
        };
        return Some((negative, digits));
    }
    if !val.is_heap_object() {
        return None;
    }
    // Registry-backed pseudo-heap values (registered strings, pathnames) carry a
    // sentinel as_ptr() that must never be dereferenced (mirrors
    // heap_number_string's guard).
    if crate::pathnames::registered_string(val).is_some() || crate::pathnames::is_pathname(val) {
        return None;
    }
    unsafe {
        let ptr = val.as_ptr();
        if (*(ptr as *const ObjectHeader)).type_id() != type_id::BIGNUM {
            return None;
        }
        let off = object_payload_offset(ptr);
        let sign = *(ptr.add(off) as *const i32);
        let n = *(ptr.add(off + 4) as *const u32) as usize;
        let mut limbs = Vec::with_capacity(n);
        for i in 0..n {
            limbs.push(*(ptr.add(off + 8 + i * 8) as *const u64));
        }
        // bignum_to_radix emits a leading '-' for negatives; split it back into
        // (negative, magnitude) so the ~D sign/comma/pad logic applies uniformly.
        let s = bignum_to_radix(sign, &limbs, radix);
        let negative = s.starts_with('-');
        Some((negative, s.trim_start_matches('-').to_string()))
    }
}

/// Append `s` to `output` in a `~A`/`~S` field: at least `minpad` `padchar`s of
/// padding, widened to `mincol` total (measured in CHARACTERS, not bytes). With
/// `@` the padding goes on the left (right-justify), otherwise on the right
/// (bliss-znib).
fn pad_format_field(
    output: &mut String,
    s: &str,
    mincol: usize,
    minpad: usize,
    padchar: char,
    at_sign: bool,
) {
    let pad = mincol.saturating_sub(s.chars().count()).max(minpad);
    if at_sign {
        output.extend(std::iter::repeat_n(padchar, pad));
        output.push_str(s);
    } else {
        output.push_str(s);
        output.extend(std::iter::repeat_n(padchar, pad));
    }
}

fn insert_commas(s: &str, commachar: char, interval: usize) -> String {
    if interval == 0 {
        return s.to_string();
    }
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % interval == 0 {
            result.push(commachar);
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn cardinal(n: i64) -> String {
    if n == 0 {
        return "zero".into();
    }
    let mut result = String::new();
    let mut v = n;
    if v < 0 {
        result.push_str("negative ");
        v = -v;
    }
    let ones = [
        "",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    let tens = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if v >= 1_000_000 {
        result.push_str(&cardinal(v / 1_000_000));
        result.push_str(" million");
        v %= 1_000_000;
        if v > 0 {
            result.push(' ');
        }
    }
    if v >= 1000 {
        result.push_str(&cardinal(v / 1000));
        result.push_str(" thousand");
        v %= 1000;
        if v > 0 {
            result.push(' ');
        }
    }
    if v >= 100 {
        result.push_str(ones[v as usize / 100]);
        result.push_str(" hundred");
        v %= 100;
        if v > 0 {
            result.push(' ');
        }
    }
    if v >= 20 {
        result.push_str(tens[v as usize / 10]);
        v %= 10;
        if v > 0 {
            result.push('-');
            result.push_str(ones[v as usize]);
        }
    } else if v > 0 {
        result.push_str(ones[v as usize]);
    }
    result
}

fn ordinal(n: i64) -> String {
    let c = cardinal(n);
    if c.ends_with("one") {
        format!("{}first", &c[..c.len() - 3])
    } else if c.ends_with("two") {
        format!("{}second", &c[..c.len() - 3])
    } else if c.ends_with("three") {
        format!("{}third", &c[..c.len() - 5])
    } else if c.ends_with("five") {
        format!("{}fifth", &c[..c.len() - 4])
    } else if c.ends_with("eight") {
        format!("{}eighth", &c[..c.len() - 5])
    } else if c.ends_with("nine") {
        format!("{}ninth", &c[..c.len() - 4])
    } else if c.ends_with("twelve") {
        format!("{}twelfth", &c[..c.len() - 6])
    } else if c.ends_with('y') {
        format!("{}ieth", &c[..c.len() - 1])
    } else {
        format!("{}th", c)
    }
}

fn to_roman(n: i64, old: bool) -> String {
    if n <= 0 || n > 3999 {
        return format!("{}", n);
    }
    let mut result = String::new();
    let mut v = n as u32;
    let vals: &[(u32, &str)] = if old {
        &[
            (1000, "M"),
            (500, "D"),
            (100, "C"),
            (50, "L"),
            (10, "X"),
            (5, "V"),
            (1, "I"),
        ]
    } else {
        &[
            (1000, "M"),
            (900, "CM"),
            (500, "D"),
            (400, "CD"),
            (100, "C"),
            (90, "XC"),
            (50, "L"),
            (40, "XL"),
            (10, "X"),
            (9, "IX"),
            (5, "V"),
            (4, "IV"),
            (1, "I"),
        ]
    };
    for &(val, sym) in vals {
        while v >= val {
            result.push_str(sym);
            v -= val;
        }
    }
    result
}

fn char_name(c: char) -> String {
    match c {
        ' ' => "Space".into(),
        '\n' => "Newline".into(),
        '\t' => "Tab".into(),
        '\r' => "Return".into(),
        '\x08' => "Backspace".into(),
        '\x7f' => "Rubout".into(),
        '\x0c' => "Page".into(),
        _ => format!("{}", c),
    }
}

// ── Directive parser ──────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Param {
    Num(i64),
    V,
    Hash,
    None,
}

#[allow(dead_code)]
struct Directive {
    params: Vec<Param>,
    colon: bool,
    at_sign: bool,
    ch: char,
    start: usize,
    end: usize,
}

#[allow(dead_code)]
fn parse_directives(control: &str) -> Result<Vec<Directive>, BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut directives = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            let start = i;
            i += 1;
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~ in format string".into()));
            }
            let mut params = Vec::new();
            // Parse parameters
            loop {
                if i >= chars.len() {
                    return Err(BlissError::Internal("dangling ~ in format string".into()));
                }
                let c = chars[i];
                if c == 'v' || c == 'V' {
                    params.push(Param::V);
                    i += 1;
                    if i < chars.len() && chars[i] == ',' {
                        i += 1;
                    }
                } else if c == '#' {
                    params.push(Param::Hash);
                    i += 1;
                    if i < chars.len() && chars[i] == ',' {
                        i += 1;
                    }
                } else if c == '\'' {
                    i += 1;
                    if i < chars.len() {
                        params.push(Param::Num(chars[i] as i64));
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == ',' {
                        i += 1;
                    }
                } else if c.is_ascii_digit() || c == '-' || c == '+' {
                    let mut num_str = String::new();
                    if c == '-' || c == '+' {
                        num_str.push(c);
                        i += 1;
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        num_str.push(chars[i]);
                        i += 1;
                    }
                    params.push(Param::Num(num_str.parse::<i64>().unwrap_or(0)));
                    if i < chars.len() && chars[i] == ',' {
                        i += 1;
                    }
                } else if c == ',' {
                    params.push(Param::None);
                    i += 1;
                } else {
                    break;
                }
            }
            // Parse colon and at-sign
            let mut colon = false;
            let mut at_sign = false;
            while i < chars.len() {
                if chars[i] == ':' {
                    colon = true;
                    i += 1;
                } else if chars[i] == '@' {
                    at_sign = true;
                    i += 1;
                } else {
                    break;
                }
            }
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~ in format string".into()));
            }
            let ch = chars[i];
            i += 1;
            // Handle ~/name/
            let end = if ch == '/' {
                while i < chars.len() && chars[i] != '/' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                i
            } else {
                i
            };
            directives.push(Directive {
                params,
                colon,
                at_sign,
                ch,
                start,
                end,
            });
        } else {
            i += 1;
        }
    }
    Ok(directives)
}

// ── Main format engine ────────────────────────────────────────────

/// Execute a FORMAT directive string. R5.40.
pub fn format(
    destination: BlissVal,
    control_string: &str,
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    // Validate destination
    let to_string = destination.is_nil();
    let to_stdout = destination == T;
    // Detect stream destinations using the streams module's own query
    // function, which correctly understands the StreamState layout.
    let to_stream = !to_string
        && !to_stdout
        && destination.is_heap_object()
        && crate::streams::output_stream_p(destination);
    if !to_string && !to_stdout && !to_stream {
        // Not NIL, not T, not an output stream — check for string type
        if destination.is_heap_object() {
            if !bliss_rt::types::stringp(destination) {
                return Err(BlissError::TypeError {
                    datum: destination,
                    expected: "stream or string-with-fill-pointer".into(),
                });
            }
        } else {
            return Err(BlissError::TypeError {
                datum: destination,
                expected: "NIL, T, stream, or string-with-fill-pointer".into(),
            });
        }
    }

    // Validate matching brackets/braces/parens before executing
    validate_matching(control_string)?;

    let mut output = String::new();
    let mut arg_idx: usize = 0;
    let result = format_impl(control_string, args, &mut arg_idx, &mut output);
    if let Err(e) = &result {
        if std::env::var_os("BLISS_FMT_DBG").is_some() {
            eprintln!(
                ";; fmt TOP error={} control={:?} nargs={}",
                e,
                control_string.chars().take(120).collect::<String>(),
                args.len()
            );
        }
    }
    result?;

    if to_stdout {
        print!("{}", output);
        Ok(NIL)
    } else if to_string {
        Ok(make_bliss_string(&output))
    } else if to_stream {
        // Write each character to the stream using the Gray streams API.
        for ch in output.chars() {
            crate::streams::stream_write_char(destination, BlissVal::from_char(ch))?;
        }
        Ok(NIL)
    } else if destination.is_heap_object() {
        // String with fill pointer — append formatted output in place.
        // We refuse to grow beyond the original allocation because realloc
        // may move the underlying buffer and BlissVal is a value type
        // holding the old pointer — growing would be undefined behavior.
        unsafe {
            let ptr = destination.as_ptr();
            let old_header = *(ptr as *const ObjectHeader);
            // A large object's true footprint lives in the size-extension word
            // at +8; size_units() is the 0xFFFF sentinel there (bliss-e9hb).
            let off = object_payload_offset(ptr);
            let old_padded = if old_header.is_large_object() {
                *(ptr.add(8) as *const u64) as usize
            } else {
                (old_header.size_units() as usize) * 8
            };
            let current_len = *(ptr.add(off) as *const u64) as usize;
            let append_bytes = output.as_bytes();
            let new_len = current_len + append_bytes.len();
            let new_total = off + 8 + new_len;
            let new_padded = (new_total + 7) & !7;

            if new_padded <= old_padded {
                // Fits in existing allocation — append in place.
                std::ptr::copy_nonoverlapping(
                    append_bytes.as_ptr(),
                    ptr.add(off + 8 + current_len),
                    append_bytes.len(),
                );
                // Update length field on the original pointer.
                *(ptr.add(off) as *mut u64) = new_len as u64;
            } else {
                // The formatted output does not fit in the original
                // allocation.  Signal an error rather than risk UB
                // from realloc moving the buffer behind a value-type
                // pointer.
                return Err(BlissError::Internal(format!(
                    "FORMAT: string destination capacity exceeded \
                     (need {} bytes, have {})",
                    new_padded, old_padded
                )));
            }
        }
        Ok(NIL)
    } else {
        Ok(NIL)
    }
}

fn validate_matching(control: &str) -> Result<(), BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            i += 1;
            // Skip params and modifiers
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == ','
                    || chars[i] == '\''
                    || chars[i] == 'v'
                    || chars[i] == 'V'
                    || chars[i] == '#'
                    || chars[i] == ':'
                    || chars[i] == '@'
                    || chars[i] == '-'
                    || chars[i] == '+')
            {
                if chars[i] == '\'' {
                    i += 1;
                } // skip char param
                i += 1;
            }
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~ in format string".into()));
            }
            match chars[i] {
                '{' => stack.push('}'),
                '}' => {
                    if stack.pop() != Some('}') {
                        return Err(BlissError::Internal("unmatched ~}".into()));
                    }
                }
                '[' => stack.push(']'),
                ']' => {
                    if stack.pop() != Some(']') {
                        return Err(BlissError::Internal("unmatched ~]".into()));
                    }
                }
                '(' => stack.push(')'),
                ')' => {
                    if stack.pop() != Some(')') {
                        return Err(BlissError::Internal("unmatched ~)".into()));
                    }
                }
                '<' => stack.push('>'),
                '>' => {
                    if stack.pop() != Some('>') {
                        return Err(BlissError::Internal("unmatched ~>".into()));
                    }
                }
                '/' => {
                    i += 1;
                    while i < chars.len() && chars[i] != '/' {
                        i += 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    if !stack.is_empty() {
        let unmatched = match stack.last().unwrap() {
            '}' => "~{",
            ']' => "~[",
            ')' => "~(",
            '>' => "~<",
            _ => "~?",
        };
        return Err(BlissError::Internal(format!("unmatched {}", unmatched)));
    }
    Ok(())
}

fn format_impl(
    control: &str,
    args: &[BlissVal],
    arg_idx: &mut usize,
    output: &mut String,
) -> Result<(), BlissError> {
    let chars: Vec<char> = control.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            output.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1; // skip ~
        if i >= chars.len() {
            return Err(BlissError::Internal("dangling ~ in format string".into()));
        }
        // Parse params
        let mut params: Vec<Param> = Vec::new();
        loop {
            if i >= chars.len() {
                return Err(BlissError::Internal("dangling ~".into()));
            }
            let c = chars[i];
            if c == 'v' || c == 'V' {
                params.push(Param::V);
                i += 1;
                if i < chars.len() && chars[i] == ',' {
                    i += 1;
                }
            } else if c == '#' && (i + 1 >= chars.len() || chars[i + 1] != '\\') {
                params.push(Param::Hash);
                i += 1;
                if i < chars.len() && chars[i] == ',' {
                    i += 1;
                }
            } else if c == '\'' {
                i += 1;
                if i < chars.len() {
                    params.push(Param::Num(chars[i] as i64));
                    i += 1;
                }
                if i < chars.len() && chars[i] == ',' {
                    i += 1;
                }
            } else if c.is_ascii_digit()
                || ((c == '-' || c == '+') && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
            {
                let mut num_str = String::new();
                if c == '-' || c == '+' {
                    num_str.push(c);
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    num_str.push(chars[i]);
                    i += 1;
                }
                params.push(Param::Num(num_str.parse::<i64>().unwrap_or(0)));
                if i < chars.len() && chars[i] == ',' {
                    i += 1;
                }
            } else if c == ',' {
                params.push(Param::None);
                i += 1;
            } else {
                break;
            }
        }
        let mut colon = false;
        let mut at_sign = false;
        while i < chars.len() {
            if chars[i] == ':' {
                colon = true;
                i += 1;
            } else if chars[i] == '@' {
                at_sign = true;
                i += 1;
            } else {
                break;
            }
        }
        if i >= chars.len() {
            return Err(BlissError::Internal("dangling ~".into()));
        }
        let directive = chars[i].to_ascii_uppercase();
        i += 1;

        let remaining = args.len().saturating_sub(*arg_idx);

        let resolve_param =
            |p: &Param, default: i64, aidx: &mut usize| -> Result<i64, BlissError> {
                match p {
                    Param::Num(n) => Ok(*n),
                    Param::V => {
                        if *aidx >= args.len() {
                            return Err(BlissError::ControlError("too few args for V param".into()));
                        }
                        let v = args[*aidx];
                        *aidx += 1;
                        if v.is_nil() {
                            Ok(default)
                        } else if v.is_fixnum() {
                            Ok(v.as_fixnum())
                        } else if v.is_character() {
                            // A `v` parameter standing in for a padchar (e.g.
                            // `~v,vd` with #\0) is a character; hand back its code
                            // point, which the padchar sites turn back into a char.
                            Ok(v.as_char() as i64)
                        } else {
                            Err(BlissError::TypeError {
                                datum: v,
                                expected: "integer".into(),
                            })
                        }
                    }
                    Param::Hash => Ok(remaining as i64),
                    Param::None => Ok(default),
                }
            };

        match directive {
            'A' => {
                // ~mincol,colinc,minpad,padchar A. Resolve params in order BEFORE
                // consuming the main arg so any v/# params consume the right args;
                // honour minpad and padchar (padchar was ignored — bliss-znib).
                let mincol = params
                    .first()
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let _colinc = params.get(1).map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?;
                let minpad = params
                    .get(2)
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let padchar = match params.get(3) {
                    Some(p) => char::from_u32(resolve_param(p, ' ' as i64, arg_idx)? as u32)
                        .unwrap_or(' '),
                    None => ' ',
                };
                if *arg_idx >= args.len() {
                    if std::env::var_os("BLISS_FMT_DBG").is_some() {
                        eprintln!(
                            ";; fmt too-few ~~A: arg_idx={} nargs={} control={:?}",
                            arg_idx,
                            args.len(),
                            control.chars().take(80).collect::<String>()
                        );
                    }
                    return Err(BlissError::ControlError("too few args for ~A".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let s = if colon && val.is_nil() {
                    "()".into()
                } else {
                    blissval_to_print_string(val, false)
                };
                pad_format_field(output, &s, mincol, minpad, padchar, at_sign);
            }
            'S' => {
                // ~mincol,colinc,minpad,padchar S — same padding as ~A (bliss-znib).
                let mincol = params
                    .first()
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let _colinc = params.get(1).map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?;
                let minpad = params
                    .get(2)
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let padchar = match params.get(3) {
                    Some(p) => char::from_u32(resolve_param(p, ' ' as i64, arg_idx)? as u32)
                        .unwrap_or(' '),
                    None => ' ',
                };
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~S".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let s = if colon && val.is_nil() {
                    "()".into()
                } else {
                    blissval_to_print_string(val, true)
                };
                pad_format_field(output, &s, mincol, minpad, padchar, at_sign);
            }
            'D' | 'B' | 'O' | 'X' => {
                let radix = match directive {
                    'B' => 2,
                    'O' => 8,
                    'X' => 16,
                    _ => 10,
                };
                // Resolve V/# params BEFORE consuming the main argument
                let mincol = if !params.is_empty() {
                    resolve_param(&params[0], 0, arg_idx)? as usize
                } else {
                    0
                };
                let padchar = if params.len() > 1 {
                    resolve_param(&params[1], ' ' as i64, arg_idx)? as u8 as char
                } else {
                    ' '
                };
                // Third/fourth ~D params are the comma character and comma
                // interval (CLHS 22.3.2.1); `~,,' ,4:d` groups every 4 digits
                // with a space. Previously both were ignored (hardcoded ",", 3).
                let commachar = if params.len() > 2 {
                    resolve_param(&params[2], ',' as i64, arg_idx)? as u8 as char
                } else {
                    ','
                };
                let comma_interval = if params.len() > 3 {
                    resolve_param(&params[3], 3, arg_idx)? as usize
                } else {
                    3
                };
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError(format!(
                        "too few args for ~{}",
                        directive
                    )));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                // Accept fixnums AND bignums (previously bignums type-errored,
                // even though ~A/PRINT render them). integer_magnitude returns
                // None for non-integers -> TYPE-ERROR.
                let (negative, digits) = match integer_magnitude(val, radix) {
                    Some(parts) => parts,
                    None => {
                        return Err(BlissError::TypeError {
                            datum: val,
                            expected: "integer".into(),
                        });
                    }
                };
                output.push_str(&format_integer_parts(
                    negative,
                    &digits,
                    radix,
                    colon,
                    at_sign,
                    mincol,
                    padchar,
                    commachar,
                    comma_interval,
                ));
            }
            'R' => {
                // Resolve V/# params BEFORE consuming the main argument
                let radix_param = if !params.is_empty() {
                    Some(resolve_param(&params[0], 10, arg_idx)? as u32)
                } else {
                    None
                };
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~R".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                if !val.is_fixnum() {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "integer".into(),
                    });
                }
                let n = val.as_fixnum();
                if let Some(radix) = radix_param {
                    output.push_str(&format_integer(n, radix, colon, at_sign, 0, ' ', ',', 3));
                } else if colon && at_sign {
                    output.push_str(&to_roman(n, true));
                } else if at_sign {
                    output.push_str(&to_roman(n, false));
                } else if colon {
                    output.push_str(&ordinal(n));
                } else {
                    output.push_str(&cardinal(n));
                }
            }
            'F' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~F".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let f = if val.is_single_float() {
                    val.as_single_float() as f64
                } else if val.is_double_float() {
                    val.as_double_float()
                } else if val.is_fixnum() {
                    val.as_fixnum() as f64
                } else {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "number".into(),
                    });
                };
                // ~w,dF: d = digits after the decimal point, w = minimum width.
                let d = if params.len() > 1 {
                    let dd = resolve_param(&params[1], -1, arg_idx)?;
                    if dd >= 0 { Some(dd as usize) } else { None }
                } else {
                    None
                };
                let shortest = if val.is_single_float() {
                    format!("{}", val.as_single_float())
                } else {
                    format!("{f}")
                };
                // With an explicit digit count there are two regimes, and CL
                // uses both (verified against SBCL):
                //
                //  * asking for MORE fraction digits than the shortest decimal
                //    carries is just padding — the extra digits are not
                //    information the float has. ~,3F of 3.4028235e38 is
                //    ...350000000000000000000000000000000.000, NOT the exact
                //    binary value ...346638528859811704183484516925440.000.
                //  * asking for FEWER means rounding, and that rounds the
                //    float's EXACT value. f32 -> f64 is lossless and a binary
                //    fraction always terminates in decimal, so `{:.150}` is the
                //    exact expansion (150 places covers the smallest subnormal).
                //    ~,2F of 1.005 is 1.00 because that f32 is really
                //    1.00499999523162841796875 — rounding the shortest decimal
                //    "1.005" would wrongly give 1.01. Ties go away from zero
                //    (~,1F of 0.25 is 0.3); Rust's `{:.1}` rounds half to even
                //    and would give 0.2.
                let shortest_frac = shortest.split_once('.').map_or(0, |(_, fr)| fr.len());
                let mut s = match d {
                    Some(dd) if shortest_frac <= dd => round_decimal_string(&shortest, dd),
                    Some(dd) => round_decimal_string(&format!("{f:.150}"), dd),
                    None => {
                        // No fraction-digit count: the shortest decimal as-is.
                        // Going via the widened f64 exposed binary32 noise, so
                        // ~F of 1.0e-10 printed 0.0000000001000000013351432
                        // (the trap bliss-8zrb fixed for ~E/~G). Rust's `{}`
                        // never switches to an exponent, which is what ~F
                        // wants; CL still requires the point.
                        let mut t = shortest.clone();
                        if !t.contains('.') {
                            t.push_str(".0");
                        }
                        t
                    }
                };
                // ~@F prints a leading + on a non-negative value (CLHS 22.3.3.1).
                if at_sign && f >= 0.0 {
                    s.insert(0, '+');
                }
                let w = if !params.is_empty() {
                    resolve_param(&params[0], 0, arg_idx)? as usize
                } else {
                    0
                };
                if s.len() < w {
                    let pad: String = std::iter::repeat_n(' ', w - s.len()).collect();
                    s = format!("{pad}{s}");
                }
                output.push_str(&s);
            }
            'E' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~E".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let f = if val.is_single_float() {
                    val.as_single_float() as f64
                } else if val.is_double_float() {
                    val.as_double_float()
                } else if val.is_fixnum() {
                    val.as_fixnum() as f64
                } else {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "number".into(),
                    });
                };
                // ~w,d,e,k,overflowchar,padchar,exponentcharE. Resolve params in
                // order (a `v` param consumes the next arg, so order matters).
                let _w = params.first().map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let d = params.get(1).map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let e = params.get(2).map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let k = params.get(3).map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?;
                let _of = params.get(4).map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let _pad = params.get(5).map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let expc = params
                    .get(6)
                    .map_or(Ok('e' as i64), |p| resolve_param(p, 'e' as i64, arg_idx))?;
                let exp_char = char::from_u32(expc as u32).unwrap_or('e');
                if d < 0 {
                    // No explicit fraction-digit count: shortest round-trip. Format
                    // a single-float from the f32 itself rather than its widened f64,
                    // which exposed binary32 imprecision (bliss-8zrb).
                    let s = if val.is_single_float() {
                        format!("{:E}", val.as_single_float())
                    } else {
                        format!("{:E}", f)
                    };
                    output.push_str(&exp_with_decimal_point(&s, exp_char));
                } else {
                    output.push_str(&format_e_fixed(
                        f,
                        d as usize,
                        if e >= 0 { Some(e as usize) } else { None },
                        k,
                        exp_char,
                    ));
                }
            }
            'G' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~G".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                // Shortest round-trip from the f32 for single-floats (bliss-8zrb).
                let s = if val.is_single_float() {
                    format!("{}", val.as_single_float())
                } else if val.is_double_float() {
                    format!("{}", val.as_double_float())
                } else if val.is_fixnum() {
                    format!("{}", val.as_fixnum() as f64)
                } else {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "number".into(),
                    });
                };
                output.push_str(&s);
            }
            '$' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~$".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let f = if val.is_single_float() {
                    val.as_single_float() as f64
                } else if val.is_double_float() {
                    val.as_double_float()
                } else if val.is_fixnum() {
                    val.as_fixnum() as f64
                } else {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "number".into(),
                    });
                };
                // ~d,n,w,padchar$: d = digits after the point (default 2), n = min
                // digits before it (default 1, zero-padded), w = min field width.
                let d = params
                    .first()
                    .map_or(Ok(2), |p| resolve_param(p, 2, arg_idx))?
                    .max(0) as usize;
                let n = params
                    .get(1)
                    .map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?
                    .max(1) as usize;
                let w = params
                    .get(2)
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let padchar = params.get(3).map_or(Ok(' '), |p| {
                    resolve_param(p, ' ' as i64, arg_idx)
                        .map(|c| char::from_u32(c as u32).unwrap_or(' '))
                })?;
                let body = format!("{:.*}", d, f.abs());
                let (int_part, frac_part) = match body.split_once('.') {
                    Some((i, fr)) => (i.to_string(), format!(".{fr}")),
                    None => (body.clone(), String::new()),
                };
                let int_padded = if int_part.len() < n {
                    format!("{}{int_part}", "0".repeat(n - int_part.len()))
                } else {
                    int_part
                };
                let sign = if f < 0.0 {
                    "-"
                } else if at_sign {
                    "+"
                } else {
                    ""
                };
                let num = format!("{sign}{int_padded}{frac_part}");
                let count = num.chars().count();
                if count < w {
                    // Default: right-justify (pad on the left). With `:`, the sign is
                    // printed first, then padding, then the digits.
                    let pad: String = std::iter::repeat_n(padchar, w - count).collect();
                    if colon && !sign.is_empty() {
                        output.push_str(sign);
                        output.push_str(&pad);
                        output.push_str(&num[sign.len()..]);
                    } else {
                        output.push_str(&pad);
                        output.push_str(&num);
                    }
                } else {
                    output.push_str(&num);
                }
            }
            '%' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.push('\n');
                }
            }
            '&' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                // Fresh line: emit newline only if not at start of line
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                for _ in 1..count {
                    output.push('\n');
                }
            }
            '|' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.push('\x0c');
                }
            }
            // ~I (indent) and ~_ (conditional/fill newline) are pretty-printer
            // hints; this linear (non-pretty) printer ignores them. Any numeric
            // parameter (e.g. ~3i) and the :/@ modifiers are already parsed and
            // harmlessly discarded. ASDF/UIOP use these inside ~<…~:> blocks.
            'I' | '_' => {}
            '~' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.push('~');
                }
            }
            'T' => {
                // ~colnum,colinc T  — absolute: move to column `colnum`, or if
                //                    already at/past it, to the next
                //                    `colnum + k*colinc`.
                // ~colrel,colinc @T — relative: emit `colrel` spaces, then the
                //                    fewest more that land on a multiple of
                //                    `colinc` (CLHS 22.3.6.1). The `@` form was
                //                    previously falling through to the absolute
                //                    computation, so `ab~3@Tx` emitted one space
                //                    instead of three.
                let first = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?.max(0) as usize
                } else {
                    1
                };
                let colinc = if params.len() > 1 {
                    resolve_param(&params[1], 1, arg_idx)?.max(0) as usize
                } else {
                    1
                };
                // Column is a CHARACTER count, not a byte count — the byte
                // length overshoots on any multibyte output already on the line.
                let cur_col = match output.rfind('\n') {
                    Some(p) => output[p + 1..].chars().count(),
                    None => output.chars().count(),
                };
                let spaces = if at_sign {
                    let target = cur_col + first;
                    let extra = if colinc > 0 && target % colinc != 0 {
                        colinc - (target % colinc)
                    } else {
                        0
                    };
                    first + extra
                } else if cur_col < first {
                    first - cur_col
                } else if colinc > 0 {
                    colinc - ((cur_col - first) % colinc)
                } else {
                    0
                };
                for _ in 0..spaces {
                    output.push(' ');
                }
            }
            '*' => {
                let n = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)? as usize
                } else {
                    1
                };
                if at_sign {
                    *arg_idx = n;
                } else if colon {
                    if *arg_idx >= n {
                        *arg_idx -= n;
                    } else {
                        *arg_idx = 0;
                    }
                } else {
                    *arg_idx += n;
                }
            }
            'C' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~C".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                if !val.is_character() {
                    return Err(BlissError::TypeError {
                        datum: val,
                        expected: "character".into(),
                    });
                }
                let c = val.as_char();
                if at_sign {
                    output.push_str(&format!("#\\{}", c));
                } else if colon {
                    output.push_str(&char_name(c));
                } else {
                    output.push(c);
                }
            }
            'W' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~W".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                output.push_str(&blissval_to_print_string(val, true));
            }
            '?' => {
                if *arg_idx >= args.len() {
                    return Err(BlissError::ControlError("too few args for ~?".into()));
                }
                let ctrl_val = args[*arg_idx];
                *arg_idx += 1;
                // Control string must be a string
                if !ctrl_val.is_heap_object() || !bliss_rt::types::stringp(ctrl_val) {
                    return Err(BlissError::TypeError {
                        datum: ctrl_val,
                        expected: "string".into(),
                    });
                }
                let sub_control = extract_bliss_string(ctrl_val).ok_or_else(|| {
                    BlissError::Internal("failed to extract format string".into())
                })?;
                if at_sign {
                    // ~@? — use the enclosing argument list from current position
                    format_impl(&sub_control, args, arg_idx, output)?;
                } else {
                    // ~? — consume a separate list argument for the sub-format's args
                    if *arg_idx >= args.len() {
                        return Err(BlissError::ControlError("too few args for ~?".into()));
                    }
                    let args_val = args[*arg_idx];
                    *arg_idx += 1;
                    let sub_args = if args_val.is_nil() {
                        Vec::new()
                    } else {
                        cons_list_to_vec(args_val)
                    };
                    let mut sub_idx = 0;
                    format_impl(&sub_control, &sub_args, &mut sub_idx, output)?;
                }
            }
            'P' => {
                // Per CL spec §22.3.8.3: ~:P backs up one argument
                // (does ~:* first) then checks if the value equals 1.
                // Plain ~P consumes the next argument directly.
                if colon {
                    // ~:P — explicitly back up one argument position.
                    if *arg_idx > 0 {
                        *arg_idx -= 1;
                    }
                }
                if *arg_idx >= args.len() {
                    // No more arguments available.  If this is plain ~P
                    // following a directive that consumed the last arg
                    // (e.g. "~D item~P" with one arg), re-examine the
                    // previous argument as a compatibility fallback.
                    if !colon && *arg_idx > 0 {
                        *arg_idx -= 1;
                    } else {
                        return Err(BlissError::ControlError("too few args for ~P".into()));
                    }
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let is_one = val.is_fixnum() && val.as_fixnum() == 1;
                if at_sign {
                    output.push_str(if is_one { "y" } else { "ies" });
                } else {
                    if !is_one {
                        output.push('s');
                    }
                }
            }
            '^' => {
                // ~^ up-and-out: in iteration context, terminates if no more args
                if *arg_idx >= args.len() {
                    return Ok(());
                }
            }
            '{' => {
                // Find matching ~}
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '{')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                // The prefix parameter of ~{ is the maximum iteration count:
                // `~2{...~}` runs the body at most twice (CLHS 22.3.5.2).
                // Resolved before the list argument is consumed (a `~V{` count
                // comes from an argument). None = unlimited.
                let max_iter: Option<i64> = if !params.is_empty() {
                    Some(resolve_param(&params[0], 0, arg_idx)?)
                } else {
                    None
                };
                let reached = |n: i64| max_iter.is_some_and(|m| n >= m);
                if at_sign && colon {
                    // ~:@{...~} — each remaining arg is itself a list (cons cell)
                    let mut iters = 0i64;
                    while *arg_idx < args.len() && !reached(iters) {
                        let sub = args[*arg_idx];
                        *arg_idx += 1;
                        iters += 1;
                        if sub.is_nil() {
                            continue;
                        } // empty sublist
                        let sub_args = cons_list_to_vec(sub);
                        let mut sub_idx = 0;
                        format_impl(&body, &sub_args, &mut sub_idx, output)?;
                    }
                } else if at_sign {
                    // ~@{...~} — remaining args form the iteration list
                    let mut iters = 0i64;
                    while *arg_idx < args.len() && !reached(iters) {
                        format_impl(&body, args, arg_idx, output)?;
                        iters += 1;
                    }
                } else if colon {
                    // ~:{...~} — arg is a list of sublists; apply body to each sublist
                    if *arg_idx >= args.len() {
                        return Err(BlissError::ControlError("too few args for ~:{".into()));
                    }
                    let list_val = args[*arg_idx];
                    *arg_idx += 1;
                    if !list_val.is_nil() {
                        let sublists = cons_list_to_vec(list_val);
                        for (iters, sublist) in sublists.iter().enumerate() {
                            if reached(iters as i64) {
                                break;
                            }
                            let sub_args = if sublist.is_nil() {
                                Vec::new()
                            } else {
                                cons_list_to_vec(*sublist)
                            };
                            let mut sub_idx = 0;
                            format_impl(&body, &sub_args, &mut sub_idx, output)?;
                        }
                    }
                } else {
                    // ~{...~} — arg is a list; iterate body over list elements
                    if *arg_idx >= args.len() {
                        return Err(BlissError::ControlError("too few args for ~{".into()));
                    }
                    let list_val = args[*arg_idx];
                    *arg_idx += 1;
                    if !list_val.is_nil() {
                        let list_elements = cons_list_to_vec(list_val);
                        let mut sub_idx = 0;
                        let mut iters = 0i64;
                        while sub_idx < list_elements.len() && !reached(iters) {
                            format_impl(&body, &list_elements, &mut sub_idx, output)?;
                            iters += 1;
                        }
                    }
                }
            }
            '}' => {
                return Err(BlissError::Internal("unmatched ~}".into()));
            }
            '[' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '[')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                // Parse clauses separated by ~;
                let clauses = split_clauses(&body);
                if colon {
                    // ~:[false~;true~] boolean conditional
                    if *arg_idx >= args.len() {
                        return Err(BlissError::ControlError("too few args for ~:[".into()));
                    }
                    let val = args[*arg_idx];
                    *arg_idx += 1;
                    let idx = if val.is_nil() { 0 } else { 1 };
                    if idx < clauses.len() {
                        format_impl(&clauses[idx], args, arg_idx, output)?;
                    }
                } else if at_sign {
                    // ~@[clause~] true-test
                    if *arg_idx >= args.len() {
                        return Err(BlissError::ControlError("too few args for ~@[".into()));
                    }
                    let val = args[*arg_idx];
                    if !val.is_nil() {
                        // Don't consume arg - it remains for use inside clause
                        if !clauses.is_empty() {
                            format_impl(&clauses[0], args, arg_idx, output)?;
                        }
                    } else {
                        *arg_idx += 1; // consume the nil
                    }
                } else {
                    // Numeric conditional. An explicit prefix parameter (~n[ or
                    // ~#[) supplies the selector directly WITHOUT consuming an
                    // argument — `#` resolves to the number of remaining args, so
                    // `~#[none~;one~:;many~]` dispatches on the arg count (CLHS
                    // 22.3.7.2). Otherwise the next argument is the selector, which
                    // must be an integer (bliss-qzxu).
                    let idx_i64 = if params.first().is_some_and(|p| !matches!(p, Param::None)) {
                        resolve_param(&params[0], 0, arg_idx)?
                    } else {
                        if *arg_idx >= args.len() {
                            return Err(BlissError::ControlError("too few args for ~[".into()));
                        }
                        let val = args[*arg_idx];
                        *arg_idx += 1;
                        if !val.is_fixnum() {
                            return Err(BlissError::TypeError {
                                datum: val,
                                expected: "integer".into(),
                            });
                        }
                        val.as_fixnum()
                    };
                    // A negative selector wraps to a huge usize -> out of range ->
                    // the ~:; default clause (if any), matching an unmatched index.
                    let idx = idx_i64 as usize;
                    // An in-range index selects that clause; otherwise fall back
                    // to the `~:;` default clause if the body has one (bliss-mrmv).
                    let chosen = if idx < clauses.len() {
                        Some(idx)
                    } else {
                        conditional_default_index(&body)
                    };
                    if let Some(c) = chosen {
                        format_impl(&clauses[c], args, arg_idx, output)?;
                    }
                }
            }
            ']' => {
                return Err(BlissError::Internal("unmatched ~]".into()));
            }
            '(' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '(')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                let mut inner = String::new();
                format_impl(&body, args, arg_idx, &mut inner)?;
                if colon && at_sign {
                    output.push_str(&inner.to_uppercase());
                } else if colon {
                    output.push_str(&capitalize_words(&inner));
                } else if at_sign {
                    output.push_str(&capitalize_first(&inner));
                } else {
                    output.push_str(&inner.to_lowercase());
                }
            }
            ')' => {
                return Err(BlissError::Internal("unmatched ~)".into()));
            }
            '<' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '<')?;
                let body: String = chars[body_start..body_end].iter().collect();
                // CLHS 22.3.6.2: it is the COLON ON THE CLOSING directive that
                // turns ~<...~> into a pretty-printing logical block. A colon on
                // the OPENING directive (~:<) is still justification — it asks
                // for padding before the first segment. ASDF's `~@<...~@:>`
                // blocks land here as logical blocks, which is what they are.
                let close_is_colon = {
                    let mut k = body_end + 1;
                    let mut found = false;
                    while k < chars.len() && chars[k] != '>' {
                        if chars[k] == ':' {
                            found = true;
                        }
                        k += 1;
                    }
                    found
                };
                i = skip_close_directive(&chars, body_end);
                if close_is_colon {
                    // Logical block: emit the segments with no justification
                    // padding. (Splitting rather than formatting the raw body
                    // keeps a `~;` inside the block from reaching the main
                    // dispatch loop as an unknown directive.)
                    for clause in &split_clauses(&body) {
                        format_impl(clause, args, arg_idx, output)?;
                    }
                } else {
                    // Justification: ~mincol,colinc,minpad,padchar<...~>.
                    let mincol = params
                        .first()
                        .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                        .max(0) as usize;
                    let colinc = params
                        .get(1)
                        .map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?
                        .max(1) as usize;
                    let minpad = params
                        .get(2)
                        .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                        .max(0) as usize;
                    let padchar = params.get(3).map_or(Ok(' '), |p| {
                        resolve_param(p, ' ' as i64, arg_idx)
                            .map(|c| char::from_u32(c as u32).unwrap_or(' '))
                    })?;
                    let clauses = split_clauses(&body);
                    let mut parts = Vec::new();
                    for clause in &clauses {
                        let mut part = String::new();
                        format_impl(clause, args, arg_idx, &mut part)?;
                        parts.push(part);
                    }
                    let total_len: usize = parts.iter().map(|p| p.chars().count()).sum();

                    // Padding goes into the gaps BETWEEN segments; `:` adds a gap
                    // before the first segment and `@` one after the last. A lone
                    // segment with neither modifier would have no gap at all, so
                    // CLHS gives it the leading one — that is what makes plain
                    // ~mincol<text~> right-justify.
                    let pad_before = colon || (parts.len() == 1 && !at_sign);
                    let pad_after = at_sign;
                    let gaps = parts.len().saturating_sub(1)
                        + usize::from(pad_before)
                        + usize::from(pad_after);

                    // Field width is mincol, grown by whole multiples of colinc
                    // until the segments and their minimum padding fit.
                    let needed = total_len + gaps * minpad;
                    let width = if needed <= mincol {
                        mincol
                    } else {
                        mincol + (needed - mincol).div_ceil(colinc) * colinc
                    };

                    // Spread the slack evenly; the remainder favours the LATER
                    // gaps (verified against SBCL: ~10:@<abc~> is "   abc    ").
                    let extra = width - total_len - gaps * minpad;
                    let per_gap = if gaps > 0 { extra / gaps } else { 0 };
                    let rem = if gaps > 0 { extra % gaps } else { 0 };
                    let gap_width = |g: usize| -> usize {
                        minpad + per_gap + usize::from(g + rem >= gaps)
                    };

                    let mut g = 0usize;
                    if pad_before {
                        output.extend(std::iter::repeat_n(padchar, gap_width(g)));
                        g += 1;
                    }
                    for (j, part) in parts.iter().enumerate() {
                        output.push_str(part);
                        if j + 1 < parts.len() {
                            output.extend(std::iter::repeat_n(padchar, gap_width(g)));
                            g += 1;
                        }
                    }
                    if pad_after {
                        output.extend(std::iter::repeat_n(padchar, gap_width(g)));
                    }
                }
            }
            '>' => {
                return Err(BlissError::Internal("unmatched ~>".into()));
            }
            '/' => {
                // ~/name/ — user dispatch function. Consume one argument.
                let name_start = i;
                while i < chars.len() && chars[i] != '/' {
                    i += 1;
                }
                let name: String = chars[name_start..i].iter().collect();
                if i < chars.len() {
                    i += 1;
                } // skip closing /
                // Consume one argument as per CL spec
                if *arg_idx >= args.len() {
                    return Err(BlissError::Internal(format!(
                        "too few args for ~/{}/",
                        name
                    )));
                }
                let arg = args[*arg_idx];
                *arg_idx += 1;
                // Look up the registered format function
                if let Some(func) = lookup_format_function(&name) {
                    // Call the registered format function with the CL-specified
                    // arguments: (stream, arg, colon-p, at-sign-p, &rest params).
                    // We create a string-output-stream proxy for the output
                    // buffer, and pass colon/at_sign as T or NIL.
                    let colon_val = if colon { T } else { NIL };
                    let at_val = if at_sign { T } else { NIL };
                    // Build argument list: stream (NIL = string accumulator),
                    // arg, colon-p, at-sign-p, then any prefix parameters.
                    let mut call_args: Vec<BlissVal> = Vec::with_capacity(4 + params.len());
                    call_args.push(NIL); // stream placeholder (output goes to buffer)
                    call_args.push(arg);
                    call_args.push(colon_val);
                    call_args.push(at_val);
                    for p in &params {
                        match p {
                            Param::Num(n) => call_args.push(BlissVal::from_fixnum(*n)),
                            _ => call_args.push(NIL),
                        }
                    }
                    // Invoke the function. For compiled functions with an
                    // entry point we call directly; otherwise fall back to
                    // the aesthetic representation of the argument.
                    let result = call_format_dispatch(func, &call_args);
                    match result {
                        Ok(val) => {
                            // If the function returned a string, append it.
                            if let Some(s) = extract_bliss_string(val) {
                                output.push_str(&s);
                            }
                            // Otherwise the function wrote to the stream
                            // directly (which we don't capture yet), so
                            // fall back to aesthetic printing.
                            else if !val.is_nil() {
                                output.push_str(&blissval_to_print_string(val, false));
                            }
                        }
                        Err(_) => {
                            // Function call failed; fall back to aesthetic
                            // representation so FORMAT itself doesn't crash.
                            output.push_str(&blissval_to_print_string(arg, false));
                        }
                    }
                } else {
                    // No registered function — return an error with the function name
                    return Err(BlissError::UndefinedFunction(make_bliss_string(&name)));
                }
            }
            '\n' => {
                // ~\n — ignored newline (with optional whitespace eating)
                if !at_sign {
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                }
            }
            _ => {
                return Err(BlissError::Internal(format!(
                    "unknown format directive ~{}",
                    directive
                )));
            }
        }
    }
    Ok(())
}

/// Skip past a closing directive like ~}, ~], ~), ~>, ~:>, etc.
/// `pos` points to the `~`. Returns position after the directive char.
fn skip_close_directive(chars: &[char], pos: usize) -> usize {
    let mut j = pos + 1; // skip ~
    while j < chars.len()
        && (chars[j] == ':'
            || chars[j] == '@'
            || chars[j].is_ascii_digit()
            || chars[j] == ','
            || chars[j] == '\''
            || chars[j] == 'v'
            || chars[j] == 'V'
            || chars[j] == '#'
            || chars[j] == '-'
            || chars[j] == '+')
    {
        if chars[j] == '\'' && j + 1 < chars.len() {
            j += 1;
        }
        j += 1;
    }
    if j < chars.len() { j + 1 } else { j }
}

/// Find matching close bracket. `start` is first char of body (after opening bracket).
/// Returns position of `~` in the closing `~close` directive.
fn find_matching_close(chars: &[char], start: usize, open: char) -> Result<usize, BlissError> {
    let close = match open {
        '{' => '}',
        '[' => ']',
        '(' => ')',
        '<' => '>',
        _ => open,
    };
    let mut depth = 1;
    let mut j = start;
    while j < chars.len() {
        if chars[j] == '~' {
            let tilde_at = j;
            j += 1;
            // Skip params and modifiers
            while j < chars.len()
                && (chars[j].is_ascii_digit()
                    || chars[j] == ','
                    || chars[j] == '\''
                    || chars[j] == 'v'
                    || chars[j] == 'V'
                    || chars[j] == '#'
                    || chars[j] == ':'
                    || chars[j] == '@'
                    || chars[j] == '-'
                    || chars[j] == '+')
            {
                if chars[j] == '\'' && j + 1 < chars.len() {
                    j += 1;
                }
                j += 1;
            }
            if j < chars.len() {
                if chars[j] == open {
                    depth += 1;
                } else if chars[j] == close {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(tilde_at);
                    }
                }
            }
        }
        j += 1;
    }
    Err(BlissError::Internal(format!("unmatched ~{}", open)))
}

/// For a numeric `~[` body, return the index of the clause introduced by a
/// depth-0 `~:;` separator — the default/else clause used when the selector is
/// out of range. `None` if there is no `~:;`. `split_clauses` already splits the
/// body at every `~;`/`~:;`; this only identifies WHICH clause is the default,
/// which `split_clauses` discards (bliss-mrmv).
fn conditional_default_index(body: &str) -> Option<usize> {
    let chars: Vec<char> = body.chars().collect();
    let mut depth = 0i32;
    let mut clause_idx = 0usize; // index of the clause AFTER the separators seen so far
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            i += 1;
            let mut had_colon = false;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || matches!(
                        chars[i],
                        ',' | '\'' | 'v' | 'V' | '#' | ':' | '@' | '-' | '+'
                    ))
            {
                // A quoted char param (`~'x`) — skip the quote and its char so a
                // quoted ':' is not mistaken for the colon modifier.
                if chars[i] == '\'' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == ':' {
                    had_colon = true;
                }
                i += 1;
            }
            if i < chars.len() {
                match chars[i] {
                    '{' | '[' | '(' | '<' => depth += 1,
                    '}' | ']' | ')' | '>' => depth -= 1,
                    ';' if depth == 0 => {
                        clause_idx += 1;
                        if had_colon {
                            return Some(clause_idx);
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    None
}

fn split_clauses(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == ','
                    || chars[i] == '\''
                    || chars[i] == 'v'
                    || chars[i] == 'V'
                    || chars[i] == '#'
                    || chars[i] == ':'
                    || chars[i] == '@'
                    || chars[i] == '-'
                    || chars[i] == '+')
            {
                if chars[i] == '\'' && i + 1 < chars.len() {
                    i += 1;
                }
                i += 1;
            }
            if i < chars.len() {
                match chars[i] {
                    '{' | '[' | '(' | '<' => depth += 1,
                    '}' | ']' | ')' | '>' => depth -= 1,
                    ';' if depth == 0 => {
                        clauses.push(current.clone());
                        current.clear();
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            let chunk: String = chars[start..=i.min(chars.len() - 1)].iter().collect();
            current.push_str(&chunk);
            i += 1;
        } else {
            current.push(chars[i]);
            i += 1;
        }
    }
    clauses.push(current);
    clauses
}

fn capitalize_words(s: &str) -> String {
    let mut result = String::new();
    let mut cap_next = true;
    for c in s.chars() {
        if c.is_whitespace() || !c.is_alphanumeric() {
            result.push(c);
            cap_next = true;
        } else if cap_next {
            result.push(c.to_uppercase().next().unwrap());
            cap_next = false;
        } else {
            result.push(c.to_lowercase().next().unwrap());
        }
    }
    result
}

fn capitalize_first(s: &str) -> String {
    let mut result = String::new();
    let mut done = false;
    for c in s.chars() {
        if !done && c.is_alphabetic() {
            result.push(c.to_uppercase().next().unwrap());
            done = true;
        } else if done {
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

// ── formatter ─────────────────────────────────────────────────────

/// Compile a FORMAT control string for repeated use.
/// Returns a closure (function-tagged heap object) that, when called with
/// a stream and arguments, performs the formatting.
pub fn formatter(control_string: &str) -> Result<BlissVal, BlissError> {
    // Validate the control string
    validate_matching(control_string)?;
    // Allocate a ClosureData that captures the control string.
    // The closure's function field points to the control string as a BlissVal.
    // When invoked, the runtime should extract the control string and call format().
    let ctrl_str = make_bliss_string(control_string);
    let total = std::mem::size_of::<bliss_rt::object::ClosureData>() + 8; // one captured var
    let size_units = total.div_ceil(8) as u16;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let header = ObjectHeader::new(type_id::CLOSURE, size_units);
        let closure = ptr as *mut bliss_rt::object::ClosureData;
        (*closure).header = header;
        (*closure).function = ctrl_str; // the captured control string
        // Store control string in closed_vars slot (offset after ClosureData)
        *(ptr.add(std::mem::size_of::<bliss_rt::object::ClosureData>()) as *mut BlissVal) =
            ctrl_str;
        // Return as function-tagged pointer so it's callable
        Ok(BlissVal::from_function_ptr(ptr))
    }
}

// ── Pretty-printer ─────────────────────────────────────────────────

/// Begin a logical block for pretty-printing (PPRINT-LOGICAL-BLOCK). R5.41.
pub fn pprint_logical_block(
    stream: BlissVal,
    list: BlissVal,
    prefix: Option<&str>,
    per_line_prefix: Option<&str>,
    suffix: Option<&str>,
    body: BlissVal,
) -> Result<(), BlissError> {
    // Build the output: per_line_prefix (or prefix) + body content + suffix
    let mut output = String::new();

    // Emit prefix or per-line-prefix
    if let Some(plp) = per_line_prefix {
        output.push_str(plp);
    } else if let Some(p) = prefix {
        output.push_str(p);
    }

    // Process the body: if body is a string, format it with the list as args
    if body.is_heap_object() && bliss_rt::types::stringp(body) {
        if let Some(body_str) = extract_bliss_string(body) {
            let list_elements = if list.is_nil() {
                Vec::new()
            } else {
                cons_list_to_vec(list)
            };
            let mut body_output = String::new();
            let mut idx = 0;
            format_impl(&body_str, &list_elements, &mut idx, &mut body_output)?;
            output.push_str(&body_output);
        }
    } else if !list.is_nil() {
        // If no body format string, print the list elements separated by spaces
        let elements = cons_list_to_vec(list);
        for (j, elem) in elements.iter().enumerate() {
            if j > 0 {
                output.push(' ');
            }
            output.push_str(&blissval_to_print_string(*elem, false));
        }
    }

    if let Some(s) = suffix {
        output.push_str(s);
    }

    if stream == T {
        print!("{}", output);
    } else if stream.is_heap_object() {
        // Write each character to the stream using the streams API.
        for ch in output.chars() {
            crate::streams::stream_write_char(stream, BlissVal::from_char(ch))?;
        }
    }
    Ok(())
}

/// Insert a conditional newline (PPRINT-NEWLINE). R5.41.
pub fn pprint_newline(kind: NewlineKind, stream: BlissVal) -> Result<(), BlissError> {
    // Without full XP line-width tracking, emit a newline for all kinds
    // so that pretty-printed output at least breaks at all marked points.
    let emit = match kind {
        NewlineKind::Mandatory | NewlineKind::Linear => true,
        NewlineKind::Fill | NewlineKind::Miser => true,
    };
    if emit {
        if stream == T {
            println!();
        } else if stream.is_heap_object() {
            crate::streams::stream_write_char(stream, BlissVal::from_char('\n'))?;
        }
    }
    Ok(())
}

/// Kind of pretty-printer newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewlineKind {
    Linear,
    Fill,
    Miser,
    Mandatory,
}

/// Thread-local indentation level for the pretty-printer.
/// Tracks the current indentation in columns; used by pprint_indent
/// and consumed when newlines are emitted.
use std::cell::Cell;
thread_local! {
    static PPRINT_INDENT_LEVEL: Cell<i32> = const { Cell::new(0) };
}

/// Adjust indentation (PPRINT-INDENT). R5.41.
pub fn pprint_indent(relative: bool, n: i32, _stream: BlissVal) -> Result<(), BlissError> {
    PPRINT_INDENT_LEVEL.with(|level| {
        if relative {
            level.set(level.get() + n);
        } else {
            level.set(n);
        }
        // Clamp to non-negative.
        if level.get() < 0 {
            level.set(0);
        }
    });
    Ok(())
}

/// Tab (PPRINT-TAB). R5.41.
///
/// Emits spaces to advance to a tab stop. For `:line` and `:section`
/// kinds, advance to column `colnum` (rounding up to the next multiple
/// of `colinc` if already past it).  For the `*-relative` variants,
/// emit at least `colnum` spaces, rounding up to `colinc` alignment.
pub fn pprint_tab(
    kind: TabKind,
    colnum: u32,
    colinc: u32,
    stream: BlissVal,
) -> Result<(), BlissError> {
    // Without full column tracking we approximate: emit `colnum` spaces
    // for absolute kinds and `colnum` spaces for relative kinds.
    let spaces = match kind {
        TabKind::Line | TabKind::Section => {
            // Emit enough spaces to reach `colnum`; since we don't
            // track the current column, emit `colnum` as a best
            // effort.  Round up to `colinc` if non-zero.
            if colinc > 0 {
                let rounded = colnum.div_ceil(colinc) * colinc;
                rounded as usize
            } else {
                colnum as usize
            }
        }
        TabKind::LineRelative | TabKind::SectionRelative => {
            // Emit at least `colnum` spaces, rounded up to `colinc`.
            let mut n = colnum as usize;
            if colinc > 0 && n % (colinc as usize) != 0 {
                n = n.div_ceil(colinc as usize) * colinc as usize;
            }
            n
        }
    };

    if spaces > 0 {
        if stream == T {
            print!("{}", " ".repeat(spaces));
        } else if stream.is_heap_object() {
            for _ in 0..spaces {
                crate::streams::stream_write_char(stream, BlissVal::from_char(' '))?;
            }
        }
    }
    Ok(())
}

/// Kind of tab for pprint-tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Line,
    Section,
    LineRelative,
    SectionRelative,
}

// ── Pprint dispatch ────────────────────────────────────────────────

/// Dispatch table entry.
#[allow(dead_code)]
struct DispatchEntry {
    _type_spec: BlissVal,
    function: BlissVal,
    priority: f64,
}

/// A simple pprint dispatch table stored as a heap object.
#[allow(dead_code)]
struct PprintDispatchTable {
    entries: Vec<DispatchEntry>,
}

// ── Format function dispatch (~/name/ directive) ─────────────────

/// Invoke a format dispatch function (registered via `register_format_function`).
/// The function is a BlissVal which may be a compiled function with an entry
/// point, an interpreted function, or a closure.  We attempt to call it with
/// the provided arguments; on any structural mismatch we return an error so
/// the caller can fall back gracefully.
fn call_format_dispatch(func: BlissVal, args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    use bliss_rt::object::{CompiledFunctionData, type_id};

    if !func.is_function() && !func.is_heap_object() {
        return Err(BlissError::Internal(
            "~/name/ function is not callable".into(),
        ));
    }

    // For compiled functions, we can read the entry point and call it directly.
    if func.is_function() || func.is_heap_object() {
        let ptr = if func.is_function() {
            // Unmask the tag to get the raw pointer.
            unsafe { func.as_ptr() }
        } else {
            unsafe { func.as_ptr() }
        };

        let header = unsafe { *(ptr as *const bliss_rt::object::ObjectHeader) };
        let tid = header.type_id();

        if tid == type_id::COMPILED_FUNCTION {
            let cf = unsafe { &*(ptr as *const CompiledFunctionData) };
            let entry = cf.entry_point;
            if !entry.is_null() {
                // Call compiled entry point as a Rust-ABI function that
                // takes a slice of BlissVal arguments and returns BlissVal.
                type EntryFn = fn(&[BlissVal]) -> BlissVal;
                let f: EntryFn = unsafe { std::mem::transmute(entry) };
                return Ok(f(args));
            }
        }

        // For interpreted functions / closures we cannot evaluate the body
        // without the full evaluator.  Return an error so the caller falls
        // back to the aesthetic representation.
        return Err(BlissError::Internal(
            "~/name/ function is interpreted/closure — direct call not yet supported".into(),
        ));
    }

    Err(BlissError::Internal(
        "~/name/ function is not callable".into(),
    ))
}

// ── User format function registry (~/ directive) ──────────────────
use std::collections::HashMap;

/// Registry for user-defined format functions used by the ~/name/ directive.
/// Maps function name (uppercase) to a BlissVal representing the function.
static FORMAT_FUNCTION_REGISTRY: OrderedMutex<Option<HashMap<String, BlissVal>>> =
    OrderedMutex::new(LockLevel::GcWorld, 14, "format function GC roots", None);

fn scan_format_global_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    let mut registry = FORMAT_FUNCTION_REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(registry) = registry.as_mut() {
        for function in registry.values_mut() {
            visit(function);
        }
    }
    let mut dispatch = DEFAULT_DISPATCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(dispatch) = dispatch.as_mut() {
        for (type_specifier, function, _) in dispatch {
            visit(type_specifier);
            visit(function);
        }
    }
}

fn install_format_global_root_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| bliss_rt::gc::register_root_scanner(scan_format_global_roots));
}

/// Register a user-defined format function for use with the ~/name/ directive.
pub fn register_format_function(name: &str, function: BlissVal) {
    install_format_global_root_scanner();
    let mut registry = FORMAT_FUNCTION_REGISTRY.lock().unwrap();
    if registry.is_none() {
        *registry = Some(HashMap::new());
    }
    registry
        .as_mut()
        .unwrap()
        .insert(name.to_uppercase(), function);
}

/// Look up a registered format function by name.
fn lookup_format_function(name: &str) -> Option<BlissVal> {
    let registry = FORMAT_FUNCTION_REGISTRY.lock().unwrap();
    registry
        .as_ref()
        .and_then(|r| r.get(&name.to_uppercase()).copied())
}

// Global default dispatch table
static DEFAULT_DISPATCH: OrderedMutex<Option<Vec<(BlissVal, BlissVal, f64)>>> =
    OrderedMutex::new(LockLevel::GcWorld, 15, "pprint dispatch GC roots", None);

/// NOTE: Leaked allocation — not GC-registered. See make_bliss_string note.
fn ensure_default_table() {
    install_format_global_root_scanner();
    let default_printer = make_bliss_string("default-printer");
    let mut table = DEFAULT_DISPATCH.lock().unwrap();
    if table.is_none() {
        // Default table with a catch-all entry
        *table = Some(vec![(NIL, default_printer, 0.0)]);
    }
}

/// Get the pprint dispatch function for a type.
pub fn pprint_dispatch(_object: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    ensure_default_table();
    let table = DEFAULT_DISPATCH.lock().unwrap();
    if let Some(entries) = table.as_ref() {
        if let Some(entry) = entries.last() {
            return Ok((entry.1, true));
        }
    }
    Ok((NIL, false))
}

/// Check if a BlissVal is a heap-encoded pprint dispatch table
/// (created by copy_pprint_dispatch).
fn is_dispatch_table(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    unsafe {
        let ptr = v.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        header.type_id() == type_id::SIMPLE_VECTOR
    }
}

/// Read entries from a heap-encoded dispatch table.
fn read_dispatch_table_entries(table_val: BlissVal) -> Vec<(BlissVal, BlissVal, f64)> {
    unsafe {
        let ptr = table_val.as_ptr();
        let entry_count = *(ptr.add(8) as *const u64) as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for idx in 0..entry_count {
            let base = ptr.add(16 + idx * 24);
            let ts = BlissVal(*(base as *const u64));
            let func = BlissVal(*((base as *const u64).add(1)));
            let prio = *((base as *const f64).add(2));
            entries.push((ts, func, prio));
        }
        entries
    }
}

/// Write entries back to a heap-encoded dispatch table, reallocating if needed.
fn write_dispatch_table_entries(table_val: BlissVal, entries: &[(BlissVal, BlissVal, f64)]) {
    let entry_count = entries.len();
    let total = 16 + entry_count * 24;
    let padded = (total + 7) & !7;
    unsafe {
        let old_ptr = table_val.as_ptr();
        let old_header = *(old_ptr as *const ObjectHeader);
        let old_padded = (old_header.size_units() as usize) * 8;
        let ptr = if padded <= old_padded {
            old_ptr
        } else {
            let old_layout = std::alloc::Layout::from_size_align(old_padded, 8).unwrap();
            let new_ptr = std::alloc::realloc(old_ptr, old_layout, padded);
            if new_ptr.is_null() {
                std::alloc::handle_alloc_error(old_layout);
            }
            new_ptr
        };
        let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, (padded / 8) as u16);
        *(ptr as *mut ObjectHeader) = header;
        *(ptr.add(8) as *mut u64) = entry_count as u64;
        for (idx, (ts, func, prio)) in entries.iter().enumerate() {
            let base = ptr.add(16 + idx * 24);
            *(base as *mut u64) = ts.0;
            *((base as *mut u64).add(1)) = func.0;
            *((base as *mut f64).add(2)) = *prio;
        }
    }
}

/// Set a pprint dispatch entry.
pub fn set_pprint_dispatch(
    type_specifier: BlissVal,
    function: Option<BlissVal>,
    priority: f64,
    table: BlissVal,
) -> Result<(), BlissError> {
    install_format_global_root_scanner();
    if !table.is_nil() && is_dispatch_table(table) {
        // Operate on the given heap-encoded dispatch table
        let mut entries = read_dispatch_table_entries(table);
        entries.retain(|e| e.0 != type_specifier || (e.2 - priority).abs() > f64::EPSILON);
        if let Some(func) = function {
            entries.push((type_specifier, func, priority));
            entries.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        }
        write_dispatch_table_entries(table, &entries);
    } else {
        // Operate on the global default dispatch table
        ensure_default_table();
        let mut guard = DEFAULT_DISPATCH.lock().unwrap();
        if let Some(entries) = guard.as_mut() {
            entries.retain(|e| e.0 != type_specifier || (e.2 - priority).abs() > f64::EPSILON);
            if let Some(func) = function {
                entries.push((type_specifier, func, priority));
                entries.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
            }
        }
    }
    Ok(())
}

/// Copy a pprint dispatch table.
/// NOTE: Leaked allocation — not GC-registered. See make_bliss_string note.
///
/// When `table` is `None`, copies the current global default dispatch
/// table.  When `Some(t)`, copies the given table `t`.  In either case
/// the returned table is an independent deep copy: subsequent
/// modifications via `set_pprint_dispatch` on one do not affect the
/// other.
pub fn copy_pprint_dispatch(table: Option<BlissVal>) -> Result<BlissVal, BlissError> {
    // Determine which entries to copy: from the given heap-encoded table
    // if provided, otherwise from the global default.
    let entries: Vec<(BlissVal, BlissVal, f64)> = match table {
        Some(table_val) if !table_val.is_nil() && is_dispatch_table(table_val) => {
            read_dispatch_table_entries(table_val)
        }
        _ => {
            ensure_default_table();
            let guard = DEFAULT_DISPATCH.lock().unwrap();
            guard.as_ref().cloned().unwrap_or_default()
        }
    };

    // Encode the entries into a heap object.  Layout:
    //   ObjectHeader (8 bytes)
    //   entry_count  (8 bytes)
    //   per entry:   type_spec (8) | function (8) | priority f64 (8) = 24 bytes
    let entry_count = entries.len();
    let total = 16 + entry_count * 24;
    let padded = (total + 7) & !7;
    let layout = std::alloc::Layout::from_size_align(padded, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, (padded / 8) as u16);
        *(ptr as *mut ObjectHeader) = header;
        *(ptr.add(8) as *mut u64) = entry_count as u64;
        for (i, (ts, func, prio)) in entries.iter().enumerate() {
            let base = ptr.add(16 + i * 24);
            *(base as *mut u64) = ts.0;
            *((base as *mut u64).add(1)) = func.0;
            *((base as *mut f64).add(2)) = *prio;
        }
        Ok(BlissVal::from_heap_ptr(ptr))
    }
}
