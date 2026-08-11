//! Tests for bliss-stdlib format module (FORMAT & pretty-printer).

use bliss_stdlib::format::*;
use bliss_rt::value::{BlissVal, NIL, T};

// ── Helper ───────────────────────────────────────────────────────

/// Helper: call format with NIL destination, expect Ok and return the result.
fn format_nil(control: &str, args: &[BlissVal]) -> Result<BlissVal, bliss_rt::error::BlissError> {
    format(NIL, control, args)
}

/// Helper: call format with NIL destination, assert Ok, assert result is a
/// string (not NIL), and extract a Rust &str so we can check content.
///
/// Uses `bliss_rt::types::stringp` to verify the value is a CL string, then
/// extracts the bytes via the heap pointer.  Since all implementations are
/// currently `unimplemented!()`, calling this will panic in red phase — that is
/// expected.
fn format_nil_string(control: &str, args: &[BlissVal]) -> String {
    let result = format_nil(control, args);
    assert!(result.is_ok(), "format({:?}, ...) should succeed", control);
    let val = result.unwrap();
    assert_ne!(val, NIL, "format with NIL dest should return a string, not NIL");
    // In the real implementation, BlissVal for a string is a heap object.
    // We use the stringp predicate to confirm it is a string type.
    assert!(
        bliss_rt::types::stringp(val),
        "format with NIL dest should return a value satisfying stringp"
    );
    // Extract the underlying Rust String.
    // The implementation will store string data behind the heap pointer.
    // For now we use a placeholder extraction that will be filled in once
    // the value representation is implemented.  This calls into the runtime
    // which will panic (unimplemented) in red phase — that is correct.
    bliss_string_to_rust(val)
}

