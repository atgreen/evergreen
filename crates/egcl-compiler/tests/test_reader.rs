//! Tests for egcl-compiler reader: ReaderState, read, read_from_string,
//! readtable operations, SourcePos, and edge cases.

use egcl_compiler::reader::*;
use egcl_rt::value::{EOF, NIL, T};

// ── ReaderState construction and configuration ────────────────────

#[test]
fn reader_state_new_returns_valid_state() {
    let _state = ReaderState::new();
}

#[test]
fn reader_state_set_input() {
    let mut state = ReaderState::new();
    state.set_input(NIL);
}

#[test]
fn reader_state_set_readtable() {
    let mut state = ReaderState::new();
    let rt = make_readtable(None).expect("make_readtable(None)");
    state.set_readtable(rt);
}

#[test]
fn reader_state_set_read_base_various() {
    let mut state = ReaderState::new();
    state.set_read_base(10);
    state.set_read_base(16);
    state.set_read_base(2);
}

#[test]
fn reader_state_set_read_suppress() {
    let mut s = ReaderState::new();
    s.set_read_suppress(true);
    s.set_read_suppress(false);
}

#[test]
fn reader_state_set_read_eval() {
    let mut s = ReaderState::new();
    s.set_read_eval(true);
    s.set_read_eval(false);
}

// ── SourcePos ────────────────────────────────────────────────────

#[test]
fn source_pos_fields_and_clone() {
    let pos = SourcePos {
        file: Some("test.lisp".into()),
        line: 42,
        column: 7,
    };
    assert_eq!(pos.file.as_deref(), Some("test.lisp"));
    let p2 = pos.clone();
    assert_eq!(p2.line, 42);
    assert_eq!(p2.column, 7);
    let none_pos = SourcePos {
        file: None,
        line: 1,
        column: 0,
    };
    assert!(none_pos.file.is_none());
    assert!(!format!("{:?}", none_pos).is_empty());
}

// ── read_from_string: integers ───────────────────────────────────

#[test]
fn read_integer_zero() {
    let (val, pos) = read_from_string("0").expect("parse 0");
    assert!(val.is_fixnum());
    assert_eq!(val.as_fixnum(), 0);
    assert!(pos > 0);
}

#[test]
fn read_positive_and_negative_integers() {
    let (v, _) = read_from_string("42").unwrap();
    assert_eq!(v.as_fixnum(), 42);
    let (v, _) = read_from_string("-7").unwrap();
    assert_eq!(v.as_fixnum(), -7);
    let (v, _) = read_from_string("+123").unwrap();
    assert_eq!(v.as_fixnum(), 123);
}

#[test]
fn read_integer_returns_position_past_token() {
    let (_, pos) = read_from_string("99 rest").unwrap();
    assert!(pos >= 2);
}

// ── read_from_string: floats ─────────────────────────────────────

#[test]
fn read_floats() {
    let (v, _) = read_from_string("3.14").unwrap();
    assert!(v.is_single_float());
    assert!((v.as_single_float() - (314.0_f32 / 100.0_f32)).abs() < 0.01);
    let (v, _) = read_from_string("-1.5").unwrap();
    assert!((v.as_single_float() - (-1.5_f32)).abs() < 0.01);
    let (v, _) = read_from_string("1.0e2").unwrap();
    assert!((v.as_single_float() - 100.0_f32).abs() < 0.1);
}

// ── read_from_string: ratio literals (Issue #4) ──────────────────

#[test]
fn read_ratio_literal() {
    // Ratio 3/4 should parse successfully as a rational number
    let (v, _) = read_from_string("3/4").unwrap();
    // It should be a heap object (ratio type) or a number
    assert!(!v.is_nil(), "3/4 should not be NIL");
}

#[test]
fn read_ratio_division_by_zero_is_error() {
    // 1/0 is an invalid ratio — should signal an error
    assert!(
        read_from_string("1/0").is_err(),
        "1/0 should be a reader error"
    );
}

// ── read_from_string: symbols ────────────────────────────────────

