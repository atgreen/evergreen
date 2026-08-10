//! Tests for bliss-stdlib format module (FORMAT & pretty-printer).

use bliss_stdlib::format::*;
use bliss_rt::value::{BlissVal, NIL, T};

// ── NewlineKind enum ──────────────────────────────────────────────

#[test]
fn newline_kind_variants_exist() {
    let _linear = NewlineKind::Linear;
    let _fill = NewlineKind::Fill;
    let _miser = NewlineKind::Miser;
    let _mandatory = NewlineKind::Mandatory;
}

#[test]
fn newline_kind_equality() {
    assert_eq!(NewlineKind::Linear, NewlineKind::Linear);
    assert_eq!(NewlineKind::Fill, NewlineKind::Fill);
    assert_eq!(NewlineKind::Miser, NewlineKind::Miser);
    assert_eq!(NewlineKind::Mandatory, NewlineKind::Mandatory);
    assert_ne!(NewlineKind::Linear, NewlineKind::Fill);
    assert_ne!(NewlineKind::Miser, NewlineKind::Mandatory);
    assert_ne!(NewlineKind::Linear, NewlineKind::Mandatory);
}

#[test]
fn newline_kind_clone_and_copy() {
    let original = NewlineKind::Fill;
    let cloned = original.clone();
    let copied = original; // Copy
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

#[test]
fn newline_kind_debug() {
    let dbg = format!("{:?}", NewlineKind::Linear);
    assert!(dbg.contains("Linear"), "Debug output should contain 'Linear', got: {}", dbg);
    let dbg = format!("{:?}", NewlineKind::Fill);
    assert!(dbg.contains("Fill"), "Debug output should contain 'Fill', got: {}", dbg);
    let dbg = format!("{:?}", NewlineKind::Miser);
    assert!(dbg.contains("Miser"), "Debug output should contain 'Miser', got: {}", dbg);
    let dbg = format!("{:?}", NewlineKind::Mandatory);
    assert!(dbg.contains("Mandatory"), "Debug output should contain 'Mandatory', got: {}", dbg);
}

// ── TabKind enum ──────────────────────────────────────────────────

#[test]
fn tab_kind_variants_exist() {
    let _line = TabKind::Line;
    let _section = TabKind::Section;
    let _line_rel = TabKind::LineRelative;
    let _section_rel = TabKind::SectionRelative;
}

#[test]
fn tab_kind_equality() {
    assert_eq!(TabKind::Line, TabKind::Line);
    assert_eq!(TabKind::Section, TabKind::Section);
    assert_eq!(TabKind::LineRelative, TabKind::LineRelative);
    assert_eq!(TabKind::SectionRelative, TabKind::SectionRelative);
    assert_ne!(TabKind::Line, TabKind::Section);
    assert_ne!(TabKind::LineRelative, TabKind::SectionRelative);
    assert_ne!(TabKind::Line, TabKind::SectionRelative);
}

#[test]
fn tab_kind_clone_and_copy() {
    let original = TabKind::SectionRelative;
    let cloned = original.clone();
    let copied = original; // Copy
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

#[test]
fn tab_kind_debug() {
    let dbg = format!("{:?}", TabKind::Line);
    assert!(dbg.contains("Line"), "Debug should contain 'Line', got: {}", dbg);
    let dbg = format!("{:?}", TabKind::Section);
    assert!(dbg.contains("Section"), "Debug should contain 'Section', got: {}", dbg);
    let dbg = format!("{:?}", TabKind::LineRelative);
    assert!(dbg.contains("LineRelative"), "Debug should contain 'LineRelative', got: {}", dbg);
    let dbg = format!("{:?}", TabKind::SectionRelative);
    assert!(dbg.contains("SectionRelative"), "Debug should contain 'SectionRelative', got: {}", dbg);
}

// ── format() with destination NIL — directive tests ───────────────

/// Helper: call format with NIL destination, expect Ok and extract the string result.
/// Since the implementation is not yet done, these tests validate the interface
/// and will fail at runtime (red phase).
fn format_nil(control: &str, args: &[BlissVal]) -> Result<BlissVal, bliss_rt::error::BlissError> {
    format(NIL, control, args)
}

#[test]
fn format_directive_tilde_a_aesthetic() {
    let result = format_nil("~A", &[BlissVal::from_fixnum(42)]);
    assert!(result.is_ok(), "format ~A should succeed");
    assert_ne!(result.unwrap(), NIL, "format with NIL dest should return a string, not NIL");
}

#[test]
fn format_directive_tilde_s_standard() {
    let result = format_nil("~S", &[BlissVal::from_fixnum(42)]);
    assert!(result.is_ok(), "format ~S should succeed");
    assert_ne!(result.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_d_decimal() {
    let r = format_nil("~D", &[BlissVal::from_fixnum(255)]);
    assert!(r.is_ok(), "format ~D should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_b_binary() {
    let r = format_nil("~B", &[BlissVal::from_fixnum(10)]);
    assert!(r.is_ok(), "format ~B should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_o_octal() {
    let r = format_nil("~O", &[BlissVal::from_fixnum(8)]);
    assert!(r.is_ok(), "format ~O should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_x_hex() {
    let r = format_nil("~X", &[BlissVal::from_fixnum(255)]);
    assert!(r.is_ok(), "format ~X should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_r_radix() {
    let r = format_nil("~R", &[BlissVal::from_fixnum(4)]);
    assert!(r.is_ok(), "format ~R should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_f_fixed_float() {
    let r = format_nil("~F", &[BlissVal::from_single_float(3.14)]);
    assert!(r.is_ok(), "format ~F should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_e_exponential() {
    let r = format_nil("~E", &[BlissVal::from_single_float(3.14)]);
    assert!(r.is_ok(), "format ~E should succeed");
    assert_ne!(r.unwrap(), NIL);
}

#[test]
fn format_directive_tilde_g_general_float() {
    let r = format_nil("~G", &[BlissVal::from_single_float(3.14)]);
    assert!(r.is_ok(), "format ~G should succeed");
    assert_ne!(r.unwrap(), NIL);
}

// ~% — newline
#[test]
fn format_directive_tilde_percent_newline() {
    let result = format_nil("hello~%world", &[]);
    assert!(result.is_ok(), "format ~%% should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ~& — fresh-line
#[test]
fn format_directive_tilde_ampersand_fresh_line() {
    let result = format_nil("hello~&world", &[]);
    assert!(result.is_ok(), "format ~& should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ~T — tabulate
#[test]
fn format_directive_tilde_t_tabulate() {
    let result = format_nil("~10T", &[]);
    assert!(result.is_ok(), "format ~T should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ~* — goto (skip argument)
#[test]
fn format_directive_tilde_star_goto() {
    let result = format_nil("~*~A", &[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)]);
    assert!(result.is_ok(), "format ~* should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ~? — recursive processing
#[test]
fn format_directive_tilde_question_recursive() {
    // ~? takes a format control string and a list of arguments
    let result = format_nil("~?", &[NIL, NIL]);
    // Whether this specific invocation is valid depends on implementation,
    // but the interface must accept it.
    let _ = result; // Just testing the interface compiles and runs
}

// ~{ ~} — iteration
#[test]
fn format_directive_tilde_brace_iteration() {
    let result = format_nil("~{~A ~}", &[NIL]);
    let _ = result; // Red-phase: testing interface
}

// ~[ ~] — conditional
#[test]
fn format_directive_tilde_bracket_conditional() {
    let result = format_nil("~[zero~;one~;two~]", &[BlissVal::from_fixnum(1)]);
    let _ = result;
}

// ~( ~) — case conversion
#[test]
fn format_directive_tilde_paren_case() {
    let result = format_nil("~(Hello World~)", &[]);
    assert!(result.is_ok(), "format ~( ~) should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ~P — plural
#[test]
fn format_directive_tilde_p_plural() {
    let result = format_nil("~D dog~P", &[BlissVal::from_fixnum(1)]);
    assert!(result.is_ok(), "format ~P should succeed");
}

#[test]
fn format_directive_tilde_p_plural_multiple() {
    let result = format_nil("~D dog~P", &[BlissVal::from_fixnum(3)]);
    assert!(result.is_ok(), "format ~P (plural) should succeed");
}

// ── format() with destination T ───────────────────────────────────

#[test]
fn format_destination_t_writes_stdout() {
    let result = format(T, "hello ~A", &[BlissVal::from_fixnum(42)]);
    assert!(result.is_ok(), "format with T destination should succeed");
    // When destination is T, result should be NIL
    let val = result.unwrap();
    assert_eq!(val, NIL, "format with T dest should return NIL");
}

// ── format() with multiple arguments ─────────────────────────────

#[test]
fn format_multiple_directives() {
    let result = format_nil(
        "~A and ~D",
        &[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)],
    );
    assert!(result.is_ok(), "format with multiple directives should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

#[test]
fn format_no_directives_literal_string() {
    let result = format_nil("hello world", &[]);
    assert!(result.is_ok(), "format with no directives should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

// ── formatter() ───────────────────────────────────────────────────

#[test]
fn formatter_compiles_control_string() {
    let result = formatter("~A ~D");
    assert!(result.is_ok(), "formatter should compile a valid control string");
    let compiled = result.unwrap();
    assert_ne!(compiled, NIL, "compiled formatter should not be NIL");
}

#[test]
fn formatter_empty_string() {
    let result = formatter("");
    assert!(result.is_ok(), "formatter with empty string should succeed");
}

// ── pprint_logical_block() ────────────────────────────────────────

#[test]
fn pprint_logical_block_with_prefix_suffix() {
    let r = pprint_logical_block(T, NIL, Some("("), None, Some(")"), NIL);
    assert!(r.is_ok(), "pprint_logical_block with prefix/suffix should succeed");
}

#[test]
fn pprint_logical_block_no_prefix_and_per_line() {
    assert!(pprint_logical_block(T, NIL, None, None, None, NIL).is_ok());
    assert!(pprint_logical_block(T, NIL, None, Some(";;; "), None, NIL).is_ok());
}

// ── pprint_newline() ──────────────────────────────────────────────

#[test]
fn pprint_newline_all_kinds() {
    for kind in [NewlineKind::Linear, NewlineKind::Fill, NewlineKind::Miser, NewlineKind::Mandatory] {
        let result = pprint_newline(kind, T);
        assert!(result.is_ok(), "pprint_newline {:?} should succeed", kind);
    }
}

// ── pprint_indent() ───────────────────────────────────────────────

#[test]
fn pprint_indent_relative_and_absolute() {
    assert!(pprint_indent(true, 4, T).is_ok(), "relative indent should succeed");
    assert!(pprint_indent(false, 8, T).is_ok(), "absolute indent should succeed");
    assert!(pprint_indent(true, -2, T).is_ok(), "negative indent should succeed");
}

// ── pprint_tab() ──────────────────────────────────────────────────

#[test]
fn pprint_tab_all_kinds() {
    for (kind, col, inc) in [
        (TabKind::Line, 10, 1),
        (TabKind::Section, 5, 2),
        (TabKind::LineRelative, 3, 1),
        (TabKind::SectionRelative, 4, 2),
    ] {
        let result = pprint_tab(kind, col, inc, T);
        assert!(result.is_ok(), "pprint_tab {:?} should succeed", kind);
    }
}

// ── pprint_dispatch() ─────────────────────────────────────────────

#[test]
fn pprint_dispatch_returns_function_and_flag() {
    let result = pprint_dispatch(BlissVal::from_fixnum(42));
    assert!(result.is_ok(), "pprint_dispatch should succeed");
    let (func, found) = result.unwrap();
    // found indicates whether a specific dispatch was found
    let _ = (func, found);
}

// ── set_pprint_dispatch() ─────────────────────────────────────────

#[test]
fn set_pprint_dispatch_with_function() {
    let r = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 0.0, NIL);
    assert!(r.is_ok(), "set_pprint_dispatch with function should succeed");
}

#[test]
fn set_pprint_dispatch_remove_and_priority() {
    // None function removes entry
    let r = set_pprint_dispatch(NIL, None, 0.0, NIL);
    assert!(r.is_ok(), "set_pprint_dispatch with None should remove entry");
    // Non-zero priority
    let r = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 10.0, NIL);
    assert!(r.is_ok(), "set_pprint_dispatch with priority should succeed");
}

// ── copy_pprint_dispatch() ────────────────────────────────────────

#[test]
fn copy_pprint_dispatch_with_none_copies_current() {
    let result = copy_pprint_dispatch(None);
    assert!(result.is_ok(), "copy_pprint_dispatch(None) should copy current table");
    let table = result.unwrap();
    assert_ne!(table, NIL, "copied table should not be NIL");
}

#[test]
fn copy_pprint_dispatch_with_some_copies_given() {
    // First get a table, then copy it
    let table = copy_pprint_dispatch(None);
    assert!(table.is_ok());
    let table_val = table.unwrap();
    let copied = copy_pprint_dispatch(Some(table_val));
    assert!(copied.is_ok(), "copy_pprint_dispatch(Some(table)) should copy given table");
    let copied_val = copied.unwrap();
    assert_ne!(copied_val, NIL);
}

// ── Error conditions ──────────────────────────────────────────────

#[test]
fn format_error_invalid_control_string() {
    // A control string with a dangling ~ at the end is malformed
    let result = format_nil("hello ~", &[]);
    assert!(result.is_err(), "format with dangling ~ should return error");
}

#[test]
fn format_error_unknown_directive() {
    // ~Z is not a standard format directive
    let result = format_nil("~Z", &[BlissVal::from_fixnum(1)]);
    assert!(result.is_err(), "format with unknown directive ~Z should return error");
}

#[test]
fn format_error_unmatched_open_brace() {
    let result = format_nil("~{~A", &[NIL]);
    assert!(result.is_err(), "format with unmatched ~{{ should return error");
}

#[test]
fn format_error_unmatched_close_brace() {
    let result = format_nil("~A~}", &[NIL]);
    assert!(result.is_err(), "format with unmatched ~}} should return error");
}

#[test]
fn format_error_unmatched_open_bracket() {
    let result = format_nil("~[hello", &[BlissVal::from_fixnum(0)]);
    assert!(result.is_err(), "format with unmatched ~[ should return error");
}

#[test]
fn format_error_unmatched_close_bracket() {
    let result = format_nil("hello~]", &[]);
    assert!(result.is_err(), "format with unmatched ~] should return error");
}

#[test]
fn format_error_too_few_arguments_for_directive() {
    // ~D requires an argument but none provided
    let result = format_nil("~D", &[]);
    assert!(result.is_err(), "format ~D with no arguments should return error");
}

#[test]
fn format_error_tilde_d_with_non_numeric() {
    // ~D expects a numeric argument; passing a character should error
    let result = format_nil("~D", &[BlissVal::from_char('a')]);
    assert!(result.is_err(), "format ~D with non-numeric argument should return error");
}

#[test]
fn format_error_radix_directives_with_non_numeric() {
    // ~B, ~O, ~X all require numeric arguments
    for directive in ["~B", "~O", "~X"] {
        let result = format_nil(directive, &[BlissVal::from_char('x')]);
        assert!(result.is_err(), "format {} with non-numeric should error", directive);
    }
}

#[test]
fn format_error_unmatched_open_paren() {
    let result = format_nil("~(hello", &[]);
    assert!(result.is_err(), "format with unmatched ~( should return error");
}

#[test]
fn format_error_unmatched_close_paren() {
    let result = format_nil("hello~)", &[]);
    assert!(result.is_err(), "format with unmatched ~) should return error");
}

// ── Edge cases ────────────────────────────────────────────────────

#[test]
fn format_edge_cases() {
    // Empty control string
    let r = format_nil("", &[]);
    assert!(r.is_ok(), "empty control string should succeed");
    assert_ne!(r.unwrap(), NIL, "empty format with NIL dest returns string, not NIL");
    // ~~ produces literal ~
    assert!(format_nil("~~", &[]).is_ok(), "~~ should succeed");
    // Multiple newlines
    assert!(format_nil("~%~%~%", &[]).is_ok(), "multiple ~%% should succeed");
    // ~D with zero and negative
    assert!(format_nil("~D", &[BlissVal::from_fixnum(0)]).is_ok());
    assert!(format_nil("~D", &[BlissVal::from_fixnum(-42)]).is_ok());
    // ~B with zero
    assert!(format_nil("~B", &[BlissVal::from_fixnum(0)]).is_ok());
    // ~X with large value
    assert!(format_nil("~X", &[BlissVal::from_fixnum(0xDEAD)]).is_ok());
}
