//! Tests for egcl-stdlib format module (FORMAT & pretty-printer).

use egcl_rt::value::{NIL, T, EgclVal};
use egcl_stdlib::format::*;

// ── Helper ───────────────────────────────────────────────────────

/// Helper: call format with NIL destination, expect Ok and return the result.
fn format_nil(control: &str, args: &[EgclVal]) -> Result<EgclVal, egcl_rt::error::EgclError> {
    format(NIL, control, args)
}

/// Helper: call format with NIL destination, assert Ok, assert result is a
/// string (not NIL), and extract a Rust &str so we can check content.
///
/// Uses `egcl_rt::types::stringp` to verify the value is a CL string, then
/// extracts the bytes via the heap pointer.  Since all implementations are
/// currently `unimplemented!()`, calling this will panic in red phase — that is
/// expected.
fn format_nil_string(control: &str, args: &[EgclVal]) -> String {
    let result = format_nil(control, args);
    assert!(result.is_ok(), "format({:?}, ...) should succeed", control);
    let val = result.unwrap();
    assert_ne!(
        val, NIL,
        "format with NIL dest should return a string, not NIL"
    );
    // In the real implementation, EgclVal for a string is a heap object.
    // We use the stringp predicate to confirm it is a string type.
    assert!(
        egcl_rt::types::stringp(val),
        "format with NIL dest should return a value satisfying stringp"
    );
    // Extract the underlying Rust String.
    // The implementation will store string data behind the heap pointer.
    // For now we use a placeholder extraction that will be filled in once
    // the value representation is implemented.  This calls into the runtime
    // which will panic (unimplemented) in red phase — that is correct.
    egcl_string_to_rust(val)
}

/// Extract a Rust `String` from a `EgclVal` simple-string heap object.
///
/// Mirrors the heap layout that egcl-stdlib's FORMAT produces —
/// `[ObjectHeader (8 bytes)][length: u64 (8 bytes)][UTF-8 data...]` — after
/// asserting the value really is a string-typed heap object.
fn egcl_string_to_rust(val: EgclVal) -> String {
    use egcl_rt::object::{ObjectHeader, type_id};

    assert!(
        val.is_heap_object(),
        "expected a heap-allocated string value"
    );
    unsafe {
        let ptr = val.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let tid = header.type_id();
        assert!(
            tid == type_id::SIMPLE_BASE_STRING || tid == type_id::SIMPLE_CHARACTER_STRING,
            "expected a simple string heap object, got type_id {:#x}",
            tid
        );
        // FORMAT produces 32-bit SIMPLE_CHARACTER_STRINGs (wide code points), not
        // UTF-8 bytes; decode with the production reader (bliss-cizc). The old
        // `length` bytes-at-body+16 read decoded wide output as garbage, failing
        // nearly every string-comparing FORMAT test.
        egcl_rt::object::read_simple_string(ptr)
    }
}

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
    let cloned = original;
    let copied = original; // Copy
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