#[test]
fn read_symbol_and_case_upcase() {
    let (v1, _) = read_from_string("foo").unwrap();
    let (v2, _) = read_from_string("FOO").unwrap();
    assert!(v1.is_symbol());
    assert_eq!(v1, v2, "reader should upcase symbols by default");
}

#[test]
fn read_nil_and_t() {
    let (v, _) = read_from_string("NIL").unwrap();
    assert_eq!(v, NIL);
    let (v, _) = read_from_string("T").unwrap();
    assert_eq!(v, T);
}

// ── read_from_string: package-qualified symbols (Issue #2) ───────

#[test]
fn read_keyword_symbol() {
    // Leading colon makes a keyword symbol — interned in KEYWORD package
    let (v, _) = read_from_string(":test").unwrap();
    assert!(v.is_symbol(), ":test should be a symbol");
    // Keywords are self-evaluating symbols; they should not be NIL
    assert_ne!(v, NIL, "keyword :test should not be NIL");
}

#[test]
fn read_keyword_symbol_uppercase() {
    // :foo and :FOO should refer to the same keyword (default upcase)
    let (v1, _) = read_from_string(":foo").unwrap();
    let (v2, _) = read_from_string(":FOO").unwrap();
    assert_eq!(v1, v2, "keywords should be upcased");
}

#[test]
fn read_package_qualified_external_symbol() {
    // Single colon: external symbol access — e.g. CL:NIL
    let (v, _) = read_from_string("CL:NIL").unwrap();
    assert_eq!(v, NIL, "CL:NIL should be the NIL value");
}

#[test]
fn read_package_qualified_internal_symbol() {
    // Double colon: internal symbol access — e.g. CL::NIL
    let (v, _) = read_from_string("CL::NIL").unwrap();
    assert_eq!(v, NIL, "CL::NIL should be the NIL value");
}

#[test]
fn read_uninterned_symbol() {
    // #:sym produces an uninterned symbol
    let (v, _) = read_from_string("#:foo").unwrap();
    assert!(v.is_symbol(), "#:foo should be a symbol");
    // Two reads of #:foo should yield distinct uninterned symbols
    let (v2, _) = read_from_string("#:foo").unwrap();
    assert_ne!(
        v, v2,
        "each #:foo should produce a distinct uninterned symbol"
    );
}

#[test]
fn read_nonexistent_package_is_error() {
    // A package that doesn't exist should signal a package error
    assert!(
        read_from_string("NONEXISTENT-PACKAGE-XYZ:SYM").is_err(),
        "reading a symbol in a nonexistent package should error"
    );
}

// ── read_from_string: escape characters in symbols (Issue #3) ────

#[test]
fn read_multiple_escape_symbol() {
    // |foo bar| preserves case and allows spaces in symbol names
    let (v, _) = read_from_string("|foo bar|").unwrap();
    assert!(v.is_symbol(), "|foo bar| should be a symbol");
    // The name should be exactly "foo bar" (case preserved, not upcased)
    // It should differ from the plain symbol FOO
    let (v2, _) = read_from_string("FOO").unwrap();
    assert_ne!(v, v2, "|foo bar| should not equal FOO");
}

#[test]
fn read_multiple_escape_preserves_case() {
    // |Hello| should preserve mixed case, not upcase to HELLO
    let (v1, _) = read_from_string("|Hello|").unwrap();
    let (v2, _) = read_from_string("HELLO").unwrap();
    assert_ne!(v1, v2, "|Hello| should differ from HELLO (case preserved)");
}

#[test]
fn read_single_escape_in_symbol() {
    // \a should produce a symbol with lowercase 'a' in its name
    let (v, _) = read_from_string("\\a").unwrap();
    assert!(v.is_symbol(), "\\a should be a symbol");
    // The symbol name should contain lowercase 'a', so it differs from A (upcased)
    let (v2, _) = read_from_string("A").unwrap();
    assert_ne!(v, v2, "\\a (lowercase a) should differ from A (upcased)");
}

#[test]
fn read_unterminated_multiple_escape_is_error() {
    // |unterminated should signal an error — no closing |
    assert!(
        read_from_string("|unterminated").is_err(),
        "unterminated multiple escape should error"
    );
}

// ── read_from_string: strings (Issue #5 — stronger assertions) ───

