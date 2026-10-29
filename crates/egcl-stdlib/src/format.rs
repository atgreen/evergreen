// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FORMAT and pretty-printer.
//!
//! See spec §5.9.

use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{LockLevel, OrderedMutex};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{EgclVal, NIL, T, UNBOUND};

// ── User print-object hook ────────────────────────────────────────
//
// FORMAT's `~A`/`~S` must honour user-defined CLOS `print-object` methods, but
// method dispatch lives in the interpreter, which this crate cannot call
// directly. The interpreter installs a hook that, given an instance and the
// `*print-escape*` mode, returns the method's rendering (or None to fall back to
// the built-in `#<CLASS>` form). Mirrors the reader's hook pattern.
type PrintObjectHook = fn(EgclVal, bool) -> Option<String>;
static PRINT_OBJECT_HOOK: OrderedMutex<Option<PrintObjectHook>> =
    OrderedMutex::new(LockLevel::CodeCache, 30, "print-object hook", None);

pub fn set_print_object_hook(hook: Option<PrintObjectHook>) {
    *PRINT_OBJECT_HOOK.lock().unwrap() = hook;
}

/// The host condition system supplies PRINT-NOT-READABLE and propagates it at
/// the public printing boundary. No Lisp values are stored in this hook.
pub type PrintNotReadableHook = fn(EgclVal);
static PRINT_NOT_READABLE_HOOK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub fn set_print_not_readable_hook(hook: Option<PrintNotReadableHook>) {
    PRINT_NOT_READABLE_HOOK.store(
        hook.map_or(0, |hook| hook as usize),
        std::sync::atomic::Ordering::Release,
    );
}

/// Returns only partial output: an installed host MUST propagate the parked
/// condition before reporting a successful print. Bare Rust printers without
/// a condition hook cannot provide Lisp condition signalling.
pub fn print_not_readable(value: EgclVal) -> String {
    let hook = PRINT_NOT_READABLE_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if hook != 0 {
        let hook: PrintNotReadableHook = unsafe { std::mem::transmute(hook) };
        hook(value);
    }
    String::new()
}

/// Shared pathname policy for stream and string printers. A namestring is
/// readable only if reparsing preserves its components; dotfiles make the
/// missing-name/nonempty-type case ambiguous.
pub fn pathname_representation(value: EgclVal, escape: bool) -> Option<String> {
    let (text, faithful) = crate::pathnames::printed_namestring(value)?;
    if print_readably_active() && !faithful {
        egcl_rt::rooted!(value = value);
        if let Some(rendered) = dispatch_print_object(*value, true) {
            return Some(rendered);
        }
        return Some(print_not_readable(*value));
    }
    Some(if escape || print_readably_active() {
        format!("#P\"{}\"", escape_string_body(&text))
    } else {
        text
    })
}

/// Structural nesting depth for *PRINT-LEVEL* (0 at the top level).
/// Incremented around each list body; balanced, so it returns to 0.
static PRINT_DEPTH: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { std::cell::Cell::new(0) }) };

fn dispatch_print_object(v: EgclVal, escapep: bool) -> Option<String> {
    let hook = *PRINT_OBJECT_HOOK.lock().unwrap();
    hook.and_then(|h| h(v, escapep))
}

// ── Pretty-print dispatch hook ────────────────────────────────────
// With *PRINT-PRETTY* true the printer consults *PRINT-PPRINT-DISPATCH*
// (CLHS 22.2.1.4). The table and its type tests live in Lisp, so the
// interpreter installs a hook that returns the rendered text when an entry
// applies. A plain atomic, so the per-object check takes no lock.
/// Applies a Lisp function object to arguments, installed by the evaluator so
/// FORMAT can run a FORMATTER function given as a `~{~}` body or `~/name/`.
pub type FormatApplyHook = fn(EgclVal, &[EgclVal]) -> Result<EgclVal, EgclError>;
static FORMAT_APPLY_HOOK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Apply a Lisp FORMAT callback to a queued pretty stream and return both its
/// primary value and the semantic events it emitted.  The callback arguments
/// exclude the stream; the evaluator creates it at `start_column`.
pub type FormatLayoutApplyHook =
    fn(EgclVal, &[EgclVal], usize) -> Result<(EgclVal, LayoutDoc), EgclError>;
static FORMAT_LAYOUT_APPLY_HOOK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub fn set_format_apply_hook(hook: Option<FormatApplyHook>) {
    FORMAT_APPLY_HOOK.store(
        hook.map_or(0, |h| h as usize),
        std::sync::atomic::Ordering::Release,
    );
}

pub fn set_format_layout_apply_hook(hook: Option<FormatLayoutApplyHook>) {
    FORMAT_LAYOUT_APPLY_HOOK.store(
        hook.map_or(0, |h| h as usize),
        std::sync::atomic::Ordering::Release,
    );
}

/// Call `func` on `args`: through the evaluator's hook when installed (which
/// handles interpreted closures), else through the compiled entry point.
fn apply_format_function(func: EgclVal, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let raw = FORMAT_APPLY_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if raw != 0 {
        // SAFETY: the cell only ever holds 0 or a `FormatApplyHook` stored above.
        let hook: FormatApplyHook = unsafe { std::mem::transmute::<usize, FormatApplyHook>(raw) };
        return hook(func, args);
    }
    call_format_dispatch(func, args)
}

fn apply_format_function_to_layout(
    func: EgclVal,
    args: &[EgclVal],
    start_column: usize,
) -> Result<(EgclVal, LayoutDoc), EgclError> {
    let raw = FORMAT_LAYOUT_APPLY_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if raw != 0 {
        // SAFETY: the cell only ever holds 0 or a FormatLayoutApplyHook.
        let hook: FormatLayoutApplyHook =
            unsafe { std::mem::transmute::<usize, FormatLayoutApplyHook>(raw) };
        return hook(func, args, start_column);
    }

    // Standalone stdlib users have no evaluator-owned pretty stream. Preserve
    // the historical string-stream behavior as a compatibility fallback.
    egcl_rt::rooted!(func = func);
    egcl_rt::rooted!(stream = crate::streams::make_string_output_stream(NIL)?);
    let mut call_args = Vec::with_capacity(args.len() + 1);
    call_args.push(*stream);
    call_args.extend_from_slice(args);
    egcl_rt::rooted_ref!(_call_args_root = &mut call_args);
    let tail = apply_format_function(*func, &call_args)?;
    let text = extract_egcl_string(crate::streams::get_output_stream_string(*stream)?)
        .unwrap_or_default();
    let mut doc = LayoutDoc::default();
    doc.text(&text);
    Ok((tail, doc))
}

/// The body FORMATTER function as `run_iteration_body` takes it: NIL, the
/// sentinel for "the directive has a literal body", becomes None.
fn body_function_arg(v: EgclVal) -> Option<EgclVal> {
    (!v.is_nil()).then_some(v)
}

/// One pass over a `~{` body. An empty body takes its control from the first
/// argument (CLHS 22.3.7.4): a string is formatted in place; a function (a
/// FORMATTER) is called with a string stream and the remaining arguments and
/// returns the arguments it did not consume (ansi format.{.17-19).
fn run_iteration_body(
    body: &str,
    body_function: Option<EgclVal>,
    elements: &[EgclVal],
    idx: &mut usize,
    output: &mut LayoutDoc,
) -> Result<(), EgclError> {
    let Some(func) = body_function else {
        return format_impl(body, elements, idx, output);
    };
    egcl_rt::rooted!(func = func);
    let (tail, doc) = apply_format_function_to_layout(
        *func,
        &elements[*idx..],
        output.current_column(),
    )?;
    let remaining = if tail.is_cons() {
        cons_list_to_vec(tail).len()
    } else {
        0
    };
    *idx += (elements.len() - *idx).saturating_sub(remaining);
    output.append(doc);
    Ok(())
}

/// The printed representation of a function object the interpreter knows more
/// about than this crate does: a generic function prints as
/// `#<STANDARD-GENERIC-FUNCTION name>` (GitHub #508). Installed by the
/// interpreter, which owns the generic-function registry; `None` for any other
/// function. Must not allocate on the Lisp heap.
pub type FunctionPrintHook = fn(EgclVal) -> Option<String>;
static FUNCTION_PRINT_HOOK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_function_print_hook(hook: Option<FunctionPrintHook>) {
    FUNCTION_PRINT_HOOK.store(
        hook.map_or(0, |h| h as usize),
        std::sync::atomic::Ordering::Release,
    );
}

/// How a function object prints: `#<STANDARD-GENERIC-FUNCTION name>` for a
/// generic function, else `#<FUNCTION>`. Shared with the interpreter's own
/// printer so WRITE and FORMAT agree.
pub fn function_representation(v: EgclVal) -> String {
    let raw = FUNCTION_PRINT_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if raw != 0 {
        // SAFETY: the cell only ever holds 0 or a `FunctionPrintHook` stored above.
        let hook: FunctionPrintHook =
            unsafe { std::mem::transmute::<usize, FunctionPrintHook>(raw) };
        if let Some(text) = hook(v) {
            return text;
        }
    }
    "#<FUNCTION>".to_string()
}

pub type PprintDispatchHook = fn(EgclVal, bool, usize) -> Option<LayoutDoc>;
static PPRINT_DISPATCH_HOOK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub fn set_pprint_dispatch_hook(hook: Option<PprintDispatchHook>) {
    PPRINT_DISPATCH_HOOK.store(
        hook.map_or(0, |h| h as usize),
        std::sync::atomic::Ordering::Release,
    );
}

/// The text a *PRINT-PPRINT-DISPATCH* entry prints for `v`, when *PRINT-PRETTY*
/// is true, a dispatch hook is installed and an entry applies. Shared with the
/// interpreter's own printer so WRITE and FORMAT agree.
pub fn dispatch_pprint_layout(
    v: EgclVal,
    escapep: bool,
    start_column: usize,
) -> Option<LayoutDoc> {
    let raw = PPRINT_DISPATCH_HOOK.load(std::sync::atomic::Ordering::Acquire);
    if raw == 0 || !print_pretty_active() {
        return None;
    }
    // SAFETY: the cell only ever holds 0 or a `PprintDispatchHook` stored above.
    let hook: PprintDispatchHook = unsafe { std::mem::transmute::<usize, PprintDispatchHook>(raw) };
    hook(v, escapep, start_column)
}

pub fn dispatch_pprint(v: EgclVal, escapep: bool) -> Option<String> {
    dispatch_pprint_layout(v, escapep, 0).map(|doc| {
        render_layout(
            &doc,
            &LayoutOptions {
                margin: print_integer_variable("*PRINT-RIGHT-MARGIN*").unwrap_or(80),
                miser_width: print_integer_variable("*PRINT-MISER-WIDTH*"),
                line_limit: (!print_readably_active())
                    .then(|| print_integer_variable("*PRINT-LINES*"))
                    .flatten(),
                start_column: 0,
                pretty: print_pretty_active(),
            },
        )
    })
}

/// `*PRINT-PRETTY*` is true.
pub fn print_pretty_active() -> bool {
    egcl_rt::symbols::find_index("*PRINT-PRETTY*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| v != UNBOUND && !v.is_nil())
        .unwrap_or(false)
}

// ── String allocation ─────────────────────────────────────────────

/// Allocate a EgclVal string using the interned string table from the
/// streams module.  This ensures that format-produced strings compare
/// pointer-equal to strings created via `make_lisp_string()`.
fn make_egcl_string(s: &str) -> EgclVal {
    crate::streams::make_lisp_string(s)
}

// ── String extraction ────────────────────────────────────────────

/// Extract Rust string from a EgclString heap object.
/// Returns None if v is not a string-typed heap object.
fn extract_egcl_string(v: EgclVal) -> Option<String> {
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
        Some(egcl_rt::object::read_simple_string(ptr))
    }
}

/// The readtable case the printer folds symbol names for: the current
/// readtable's, except that *PRINT-READABLY* output must read back under the
/// STANDARD readtable (CLHS 22.1.3.3.1), i.e. :UPCASE.
fn effective_readtable_case_mode() -> u8 {
    if print_readably_active() {
        0
    } else {
        egcl_compiler::reader::current_readtable_case_mode()
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
    // CLHS 22.1.3.3.2: the readtable case decides which characters
    // *PRINT-CASE* governs.  :UPCASE -- the uppercase ones; :DOWNCASE -- the
    // lowercase ones; :PRESERVE -- none; :INVERT -- a name whose letters are
    // all one case is inverted, a mixed one is left alone.
    let mode = effective_readtable_case_mode();
    match mode {
        2 => return name.to_string(),
        3 => {
            let has_upper = name.chars().any(char::is_uppercase);
            let has_lower = name.chars().any(char::is_lowercase);
            return if has_upper && !has_lower {
                name.to_lowercase()
            } else if has_lower && !has_upper {
                name.to_uppercase()
            } else {
                name.to_string()
            };
        }
        _ => {}
    }
    let affected = |c: char| {
        if mode == 1 {
            c.is_lowercase()
        } else {
            c.is_uppercase()
        }
    };
    let case = egcl_rt::symbols::find_index("*PRINT-CASE*")
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|v| v.is_symbol())
        .and_then(|v| egcl_compiler::reader::symbol_name(v.as_symbol_index()));
    let mut out = String::with_capacity(name.len());
    let mut word_start = true;
    for c in name.chars() {
        if affected(c) {
            match case.as_deref() {
                Some("KEYWORD:DOWNCASE") => out.extend(c.to_lowercase()),
                Some("KEYWORD:CAPITALIZE") => {
                    if word_start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                }
                _ => out.extend(c.to_uppercase()), // :UPCASE or unset
            }
        } else {
            out.push(c);
        }
        word_start = !c.is_alphanumeric();
    }
    out
}

/// Would `name` read back as a number rather than a symbol? Such a name must be
/// printed inside bars (CLHS 2.3.4). Recognises integer, ratio and float syntax
/// — `123`, `-7.`, `1/2`, `1.5`, `1E5` — but not names that merely contain
/// digits or exponent markers. Integer and ratio digits use *PRINT-BASE*;
/// decimal points and float syntax remain decimal regardless of that base.
fn reads_as_number(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let radix = print_base();
    let radix_digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    let body = name.strip_prefix(['+', '-']).unwrap_or(name);
    if body.is_empty() {
        return false;
    }
    // Recognize syntax without parsing a machine integer: a long name can
    // read as a bignum too. In high bases exponent letters are digits first.
    if radix_digits(body) {
        return true;
    }
    // Integer, with CLHS's optional trailing decimal point: 123 or 123.
    if digits(body.strip_suffix('.').unwrap_or(body)) {
        return true;
    }
    // Ratio: digits / digits.
    if let Some((num, den)) = body.split_once('/') {
        // The reader also accepts a sign on its denominator. Escape that
        // spelling conservatively so it cannot change a symbol into a ratio.
        return (radix_digits(num) && radix_digits(den.strip_prefix(['+', '-']).unwrap_or(den)))
            || (digits(num) && digits(den));
    }
    // Float: split a trailing exponent off first, then check the mantissa. An
    // exponent alone is not enough (`E5` is a symbol); the mantissa must be
    // numeric, and a float needs either a decimal point or an exponent.
    let (mantissa, has_exponent) = body
        .char_indices()
        .find(|(_, c)| matches!(c, 'e' | 'E' | 's' | 'S' | 'f' | 'F' | 'd' | 'D' | 'l' | 'L'))
        .and_then(|(i, _)| {
            let exp = &body[i + 1..];
            let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
            digits(exp_digits).then(|| (&body[..i], true))
        })
        .unwrap_or((body, false));
    match mantissa.split_once('.') {
        Some((int, frac)) => {
            // `1.5`, `.5`, `1.` — at least one digit, both halves numeric.
            (int.is_empty() || digits(int))
                && (frac.is_empty() || digits(frac))
                && mantissa.bytes().any(|b| b.is_ascii_digit())
        }
        None => has_exponent && digits(mantissa),
    }
}

/// Whether a symbol name must be printed inside `|…|` bars so it reads back as
/// the same symbol (CLHS 22.1.3.3). A name needs bars when it is empty, is all
/// dots, contains a character the reader would treat specially (whitespace, a
/// macro or terminating character, a package marker, or the escape characters
/// themselves), contains lowercase the standard `:upcase` readtable would fold,
/// or would read as a number.
pub fn symbol_name_needs_bars(name: &str) -> bool {
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return true;
    }
    // Which letters would the reader fold?  Those need bars: lowercase under
    // an :UPCASE readtable, uppercase under :DOWNCASE, none under :PRESERVE
    // or :INVERT (the latter round-trips a single-case name).
    let folded = |c: char| match effective_readtable_case_mode() {
        0 => c.is_lowercase(),
        1 => c.is_uppercase(),
        _ => false,
    };
    if name.chars().any(|c| {
        c.is_whitespace()
            || folded(c)
            || matches!(
                c,
                '(' | ')' | '\'' | '"' | ';' | '`' | ',' | '|' | '\\' | '#' | ':'
            )
    }) {
        return true;
    }
    reads_as_number(name)
}

