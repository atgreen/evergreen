// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Leaf helpers for builtin calls from compiled code (bliss-gg3i1.7.3).
//!
//! A compiled call to a builtin otherwise crosses the full runtime boundary:
//! the T1 bridge or the ordinary named adapter, root publication, the unwind
//! guard, the safepoint poll and builtin dispatch -- ~300 ns or more, against a
//! few ns of work for `(eql a b)` or `(svref v i)`.
//!
//! Each helper here answers its builtin exactly as `apply_builtin_fast` would
//! for the arguments it accepts, and UNBOUND for everything else, with no
//! effect; the caller then makes the unchanged call, which handles the rest
//! and signals any error. A helper never allocates on the Lisp heap, collects,
//! polls, signals, unwinds or blocks: it runs without root publication. On a
//! hit it leaves the multiple-value buffer empty and inactive, as a single
//! value return must.
//!
//! The helpers are reached three ways:
//! - ordinary-ABI T2 call sites place a fast path in front of the call
//!   (`egcl_compiler::t2::emit::LeafHelpers`);
//! - a builtin's ordinary named adapter starts with a stub that tries the
//!   helper before the runtime veneer ([`ordinary_stub`]);
//! - a builtin's T1 register entry is a stub that tries the helper before the
//!   bridge ([`t1_register_entry`]).

#![cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]

use super::*;

const MISS: u64 = egcl_rt::value::UNBOUND.0;

/// A single-value result: empty the execution's multiple-value buffer, as a
/// single-value return does, unless the caller (`C` false) proved it empty.
#[inline]
fn single<const C: bool>(value: EgclVal) -> u64 {
    if !C {
        return value.0;
    }
    let env = NATIVE_ENV.with(std::cell::Cell::get);
    if !env.is_null() {
        // SAFETY: the parked Env of the running native activation; no Rust
        // borrow of it spans generated code.
        unsafe { (*env).clear_mv() };
    }
    value.0
}

#[inline]
fn boolean<const C: bool>(yes: bool) -> u64 {
    single::<C>(if yes { T } else { NIL })
}

/// Run a helper body that calls library code, turning a panic into a miss
/// (the general path then reports it) instead of unwinding into native code.
#[inline]
fn guarded(body: impl FnOnce() -> u64) -> u64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(MISS)
}

/// An integer with at most one 64-bit limb of magnitude: every fixnum and the
/// one-limb bignums, read without allocating.
#[inline]
fn small_integer(v: EgclVal) -> Option<i128> {
    if v.is_fixnum() {
        return Some(i128::from(v.as_fixnum()));
    }
    if !v.is_heap_object() {
        return None;
    }
    // SAFETY: a live tagged heap object; the header and limb count are read
    // before the payload, as egcl_rt::bignum does.
    unsafe {
        let ptr = v.as_ptr();
        if (*(ptr as *const egcl_rt::object::ObjectHeader)).type_id()
            != egcl_rt::object::type_id::BIGNUM
            || *(ptr.add(12) as *const u32) != 1
        {
            return None;
        }
        let sign = *(ptr.add(8) as *const i32);
        let magnitude = i128::from(*(ptr.add(16) as *const u64));
        match sign {
            1 => Some(magnitude),
            -1 => Some(-magnitude),
            _ => None,
        }
    }
}

/// A result that is a fixnum; a larger one would need a bignum allocation.
#[inline]
fn integer_result<const C: bool>(n: i128) -> u64 {
    match egcl_rt::bignum::fixnum_from_i128(n) {
        Some(value) => single::<C>(value),
        None => MISS,
    }
}

/// A global variable's current dynamic binding or global value.
extern "C" fn load_global(symbol: u64) -> u64 {
    match egcl_rt::symbols::symbol_value(symbol as u32) {
        Some(value) if value != egcl_rt::value::UNBOUND => single::<true>(value),
        _ => MISS,
    }
}

// ── Lists and identity ─────────────────────────────────────────────

extern "C" fn eq<const C: bool>(a: u64, b: u64) -> u64 {
    boolean::<C>(a == b)
}

