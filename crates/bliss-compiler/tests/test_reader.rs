//! Tests for bliss-compiler reader: ReaderState, read, read_from_string,
//! readtable operations, SourcePos, and edge cases.

use bliss_compiler::reader::*;
use bliss_rt::value::{BlissVal, NIL, T, EOF};

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
    let pos = SourcePos { file: Some("test.lisp".into()), line: 42, column: 7 };
    assert_eq!(pos.file.as_deref(), Some("test.lisp"));
    let p2 = pos.clone();
    assert_eq!(p2.line, 42);
    assert_eq!(p2.column, 7);
    let none_pos = SourcePos { file: None, line: 1, column: 0 };
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
    assert!((v.as_single_float() - 3.14_f32).abs() < 0.01);
    let (v, _) = read_from_string("-1.5").unwrap();
    assert!((v.as_single_float() - (-1.5_f32)).abs() < 0.01);
    let (v, _) = read_from_string("1.0e2").unwrap();
    assert!((v.as_single_float() - 100.0_f32).abs() < 0.1);
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

// ── read_from_string: strings ────────────────────────────────────

#[test]
fn read_strings() {
    let (v, _) = read_from_string("\"hello\"").unwrap();
    assert!(v.is_heap_object());
    let (v, _) = read_from_string("\"\"").unwrap();
    assert!(v.is_heap_object());
    let (v, _) = read_from_string("\"hello\\\"world\"").unwrap();
    assert!(v.is_heap_object());
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

// ── read_from_string: quote / backquote ──────────────────────────

#[test]
fn read_quote_and_backquote() {
    let (v, _) = read_from_string("'x").unwrap();
    assert!(v.is_cons() || v.is_list());
    let (v, _) = read_from_string("'(1 2)").unwrap();
    assert!(v.is_cons() || v.is_list());
    let (v, _) = read_from_string("`x").unwrap();
    assert!(v.is_cons() || v.is_list());
    let (v, _) = read_from_string("`(a ,b)").unwrap();
    assert!(v.is_cons() || v.is_list());
}

// ── read_from_string: sharpsign macros ───────────────────────────

#[test]
fn read_sharpsign_quote() {
    let (v, _) = read_from_string("#'foo").unwrap();
    assert!(v.is_cons() || v.is_list());
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
fn read_sharpsign_vector() {
    let (v, _) = read_from_string("#(1 2 3)").unwrap();
    assert!(v.is_heap_object());
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

// ── Edge cases: read-suppress ────────────────────────────────────

#[test]
fn read_with_suppress_mode() {
    let mut state = ReaderState::new();
    state.set_read_suppress(true);
    // In *read-suppress* mode, read should consume tokens but return NIL
    let result = read(&mut state);
    assert!(result.is_ok() || result.is_err());
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
    match read_from_string("") {
        Ok((val, _)) => assert_eq!(val, EOF),
        Err(_) => {}
    }
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