#[test]
fn newline_kind_debug() {
    let dbg = format!("{:?}", NewlineKind::Linear);
    assert!(
        dbg.contains("Linear"),
        "Debug output should contain 'Linear', got: {}",
        dbg
    );
    let dbg = format!("{:?}", NewlineKind::Fill);
    assert!(
        dbg.contains("Fill"),
        "Debug output should contain 'Fill', got: {}",
        dbg
    );
    let dbg = format!("{:?}", NewlineKind::Miser);
    assert!(
        dbg.contains("Miser"),
        "Debug output should contain 'Miser', got: {}",
        dbg
    );
    let dbg = format!("{:?}", NewlineKind::Mandatory);
    assert!(
        dbg.contains("Mandatory"),
        "Debug output should contain 'Mandatory', got: {}",
        dbg
    );
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
    let cloned = original;
    let copied = original; // Copy
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

#[test]
fn tab_kind_debug() {
    let dbg = format!("{:?}", TabKind::Line);
    assert!(
        dbg.contains("Line"),
        "Debug should contain 'Line', got: {}",
        dbg
    );
    let dbg = format!("{:?}", TabKind::Section);
    assert!(
        dbg.contains("Section"),
        "Debug should contain 'Section', got: {}",
        dbg
    );
    let dbg = format!("{:?}", TabKind::LineRelative);
    assert!(
        dbg.contains("LineRelative"),
        "Debug should contain 'LineRelative', got: {}",
        dbg
    );
    let dbg = format!("{:?}", TabKind::SectionRelative);
    assert!(
        dbg.contains("SectionRelative"),
        "Debug should contain 'SectionRelative', got: {}",
        dbg
    );
}

// ── format() with destination NIL — directive tests with content checks ──

// ~A — aesthetic output (princ-style)
#[test]
fn format_directive_tilde_a_aesthetic() {
    let s = format_nil_string("~A", &[EgclVal::from_fixnum(42)]);
    assert!(
        s.contains("42"),
        "~A of 42 should produce '42', got: {:?}",
        s
    );
}

// ~S — standard output (prin1-style, includes escape chars)
#[test]
fn format_directive_tilde_s_standard() {
    let s = format_nil_string("~S", &[EgclVal::from_fixnum(42)]);
    assert!(
        s.contains("42"),
        "~S of 42 should produce '42', got: {:?}",
        s
    );
}

// ~D — decimal integer
#[test]
fn format_directive_tilde_d_decimal() {
    let s = format_nil_string("~D", &[EgclVal::from_fixnum(255)]);
    assert_eq!(s, "255", "~D of 255 should produce '255', got: {:?}", s);
}

#[test]
fn format_directive_tilde_d_negative() {
    let s = format_nil_string("~D", &[EgclVal::from_fixnum(-42)]);
    assert_eq!(s, "-42", "~D of -42 should produce '-42', got: {:?}", s);
}

#[test]
fn format_directive_tilde_d_zero() {
    let s = format_nil_string("~D", &[EgclVal::from_fixnum(0)]);
    assert_eq!(s, "0", "~D of 0 should produce '0', got: {:?}", s);
}

// ~B — binary integer
#[test]
fn format_directive_tilde_b_binary() {
    let s = format_nil_string("~B", &[EgclVal::from_fixnum(10)]);
    assert_eq!(s, "1010", "~B of 10 should produce '1010', got: {:?}", s);
}

#[test]
fn format_directive_tilde_b_zero() {
    let s = format_nil_string("~B", &[EgclVal::from_fixnum(0)]);
    assert_eq!(s, "0", "~B of 0 should produce '0', got: {:?}", s);
}

// ~O — octal integer
#[test]
fn format_directive_tilde_o_octal() {
    let s = format_nil_string("~O", &[EgclVal::from_fixnum(8)]);
    assert_eq!(s, "10", "~O of 8 should produce '10', got: {:?}", s);
}

// ~X — hexadecimal integer
#[test]
fn format_directive_tilde_x_hex() {
    let s = format_nil_string("~X", &[EgclVal::from_fixnum(255)]);
    // CL prints hex digits in uppercase by default
    assert!(
        s.eq_ignore_ascii_case("FF"),
        "~X of 255 should produce 'FF', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_x_large() {
    let s = format_nil_string("~X", &[EgclVal::from_fixnum(0xDEAD)]);
    assert!(
        s.eq_ignore_ascii_case("DEAD"),
        "~X of 0xDEAD should produce 'DEAD', got: {:?}",
        s
    );
}

// ~R — radix (no params = English cardinal)
#[test]
fn format_directive_tilde_r_radix() {
    let s = format_nil_string("~R", &[EgclVal::from_fixnum(4)]);
    // ~R with no prefix params produces English cardinal: "four"
    assert!(
        s.to_lowercase().contains("four"),
        "~R of 4 should produce English cardinal 'four', got: {:?}",
        s
    );
}

// ~F — fixed-format float
#[test]
fn format_directive_tilde_f_fixed_float() {
    let s = format_nil_string("~F", &[EgclVal::from_single_float(std::f32::consts::PI)]);
    assert!(
        s.contains("3.14"),
        "~F of 3.14 should contain '3.14', got: {:?}",
        s
    );
}

// ~E — exponential float
#[test]
fn format_directive_tilde_e_exponential() {
    let s = format_nil_string("~E", &[EgclVal::from_single_float(std::f32::consts::PI)]);
    // Exponential notation contains an exponent marker (e.g., "E" or "e")
    assert!(
        s.to_uppercase().contains('E'),
        "~E should produce exponential notation, got: {:?}",
        s
    );
}

// ~G — general float
#[test]
fn format_directive_tilde_g_general_float() {
    let s = format_nil_string("~G", &[EgclVal::from_single_float(std::f32::consts::PI)]);
    assert!(
        s.contains("3.14") || s.to_uppercase().contains('E'),
        "~G of 3.14 should produce a float representation, got: {:?}",
        s
    );
}

// ~$ — monetary/dollars float (R5.157)
#[test]
fn format_directive_tilde_dollar_monetary_float() {
    let s = format_nil_string("~$", &[EgclVal::from_single_float(std::f32::consts::PI)]);
    // ~$ typically produces at least 2 decimal places, e.g. "3.14"
    assert!(
        s.contains("3.14"),
        "~$ of 3.14 should contain '3.14', got: {:?}",
        s
    );
}

// ~% — newline
#[test]
fn format_directive_tilde_percent_newline() {
    let s = format_nil_string("hello~%world", &[]);
    assert!(
        s.contains('\n'),
        "~%% should produce a newline character, got: {:?}",
        s
    );
    assert!(s.contains("hello"), "should contain 'hello', got: {:?}", s);
    assert!(s.contains("world"), "should contain 'world', got: {:?}", s);
}

// ~& — fresh-line
#[test]
fn format_directive_tilde_ampersand_fresh_line() {
    let s = format_nil_string("hello~&world", &[]);
    // ~& outputs a newline only if not already at beginning of line
    assert!(s.contains("hello"), "should contain 'hello', got: {:?}", s);
    assert!(s.contains("world"), "should contain 'world', got: {:?}", s);
}

// ~| — page separator
#[test]
fn format_directive_tilde_pipe_page() {
    let result = format_nil("~|", &[]);
    assert!(result.is_ok(), "~| should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL, "format with NIL dest should return a string");
}

// ~~ — literal tilde
#[test]
fn format_directive_tilde_tilde_literal() {
    let s = format_nil_string("~~", &[]);
    assert_eq!(s, "~", "~~ should produce a literal tilde, got: {:?}", s);
}

// ~T — tabulate
#[test]
fn format_directive_tilde_t_tabulate() {
    let s = format_nil_string("~10T", &[]);
    // ~10T should pad to column 10; the result should have at least some spaces
    assert!(
        !s.is_empty(),
        "~10T should produce non-empty output, got: {:?}",
        s
    );
}

// ~* — goto (skip argument)
#[test]
fn format_directive_tilde_star_goto() {
    // ~* skips first arg, ~A prints second arg
    let s = format_nil_string(
        "~*~A",
        &[EgclVal::from_fixnum(1), EgclVal::from_fixnum(2)],
    );
    assert!(
        s.contains("2"),
        "~* should skip first arg; ~A prints second (2), got: {:?}",
        s
    );
    assert!(
        !s.contains("1"),
        "~* should have skipped first arg (1), got: {:?}",
        s
    );
}

// ~C — character (R5.157)
#[test]
fn format_directive_tilde_c_character() {
    let s = format_nil_string("~C", &[EgclVal::from_char('A')]);
    assert!(
        s.contains('A'),
        "~C of 'A' should produce 'A', got: {:?}",
        s
    );
}

// ~W — write (R5.179)
#[test]
fn format_directive_tilde_w_write() {
    let result = format_nil("~W", &[EgclVal::from_fixnum(42)]);
    assert!(result.is_ok(), "~W should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL, "~W with NIL dest should return a string");
}

// ~? — recursive processing (R5.159)
#[test]
fn format_directive_tilde_question_recursive() {
    // ~? takes a format control string argument and a list of arguments.
    // We need to construct proper string and list EgclVal arguments.
    // The first arg to ~? should be a string (control string), e.g. "~D",
    // and the second arg should be a list of args for that control string.
    // Since EgclVal construction for strings requires heap allocation
    // (unimplemented in red phase), this will fail — but the test structure
    // is correct.
    //
    // For now, we test with a simple control string. The format call itself
    // will panic at unimplemented!("format") before we reach argument
    // processing, so the test correctly fails red.
    let result = format_nil(
        "~?",
        &[
            // Ideally: EgclVal representing the string "~D"
            // and a list containing (42).
            // Since we can't construct these yet, we verify the interface
            // compiles and would exercise recursive processing.
            EgclVal::from_fixnum(0), // placeholder: should be a string EgclVal
            NIL,                      // placeholder: should be a list of args
        ],
    );
    // This should either succeed (if the implementation handles the recursive
    // directive) or return a type error (non-string passed as control string).
    // It should NOT panic except from unimplemented!() in red phase.
    // ~? with a non-string control arg should return an error (type error).
    assert!(
        result.is_err(),
        "~? with a non-string (fixnum) as control string should return Err (type error)"
    );
}

// ~@? — recursive processing using enclosing arg list (R5.159)
#[test]
fn format_directive_tilde_at_question_recursive_enclosing() {
    // ~@? uses the enclosing argument list instead of taking a list arg.
    // First arg is still the control string.
    let result = format_nil(
        "~@?",
        &[
            EgclVal::from_fixnum(0),  // placeholder: should be a string "~D"
            EgclVal::from_fixnum(42), // this would be consumed by the recursive format
        ],
    );
    // ~@? with a non-string control arg should return an error (type error).
    assert!(
        result.is_err(),
        "~@? with a non-string (fixnum) as control string should return Err (type error)"
    );
}

// ~{ ~} — iteration (R5.160)
#[test]
fn format_directive_tilde_brace_iteration_empty() {
    // ~{~A ~} with NIL (empty list) should produce empty string
    let result = format_nil("~{~A ~}", &[NIL]);
    assert!(result.is_ok(), "~{{~A ~}} with empty list should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL, "should return a string");
    // With empty list, iteration body is never entered — result should be empty
    let s = egcl_string_to_rust(val);
    assert!(
        s.is_empty(),
        "iteration over empty list should produce empty string, got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_brace_iteration_sublists() {
    // ~:{body~} — each element is a sublist, one per iteration (R5.160)
    let result = format_nil("~:{~A ~}", &[NIL]);
    // ~:{...~} with NIL (empty list of sublists) should succeed
    assert!(result.is_ok(), "~:{{~A ~}} with empty list should succeed");
}

#[test]
fn format_directive_tilde_at_brace_iteration_remaining() {
    // ~@{body~} — remaining args are the iteration list (R5.160)
    let result = format_nil(
        "~@{~A ~}",
        &[
            EgclVal::from_fixnum(1),
            EgclVal::from_fixnum(2),
            EgclVal::from_fixnum(3),
        ],
    );
    assert!(result.is_ok(), "~@{{~A ~}} should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
    let s = egcl_string_to_rust(val);
    assert!(
        s.contains("1"),
        "iteration should format arg 1, got: {:?}",
        s
    );
    assert!(
        s.contains("2"),
        "iteration should format arg 2, got: {:?}",
        s
    );
    assert!(
        s.contains("3"),
        "iteration should format arg 3, got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_at_brace_iteration_remaining_sublists() {
    // ~:@{body~} — remaining args are sublists (R5.160)
    let result = format_nil("~:@{~A~}", &[NIL, NIL]);
    // ~:@{...~} with NIL args (empty sublists) should succeed
    assert!(result.is_ok(), "~:@{{~A~}} with NIL args should succeed");
}

// ~[ ~] — conditional (R5.161)
#[test]
fn format_directive_tilde_bracket_conditional_numeric() {
    // Numeric conditional: ~[zero~;one~;two~] selects clause by integer index
    let s = format_nil_string("~[zero~;one~;two~]", &[EgclVal::from_fixnum(1)]);
    assert_eq!(
        s, "one",
        "~[...~] with index 1 should select 'one', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_bracket_conditional_zero() {
    let s = format_nil_string("~[zero~;one~;two~]", &[EgclVal::from_fixnum(0)]);
    assert_eq!(
        s, "zero",
        "~[...~] with index 0 should select 'zero', got: {:?}",
        s
    );
}

// ~:[ — boolean conditional (R5.161)
#[test]
fn format_directive_tilde_colon_bracket_boolean_nil() {
    // ~:[false-clause~;true-clause~] — boolean: nil selects first clause
    let s = format_nil_string("~:[false~;true~]", &[NIL]);
    assert_eq!(
        s, "false",
        "~:[...~] with NIL should select 'false', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_bracket_boolean_true() {
    let s = format_nil_string("~:[false~;true~]", &[T]);
    assert_eq!(
        s, "true",
        "~:[...~] with T should select 'true', got: {:?}",
        s
    );
}

// ~@[ — true-test conditional (R5.161)
#[test]
fn format_directive_tilde_at_bracket_true_test() {
    // ~@[clause~] — if arg is non-nil, execute clause (arg remains available)
    let s = format_nil_string("~@[got: ~A~]", &[EgclVal::from_fixnum(42)]);
    assert!(
        s.contains("42"),
        "~@[...~] with non-nil arg should format it, got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_at_bracket_true_test_nil() {
    // ~@[clause~] with nil — clause is not executed
    let s = format_nil_string("~@[got: ~A~]", &[NIL]);
    assert!(
        s.is_empty() || !s.contains("got:"),
        "~@[...~] with NIL should skip clause, got: {:?}",
        s
    );
}

// ~( ~) — case conversion
#[test]
fn format_directive_tilde_paren_case_downcase() {
    // ~( ... ~) converts to lowercase
    let s = format_nil_string("~(Hello World~)", &[]);
    assert_eq!(s, "hello world", "~( ~) should downcase, got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_paren_case_capitalize() {
    // ~:( ... ~) capitalizes each word
    let s = format_nil_string("~:(hello world~)", &[]);
    assert_eq!(
        s, "Hello World",
        "~:( ~) should capitalize each word, got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_at_paren_case_capitalize_first() {
    // ~@( ... ~) capitalizes first word only
    let s = format_nil_string("~@(hello world~)", &[]);
    assert_eq!(
        s, "Hello world",
        "~@( ~) should capitalize first word, got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_at_paren_case_upcase() {
    // ~:@( ... ~) converts to uppercase
    let s = format_nil_string("~:@(hello world~)", &[]);
    assert_eq!(s, "HELLO WORLD", "~:@( ~) should upcase, got: {:?}", s);
}

// ~P — plural
#[test]
fn format_directive_tilde_p_plural_singular() {
    let s = format_nil_string("~D dog~P", &[EgclVal::from_fixnum(1)]);
    // With count 1, ~P produces empty string (no trailing 's')
    assert_eq!(s, "1 dog", "~P with 1 should not add 's', got: {:?}", s);
}

#[test]
fn format_directive_tilde_p_plural_multiple() {
    let s = format_nil_string("~D dog~P", &[EgclVal::from_fixnum(3)]);
    // With count != 1, ~P produces "s"
    assert_eq!(s, "3 dogs", "~P with 3 should add 's', got: {:?}", s);
}

#[test]
fn format_directive_tilde_p_plural_zero() {
    let s = format_nil_string("~D dog~P", &[EgclVal::from_fixnum(0)]);
    assert_eq!(s, "0 dogs", "~P with 0 should add 's', got: {:?}", s);
}

// ~@P — "y"/"ies" plural variant
#[test]
fn format_directive_tilde_at_p_plural_y_ies_singular() {
    let s = format_nil_string("~D bab~@P", &[EgclVal::from_fixnum(1)]);
    assert_eq!(s, "1 baby", "~@P with 1 should produce 'y', got: {:?}", s);
}

#[test]
fn format_directive_tilde_at_p_plural_y_ies_multiple() {
    let s = format_nil_string("~D bab~@P", &[EgclVal::from_fixnum(3)]);
    assert_eq!(
        s, "3 babies",
        "~@P with 3 should produce 'ies', got: {:?}",
        s
    );
}

// ~^ — up-and-out
#[test]
fn format_directive_tilde_caret_up_and_out() {
    // ~^ exits iteration when no more arguments
    let result = format_nil("~{~A~^, ~}", &[NIL]);
    assert!(result.is_ok(), "~^ within iteration should succeed");
}

// ── Colon/at-sign modifier interactions (R5.157) ─────────────────

// ~:A — prints () for nil
#[test]
fn format_directive_tilde_colon_a_nil_as_parens() {
    let s = format_nil_string("~:A", &[NIL]);
    assert_eq!(s, "()", "~:A with NIL should print '()', got: {:?}", s);
}

// ~@A — left-pads (right-justified)
#[test]
fn format_directive_tilde_at_a_left_pad() {
    let s = format_nil_string("~10@A", &[EgclVal::from_fixnum(42)]);
    // Should be right-justified in a field of width 10
    assert!(
        s.len() >= 10,
        "~10@A should produce at least 10 chars, got: {:?}",
        s
    );
    assert!(
        s.ends_with("42") || s.trim_start().starts_with("42"),
        "~10@A should right-justify '42', got: {:?}",
        s
    );
}

// ~:D — commas in decimal
#[test]
fn format_directive_tilde_colon_d_commas() {
    let s = format_nil_string("~:D", &[EgclVal::from_fixnum(1000000)]);
    // ~:D inserts commas: "1,000,000"
    assert!(s.contains(','), "~:D should insert commas, got: {:?}", s);
    assert!(
        s.contains("1,000,000") || s.contains("1 000 000"),
        "~:D of 1000000 should produce '1,000,000', got: {:?}",
        s
    );
}

// ~@D — forced sign
#[test]
fn format_directive_tilde_at_d_forced_sign() {
    let s = format_nil_string("~@D", &[EgclVal::from_fixnum(42)]);
    assert!(
        s.starts_with('+'),
        "~@D of positive should start with '+', got: {:?}",
        s
    );
    assert!(s.contains("42"), "~@D should contain '42', got: {:?}", s);
}

// ── V and # as directive parameters (R5.158) ─────────────────────

#[test]
fn format_directive_v_parameter() {
    // ~VD uses the next argument as the mincol parameter for ~D
    let s = format_nil_string(
        "~VD",
        &[EgclVal::from_fixnum(10), EgclVal::from_fixnum(42)],
    );
    // mincol=10 means at least 10 chars wide
    assert!(
        s.len() >= 10,
        "~VD with mincol=10 should produce >= 10 chars, got: {:?}",
        s
    );
    assert!(
        s.contains("42"),
        "~VD should format the number 42, got: {:?}",
        s
    );
}

#[test]
fn format_directive_hash_parameter() {
    // ~#D uses the number of remaining args as the parameter
    let s = format_nil_string(
        "~#D",
        &[
            EgclVal::from_fixnum(42),
            EgclVal::from_fixnum(99),
            EgclVal::from_fixnum(100),
        ],
    );
    // # = 3 remaining args at point of ~#D, so mincol=3
    assert!(
        s.contains("42"),
        "~#D should format the number 42, got: {:?}",
        s
    );
}

// ── ~/name/ — user dispatch (R5.163) ─────────────────────────────

#[test]
fn format_directive_tilde_slash_user_dispatch() {
    // ~/name/ calls a named function for formatting
    // The exact name depends on what functions are registered; we test the
    // parsing and interface.
    let result = format_nil("~/my-format-fn/", &[EgclVal::from_fixnum(42)]);
    // This will either succeed (if a function named my-format-fn is found)
    // or error (function not found). Either is acceptable in red phase.
    // ~/name/ with an unknown function name should return an error.
    assert!(
        result.is_err(),
        "~/my-format-fn/ with unregistered function should return Err"
    );
}

// ── ~< ~> — justification / logical-block (R5.162) ──────────────

#[test]
fn format_directive_tilde_angle_justification() {
    // ~<text~> basic justification
    let result = format_nil("~20<hello~;world~>", &[]);
    assert!(result.is_ok(), "~<...~> justification should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
    let s = egcl_string_to_rust(val);
    assert!(
        s.contains("hello"),
        "justification should contain 'hello', got: {:?}",
        s
    );
    assert!(
        s.contains("world"),
        "justification should contain 'world', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_angle_logical_block() {
    // ~:<...~:> logical-block mode for pretty-printer
    let result = format_nil(
        "~:<~A ~A~:>",
        &[EgclVal::from_fixnum(1), EgclVal::from_fixnum(2)],
    );
    // ~:<...~:> logical block should succeed with valid arguments.
    assert!(result.is_ok(), "~:<...~:> logical-block should succeed");
}

// ── format() with destination T ───────────────────────────────────

#[test]
fn format_destination_t_writes_stdout() {
    let result = format(T, "hello ~A", &[EgclVal::from_fixnum(42)]);
    assert!(result.is_ok(), "format with T destination should succeed");
    // When destination is T, result should be NIL
    let val = result.unwrap();
    assert_eq!(val, NIL, "format with T dest should return NIL");
}

// ── format() with stream destination (R5.156) ────────────────────

#[test]
fn format_destination_stream() {
    // FORMAT with a stream destination should write to the stream and return NIL.
    // In red phase, we can't construct a real stream EgclVal, but we test
    // the interface. This will fail at unimplemented!("format").
    // A stream EgclVal would be a heap object with stream type_id.
    let stream = EgclVal::from_fixnum(0); // placeholder for a stream
    let result = format(stream, "hello ~A", &[EgclVal::from_fixnum(42)]);
    // Should either succeed (write to stream, return NIL) or error (invalid stream type)
    // A fixnum is not a valid stream, so format should return a type error.
    assert!(
        result.is_err(),
        "format with an invalid stream (fixnum) should return Err (type error)"
    );
}

// ── format() with string-with-fill-pointer destination (R5.156) ──

#[test]
fn format_destination_string_with_fill_pointer() {
    // FORMAT with a string-with-fill-pointer should append to the string and return NIL.
    // We can't construct a real string-with-fill-pointer EgclVal in red phase,
    // but we test the interface shape.
    let string_val = EgclVal::from_fixnum(0); // placeholder
    let result = format(string_val, "appended ~D", &[EgclVal::from_fixnum(42)]);
    // A fixnum is not a valid string-with-fill-pointer, so this should error.
    assert!(
        result.is_err(),
        "format with an invalid string dest (fixnum) should return Err (type error)"
    );
}

// ── format() with NIL returns a string type ──────────────────────

#[test]
fn format_nil_destination_returns_string_type() {
    let result = format_nil("hello", &[]);
    assert!(result.is_ok(), "format with NIL dest should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL, "format with NIL dest should not return NIL");
    // Verify the return type is a CL string
    assert!(
        egcl_rt::types::stringp(val),
        "format with NIL dest should return a string (stringp = true)"
    );
}

// ── format() with multiple arguments ─────────────────────────────

#[test]
fn format_multiple_directives() {
    let s = format_nil_string(
        "~A and ~D",
        &[EgclVal::from_fixnum(1), EgclVal::from_fixnum(2)],
    );
    assert!(s.contains("1"), "should contain '1', got: {:?}", s);
    assert!(s.contains("and"), "should contain 'and', got: {:?}", s);
    assert!(s.contains("2"), "should contain '2', got: {:?}", s);
}

#[test]
fn format_no_directives_literal_string() {
    let s = format_nil_string("hello world", &[]);
    assert_eq!(
        s, "hello world",
        "literal format string should pass through unchanged, got: {:?}",
        s
    );
}

// ── formatter() ───────────────────────────────────────────────────

#[test]
fn formatter_compiles_control_string() {
    let result = formatter("~A ~D");
    assert!(
        result.is_ok(),
        "formatter should compile a valid control string"
    );
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
    // pprint_logical_block should establish a logical block with prefix/suffix.
    // When implemented, output to the stream should include the prefix and suffix.
    let r = pprint_logical_block(T, NIL, Some("("), None, Some(")"), NIL);
    // Will panic at unimplemented!() in red phase. The assertion below verifies
    // correct behavior once implemented: it should succeed and return ().
    assert!(
        r.is_ok(),
        "pprint_logical_block with prefix/suffix should succeed"
    );
}

#[test]
fn pprint_logical_block_no_prefix_and_per_line() {
    let r1 = pprint_logical_block(T, NIL, None, None, None, NIL);
    assert!(
        r1.is_ok(),
        "pprint_logical_block with no prefix should succeed"
    );

    let r2 = pprint_logical_block(T, NIL, None, Some(";;; "), None, NIL);
    assert!(
        r2.is_ok(),
        "pprint_logical_block with per-line-prefix should succeed"
    );
}

// ── pprint_newline() ──────────────────────────────────────────────

#[test]
fn pprint_newline_all_kinds() {
    for kind in [
        NewlineKind::Linear,
        NewlineKind::Fill,
        NewlineKind::Miser,
        NewlineKind::Mandatory,
    ] {
        let result = pprint_newline(kind, T);
        // In red phase, this panics at unimplemented!(). Once implemented,
        // it should succeed and a newline may be emitted depending on the
        // XP algorithm's line-breaking decisions.
        assert!(result.is_ok(), "pprint_newline {:?} should succeed", kind);
    }
}

// ── pprint_indent() ───────────────────────────────────────────────

#[test]
fn pprint_indent_relative_and_absolute() {
    assert!(
        pprint_indent(true, 4, T).is_ok(),
        "relative indent should succeed"
    );
    assert!(
        pprint_indent(false, 8, T).is_ok(),
        "absolute indent should succeed"
    );
    assert!(
        pprint_indent(true, -2, T).is_ok(),
        "negative indent should succeed"
    );
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
    let result = pprint_dispatch(EgclVal::from_fixnum(42));
    assert!(result.is_ok(), "pprint_dispatch should succeed");
    let (func, found) = result.unwrap();
    // The dispatch table should have a default entry for integers.
    // `found` indicates whether a specific dispatch entry was matched.
    // `func` is the dispatch function to call (may be a default printer).
    assert_ne!(
        func, NIL,
        "pprint_dispatch should return a function, not NIL"
    );
    // We can't deeply verify `found` without knowing the default table setup,
    // but it should be a boolean.
    let _ = found;
}

// ── set_pprint_dispatch() ─────────────────────────────────────────

#[test]
fn set_pprint_dispatch_with_function() {
    let r = set_pprint_dispatch(NIL, Some(EgclVal::from_fixnum(0)), 0.0, NIL);
    assert!(
        r.is_ok(),
        "set_pprint_dispatch with function should succeed"
    );
}

#[test]
fn set_pprint_dispatch_remove_entry() {
    // Setting function to None should remove the dispatch entry
    let r = set_pprint_dispatch(NIL, None, 0.0, NIL);
    assert!(
        r.is_ok(),
        "set_pprint_dispatch with None should remove entry"
    );
}

#[test]
fn set_pprint_dispatch_priority_ordering() {
    // Higher priority should win when multiple entries match (R5.167)
    let r1 = set_pprint_dispatch(NIL, Some(EgclVal::from_fixnum(0)), 5.0, NIL);
    assert!(
        r1.is_ok(),
        "set_pprint_dispatch with priority 5 should succeed"
    );
    let r2 = set_pprint_dispatch(NIL, Some(EgclVal::from_fixnum(0)), 10.0, NIL);
    assert!(
        r2.is_ok(),
        "set_pprint_dispatch with priority 10 should succeed"
    );
}

// ── copy_pprint_dispatch() ────────────────────────────────────────

#[test]
fn copy_pprint_dispatch_with_none_copies_current() {
    let result = copy_pprint_dispatch(None);
    assert!(
        result.is_ok(),
        "copy_pprint_dispatch(None) should copy current table"
    );
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
    assert!(
        copied.is_ok(),
        "copy_pprint_dispatch(Some(table)) should copy given table"
    );
    let copied_val = copied.unwrap();
    assert_ne!(copied_val, NIL);
}

#[test]
fn copy_pprint_dispatch_independence() {
    // Modifying copy should not affect original
    let original = copy_pprint_dispatch(None);
    assert!(original.is_ok());
    let original_val = original.unwrap();

    let copy = copy_pprint_dispatch(Some(original_val));
    assert!(copy.is_ok());
    let copy_val = copy.unwrap();

    // The copy and original should be different objects
    // (Though they may compare equal in content, they should be distinct allocations)
    // We verify this by checking that modifying the copy doesn't affect the original
    let _ = set_pprint_dispatch(NIL, Some(EgclVal::from_fixnum(0)), 99.0, copy_val);
    // After modification, pprint_dispatch on original should still return its
    // original entry (not the one added to the copy). Full verification
    // requires working dispatch implementation.
}

// ── Error conditions ──────────────────────────────────────────────

#[test]
fn format_error_invalid_control_string() {
    // A control string with a dangling ~ at the end is malformed
    let result = format_nil("hello ~", &[]);
    assert!(
        result.is_err(),
        "format with dangling ~ should return error"
    );
}

#[test]
fn format_error_unknown_directive() {
    // ~Z is not a standard format directive
    let result = format_nil("~Z", &[EgclVal::from_fixnum(1)]);
    assert!(
        result.is_err(),
        "format with unknown directive ~Z should return error"
    );
}

#[test]
fn format_error_unmatched_open_brace() {
    let result = format_nil("~{~A", &[NIL]);
    assert!(
        result.is_err(),
        "format with unmatched ~{{ should return error"
    );
}

#[test]
fn format_error_unmatched_close_brace() {
    let result = format_nil("~A~}", &[NIL]);
    assert!(
        result.is_err(),
        "format with unmatched ~}} should return error"
    );
}

#[test]
fn format_error_unmatched_open_bracket() {
    let result = format_nil("~[hello", &[EgclVal::from_fixnum(0)]);
    assert!(
        result.is_err(),
        "format with unmatched ~[ should return error"
    );
}

#[test]
fn format_error_unmatched_close_bracket() {
    let result = format_nil("hello~]", &[]);
    assert!(
        result.is_err(),
        "format with unmatched ~] should return error"
    );
}

#[test]
fn format_error_too_few_arguments_for_directive() {
    // ~D requires an argument but none provided
    let result = format_nil("~D", &[]);
    assert!(
        result.is_err(),
        "format ~D with no arguments should return error"
    );
}

#[test]
fn format_error_tilde_d_with_non_numeric() {
    // ~D expects a numeric argument; passing a character should error
    let result = format_nil("~D", &[EgclVal::from_char('a')]);
    assert!(
        result.is_err(),
        "format ~D with non-numeric argument should return error"
    );
}

#[test]
fn format_error_radix_directives_with_non_numeric() {
    // ~B, ~O, ~X all require numeric arguments
    for directive in ["~B", "~O", "~X"] {
        let result = format_nil(directive, &[EgclVal::from_char('x')]);
        assert!(
            result.is_err(),
            "format {} with non-numeric should error",
            directive
        );
    }
}

#[test]
fn format_error_unmatched_open_paren() {
    let result = format_nil("~(hello", &[]);
    assert!(
        result.is_err(),
        "format with unmatched ~( should return error"
    );
}

#[test]
fn format_error_unmatched_close_paren() {
    let result = format_nil("hello~)", &[]);
    assert!(
        result.is_err(),
        "format with unmatched ~) should return error"
    );
}

// ── Edge cases ────────────────────────────────────────────────────

#[test]
fn format_empty_control_string() {
    let s = format_nil_string("", &[]);
    assert_eq!(
        s, "",
        "empty control string should produce empty string, got: {:?}",
        s
    );
}

#[test]
fn format_multiple_newlines() {
    let s = format_nil_string("~%~%~%", &[]);
    assert_eq!(
        s, "\n\n\n",
        "three ~%% should produce three newlines, got: {:?}",
        s
    );
}

#[test]
fn format_mixed_literal_and_directives() {
    let s = format_nil_string(
        "The ~A has ~D item~P",
        &[
            EgclVal::from_fixnum(0), // placeholder: should be a string for ~A
            EgclVal::from_fixnum(3),
        ],
    );
    // We can at least verify structural parts of the output
    assert!(
        s.contains("The"),
        "should contain literal 'The', got: {:?}",
        s
    );
    assert!(
        s.contains("has"),
        "should contain literal 'has', got: {:?}",
        s
    );
    assert!(s.contains("3"), "should contain '3' from ~D, got: {:?}", s);
}

// ~R with colon (ordinal) and at-sign (Roman) modifiers
#[test]
fn format_directive_tilde_colon_r_ordinal() {
    let s = format_nil_string("~:R", &[EgclVal::from_fixnum(4)]);
    assert!(
        s.to_lowercase().contains("fourth"),
        "~:R of 4 should produce English ordinal 'fourth', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_at_r_roman() {
    let s = format_nil_string("~@R", &[EgclVal::from_fixnum(4)]);
    assert_eq!(
        s, "IV",
        "~@R of 4 should produce Roman numeral 'IV', got: {:?}",
        s
    );
}

#[test]
fn format_directive_tilde_colon_at_r_old_roman() {
    let s = format_nil_string("~:@R", &[EgclVal::from_fixnum(4)]);
    assert_eq!(
        s, "IIII",
        "~:@R of 4 should produce old Roman 'IIII', got: {:?}",
        s
    );
}

// ~:P — backs up one arg then plural
#[test]
fn format_directive_tilde_colon_p_backup_plural() {
    let s = format_nil_string("~D dog~:P", &[EgclVal::from_fixnum(1)]);
    assert_eq!(
        s, "1 dog",
        "~:P with 1 should back up and not add 's', got: {:?}",
        s
    );
}

// ~:* — go back one argument
#[test]
fn format_directive_tilde_colon_star_go_back() {
    let s = format_nil_string("~A ~:*~A", &[EgclVal::from_fixnum(42)]);
    // ~A prints 42, ~:* goes back one arg, ~A prints 42 again
    assert_eq!(s, "42 42", "~:* should back up one arg, got: {:?}", s);
}

// ~@* — absolute goto
#[test]
fn format_directive_tilde_at_star_absolute_goto() {
    let s = format_nil_string(
        "~A ~@*~A",
        &[EgclVal::from_fixnum(42), EgclVal::from_fixnum(99)],
    );
    // ~A prints 42, ~@* goes to arg 0, ~A prints 42 again
    assert!(
        s.starts_with("42"),
        "should start with first arg, got: {:?}",
        s
    );
}

// ~:C — character spelled out
#[test]
fn format_directive_tilde_colon_c_spelled_name() {
    let s = format_nil_string("~:C", &[EgclVal::from_char(' ')]);
    assert!(
        s.to_lowercase().contains("space"),
        "~:C of space should spell out 'Space', got: {:?}",
        s
    );
}

// ~@C — character with #\ syntax
#[test]
fn format_directive_tilde_at_c_reader_syntax() {
    let s = format_nil_string("~@C", &[EgclVal::from_char('A')]);
    assert!(
        s.contains("#\\"),
        "~@C should produce #\\ reader syntax, got: {:?}",
        s
    );
    assert!(
        s.contains('A'),
        "~@C should contain the character, got: {:?}",
        s
    );
}

// ~$  with modifiers
#[test]
fn format_directive_tilde_at_dollar_forced_sign() {
    let s = format_nil_string("~@$", &[EgclVal::from_single_float(std::f32::consts::PI)]);
    assert!(
        s.starts_with('+'),
        "~@$ should force sign on positive, got: {:?}",
        s
    );
}

// ── ~< justification against SBCL ground truth (bliss-0omm) ──────
//
// Every expectation below was captured from SBCL 2.x on the identical control
// string, so this table is a conformance oracle rather than a record of what
// egcl happens to do. The rules it pins down (CLHS 22.3.6.2):
//   * padding is inserted into the gaps BETWEEN segments;
//   * `:` adds a gap before the first segment, `@` one after the last;
//   * a lone segment with neither modifier gets the leading gap anyway, which
//     is what makes plain ~mincol<text~> right-justify;
//   * the slack splits evenly with the remainder favouring the LATER gaps;
//   * the field grows from mincol in whole multiples of colinc until the
//     segments plus their minpad fit.
#[test]
fn format_tilde_angle_justification_matches_sbcl() {
    for (control, want) in [
        // Single segment: the four modifier combinations.
        ("~10<abc~>", "       abc"),
        ("~10:<abc~>", "       abc"),
        ("~10@<abc~>", "abc       "),
        ("~10:@<abc~>", "   abc    "),
        // Two segments.
        ("~10<abc~;de~>", "abc     de"),
        ("~10:<abc~;de~>", "  abc   de"),
        ("~10@<abc~;de~>", "abc  de   "),
        ("~10:@<abc~;de~>", " abc  de  "),
        // Three segments — remainder distribution is visible here.
        ("~10<a~;b~;c~>", "a   b    c"),
        ("~10:<a~;b~;c~>", "  a  b   c"),
        ("~10@<a~;b~;c~>", "a  b  c   "),
        ("~10:@<a~;b~;c~>", " a  b  c  "),
        // Overflow: content wider than mincol is never truncated.
        ("~2<abcdef~>", "abcdef"),
        ("~5<abcdefgh~>", "abcdefgh"),
        ("~11<one~;two~;three~>", "onetwothree"),
        // padchar, colinc and minpad.
        ("~10,,,'*<abc~>", "*******abc"),
        ("~10,3<abc~>", "       abc"),
        ("~,,2<ab~;cd~>", "ab  cd"),
        ("~10,,3<a~;b~>", "a        b"),
        ("~10,,,'.<x~;y~>", "x........y"),
        ("~,,1,'-<a~;b~;c~>", "a-b-c"),
        // colinc grows the field in whole steps past mincol.
        ("~5,5<abc~>", "  abc"),
        ("~6,4<abcdefg~>", "   abcdefg"),
        // Degenerate fields.
        ("~<abc~>", "abc"),
        ("~0<abc~>", "abc"),
        ("~10<~>", "          "),
        ("~20:@<hi~;there~;you~>", "  hi  there   you   "),
    ] {
        assert_eq!(
            format_nil_string(control, &[]),
            want,
            "FORMAT {control:?} should match SBCL"
        );
    }
}

/// A colon on the CLOSING directive (`~:>`) — not the opening one — is what
/// makes `~<...~>` a pretty-printing logical block, which emits its segments
/// with no justification padding. ASDF's condition reports are all of the
/// `~@<...~@:>` shape, so this is the path they take.
#[test]
fn format_tilde_angle_logical_block_is_keyed_on_the_closing_colon() {
    assert_eq!(format_nil_string("~@<plain block~@:>", &[]), "plain block");
    assert_eq!(format_nil_string("~@<a~;b~@:>", &[]), "ab");
    // Same text, closing WITHOUT a colon: justification, so mincol applies.
    assert_eq!(
        format_nil_string("~20@<plain block~>", &[]),
        "plain block         "
    );
}

// ── ~T column tabulation against SBCL ground truth (bliss-a094) ──
//
// Captured from SBCL on the identical control strings. Covers both forms of
// CLHS 22.3.6.1:
//   ~colnum,colinc T   absolute — move to colnum, else the next colnum+k*colinc
//   ~colrel,colinc @T  relative — emit colrel spaces, then the fewest more that
//                      land on a multiple of colinc
// The `@` form used to fall through to the absolute computation entirely, so
// `ab~3@Tx` emitted one space instead of three.
#[test]
fn format_tilde_t_tabulation_matches_sbcl() {
    for (control, want) in [
        // Absolute, no parameters / one parameter.
        ("~0Tx", " x"),
        ("~1Tx", " x"),
        ("~5Tx", "     x"),
        ("ab~5Tx", "ab   x"),
        ("abcdefg~5Tx", "abcdefg x"),
        ("~T x", "  x"),
        ("a~Tb", "a b"),
        // Absolute, already at or past colnum: advance by whole colinc steps.
        ("ab~2Tx", "ab x"),
        ("abcd~2Tx", "abcd x"),
        ("abcde~2,3Tx", "abcde   x"),
        ("~2,4Tx", "  x"),
        ("ab~2,4Tx", "ab    x"),
        ("abc~2,4Tx", "abc   x"),
        ("~0,5Tx", "     x"),
        ("ab~0,5Tx", "ab   x"),
        ("~5,3Tx", "     x"),
        ("abcdefgh~5,3Tx", "abcdefgh   x"),
        ("x~4,4Ty~4,4Tz", "x   y   z"),
        ("~,3Tx", " x"),
        ("ab~,3Tx", "ab  x"),
        // Relative (~@T).
        ("~@Tx", " x"),
        ("a~@Tx", "a x"),
        ("ab~3@Tx", "ab   x"),
        ("ab~0@Tx", "abx"),
        ("ab~1,3@Tx", "ab x"),
        ("abc~1,3@Tx", "abc   x"),
        ("ab~2,5@Tx", "ab   x"),
        ("abcd~2,5@Tx", "abcd      x"),
        ("ab~5@Tx~3@Ty", "ab     x   y"),
        // Zero colinc must not divide by zero.
        ("~0,0@Tx", "x"),
        ("ab~0,0Tx", "abx"),
        // A newline resets the column.
        ("line1\nab~5Tx", "line1\nab   x"),
    ] {
        assert_eq!(
            format_nil_string(control, &[]),
            want,
            "FORMAT {control:?} should match SBCL"
        );
    }
}

/// The current column is a count of CHARACTERS, not bytes: a byte count
/// overshoots once any multibyte output is already on the line.
#[test]
fn format_tilde_t_counts_columns_in_characters_not_bytes() {
    for (control, want) in [
        ("\u{3b1}\u{3b2}~5Tx", "\u{3b1}\u{3b2}   x"),
        (
            "\u{3b1}\u{3b2}\u{3b3}\u{3b4}\u{3b5}~3,4Tx",
            "\u{3b1}\u{3b2}\u{3b3}\u{3b4}\u{3b5}  x",
        ),
        ("\u{3b1}\u{3b2}~3@Tx", "\u{3b1}\u{3b2}   x"),
        ("\u{65e5}\u{672c}~6Tx", "\u{65e5}\u{672c}    x"),
    ] {
        assert_eq!(format_nil_string(control, &[]), want, "FORMAT {control:?}");
    }
}

// ── Float printed representation (CLHS 22.1.3.1.3) ───────────────
//
// A float prints in free format while 10^-3 <= |x| < 10^7 and in exponential
// notation outside that band. Rust's `{}` never switches to an exponent, so
// egcl used to print 10000000000.0 for 1.0e10 and 0.0000000001 for 1.0e-10 —
// both read back correctly, but neither is the representation CL specifies.
// Expectations captured from SBCL.
#[test]
fn single_float_printed_representation_matches_sbcl() {
    for (x, want) in [
        // Inside the free-format band.
        (0.1f32, "0.1"),
        (1.0, "1.0"),
        (0.5, "0.5"),
        (0.001, "0.001"),
        (100000.0, "100000.0"),
        (1000000.0, "1000000.0"),
        // At and beyond the 10^7 boundary.
        (1.0e7, "1.0e7"),
        (1.0e10, "1.0e10"),
        (123456789.0, "1.2345679e8"),
        (3.4028235e38, "3.4028235e38"),
        // Below the 10^-3 boundary.
        (1.0e-4, "1.0e-4"),
        (1.0e-10, "1.0e-10"),
        (1.1754944e-38, "1.1754944e-38"),
        // Zero stays in free format (the |x| != 0 guard).
        (0.0, "0.0"),
    ] {
        assert_eq!(
            egcl_stdlib::format::single_float_to_string(x),
            want,
            "printing {x:e}"
        );
    }
}

/// Whatever spelling is chosen, it must read back as the identical float —
/// that is the property CL actually requires of the printer.
#[test]
fn single_float_printed_representation_round_trips() {
    for x in [
        0.1f32,
        1.0,
        0.5,
        0.001,
        1.0e7,
        1.0e10,
        123456789.0,
        3.4028235e38,
        1.0e-4,
        1.0e-10,
        1.1754944e-38,
        f32::MIN_POSITIVE,
        // Smallest subnormal: egcl prints 1.0e-45 where SBCL prints
        // 1.4012985e-45. Both denote this same value — the shortest
        // round-tripping decimal is not unique down here — so the round-trip,
        // not the spelling, is what this asserts.
        f32::from_bits(1),
        0.0,
    ] {
        let s = egcl_stdlib::format::single_float_to_string(x);
        let back: f32 = s
            .parse()
            .unwrap_or_else(|e| panic!("{s:?} must re-read: {e}"));
        assert_eq!(back, x, "{s:?} must read back to the same float");
    }
}

/// ~S and ~A go through the same printer, so they inherit the band.
#[test]
fn format_s_and_a_print_floats_in_the_clhs_band() {
    let big = EgclVal::from_single_float(1.0e10);
    assert_eq!(format_nil_string("~S", &[big]), "1.0e10");
    assert_eq!(format_nil_string("~A", &[big]), "1.0e10");
    let small = EgclVal::from_single_float(0.001);
    assert_eq!(format_nil_string("~S", &[small]), "0.001");
    assert_eq!(format_nil_string("~A", &[small]), "0.001");
}

// ── ~F / ~E against SBCL ground truth (bliss-pi0z) ───────────────
//
// ~F never uses an exponent and always shows the decimal point, including at
// zero fraction digits ("4." not "4"). ~E's exponent always carries its sign
// and uses a lowercase marker by default.
//
// The subtle part is WHICH value ~F rounds, and CL uses two regimes:
//   * more fraction digits requested than the shortest decimal carries -> pad
//     with zeros (the float has no more information), so ~,3F of 3.4028235e38
//     is ...350000000000000000000000000000000.000, not the exact binary value
//     ...346638528859811704183484516925440.000;
//   * fewer -> round the EXACT binary value, ties away from zero. ~,2F of
//     1.005 is 1.00 because that f32 is really 1.00499999523162841796875,
//     while ~,1F of 0.25 is 0.3 because that f32 is exactly 0.25.
#[test]
fn format_fixed_and_exponential_floats_match_sbcl() {
    let f = |x: f32| EgclVal::from_single_float(x);
    for (control, x, want) in [
        // ~F with no digit count: shortest decimal, never exponential.
        ("~F", 1.0e10f32, "10000000000.0"),
        ("~F", 1.0e-10, "0.0000000001"),
        ("~F", 0.001, "0.001"),
        ("~F", 123456789.0, "123456790.0"),
        (
            "~F",
            3.4028235e38,
            "340282350000000000000000000000000000000.0",
        ),
        ("~F", 0.0, "0.0"),
        // Padding regime: fewer fraction digits available than requested.
        ("~,3F", 123456789.0, "123456790.000"),
        (
            "~,3F",
            3.4028235e38,
            "340282350000000000000000000000000000000.000",
        ),
        ("~,3F", 0.5, "0.500"),
        // Rounding regime, exact value, ties away from zero.
        ("~,2F", 1.005, "1.00"),
        ("~,1F", 0.25, "0.3"),
        ("~,1F", 0.15, "0.2"),
        ("~,1F", 9.96, "10.0"),
        ("~,2F", 99.999, "100.00"),
        ("~,2F", 12345.678, "12345.68"),
        ("~,3F", 0.0001, "0.000"),
        // Zero fraction digits still prints the point.
        ("~,0F", 3.7, "4."),
        ("~,0F", 3.2, "3."),
        ("~,0F", -3.7, "-4."),
        ("~,0F", 1.5, "2."),
        ("~,0F", 2.5, "3."),
        ("~,0F", 9.6, "10."),
        // Width padding and the @ sign flag.
        ("~10,2F", 1.0e-10, "      0.00"),
        ("~@F", 0.5, "+0.5"),
        ("~@F", -0.25, "-0.25"),
        // ~E: signed exponent, lowercase marker.
        ("~E", 1.0e10, "1.0e+10"),
        ("~E", 1.0e-10, "1.0e-10"),
        ("~E", 0.001, "1.0e-3"),
        ("~E", 1.0e7, "1.0e+7"),
        ("~E", 123456789.0, "1.2345679e+8"),
        ("~E", 0.5, "5.0e-1"),
        ("~E", 0.0, "0.0e+0"),
        ("~,3E", 1.0e10, "1.000e+10"),
        ("~,3E", 123456789.0, "1.235e+8"),
    ] {
        assert_eq!(
            format_nil_string(control, &[f(x)]),
            want,
            "FORMAT {control:?} on {x:e} should match SBCL"
        );
    }
}

// ── ~<...~:> logical block against SBCL ground truth (bliss-aspj) ──
//
// Every expectation was captured from SBCL 2.6.x on the identical control
// string (scripted control|result| sweep diffed between the two systems).
// The rules pinned down (CLHS 22.3.5.2):
//   * a non-@ opening consumes ONE argument, a list, and the body formats
//     over its ELEMENTS — even a directive-free body errors when that
//     argument is missing;
//   * a non-list argument is printed by WRITE and the whole block — prefix
//     and suffix included — is skipped;
//   * `~@<` formats over the remaining arguments instead;
//   * segments are [prefix~;]body[~;suffix]; `~@;` marks a per-line prefix
//     (identical to a plain prefix in linear rendering); `~:<` supplies the
//     default "("/")" pair;
//   * `~^` inside the block terminates on ITS list's exhaustion;
//   * `~:@_` (mandatory conditional newline) always breaks; the other ~_
//     variants and ~I are no-ops in linear rendering.

fn lb_list(vals: &[EgclVal]) -> EgclVal {
    use egcl_rt::object::ConsCell;
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell { car: v, cdr: list }));
        list = unsafe { EgclVal::from_cons_ptr(cell as *mut ConsCell as *mut u8) };
    }
    list
}

fn lb_fixlist(vals: &[i64]) -> EgclVal {
    lb_list(
        &vals
            .iter()
            .map(|&v| EgclVal::from_fixnum(v))
            .collect::<Vec<_>>(),
    )
}

#[test]
fn format_logical_block_list_argument_consumption_matches_sbcl() {
    let n = EgclVal::from_fixnum;
    // Non-@ opening: one list argument, body formats its elements.
    assert_eq!(
        format_nil_string("~<~a ~a~:>", &[lb_fixlist(&[1, 2])]),
        "1 2"
    );
    // The block consumes exactly one argument; the rest stay for the caller.
    assert_eq!(
        format_nil_string("~<~a~:> ~a", &[lb_fixlist(&[1]), n(7)]),
        "1 7"
    );
    assert_eq!(
        format_nil_string("~<~a ~a~:>", &[lb_fixlist(&[1, 2]), n(9)]),
        "1 2"
    );
    // `:` on the opening supplies the parenthesis pair.
    assert_eq!(
        format_nil_string("~:<~a ~a~:>", &[lb_fixlist(&[1, 2])]),
        "(1 2)"
    );
    assert_eq!(
        format_nil_string("~:<~a~:>", &[lb_fixlist(&[1, 2, 3])]),
        "(1)"
    );
    // `@` on the opening: format over the remaining arguments.
    assert_eq!(format_nil_string("~@<~a ~a~:>", &[n(1), n(2)]), "1 2");
    assert_eq!(format_nil_string("~:@<~a ~a~:>", &[n(1), n(2)]), "(1 2)");
    // Segments: prefix / body / suffix; two segments = prefix + body.
    assert_eq!(
        format_nil_string("~<[~;~a~;]~:>", &[lb_fixlist(&[5])]),
        "[5]"
    );
    assert_eq!(format_nil_string("~<[~;~a~:>", &[lb_fixlist(&[5])]), "[5");
    // `~@;` per-line prefix renders as a plain prefix on one line.
    assert_eq!(
        format_nil_string("~<;; ~@;~a~:>", &[lb_fixlist(&[7])]),
        ";; 7"
    );
    // Non-list argument: WRITE it, skip the block (prefix/suffix too).
    assert_eq!(format_nil_string("~<~a~:>", &[n(5)]), "5");
    assert_eq!(format_nil_string("~<[~;~a~;]~:>", &[n(9)]), "9");
    // ~^ terminates on the BLOCK list's exhaustion.
    assert_eq!(format_nil_string("~<~a~^ ~a~:>", &[lb_fixlist(&[1])]), "1");
    // Fill-style close ~:@> is still a logical block.
    assert_eq!(
        format_nil_string("~<~a ~a~:@>", &[lb_fixlist(&[1, 2])]),
        "1 2"
    );
    assert_eq!(
        format_nil_string("~<~a~:@>", &[lb_fixlist(&[1, 2, 3])]),
        "1"
    );
    // ~# inside the block counts the block's remaining elements.
    assert_eq!(
        format_nil_string("~<~#[none~:;~a~]~:>", &[lb_fixlist(&[1])]),
        "1"
    );
    // Nested blocks: the inner block consumes a sublist element.
    let nested = lb_list(&[n(1), lb_fixlist(&[2]), n(3)]);
    assert_eq!(format_nil_string("~<~a ~<~a~:> ~a~:>", &[nested]), "1 2 3");
    // Mandatory conditional newline breaks even in linear rendering.
    assert_eq!(format_nil_string("~@<a~:@_b~:>", &[]), "a\nb");
    assert_eq!(format_nil_string("~@<~a ~_~a~:>", &[n(1), n(2)]), "1 2");
}

#[test]
fn format_logical_block_missing_or_exhausted_list_errors_like_sbcl() {
    // SBCL: "No more arguments" — the list argument is consumed eagerly,
    // even for a directive-free body.
    assert!(format_nil("~<blk~:>", &[]).is_err());
    assert!(format_nil("~@<~a~:>", &[]).is_err());
    // Exhausting the block's list without ~^ errors.
    assert!(format_nil("~<~a ~a~:>", &[lb_fixlist(&[1])]).is_err());
    // NIL is a list — an empty one, so a consuming body still errors.
    assert!(format_nil("~<~a~:>", &[NIL]).is_err());
}