extern "C" fn eql<const C: bool>(a: u64, b: u64) -> u64 {
    guarded(|| boolean::<C>(egcl_stdlib::equality::eql(EgclVal(a), EgclVal(b))))
}

extern "C" fn equal<const C: bool>(a: u64, b: u64) -> u64 {
    guarded(|| boolean::<C>(egcl_stdlib::equality::equal(EgclVal(a), EgclVal(b))))
}

extern "C" fn equalp<const C: bool>(a: u64, b: u64) -> u64 {
    guarded(|| boolean::<C>(egcl_stdlib::equality::equalp(EgclVal(a), EgclVal(b))))
}

extern "C" fn null<const C: bool>(x: u64) -> u64 {
    boolean::<C>(EgclVal(x).is_nil())
}

extern "C" fn consp<const C: bool>(x: u64) -> u64 {
    boolean::<C>(EgclVal(x).is_cons())
}

extern "C" fn endp<const C: bool>(x: u64) -> u64 {
    let x = EgclVal(x);
    if x.is_nil() {
        single::<C>(T)
    } else if x.is_cons() {
        single::<C>(NIL)
    } else {
        MISS
    }
}

extern "C" fn car<const C: bool>(x: u64) -> u64 {
    let x = EgclVal(x);
    if x.is_nil() {
        single::<C>(NIL)
    } else if x.is_cons() {
        // SAFETY: a live cons.
        single::<C>(unsafe { (*(x.as_ptr() as *const egcl_rt::object::ConsCell)).car })
    } else {
        MISS
    }
}

extern "C" fn cdr<const C: bool>(x: u64) -> u64 {
    let x = EgclVal(x);
    if x.is_nil() {
        single::<C>(NIL)
    } else if x.is_cons() {
        // SAFETY: a live cons.
        single::<C>(unsafe { (*(x.as_ptr() as *const egcl_rt::object::ConsCell)).cdr })
    } else {
        MISS
    }
}

// ── Sequences and hash tables ──────────────────────────────────────

extern "C" fn length<const C: bool>(sequence: u64) -> u64 {
    guarded(|| match egcl_stdlib::length(EgclVal(sequence)) {
        Ok(n) => single::<C>(EgclVal::from_fixnum(n as i64)),
        Err(_) => MISS,
    })
}

fn index(i: u64) -> Option<usize> {
    let i = EgclVal(i);
    (i.is_fixnum() && i.as_fixnum() >= 0).then(|| i.as_fixnum() as usize)
}

extern "C" fn elt<const C: bool>(sequence: u64, i: u64) -> u64 {
    guarded(|| {
        match index(i).and_then(|i| egcl_stdlib::sequences::elt_leaf(EgclVal(sequence), i)) {
            Some(value) => single::<C>(value),
            None => MISS,
        }
    })
}

extern "C" fn aref<const C: bool>(array: u64, i: u64) -> u64 {
    guarded(
        || match index(i).and_then(|i| egcl_stdlib::sequences::aref_leaf(EgclVal(array), i)) {
            Some(value) => single::<C>(value),
            None => MISS,
        },
    )
}

extern "C" fn svref<const C: bool>(vector: u64, i: u64) -> u64 {
    guarded(|| {
        match index(i).and_then(|i| egcl_stdlib::sequences::svref_leaf(EgclVal(vector), i)) {
            Some(value) => single::<C>(value),
            None => MISS,
        }
    })
}

/// `(setf (aref array i) value)`, compiled as EGCL::SET-AREF: the value.
extern "C" fn set_aref<const C: bool>(array: u64, i: u64, value: u64) -> u64 {
    guarded(|| {
        match index(i).is_some_and(|i| {
            egcl_stdlib::sequences::set_aref_leaf(EgclVal(array), i, EgclVal(value))
        }) {
            true => single::<C>(EgclVal(value)),
            false => MISS,
        }
    })
}

