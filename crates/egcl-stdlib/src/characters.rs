// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

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

use egcl_rt::value::EgclVal;

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
    /// Folds with [`char_upcase`], the same Unicode 1:1 mapping CHAR-UPCASE
    /// uses, so the two always agree.
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
    NotACharacter(EgclVal),
    /// Called with no arguments at all.
    NoArguments,
}

/// The integer `v` contributes to a comparison under `key`.
#[inline]
fn char_key(v: EgclVal, key: CharKey) -> Result<u32, CharCmpError> {
    if !v.is_character() {
        return Err(CharCmpError::NotACharacter(v));
    }
    let c = v.as_char();
    Ok(match key {
        CharKey::Exact => c as u32,
        CharKey::Folded => char_upcase(c) as u32,
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
pub fn compare(args: &[EgclVal], key: CharKey, cmp: CharCmp) -> Result<bool, CharCmpError> {
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
pub fn compare2(
    a: EgclVal,
    b: EgclVal,
    key: CharKey,
    cmp: CharCmp,
) -> Result<bool, CharCmpError> {
    let ka = char_key(a, key)?;
    let kb = char_key(b, key)?;
    Ok(ordered(ka, kb, cmp))
}

/// The one character `it` yields, or `None` when it yields none or several.
#[inline]
fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let c = it.next()?;
    if it.next().is_some() {
        None
    } else {
        Some(c)
    }
}

/// Characters Unicode gives case mappings to that are not letters: the Roman
/// numerals (U+2160..U+217F, category Nl) and the circled Latin letters
/// (U+24B6..U+24E9, category So). CLHS 13.1.4.3 makes the characters with case
/// a subset of the alphabetic characters, and SBCL treats both blocks as
/// having no case, so they are excluded here too. Each block holds both
/// members of every pair, so excluding them keeps the mapping symmetric.
#[inline]
fn non_letter_with_case(c: char) -> bool {
    matches!(c as u32, 0x2160..=0x217F | 0x24B6..=0x24E9)
}

/// `CHAR-UPCASE` on a Rust `char`: the Unicode simple uppercase mapping of a
/// lowercase character with case, or `c` itself.
///
/// CLHS 13.1.4.3 requires a character with case to map one-to-one onto its
/// counterpart, so a mapping is used only when it yields exactly one
/// character AND that character's lowercase mapping is `c` again. That rules
/// out U+00DF (sharp s, whose uppercase is "SS"), U+00B5 (micro sign, whose
/// uppercase U+039C lowercases to U+03BC), U+0131 (dotless i, uppercased to an
/// I that lowercases to i) and the titlecase digraphs such as U+01C5. All of
/// those have no case and are returned unchanged.
///
/// UPPER-CASE-P, LOWER-CASE-P, BOTH-CASE-P, the CHAR-EQUAL family, EQUALP and
/// the string case operators are all defined from this function and
/// [`char_downcase`], so they cannot disagree: ansi CHAR-UPCASE.2 /
/// CHAR-DOWNCASE.2 check exactly that over every code point (bliss-ne14).
#[inline]
pub fn char_upcase(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    match single(c.to_uppercase()) {
        Some(u) if u != c && single(u.to_lowercase()) == Some(c) && !non_letter_with_case(c) => u,
        _ => c,
    }
}

/// `CHAR-DOWNCASE` on a Rust `char`; the mirror image of [`char_upcase`].
#[inline]
pub fn char_downcase(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    match single(c.to_lowercase()) {
        Some(l) if l != c && single(l.to_uppercase()) == Some(c) && !non_letter_with_case(c) => l,
        _ => c,
    }
}

/// `UPPER-CASE-P`: `c` has case and is the uppercase member of its pair.
#[inline]
pub fn upper_case_p(c: char) -> bool {
    char_downcase(c) != c
}

/// `LOWER-CASE-P`: `c` has case and is the lowercase member of its pair.
#[inline]
pub fn lower_case_p(c: char) -> bool {
    char_upcase(c) != c
}

/// `BOTH-CASE-P`: `c` is a character with case.
#[inline]
pub fn both_case_p(c: char) -> bool {
    upper_case_p(c) || lower_case_p(c)
}

/// `CHAR-EQUAL` on two Rust `char`s: equal after folding both up.
#[inline]
pub fn char_equal(a: char, b: char) -> bool {
    a == b || char_upcase(a) == char_upcase(b)
}

/// `CHAR-UPCASE` / `CHAR-DOWNCASE`.
///
/// CLHS: a character with no case, or already in the requested case, is
/// returned unchanged. See [`char_upcase`] for which characters have case.
pub fn convert_case(v: EgclVal, up: bool) -> Result<EgclVal, CharCmpError> {
    if !v.is_character() {
        return Err(CharCmpError::NotACharacter(v));
    }
    let c = v.as_char();
    Ok(EgclVal::from_char(if up {
        char_upcase(c)
    } else {
        char_downcase(c)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(code: u32) -> char {
        char::from_u32(code).unwrap()
    }

    /// Spot values produced by SBCL (bliss-ne14).
    #[test]
    fn case_mapping_matches_sbcl() {
        // (char-upcase e-acute) => E-acute and back; Greek and Cyrillic pairs.
        assert_eq!(char_upcase(ch(0xE9)), ch(0xC9));
        assert_eq!(char_downcase(ch(0xC9)), ch(0xE9));
        assert_eq!(char_upcase(ch(0xFF)), ch(0x178));
        assert_eq!(char_upcase('ω'), 'Ω');
        assert_eq!(char_downcase('Ж'), 'ж');
        assert!(lower_case_p(ch(0xE9)) && !upper_case_p(ch(0xE9)));
        assert!(upper_case_p(ch(0xC9)) && !lower_case_p(ch(0xC9)));
        assert!(char_equal(ch(0xE9), ch(0xC9)));
        // No 1:1 counterpart, so no case: sharp s, micro sign, dotless i,
        // dotted capital I, Kelvin sign, Roman numerals, circled letters.
        for code in [
            0xDF, 0xB5, 0x131, 0x130, 0x212A, 0x2160, 0x2170, 0x24B6, 0x24D0,
        ] {
            let c = ch(code);
            assert_eq!(char_upcase(c), c, "{code:#x}");
            assert_eq!(char_downcase(c), c, "{code:#x}");
            assert!(!both_case_p(c), "{code:#x}");
        }
        assert!(!char_equal(ch(0xDF), 'S'));
        assert!(!char_equal(ch(0x212A), 'k'));
        // Uncased characters are untouched.
        for c in ['1', ' ', '漢', '-'] {
            assert_eq!(char_upcase(c), c);
            assert!(!both_case_p(c));
        }
    }

    /// The titlecase digraphs (U+01C5 and friends) have no 1:1 counterpart:
    /// upcasing and downcasing lead to two different characters. SBCL maps
    /// them anyway and calls them BOTH-CASE-P while neither UPPER- nor
    /// LOWER-CASE-P, which breaks the ansi CHAR-UPCASE.2 invariant; egcl
    /// gives them no case.
    #[test]
    fn titlecase_digraphs_have_no_case() {
        for code in [0x1C5, 0x1C8, 0x1CB, 0x1F2] {
            let c = ch(code);
            assert_eq!(char_upcase(c), c, "{code:#x}");
            assert_eq!(char_downcase(c), c, "{code:#x}");
        }
    }

    /// The CLHS 13.1.4.3 contract over every code point: the mapping is a
    /// bijection between the upper- and lowercase characters, and the
    /// predicates agree with the conversions.
    #[test]
    fn case_mapping_is_one_to_one_everywhere() {
        let mut lower = 0;
        for code in 0..=0x10FFFFu32 {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let (u, d) = (char_upcase(c), char_downcase(c));
            assert!(!(upper_case_p(c) && lower_case_p(c)), "{code:#x}");
            assert_eq!(char_upcase(u), u, "{code:#x}");
            assert_eq!(char_downcase(d), d, "{code:#x}");
            assert!(char_equal(u, d), "{code:#x}");
            if lower_case_p(c) {
                lower += 1;
                assert!(upper_case_p(u), "{code:#x}");
                assert_eq!(char_downcase(u), c, "{code:#x}");
            }
            if upper_case_p(c) {
                assert!(lower_case_p(d), "{code:#x}");
                assert_eq!(char_upcase(d), c, "{code:#x}");
            }
        }
        // SBCL counts 1,354 lowercase characters under its Unicode version;
        // later Unicode versions only add pairs.
        assert!(lower >= 1354, "{lower}");
    }
}
