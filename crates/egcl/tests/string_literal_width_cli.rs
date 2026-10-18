// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! R4.94: the element type of a string literal, and the readtable's base-char
//! preference that chooses it, as in SBCL. Each program is one form whose value
//! `--eval` echoes on its last line of output.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `--eval` echoes the value on the last line, after anything LOAD printed.
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Per R4.94, the standard readtable reads every string literal as a CHARACTER
/// string, so copying one gives a string that can hold any character.
#[test]
fn a_literal_is_a_character_string() {
    assert_eq!(
        eval(
            "(list (typep \"abc\" 'base-string) (typep \"abc\" 'simple-string) \
                   (array-element-type \"abc\"))"
        ),
        "(NIL T CHARACTER)"
    );
    assert_eq!(
        eval("(let ((s (copy-seq \"abc\"))) (setf (char s 0) (code-char 955)) s)"),
        "\"λbc\""
    );
    assert_eq!(
        eval("(substitute (code-char 8212) #\\- \"a-b\")"),
        "\"a—b\""
    );
}

/// Per R4.94, the standard readtable's preference is :SYMBOLS, and
/// COPY-READTABLE carries a readtable's preference to the copy.
#[test]
fn the_standard_preference_is_symbols_and_copies_carry_it() {
    assert_eq!(
        eval(
            "(list (egcl-ext:readtable-base-char-preference *readtable*) \
                   (egcl-ext:readtable-base-char-preference (copy-readtable nil)))"
        ),
        "(:SYMBOLS :SYMBOLS)"
    );
    assert_eq!(
        eval(
            "(let ((rt (copy-readtable nil))) \
               (setf (egcl-ext:readtable-base-char-preference rt) :both) \
               (list (egcl-ext:readtable-base-char-preference (copy-readtable rt)) \
                     (egcl-ext:readtable-base-char-preference \
                       (copy-readtable rt (copy-readtable nil))) \
                     (egcl-ext:readtable-base-char-preference (copy-readtable nil rt))))"
        ),
        "(:BOTH :BOTH :SYMBOLS)"
    );
}

/// Per R4.94, a readtable preferring base strings for literals (:STRINGS or
/// :BOTH) reads an all-BASE-CHAR literal as an 8-bit SIMPLE-BASE-STRING; a
/// literal holding a wider character is still a CHARACTER string, and :SYMBOLS
/// or NIL keeps every literal a CHARACTER string.
#[test]
fn a_base_string_preference_reads_compact_literals() {
    let read_under = |preference: &str, literal: &str| {
        eval(&format!(
            "(let ((*readtable* (copy-readtable nil))) \
               (setf (egcl-ext:readtable-base-char-preference *readtable*) {preference}) \
               (let ((s (read-from-string {literal}))) \
                 (list (typep s 'simple-base-string) (length s))))"
        ))
    };
    let ascii = "\"\\\"abc\\\"\"";
    let wide = "(format nil \"\\\"a~cb\\\"\" (code-char 955))";
    assert_eq!(read_under(":both", ascii), "(T 3)");
    assert_eq!(read_under(":strings", ascii), "(T 3)");
    assert_eq!(read_under(":both", wide), "(NIL 3)");
    assert_eq!(read_under(":symbols", ascii), "(NIL 3)");
    assert_eq!(read_under("nil", ascii), "(NIL 3)");
}

#[test]
fn an_invalid_preference_or_readtable_is_a_type_error() {
    assert_eq!(
        eval(
            "(list (handler-case (setf (egcl-ext:readtable-base-char-preference \
                                         (copy-readtable nil)) :bogus) \
                     (type-error () :type-error)) \
                   (handler-case (egcl-ext:readtable-base-char-preference 42) \
                     (type-error () :type-error)))"
        ),
        "(:TYPE-ERROR :TYPE-ERROR)"
    );
}

/// Per R4.94, a literal is the same kind of string whether its file is loaded
/// as source or compiled first. Compiled constants were already CHARACTER
/// strings while source literals were base strings, so a program's strings
/// changed type when its file was compiled.
#[test]
fn source_and_compiled_literals_agree() {
    let dir = std::env::temp_dir().join(format!("egcl-literal-width-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("literal.lisp");
    std::fs::write(&source, "(defun literal-text () \"abc\")\n").unwrap();
    let path = source.display();
    let program = format!(
        "(list (progn (load \"{path}\") (typep (literal-text) 'base-string)) \
               (progn (load (compile-file \"{path}\")) (typep (literal-text) 'base-string)) \
               (let ((s (copy-seq (literal-text)))) \
                 (setf (char s 0) (code-char 955)) \
                 (length s)))"
    );
    let result = eval(&program);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(result, "(NIL NIL 3)");
}