/// `(setf (svref vector i) value)`, compiled as EGCL::SET-SVREF: the value.
extern "C" fn set_svref<const C: bool>(vector: u64, i: u64, value: u64) -> u64 {
    guarded(|| {
        match index(i).is_some_and(|i| {
            egcl_stdlib::sequences::set_svref_leaf(EgclVal(vector), i, EgclVal(value))
        }) {
            true => single::<C>(EgclVal(value)),
            false => MISS,
        }
    })
}

/// `(gethash key table)` where the second value is discarded.
extern "C" fn gethash2<const C: bool>(key: u64, table: u64) -> u64 {
    guarded(
        || match egcl_stdlib::gethash_leaf(EgclVal(key), EgclVal(table), NIL) {
            Some((value, _)) => single::<C>(value),
            None => MISS,
        },
    )
}

/// `(gethash key table default)` where the second value is discarded.
extern "C" fn gethash3<const C: bool>(key: u64, table: u64, default: u64) -> u64 {
    guarded(
        || match egcl_stdlib::gethash_leaf(EgclVal(key), EgclVal(table), EgclVal(default)) {
            Some((value, _)) => single::<C>(value),
            None => MISS,
        },
    )
}

// ── Integers ───────────────────────────────────────────────────────
//
// Fixnums and one-limb bignums, with fixnum results only. Every other
// argument (another number type, a wider bignum, a non-number) and every
// result that needs a bignum misses.

fn integers(a: u64, b: u64) -> Option<(i128, i128)> {
    Some((small_integer(EgclVal(a))?, small_integer(EgclVal(b))?))
}

macro_rules! compare {
    ($name:ident, $op:tt) => {
        extern "C" fn $name<const C: bool>(a: u64, b: u64) -> u64 {
            match integers(a, b) {
                Some((a, b)) => boolean::<C>(a $op b),
                None => MISS,
            }
        }
    };
}
compare!(num_eq, ==);
compare!(num_ne, !=);
compare!(num_lt, <);
compare!(num_le, <=);
compare!(num_gt, >);
compare!(num_ge, >=);

macro_rules! arith {
    ($name:ident, |$a:ident, $b:ident| $body:expr) => {
        extern "C" fn $name<const C: bool>(a: u64, b: u64) -> u64 {
            match integers(a, b) {
                Some(($a, $b)) => match $body {
                    Some(n) => integer_result::<C>(n),
                    None => MISS,
                },
                None => MISS,
            }
        }
    };
}
arith!(add, |a, b| a.checked_add(b));
arith!(sub, |a, b| a.checked_sub(b));
arith!(mul, |a, b| a.checked_mul(b));
// Two's-complement bit operations are exact on the i128 forms.
arith!(logand, |a, b| Some(a & b));
arith!(logior, |a, b| Some(a | b));
arith!(logxor, |a, b| Some(a ^ b));

extern "C" fn one_plus<const C: bool>(a: u64) -> u64 {
    match small_integer(EgclVal(a)) {
        Some(a) => integer_result::<C>(a + 1),
        None => MISS,
    }
}

extern "C" fn one_minus<const C: bool>(a: u64) -> u64 {
    match small_integer(EgclVal(a)) {
        Some(a) => integer_result::<C>(a - 1),
        None => MISS,
    }
}

extern "C" fn zerop<const C: bool>(a: u64) -> u64 {
    match small_integer(EgclVal(a)) {
        Some(a) => boolean::<C>(a == 0),
        None => MISS,
    }
}

extern "C" fn ash<const C: bool>(value: u64, count: u64) -> u64 {
    let (Some(n), count) = (small_integer(EgclVal(value)), EgclVal(count)) else {
        return MISS;
    };
    if !count.is_fixnum() {
        return MISS;
    }
    let shift = count.as_fixnum();
    if shift <= 0 {
        return integer_result::<C>(n >> shift.unsigned_abs().min(127));
    }
    if n == 0 {
        return single::<C>(EgclVal::from_fixnum(0));
    }
    // |n| < 2^64, so a shift below 63 cannot overflow i128; any larger shift
    // of a nonzero integer leaves the fixnum range.
    if shift < 63 {
        integer_result::<C>(n << shift)
    } else {
        MISS
    }
}