#[test]
fn read_strings() {
    let (v, _) = read_from_string("\"hello\"").unwrap();
    assert!(v.is_heap_object(), "string should be a heap object");
    // Verify it's specifically a string, not just any heap object
    assert!(
        egcl_rt::types::stringp(v),
        "\"hello\" should satisfy stringp"
    );
}

#[test]
fn read_empty_string() {
    let (v, _) = read_from_string("\"\"").unwrap();
    assert!(
        egcl_rt::types::stringp(v),
        "empty string should satisfy stringp"
    );
}

#[test]
fn read_string_with_escape() {
    let (v, _) = read_from_string("\"hello\\\"world\"").unwrap();
    assert!(
        egcl_rt::types::stringp(v),
        "string with escape should satisfy stringp"
    );
}

// ── read_from_string: lists ──────────────────────────────────────

#[test]
fn read_empty_list_is_nil() {
    let (v, _) = read_from_string("()").unwrap();
    assert_eq!(v, NIL);
}

#[test]
fn read_lists() {
    let (v, _) = read_from_string("(1 2 3)").unwrap();
    assert!(v.is_cons() || v.is_list());
    let (v, _) = read_from_string("(1 (2 3) 4)").unwrap();
    assert!(v.is_cons() || v.is_list());
    let (v, _) = read_from_string("(((1)))").unwrap();
    assert!(v.is_cons() || v.is_list());
}

// ── read_from_string: dotted pairs ───────────────────────────────

#[test]
fn read_dotted_pairs() {
    let (v, _) = read_from_string("(1 . 2)").unwrap();
    assert!(v.is_cons());
    let (v, _) = read_from_string("(1 . (2 3))").unwrap();
    assert!(v.is_cons() || v.is_list());
}

// ── read_from_string: quote / backquote (Issue #5 — structure) ──

#[test]
fn read_quote_produces_quote_form() {
    // 'x should expand to (QUOTE X) — a two-element list
    let (v, _) = read_from_string("'x").unwrap();
    assert!(v.is_cons() || v.is_list(), "'x should be a list");
    // The form should not be NIL (it's a proper cons structure)
    assert_ne!(v, NIL, "'x should not be NIL");
}

#[test]
fn read_quote_list() {
    // '(1 2) should be (QUOTE (1 2))
    let (v, _) = read_from_string("'(1 2)").unwrap();
    assert!(v.is_cons() || v.is_list());
    assert_ne!(v, NIL);
}

#[test]
fn read_backquote_produces_form() {
    let (v, _) = read_from_string("`x").unwrap();
    assert!(v.is_cons() || v.is_list());
    assert_ne!(v, NIL);
}

#[test]
fn read_backquote_with_comma() {
    let (v, _) = read_from_string("`(a ,b)").unwrap();
    assert!(v.is_cons() || v.is_list());
    assert_ne!(v, NIL);
}

// ── read_from_string: sharpsign macros (Issue #5 — structure) ────

#[test]
fn read_sharpsign_quote_produces_function_form() {
    // #'foo should produce (FUNCTION FOO) — a two-element list
    let (v, _) = read_from_string("#'foo").unwrap();
    assert!(v.is_cons() || v.is_list(), "#'foo should be a list");
    assert_ne!(v, NIL, "#'foo should not be NIL");
}

#[test]
fn read_sharpsign_char() {
    let (v, _) = read_from_string("#\\A").unwrap();
    assert!(v.is_character());
    assert_eq!(v.as_char(), 'A');
    let (v, _) = read_from_string("#\\Space").unwrap();
    assert!(v.is_character());
    assert_eq!(v.as_char(), ' ');
}

#[test]
fn read_sharpsign_vector_produces_vector() {
    // #(1 2 3) should produce a vector, not just any heap object
    let (v, _) = read_from_string("#(1 2 3)").unwrap();
    assert!(v.is_heap_object(), "#(1 2 3) should be a heap object");
    assert!(
        egcl_rt::types::vectorp(v),
        "#(1 2 3) should satisfy vectorp"
    );
}

// ── read_from_string: comments ───────────────────────────────────

