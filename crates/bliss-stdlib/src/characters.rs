//! Character comparison and case conversion (CLHS 13.2).
//!
//! These live here rather than in `lib/boot.lisp` because they are hot and
//! trivial: a character is an immediate value, so a comparison is a tag check
//! and an integer compare. As boot.lisp `&rest` defuns they cost 37-200x `EQ`
//! -- every 2-argument call allocated a rest list, ran an interpreted `DOLIST`,
//! dispatched `CHAR-CODE` twice and then generic `=`, and `CHAR-EQUAL` consed a
//! fresh closure per call on top of that (bliss-7oa5).
//!
//! CLHS shapes every one of these the same way, and the arity rules are easy to
//! get subtly wrong, so the n-ary structure is expressed once here:
//!
//!   - zero arguments is a PROGRAM-ERROR, not NIL and not T;
//!   - one argument is T (nothing can contradict the ordering);
//!   - the monotonic predicates (`=`, `<`, `>`, `<=`, `>=`) compare ADJACENT
//!     pairs, so `(char< a b c)` is `a<b and b<c` -- O(n);
//!   - `/=` means PAIRWISE distinct, so it must compare every pair -- O(n^2),
//!     and `(char/= #\a #\b #\a)` is NIL even though no two neighbours match.
//!
//! Verified against SBCL, including the two PROGRAM-ERROR cases.

use bliss_rt::value::BlissVal;

/// How a pair of characters is reduced to the integer being compared.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharKey {
    /// `CHAR=` family: the code point as-is.
    Exact,
    /// `CHAR-EQUAL` family: case-folded, so `#\a` and `#\A` compare equal.
    ///
    /// CLHS says these "ignore differences in case", which it defines via
    /// CHAR-UPCASE -- fold UP, not down. The two differ for characters whose
    /// up- and down-case mappings are not symmetric, so the direction matters.
    ///
    /// Folds with the SAME ASCII rule `convert_case` uses; see the note there
    /// on why this is not Unicode-aware.
    Folded,
}

/// The ordering a comparison accepts between adjacent elements.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharCmp {
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
    /// Pairwise distinct -- not the negation of `Eq` for more than 2 arguments.
    Ne,
}

/// Why a character comparison could not be performed.
pub enum CharCmpError {
    /// An argument was not a character; carries it for the TYPE-ERROR datum.
    NotACharacter(BlissVal),
    /// Called with no arguments at all.
    NoArguments,
}

/// The integer `v` contributes to a comparison under `key`.
#[inline]
fn char_key(v: BlissVal, key: CharKey) -> Result<u32, CharCmpError> {
    if !v.is_character() {
        return Err(CharCmpError::NotACharacter(v));
    }
    let c = v.as_char();
    Ok(match key {
        CharKey::Exact => c as u32,
        CharKey::Folded => ascii_upcase(c) as u32,
    })
}

#[inline]
fn ordered(a: u32, b: u32, cmp: CharCmp) -> bool {
    match cmp {
        CharCmp::Eq => a == b,
        CharCmp::Lt => a < b,
        CharCmp::Gt => a > b,
        CharCmp::Le => a <= b,
        CharCmp::Ge => a >= b,
        // Handled by the pairwise loop in `compare`; never reached adjacently.
        CharCmp::Ne => a != b,
    }
}

/// The whole CHAR= / CHAR-EQUAL family, for any arity.
///
/// Every argument is type-checked even when an earlier pair already decides the
/// answer. CLHS requires the arguments to BE characters, so a short-circuit
/// that skipped the check would let `(char= #\a #\b 5)` quietly answer NIL --
/// the recurring "a branch that skips the work also skips the validation" bug
/// in this tree (LOGEQV, GCD/LCM, ASH, ISQRT, BOOLE).
pub fn compare(args: &[BlissVal], key: CharKey, cmp: CharCmp) -> Result<bool, CharCmpError> {
    if args.is_empty() {
        return Err(CharCmpError::NoArguments);
    }
    let mut keys = Vec::with_capacity(args.len());
    for &a in args {
        keys.push(char_key(a, key)?);
    }
    if cmp == CharCmp::Ne {
        // Pairwise distinct: O(n^2) and genuinely not the negation of Eq.
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                if keys[i] == keys[j] {
                    return Ok(false);
                }
            }
        }
        return Ok(true);
    }
    Ok(keys.windows(2).all(|w| ordered(w[0], w[1], cmp)))
}

/// Two-argument fast path: no Vec, no allocation at all.
///
/// Correct for `Ne` as well -- with exactly two arguments "pairwise distinct"
/// and "adjacent pair differs" coincide.
#[inline]
pub fn compare2(a: BlissVal, b: BlissVal, key: CharKey, cmp: CharCmp) -> Result<bool, CharCmpError> {
    let ka = char_key(a, key)?;
    let kb = char_key(b, key)?;
    Ok(ordered(ka, kb, cmp))
}

/// ASCII-only case mapping, matching UPPER-CASE-P / LOWER-CASE-P.
///
/// Deliberately NOT Unicode. CLHS ties CHAR-UPCASE to which characters HAVE
/// case, and in bliss that is decided by UPPER-CASE-P / LOWER-CASE-P, which are
/// boot.lisp defuns testing the ASCII ranges only. Using Rust's Unicode mapping
/// here made the two disagree: for a character like U+00E9, LOWER-CASE-P said
/// NIL while CHAR-UPCASE still changed it, breaking the ansi CHAR-UPCASE.2
/// invariant `(or (lower-case-p x) (eql (char-upcase x) x))` over all 65536
/// code points -- CHAR-UPCASE.2 / CHAR-DOWNCASE.2.
///
/// Making the whole case system Unicode-aware is a real improvement, but it is
/// a semantics change that belongs with UPPER-CASE-P/LOWER-CASE-P/BOTH-CASE-P,
/// not smuggled in with a perf fix. Tracked separately.
#[inline]
fn ascii_upcase(c: char) -> char {
    if c.is_ascii_lowercase() {
        ((c as u8) - 32) as char
    } else {
        c
    }
}

#[inline]
fn ascii_downcase(c: char) -> char {
    if c.is_ascii_uppercase() {
        ((c as u8) + 32) as char
    } else {
        c
    }
}

/// `CHAR-UPCASE` / `CHAR-DOWNCASE`.
///
/// CLHS: a character with no case, or already in the requested case, is
/// returned unchanged. ASCII-only, for the reason on `ascii_upcase`.
pub fn convert_case(v: BlissVal, up: bool) -> Result<BlissVal, CharCmpError> {
    if !v.is_character() {
        return Err(CharCmpError::NotACharacter(v));
    }
    let c = v.as_char();
    Ok(BlissVal::from_char(if up {
        ascii_upcase(c)
    } else {
        ascii_downcase(c)
    }))
}