extern "C" fn logbitp<const C: bool>(i: u64, n: u64) -> u64 {
    let (i, Some(n)) = (EgclVal(i), small_integer(EgclVal(n))) else {
        return MISS;
    };
    if !i.is_fixnum() || i.as_fixnum() < 0 {
        return MISS;
    }
    boolean::<C>((n >> (i.as_fixnum() as u64).min(127)) & 1 == 1)
}

/// A helper's two entries: the one that empties the multiple-value buffer
/// and the one for callers that proved it empty.
#[derive(Clone, Copy)]
struct Entry {
    clearing: u64,
    preserving: u64,
}

macro_rules! entry {
    ($f:ident, $ty:ty) => {
        Entry {
            clearing: $f::<true> as $ty as usize as u64,
            preserving: $f::<false> as $ty as usize as u64,
        }
    };
}

type Helper1 = extern "C" fn(u64) -> u64;
type Helper2 = extern "C" fn(u64, u64) -> u64;
type Helper3 = extern "C" fn(u64, u64, u64) -> u64;

/// `(name, argument count, helper, primary value only)`.
fn table() -> Vec<(&'static str, u8, Entry, bool)> {
    vec![
        ("EQ", 2, entry!(eq, Helper2), false),
        ("EQL", 2, entry!(eql, Helper2), false),
        ("EQUAL", 2, entry!(equal, Helper2), false),
        ("EQUALP", 2, entry!(equalp, Helper2), false),
        ("NULL", 1, entry!(null, Helper1), false),
        ("NOT", 1, entry!(null, Helper1), false),
        ("CONSP", 1, entry!(consp, Helper1), false),
        ("ENDP", 1, entry!(endp, Helper1), false),
        ("CAR", 1, entry!(car, Helper1), false),
        ("FIRST", 1, entry!(car, Helper1), false),
        ("CDR", 1, entry!(cdr, Helper1), false),
        ("REST", 1, entry!(cdr, Helper1), false),
        ("LENGTH", 1, entry!(length, Helper1), false),
        ("ELT", 2, entry!(elt, Helper2), false),
        ("AREF", 2, entry!(aref, Helper2), false),
        ("SVREF", 2, entry!(svref, Helper2), false),
        ("EGCL::SET-AREF", 3, entry!(set_aref, Helper3), false),
        ("EGCL::SET-SVREF", 3, entry!(set_svref, Helper3), false),
        ("GETHASH", 2, entry!(gethash2, Helper2), true),
        ("GETHASH", 3, entry!(gethash3, Helper3), true),
        ("=", 2, entry!(num_eq, Helper2), false),
        ("/=", 2, entry!(num_ne, Helper2), false),
        ("<", 2, entry!(num_lt, Helper2), false),
        ("<=", 2, entry!(num_le, Helper2), false),
        (">", 2, entry!(num_gt, Helper2), false),
        (">=", 2, entry!(num_ge, Helper2), false),
        ("+", 2, entry!(add, Helper2), false),
        ("-", 2, entry!(sub, Helper2), false),
        ("*", 2, entry!(mul, Helper2), false),
        ("1+", 1, entry!(one_plus, Helper1), false),
        ("1-", 1, entry!(one_minus, Helper1), false),
        ("ZEROP", 1, entry!(zerop, Helper1), false),
        ("LOGAND", 2, entry!(logand, Helper2), false),
        ("LOGIOR", 2, entry!(logior, Helper2), false),
        ("LOGXOR", 2, entry!(logxor, Helper2), false),
        ("ASH", 2, entry!(ash, Helper2), false),
        ("LOGBITP", 2, entry!(logbitp, Helper2), false),
    ]
}

fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var_os("EGCL_NO_LEAF_BUILTINS").is_some())
}

/// `symbol -> [(nargs, helper, primary_only)]`.
type HelperMap = HashMap<u32, Vec<(u8, u64, bool)>>;
/// `(nargs, helper)` per single-value helper, and the function-cell guard.
type StubPlan = (Vec<(u8, u64)>, Option<(usize, u8)>);