/// A symbol name as the printer should render it. Under `*print-escape*`
/// (prin1/~S) a name that would not read back is wrapped in bars, with any
/// embedded `|` or `\` backslash-escaped, and *print-case* deliberately does
/// NOT apply inside bars — the name is reproduced verbatim. princ/~A never
/// adds bars (bliss-4m85).
pub fn print_symbol_name(name: &str, escapep: bool) -> String {
    if escapep && symbol_name_needs_bars(name) {
        let mut out = String::with_capacity(name.len() + 2);
        out.push('|');
        for c in name.chars() {
            if c == '|' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('|');
        return out;
    }
    apply_print_case(name)
}

/// Render a possibly package-qualified symbol spelling (as produced by
/// `symbol_name_for_print`).
///
/// Bars quote a TOKEN, not a whole spelling: the package marker is printer
/// syntax and belongs OUTSIDE them. A name needing escapes therefore prints as
/// `PKG::|has space|`, never `|PKG::has space|` — the latter reads back as a
/// single symbol whose name contains a colon, i.e. a different symbol
/// (bliss-10cf). Each token is quoted independently, so a package whose own
/// name needs bars is handled too.
///
/// Without `*print-escape*` (princ / `~A`) only the characters of the NAME are
/// output: the package prefix is part of the escaped syntax, which is why
/// `(format nil "~A" 'wq::zed)` is `"ZED"` (CLHS 22.1.3.3).
///
/// `spelling` must be a qualified-or-bare spelling, NOT a raw uninterned or
/// keyword registry name — those are bare tokens and go to `print_symbol_name`.
/// Render `symbol` as the printer should emit it.
///
/// Bars quote a TOKEN: a qualifier's package and name are quoted separately so
/// the marker stays outside them (`PW::|has space|`), while a symbol printing
/// bare is a single token quoted whole (`|PR2::ZED|`). Without `*print-escape*`
/// only the name's characters are output (CLHS 22.1.3.3).
pub fn print_symbol_for_output(symbol: EgclVal, escapep: bool) -> String {
    // A low-index symbol can lose its home through UNINTERN/DELETE-PACKAGE.
    // Its saved name still carries the old qualifier, which must not point at
    // a new package incarnation when printing the retained homeless symbol.
    if let Some(index) = symbol.symbol_index()
        && !egcl_rt::symbols::is_uninterned(index)
        && egcl_rt::symbols::registry_key(index).is_none()
        && egcl_rt::symbols::symbol_package(index).is_some_and(|home| home.is_nil())
        && let Some(name) = egcl_rt::symbols::symbol_name(index)
    {
        let bare = egcl_rt::symbols::split_registry_key(&name)
            .map_or(name.as_str(), |(_, bare)| bare);
        let printed = print_symbol_name(bare, escapep);
        return if escapep && print_gensym() {
            format!("#:{printed}")
        } else {
            printed
        };
    }
    let (qualifier, name) = symbol_print_parts(symbol);
    match qualifier {
        None => print_symbol_name(&name, escapep),
        // princ/~A drops the qualifier entirely.
        Some(_) if !escapep => print_symbol_name(&name, false),
        Some((package, marker)) => format!(
            "{}{}{}",
            print_symbol_name(&package, true),
            marker,
            print_symbol_name(&name, true)
        ),
    }
}

pub fn print_qualified_symbol_name(spelling: &str, escapep: bool) -> String {
    // Split on the FIRST marker: later colons belong to the symbol's own name
    // (bliss-wne9.2.3). A leading marker means there is no package token, so
    // leave such a spelling alone rather than inventing an empty one.
    let split = spelling
        .split_once("::")
        .map(|(package, bare)| (package, "::", bare))
        .or_else(|| {
            spelling
                .split_once(':')
                .map(|(package, bare)| (package, ":", bare))
        })
        .filter(|(package, _, _)| !package.is_empty());

    let Some((package, marker, bare)) = split else {
        return print_symbol_name(spelling, escapep);
    };
    if !escapep {
        return print_symbol_name(bare, false);
    }
    format!(
        "{}{}{}",
        print_symbol_name(package, true),
        marker,
        print_symbol_name(bare, true)
    )
}

/// A symbol's readable name relative to the current `*PACKAGE*`. Registry keys
/// encode home identity as `PKG::NAME`, but that qualifier is omitted when the
/// same symbol is accessible in the current package (CLHS 22.1.3.3). Keep the
/// qualifier when omitting it would read back as a different symbol.
pub fn symbol_name_for_print(symbol: EgclVal) -> String {
    let (qualifier, name) = symbol_print_parts(symbol);
    match qualifier {
        Some((package, marker)) => format!("{package}{marker}{name}"),
        None => name,
    }
}

/// How a symbol should be spelled, split into its optional `(package, marker)`
/// qualifier and its NAME.
///
/// The printer needs the split rather than a joined spelling: when the
/// qualifier is `None` the name is a single TOKEN and must be quoted as one,
/// even though it may itself contain colons. Re-deriving the split from a
/// joined string cannot tell `|PR2::ZED|` — one symbol, accessible bare, whose
/// NAME is "PR2::ZED" — from the qualified spelling `PR2::ZED`, and printing
/// the former as the latter reads back as a different symbol (bliss-lgml).
pub fn symbol_print_parts(symbol: EgclVal) -> (Option<(String, &'static str)>, String) {
    let Some(index) = symbol.symbol_index() else {
        return (None, if symbol.is_nil() { "NIL" } else { "T" }.to_string());
    };
    let name = egcl_compiler::reader::symbol_name(index).unwrap_or_else(|| format!("SYM#{index}"));
    if name.starts_with("KEYWORD:") || egcl_compiler::reader::is_uninterned(index) {
        return (None, name);
    }
    let bare = egcl_rt::symbols::split_registry_key(&name)
        .map(|(_, bare)| bare)
        .unwrap_or(&name);
    let current_package = egcl_rt::symbols::find_index("*PACKAGE*")
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|package| crate::packages::is_package(*package));
    if current_package.is_some_and(|package| {
        crate::packages::find_symbol(bare, package)
            .ok()
            .flatten()
            .is_some_and(|(visible, _)| visible == symbol)
    }) {
        return (None, bare.to_string());
    }
    // Keep the qualifier, spelled by ACCESSIBILITY per CLHS 22.1.3.3.1: an
    // EXTERNAL symbol of its home package prints "PKG:NAME", anything else
    // "PKG::NAME" — independent of which spelling the registry key happens to
    // carry (canonical keys are "::"; legacy single-colon keys still exist).
    let qualifier = egcl_rt::symbols::split_registry_key(&name).map(|(package, _)| package);
    if let Some(package_name) = qualifier {
        if let Some(home) = crate::packages::find_package(package_name) {
            let marker = if crate::packages::is_external_symbol(home, bare) {
                ":"
            } else {
                "::"
            };
            return (Some((package_name.to_string(), marker)), bare.to_string());
        }
    }
    (None, name)
}

/// `*PRINT-READABLY*`: when true the output must read back as a similar
/// object, so *PRINT-LENGTH*, *PRINT-LEVEL* and *PRINT-LINES* are treated as
/// NIL and *PRINT-ESCAPE*, *PRINT-GENSYM* and *PRINT-ARRAY* as T (CLHS
/// 22.1.3.5; ansi printer print.cons.random.*).
pub fn print_readably_active() -> bool {
    egcl_rt::symbols::find_index("*PRINT-READABLY*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| v != UNBOUND && !v.is_nil())
        .unwrap_or(false)
}

/// `*PRINT-LENGTH*`: the max number of elements of a list/vector to print before
/// `...`, or `None` (unbounded) when the variable is NIL/unset or
/// *PRINT-READABLY* is true.
pub fn print_length() -> Option<usize> {
    if print_readably_active() {
        return None;
    }
    egcl_rt::symbols::find_index("*PRINT-LENGTH*")
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum().max(0) as usize)
}

/// `*PRINT-LEVEL*`: the max nesting depth to print before `#`, or `None`
/// (unbounded) when the variable is NIL/unset or *PRINT-READABLY* is true.
pub fn print_level() -> Option<usize> {
    if print_readably_active() {
        return None;
    }
    egcl_rt::symbols::find_index("*PRINT-LEVEL*")
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|v| v.is_fixnum())
        .map(|v| v.as_fixnum().max(0) as usize)
}

/// Common default-structure policy for both printer front ends. A slot name
/// and its value count together as one component; the type name is the header.
/// Existing control readers make readable output independent of both limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructurePrintPlan {
    pub level_exceeded: bool,
    pub shown_slots: usize,
    pub ellipsis: bool,
}

pub fn structure_print_plan(depth: usize, available_slots: usize) -> StructurePrintPlan {
    let shown_slots = print_length().unwrap_or(available_slots).min(available_slots);
    StructurePrintPlan {
        level_exceeded: print_level().is_some_and(|level| depth >= level),
        shown_slots,
        ellipsis: shown_slots < available_slots,
    }
}

/// `*PRINT-CIRCLE*`: when non-NIL, shared and circular structure is printed
/// with `#N=`/`#N#` labels instead of looping forever.
/// `*PRINT-ARRAY*`: whether arrays other than strings print readably as
/// `#(...)` / `#*...` / `#nA(...)`; NIL prints them as `#<...>`. True under
/// *PRINT-READABLY*, and when the variable is unbound.
pub fn print_array_active() -> bool {
    if print_readably_active() {
        return true;
    }
    egcl_rt::symbols::find_index("*PRINT-ARRAY*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| v == UNBOUND || !v.is_nil())
        .unwrap_or(true)
}

/// Concise standard array output shared by stream and string printers.
/// Pretty dispatch and circle/depth handling run before the ordinary body;
/// readable output overrides PRINT-ARRAY, and character arrays remain strings.
pub fn concise_array_representation(value: EgclVal) -> Option<String> {
    if print_array_active() { return None; }
    let kind = egcl_rt::gc::heap_object_type_id(value)?;
    match kind {
        type_id::SIMPLE_VECTOR => Some(format!(
            "#<SIMPLE-VECTOR {}>", crate::sequences::length(value).ok()?,
        )),
        type_id::MD_ARRAY => Some("#<ARRAY>".into()),
        type_id::COMPLEX_ARRAY if !crate::sequences::cvec_is_string(value) => {
            let kind = if crate::sequences::cvec_is_bit(value) { "BIT-VECTOR" } else { "VECTOR" };
            Some(format!("#<{kind} {}>", crate::sequences::cvec_fill_pointer(value)))
        }
        type_id::SIMPLE_ARRAY => {
            let (kind, length) = if let Some(length) = egcl_rt::packed_u8::len(value) {
                ("UB8-VECTOR", length)
            } else if let Some(length) = egcl_rt::packed_f64::len(value) {
                ("DOUBLE-FLOAT-VECTOR", length)
            } else if let Some(length) = egcl_rt::types::bit_vector_len(value) {
                ("BIT-VECTOR", length)
            } else {
                return Some("#<ARRAY>".into());
            };
            Some(format!("#<{kind} {length}>"))
        }
        _ => None,
    }
}

/// Whether `*READ-DEFAULT-FLOAT-FORMAT*` names DOUBLE-FLOAT or LONG-FLOAT, so a
/// double prints without an exponent marker and a single float needs `f`.
fn default_float_format_is_double() -> bool {
    egcl_rt::symbols::find_index("*READ-DEFAULT-FLOAT-FORMAT*")
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|v| v.is_symbol() && *v != UNBOUND)
        .and_then(|v| egcl_compiler::reader::symbol_name(v.as_symbol_index()))
        .is_some_and(|name| {
            let bare = egcl_rt::symbols::split_registry_key(&name)
                .map(|(_, bare)| bare.to_string())
                .unwrap_or(name);
            bare == "DOUBLE-FLOAT" || bare == "LONG-FLOAT"
        })
}

pub fn print_circle_active() -> bool {
    egcl_rt::symbols::find_index("*PRINT-CIRCLE*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| v != UNBOUND && !v.is_nil())
        .unwrap_or(false)
}

/// `*PRINT-GENSYM*` (default T): when true, an uninterned symbol prints with the
/// `#:` prefix under escape printing so it reads back as a fresh symbol; NIL
/// suppresses the prefix (CLHS 22.1.3.3).
pub fn print_gensym() -> bool {
    if print_readably_active() {
        return true;
    }
    egcl_rt::symbols::find_index("*PRINT-GENSYM*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| !v.is_nil())
        .unwrap_or(true)
}

// ── *PRINT-CIRCLE* support (bliss-dlil) ──────────────────────────────────
//
// A two-pass scheme shared by BOTH printers (cli.rs `print_val` and this
// module's `egclval_to_print_string`), so `princ`/`print`/`prin1` and
// `prin1-to-string`/`format ~S` behave identically:
//
//   Pass 1 (`CircleTable::build`) walks the structure once, counting how many
//   times each cons, array, or uninterned symbol is reached by EQ identity.
//   Objects reached ≥2 times are "shared" (this includes a circular back-edge,
//   which reaches the head a second time). Cycles can't diverge because the
//   scan stops following a node the moment it is seen again.
//
//   Pass 2 is the normal recursive print, but each printer asks the table,
//   per object, whether to emit a `#N=` label (first visit of a shared node),
//   a `#N#` back-reference (subsequent visit — do NOT recurse), or nothing.
//
// Circle identities are relocated by the root scanner, including tables
// belonging to fibers suspended inside a user PRINT-OBJECT method.

/// What to emit for a shared object under `*print-circle*`.
pub enum CircleMark {
    /// Not shared — print the object normally.
    NotShared,
    /// First time this shared node is printed: emit `#N=` then its contents.
    First(u32),
    /// Already printed once: emit `#N#` and do not recurse.
    Repeat(u32),
}

struct CircleTable {
    /// Tagged object identities and their multiplicities. Preserve tags when
    /// tracing: arrays and conses have different GC pointer encodings.
    counts: std::collections::HashMap<u64, u32>,
    /// Tagged identity → label number, assigned in first-print order.
    labels: std::collections::HashMap<u64, u32>,
    next_label: u32,
}

/// Visible vector length, or total row-major size of an array. Scalars and
/// opaque pseudo-heap handles are excluded before any header access.
pub fn circle_array_length(value: EgclVal) -> Option<usize> {
    if !value.is_heap_object() || crate::pathnames::is_pathname(value) { return None; }
    if crate::sequences::is_complex_vector(value) {
        Some(crate::sequences::cvec_fill_pointer(value))
    } else {
        crate::sequences::array_total_size(value)
    }
}

/// Read only reference-bearing array storage. Specialized scalar storage has
/// no outgoing graph edges, and reading it through ELT could allocate a box.
fn circle_array_reference(mut value: EgclVal, mut index: usize) -> Option<EgclVal> {
    loop {
        if !value.is_heap_object() { return None; }
        unsafe {
            let pointer = value.as_ptr();
            match (*(pointer as *const ObjectHeader)).type_id() {
                type_id::SIMPLE_VECTOR => {
                    return Some(*(pointer.add(object_payload_offset(pointer) + 8 + index * 8) as *const EgclVal));
                }
                type_id::MD_ARRAY => value = *(pointer.add(8) as *const EgclVal),
                type_id::COMPLEX_ARRAY => {
                    if let Some((base, offset, _)) = crate::sequences::cvec_displacement(value) {
                        value = base;
                        index += offset;
                    } else {
                        value = *(pointer.add(8) as *const EgclVal);
                    }
                }
                _ => return None,
            }
        }
    }
}

// A retained low-index symbol can become homeless after UNINTERN or
// DELETE-PACKAGE. A nil home alone is insufficient: internal interned symbols
// can still have a lazy home, so require the retired interning key as well.
fn circle_uninterned_symbol(value: EgclVal) -> bool {
    value.symbol_index().is_some_and(|index| {
        egcl_rt::symbols::is_uninterned(index)
            || (egcl_rt::symbols::registry_key(index).is_none()
                && egcl_rt::symbols::symbol_package(index).is_some_and(|home| home.is_nil()))
    })
}

impl CircleTable {
    fn build(root: EgclVal) -> Option<CircleTable> {
        if !print_circle_active() {
            return None;
        }
        let mut t = CircleTable {
            counts: std::collections::HashMap::new(),
            labels: std::collections::HashMap::new(),
            next_label: 1,
        };
        t.scan(root);
        if t.counts.is_empty() { None } else { Some(t) }
    }

    /// Walk reference-bearing storage without allocating Lisp values. In
    /// particular, generic ELT can box packed floats and cannot run while the
    /// new table and pending graph are not registered as GC roots.
    fn scan(&mut self, root: EgclVal) {
        let mut pending = vec![root];
        while let Some(value) = pending.pop() {
            let array = circle_array_length(value);
            if !value.is_cons() && array.is_none()
                && !(circle_uninterned_symbol(value))
                && !egcl_rt::random_state::is_random_state(value)
            {
                continue;
            }
            let count = self.counts.entry(value.0).or_insert(0);
            *count += 1;
            if *count > 1 { continue; }
            if value.is_cons() {
                unsafe {
                    let cell = &*(value.as_ptr() as *const egcl_rt::object::ConsCell);
                    pending.push(cell.cdr);
                    pending.push(cell.car);
                }
            } else if let Some(length) = array {
                // Only elements belong to the printed graph. Array backing
                // storage and displacement bases are implementation details.
                for index in (0..length).rev() {
                    if let Some(element) = circle_array_reference(value, index) {
                        pending.push(element);
                    } else {
                        break; // Packed scalar storage has no reference edges.
                    }
                }
            }
        }
    }

    fn is_shared(&self, value: EgclVal) -> bool {
        self.counts.get(&value.0).is_some_and(|count| *count >= 2)
    }

    fn visit(&mut self, v: EgclVal) -> CircleMark {
        if !self.is_shared(v) {
            return CircleMark::NotShared;
        }
        let addr = v.0;
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

#[derive(Default)]
struct CircleContexts {
    tables: Vec<CircleTable>,
    pushed: Vec<bool>,
    next_label: u32,
}

static CIRCLE: egcl_rt::execution_local::ExecutionLocal<std::cell::RefCell<CircleContexts>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        std::cell::RefCell::new(CircleContexts::default())
    })
};

/// Reuse the current graph's table for recursive printing, but scan an
/// independent graph written inside a logical block. Pair with [`circle_exit`].
pub fn circle_enter(root: EgclVal) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        egcl_rt::gc::register_root_scanner(|visit| {
            // SAFETY: all mutators are stopped while registered scanners run.
            unsafe {
                CIRCLE.scan(|slot| {
                    for table in &mut slot.borrow_mut().tables {
                        let mut counts = std::collections::HashMap::new();
                        let mut labels = std::collections::HashMap::new();
                        for (address, count) in table.counts.drain() {
                            let mut value = EgclVal(address);
                            visit(&mut value);
                            let relocated = value.0;
                            counts.insert(relocated, count);
                            if let Some(label) = table.labels.remove(&address) {
                                labels.insert(relocated, label);
                            }
                        }
                        table.counts = counts;
                        table.labels = labels;
                    }
                })
            };
        })
    });
    let covered = CIRCLE.with(|c| {
        c.borrow().tables.last().is_some_and(|table| table.counts.contains_key(&root.0))
    });
    let table = if covered {
        None
    } else {
        CircleTable::build(root)
    };
    CIRCLE.with(|c| {
        let mut contexts = c.borrow_mut();
        if contexts.pushed.is_empty() {
            contexts.next_label = 1;
        }
        contexts.pushed.push(table.is_some());
        if let Some(mut table) = table {
            table.next_label = contexts
                .tables
                .last()
                .map_or(contexts.next_label, |parent| parent.next_label);
            contexts.tables.push(table);
        }
    });
}

/// Leave a print level, restoring any enclosing graph's circle context.
pub fn circle_exit() {
    CIRCLE.with(|c| {
        let mut contexts = c.borrow_mut();
        if contexts.pushed.pop() == Some(true) {
            if let Some(child) = contexts.tables.pop() {
                contexts.next_label = child.next_label;
                if let Some(parent) = contexts.tables.last_mut() {
                    parent.next_label = child.next_label;
                }
            }
        }
    });
}

/// True when an object graph is being tracked for sharing or circularity.
pub fn circle_active() -> bool {
    CIRCLE.with(|c| !c.borrow().tables.is_empty())
}

/// True when `v` is shared/circular (also used to force a dotted cons tail).
pub fn circle_is_shared(v: EgclVal) -> bool {
    CIRCLE.with(|c| c.borrow().tables.last().is_some_and(|t| t.is_shared(v)))
}

/// Register a visit to `v`, assigning a label on first sight.
pub fn circle_visit(v: EgclVal) -> CircleMark {
    CIRCLE.with(|c| {
        let mut b = c.borrow_mut();
        match b.tables.last_mut() {
            Some(t) => t.visit(v),
            None => CircleMark::NotShared,
        }
    })
}

thread_local! {
    /// A radix a `~B`/`~O`/`~X`/`~nR` directive imposes while it prints a
    /// non-integer argument as `~A` (CLHS 22.3.2.2); 0 when none.
    static PRINT_BASE_OVERRIDE: Cell<u32> = const { Cell::new(0) };
}

/// Print `val` as `~A` would under `*PRINT-BASE*` = `radix`, padded on the
/// left to `mincol` with `padchar`: the non-integer case of the radix
/// directives.
fn push_non_integer_in_radix(
    output: &mut LayoutDoc,
    mut val: EgclVal,
    radix: u32,
    mincol: usize,
    padchar: char,
) {
    egcl_rt::rooted_ref!(_val_root = &mut val);
    let prev = PRINT_BASE_OVERRIDE.with(|c| c.replace(radix));
    let text = with_princ_bindings(|| egclval_to_print_string(val, false));
    PRINT_BASE_OVERRIDE.with(|c| c.set(prev));
    push_padded_left(output, &text, mincol, padchar);
}

pub fn print_base() -> u32 {
    let forced = PRINT_BASE_OVERRIDE.with(|c| c.get());
    if forced != 0 {
        return forced;
    }
    egcl_rt::symbols::find_index("*PRINT-BASE*")
        .and_then(egcl_rt::symbols::symbol_value)
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

// Set while printing a rational's numerator/denominator so those integer
// components are NOT individually radix-decorated: a ratio takes a single
// leading radix specifier around the whole `num/den`, not a per-part
// decoration (CLHS 22.1.3.1.1; bliss-6i2z).
static SUPPRESS_INT_RADIX: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| const { std::cell::Cell::new(false) })
};