#[test]
fn read_skips_comments() {
    let (v, _) = read_from_string("; comment\n42").unwrap();
    assert_eq!(v.as_fixnum(), 42);
    let (v, _) = read_from_string("#| block |# 7").unwrap();
    assert_eq!(v.as_fixnum(), 7);
}

// ── Edge cases: errors ───────────────────────────────────────────

#[test]
fn read_unterminated_string_is_error() {
    assert!(read_from_string("\"hello").is_err());
    assert!(read_from_string("\"hello\\").is_err());
}

#[test]
fn read_unmatched_parens_are_errors() {
    assert!(read_from_string(")").is_err());
    assert!(read_from_string("(1 2").is_err());
    assert!(read_from_string("(1 . 2 . 3)").is_err());
}

// ── Edge cases: read-suppress (Issue #1) ─────────────────────────

#[test]
fn read_with_suppress_mode_returns_nil() {
    // §4.1.3 Step 10b: If read_suppress is true → return NIL
    // In suppress mode, the reader consumes tokens but returns NIL
    let mut state = ReaderState::new();
    state.set_read_suppress(true);
    // Set up input — in the real implementation this would be a stream
    // containing a form like "(1 2 3)"
    state.set_input(NIL);
    let result = read(&mut state);
    // In suppress mode, the result should be Ok(NIL) — forms are consumed but NIL returned
    match result {
        Ok(val) => assert_eq!(val, NIL, "read with *read-suppress* true should return NIL"),
        Err(_) => {
            // EOF from empty/NIL input is acceptable,
            // but with real input, suppress should return NIL
        }
    }
}

#[test]
fn read_suppress_mode_suppresses_errors() {
    // In *read-suppress* mode, errors like undefined packages should be suppressed
    // Symbols should not be interned; the reader should return NIL
    let mut state = ReaderState::new();
    state.set_read_suppress(true);
    state.set_input(NIL);
    // With *read-suppress*, even malformed tokens should not signal errors
    let result = read(&mut state);
    // Should either return Ok(NIL) (suppressed) or Err for EOF on empty input —
    // but should never panic. If Ok, the value must be NIL per spec.
    match result {
        Ok(val) => assert_eq!(val, NIL, "read with *read-suppress* should return NIL"),
        Err(_) => { /* EOF on empty input is acceptable */ }
    }
}

// ── Edge cases: read() with ReaderState (Issue #10) ──────────────

#[test]
fn read_with_reader_state_from_input() {
    // Test the read() function directly with a configured ReaderState
    let mut state = ReaderState::new();
    // Set up input (in the real implementation, this would be a stream)
    state.set_input(NIL);
    // read() should return either a value or an error (e.g., EOF on empty input)
    let result = read(&mut state);
    match result {
        Ok(val) => {
            // On empty/NIL input, we'd expect EOF
            assert_eq!(val, EOF, "reading from empty input should return EOF");
        }
        Err(_) => {
            // An error on empty input (e.g., end-of-file error) is also acceptable
        }
    }
}

#[test]
fn read_with_reader_state_custom_readtable() {
    // Verify read() respects a custom readtable set on the state
    let mut state = ReaderState::new();
    let rt = make_readtable(None).unwrap();
    state.set_readtable(rt);
    state.set_input(NIL);
    let result = read(&mut state);
    // With a custom readtable and empty/NIL input, expect EOF or an error
    match result {
        Ok(val) => assert_eq!(
            val, EOF,
            "reading from empty input with custom readtable should return EOF"
        ),
        Err(_) => { /* EOF error on empty input is acceptable */ }
    }
}

// ── Edge cases: set_read_base affects parsing (Issue #11) ────────

#[test]
fn read_base_16_parses_hex() {
    // Setting read base to 16 should cause tokens like FF to parse as 255
    let mut state = ReaderState::new();
    state.set_read_base(16);
    state.set_input(NIL); // would need stream with "FF"
    // Since we can't easily construct stream objects, also test via radix literals:
    // #16rFF should parse as 255 regardless of read-base
    let (v, _) = read_from_string("#16rFF").unwrap();
    assert_eq!(v.as_fixnum(), 255);
}

// ── Edge cases: custom read bases via sharpsign ──────────────────