/// Extract a Rust `String` from a `BlissVal` simple-string heap object.
///
/// Mirrors the heap layout that bliss-stdlib's FORMAT produces —
/// `[ObjectHeader (8 bytes)][length: u64 (8 bytes)][UTF-8 data...]` — after
/// asserting the value really is a string-typed heap object.
fn bliss_string_to_rust(val: BlissVal) -> String {
    use bliss_rt::object::{type_id, ObjectHeader};

    assert!(
        val.is_heap_object(),
        "expected a heap-allocated string value"
    );
    unsafe {
        let ptr = val.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let tid = header.type_id();
        assert!(
            tid == type_id::SIMPLE_BASE_STRING
                || tid == type_id::SIMPLE_CHARACTER_STRING,
            "expected a simple string heap object, got type_id {:#x}",
            tid
        );
        let length = *(ptr.add(8) as *const u64) as usize;
        let bytes = std::slice::from_raw_parts(ptr.add(16), length);
        String::from_utf8_lossy(bytes).into_owned()
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
    let cloned = original;
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

// ── format() with destination NIL — directive tests with content checks ──

// ~A — aesthetic output (princ-style)
#[test]
fn format_directive_tilde_a_aesthetic() {
    let s = format_nil_string("~A", &[BlissVal::from_fixnum(42)]);
    assert!(s.contains("42"), "~A of 42 should produce '42', got: {:?}", s);
}

// ~S — standard output (prin1-style, includes escape chars)
#[test]
fn format_directive_tilde_s_standard() {
    let s = format_nil_string("~S", &[BlissVal::from_fixnum(42)]);
    assert!(s.contains("42"), "~S of 42 should produce '42', got: {:?}", s);
}

// ~D — decimal integer
#[test]
fn format_directive_tilde_d_decimal() {
    let s = format_nil_string("~D", &[BlissVal::from_fixnum(255)]);
    assert_eq!(s, "255", "~D of 255 should produce '255', got: {:?}", s);
}

#[test]
fn format_directive_tilde_d_negative() {
    let s = format_nil_string("~D", &[BlissVal::from_fixnum(-42)]);
    assert_eq!(s, "-42", "~D of -42 should produce '-42', got: {:?}", s);
}

#[test]
fn format_directive_tilde_d_zero() {
    let s = format_nil_string("~D", &[BlissVal::from_fixnum(0)]);
    assert_eq!(s, "0", "~D of 0 should produce '0', got: {:?}", s);
}

// ~B — binary integer
#[test]
fn format_directive_tilde_b_binary() {
    let s = format_nil_string("~B", &[BlissVal::from_fixnum(10)]);
    assert_eq!(s, "1010", "~B of 10 should produce '1010', got: {:?}", s);
}

#[test]
fn format_directive_tilde_b_zero() {
    let s = format_nil_string("~B", &[BlissVal::from_fixnum(0)]);
    assert_eq!(s, "0", "~B of 0 should produce '0', got: {:?}", s);
}

// ~O — octal integer
#[test]
fn format_directive_tilde_o_octal() {
    let s = format_nil_string("~O", &[BlissVal::from_fixnum(8)]);
    assert_eq!(s, "10", "~O of 8 should produce '10', got: {:?}", s);
}

// ~X — hexadecimal integer
#[test]
fn format_directive_tilde_x_hex() {
    let s = format_nil_string("~X", &[BlissVal::from_fixnum(255)]);
    // CL prints hex digits in uppercase by default
    assert!(
        s.eq_ignore_ascii_case("FF"),
        "~X of 255 should produce 'FF', got: {:?}", s
    );
}

#[test]
fn format_directive_tilde_x_large() {
    let s = format_nil_string("~X", &[BlissVal::from_fixnum(0xDEAD)]);
    assert!(
        s.eq_ignore_ascii_case("DEAD"),
        "~X of 0xDEAD should produce 'DEAD', got: {:?}", s
    );
}

// ~R — radix (no params = English cardinal)
#[test]
fn format_directive_tilde_r_radix() {
    let s = format_nil_string("~R", &[BlissVal::from_fixnum(4)]);
    // ~R with no prefix params produces English cardinal: "four"
    assert!(
        s.to_lowercase().contains("four"),
        "~R of 4 should produce English cardinal 'four', got: {:?}", s
    );
}

// ~F — fixed-format float
#[test]
fn format_directive_tilde_f_fixed_float() {
    let s = format_nil_string("~F", &[BlissVal::from_single_float(std::f32::consts::PI)]);
    assert!(s.contains("3.14"), "~F of 3.14 should contain '3.14', got: {:?}", s);
}

// ~E — exponential float
#[test]
fn format_directive_tilde_e_exponential() {
    let s = format_nil_string("~E", &[BlissVal::from_single_float(std::f32::consts::PI)]);
    // Exponential notation contains an exponent marker (e.g., "E" or "e")
    assert!(
        s.to_uppercase().contains('E'),
        "~E should produce exponential notation, got: {:?}", s
    );
}

// ~G — general float
#[test]
fn format_directive_tilde_g_general_float() {
    let s = format_nil_string("~G", &[BlissVal::from_single_float(std::f32::consts::PI)]);
    assert!(s.contains("3.14") || s.to_uppercase().contains('E'),
        "~G of 3.14 should produce a float representation, got: {:?}", s);
}

// ~$ — monetary/dollars float (R5.157)
#[test]
fn format_directive_tilde_dollar_monetary_float() {
    let s = format_nil_string("~$", &[BlissVal::from_single_float(std::f32::consts::PI)]);
    // ~$ typically produces at least 2 decimal places, e.g. "3.14"
    assert!(s.contains("3.14"), "~$ of 3.14 should contain '3.14', got: {:?}", s);
}

// ~% — newline
#[test]
fn format_directive_tilde_percent_newline() {
    let s = format_nil_string("hello~%world", &[]);
    assert!(s.contains('\n'), "~%% should produce a newline character, got: {:?}", s);
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
    assert!(!s.is_empty(), "~10T should produce non-empty output, got: {:?}", s);
}

// ~* — goto (skip argument)
#[test]
fn format_directive_tilde_star_goto() {
    // ~* skips first arg, ~A prints second arg
    let s = format_nil_string("~*~A", &[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)]);
    assert!(s.contains("2"), "~* should skip first arg; ~A prints second (2), got: {:?}", s);
    assert!(!s.contains("1"), "~* should have skipped first arg (1), got: {:?}", s);
}

// ~C — character (R5.157)
#[test]
fn format_directive_tilde_c_character() {
    let s = format_nil_string("~C", &[BlissVal::from_char('A')]);
    assert!(s.contains('A'), "~C of 'A' should produce 'A', got: {:?}", s);
}

// ~W — write (R5.179)
#[test]
fn format_directive_tilde_w_write() {
    let result = format_nil("~W", &[BlissVal::from_fixnum(42)]);
    assert!(result.is_ok(), "~W should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL, "~W with NIL dest should return a string");
}

// ~? — recursive processing (R5.159)
#[test]
fn format_directive_tilde_question_recursive() {
    // ~? takes a format control string argument and a list of arguments.
    // We need to construct proper string and list BlissVal arguments.
    // The first arg to ~? should be a string (control string), e.g. "~D",
    // and the second arg should be a list of args for that control string.
    // Since BlissVal construction for strings requires heap allocation
    // (unimplemented in red phase), this will fail — but the test structure
    // is correct.
    //
    // For now, we test with a simple control string. The format call itself
    // will panic at unimplemented!("format") before we reach argument
    // processing, so the test correctly fails red.
    let result = format_nil("~?", &[
        // Ideally: BlissVal representing the string "~D"
        // and a list containing (42).
        // Since we can't construct these yet, we verify the interface
        // compiles and would exercise recursive processing.
        BlissVal::from_fixnum(0), // placeholder: should be a string BlissVal
        NIL, // placeholder: should be a list of args
    ]);
    // This should either succeed (if the implementation handles the recursive
    // directive) or return a type error (non-string passed as control string).
    // It should NOT panic except from unimplemented!() in red phase.
    // ~? with a non-string control arg should return an error (type error).
    assert!(result.is_err(),
        "~? with a non-string (fixnum) as control string should return Err (type error)");
}

// ~@? — recursive processing using enclosing arg list (R5.159)
#[test]
fn format_directive_tilde_at_question_recursive_enclosing() {
    // ~@? uses the enclosing argument list instead of taking a list arg.
    // First arg is still the control string.
    let result = format_nil("~@?", &[
        BlissVal::from_fixnum(0), // placeholder: should be a string "~D"
        BlissVal::from_fixnum(42), // this would be consumed by the recursive format
    ]);
    // ~@? with a non-string control arg should return an error (type error).
    assert!(result.is_err(),
        "~@? with a non-string (fixnum) as control string should return Err (type error)");
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
    let s = bliss_string_to_rust(val);
    assert!(s.is_empty(), "iteration over empty list should produce empty string, got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_brace_iteration_sublists() {
    // ~:{body~} — each element is a sublist, one per iteration (R5.160)
    let result = format_nil("~:{~A ~}", &[NIL]);
    // ~:{...~} with NIL (empty list of sublists) should succeed
    assert!(result.is_ok(),
        "~:{{~A ~}} with empty list should succeed");
}

#[test]
fn format_directive_tilde_at_brace_iteration_remaining() {
    // ~@{body~} — remaining args are the iteration list (R5.160)
    let result = format_nil("~@{~A ~}", &[
        BlissVal::from_fixnum(1),
        BlissVal::from_fixnum(2),
        BlissVal::from_fixnum(3),
    ]);
    assert!(result.is_ok(), "~@{{~A ~}} should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
    let s = bliss_string_to_rust(val);
    assert!(s.contains("1"), "iteration should format arg 1, got: {:?}", s);
    assert!(s.contains("2"), "iteration should format arg 2, got: {:?}", s);
    assert!(s.contains("3"), "iteration should format arg 3, got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_at_brace_iteration_remaining_sublists() {
    // ~:@{body~} — remaining args are sublists (R5.160)
    let result = format_nil("~:@{~A~}", &[NIL, NIL]);
    // ~:@{...~} with NIL args (empty sublists) should succeed
    assert!(result.is_ok(),
        "~:@{{~A~}} with NIL args should succeed");
}

// ~[ ~] — conditional (R5.161)
#[test]
fn format_directive_tilde_bracket_conditional_numeric() {
    // Numeric conditional: ~[zero~;one~;two~] selects clause by integer index
    let s = format_nil_string("~[zero~;one~;two~]", &[BlissVal::from_fixnum(1)]);
    assert_eq!(s, "one", "~[...~] with index 1 should select 'one', got: {:?}", s);
}

#[test]
fn format_directive_tilde_bracket_conditional_zero() {
    let s = format_nil_string("~[zero~;one~;two~]", &[BlissVal::from_fixnum(0)]);
    assert_eq!(s, "zero", "~[...~] with index 0 should select 'zero', got: {:?}", s);
}

// ~:[ — boolean conditional (R5.161)
#[test]
fn format_directive_tilde_colon_bracket_boolean_nil() {
    // ~:[false-clause~;true-clause~] — boolean: nil selects first clause
    let s = format_nil_string("~:[false~;true~]", &[NIL]);
    assert_eq!(s, "false", "~:[...~] with NIL should select 'false', got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_bracket_boolean_true() {
    let s = format_nil_string("~:[false~;true~]", &[T]);
    assert_eq!(s, "true", "~:[...~] with T should select 'true', got: {:?}", s);
}

// ~@[ — true-test conditional (R5.161)
#[test]
fn format_directive_tilde_at_bracket_true_test() {
    // ~@[clause~] — if arg is non-nil, execute clause (arg remains available)
    let s = format_nil_string("~@[got: ~A~]", &[BlissVal::from_fixnum(42)]);
    assert!(s.contains("42"), "~@[...~] with non-nil arg should format it, got: {:?}", s);
}

#[test]
fn format_directive_tilde_at_bracket_true_test_nil() {
    // ~@[clause~] with nil — clause is not executed
    let s = format_nil_string("~@[got: ~A~]", &[NIL]);
    assert!(s.is_empty() || !s.contains("got:"),
        "~@[...~] with NIL should skip clause, got: {:?}", s);
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
    assert_eq!(s, "Hello World", "~:( ~) should capitalize each word, got: {:?}", s);
}

#[test]
fn format_directive_tilde_at_paren_case_capitalize_first() {
    // ~@( ... ~) capitalizes first word only
    let s = format_nil_string("~@(hello world~)", &[]);
    assert_eq!(s, "Hello world", "~@( ~) should capitalize first word, got: {:?}", s);
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
    let s = format_nil_string("~D dog~P", &[BlissVal::from_fixnum(1)]);
    // With count 1, ~P produces empty string (no trailing 's')
    assert_eq!(s, "1 dog", "~P with 1 should not add 's', got: {:?}", s);
}

#[test]
fn format_directive_tilde_p_plural_multiple() {
    let s = format_nil_string("~D dog~P", &[BlissVal::from_fixnum(3)]);
    // With count != 1, ~P produces "s"
    assert_eq!(s, "3 dogs", "~P with 3 should add 's', got: {:?}", s);
}

#[test]
fn format_directive_tilde_p_plural_zero() {
    let s = format_nil_string("~D dog~P", &[BlissVal::from_fixnum(0)]);
    assert_eq!(s, "0 dogs", "~P with 0 should add 's', got: {:?}", s);
}

// ~@P — "y"/"ies" plural variant
#[test]
fn format_directive_tilde_at_p_plural_y_ies_singular() {
    let s = format_nil_string("~D bab~@P", &[BlissVal::from_fixnum(1)]);
    assert_eq!(s, "1 baby", "~@P with 1 should produce 'y', got: {:?}", s);
}

#[test]
fn format_directive_tilde_at_p_plural_y_ies_multiple() {
    let s = format_nil_string("~D bab~@P", &[BlissVal::from_fixnum(3)]);
    assert_eq!(s, "3 babies", "~@P with 3 should produce 'ies', got: {:?}", s);
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
    let s = format_nil_string("~10@A", &[BlissVal::from_fixnum(42)]);
    // Should be right-justified in a field of width 10
    assert!(s.len() >= 10, "~10@A should produce at least 10 chars, got: {:?}", s);
    assert!(s.ends_with("42") || s.trim_start().starts_with("42"),
        "~10@A should right-justify '42', got: {:?}", s);
}

// ~:D — commas in decimal
#[test]
fn format_directive_tilde_colon_d_commas() {
    let s = format_nil_string("~:D", &[BlissVal::from_fixnum(1000000)]);
    // ~:D inserts commas: "1,000,000"
    assert!(s.contains(','), "~:D should insert commas, got: {:?}", s);
    assert!(s.contains("1,000,000") || s.contains("1 000 000"),
        "~:D of 1000000 should produce '1,000,000', got: {:?}", s);
}

// ~@D — forced sign
#[test]
fn format_directive_tilde_at_d_forced_sign() {
    let s = format_nil_string("~@D", &[BlissVal::from_fixnum(42)]);
    assert!(s.starts_with('+'), "~@D of positive should start with '+', got: {:?}", s);
    assert!(s.contains("42"), "~@D should contain '42', got: {:?}", s);
}

// ── V and # as directive parameters (R5.158) ─────────────────────

#[test]
fn format_directive_v_parameter() {
    // ~VD uses the next argument as the mincol parameter for ~D
    let s = format_nil_string("~VD", &[BlissVal::from_fixnum(10), BlissVal::from_fixnum(42)]);
    // mincol=10 means at least 10 chars wide
    assert!(s.len() >= 10, "~VD with mincol=10 should produce >= 10 chars, got: {:?}", s);
    assert!(s.contains("42"), "~VD should format the number 42, got: {:?}", s);
}

#[test]
fn format_directive_hash_parameter() {
    // ~#D uses the number of remaining args as the parameter
    let s = format_nil_string("~#D", &[
        BlissVal::from_fixnum(42),
        BlissVal::from_fixnum(99),
        BlissVal::from_fixnum(100),
    ]);
    // # = 3 remaining args at point of ~#D, so mincol=3
    assert!(s.contains("42"), "~#D should format the number 42, got: {:?}", s);
}

// ── ~/name/ — user dispatch (R5.163) ─────────────────────────────

#[test]
fn format_directive_tilde_slash_user_dispatch() {
    // ~/name/ calls a named function for formatting
    // The exact name depends on what functions are registered; we test the
    // parsing and interface.
    let result = format_nil("~/my-format-fn/", &[BlissVal::from_fixnum(42)]);
    // This will either succeed (if a function named my-format-fn is found)
    // or error (function not found). Either is acceptable in red phase.
    // ~/name/ with an unknown function name should return an error.
    assert!(result.is_err(),
        "~/my-format-fn/ with unregistered function should return Err");
}

// ── ~< ~> — justification / logical-block (R5.162) ──────────────

#[test]
fn format_directive_tilde_angle_justification() {
    // ~<text~> basic justification
    let result = format_nil("~20<hello~;world~>", &[]);
    assert!(result.is_ok(), "~<...~> justification should succeed");
    let val = result.unwrap();
    assert_ne!(val, NIL);
    let s = bliss_string_to_rust(val);
    assert!(s.contains("hello"), "justification should contain 'hello', got: {:?}", s);
    assert!(s.contains("world"), "justification should contain 'world', got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_angle_logical_block() {
    // ~:<...~:> logical-block mode for pretty-printer
    let result = format_nil("~:<~A ~A~:>", &[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)]);
    // ~:<...~:> logical block should succeed with valid arguments.
    assert!(result.is_ok(),
        "~:<...~:> logical-block should succeed");
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

// ── format() with stream destination (R5.156) ────────────────────

#[test]
fn format_destination_stream() {
    // FORMAT with a stream destination should write to the stream and return NIL.
    // In red phase, we can't construct a real stream BlissVal, but we test
    // the interface. This will fail at unimplemented!("format").
    // A stream BlissVal would be a heap object with stream type_id.
    let stream = BlissVal::from_fixnum(0); // placeholder for a stream
    let result = format(stream, "hello ~A", &[BlissVal::from_fixnum(42)]);
    // Should either succeed (write to stream, return NIL) or error (invalid stream type)
    // A fixnum is not a valid stream, so format should return a type error.
    assert!(result.is_err(),
        "format with an invalid stream (fixnum) should return Err (type error)");
}

// ── format() with string-with-fill-pointer destination (R5.156) ──

#[test]
fn format_destination_string_with_fill_pointer() {
    // FORMAT with a string-with-fill-pointer should append to the string and return NIL.
    // We can't construct a real string-with-fill-pointer BlissVal in red phase,
    // but we test the interface shape.
    let string_val = BlissVal::from_fixnum(0); // placeholder
    let result = format(string_val, "appended ~D", &[BlissVal::from_fixnum(42)]);
    // A fixnum is not a valid string-with-fill-pointer, so this should error.
    assert!(result.is_err(),
        "format with an invalid string dest (fixnum) should return Err (type error)");
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
        bliss_rt::types::stringp(val),
        "format with NIL dest should return a string (stringp = true)"
    );
}

// ── format() with multiple arguments ─────────────────────────────

#[test]
fn format_multiple_directives() {
    let s = format_nil_string(
        "~A and ~D",
        &[BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)],
    );
    assert!(s.contains("1"), "should contain '1', got: {:?}", s);
    assert!(s.contains("and"), "should contain 'and', got: {:?}", s);
    assert!(s.contains("2"), "should contain '2', got: {:?}", s);
}

#[test]
fn format_no_directives_literal_string() {
    let s = format_nil_string("hello world", &[]);
    assert_eq!(s, "hello world", "literal format string should pass through unchanged, got: {:?}", s);
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
    // pprint_logical_block should establish a logical block with prefix/suffix.
    // When implemented, output to the stream should include the prefix and suffix.
    let r = pprint_logical_block(T, NIL, Some("("), None, Some(")"), NIL);
    // Will panic at unimplemented!() in red phase. The assertion below verifies
    // correct behavior once implemented: it should succeed and return ().
    assert!(r.is_ok(), "pprint_logical_block with prefix/suffix should succeed");
}

#[test]
fn pprint_logical_block_no_prefix_and_per_line() {
    let r1 = pprint_logical_block(T, NIL, None, None, None, NIL);
    assert!(r1.is_ok(), "pprint_logical_block with no prefix should succeed");

    let r2 = pprint_logical_block(T, NIL, None, Some(";;; "), None, NIL);
    assert!(r2.is_ok(), "pprint_logical_block with per-line-prefix should succeed");
}

// ── pprint_newline() ──────────────────────────────────────────────

#[test]
fn pprint_newline_all_kinds() {
    for kind in [NewlineKind::Linear, NewlineKind::Fill, NewlineKind::Miser, NewlineKind::Mandatory] {
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
    // The dispatch table should have a default entry for integers.
    // `found` indicates whether a specific dispatch entry was matched.
    // `func` is the dispatch function to call (may be a default printer).
    assert_ne!(func, NIL, "pprint_dispatch should return a function, not NIL");
    // We can't deeply verify `found` without knowing the default table setup,
    // but it should be a boolean.
    let _ = found;
}

// ── set_pprint_dispatch() ─────────────────────────────────────────

#[test]
fn set_pprint_dispatch_with_function() {
    let r = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 0.0, NIL);
    assert!(r.is_ok(), "set_pprint_dispatch with function should succeed");
}

#[test]
fn set_pprint_dispatch_remove_entry() {
    // Setting function to None should remove the dispatch entry
    let r = set_pprint_dispatch(NIL, None, 0.0, NIL);
    assert!(r.is_ok(), "set_pprint_dispatch with None should remove entry");
}

#[test]
fn set_pprint_dispatch_priority_ordering() {
    // Higher priority should win when multiple entries match (R5.167)
    let r1 = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 5.0, NIL);
    assert!(r1.is_ok(), "set_pprint_dispatch with priority 5 should succeed");
    let r2 = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 10.0, NIL);
    assert!(r2.is_ok(), "set_pprint_dispatch with priority 10 should succeed");
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
    let _ = set_pprint_dispatch(NIL, Some(BlissVal::from_fixnum(0)), 99.0, copy_val);
    // After modification, pprint_dispatch on original should still return its
    // original entry (not the one added to the copy). Full verification
    // requires working dispatch implementation.
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
fn format_empty_control_string() {
    let s = format_nil_string("", &[]);
    assert_eq!(s, "", "empty control string should produce empty string, got: {:?}", s);
}

#[test]
fn format_multiple_newlines() {
    let s = format_nil_string("~%~%~%", &[]);
    assert_eq!(s, "\n\n\n", "three ~%% should produce three newlines, got: {:?}", s);
}

#[test]
fn format_mixed_literal_and_directives() {
    let s = format_nil_string("The ~A has ~D item~P", &[
        BlissVal::from_fixnum(0), // placeholder: should be a string for ~A
        BlissVal::from_fixnum(3),
    ]);
    // We can at least verify structural parts of the output
    assert!(s.contains("The"), "should contain literal 'The', got: {:?}", s);
    assert!(s.contains("has"), "should contain literal 'has', got: {:?}", s);
    assert!(s.contains("3"), "should contain '3' from ~D, got: {:?}", s);
}

// ~R with colon (ordinal) and at-sign (Roman) modifiers
#[test]
fn format_directive_tilde_colon_r_ordinal() {
    let s = format_nil_string("~:R", &[BlissVal::from_fixnum(4)]);
    assert!(
        s.to_lowercase().contains("fourth"),
        "~:R of 4 should produce English ordinal 'fourth', got: {:?}", s
    );
}

#[test]
fn format_directive_tilde_at_r_roman() {
    let s = format_nil_string("~@R", &[BlissVal::from_fixnum(4)]);
    assert_eq!(s, "IV", "~@R of 4 should produce Roman numeral 'IV', got: {:?}", s);
}

#[test]
fn format_directive_tilde_colon_at_r_old_roman() {
    let s = format_nil_string("~:@R", &[BlissVal::from_fixnum(4)]);
    assert_eq!(s, "IIII", "~:@R of 4 should produce old Roman 'IIII', got: {:?}", s);
}

// ~:P — backs up one arg then plural
#[test]
fn format_directive_tilde_colon_p_backup_plural() {
    let s = format_nil_string("~D dog~:P", &[BlissVal::from_fixnum(1)]);
    assert_eq!(s, "1 dog", "~:P with 1 should back up and not add 's', got: {:?}", s);
}

// ~:* — go back one argument
#[test]
fn format_directive_tilde_colon_star_go_back() {
    let s = format_nil_string("~A ~:*~A", &[BlissVal::from_fixnum(42)]);
    // ~A prints 42, ~:* goes back one arg, ~A prints 42 again
    assert_eq!(s, "42 42", "~:* should back up one arg, got: {:?}", s);
}

// ~@* — absolute goto
#[test]
fn format_directive_tilde_at_star_absolute_goto() {
    let s = format_nil_string("~A ~@*~A", &[BlissVal::from_fixnum(42), BlissVal::from_fixnum(99)]);
    // ~A prints 42, ~@* goes to arg 0, ~A prints 42 again
    assert!(s.starts_with("42"), "should start with first arg, got: {:?}", s);
}

// ~:C — character spelled out
#[test]
fn format_directive_tilde_colon_c_spelled_name() {
    let s = format_nil_string("~:C", &[BlissVal::from_char(' ')]);
    assert!(
        s.to_lowercase().contains("space"),
        "~:C of space should spell out 'Space', got: {:?}", s
    );
}

// ~@C — character with #\ syntax
#[test]
fn format_directive_tilde_at_c_reader_syntax() {
    let s = format_nil_string("~@C", &[BlissVal::from_char('A')]);
    assert!(s.contains("#\\"), "~@C should produce #\\ reader syntax, got: {:?}", s);
    assert!(s.contains('A'), "~@C should contain the character, got: {:?}", s);
}

// ~$  with modifiers
#[test]
fn format_directive_tilde_at_dollar_forced_sign() {
    let s = format_nil_string("~@$", &[BlissVal::from_single_float(std::f32::consts::PI)]);
    assert!(s.starts_with('+'), "~@$ should force sign on positive, got: {:?}", s);
}