/// The current `*PRINT-RADIX*` flag, read from the value cell via `find_index`
/// (like [`print_base`]). Defaults to NIL. bliss-6i2z.
fn print_radix() -> bool {
    egcl_rt::symbols::find_index("*PRINT-RADIX*")
        .and_then(egcl_rt::symbols::symbol_value)
        .map(|v| v != UNBOUND && !v.is_nil())
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
fn heap_number_string(v: EgclVal, escapep: bool) -> Option<String> {
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
                let num = *(ptr.add(8) as *const EgclVal);
                let den = *(ptr.add(16) as *const EgclVal);
                let base = print_base();
                let radix = print_radix_active();
                // Print num/den WITHOUT per-part radix decoration; the ratio
                // takes a single leading specifier (bliss-6i2z).
                let prev = suppress_integer_radix(true);
                let body = format!(
                    "{}/{}",
                    egclval_to_print_string(num, escapep),
                    egclval_to_print_string(den, escapep)
                );
                suppress_integer_radix(prev);
                Some(if radix {
                    format!("{}{}", ratio_radix_prefix(base), body)
                } else {
                    body
                })
            }
            type_id::COMPLEX => {
                let rp = *(ptr.add(8) as *const EgclVal);
                let ip = *(ptr.add(16) as *const EgclVal);
                Some(format!(
                    "#C({} {})",
                    egclval_to_print_string(rp, escapep),
                    egclval_to_print_string(ip, escapep)
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

/// Preserve empty multidimensional array metadata that #nA cannot encode.
/// No elements are printed and no Lisp allocation is needed; the explicit
/// dimensions also retain axes following a zero-length dimension.
pub fn readable_array_representation(value: EgclVal) -> Option<String> {
    if print_readably_active()
        && let Some(dimensions) = egcl_rt::types::md_array_dims(value)
    {
        let dimensions = (0..crate::sequences::length(dimensions).ok()?)
            .map(|index| {
                crate::sequences::elt(dimensions, index)
                    .ok()
                    .map(|v| v.as_fixnum())
            })
            .collect::<Option<Vec<_>>>()?;
        if dimensions.contains(&0) {
            use crate::sequences::ArrayElementKind;
            let element_type = match crate::sequences::array_element_kind(value)? {
                ArrayElementKind::General => "COMMON-LISP:T",
                ArrayElementKind::BaseChar => "COMMON-LISP:BASE-CHAR",
                ArrayElementKind::Character => "COMMON-LISP:CHARACTER",
                ArrayElementKind::Bit => "COMMON-LISP:BIT",
                ArrayElementKind::DoubleFloat => "COMMON-LISP:DOUBLE-FLOAT",
                ArrayElementKind::U8 => "(COMMON-LISP:UNSIGNED-BYTE #10r8)",
            };
            let mut text = String::from("#.(COMMON-LISP:MAKE-ARRAY '(");
            for (index, dimension) in dimensions.iter().enumerate() {
                if index != 0 {
                    text.push(' ');
                }
                text.push_str(&format!("#10r{dimension}"));
            }
            text.push_str(&format!(") :ELEMENT-TYPE '{element_type}"));
            if crate::sequences::array_adjustable(value) {
                text.push_str(" :ADJUSTABLE COMMON-LISP:T");
            }
            text.push(')');
            return Some(text);
        }
    }
    u8_readable_representation(value)
}

/// Readable octet arrays use the existing standard read-time evaluation
/// syntax. Reading this representation requires the normal *READ-EVAL* = T.
/// Explicit decimal radix prefixes make reconstruction independent of the
/// printer's base and of the reader's numeric base. Displaced views are copied
/// into similar storage; capacity, rank, element type and vector flags survive.
/// This path allocates only Rust strings/vectors, never Lisp objects.
pub fn u8_readable_representation(value: EgclVal) -> Option<String> {
    if !print_readably_active() || !crate::sequences::is_u8_array(value) {
        return None;
    }
    fn contents(value: EgclVal, dims: &[usize], index: &mut usize, out: &mut String) {
        if let Some((&length, rest)) = dims.split_first() {
            out.push('(');
            for i in 0..length {
                if i != 0 {
                    out.push(' ');
                }
                contents(value, rest, index, out);
            }
            out.push(')');
        } else {
            let byte =
                crate::sequences::u8_array_element(value, *index).expect("octet array element");
            out.push_str(&format!("#10r{byte}"));
            *index += 1;
        }
    }
    let md_dims = egcl_rt::types::md_array_dims(value);
    let dims = if let Some(dims) = md_dims {
        (0..crate::sequences::length(dims).expect("array dimensions"))
            .map(|i| {
                crate::sequences::elt(dims, i)
                    .expect("array dimension")
                    .as_fixnum() as usize
            })
            .collect::<Vec<_>>()
    } else {
        vec![crate::sequences::array_total_size(value).expect("octet array size")]
    };
    let mut out = String::from("#.(COMMON-LISP:MAKE-ARRAY ");
    if md_dims.is_some() {
        out.push_str("'(");
        for (i, dimension) in dims.iter().enumerate() {
            if i != 0 {
                out.push(' ');
            }
            out.push_str(&format!("#10r{dimension}"));
        }
        out.push(')');
    } else {
        out.push_str(&format!("#10r{}", dims[0]));
    }
    out.push_str(" :ELEMENT-TYPE '(COMMON-LISP:UNSIGNED-BYTE #10r8) :INITIAL-CONTENTS '");
    let mut index = 0;
    contents(value, &dims, &mut index, &mut out);
    if crate::sequences::is_complex_vector(value)
        && crate::sequences::cvec_has_fill_pointer(value)
    {
        out.push_str(&format!(
            " :FILL-POINTER #10r{}",
            crate::sequences::cvec_fill_pointer(value)
        ));
    }
    if crate::sequences::array_adjustable(value) {
        out.push_str(" :ADJUSTABLE COMMON-LISP:T");
    }
    out.push(')');
    Some(out)
}

/// Render a heap vector — simple-vector or complex (fill-pointer / adjustable) —
/// as `#(e0 e1 …)`. `None` for non-vector heap objects. A complex vector shows
/// only its active elements (0..fill-pointer).
fn heap_vector_string(v: EgclVal, escapep: bool) -> Option<String> {
    if !v.is_heap_object() {
        return None;
    }
    if crate::pathnames::registered_string(v).is_some() || crate::pathnames::is_pathname(v) {
        return None;
    }
    if let Some(length) = egcl_rt::packed_u8::len(v) {
        egcl_rt::rooted!(v = v);
        return Some(vector_elements_string(length, |i| {
            egclval_to_print_string(
                EgclVal::from_fixnum(i64::from(egcl_rt::packed_u8::get(*v, i).unwrap())),
                escapep,
            )
        }));
    }
    if let Some(length) = egcl_rt::packed_f64::len(v) {
        egcl_rt::rooted!(v = v);
        return Some(vector_elements_string(length, |i| {
            double_float_to_string(egcl_rt::packed_f64::get(*v, i).unwrap())
        }));
    }
    unsafe {
        let ptr = v.as_ptr();
        // Multidimensional array → `#<rank>A(nested row-major lists)`.
        if (*(ptr as *const ObjectHeader)).type_id() == type_id::MD_ARRAY {
            let storage = *(ptr.add(8) as *const EgclVal);
            let dims_vec = *(ptr.add(16) as *const EgclVal);
            let rank = (*(ptr.add(24) as *const EgclVal)).as_fixnum().max(0) as usize;
            let dbase = dims_vec.as_ptr();
            let doff = object_payload_offset(dbase);
            let dn = *(dbase.add(doff) as *const u64) as usize;
            let mut dims = Vec::with_capacity(dn);
            for k in 0..dn {
                dims.push(
                    (*(dbase.add(doff + 8 + k * 8) as *const EgclVal))
                        .as_fixnum()
                        .max(0) as usize,
                );
            }
            // Past *PRINT-LEVEL* the whole array prints as `#`.
            if print_level().is_some_and(|lvl| PRINT_DEPTH.with(|d| d.get()) >= lvl) {
                return Some("#".to_string());
            }
            let nested = md_render(storage, &dims, 0, escapep);
            return Some(format!("#{rank}A{nested}"));
        }
        // A complex vector reads its elements through the displacement-aware
        // accessor: word 0 of a DISPLACED array is the BASE array reference,
        // not element storage, and walking a bit-vector/string base's internal
        // words as values printed garbage and segfaulted (bliss-v9nb; keep in
        // sync with cli print_val's COMPLEX_ARRAY branch — the two printers).
        if (*(ptr as *const ObjectHeader)).type_id() == type_id::COMPLEX_ARRAY {
            let count = crate::sequences::cvec_fill_pointer(v);
            if crate::sequences::cvec_is_bit(v) {
                // A fill-pointer / adjustable / displaced bit vector prints
                // like a simple one (ansi print.bit-vector.5-7).
                let mut s = String::from("#*");
                for i in 0..count {
                    let e = crate::sequences::cvec_element(v, i).unwrap_or(NIL);
                    s.push(if e.is_fixnum() && e.as_fixnum() == 1 {
                        '1'
                    } else {
                        '0'
                    });
                }
                return Some(s);
            }
            egcl_rt::rooted!(v = v);
            return Some(vector_elements_string(count, |i| {
                if let Some(raw) = crate::sequences::double_float_array_element(*v, i) {
                    double_float_to_string(raw)
                } else {
                    let e = crate::sequences::cvec_element(*v, i).unwrap_or(NIL);
                    egclval_to_print_string(e, escapep)
                }
            }));
        }
        let count = match (*(ptr as *const ObjectHeader)).type_id() {
            type_id::SIMPLE_VECTOR => *(ptr.add(object_payload_offset(ptr)) as *const u64) as usize,
            _ => return None,
        };
        // Re-read the payload through the rooted vector for each element:
        // printing one may dispatch PRINT-OBJECT, which allocates.
        egcl_rt::rooted!(v = v);
        Some(vector_elements_string(count, |i| {
            let p = v.as_ptr();
            let e = *(p.add(object_payload_offset(p) + 8 + i * 8) as *const EgclVal);
            egclval_to_print_string(e, escapep)
        }))
    }
}

/// Render one axis of a multidimensional array's row-major storage as nested
/// parenthesised lists (CLHS `#nA` syntax). `base` is the row-major storage
/// vector, `dims` the remaining axes, `start` the flat offset of this
/// subtree. Each axis is a list for *PRINT-LEVEL* and *PRINT-LENGTH* (CLHS
/// 22.1.3.8): the caller has checked this list's own depth, a sub-axis past
/// the level prints as `#`, and a list stops at `...` after *PRINT-LENGTH*
/// elements (keep in sync with cli `print_md_nested`).
fn md_render(base: EgclVal, dims: &[usize], start: usize, escapep: bool) -> String {
    // Printing an element may dispatch PRINT-OBJECT, which allocates.
    egcl_rt::rooted!(base = base);
    let Some((&n, rest)) = dims.split_first() else {
        return if let Some(raw) = egcl_rt::packed_f64::get(*base, start) {
            double_float_to_string(raw)
        } else {
            egclval_to_print_string(crate::sequences::elt(*base, start).unwrap_or(NIL), escapep)
        };
    };
    let depth = PRINT_DEPTH.with(|d| d.get());
    let limit = print_length();
    let sub_axis_elided = !rest.is_empty() && print_level().is_some_and(|lvl| depth + 1 >= lvl);
    let stride: usize = rest.iter().product();
    PRINT_DEPTH.with(|d| d.set(depth + 1));
    let mut s = String::from("(");
    for i in 0..n {
        if i > 0 {
            s.push(' ');
        }
        if limit.is_some_and(|l| i >= l) {
            s.push_str("...");
            break;
        }
        if sub_axis_elided {
            s.push('#');
        } else {
            s.push_str(&md_render(*base, rest, start + i * stride, escapep));
        }
    }
    s.push(')');
    PRINT_DEPTH.with(|d| d.set(depth));
    s
}

/// `#(...)` over a vector's `count` elements under *PRINT-LEVEL* and
/// *PRINT-LENGTH* (CLHS 22.1.3.7), as cli `print_val` prints a simple vector:
/// `#` past the level, `...` after the length. `element` renders element `i`.
fn vector_elements_string(count: usize, mut element: impl FnMut(usize) -> String) -> String {
    let depth = PRINT_DEPTH.with(|d| d.get());
    if print_level().is_some_and(|lvl| depth >= lvl) {
        return "#".to_string();
    }
    let limit = print_length();
    PRINT_DEPTH.with(|d| d.set(depth + 1));
    let mut s = String::from("#(");
    for i in 0..count {
        if i > 0 {
            s.push(' ');
        }
        if limit.is_some_and(|l| i >= l) {
            s.push_str("...");
            break;
        }
        s.push_str(&element(i));
    }
    s.push(')');
    PRINT_DEPTH.with(|d| d.set(depth));
    s
}

/// True iff `v` is the interpreter closure representation `(EGCL::CLOSURE . id)`
/// — a callable that must print as `#<FUNCTION>`. Reads only a genuine cons's
/// car (no sentinel dereference).
fn is_closure_cons(v: EgclVal) -> bool {
    if !v.is_cons() {
        return false;
    }
    let car = unsafe { (*(v.as_ptr() as *const egcl_rt::object::ConsCell)).car };
    // NIL and T report is_symbol() == true but have no symbol-table index
    // (as_symbol_index panics on them), so exclude them before indexing.
    if car == NIL || car == T || !car.is_symbol() {
        return false;
    }
    egcl_compiler::reader::symbol_name(car.as_symbol_index())
        .map(|n| n == "EGCL::CLOSURE")
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
/// `k` is the scale factor (default 1): a positive `k` puts `k` significant
/// digits before the decimal point and `d-k+1` after it, so `~,2,,2E` of 0.05
/// is `50.0e-3` and `~,2,,3E` is `500.e-4`; a `k` of zero or less puts `d`
/// digits after the point. `e` zero-pads the exponent to that many digits. The
/// exponent always carries a sign. Numbers are computed in f64 and rounded to `d`
/// fraction digits, so binary32 imprecision beyond `d` is discarded (no bliss-8zrb
/// regression on this path).
fn format_e_fixed(f: f64, d: usize, e: Option<usize>, k: i64, exp_char: char) -> String {
    let neg = f.is_sign_negative() && f != 0.0;
    let af = f.abs();
    let frac = if k > 0 {
        (d as i64 + 1 - k).max(0) as usize
    } else {
        d
    };
    let factor = 10f64.powi(frac as i32);
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
    let mut mant_str = format!("{:.*}", frac, mant);
    if frac == 0 {
        mant_str.push('.');
    }
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
/// The elements of a list. A cdr-circular list stops once the cycle is
/// detected, so a `~:<...~:>` or `~{...~}` over `#1=(0 . #1#)` cannot grow a
/// vector without bound (the printer chapter once died allocating 8 GB here).
fn cons_list_to_vec(v: EgclVal) -> Vec<EgclVal> {
    let mut result = Vec::new();
    let mut current = v;
    // Floyd's tortoise: advances one cons for every two the cursor takes.
    let mut tortoise = v;
    let mut step = false;
    while current.is_cons() {
        unsafe {
            let ptr = current.as_ptr() as *const egcl_rt::object::ConsCell;
            result.push((*ptr).car);
            current = (*ptr).cdr;
        }
        if step {
            unsafe {
                let ptr = tortoise.as_ptr() as *const egcl_rt::object::ConsCell;
                tortoise = (*ptr).cdr;
            }
        }
        step = !step;
        if current.is_cons() && current == tortoise {
            break;
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
/// (egcl-str-esc).
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

fn egclval_to_print_string(v: EgclVal, escapep: bool) -> String {
    egclval_to_print_string_with_dispatch(v, escapep, true)
}

fn egclval_to_print_string_with_dispatch(
    v: EgclVal,
    escapep: bool,
    dispatch_pretty: bool,
) -> String {
    // Printing circular structure without *PRINT-CIRCLE* is unbounded (CLHS
    // 22.1.3.5 leaves it undefined); stop at the native stack budget with a
    // marker rather than faulting through the guard page, which took the ansi
    // printer chapter down with it (print.cons.random.2 reported through rt).
    if egcl_rt::host_stack_budget_exhausted() {
        return "#<print depth exhausted>".to_string();
    }
    // Establish (at the outermost level) the *print-circle* label table, so
    // shared/circular structure prints with #N=/#N# instead of looping.
    circle_enter(v);
    let s = egclval_to_print_inner(v, escapep, dispatch_pretty);
    circle_exit();
    s
}

/// Print a single-float the way CL requires (CLHS 22.1.3.1.3): free format
/// while `10^-3 <= |x| < 10^7`, exponential notation outside that band, and
/// always with a decimal point so the result reads back as a float.
///
/// Rust's `{}` never switches to an exponent, so egcl used to print
/// `10000000000.0` for `1.0e10` and `0.0000000001` for `1.0e-10`. Both read
/// back correctly, but neither is the printed representation CL specifies.
///
/// This is the single implementation for BOTH printers — `format`'s
/// `egclval_to_print_inner` here and the interpreter's `print_val` — which
/// have drifted before (bliss-i608).
pub fn single_float_to_string(x: f32) -> String {
    if x.is_nan() || x.is_infinite() {
        return format!("{x}");
    }
    // A single float carries an explicit `f` marker when the default format
    // is a double format (CLHS 22.1.3.1.3; ansi print.single-float.3).
    let marker = if default_float_format_is_double() {
        'f'
    } else {
        'e'
    };
    let a = x.abs();
    if a != 0.0 && !(1e-3..1e7).contains(&a) {
        let s = format!("{x:e}");
        return match s.split_once('e') {
            Some((mantissa, exp)) if !mantissa.contains('.') => {
                format!("{mantissa}.0{marker}{exp}")
            }
            Some((mantissa, exp)) => format!("{mantissa}{marker}{exp}"),
            None => s,
        };
    }
    let s = format!("{x}");
    let s = if s.contains('.') { s } else { format!("{s}.0") };
    if marker == 'f' { format!("{s}f0") } else { s }
}

/// Print a DOUBLE-FLOAT in CL external syntax. Because egcl's
/// `*read-default-float-format*` is SINGLE-FLOAT, a double always carries an
/// explicit `d` exponent marker so it reads back as a double (CLHS 22.1.3.1.3):
/// `1.0d0`, `3.141592653589793d0`, `1.5d-10`. Kept in lockstep with
/// [`single_float_to_string`] so `prin1`/`~S` and the interpreter's `print_val`
/// spell doubles identically.
pub fn double_float_to_string(x: f64) -> String {
    if x.is_nan() || x.is_infinite() {
        return format!("{x}");
    }
    // A double prints without a marker when the default format is a double
    // format, else with `d` (CLHS 22.1.3.1.3; ansi print.double-float.*).
    let default_double = default_float_format_is_double();
    let a = x.abs();
    if a != 0.0 && !(1e-3..1e7).contains(&a) {
        let marker = if default_double { 'e' } else { 'd' };
        let s = format!("{x:e}");
        return match s.split_once('e') {
            Some((mantissa, exp)) if !mantissa.contains('.') => {
                format!("{mantissa}.0{marker}{exp}")
            }
            Some((mantissa, exp)) => format!("{mantissa}{marker}{exp}"),
            None => s,
        };
    }
    let s = format!("{x}");
    let s = if s.contains('.') { s } else { format!("{s}.0") };
    if default_double { s } else { format!("{s}d0") }
}

fn rational_format_value(rational: &egcl_rt::bignum::BigRat) -> f64 {
    let numerator = rational.num.to_f64();
    let denominator = rational.den.to_f64();
    if numerator.is_finite() && denominator.is_finite() {
        return numerator / denominator;
    }

    // Divide leading limbs before restoring their relative scale, so huge
    // components of a representable ratio do not turn into infinity/infinity.
    fn leading_limbs(integer: &egcl_rt::bignum::BigInt) -> (f64, usize) {
        let dropped = integer.mag.len().saturating_sub(2);
        let leading =
            egcl_rt::bignum::BigInt::from_parts(integer.sign as i32, &integer.mag[dropped..]);
        (leading.to_f64(), dropped)
    }
    let (numerator, numerator_scale) = leading_limbs(&rational.num);
    let (denominator, denominator_scale) = leading_limbs(&rational.den);
    let exponent =
        ((numerator_scale as i64 - denominator_scale as i64) * 64).clamp(-4096, 4096) as i32;
    // Split the exponent to keep the scale itself finite and nonzero, even
    // when the final quotient is near the overflow or subnormal boundary.
    let first_scale = exponent.clamp(-1022, 1023);
    (numerator / denominator * 2.0f64.powi(first_scale)) * 2.0f64.powi(exponent - first_scale)
}

fn real_format_value(val: EgclVal) -> Result<f64, EgclError> {
    if val.is_single_float() {
        Ok(val.as_single_float() as f64)
    } else if val.is_double_float() {
        Ok(val.as_double_float())
    } else if val.is_fixnum() {
        Ok(val.as_fixnum() as f64)
    } else if let Some(rational) = egcl_rt::bignum::as_bigrat(val) {
        let value = rational_format_value(&rational);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(EgclError::ArithmeticError(
                "rational argument exceeds the floating-point FORMAT range".into(),
            ))
        }
    } else {
        Err(EgclError::TypeError {
            datum: val,
            expected: "number".into(),
        })
    }
}

fn egclval_to_print_inner(mut v: EgclVal, escapep: bool, dispatch_pretty: bool) -> String {
    // Root V for the rest of the function. dispatch_pprint below calls a Lisp
    // pprint function through the dispatch hook: that allocates (the applicable
    // entry search, a string stream, the call itself) and so can fire a
    // relocating minor GC. The hook roots its OWN copy, which keeps the object
    // alive but leaves this one stale, and every test after the call -- is_nil,
    // is_instance, the heap-object header reads -- would then read a moved
    // object. Same shape as the condition that printed as #<HEAP-OBJECT> in
    // stdlib_print_object_hook (bliss-phgt).
    egcl_rt::rooted_ref!(_v_root = &mut v);
    // A *PRINT-PPRINT-DISPATCH* entry applying to V prints it instead
    // (only when *PRINT-PRETTY* is true and a table with entries is bound).
    if dispatch_pretty {
        if let Some(s) = dispatch_pprint(v, escapep) {
            return s;
        }
    }
    // Pretty-dispatch implementations own their logical-block marks. The
    // ordinary printer also labels arrays and uninterned symbols.
    if circle_active() && !v.is_cons() {
        if circle_array_length(v).is_some()
            && print_level().is_some_and(|level| PRINT_DEPTH.with(|depth| depth.get()) >= level)
        { return "#".into(); }
        match circle_visit(v) {
            CircleMark::Repeat(label) => return format!("#{label}#"),
            CircleMark::First(label) => return format!("#{label}={}", egclval_to_print_body(v, escapep)),
            CircleMark::NotShared => {}
        }
    }
    egclval_to_print_body(v, escapep)
}

fn egclval_to_print_body(mut v: EgclVal, escapep: bool) -> String {
    egcl_rt::rooted_ref!(_v_root = &mut v);
    if let Some(text) = concise_array_representation(v) {
        return text;
    }
    if let Some(text) = readable_array_representation(v) {
        return text;
    }
    if crate::random_state::random_state_p(v) {
        return random_state_representation(v, escapep);
    }
    if v.is_nil() {
        return print_symbol_name("NIL", escapep);
    }
    if v == T {
        return print_symbol_name("T", escapep);
    }
    // CLOS instances are heap objects; detect them via the liveness registry
    // before the generic heap-object branch. A condition prints as its report
    // message (~A/princ semantics); any other instance prints as #<CLASS-NAME>.
    if crate::clos::is_instance(v) {
        // A user `print-object` method wins when one applies; else fall back.
        //
        // Root V across the dispatch: the hook creates a string output stream
        // and searches for an applicable PRINT-OBJECT method, both of which
        // allocate, so a minor GC can relocate the instance while this local
        // copy is looking away. When the dispatch then declines (the common
        // case — no user method), the fallback below received a stale pointer,
        // `class_of` did not recognize it, and a perfectly good instance printed
        // as `#<HEAP-OBJECT>`: `(princ-to-string (make-instance 'foo))` answered
        // "#<FOO>" normally and "#<HEAP-OBJECT>" under EGCL_GC_STRESS=1, which
        // silently swallowed every condition report printed through FORMAT
        // (bliss-phgt).
        egcl_rt::rooted!(v = v);
        if let Some(s) = dispatch_print_object(*v, escapep) {
            return s;
        }
        return format_instance(*v, escapep);
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
            s.push_str(&character_name(c));
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
            egcl_compiler::reader::symbol_name(idx).unwrap_or_else(|| format!("SYM#{}", idx));
        // A keyword's registry name is `KEYWORD:FOO`; it must PRINT as `:FOO`
        // (both prin1/~S and princ/~A), which is what a reader — including a
        // SLIME/slynk client parsing the wire — round-trips back to the keyword.
        if let Some(bare) = name
            .strip_prefix("KEYWORD::")
            .or_else(|| name.strip_prefix("KEYWORD:"))
        {
            // prin1/~S prints the readable `:FOO`; princ/~A drops the marker.
            let bare = print_symbol_name(bare, escapep);
            return if escapep { format!(":{}", bare) } else { bare };
        }
        // An uninterned symbol (make-symbol/gensym, no home package) prints as
        // `#:NAME` under prin1/~S so it reads back as a fresh uninterned symbol
        // (CLHS 22.1.3.3, *print-gensym* default T); princ/~A drops the marker.
        // slynk's UNPARSE-NAME relies on this: `(subseq (prin1-to-string
        // (make-symbol s)) 2)` strips the `#:` — without it that subseq errors.
        if egcl_compiler::reader::is_uninterned(idx) {
            let name = print_symbol_name(&name, escapep);
            return if escapep && print_gensym() {
                format!("#:{}", name)
            } else {
                name
            };
        }
        return print_symbol_for_output(v, escapep);
    }
    // Reified function objects and legacy source closures share the same
    // opaque function representation (matches cli print_val).
    if egcl_rt::function::is_interpreted_function(v) || is_closure_cons(v) {
        return function_representation(v);
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
        if let Some(text) = pathname_representation(v, escapep) {
            return text;
        }
        // Check if it's a string and extract its content. This runs BEFORE the
        // number check because a registered-string sentinel is a pseudo-heap
        // value whose `as_ptr()` must not be dereferenced (extract_egcl_string
        // resolves it via the registry); a real string is consumed here too.
        if let Some(s) = extract_egcl_string(v) {
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
        if egcl_rt::types::bit_vector_p(v) {
            let len = egcl_rt::types::bit_vector_len(v).unwrap_or(0);
            let mut s = String::with_capacity(2 + len);
            s.push_str("#*");
            for i in 0..len {
                s.push(if egcl_rt::types::bit_vector_ref(v, i) == Some(1) {
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
        // Heap-backed opaque values (including packages) use the same host
        // PRINT-OBJECT hook as non-heap handles. V stays rooted if it declines.
        if let Some(text) = dispatch_print_object(v, escapep) {
            return text;
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

fn print_value_layout(output: &mut LayoutDoc, mut value: EgclVal, escapep: bool) -> LayoutDoc {
    // A declining pretty hook can allocate before the ordinary fallback.
    egcl_rt::rooted_ref!(_value_root = &mut value);
    if let Some(doc) = dispatch_pprint_layout(value, escapep, output.current_column()) {
        doc
    } else {
        let mut doc = LayoutDoc::default();
        doc.text(&egclval_to_print_string_with_dispatch(
            value, escapep, false,
        ));
        doc
    }
}

/// Print a CLOS instance. Under `princ`/`~A` a condition carrying a
/// `format-control` slot renders as its report message; every other instance
/// renders as `#<CLASS-NAME>`. Guarded against unbounded re-entry in case a
/// report control string references its own condition.
///
/// `escapep` matters: CLHS 9.1.3 gives the bare report ONLY when
/// *print-escape* is nil. Under prin1/~S a condition must print unreadably, so
/// the report is wrapped as `#<CLASS-NAME "report">` — SBCL prints
/// `#<SIMPLE-ERROR "boom" {addr}>`, and this is that minus the address. This
/// argument used not to exist, so prin1 of a condition returned the bare report
/// and `(ignore-errors (error "x"))` printed its second value as `x` rather
/// than as a condition object (bliss-egj6 sweep).
fn format_instance(v: EgclVal, escapep: bool) -> String {
    use std::cell::Cell;

    static DEPTH: egcl_rt::execution_local::ExecutionLocal<Cell<u32>> =
        unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(0) }) };

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
        EgclVal::from_symbol_index(egcl_compiler::reader::intern_symbol("FORMAT-CONTROL"));
    let result = match crate::clos::slot_value(v, control_sym) {
        Ok(control) => match extract_egcl_string(control) {
            Some(control_str) => {
                let args_sym = EgclVal::from_symbol_index(egcl_compiler::reader::intern_symbol(
                    "FORMAT-ARGUMENTS",
                ));
                let args = crate::clos::slot_value(v, args_sym)
                    .ok()
                    .map(cons_list_to_vec)
                    .unwrap_or_default();
                let report = if args.is_empty() {
                    control_str
                } else {
                    match format(NIL, &control_str, &args) {
                        Ok(formatted) => extract_egcl_string(formatted).unwrap_or(control_str),
                        Err(_) => control_str,
                    }
                };
                if escapep {
                    let tag = instance_class_tag(v);
                    // instance_class_tag is `#<CLASS-NAME>`; splice the report in
                    // before the closing bracket.
                    match tag.strip_suffix('>') {
                        Some(head) => format!("{head} {:?}>", report),
                        None => tag,
                    }
                } else {
                    report
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
fn format_struct(v: EgclVal, class: EgclVal) -> String {
    egcl_rt::rooted!(v = v);
    egcl_rt::rooted!(slots = crate::clos::effective_slots(class));
    let depth = PRINT_DEPTH.with(|depth| depth.get());
    let plan = structure_print_plan(depth, slots.len());
    if plan.level_exceeded { return "#".into(); }
    struct RestoreDepth(usize);
    impl Drop for RestoreDepth {
        fn drop(&mut self) { PRINT_DEPTH.with(|depth| depth.set(self.0)); }
    }
    let _depth = RestoreDepth(depth);
    PRINT_DEPTH.with(|current| current.set(depth + 1));
    let name = crate::clos::class_name(class);
    let mut out = String::from("#S(");
    out.push_str(&egclval_to_print_string(name, false));
    for slot_index in 0..plan.shown_slots {
        let slot = slots[slot_index];
        if let Ok(value) = crate::clos::slot_value(*v, slot) {
            egcl_rt::rooted!(value = value);
            out.push(' ');
            out.push(':');
            out.push_str(&egclval_to_print_string(slot, false));
            out.push(' ');
            out.push_str(&egclval_to_print_string(*value, true));
        }
    }
    if plan.ellipsis { out.push_str(" ..."); }
    out.push(')');
    out
}

/// `#<CLASS-NAME>` tag for an instance with no printable report.
///
/// A class metaobject names ITSELF as well as its metaclass —
/// `#<STANDARD-CLASS FOO>` — the way SBCL, CCL and ECL print one. Without
/// that, every standard class in the image printed as the identical text
/// `#<STANDARD-CLASS>`, which is useless in a backtrace or a slot dump and was
/// invisible before classes became objects: a class used to print as its own
/// name symbol (bliss-w53r3, bliss-rj5o).
///
/// `class_name` of a non-class answers NIL — the lookup is keyed by the class
/// value and an ordinary instance is not one — so this costs one registry
/// lookup on a path that already does two.
fn instance_class_tag(v: EgclVal) -> String {
    let class = crate::clos::class_of(v);
    let name = crate::clos::class_name(class);
    let own_name = crate::clos::class_name(v);
    if !own_name.is_nil() {
        return format!(
            "#<{} {}>",
            egclval_to_print_string(name, false),
            egclval_to_print_string(own_name, false)
        );
    }
    format!("#<{}>", egclval_to_print_string(name, false))
}

/// Print a cons/list the way `~A`/`~S` should: `(a b c)`, nested lists
/// recursively, and dotted tails as `(a . b)`. `escapep` propagates so `~S`
/// escapes strings and characters inside the list.
fn format_cons(v: EgclVal, escapep: bool) -> String {
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
    egcl_rt::rooted!(current = v);
    let mut count = 0usize;
    let mut first = true;
    loop {
        if current.is_cons() {
            // A shared/circular cons reached in the cdr position prints as a
            // dotted tail so its own #N=/#N# label appears (the head cons,
            // `first`, was already labelled above).
            if !first && circle && circle_is_shared(*current) {
                out.push_str(" . ");
                out.push_str(&egclval_to_print_string(*current, escapep));
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
                let ptr = current.as_ptr() as *const egcl_rt::object::ConsCell;
                if count > 0 {
                    out.push(' ');
                }
                out.push_str(&egclval_to_print_string((*ptr).car, escapep));
                // PRINT-OBJECT may allocate or suspend; reload the relocated cons.
                *current = (*(current.as_ptr() as *const egcl_rt::object::ConsCell)).cdr;
            }
            count += 1;
            first = false;
        } else if current.is_nil() {
            break;
        } else {
            out.push_str(" . ");
            out.push_str(&egclval_to_print_string(*current, escapep));
            break;
        }
    }
    out.push(')');
    out
}

/// Append `text` to `output`, left-padded with `padchar` to `mincol` columns.
fn push_padded_left(output: &mut LayoutDoc, text: &str, mincol: usize, padchar: char) {
    let len = text.chars().count();
    for _ in len..mincol {
        output.character(padchar);
    }
    output.text(text);
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
/// honor the same parameters (egcl-1kib).
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
    // The colon modifier groups digits in every radix (~:B, ~:X, ~:R too),
    // not only decimal.
    let _ = radix;
    let with_commas = if colon {
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

/// Extract `(negative, magnitude_digits)` from an integer EgclVal (fixnum or
/// bignum) in `radix`, or `None` if `val` is not an integer. Lets FORMAT ~D/~B/
/// ~O/~X render bignums, which used to type-error (only `is_fixnum()` was
/// accepted) even though ~A/PRINT already print them (egcl-1kib).
fn integer_magnitude(val: EgclVal, radix: u32) -> Option<(bool, String)> {
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

fn pad_layout_field(
    output: &mut LayoutDoc,
    doc: LayoutDoc,
    mincol: usize,
    colinc: usize,
    minpad: usize,
    padchar: char,
    at_sign: bool,
) {
    let len = doc.flat_string().chars().count();
    let mut pad = minpad;
    while len + pad < mincol {
        pad += colinc.max(1);
    }
    if at_sign {
        for _ in 0..pad {
            output.character(padchar);
        }
    }
    output.append(doc);
    if !at_sign {
        for _ in 0..pad {
            output.character(padchar);
        }
    }
}

/// A character prefix parameter (padchar, commachar), which `resolve_param`
/// delivers as a code point. Narrowing it with `as u8` kept only the low byte,
/// so `~5,'★D` padded with U+0005; only a value that is not a character at all
/// falls back to the directive's default.
fn param_char(code: i64, default: char) -> char {
    u32::try_from(code)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(default)
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

/// The short-scale names `~R` gives each group of three digits, as SBCL spells
/// them; anything past the vigintillions has no name and is a FORMAT error.
const CARDINAL_SCALES: [&str; 22] = [
    "",
    " thousand",
    " million",
    " billion",
    " trillion",
    " quadrillion",
    " quintillion",
    " sextillion",
    " septillion",
    " octillion",
    " nonillion",
    " decillion",
    " undecillion",
    " duodecillion",
    " tredecillion",
    " quattuordecillion",
    " quindecillion",
    " sexdecillion",
    " septendecillion",
    " octodecillion",
    " novemdecillion",
    " vigintillion",
];

/// English for 1..=999.
fn cardinal_below_thousand(mut v: usize) -> String {
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
    let mut result = String::new();
    if v >= 100 {
        result.push_str(ones[v / 100]);
        result.push_str(" hundred");
        v %= 100;
        if v > 0 {
            result.push(' ');
        }
    }
    if v >= 20 {
        result.push_str(tens[v / 10]);
        v %= 10;
        if v > 0 {
            result.push('-');
            result.push_str(ones[v]);
        }
    } else if v > 0 {
        result.push_str(ones[v]);
    }
    result
}

/// `~R` English for an integer given as its sign and decimal digits, so a
/// bignum spells out like a fixnum. Each three-digit group is named by
/// [`CARDINAL_SCALES`]; `None` means the number is too large to name.
fn cardinal(negative: bool, digits: &str) -> Option<String> {
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some("zero".into());
    }
    let groups = digits.len().div_ceil(3);
    if groups > CARDINAL_SCALES.len() {
        return None;
    }
    let mut parts = Vec::with_capacity(groups);
    let mut start = 0;
    let mut len = digits.len() - (groups - 1) * 3;
    for scale in CARDINAL_SCALES[..groups].iter().rev() {
        let chunk: usize = digits[start..start + len].parse().ok()?;
        if chunk > 0 {
            parts.push(format!("{}{}", cardinal_below_thousand(chunk), scale));
        }
        start += len;
        len = 3;
    }
    let body = parts.join(" ");
    Some(if negative {
        format!("negative {body}")
    } else {
        body
    })
}

fn ordinal(c: String) -> String {
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
    // Four thousands print as MMMM (ansi format.r.19/20 go to 4999).
    if n <= 0 || n > 4999 {
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

/// Return EGCL's preferred reader-compatible name for a character, or the
/// character itself when it has no name.  The implementation-defined names
/// match SBCL for compatibility with libraries such as cl-str (bliss-rr9q).
pub fn character_name(c: char) -> String {
    // CLHS 22.1.3.2: a graphic character prints as itself; a non-graphic one
    // prints by name.
    //
    // Every WHITESPACE character prints by name, not just Space. A literal
    // NO-BREAK SPACE (U+00A0) or IDEOGRAPHIC SPACE (U+3000) in the output is
    // indistinguishable from a plain space, so PRIN1 would not read back to the
    // same character -- and `graphic_char_p` accepts both, being true for
    // everything from U+00A0 up. SBCL prints #\NO-BREAK_SPACE and
    // #\IDEOGRAPHIC_SPACE for them (bliss-nbsp-name). A whitespace character
    // with no name in the table still falls through and prints as itself.
    if !c.is_whitespace() && graphic_char_p(c) {
        return c.to_string();
    }
    egcl_compiler::reader::char_name(c).unwrap_or_else(|| c.to_string())
}

/// GRAPHIC-CHAR-P as the printer sees it: the printing ASCII characters and
/// everything from U+00A0 up (the C0 and C1 controls are not graphic).
pub fn graphic_char_p(c: char) -> bool {
    let code = c as u32;
    (32..127).contains(&code) || code >= 160
}

// ── Directive parser ──────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Param {
    Num(i64),
    /// A quoted character parameter (`~'X`), kept as a character so a
    /// `~/name/` function receives it as one; numeric sites use its code.
    Char(char),
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
fn parse_directives(control: &str) -> Result<Vec<Directive>, EgclError> {
    let chars: Vec<char> = control.chars().collect();
    let mut directives = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '~' {
            let start = i;
            i += 1;
            if i >= chars.len() {
                return Err(EgclError::Internal("dangling ~ in format string".into()));
            }
            let mut params = Vec::new();
            // Parse parameters
            loop {
                if i >= chars.len() {
                    return Err(EgclError::Internal("dangling ~ in format string".into()));
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
                        params.push(Param::Char(chars[i]));
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
                    // A literal too large for an i64 saturates: it is still "out
                    // of range" for ~[ and "pad everything" for widths.
                    params.push(Param::Num(num_str.parse::<i64>().unwrap_or(
                        if num_str.starts_with('-') {
                            i64::MIN
                        } else {
                            i64::MAX
                        },
                    )));
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
                return Err(EgclError::Internal("dangling ~ in format string".into()));
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

// ── Queued pretty-layout engine ───────────────────────────────────

/// An owned pretty-printing document.  FORMAT returns this intermediate form
/// to callers that need to splice its directives into an enclosing logical
/// block (notably the evaluator's Gray pretty stream) without flattening them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutDoc {
    events: Vec<LayoutEvent>,
    start_column: usize,
}

impl LayoutDoc {
    pub fn from_events(events: Vec<LayoutEvent>) -> Self {
        Self {
            events,
            start_column: 0,
        }
    }

    pub fn events(&self) -> &[LayoutEvent] {
        &self.events
    }

    pub fn into_events(self) -> Vec<LayoutEvent> {
        self.events
    }

    fn append(&mut self, mut other: LayoutDoc) {
        if let Some(LayoutEvent::Text(first)) = other.events.first_mut()
            && let Some(LayoutEvent::Text(last)) = self.events.last_mut()
        {
            last.push_str(first);
            other.events.remove(0);
        }
        self.events.extend(other.events);
    }

    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(LayoutEvent::Text(last)) = self.events.last_mut() {
            last.push_str(text);
        } else {
            self.events.push(LayoutEvent::Text(text.to_string()));
        }
    }

    fn character(&mut self, character: char) {
        if let Some(LayoutEvent::Text(last)) = self.events.last_mut() {
            last.push(character);
        } else {
            self.events.push(LayoutEvent::Text(character.to_string()));
        }
    }

    fn flat_string(&self) -> String {
        let options = LayoutOptions {
            pretty: false,
            ..LayoutOptions::default()
        };
        render_layout(self, &options)
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn ends_with(&self, suffix: char) -> bool {
        self.flat_string().ends_with(suffix)
    }

    fn current_column(&self) -> usize {
        // A callback observes the speculative queued column: conditional
        // newlines are assumed not to break yet, but a mandatory newline does
        // reset the column whenever pretty printing is active.  Rendering with
        // an effectively unbounded margin gives exactly that behavior while
        // retaining tabs and nested blocks' column rules.
        let options = LayoutOptions {
            margin: 1_000_000_000,
            miser_width: None,
            line_limit: None,
            start_column: self.start_column,
            pretty: print_pretty_active(),
        };
        let rendered = render_layout(self, &options);
        let last_line = rendered.rsplit('\n').next().unwrap_or_default();
        if rendered.contains('\n') {
            last_line.chars().count()
        } else {
            self.start_column + last_line.chars().count()
        }
    }
}

struct PrintDynamicBinding {
    index: u32,
    saved: Option<EgclVal>,
}

impl PrintDynamicBinding {
    fn establish(name: &str, value: EgclVal) -> Option<Self> {
        let index = egcl_rt::symbols::find_index(name)?;
        let saved = egcl_rt::symbols::bind_symbol_value(index, value);
        Some(Self { index, saved })
    }
}

impl egcl_rt::gc::TraceHostRoots for PrintDynamicBinding {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(saved) = &mut self.saved {
            visit(saved);
        }
    }
}

impl Drop for PrintDynamicBinding {
    fn drop(&mut self) {
        egcl_rt::symbols::restore_symbol_binding(self.index, self.saved);
    }
}

/// PRINC and FORMAT's aesthetic operations bind actual Lisp-visible specials.
/// Root saved bindings: generalized booleans may be movable heap values.
pub fn with_princ_bindings<R>(body: impl FnOnce() -> R) -> R {
    egcl_rt::rooted!(_escape = PrintDynamicBinding::establish("*PRINT-ESCAPE*", NIL));
    egcl_rt::rooted!(_readably = PrintDynamicBinding::establish("*PRINT-READABLY*", NIL));
    body()
}

/// A semantic operation in a queued pretty-print document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutEvent {
    Text(String),
    CaseStart(CaseMode),
    CaseEnd,
    Block {
        prefix: String,
        per_line_prefix: Option<String>,
        suffix: String,
        events: Vec<LayoutEvent>,
    },
    Newline(NewlineKind),
    Indent {
        relative_to: IndentKind,
        amount: i32,
    },
    Tab {
        kind: TabKind,
        colnum: u32,
        colinc: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentKind {
    Block,
    Current,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMode {
    Lower,
    Upper,
    CapitalizeWords,
    CapitalizeFirst,
}

/// Dynamic parameters needed to resolve a queued layout document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    pub margin: usize,
    pub miser_width: Option<usize>,
    pub line_limit: Option<usize>,
    pub start_column: usize,
    pub pretty: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            margin: 80,
            miser_width: None,
            line_limit: None,
            start_column: 0,
            pretty: true,
        }
    }
}

struct LayoutState<'a> {
    options: &'a LayoutOptions,
    output: String,
    column: usize,
    pending_spaces: usize,
    lines: usize,
    prefixes: Vec<(usize, String)>,
    stopped: bool,
    limit_enabled: bool,
    cases: Vec<CaseState>,
}

struct CaseState {
    mode: CaseMode,
    word_start: bool,
    first_seen: bool,
}

impl CaseState {
    fn new(mode: CaseMode) -> Self {
        Self {
            mode,
            word_start: true,
            first_seen: false,
        }
    }

    fn character(&mut self, character: char) -> char {
        match self.mode {
            CaseMode::Lower => crate::characters::char_downcase(character),
            CaseMode::Upper => crate::characters::char_upcase(character),
            CaseMode::CapitalizeWords => {
                if !character.is_alphanumeric() {
                    self.word_start = true;
                    character
                } else if self.word_start {
                    self.word_start = false;
                    crate::characters::char_upcase(character)
                } else {
                    crate::characters::char_downcase(character)
                }
            }
            CaseMode::CapitalizeFirst => {
                if !self.first_seen && character.is_alphabetic() {
                    self.first_seen = true;
                    crate::characters::char_upcase(character)
                } else if self.first_seen {
                    crate::characters::char_downcase(character)
                } else {
                    character
                }
            }
        }
    }
}

impl LayoutState<'_> {
    fn flush_spaces(&mut self) {
        self.output
            .extend(std::iter::repeat_n(' ', self.pending_spaces));
        self.pending_spaces = 0;
    }

    fn character(&mut self, character: char) {
        if character == '\n' {
            self.newline(0, false);
        } else {
            let character = self
                .cases
                .iter_mut()
                .rev()
                .fold(character, |character, state| state.character(character));
            self.column += 1;
            if character == ' ' {
                self.pending_spaces += 1;
            } else {
                self.flush_spaces();
                self.output.push(character);
            }
        }
    }

    fn text(&mut self, text: &str) {
        for character in text.chars() {
            self.character(character);
        }
    }

    fn newline(&mut self, indent: usize, trim: bool) {
        if self.stopped {
            return;
        }
        if trim {
            self.pending_spaces = 0;
        } else {
            self.flush_spaces();
        }
        if self.limit_enabled
            && self
                .options
                .line_limit
                .is_some_and(|limit| self.lines + 1 >= limit)
        {
            self.output.push_str(" ..");
            self.stopped = true;
            return;
        }
        for case in self.cases.iter_mut().rev() {
            case.character('\n');
        }
        self.output.push('\n');
        self.lines += 1;
        self.column = 0;
        let prefixes = self.prefixes.clone();
        for (column, prefix) in prefixes {
            while self.column < column {
                self.character(' ');
            }
            self.text(&prefix);
        }
        while self.column < indent {
            self.character(' ');
        }
    }

    fn tab(&mut self, kind: TabKind, colnum: usize, colinc: usize, section: usize) {
        let (origin, relative) = match kind {
            TabKind::Line => (0, false),
            TabKind::Section => (section, false),
            TabKind::LineRelative => (0, true),
            TabKind::SectionRelative => (section, true),
        };
        let current = self.column.saturating_sub(origin);
        let spaces = if relative {
            let target = current + colnum;
            colnum
                + if colinc > 0 && target % colinc != 0 {
                    colinc - target % colinc
                } else {
                    0
                }
        } else if current < colnum {
            colnum - current
        } else if colinc > 0 {
            colinc - ((current - colnum) % colinc)
        } else {
            0
        };
        for _ in 0..spaces {
            self.character(' ');
        }
    }
}

fn flat_end_position(
    events: &[LayoutEvent],
    mut column: usize,
    mut section: usize,
) -> Option<(usize, usize)> {
    for event in events {
        match event {
            LayoutEvent::Text(text) => {
                for character in text.chars() {
                    if character == '\n' {
                        return None;
                    }
                    column += 1;
                }
            }
            LayoutEvent::CaseStart(_) | LayoutEvent::CaseEnd => {}
            LayoutEvent::Block {
                prefix,
                suffix,
                events,
                ..
            } => {
                for character in prefix.chars() {
                    if character == '\n' {
                        return None;
                    }
                    column += 1;
                }
                column = flat_end_position(events, column, column)?.0;
                for character in suffix.chars() {
                    if character == '\n' {
                        return None;
                    }
                    column += 1;
                }
            }
            LayoutEvent::Newline(NewlineKind::Mandatory) => return None,
            LayoutEvent::Newline(_) => section = column,
            LayoutEvent::Indent { .. } => {}
            LayoutEvent::Tab {
                kind,
                colnum,
                colinc,
            } => {
                let (origin, relative) = match kind {
                    TabKind::Line => (0, false),
                    TabKind::Section => (section, false),
                    TabKind::LineRelative => (0, true),
                    TabKind::SectionRelative => (section, true),
                };
                let current = column.saturating_sub(origin);
                let colnum = *colnum as usize;
                let colinc = *colinc as usize;
                column += if relative {
                    let target = current + colnum;
                    colnum
                        + if colinc > 0 && target % colinc != 0 {
                            colinc - target % colinc
                        } else {
                            0
                        }
                } else if current < colnum {
                    colnum - current
                } else if colinc > 0 {
                    colinc - ((current - colnum) % colinc)
                } else {
                    0
                };
            }
        }
    }
    Some((column, section))
}

fn flat_end_column(events: &[LayoutEvent], column: usize, section: usize) -> Option<usize> {
    flat_end_position(events, column, section).map(|position| position.0)
}

/// Flat column at the next conditional-newline boundary.  The boolean says
/// the event slice ended, in which case the enclosing suffix belongs to this
/// section's fit calculation.
fn section_end_column(
    events: &[LayoutEvent],
    mut column: usize,
    section: usize,
) -> Option<(usize, bool)> {
    for event in events {
        match event {
            LayoutEvent::Text(text) => {
                for character in text.chars() {
                    if character == '\n' {
                        return Some((column, false));
                    }
                    column += 1;
                }
            }
            LayoutEvent::Block {
                prefix,
                suffix,
                events,
                ..
            } => {
                for character in prefix.chars() {
                    if character == '\n' {
                        return Some((column, false));
                    }
                    column += 1;
                }
                column = flat_end_column(events, column, column)?;
                for character in suffix.chars() {
                    if character == '\n' {
                        return Some((column, false));
                    }
                    column += 1;
                }
            }
            LayoutEvent::CaseStart(_) | LayoutEvent::CaseEnd => {}
            LayoutEvent::Newline(_) => return Some((column, false)),
            LayoutEvent::Indent { .. } => {}
            LayoutEvent::Tab {
                kind,
                colnum,
                colinc,
            } => {
                let doc = LayoutDoc::from_events(vec![LayoutEvent::Tab {
                    kind: *kind,
                    colnum: *colnum,
                    colinc: *colinc,
                }]);
                column = flat_end_column(doc.events(), column, section)?;
            }
        }
    }
    Some((column, true))
}

fn render_events(events: &[LayoutEvent], state: &mut LayoutState<'_>, section: usize) {
    let enclosing_case_depth = state.cases.len();
    for event in events {
        if state.stopped {
            break;
        }
        match event {
            LayoutEvent::Text(text) => state.text(text),
            LayoutEvent::CaseStart(mode) => {
                state.cases.push(CaseState::new(*mode));
            }
            LayoutEvent::CaseEnd => {
                state.cases.pop();
            }
            LayoutEvent::Block {
                prefix,
                per_line_prefix,
                suffix,
                events,
            } => render_block(prefix, per_line_prefix.as_deref(), suffix, events, state),
            LayoutEvent::Newline(NewlineKind::Mandatory) if state.options.pretty => {
                state.newline(section, true)
            }
            // Conditional directives outside a logical block are inert, as
            // are all PPRINT-NEWLINE kinds when pretty printing is disabled.
            LayoutEvent::Newline(_) | LayoutEvent::Indent { .. } => {}
            LayoutEvent::Tab {
                kind,
                colnum,
                colinc,
            } if matches!(kind, TabKind::Line | TabKind::LineRelative) => {
                state.tab(*kind, *colnum as usize, *colinc as usize, section)
            }
            LayoutEvent::Tab { .. } => {}
        }
    }
    state.cases.truncate(enclosing_case_depth);
}

fn render_block(
    prefix: &str,
    per_line_prefix: Option<&str>,
    suffix: &str,
    events: &[LayoutEvent],
    state: &mut LayoutState<'_>,
) {
    let enclosing_case_depth = state.cases.len();
    let enclosing_limit_enabled = state.limit_enabled;
    state.limit_enabled = true;
    let start = state.column;
    state.text(prefix);
    let base = state.column;
    if let Some(per_line_prefix) = per_line_prefix {
        state.prefixes.push((start, per_line_prefix.to_string()));
    }
    let multiline_delimiter = prefix.contains('\n') || suffix.contains('\n');
    let linear = multiline_delimiter
        || flat_end_column(events, base, base)
            .and_then(|column| column.checked_add(suffix.chars().count()))
            .is_none_or(|column| column > state.options.margin);
    let miser = state
        .options
        .miser_width
        .is_some_and(|width| state.options.margin.saturating_sub(base) <= width);
    let mut indent = base;
    let mut section_line = state.lines;
    let mut section = base;

    for (index, event) in events.iter().enumerate() {
        if state.stopped {
            break;
        }
        match event {
            LayoutEvent::Text(text) => state.text(text),
            LayoutEvent::CaseStart(mode) => {
                state.cases.push(CaseState::new(*mode));
            }
            LayoutEvent::CaseEnd => {
                state.cases.pop();
            }
            LayoutEvent::Block {
                prefix,
                per_line_prefix,
                suffix,
                events,
            } => render_block(prefix, per_line_prefix.as_deref(), suffix, events, state),
            LayoutEvent::Indent {
                relative_to,
                amount,
            } if state.options.pretty && !miser => {
                let origin = match relative_to {
                    IndentKind::Block => base,
                    IndentKind::Current => state.column,
                };
                indent = origin.saturating_add_signed(*amount as isize);
            }
            LayoutEvent::Indent { .. } => {}
            LayoutEvent::Newline(kind) => {
                let should_break = if !state.options.pretty {
                    false
                } else {
                    match kind {
                        NewlineKind::Mandatory => true,
                        NewlineKind::Linear => linear,
                        NewlineKind::Miser => miser,
                        NewlineKind::Fill if miser => linear,
                        NewlineKind::Fill => {
                            state.lines > section_line
                                || section_end_column(&events[index + 1..], state.column, section)
                                    .and_then(|(column, at_end)| {
                                        if at_end && suffix.contains('\n') {
                                            None
                                        } else if at_end {
                                            column.checked_add(suffix.chars().count())
                                        } else {
                                            Some(column)
                                        }
                                    })
                                    .is_none_or(|column| column > state.options.margin)
                        }
                    }
                };
                if should_break {
                    state.newline(if state.options.pretty { indent } else { 0 }, true);
                }
                section_line = state.lines;
                section = state.column;
            }
            LayoutEvent::Tab {
                kind,
                colnum,
                colinc,
            } if state.options.pretty || matches!(kind, TabKind::Line | TabKind::LineRelative) => {
                state.tab(*kind, *colnum as usize, *colinc as usize, section)
            }
            LayoutEvent::Tab { .. } => {}
        }
    }
    state.cases.truncate(enclosing_case_depth);
    // A line limit truncates the body, but every open block must still write
    // its suffix in full (CLHS *PRINT-LINES*). Suffix newlines themselves do
    // not trigger a second truncation marker.
    let stopped = state.stopped;
    state.stopped = false;
    state.limit_enabled = false;
    state.text(suffix);
    if per_line_prefix.is_some() {
        state.prefixes.pop();
    }
    state.stopped = stopped;
    state.limit_enabled = enclosing_limit_enabled;
}

/// Resolve an owned layout document using the supplied printer parameters.
pub fn render_layout(doc: &LayoutDoc, options: &LayoutOptions) -> String {
    let mut state = LayoutState {
        options,
        output: String::new(),
        column: options.start_column,
        pending_spaces: 0,
        lines: 0,
        prefixes: Vec::new(),
        stopped: false,
        limit_enabled: false,
        cases: Vec::new(),
    };
    render_events(doc.events(), &mut state, options.start_column);
    state.flush_spaces();
    state.output
}

// ── Main format engine ────────────────────────────────────────────

// Number of arguments the most recent top-level `format` call consumed.
// FORMATTER functions return the unconsumed tail of their arguments
// (CLHS 22.2.1.3), which only the directive interpreter knows.
thread_local! {
    static LAST_CONSUMED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Raised by `~^` (1) or `~:^` (2).  `format_impl` returns at its next
    /// step, and the enclosing iteration, `~?` or `~<` decides what ends.
    static FORMAT_ESCAPE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Inside `~:{` / `~:@{`: how many sublists remain after the current
    /// pass, which is what a parameterless `~:^` tests.
    static OUTER_REMAINING: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// How many arguments the last completed `format` call consumed.
pub fn last_consumed_arg_count() -> usize {
    LAST_CONSUMED.with(|c| c.get())
}

/// Execute a FORMAT directive string. R5.40.
/// Clear and return the pending `~^` escape level (0 when none).
fn take_escape() -> u8 {
    FORMAT_ESCAPE.with(|c| c.replace(0))
}

fn escape_pending() -> bool {
    FORMAT_ESCAPE.with(|c| c.get()) != 0
}

pub fn format(
    destination: EgclVal,
    control_string: &str,
    args: &[EgclVal],
) -> Result<EgclVal, EgclError> {
    let mut destination = destination;
    egcl_rt::rooted_ref!(_destination_root = &mut destination);
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
            if !egcl_rt::types::stringp(destination) {
                return Err(EgclError::TypeError {
                    datum: destination,
                    expected: "stream or string-with-fill-pointer".into(),
                });
            }
        } else {
            return Err(EgclError::TypeError {
                datum: destination,
                expected: "NIL, T, stream, or string-with-fill-pointer".into(),
            });
        }
    }

    let start_column = if to_stream {
        crate::streams::stream_line_column(destination).unwrap_or(0) as usize
    } else {
        0
    };
    let doc = format_to_layout_at_column(control_string, args, start_column)?;
    let options = LayoutOptions {
        margin: print_integer_variable("*PRINT-RIGHT-MARGIN*").unwrap_or(80),
        miser_width: print_integer_variable("*PRINT-MISER-WIDTH*"),
        line_limit: (!print_readably_active())
            .then(|| print_integer_variable("*PRINT-LINES*"))
            .flatten(),
        start_column,
        pretty: print_pretty_active(),
    };
    let output = render_layout(&doc, &options);

    if to_stdout {
        print!("{}", output);
        Ok(NIL)
    } else if to_string {
        // FORMAT NIL returns a fresh string (CLHS 22.3). An interned string
        // lived off-heap forever, leaking every distinct result (bliss-4due).
        Ok(crate::streams::make_lisp_string_fresh(&output))
    } else if to_stream {
        // Write each character to the stream using the Gray streams API.
        for ch in output.chars() {
            crate::streams::stream_write_char(destination, EgclVal::from_char(ch))?;
        }
        Ok(NIL)
    } else if destination.is_heap_object() {
        // String with fill pointer — append formatted output in place.
        // We refuse to grow beyond the original allocation because realloc
        // may move the underlying buffer and EgclVal is a value type
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
                return Err(EgclError::Internal(format!(
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

fn print_integer_variable(name: &str) -> Option<usize> {
    egcl_rt::symbols::find_index(name)
        .and_then(egcl_rt::symbols::symbol_value)
        .filter(|value| value.is_fixnum() && value.as_fixnum() >= 0)
        .map(|value| value.as_fixnum() as usize)
}

/// Parse and execute FORMAT into semantic layout events without resolving line
/// breaks.  The evaluator uses this to splice FORMAT into an enclosing Lisp
/// pretty stream instead of flattening conditional directives into text.
pub fn format_to_layout(control_string: &str, args: &[EgclVal]) -> Result<LayoutDoc, EgclError> {
    format_to_layout_at_column(control_string, args, 0)
}

pub fn format_to_layout_at_column(
    control_string: &str,
    args: &[EgclVal],
    start_column: usize,
) -> Result<LayoutDoc, EgclError> {
    let mut args = args.to_vec();
    egcl_rt::rooted_ref!(_args_root = &mut args);
    validate_matching(control_string)?;
    if control_has_overflow_justification(control_string) {
        if let Some(d) = clause_directive_among(control_string, &['W', 'I', '_']) {
            return Err(EgclError::ControlError(format!(
                "~{d} is not allowed in a control string with a ~<...~:;...~> justification"
            )));
        }
    }

    let mut output = LayoutDoc {
        events: Vec::new(),
        start_column,
    };
    let mut arg_idx = 0;
    take_escape();
    let result = format_impl(control_string, &args, &mut arg_idx, &mut output);
    take_escape();
    if let Err(error) = &result {
        if std::env::var_os("EGCL_FMT_DBG").is_some() {
            eprintln!(
                ";; fmt TOP error={} control={:?} nargs={}",
                error,
                control_string.chars().take(120).collect::<String>(),
                args.len()
            );
        }
    }
    result?;
    LAST_CONSUMED.with(|cell| cell.set(arg_idx.min(args.len())));
    Ok(output)
}

fn validate_matching(control: &str) -> Result<(), EgclError> {
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
                return Err(EgclError::Internal("dangling ~ in format string".into()));
            }
            match chars[i] {
                '{' => stack.push('}'),
                '}' => {
                    if stack.pop() != Some('}') {
                        return Err(EgclError::Internal("unmatched ~}".into()));
                    }
                }
                '[' => stack.push(']'),
                ']' => {
                    if stack.pop() != Some(']') {
                        return Err(EgclError::Internal("unmatched ~]".into()));
                    }
                }
                '(' => stack.push(')'),
                ')' => {
                    if stack.pop() != Some(')') {
                        return Err(EgclError::Internal("unmatched ~)".into()));
                    }
                }
                '<' => stack.push('>'),
                '>' => {
                    if stack.pop() != Some('>') {
                        return Err(EgclError::Internal("unmatched ~>".into()));
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
        return Err(EgclError::Internal(format!("unmatched {}", unmatched)));
    }
    Ok(())
}

fn format_impl(
    control: &str,
    args: &[EgclVal],
    arg_idx: &mut usize,
    output: &mut LayoutDoc,
) -> Result<(), EgclError> {
    let chars: Vec<char> = control.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if escape_pending() {
            return Ok(());
        }
        if chars[i] != '~' {
            output.character(chars[i]);
            i += 1;
            continue;
        }
        i += 1; // skip ~
        if i >= chars.len() {
            return Err(EgclError::Internal("dangling ~ in format string".into()));
        }
        // Parse params
        let mut params: Vec<Param> = Vec::new();
        loop {
            if i >= chars.len() {
                return Err(EgclError::Internal("dangling ~".into()));
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
                    params.push(Param::Char(chars[i]));
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
                // A literal too large for an i64 saturates: it is still "out
                // of range" for ~[ and "pad everything" for widths.
                params.push(Param::Num(num_str.parse::<i64>().unwrap_or(
                    if num_str.starts_with('-') {
                        i64::MIN
                    } else {
                        i64::MAX
                    },
                )));
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
            return Err(EgclError::Internal("dangling ~".into()));
        }
        let directive = chars[i].to_ascii_uppercase();
        i += 1;

        let resolve_param = |p: &Param, default: i64, aidx: &mut usize| -> Result<i64, EgclError> {
            match p {
                Param::Num(n) => Ok(*n),
                Param::Char(c) => Ok(*c as i64),
                Param::V => {
                    if *aidx >= args.len() {
                        return Err(EgclError::ControlError("too few args for V param".into()));
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
                    } else if let Some((negative, _)) = integer_magnitude(v, 10) {
                        // A bignum parameter is a legal integer; clamp it, so a
                        // hugely negative MINCOL pads nothing (ansi format.d.29).
                        Ok(if negative { i64::MIN } else { i64::MAX })
                    } else {
                        Err(EgclError::TypeError {
                            datum: v,
                            expected: "integer".into(),
                        })
                    }
                }
                // `#` counts the arguments remaining when IT is resolved, so
                // a `v` before it has already taken its argument
                // (ansi format.:^.:{.20: `~V,#:^`).
                Param::Hash => Ok(args.len().saturating_sub(*aidx) as i64),
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
                let colinc = params
                    .get(1)
                    .map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?
                    .max(1) as usize;
                let minpad = params
                    .get(2)
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let padchar = match params.get(3) {
                    Some(p) => {
                        char::from_u32(resolve_param(p, ' ' as i64, arg_idx)? as u32).unwrap_or(' ')
                    }
                    None => ' ',
                };
                if *arg_idx >= args.len() {
                    if std::env::var_os("EGCL_FMT_DBG").is_some() {
                        eprintln!(
                            ";; fmt too-few ~~A: arg_idx={} nargs={} control={:?}",
                            arg_idx,
                            args.len(),
                            control.chars().take(80).collect::<String>()
                        );
                    }
                    return Err(EgclError::ControlError("too few args for ~A".into()));
                }
                let mut val = args[*arg_idx];
                egcl_rt::rooted_ref!(_val_root = &mut val);
                *arg_idx += 1;
                let doc = with_princ_bindings(|| {
                    if colon && val.is_nil() {
                        let mut doc = LayoutDoc::default();
                        doc.text("()");
                        doc
                    } else {
                        print_value_layout(output, val, false)
                    }
                });
                pad_layout_field(output, doc, mincol, colinc, minpad, padchar, at_sign);
            }
            'S' => {
                // ~mincol,colinc,minpad,padchar S — same padding as ~A (bliss-znib).
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
                let padchar = match params.get(3) {
                    Some(p) => {
                        char::from_u32(resolve_param(p, ' ' as i64, arg_idx)? as u32).unwrap_or(' ')
                    }
                    None => ' ',
                };
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~S".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let doc = if colon && val.is_nil() {
                    let mut doc = LayoutDoc::default();
                    doc.text("()");
                    doc
                } else {
                    print_value_layout(output, val, true)
                };
                pad_layout_field(output, doc, mincol, colinc, minpad, padchar, at_sign);
            }
            'D' | 'B' | 'O' | 'X' => {
                let radix = match directive {
                    'B' => 2,
                    'O' => 8,
                    'X' => 16,
                    _ => 10,
                };
                // Resolve V/# params BEFORE consuming the main argument
                // A negative mincol pads nothing (FORMAT.X.28): `as usize` on a
                // negative i64 wrapped to a huge width and panicked in
                // repeat_n with "capacity overflow".
                let mincol = if !params.is_empty() {
                    resolve_param(&params[0], 0, arg_idx)?.max(0) as usize
                } else {
                    0
                };
                let padchar = if params.len() > 1 {
                    param_char(resolve_param(&params[1], ' ' as i64, arg_idx)?, ' ')
                } else {
                    ' '
                };
                // Third/fourth ~D params are the comma character and comma
                // interval (CLHS 22.3.2.1); `~,,' ,4:d` groups every 4 digits
                // with a space. Previously both were ignored (hardcoded ",", 3).
                let commachar = if params.len() > 2 {
                    param_char(resolve_param(&params[2], ',' as i64, arg_idx)?, ',')
                } else {
                    ','
                };
                let comma_interval = if params.len() > 3 {
                    resolve_param(&params[3], 3, arg_idx)?.max(0) as usize
                } else {
                    3
                };
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError(format!(
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
                        // CLHS 22.3.2.2: a non-integer prints as by ~A in the
                        // directive's radix, padded on the left to MINCOL.
                        push_non_integer_in_radix(output, val, radix, mincol, padchar);
                        continue;
                    }
                };
                output.text(&format_integer_parts(
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
                // An omitted radix, or a `v` whose argument is NIL, selects the
                // English / roman spellings instead (ansi format.r.8).
                let radix_given = match params.first() {
                    None | Some(Param::None) => false,
                    Some(Param::V) => !args.get(*arg_idx).is_some_and(|v| v.is_nil()),
                    Some(_) => true,
                };
                let radix_param = if radix_given {
                    let radix = resolve_param(&params[0], 10, arg_idx)?;
                    // CLHS 22.3.2.2: radix is an integer between 2 and 36.
                    // Out-of-range values used to reach from_digit / `% 0` and
                    // panic; signal a FORMAT error instead.
                    if !(2..=36).contains(&radix) {
                        return Err(EgclError::ControlError(format!(
                            "~R radix must be between 2 and 36, got {}",
                            radix
                        )));
                    }
                    Some(radix as u32)
                } else {
                    // An omitted (or NIL `v`) radix: still consume the `v`
                    // argument, then print in English or roman numerals.
                    if let Some(p @ Param::V) = params.first() {
                        resolve_param(p, 10, arg_idx)?;
                    }
                    None
                };
                // ~radix,mincol,padchar,commachar,comma-intervalR: the
                // remaining prefix parameters mean what they do for ~D.
                let mincol = params
                    .get(1)
                    .map_or(Ok(0), |p| resolve_param(p, 0, arg_idx))?
                    .max(0) as usize;
                let padchar = params
                    .get(2)
                    .map_or(Ok(' ' as i64), |p| resolve_param(p, ' ' as i64, arg_idx))
                    .map(|code| param_char(code, ' '))?;
                let commachar = params
                    .get(3)
                    .map_or(Ok(',' as i64), |p| resolve_param(p, ',' as i64, arg_idx))
                    .map(|code| param_char(code, ','))?;
                let comma_interval = params
                    .get(4)
                    .map_or(Ok(3), |p| resolve_param(p, 3, arg_idx))?
                    .max(0) as usize;
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~R".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                if let Some(radix) = radix_param {
                    // Fixnums and bignums alike, through the ~D machinery.
                    let (negative, digits) = match integer_magnitude(val, radix) {
                        Some(parts) => parts,
                        None => {
                            push_non_integer_in_radix(output, val, radix, mincol, padchar);
                            continue;
                        }
                    };
                    output.text(&format_integer_parts(
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
                    continue;
                }
                // Bignums are integers too: `~R` used to accept only fixnums
                // and signal a TYPE-ERROR claiming 10^20 was not an integer.
                let Some((negative, digits)) = integer_magnitude(val, 10) else {
                    return Err(EgclError::TypeError {
                        datum: val,
                        expected: "integer".into(),
                    });
                };
                if at_sign {
                    // Roman numerals stop at 4999; to_roman prints anything
                    // outside that range in decimal, bignums included.
                    if val.is_fixnum() {
                        output.text(&to_roman(val.as_fixnum(), colon));
                    } else {
                        if negative {
                            output.character('-');
                        }
                        output.text(&digits);
                    }
                    continue;
                }
                let english = cardinal(negative, &digits).ok_or_else(|| {
                    EgclError::ControlError(format!(
                        "~R cannot print {}{} in English: it is too large",
                        if negative { "-" } else { "" },
                        digits
                    ))
                })?;
                output.text(&if colon { ordinal(english) } else { english });
            }
            'F' => {
                // ~w,d,k,overflowchar,padcharF (CLHS 22.3.3.1).  Parameters are
                // resolved left to right BEFORE the value so `~v,vF` binds them
                // to the right arguments (bliss-8rs1); a NIL `v` is an omitted
                // parameter.
                let mut fixed_params: Vec<Option<i64>> = Vec::with_capacity(5);
                for p in params.iter().take(5) {
                    let omitted = matches!(p, Param::None)
                        || (matches!(p, Param::V)
                            && args.get(*arg_idx).is_some_and(|v| v.is_nil()));
                    if omitted {
                        if matches!(p, Param::V) {
                            *arg_idx += 1;
                        }
                        fixed_params.push(None);
                    } else {
                        fixed_params.push(Some(resolve_param(p, 0, arg_idx)?));
                    }
                }
                let param = |k: usize| fixed_params.get(k).copied().flatten();
                let w_opt = param(0).map(|w| w.max(0) as usize);
                let d_opt = param(1).map(|d| d.max(0) as usize);
                let scale = param(2).unwrap_or(0).clamp(-300, 300) as i32;
                let overflow_char = param(3).and_then(|c| char::from_u32(c as u32));
                let pad_char = param(4)
                    .and_then(|c| char::from_u32(c as u32))
                    .unwrap_or(' ');
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~F".into()));
                }
                let mut val = args[*arg_idx];
                egcl_rt::rooted_ref!(_val_root = &mut val);
                *arg_idx += 1;
                // A non-number prints as ~wD (CLHS 22.3.3.1).
                if !egcl_rt::types::numberp(val) || egcl_rt::types::complexp(val) {
                    push_padded_left(
                        output,
                        &with_princ_bindings(|| egclval_to_print_string(val, false)),
                        w_opt.unwrap_or(0),
                        pad_char,
                    );
                    continue;
                }
                let f = real_format_value(val)?;
                // The scale factor multiplies in the value's own precision, so
                // a single float keeps its short decimal (0.1 * 100 is 10.0).
                let (scaled, shortest_abs) = if val.is_single_float() {
                    let v = val.as_single_float() * 10f32.powi(scale);
                    (v as f64, format!("{}", v.abs()))
                } else {
                    let v = f * 10f64.powi(scale);
                    (v, format!("{}", v.abs()))
                };
                // Scaling by a power of ten never changes the sign.
                let negative = f.is_sign_negative();
                let sign = if negative {
                    "-"
                } else if at_sign {
                    "+"
                } else {
                    ""
                };
                let abs = scaled.abs();
                // Fixed digits after the point: D when given; otherwise, with a
                // width, as many of the shortest decimal's as fit (at least the
                // point); otherwise the shortest decimal itself.
                let shortest_frac = shortest_abs.split_once('.').map_or(0, |(_, fr)| fr.len());
                let int_len = shortest_abs
                    .split_once('.')
                    .map_or(shortest_abs.len(), |(i, _)| i.len());
                let fixed_digits = |dd: usize| -> String {
                    // More digits than the shortest decimal carries is padding;
                    // fewer means rounding the float's EXACT value, ties away
                    // from zero (verified against SBCL; see round_decimal_string).
                    if shortest_frac <= dd {
                        round_decimal_string(&shortest_abs, dd)
                    } else {
                        round_decimal_string(&format!("{abs:.150}"), dd)
                    }
                };
                let mut digits = match (w_opt, d_opt) {
                    (_, Some(dd)) => fixed_digits(dd),
                    (Some(w), None) => {
                        let room =
                            (w as i64 - sign.len() as i64 - int_len as i64 - 1).max(0) as usize;
                        // Rounding a value just off a short decimal to the
                        // room can leave zeros (-112.400000000001 -> "112.400");
                        // the width-chosen fraction has no trailing zero digits
                        // (CLHS 22.3.3.1), though it keeps one ("10.0").
                        let mut t = fixed_digits(room.min(shortest_frac.max(1)));
                        if let Some(dot) = t.find('.') {
                            let keep = t.trim_end_matches('0').len().max(dot + 2).min(t.len());
                            t.truncate(keep);
                        }
                        t
                    }
                    (None, None) => {
                        let mut t = shortest_abs.clone();
                        if !t.contains('.') {
                            t.push_str(".0");
                        }
                        t
                    }
                };
                // A width too small for the leading zero drops it ("0.50" ->
                // ".50"); a bare trailing point always gets a zero ("1." ->
                // "1.0"), even past the width.
                if let Some(w) = w_opt {
                    if sign.len() + digits.len() > w && digits.starts_with("0.") && digits.len() > 2
                    {
                        digits.remove(0);
                    }
                    // Only a width-chosen zero fraction loses its zero ("~0F"
                    // of 0.01 is ".0"); an explicit D of 0 keeps it ("~0,0F"
                    // is "0.", ansi format.f.46b).
                    if digits == "0." && d_opt.is_none() {
                        digits = ".".into();
                    }
                }
                // ...except when D is explicitly 0: `~,0F` of 2.5 is "3.".
                if digits.ends_with('.') && d_opt != Some(0) {
                    digits.push('0');
                }
                let mut s = format!("{sign}{digits}");
                if let Some(w) = w_opt {
                    let len = s.chars().count();
                    if len > w {
                        if let Some(oc) = overflow_char {
                            for _ in 0..w {
                                output.character(oc);
                            }
                            continue;
                        }
                    } else {
                        let pad: String = std::iter::repeat_n(pad_char, w - len).collect();
                        s = format!("{pad}{s}");
                    }
                }
                output.text(&s);
            }
            'E' => {
                // ~w,d,e,k,overflowchar,padchar,exponentcharE. Resolve params in
                // left-to-right order (each `v` param consumes the next arg) BEFORE
                // reading the value, so `~v,vE` binds the params to the parameter
                // args rather than mis-binding the value (CLHS 22.3.1; bliss-8rs1).
                let w = params
                    .first()
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let d = params
                    .get(1)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let e = params
                    .get(2)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let k = params
                    .get(3)
                    .map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?;
                let overflow = params
                    .get(4)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let pad_param = params
                    .get(5)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let expc = params
                    .get(6)
                    .map_or(Ok('e' as i64), |p| resolve_param(p, 'e' as i64, arg_idx))?;
                let exp_char = char::from_u32(expc as u32).unwrap_or('e');
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~E".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let f = real_format_value(val)?;
                let mut s = if d < 0 {
                    // No explicit fraction-digit count: shortest round-trip. Format
                    // a single-float from the f32 itself rather than its widened f64,
                    // which exposed binary32 imprecision (bliss-8zrb).
                    let raw = if val.is_single_float() {
                        format!("{:E}", val.as_single_float())
                    } else {
                        format!("{:E}", f)
                    };
                    exp_with_decimal_point(&raw, exp_char)
                } else {
                    format_e_fixed(
                        f,
                        d as usize,
                        if e >= 0 { Some(e as usize) } else { None },
                        k,
                        exp_char,
                    )
                };
                // ~@E signs a non-negative value too (CLHS 22.3.3.2).
                if at_sign && !s.starts_with('-') {
                    s.insert(0, '+');
                }
                if w >= 0 && (s.chars().count() as i64) > w {
                    // Without d the single zero after the point is optional, and
                    // goes when the field is too narrow for it: ~5E of 1.0 is
                    // "1.e+0" (ansi format.e.4-9).
                    if d < 0 {
                        if let Some(p) = s.find(".0") {
                            if !s[p + 2..].starts_with(|c: char| c.is_ascii_digit()) {
                                s.remove(p + 1);
                            }
                        }
                    }
                    // Still too wide: an overflow character fills the field
                    // instead (ansi format.e.21).
                    if (s.chars().count() as i64) > w && overflow >= 0 {
                        let c = char::from_u32(overflow as u32).unwrap_or('*');
                        s = std::iter::repeat_n(c, w as usize).collect();
                    }
                }
                // ~w,…E: right-justify in a field of at least w chars, padding on the
                // left with padchar (default space) (bliss-o30e).
                if w > 0 && (s.chars().count() as i64) < w {
                    let pad = char::from_u32(if pad_param >= 0 {
                        pad_param as u32
                    } else {
                        u32::from(b' ')
                    })
                    .unwrap_or(' ');
                    let n = w as usize - s.chars().count();
                    let prefix: String = std::iter::repeat_n(pad, n).collect();
                    s = format!("{prefix}{s}");
                }
                output.text(&s);
            }
            'G' => {
                // ~w,d,e,k,overflowchar,padcharG. Resolve the prefix params in order
                // (each `v` consumes the next arg) BEFORE the value, so a `v` param
                // binds to its arg and later directives stay aligned (bliss-o30e).
                // Full ~G chooses ~F- vs ~E-style output by magnitude (CLHS 22.3.3.3);
                // that selection is not yet implemented — this prints the shortest
                // round-trip — but the params (and width) now apply.
                let w = params
                    .first()
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let _d = params
                    .get(1)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let _e = params
                    .get(2)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let _k = params
                    .get(3)
                    .map_or(Ok(1), |p| resolve_param(p, 1, arg_idx))?;
                let _of = params
                    .get(4)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                let pad_param = params
                    .get(5)
                    .map_or(Ok(-1), |p| resolve_param(p, -1, arg_idx))?;
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~G".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                // Shortest round-trip from the f32 for single-floats (bliss-8zrb).
                let mut s = if val.is_single_float() {
                    format!("{}", val.as_single_float())
                } else {
                    format!("{}", real_format_value(val)?)
                };
                if w > 0 && (s.chars().count() as i64) < w {
                    let pad = char::from_u32(if pad_param >= 0 {
                        pad_param as u32
                    } else {
                        u32::from(b' ')
                    })
                    .unwrap_or(' ');
                    let n = w as usize - s.chars().count();
                    let prefix: String = std::iter::repeat_n(pad, n).collect();
                    s = format!("{prefix}{s}");
                }
                output.text(&s);
            }
            '$' => {
                // ~d,n,w,padchar$: d = digits after the point (default 2), n = min
                // digits before it (default 1, zero-padded), w = min field width.
                // Resolve the params in left-to-right order (each `v` consumes the
                // next arg) BEFORE reading the value, so `~v,v$` binds the params to
                // the parameter args, not the value (CLHS 22.3.1; bliss-8rs1).
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
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~$".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let f = real_format_value(val)?;
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
                let sign = if f.is_sign_negative() {
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
                        output.text(sign);
                        output.text(&pad);
                        output.text(&num[sign.len()..]);
                    } else {
                        output.text(&pad);
                        output.text(&num);
                    }
                } else {
                    output.text(&num);
                }
            }
            '%' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.character('\n');
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
                    output.character('\n');
                }
                for _ in 1..count {
                    output.character('\n');
                }
            }
            '|' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.character('\x0c');
                }
            }
            // Inside a logical block these remain semantic operations until the
            // block is laid out.  Outside one the renderer treats indentation and
            // non-mandatory conditional newlines as inert.
            'I' => {
                let amount = params
                    .first()
                    .map_or(Ok(0), |param| resolve_param(param, 0, arg_idx))?
                    .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                output.events.push(LayoutEvent::Indent {
                    // CLHS ~:I is relative to the current column; plain ~I is
                    // relative to the logical block's body origin.
                    relative_to: if colon {
                        IndentKind::Current
                    } else {
                        IndentKind::Block
                    },
                    amount,
                });
            }
            '_' => {
                let kind = match (colon, at_sign) {
                    (false, false) => NewlineKind::Linear,
                    (true, false) => NewlineKind::Fill,
                    (false, true) => NewlineKind::Miser,
                    (true, true) => NewlineKind::Mandatory,
                };
                output.events.push(LayoutEvent::Newline(kind));
            }
            '~' => {
                let count = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?
                } else {
                    1
                };
                for _ in 0..count {
                    output.character('~');
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
                // ~:T and ~:@T are PPRINT-TAB (CLHS 22.3.5.2): they apply only
                // inside a logical block while *PRINT-PRETTY* is true (ansi
                // format.:t.2-3, format.:@t.4-5).
                let first = if !params.is_empty() {
                    resolve_param(&params[0], 1, arg_idx)?.clamp(0, u32::MAX as i64) as u32
                } else {
                    1
                };
                let colinc = if params.len() > 1 {
                    resolve_param(&params[1], 1, arg_idx)?.clamp(0, u32::MAX as i64) as u32
                } else {
                    1
                };
                output.events.push(LayoutEvent::Tab {
                    kind: if colon {
                        if at_sign {
                            TabKind::SectionRelative
                        } else {
                            TabKind::Section
                        }
                    } else if at_sign {
                        TabKind::LineRelative
                    } else {
                        TabKind::Line
                    },
                    colnum: first,
                    colinc,
                });
            }
            '*' => {
                // ~n* skips n (default 1); ~n:* backs up n; ~n@* goes to
                // argument n (default 0).
                let default = if at_sign { 0 } else { 1 };
                let n = if !params.is_empty() {
                    resolve_param(&params[0], default, arg_idx)?.max(0) as usize
                } else {
                    default as usize
                };
                if at_sign {
                    *arg_idx = n.min(args.len());
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
                    return Err(EgclError::ControlError("too few args for ~C".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                if !val.is_character() {
                    return Err(EgclError::TypeError {
                        datum: val,
                        expected: "character".into(),
                    });
                }
                let c = val.as_char();
                if colon {
                    // Printing characters are graphic characters other than
                    // Space. Unicode whitespace can still be graphic; reader
                    // syntax uses names for it independently of this display.
                    // ~:@C adds nothing (no keyboard attributes to describe).
                    if c != ' ' && graphic_char_p(c) {
                        output.character(c);
                    } else {
                        output.text(&character_name(c));
                    }
                } else if at_sign {
                    // ~@C prints the character as the reader would read it.
                    output.text("#\\");
                    if c == ' ' && !print_readably_active() {
                        output.character(' ');
                    } else {
                        output.text(&character_name(c));
                    }
                } else {
                    output.character(c);
                }
            }
            'W' => {
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~W".into()));
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let mut pretty_binding = colon
                    .then(|| PrintDynamicBinding::establish("*PRINT-PRETTY*", T))
                    .flatten();
                let mut level_binding = at_sign
                    .then(|| PrintDynamicBinding::establish("*PRINT-LEVEL*", NIL))
                    .flatten();
                let mut length_binding = at_sign
                    .then(|| PrintDynamicBinding::establish("*PRINT-LENGTH*", NIL))
                    .flatten();
                egcl_rt::rooted_ref!(_pretty_binding_root = &mut pretty_binding);
                egcl_rt::rooted_ref!(_level_binding_root = &mut level_binding);
                egcl_rt::rooted_ref!(_length_binding_root = &mut length_binding);
                let doc = print_value_layout(output, val, true);
                output.append(doc);
            }
            '?' => {
                if *arg_idx >= args.len() {
                    return Err(EgclError::ControlError("too few args for ~?".into()));
                }
                let ctrl_val = args[*arg_idx];
                *arg_idx += 1;
                // The control is a format string or, as for `~{~}`, a function
                // such as FORMATTER returns (CLHS 22.3.7.6): it is called with
                // a string stream and the arguments, and returns those it did
                // not consume (ansi format.?.6-11, format.@?.6-9). An
                // interpreted closure is cons-tagged; the apply hook calls it.
                let is_string = ctrl_val.is_heap_object() && egcl_rt::types::stringp(ctrl_val);
                let callable =
                    ctrl_val.is_function() || ctrl_val.is_heap_object() || ctrl_val.is_cons();
                if !is_string && callable {
                    if at_sign {
                        run_iteration_body("", Some(ctrl_val), args, arg_idx, output)?;
                    } else {
                        if *arg_idx >= args.len() {
                            return Err(EgclError::ControlError("too few args for ~?".into()));
                        }
                        let args_val = args[*arg_idx];
                        *arg_idx += 1;
                        let mut sub_args = if args_val.is_nil() {
                            Vec::new()
                        } else {
                            cons_list_to_vec(args_val)
                        };
                        egcl_rt::rooted_ref!(_sub_args_root = &mut sub_args);
                        let mut sub_idx = 0;
                        run_iteration_body("", Some(ctrl_val), &sub_args, &mut sub_idx, output)?;
                    }
                    take_escape();
                } else if !is_string {
                    return Err(EgclError::TypeError {
                        datum: ctrl_val,
                        expected: "(OR STRING FUNCTION)".into(),
                    });
                } else {
                    let sub_control = extract_egcl_string(ctrl_val).ok_or_else(|| {
                        EgclError::Internal("failed to extract format string".into())
                    })?;
                    if at_sign {
                        // ~@? — use the enclosing argument list from current position
                        format_impl(&sub_control, args, arg_idx, output)?;
                        // A `~^` inside ends the ~? control string only.
                        take_escape();
                    } else {
                        // ~? — consume a separate list argument for the sub-format's args
                        if *arg_idx >= args.len() {
                            return Err(EgclError::ControlError("too few args for ~?".into()));
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
                        take_escape();
                    }
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
                        return Err(EgclError::ControlError("too few args for ~P".into()));
                    }
                }
                let val = args[*arg_idx];
                *arg_idx += 1;
                let is_one = val.is_fixnum() && val.as_fixnum() == 1;
                if at_sign {
                    output.text(if is_one { "y" } else { "ies" });
                } else {
                    if !is_one {
                        output.character('s');
                    }
                }
            }
            '^' => {
                // ~^ (CLHS 22.3.9.2): with no parameters, terminate when no
                // arguments remain (`~:^`: when no sublists remain); with one,
                // when it is zero; with two, when they are equal; with three,
                // when they are in ascending order.  The enclosing iteration,
                // `~?` or `~<` sees the flag and decides what it ends.
                // A `v` parameter whose argument is NIL counts as omitted
                // (ansi format.^.{.25/33), so `~1,1,v^` with NIL compares two.
                let mut vals: Vec<i64> = Vec::with_capacity(params.len());
                for p in &params {
                    if matches!(p, Param::V) && args.get(*arg_idx).is_some_and(|v| v.is_nil()) {
                        *arg_idx += 1;
                        continue;
                    }
                    if matches!(p, Param::None) {
                        continue;
                    }
                    vals.push(resolve_param(p, 0, arg_idx)?);
                }
                let terminate = match vals.len() {
                    0 if colon => OUTER_REMAINING
                        .with(|c| c.get())
                        .map_or(*arg_idx >= args.len(), |n| n == 0),
                    0 => *arg_idx >= args.len(),
                    1 => vals[0] == 0,
                    2 => vals[0] == vals[1],
                    _ => vals[0] <= vals[1] && vals[1] <= vals[2],
                };
                if terminate {
                    FORMAT_ESCAPE.with(|c| c.set(if colon { 2 } else { 1 }));
                    return Ok(());
                }
            }
            '{' => {
                // Find matching ~}
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '{')?;
                let mut body: String = chars[body_start..body_end].iter().collect();
                // `~:}` runs the body at least once even over no arguments.
                let at_least_once = {
                    let mut k = body_end + 1;
                    let mut found = false;
                    while k < chars.len() && chars[k] != '}' {
                        if chars[k] == ':' {
                            found = true;
                        }
                        k += 1;
                    }
                    found
                };
                i = skip_close_directive(&chars, body_end);
                // The prefix parameter of ~{ is the maximum iteration count; a
                // NIL `v` argument means no limit (ansi format.{.33).
                let count_given = match params.first() {
                    None | Some(Param::None) => false,
                    Some(Param::V) => {
                        let is_nil = args.get(*arg_idx).is_some_and(|v| v.is_nil());
                        if is_nil {
                            *arg_idx += 1;
                        }
                        !is_nil
                    }
                    Some(_) => true,
                };
                let max_iter: Option<i64> = if count_given {
                    Some(resolve_param(&params[0], 0, arg_idx)?)
                } else {
                    None
                };
                // An empty body takes the control -- a string or a FORMATTER
                // function -- from the next argument (CLHS 22.3.7.4).
                // Root the body FORMATTER function across the iteration calls
                // below. Each run_iteration_body invokes Lisp and allocates (the
                // nested format, its argument lists, the output string), so a
                // relocating GC would leave an unrooted copy stale for the NEXT
                // pass -- and the directive may iterate many times. Held as a
                // EgclVal with NIL for "no function" because the root macros
                // take a EgclVal, not an Option (bliss-gppm7).
                let mut body_function: EgclVal = NIL;
                egcl_rt::rooted_ref!(_body_function_root = &mut body_function);
                if body.is_empty() {
                    if *arg_idx >= args.len() {
                        return Err(EgclError::ControlError("too few args for ~{~}".into()));
                    }
                    let control = args[*arg_idx];
                    *arg_idx += 1;
                    match extract_egcl_string(control) {
                        Some(text) => body = text,
                        None => body_function = control,
                    }
                }
                let reached = |n: i64| max_iter.is_some_and(|m| n >= m);
                if at_sign && colon {
                    // ~:@{ : each remaining argument is a sublist for one pass.
                    let mut iters = 0i64;
                    let saved_outer = OUTER_REMAINING.with(|c| c.get());
                    if at_least_once && *arg_idx >= args.len() && !reached(0) {
                        let mut sub_idx = 0;
                        OUTER_REMAINING.with(|c| c.set(Some(0)));
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            &[],
                            &mut sub_idx,
                            output,
                        )?;
                        take_escape();
                    }
                    while *arg_idx < args.len() && !reached(iters) {
                        let sub = args[*arg_idx];
                        *arg_idx += 1;
                        iters += 1;
                        if !sub.is_nil() && !sub.is_cons() {
                            return Err(EgclError::TypeError {
                                datum: sub,
                                expected: "LIST".into(),
                            });
                        }
                        let sub_args = proper_list_elements(sub)?;
                        let mut sub_idx = 0;
                        OUTER_REMAINING.with(|c| c.set(Some(args.len() - *arg_idx)));
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            &sub_args,
                            &mut sub_idx,
                            output,
                        )?;
                        // `~^` ends this sublist's pass; `~:^` the iteration.
                        if take_escape() == 2 {
                            break;
                        }
                    }
                    OUTER_REMAINING.with(|c| c.set(saved_outer));
                } else if at_sign {
                    // ~@{ : iterate over the remaining arguments themselves.
                    let mut iters = 0i64;
                    if at_least_once && *arg_idx >= args.len() && !reached(0) {
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            args,
                            arg_idx,
                            output,
                        )?;
                        take_escape();
                    }
                    while *arg_idx < args.len() && !reached(iters) {
                        let before = *arg_idx;
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            args,
                            arg_idx,
                            output,
                        )?;
                        if take_escape() != 0 {
                            break;
                        }
                        iters += 1;
                        // A pass that consumed nothing would never end.
                        if *arg_idx == before && max_iter.is_none() {
                            break;
                        }
                    }
                } else if colon {
                    // ~:{ : the argument is a list of sublists, one per pass.
                    if *arg_idx >= args.len() {
                        return Err(EgclError::ControlError("too few args for ~:{".into()));
                    }
                    let list_val = args[*arg_idx];
                    *arg_idx += 1;
                    if !list_val.is_nil() && !list_val.is_cons() {
                        return Err(EgclError::TypeError {
                            datum: list_val,
                            expected: "LIST".into(),
                        });
                    }
                    if list_val.is_nil() && at_least_once && !reached(0) {
                        let saved_outer = OUTER_REMAINING.with(|c| c.get());
                        OUTER_REMAINING.with(|c| c.set(Some(0)));
                        let mut sub_idx = 0;
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            &[],
                            &mut sub_idx,
                            output,
                        )?;
                        take_escape();
                        OUTER_REMAINING.with(|c| c.set(saved_outer));
                    }
                    if !list_val.is_nil() {
                        let mut sublists = proper_list_elements(list_val)?;
                        egcl_rt::rooted_ref!(_sublists_root = &mut sublists);
                        let saved_outer = OUTER_REMAINING.with(|c| c.get());
                        for (iters, sublist) in sublists.iter().enumerate() {
                            if reached(iters as i64) {
                                break;
                            }
                            if !sublist.is_nil() && !sublist.is_cons() {
                                return Err(EgclError::TypeError {
                                    datum: *sublist,
                                    expected: "LIST".into(),
                                });
                            }
                            let sub_args = proper_list_elements(*sublist)?;
                            let mut sub_idx = 0;
                            OUTER_REMAINING.with(|c| c.set(Some(sublists.len() - iters - 1)));
                            run_iteration_body(
                                &body,
                                body_function_arg(body_function),
                                &sub_args,
                                &mut sub_idx,
                                output,
                            )?;
                            if take_escape() == 2 {
                                break;
                            }
                        }
                        OUTER_REMAINING.with(|c| c.set(saved_outer));
                    }
                } else {
                    // ~{ : the argument is the list of arguments for the passes.
                    if *arg_idx >= args.len() {
                        return Err(EgclError::ControlError("too few args for ~{".into()));
                    }
                    let list_val = args[*arg_idx];
                    *arg_idx += 1;
                    if !list_val.is_nil() && !list_val.is_cons() {
                        return Err(EgclError::TypeError {
                            datum: list_val,
                            expected: "LIST".into(),
                        });
                    }
                    if list_val.is_nil() && at_least_once && !reached(0) {
                        let mut sub_idx = 0;
                        run_iteration_body(
                            &body,
                            body_function_arg(body_function),
                            &[],
                            &mut sub_idx,
                            output,
                        )?;
                        take_escape();
                    }
                    if !list_val.is_nil() {
                        let mut list_elements = proper_list_elements(list_val)?;
                        egcl_rt::rooted_ref!(_list_elements_root = &mut list_elements);
                        let mut sub_idx = 0;
                        let mut iters = 0i64;
                        while sub_idx < list_elements.len() && !reached(iters) {
                            let before = sub_idx;
                            run_iteration_body(
                                &body,
                                body_function_arg(body_function),
                                &list_elements,
                                &mut sub_idx,
                                output,
                            )?;
                            if take_escape() != 0 {
                                break;
                            }
                            iters += 1;
                            // A pass that consumed nothing would never end.
                            if sub_idx == before && max_iter.is_none() {
                                break;
                            }
                        }
                    }
                }
            }
            '}' => {
                return Err(EgclError::Internal("unmatched ~}".into()));
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
                        return Err(EgclError::ControlError("too few args for ~:[".into()));
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
                        return Err(EgclError::ControlError("too few args for ~@[".into()));
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
                    // A `v` whose argument is NIL counts as no parameter
                    // (ansi format.cond.14): consume it and use the next
                    // argument as the selector.
                    let nil_v = matches!(params.first(), Some(Param::V))
                        && args.get(*arg_idx).is_some_and(|v| v.is_nil());
                    if nil_v {
                        *arg_idx += 1;
                    }
                    let idx_i64 =
                        if !nil_v && params.first().is_some_and(|p| !matches!(p, Param::None)) {
                            resolve_param(&params[0], 0, arg_idx)?
                        } else {
                            if *arg_idx >= args.len() {
                                return Err(EgclError::ControlError("too few args for ~[".into()));
                            }
                            let val = args[*arg_idx];
                            *arg_idx += 1;
                            if val.is_fixnum() {
                                val.as_fixnum()
                            } else if let Some((negative, _)) = integer_magnitude(val, 10) {
                                // A bignum selects no clause (ansi format.cond.4).
                                if negative { i64::MIN } else { i64::MAX }
                            } else {
                                return Err(EgclError::TypeError {
                                    datum: val,
                                    expected: "integer".into(),
                                });
                            }
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
                return Err(EgclError::Internal("unmatched ~]".into()));
            }
            '(' => {
                let body_start = i;
                let body_end = find_matching_close(&chars, i, '(')?;
                let body: String = chars[body_start..body_end].iter().collect();
                i = skip_close_directive(&chars, body_end);
                let mut inner = LayoutDoc::default();
                format_impl(&body, args, arg_idx, &mut inner)?;
                let mode = match (colon, at_sign) {
                    (true, true) => CaseMode::Upper,
                    (true, false) => CaseMode::CapitalizeWords,
                    (false, true) => CaseMode::CapitalizeFirst,
                    (false, false) => CaseMode::Lower,
                };
                output.events.push(LayoutEvent::CaseStart(mode));
                output.events.extend(inner.into_events());
                output.events.push(LayoutEvent::CaseEnd);
            }
            ')' => {
                return Err(EgclError::Internal("unmatched ~)".into()));
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
                    // Pretty-printing logical block (CLHS 22.3.5.2). Segments
                    // are [prefix~;]body[~;suffix]; `~@;` marks a per-line
                    // prefix. A `:` on the OPENING supplies the
                    // default "("/")" pair. `@` on the opening formats the
                    // block over the remaining arguments; otherwise the block
                    // consumes ONE argument — a list, whose elements become
                    // the body's arguments (SBCL errors even for a directive-
                    // free body when that argument is missing) — and a
                    // NON-list argument is printed by WRITE with the whole
                    // block, prefix and suffix included, skipped (bliss-aspj).
                    let per_line_prefix = first_separator_has_at_sign(&body);
                    let clauses = split_clauses(&body);
                    let (pre_seg, body_seg, suf_seg) = match clauses.len() {
                        0 => (None, String::new(), None),
                        1 => (None, clauses[0].clone(), None),
                        2 => (Some(clauses[0].clone()), clauses[1].clone(), None),
                        _ => (
                            Some(clauses[0].clone()),
                            clauses[1].clone(),
                            Some(clauses[2].clone()),
                        ),
                    };
                    let prefix =
                        pre_seg.unwrap_or_else(|| if colon { "(" } else { "" }.to_string());
                    let suffix =
                        suf_seg.unwrap_or_else(|| if colon { ")" } else { "" }.to_string());
                    // Prefix and suffix are constant controls, formatted over
                    // no arguments and flattened before becoming block metadata.
                    let format_const = |seg: &str| -> Result<String, EgclError> {
                        let mut doc = LayoutDoc::default();
                        format_impl(seg, &[], &mut 0, &mut doc)?;
                        Ok(doc.flat_string())
                    };
                    let prefix = format_const(&prefix)?;
                    let suffix = format_const(&suffix)?;
                    if at_sign {
                        let mut body_doc = LayoutDoc::default();
                        format_impl(&body_seg, args, arg_idx, &mut body_doc)?;
                        take_escape();
                        output.events.push(LayoutEvent::Block {
                            prefix: prefix.clone(),
                            per_line_prefix: per_line_prefix.then_some(prefix),
                            suffix,
                            events: body_doc.into_events(),
                        });
                    } else {
                        if *arg_idx >= args.len() {
                            return Err(EgclError::ControlError("too few args for ~<".into()));
                        }
                        let val = args[*arg_idx];
                        *arg_idx += 1;
                        if val.is_cons() || val.is_nil() {
                            let mut sub_args = cons_list_to_vec(val);
                            egcl_rt::rooted_ref!(_sub_args_root = &mut sub_args);
                            let mut sub_idx = 0;
                            let mut body_doc = LayoutDoc::default();
                            // One graph-wide table makes repeated elements share
                            // labels across separate ~A/~S directives.
                            circle_enter(val);
                            let body_result =
                                format_impl(&body_seg, &sub_args, &mut sub_idx, &mut body_doc);
                            circle_exit();
                            body_result?;
                            take_escape();
                            output.events.push(LayoutEvent::Block {
                                prefix: prefix.clone(),
                                per_line_prefix: per_line_prefix.then_some(prefix),
                                suffix,
                                events: body_doc.into_events(),
                            });
                        } else {
                            output.text(&egclval_to_print_string(val, true));
                        }
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
                    let mut clauses = split_clauses(&body);
                    // ~W, ~I and ~_ belong to the pretty printer and are
                    // errors inside a justification (CLHS 22.3.6.2).
                    for clause in &clauses {
                        if let Some(d) = clause_directive_among(clause, &['W', 'I', '_']) {
                            return Err(EgclError::ControlError(format!(
                                "~{d} is not allowed inside ~<...~> justification"
                            )));
                        }
                    }
                    // `~n,m:;` after the first segment makes it an overflow
                    // prefix: printed only when the justified text plus `n`
                    // spare columns would not fit in a line of `m` columns.
                    let overflow = justify_overflow_params(&body);
                    let mut overflow_text: Option<String> = None;
                    if overflow.is_some() && !clauses.is_empty() {
                        let first = clauses.remove(0);
                        let mut text = LayoutDoc::default();
                        format_impl(&first, args, arg_idx, &mut text)?;
                        if take_escape() != 0 {
                            clauses.clear();
                        }
                        overflow_text = Some(text.flat_string());
                    }
                    let mut parts = Vec::new();
                    for clause in &clauses {
                        let mut part = LayoutDoc::default();
                        format_impl(clause, args, arg_idx, &mut part)?;
                        // `~^` in a segment discards that segment and ends the
                        // clause; the segments so far are justified.
                        if take_escape() != 0 {
                            break;
                        }
                        parts.push(part.flat_string());
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
                    let gap_width =
                        |g: usize| -> usize { minpad + per_gap + usize::from(g + rem >= gaps) };

                    let mut g = 0usize;
                    let mut justified = String::new();
                    if pad_before {
                        justified.extend(std::iter::repeat_n(padchar, gap_width(g)));
                        g += 1;
                    }
                    for (j, part) in parts.iter().enumerate() {
                        justified.push_str(part);
                        if j + 1 < parts.len() {
                            justified.extend(std::iter::repeat_n(padchar, gap_width(g)));
                            g += 1;
                        }
                    }
                    if pad_after {
                        justified.extend(std::iter::repeat_n(padchar, gap_width(g)));
                    }
                    if let (Some((spare, line_width)), Some(text)) = (overflow, overflow_text) {
                        let column = output.current_column();
                        let line_width = line_width.unwrap_or_else(|| {
                            egcl_rt::symbols::find_index("*PRINT-RIGHT-MARGIN*")
                                .and_then(egcl_rt::symbols::symbol_value)
                                .filter(|v| v.is_fixnum())
                                .map_or(72, |v| v.as_fixnum().max(0) as usize)
                        });
                        if column + justified.chars().count() + spare > line_width {
                            output.text(&text);
                        }
                    }
                    output.text(&justified);
                }
            }
            '>' => {
                return Err(EgclError::Internal("unmatched ~>".into()));
            }
            '/' => {
                // ~/name/ — call the function NAME names (CLHS 22.3.5.4) with
                // (stream arg colon-p at-sign-p . params).  NAME is read as a
                // symbol in COMMON-LISP-USER unless it carries a package prefix;
                // the function writes to a string stream whose text goes into
                // the output.  Consumes one argument.
                let name_start = i;
                while i < chars.len() && chars[i] != '/' {
                    i += 1;
                }
                let name: String = chars[name_start..i].iter().collect();
                if i < chars.len() {
                    i += 1;
                } // skip closing /
                // Prefix parameters come first in the argument list, so a `v`
                // parameter takes its value before the directive's own argument.
                let mut param_vals: Vec<EgclVal> = Vec::with_capacity(params.len());
                for p in &params {
                    param_vals.push(match p {
                        Param::Num(n) => EgclVal::from_fixnum(*n),
                        Param::Char(c) => EgclVal::from_char(*c),
                        Param::V => {
                            if *arg_idx >= args.len() {
                                return Err(EgclError::ControlError(
                                    "too few args for V param".into(),
                                ));
                            }
                            let v = args[*arg_idx];
                            *arg_idx += 1;
                            v
                        }
                        Param::Hash => {
                            EgclVal::from_fixnum(args.len().saturating_sub(*arg_idx) as i64)
                        }
                        Param::None => NIL,
                    });
                }
                if *arg_idx >= args.len() {
                    return Err(EgclError::Internal(format!("too few args for ~/{}/", name)));
                }
                let arg = args[*arg_idx];
                *arg_idx += 1;
                let Some(func) =
                    lookup_format_function(&name).or_else(|| format_function_designator(&name))
                else {
                    return Err(EgclError::UndefinedFunction(
                        egcl_compiler::reader::read_symbol_token(&name.to_uppercase())
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| make_egcl_string(&name)),
                    ));
                };
                egcl_rt::rooted!(func = func);
                egcl_rt::rooted!(arg = arg);
                egcl_rt::rooted_ref!(_param_vals_root = &mut param_vals);
                let mut call_args: Vec<EgclVal> = Vec::with_capacity(3 + param_vals.len());
                call_args.push(*arg);
                call_args.push(if colon { T } else { NIL });
                call_args.push(if at_sign { T } else { NIL });
                call_args.extend_from_slice(&param_vals);
                egcl_rt::rooted_ref!(_call_args_root = &mut call_args);
                let (_, doc) = apply_format_function_to_layout(
                    *func,
                    &call_args,
                    output.current_column(),
                )?;
                output.append(doc);
            }
            '\n' => {
                // ~<newline>: the newline and the following whitespace are
                // skipped; `~:` keeps the whitespace, `~@` keeps the newline
                // (CLHS 22.3.9.3; ansi format.newline.2/3).
                if at_sign {
                    output.character('\n');
                }
                if !colon {
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                }
            }
            _ => {
                return Err(EgclError::Internal(format!(
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
fn find_matching_close(chars: &[char], start: usize, open: char) -> Result<usize, EgclError> {
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
    Err(EgclError::Internal(format!("unmatched ~{}", open)))
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

/// The elements of a proper list; a dotted tail is a TYPE-ERROR (what `~{`
/// signals for `(x y . z)`, ansi format.{.error.5).
fn proper_list_elements(list: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let mut elements = Vec::new();
    let mut cur = list;
    while cur.is_cons() {
        // SAFETY: `cur` is a cons, so its payload is a ConsCell.
        unsafe {
            let ptr = cur.as_ptr() as *const egcl_rt::object::ConsCell;
            elements.push((*ptr).car);
            cur = (*ptr).cdr;
        }
    }
    if !cur.is_nil() {
        // The offending datum is the dotted tail, as SBCL reports it.
        return Err(EgclError::TypeError {
            datum: cur,
            expected: "LIST".into(),
        });
    }
    Ok(elements)
}

/// Does `control` contain a justification with a `~:;` overflow clause?
/// CLHS 22.3.6.2 forbids `~W`, `~I` and `~_` anywhere in such a control
/// string (ansi format.justify.error.*).
fn control_has_overflow_justification(control: &str) -> bool {
    let chars: Vec<char> = control.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            i += 1;
            continue;
        }
        i += 1;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || matches!(
                    chars[i],
                    ',' | '\'' | 'v' | 'V' | '#' | ':' | '@' | '-' | '+'
                ))
        {
            if chars[i] == '\'' {
                i += 1;
            }
            i += 1;
        }
        if i < chars.len() && chars[i] == '<' {
            let body_start = i + 1;
            let Ok(body_end) = find_matching_close(&chars, body_start, '<') else {
                return false;
            };
            let mut k = body_end + 1;
            let mut close_colon = false;
            while k < chars.len() && chars[k] != '>' {
                if chars[k] == ':' {
                    close_colon = true;
                }
                k += 1;
            }
            let body: String = chars[body_start..body_end].iter().collect();
            if !close_colon && justify_overflow_params(&body).is_some() {
                return true;
            }
            i = k;
        }
        i += 1;
    }
    false
}

/// Scan `clause` for a directive whose character (upper-cased) is one of
/// `wanted`, skipping prefix parameters and modifiers; `~~` and `~'x`
/// parameters do not count.
fn clause_directive_among(clause: &str, wanted: &[char]) -> Option<char> {
    let chars: Vec<char> = clause.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            i += 1;
            continue;
        }
        i += 1;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || matches!(
                    chars[i],
                    ',' | '\'' | 'v' | 'V' | '#' | ':' | '@' | '-' | '+'
                ))
        {
            if chars[i] == '\'' {
                i += 1;
            }
            i += 1;
        }
        if i < chars.len() {
            let d = chars[i].to_ascii_uppercase();
            if wanted.contains(&d) {
                return Some(d);
            }
            i += 1;
        }
    }
    None
}

/// For a justification body whose FIRST top-level separator is `~n,m:;`,
/// the `(n, m)` parameters (n defaults to 0, m to the right margin): the
/// first segment is then an overflow prefix (CLHS 22.3.6.2).
fn justify_overflow_params(body: &str) -> Option<(usize, Option<usize>)> {
    let chars: Vec<char> = body.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            i += 1;
            continue;
        }
        let start = i + 1;
        i += 1;
        let mut colon = false;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || matches!(
                    chars[i],
                    ',' | '\'' | 'v' | 'V' | '#' | ':' | '@' | '-' | '+'
                ))
        {
            if chars[i] == '\'' {
                i += 1;
            } else if chars[i] == ':' {
                colon = true;
            }
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        match chars[i] {
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            ';' if depth == 0 => {
                if !colon {
                    return None;
                }
                let params: String = chars[start..i]
                    .iter()
                    .filter(|c| **c != ':' && **c != '@')
                    .collect();
                let mut fields = params.split(',');
                let spare = fields
                    .next()
                    .and_then(|f| f.parse::<usize>().ok())
                    .unwrap_or(0);
                let width = fields.next().and_then(|f| f.parse::<usize>().ok());
                return Some((spare, width));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether the first top-level logical-block separator is `~@;`, which makes
/// the preceding prefix repeat on every continuation line.
fn first_separator_has_at_sign(body: &str) -> bool {
    let chars: Vec<char> = body.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            i += 1;
            continue;
        }
        i += 1;
        let mut at_sign = false;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || matches!(
                    chars[i],
                    ',' | '\'' | 'v' | 'V' | '#' | ':' | '@' | '-' | '+'
                ))
        {
            if chars[i] == '\'' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            at_sign |= chars[i] == '@';
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        match chars[i] {
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            ';' if depth == 0 => return at_sign,
            _ => {}
        }
        i += 1;
    }
    false
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

// ── formatter ─────────────────────────────────────────────────────

/// Compile a FORMAT control string for repeated use.
/// Returns a closure (function-tagged heap object) that, when called with
/// a stream and arguments, performs the formatting.
pub fn formatter(control_string: &str) -> Result<EgclVal, EgclError> {
    // Validate the control string
    validate_matching(control_string)?;
    // Allocate a ClosureData that captures the control string.
    // The closure's function field points to the control string as a EgclVal.
    // When invoked, the runtime should extract the control string and call format().
    let ctrl_str = make_egcl_string(control_string);
    let total = std::mem::size_of::<egcl_rt::object::ClosureData>() + 8; // one captured var
    let size_units = total.div_ceil(8) as u16;
    let layout = std::alloc::Layout::from_size_align(total, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let header = ObjectHeader::new(type_id::CLOSURE, size_units);
        let closure = ptr as *mut egcl_rt::object::ClosureData;
        (*closure).header = header;
        (*closure).function = ctrl_str; // the captured control string
        // Store control string in closed_vars slot (offset after ClosureData)
        *(ptr.add(std::mem::size_of::<egcl_rt::object::ClosureData>()) as *mut EgclVal) = ctrl_str;
        // Return as function-tagged pointer so it's callable
        Ok(EgclVal::from_function_ptr(ptr))
    }
}

// ── Pretty-printer ─────────────────────────────────────────────────

/// Begin a logical block for pretty-printing (PPRINT-LOGICAL-BLOCK). R5.41.
pub fn pprint_logical_block(
    stream: EgclVal,
    list: EgclVal,
    prefix: Option<&str>,
    per_line_prefix: Option<&str>,
    suffix: Option<&str>,
    body: EgclVal,
) -> Result<(), EgclError> {
    let mut stream = stream;
    egcl_rt::rooted_ref!(_stream_root = &mut stream);
    // Build the output: per_line_prefix (or prefix) + body content + suffix
    let mut output = String::new();

    // Emit prefix or per-line-prefix
    if let Some(plp) = per_line_prefix {
        output.push_str(plp);
    } else if let Some(p) = prefix {
        output.push_str(p);
    }

    // Process the body: if body is a string, format it with the list as args
    if body.is_heap_object() && egcl_rt::types::stringp(body) {
        if let Some(body_str) = extract_egcl_string(body) {
            let list_elements = if list.is_nil() {
                Vec::new()
            } else {
                cons_list_to_vec(list)
            };
            let mut body_output = LayoutDoc::default();
            let mut idx = 0;
            format_impl(&body_str, &list_elements, &mut idx, &mut body_output)?;
            output.push_str(&body_output.flat_string());
        }
    } else if !list.is_nil() {
        // If no body format string, print the list elements separated by spaces
        let elements = cons_list_to_vec(list);
        for (j, elem) in elements.iter().enumerate() {
            if j > 0 {
                output.push(' ');
            }
            output.push_str(&egclval_to_print_string(*elem, false));
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
            crate::streams::stream_write_char(stream, EgclVal::from_char(ch))?;
        }
    }
    Ok(())
}

/// Insert a conditional newline (PPRINT-NEWLINE). R5.41.
pub fn pprint_newline(kind: NewlineKind, stream: EgclVal) -> Result<(), EgclError> {
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
            crate::streams::stream_write_char(stream, EgclVal::from_char('\n'))?;
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

static PPRINT_INDENT_LEVEL: egcl_rt::execution_local::ExecutionLocal<Cell<i32>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(0) }) };

/// Adjust indentation (PPRINT-INDENT). R5.41.
pub fn pprint_indent(relative: bool, n: i32, _stream: EgclVal) -> Result<(), EgclError> {
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
    stream: EgclVal,
) -> Result<(), EgclError> {
    let mut stream = stream;
    egcl_rt::rooted_ref!(_stream_root = &mut stream);
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
                crate::streams::stream_write_char(stream, EgclVal::from_char(' '))?;
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
    _type_spec: EgclVal,
    function: EgclVal,
    priority: f64,
}

/// A simple pprint dispatch table stored as a heap object.
#[allow(dead_code)]
struct PprintDispatchTable {
    entries: Vec<DispatchEntry>,
}

// ── Format function dispatch (~/name/ directive) ─────────────────

/// Invoke a format dispatch function (registered via `register_format_function`).
/// The function is a EgclVal which may be a compiled function with an entry
/// point, an interpreted function, or a closure.  We attempt to call it with
/// the provided arguments; on any structural mismatch we return an error so
/// the caller can fall back gracefully.
fn call_format_dispatch(func: EgclVal, args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    use egcl_rt::object::{CompiledFunctionData, type_id};

    if !func.is_function() && !func.is_heap_object() {
        return Err(EgclError::Internal(
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

        let header = unsafe { *(ptr as *const egcl_rt::object::ObjectHeader) };
        let tid = header.type_id();

        if tid == type_id::COMPILED_FUNCTION {
            let cf = unsafe { &*(ptr as *const CompiledFunctionData) };
            let entry = cf.entry_point;
            if !entry.is_null() {
                // Call compiled entry point as a Rust-ABI function that
                // takes a slice of EgclVal arguments and returns EgclVal.
                type EntryFn = fn(&[EgclVal]) -> EgclVal;
                let f: EntryFn = unsafe { std::mem::transmute(entry) };
                return Ok(f(args));
            }
        }

        // For interpreted functions / closures we cannot evaluate the body
        // without the full evaluator.  Return an error so the caller falls
        // back to the aesthetic representation.
        return Err(EgclError::Internal(
            "~/name/ function is interpreted/closure — direct call not yet supported".into(),
        ));
    }

    Err(EgclError::Internal(
        "~/name/ function is not callable".into(),
    ))
}

// ── User format function registry (~/ directive) ──────────────────
use std::collections::HashMap;

/// Registry for user-defined format functions used by the ~/name/ directive.
/// Maps function name (uppercase) to a EgclVal representing the function.
static FORMAT_FUNCTION_REGISTRY: OrderedMutex<Option<HashMap<String, EgclVal>>> =
    OrderedMutex::new(LockLevel::GcWorld, 14, "format function GC roots", None);

fn scan_format_global_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
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
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_format_global_roots));
}

/// Register a user-defined format function for use with the ~/name/ directive.
pub fn register_format_function(name: &str, function: EgclVal) {
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

/// The function a `~/name/` directive names: NAME read as a symbol in
/// COMMON-LISP-USER unless it carries a package prefix (CLHS 22.3.5.4).  The
/// symbol itself is returned; applying it selects its global function, and an
/// unbound one signals UNDEFINED-FUNCTION with that symbol.
fn format_function_designator(name: &str) -> Option<EgclVal> {
    let upper = name.to_uppercase();
    let qualified = if upper.contains(':') {
        upper
    } else {
        format!("COMMON-LISP-USER::{}", upper)
    };
    egcl_compiler::reader::read_symbol_token(&qualified)
        .ok()
        .flatten()
        .filter(|v| v.is_symbol())
}

/// Look up a registered format function by name.
fn lookup_format_function(name: &str) -> Option<EgclVal> {
    let registry = FORMAT_FUNCTION_REGISTRY.lock().unwrap();
    registry
        .as_ref()
        .and_then(|r| r.get(&name.to_uppercase()).copied())
}

// Global default dispatch table
static DEFAULT_DISPATCH: OrderedMutex<Option<Vec<(EgclVal, EgclVal, f64)>>> =
    OrderedMutex::new(LockLevel::GcWorld, 15, "pprint dispatch GC roots", None);

/// NOTE: Leaked allocation — not GC-registered. See make_egcl_string note.
fn ensure_default_table() {
    install_format_global_root_scanner();
    let default_printer = make_egcl_string("default-printer");
    let mut table = DEFAULT_DISPATCH.lock().unwrap();
    if table.is_none() {
        // Default table with a catch-all entry
        *table = Some(vec![(NIL, default_printer, 0.0)]);
    }
}

/// Get the pprint dispatch function for a type.
pub fn pprint_dispatch(_object: EgclVal) -> Result<(EgclVal, bool), EgclError> {
    ensure_default_table();
    let table = DEFAULT_DISPATCH.lock().unwrap();
    if let Some(entries) = table.as_ref() {
        if let Some(entry) = entries.last() {
            return Ok((entry.1, true));
        }
    }
    Ok((NIL, false))
}

/// Check if a EgclVal is a heap-encoded pprint dispatch table
/// (created by copy_pprint_dispatch).
fn is_dispatch_table(v: EgclVal) -> bool {
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
fn read_dispatch_table_entries(table_val: EgclVal) -> Vec<(EgclVal, EgclVal, f64)> {
    unsafe {
        let ptr = table_val.as_ptr();
        let entry_count = *(ptr.add(8) as *const u64) as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for idx in 0..entry_count {
            let base = ptr.add(16 + idx * 24);
            let ts = EgclVal(*(base as *const u64));
            let func = EgclVal(*((base as *const u64).add(1)));
            let prio = *((base as *const f64).add(2));
            entries.push((ts, func, prio));
        }
        entries
    }
}

/// Write entries back to a heap-encoded dispatch table, reallocating if needed.
fn write_dispatch_table_entries(table_val: EgclVal, entries: &[(EgclVal, EgclVal, f64)]) {
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
    type_specifier: EgclVal,
    function: Option<EgclVal>,
    priority: f64,
    table: EgclVal,
) -> Result<(), EgclError> {
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
/// NOTE: Leaked allocation — not GC-registered. See make_egcl_string note.
///
/// When `table` is `None`, copies the current global default dispatch
/// table.  When `Some(t)`, copies the given table `t`.  In either case
/// the returned table is an independent deep copy: subsequent
/// modifications via `set_pprint_dispatch` on one do not affect the
/// other.
pub fn copy_pprint_dispatch(table: Option<EgclVal>) -> Result<EgclVal, EgclError> {
    // Determine which entries to copy: from the given heap-encoded table
    // if provided, otherwise from the global default.
    let entries: Vec<(EgclVal, EgclVal, f64)> = match table {
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
        Ok(EgclVal::from_heap_ptr(ptr))
    }
}

/// The opaque random-state printer, shared with the evaluator.
pub fn random_state_representation(value: EgclVal, escapep: bool) -> String {
    if escapep || print_readably_active() {
        let seed = egcl_rt::random_state::snapshot(value).expect("random-state printer type");
        format!("#.(EGCL::%RANDOM-STATE-FROM-SEED #10r{seed})")
    } else {
        dispatch_print_object(value, escapep).unwrap_or_else(|| "#<RANDOM-STATE>".into())
    }
}