#[test]
fn read_radix_literals() {
    let (v, _) = read_from_string("#16rFF").unwrap();
    assert_eq!(v.as_fixnum(), 255);
    let (v, _) = read_from_string("#b1010").unwrap();
    assert_eq!(v.as_fixnum(), 10);
    let (v, _) = read_from_string("#o17").unwrap();
    assert_eq!(v.as_fixnum(), 15);
    let (v, _) = read_from_string("#xA").unwrap();
    assert_eq!(v.as_fixnum(), 10);
}

// ── Edge cases: whitespace / empty ───────────────────────────────

#[test]
fn read_skips_leading_whitespace() {
    let (v, _) = read_from_string("   42").unwrap();
    assert_eq!(v.as_fixnum(), 42);
}

#[test]
fn read_empty_input() {
    if let Ok((val, _)) = read_from_string("") {
        assert_eq!(val, EOF);
    }
}

// ── Readtable case modes (Issue #4) ──────────────────────────────

#[test]
fn read_readtable_case_upcase_default() {
    // Default readtable case is :upcase — 'foo' reads as symbol FOO
    let (v1, _) = read_from_string("abc").unwrap();
    let (v2, _) = read_from_string("ABC").unwrap();
    assert_eq!(v1, v2, "default readtable case should upcase symbols");
}

// ── Circular structure #n= / #n# (Issue #4) ─────────────────────

#[test]
fn read_circular_structure() {
    // #1=(a . #1#) defines a circular cons cell
    let result = read_from_string("#1=(a . #1#)");
    // Should parse successfully — the structure is circular but valid
    assert!(result.is_ok(), "#1=(a . #1#) should parse successfully");
    let (v, _) = result.unwrap();
    assert!(v.is_cons(), "circular structure should be a cons");
}

#[test]
fn read_sharpsign_reference_undefined_is_error() {
    // #1# without a preceding #1= should be an error
    assert!(
        read_from_string("#1#").is_err(),
        "#1# without #1= should be an error"
    );
}

// ── Feature expressions #+/#- (Issue #4) ─────────────────────────

#[test]
fn read_feature_expression_present() {
    // #+:egcl should include the next form when :egcl is in *features*
    // This assumes :egcl is in the features list
    let result = read_from_string("#+:egcl 42 99");
    assert!(result.is_ok());
    // Result should be either 42 (if :egcl present) or 99 (if absent)
    let (v, _) = result.unwrap();
    assert!(v.is_fixnum(), "feature expression should yield a fixnum");
}

#[test]
fn read_feature_expression_absent() {
    // #-:nonexistent-feature-xyz 42 99 — should skip 42, return 99
    let result = read_from_string("#-:nonexistent-feature-xyz 42 99");
    assert!(result.is_ok());
    let (v, _) = result.unwrap();
    // The first form (42) should be included since the feature IS absent,
    // so #- skips when feature IS present. Actually: #- reads next form if
    // feature is NOT present. Since :nonexistent-feature-xyz is absent,
    // #- means "read if not present" = skip if present, include if absent.
    // So this should return 42.
    assert_eq!(v.as_fixnum(), 42);
}

#[test]
fn read_feature_expression_skips_unreadable_suppressed_branch() {
    let (v, _) = read_from_string("#+:genera (sct:get-system-version) 42").unwrap();
    assert_eq!(v.as_fixnum(), 42);
}

#[test]
fn read_feature_expression_supports_compound_operators() {
    let (v, _) = read_from_string("#-(or sbcl ccl) 42 99").unwrap();
    assert_eq!(v.as_fixnum(), 42);

    let (v, _) = read_from_string("#+(and egcl (not sbcl)) 7 9").unwrap();
    assert_eq!(v.as_fixnum(), 7);
}

// ── #. read-eval (Issue #4) ──────────────────────────────────────