/// The stubs' helpers, built once.
fn helpers() -> &'static HelperMap {
    static HELPERS: std::sync::OnceLock<HelperMap> = std::sync::OnceLock::new();
    HELPERS.get_or_init(|| {
        // Neither stub can prove the caller's buffer empty.
        let mut map = HelperMap::new();
        for (name, nargs, helper, primary_only) in table() {
            map.entry(egcl_rt::symbols::intern(name))
                .or_default()
                .push((nargs, helper.clearing, primary_only));
        }
        map
    })
}

/// The single-value helpers for `symbol` and the function-cell guard their
/// stubs need, `None` when there are none or the builtin has been redefined.
#[cfg(all(target_arch = "x86_64", unix))]
fn stub_plan(symbol: u32) -> Option<StubPlan> {
    if disabled() {
        return None;
    }
    let entries: Vec<(u8, u64)> = helpers()
        .get(&symbol)?
        .iter()
        .filter(|(_, _, primary_only)| !primary_only)
        .map(|&(nargs, helper, _)| (nargs, helper))
        .collect();
    if entries.is_empty() {
        return None;
    }
    // A replaced function cell must reach the general path; the builtin's
    // absent binding is a small immediate.
    let guard = match egcl_compiler::t2::builtin_binding::snapshot(symbol)? {
        egcl_compiler::t2::builtin_binding::Binding::FixedStandard => None,
        egcl_compiler::t2::builtin_binding::Binding::Guarded { address, expected } => {
            Some((address, u8::try_from(expected).ok().filter(|&e| e < 0x80)?))
        }
    };
    Some((entries, guard))
}

/// `cmp qword [r11 = address], expected; jne miss` with a rel32 placeholder,
/// returning the placeholder's offset.
#[cfg(all(target_arch = "x86_64", unix))]
fn emit_guard(code: &mut Vec<u8>, guard: Option<(usize, u8)>, misses: &mut Vec<usize>) {
    if let Some((address, expected)) = guard {
        code.extend_from_slice(&[0x49, 0xBB]); // mov r11, imm64
        code.extend_from_slice(&(address as u64).to_le_bytes());
        code.extend_from_slice(&[0x49, 0x83, 0x3B, expected]); // cmp qword [r11], imm8
        jcc_rel32(code, 0x85, misses); // jne miss
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
fn jcc_rel32(code: &mut Vec<u8>, cc: u8, fixups: &mut Vec<usize>) {
    code.extend_from_slice(&[0x0F, cc, 0, 0, 0, 0]);
    fixups.push(code.len() - 4);
}

#[cfg(all(target_arch = "x86_64", unix))]
fn patch(code: &mut [u8], fixups: &[usize], target: usize) {
    for &at in fixups {
        let rel = i32::try_from(target as i64 - (at as i64 + 4)).expect("stub branch");
        code[at..at + 4].copy_from_slice(&rel.to_le_bytes());
    }
}

/// `call r11 = helper`.
#[cfg(all(target_arch = "x86_64", unix))]
fn call_helper(code: &mut Vec<u8>, helper: u64) {
    code.extend_from_slice(&[0x49, 0xBB]);
    code.extend_from_slice(&helper.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xFF, 0xD3]);
}

/// The prefix of a builtin's ordinary named adapter: with the supplied count
/// (RCX) of a helper and the arguments in RDI, RSI, RDX, call the helper and
/// return its value as a single-value ordinary return (CF clear). Any other
/// count, a redefinition or a miss restores RAX, RCX, RDX, RSI, RDI and falls
/// through to the veneer that follows. Empty when the symbol has no helper.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn ordinary_stub(symbol: u32) -> Vec<u8> {
    let Some((entries, guard)) = stub_plan(symbol) else {
        return Vec::new();
    };
    let mut code = vec![0xF3, 0x0F, 0x1E, 0xFA]; // endbr64
    let mut misses = Vec::new();
    emit_guard(&mut code, guard, &mut misses);
    let mut restores = Vec::new();
    for (nargs, helper) in entries {
        // cmp rcx, nargs; jne next
        code.extend_from_slice(&[0x48, 0x83, 0xF9, nargs]);
        let mut next = Vec::new();
        jcc_rel32(&mut code, 0x85, &mut next);
        // Entry RSP is 8 mod 16; five pushes align the helper call.
        code.extend_from_slice(&[0x50, 0x51, 0x52, 0x56, 0x57]); // push rax, rcx, rdx, rsi, rdi
        call_helper(&mut code, helper);
        code.extend_from_slice(&[0x48, 0x83, 0xF8, egcl_rt::value::UNBOUND_BITS as u8]); // cmp rax, UNBOUND
        jcc_rel32(&mut code, 0x84, &mut restores); // je restore
        code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x28]); // add rsp, 40
        code.extend_from_slice(&[0xF8, 0xC3]); // clc; ret
        let here = code.len();
        patch(&mut code, &next, here);
    }
    // The last count test falls through here too: no registers were pushed.
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]); // jmp veneer
    let jump = code.len() - 4;
    let restore = code.len();
    code.extend_from_slice(&[0x5F, 0x5E, 0x5A, 0x59, 0x58]); // pop rdi, rsi, rdx, rcx, rax
    let veneer = code.len();
    patch(&mut code, &restores, restore);
    patch(&mut code, &misses, veneer);
    patch(&mut code, &[jump], veneer);
    code
}