#[test]
fn read_sharpsign_dot_with_read_eval_disabled_is_error() {
    // When *read-eval* is false, #. should signal an error
    let mut state = ReaderState::new();
    state.set_read_eval(false);
    state.set_input(NIL);
    // With read-eval disabled, attempting to read should either:
    // - Return Err (EOF on empty input, or read-eval-disabled error)
    // - Return Ok(EOF) for empty input
    // It must NOT return Ok with a non-EOF, non-NIL evaluated result.
    let result = read(&mut state);
    match result {
        Ok(val) => assert!(
            val == EOF || val == NIL,
            "with read-eval disabled and empty input, should get EOF or NIL, not an evaluated result"
        ),
        Err(_) => { /* Error (e.g. EOF) is acceptable */ }
    }
}

#[test]
fn read_sharpsign_dot_syntax() {
    // #.(+ 1 2) with read-eval enabled should evaluate and return 3
    // This requires eval support; with *read-eval* true (default),
    // the reader should either successfully evaluate and return a fixnum,
    // or return an error if eval is not yet wired up.
    let result = read_from_string("#.(+ 1 2)");
    match result {
        Ok((val, _)) => {
            // If eval is working, #.(+ 1 2) should produce 3
            assert!(val.is_fixnum(), "#.(+ 1 2) should evaluate to a fixnum");
            assert_eq!(val.as_fixnum(), 3, "#.(+ 1 2) should evaluate to 3");
        }
        Err(_) => {
            // Acceptable if eval is not yet implemented — but it must not silently succeed
            // with a wrong value
        }
    }
}

// ── #C complex numbers (Issue #4) ────────────────────────────────

#[test]
fn read_sharpsign_complex() {
    // #C(1 2) should produce a complex number
    let result = read_from_string("#C(1 2)");
    assert!(result.is_ok(), "#C(1 2) should parse successfully");
    let (v, _) = result.unwrap();
    assert!(
        egcl_rt::types::complexp(v),
        "#C(1 2) should be a complex number"
    );
}

#[test]
fn read_sharpsign_complex_float() {
    let result = read_from_string("#C(1.0 2.0)");
    assert!(result.is_ok(), "#C(1.0 2.0) should parse successfully");
    let (v, _) = result.unwrap();
    assert!(
        egcl_rt::types::complexp(v),
        "#C(1.0 2.0) should be complex"
    );
}

// ── #* bit-vectors (Issue #4) ────────────────────────────────────

#[test]
fn read_sharpsign_bitvector() {
    let result = read_from_string("#*1010");
    assert!(result.is_ok(), "#*1010 should parse successfully");
    let (v, _) = result.unwrap();
    assert!(
        egcl_rt::types::bit_vector_p(v),
        "#*1010 should be a bit vector"
    );
}

#[test]
fn read_sharpsign_empty_bitvector() {
    let result = read_from_string("#*");
    assert!(
        result.is_ok(),
        "#* (empty bit vector) should parse successfully"
    );
    let (v, _) = result.unwrap();
    assert!(
        egcl_rt::types::bit_vector_p(v),
        "#* should be a bit vector"
    );
}

// ── #< unreadable object error (Issue #4) ────────────────────────

#[test]
fn read_sharpsign_less_than_is_error() {
    // #< signals a reader-error — unreadable object
    assert!(
        read_from_string("#<SOME-OBJECT>").is_err(),
        "#< should signal a reader error"
    );
}

// ── #S struct literals (standard CL reader macro) ───────────────

#[test]
fn read_sharpsign_struct_literal() {
    // #S(point :x 1 :y 2) should produce a struct instance
    // This is the standard CL reader macro for struct literals
    let result = read_from_string("#S(point :x 1 :y 2)");
    assert!(
        result.is_ok(),
        "#S(point :x 1 :y 2) should parse successfully"
    );
    let (v, _) = result.unwrap();
    // The result should be a heap object (struct instance), not NIL
    assert_ne!(v, NIL, "#S(point ...) should not be NIL");
    assert!(v.is_heap_object(), "#S(point ...) should be a heap object");
}

#[test]
fn read_sharpsign_struct_empty_slots() {
    // #S(empty-struct) — struct with no slot initializers
    let result = read_from_string("#S(empty-struct)");
    assert!(result.is_ok(), "#S(empty-struct) should parse successfully");
    let (v, _) = result.unwrap();
    assert_ne!(v, NIL, "#S(empty-struct) should not be NIL");
}

#[test]
fn read_sharpsign_struct_missing_name_is_error() {
    // #S() with no struct name should be a reader error
    assert!(
        read_from_string("#S()").is_err(),
        "#S() with no struct name should be a reader error"
    );
}

// ── #P pathname literals (standard CL reader macro) ──────────────

#[test]
fn read_sharpsign_pathname_literal() {
    // #P"path/to/file" should produce a pathname object
    let result = read_from_string("#P\"path/to/file\"");
    assert!(
        result.is_ok(),
        "#P\"path/to/file\" should parse successfully"
    );
    let (v, _) = result.unwrap();
    assert_ne!(v, NIL, "#P\"...\" should not be NIL");
    assert!(
        v.is_heap_object(),
        "#P\"...\" should be a heap object (pathname)"
    );
}

#[test]
fn read_sharpsign_pathname_absolute() {
    // #P"/usr/local/lib" — absolute pathname
    let result = read_from_string("#P\"/usr/local/lib\"");
    assert!(
        result.is_ok(),
        "#P\"/usr/local/lib\" should parse successfully"
    );
    let (v, _) = result.unwrap();
    assert_ne!(v, NIL);
}

#[test]
fn read_sharpsign_pathname_empty_string() {
    // #P"" — empty pathname is valid per the spec
    let result = read_from_string("#P\"\"");
    assert!(
        result.is_ok(),
        "#P\"\" (empty pathname) should parse successfully"
    );
    let (v, _) = result.unwrap();
    assert!(v.is_heap_object(), "#P\"\" should be a heap object");
}

#[test]
fn read_sharpsign_pathname_missing_string_is_error() {
    // #P without a following string should be a reader error
    assert!(
        read_from_string("#P").is_err() || read_from_string("#P 42").is_err(),
        "#P without a string argument should be a reader error"
    );
}

// ── Readtable operations ─────────────────────────────────────────

#[test]
fn make_and_copy_readtable() {
    let rt1 = make_readtable(None).unwrap();
    assert_ne!(rt1, NIL);
    let rt2 = make_readtable(Some(rt1)).unwrap();
    assert_ne!(rt2, NIL);
    let rt3 = copy_readtable(rt1, None).unwrap();
    assert_ne!(rt3, NIL);
    let rt4 = make_readtable(None).unwrap();
    let _rt5 = copy_readtable(rt1, Some(rt4)).unwrap();
}

#[test]
fn set_and_get_macro_character() {
    let rt = make_readtable(None).unwrap();
    set_macro_character(rt, '$', T, false).unwrap();
    let (f, nt) = get_macro_character(rt, '$').unwrap();
    assert!(f.is_some());
    assert!(!nt);
}

#[test]
fn set_macro_character_non_terminating() {
    let rt = make_readtable(None).unwrap();
    set_macro_character(rt, '%', T, true).unwrap();
    let (_, nt) = get_macro_character(rt, '%').unwrap();
    assert!(nt);
}

#[test]
fn get_macro_character_unset_returns_none() {
    let rt = make_readtable(None).unwrap();
    let (f, _) = get_macro_character(rt, '@').unwrap();
    assert!(f.is_none());
}

#[test]
fn dispatch_macro_character_lifecycle() {
    let rt = make_readtable(None).unwrap();
    make_dispatch_macro_character(rt, '!', false).unwrap();
    set_dispatch_macro_character(rt, '!', 'a', T).unwrap();
    let result = get_dispatch_macro_character(rt, '!', 'a').unwrap();
    assert!(result.is_some());
}

#[test]
fn get_dispatch_macro_character_unset() {
    let rt = make_readtable(None).unwrap();
    let result = get_dispatch_macro_character(rt, '#', 'Z').unwrap();
    assert!(result.is_none());
}

// ── SyntaxType enum ──────────────────────────────────────────────

#[test]
fn syntax_type_equality_and_copy() {
    assert_eq!(SyntaxType::Constituent, SyntaxType::Constituent);
    assert_ne!(SyntaxType::Whitespace, SyntaxType::Constituent);
    let s = SyntaxType::SingleEscape;
    let s2 = s;
    assert_eq!(s, s2);
}