/// A register entry `(cell, n, a0, a1, a2, r9)` for a builtin's call cell:
/// with a helper's count in RSI, call it on the arguments and return its value
/// as a single value (CF clear); otherwise -- another count, a redefinition, a
/// miss -- jump to `fallback` with RDI, RSI, RDX, RCX, R8 and R9 intact. The
/// T1 checked entry and the mapped native entry share this convention.
/// `None` when the symbol has no single-value helper. Stubs are cached per
/// (symbol, fallback) and live for the process, like the entries they front.
#[cfg(all(target_arch = "x86_64", unix))]
pub(super) fn register_stub(symbol: u32, fallback: usize) -> Option<usize> {
    type Stubs = HashMap<(u32, usize), Option<usize>>;
    static STUBS: std::sync::Mutex<Option<Stubs>> = std::sync::Mutex::new(None);
    let mut stubs = STUBS.lock().unwrap_or_else(|e| e.into_inner());
    let stubs = stubs.get_or_insert_with(HashMap::new);
    if let Some(&stub) = stubs.get(&(symbol, fallback)) {
        return stub;
    }
    // A fallback that is itself one of these stubs already answers.
    if stubs.values().any(|&stub| stub == Some(fallback)) {
        return None;
    }
    let stub = build_register_stub(symbol, fallback);
    stubs.insert((symbol, fallback), stub);
    stub
}

#[cfg(all(target_arch = "x86_64", unix))]
fn build_register_stub(symbol: u32, fallback: usize) -> Option<usize> {
    let (helpers, guard) = stub_plan(symbol)?;
    let mut code = vec![0xF3, 0x0F, 0x1E, 0xFA]; // endbr64
    let mut misses = Vec::new();
    emit_guard(&mut code, guard, &mut misses);
    for (nargs, helper) in helpers {
        code.extend_from_slice(&[0x48, 0x83, 0xFE, nargs]); // cmp rsi, nargs
        let mut next = Vec::new();
        jcc_rel32(&mut code, 0x85, &mut next);
        // push rdi, rsi, rdx, rcx, r8, r9; sub rsp, 8: aligned at the call.
        code.extend_from_slice(&[0x57, 0x56, 0x52, 0x51, 0x41, 0x50, 0x41, 0x51]);
        code.extend_from_slice(&[0x48, 0x83, 0xEC, 0x08]);
        // rdi, rsi, rdx = a0, a1, a2
        code.extend_from_slice(&[0x48, 0x89, 0xD7, 0x48, 0x89, 0xCE, 0x4C, 0x89, 0xC2]);
        call_helper(&mut code, helper);
        // add rsp, 8; pop r9, r8, rcx, rdx, rsi, rdi
        code.extend_from_slice(&[0x48, 0x83, 0xC4, 0x08]);
        code.extend_from_slice(&[0x41, 0x59, 0x41, 0x58, 0x59, 0x5A, 0x5E, 0x5F]);
        code.extend_from_slice(&[0x48, 0x83, 0xF8, egcl_rt::value::UNBOUND_BITS as u8]); // cmp rax, UNBOUND
        jcc_rel32(&mut code, 0x84, &mut misses); // je miss
        code.extend_from_slice(&[0xF8, 0xC3]); // clc; ret: one value
        let here = code.len();
        patch(&mut code, &next, here);
    }
    let miss = code.len();
    code.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]); // jmp [rip]
    code.extend_from_slice(&(fallback as u64).to_le_bytes());
    patch(&mut code, &misses, miss);
    let buffer = egcl_rt::jit::JitBuffer::new(&code)?;
    let entry = buffer.as_ptr() as usize;
    std::mem::forget(buffer);
    Some(entry)
}

/// Front a builtin cell's mapped native register entry with its leaf stub. The
/// stub falls back to the entry it replaces for every case it does not answer,
/// including a later redefinition, so it never needs to be withdrawn.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub(super) fn front_native_entry(cell: &egcl_rt::call_table::CallCell, symbol: u32) {
    use std::sync::atomic::Ordering;
    // SAFETY: the cell's own stable entry slot.
    let slot = unsafe { &*cell.entry_address(false) };
    let current = slot.load(Ordering::Acquire);
    if current == 0 || std::ptr::eq(slot, cell.checked_entry_address(false)) {
        return;
    }
    if let Some(stub) = register_stub(symbol, current) {
        slot.store(stub, Ordering::Release);
    }
}

/// Install the helpers for the T2 emitter.
#[cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_t2)))]
pub(super) fn install() {
    use egcl_compiler::t2::emit::{LeafBuiltin, LeafHelpers, install_leaf_helpers};
    if disabled() {
        return;
    }
    let builtins = table()
        .into_iter()
        .map(|(name, nargs, helper, primary_only)| LeafBuiltin {
            symbol: egcl_rt::symbols::intern(name),
            nargs,
            helper: helper.clearing,
            helper_mv_empty: helper.preserving,
            primary_only,
        })
        .collect();
    install_leaf_helpers(LeafHelpers {
        builtins,
        load_global: load_global as Helper1 as usize as u64,
        bind_special: bind_special::<true> as Helper3 as usize as u64,
        bind_special_keep_values: bind_special::<false> as Helper3 as usize as u64,
        unbind_special: unbind_special::<true> as Helper1 as usize as u64,
        unbind_special_keep_values: unbind_special::<false> as Helper1 as usize as u64,
    });
}

/// Establish a special binding owned by an ordinary native frame
/// (bliss-gg3i1.7.4). The segment the frame runs in restores it if the frame
/// is abandoned; see `egcl_rt::symbols::native_bind_symbol_value`. `C`
/// false leaves the multiple values alone, as the interpreter's binding does.
/// `object` is the symbol's pinned object, or 0.
extern "C" fn bind_special<const C: bool>(symbol: u64, value: u64, object: u64) -> u64 {
    egcl_rt::symbols::native_bind_symbol_value(symbol as u32, EgclVal(value), object as usize);
    single::<C>(NIL)
}

/// Restore the `count` newest native special bindings. `C` false leaves the
/// multiple values alone: the exit forwards a call's complete result. The
/// compiled frame holds every binding it unbinds.
extern "C" fn unbind_special<const C: bool>(count: u64) -> u64 {
    let restored = egcl_rt::symbols::native_unbind_symbol_values(count as usize);
    debug_assert!(restored, "unbinding more native bindings than are held");
    single::<C>(NIL)
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_t2))))]
pub(super) fn install() {}
