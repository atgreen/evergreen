// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! BASE-CHAR, STANDARD-CHAR and EXTENDED-CHAR as proper subtypes of CHARACTER
//! at the doors that convert or check characters, and MAKE-ARRAY's
//! :INITIAL-CONTENTS length, compared with SBCL's answers. Each program is one
//! form whose value `--eval` echoes on its last line of output.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval_in(tier: Option<&str>, program: &str) -> String {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let out = command.output().expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn eval(program: &str) -> String {
    eval_in(None, program)
}

/// COERCE never changes a character's code, so a character of another
/// subtype is a TYPE-ERROR rather than being returned unchanged.
#[test]
fn coerce_to_a_character_subtype_checks_the_subtype() {
    assert_eq!(
        eval(
            "(flet ((try (x type) (handler-case (coerce x type) (type-error () :type-error)))) \
               (list (try (code-char 955) 'base-char) \
                     (try #\\a 'base-char) \
                     (try \"a\" 'base-char) \
                     (try (code-char 200) 'standard-char) \
                     (try #\\a 'standard-char) \
                     (try #\\a 'extended-char) \
                     (try (code-char 955) 'extended-char)))"
        ),
        "(:TYPE-ERROR #\\a #\\a :TYPE-ERROR #\\a :TYPE-ERROR #\\λ)"
    );
}

/// An element that is not a character (or not a BASE-CHAR) is a TYPE-ERROR;
/// COERCE used to drop it, so (coerce '(1 #\a) 'string) was "a".
#[test]
fn coerce_to_a_string_rejects_non_characters() {
    assert_eq!(
        eval(
            "(flet ((try (x type) (handler-case (coerce x type) (type-error () :type-error)))) \
               (list (try '(1 #\\a) 'string) \
                     (try '(1 #\\a) 'base-string) \
                     (try (list (code-char 955)) 'base-string) \
                     (try '(#\\a #\\b) 'string)))"
        ),
        "(:TYPE-ERROR :TYPE-ERROR :TYPE-ERROR \"ab\")"
    );
}

/// The interpreter's quick check for THE no longer treats BASE-CHAR and
/// STANDARD-CHAR as any character, nor SIMPLE-BASE-STRING as any string.
#[test]
fn the_checks_character_and_base_string_subtypes() {
    let program = "(flet ((try (thunk) (handler-case (funcall thunk) (type-error () :type-error)))) \
         (list (try (lambda () (the base-char (code-char 955)))) \
               (try (lambda () (the base-char #\\a))) \
               (try (lambda () (the standard-char (code-char 200)))) \
               (try (lambda () (the simple-base-string (make-string 2)))) \
               (try (lambda () (length (the simple-base-string \
                                         (make-string 2 :element-type 'base-char)))))))";
    assert_eq!(
        eval_in(Some("interp"), program),
        "(:TYPE-ERROR #\\a :TYPE-ERROR :TYPE-ERROR 2)"
    );
}

/// EXTENDED-CHAR upgrades to CHARACTER, so such an array is a string.
#[test]
fn an_extended_char_array_is_a_character_string() {
    assert_eq!(
        eval(
            "(let ((a (make-array 2 :element-type 'extended-char :initial-element (code-char 955)))) \
               (list (array-element-type a) (stringp a) \
                     (upgraded-array-element-type 'extended-char)))"
        ),
        "(CHARACTER T CHARACTER)"
    );
}

/// A vector's :INITIAL-CONTENTS must have exactly as many elements as the
/// vector, on every MAKE-ARRAY path; a short one used to be padded.
#[test]
fn initial_contents_must_match_the_vector_length() {
    assert_eq!(
        eval(
            "(flet ((try (thunk) (handler-case (length (funcall thunk)) (error () :error)))) \
               (list (try (lambda () (make-array 3 :element-type 'base-char :initial-contents \"ab\"))) \
                     (try (lambda () (make-array 3 :element-type 'character :initial-contents \"abcd\"))) \
                     (try (lambda () (make-array 3 :initial-contents '(1 2)))) \
                     (try (lambda () (make-array 3 :fill-pointer t :initial-contents '(1 2)))) \
                     (try (lambda () (make-array 3 :adjustable t :initial-contents '(1 2 3 4)))) \
                     (try (lambda () (make-array 3 :element-type 'base-char :initial-contents \"abc\"))) \
                     (try (lambda () (make-array '(3) :initial-contents '(1 2 3))))))"
        ),
        "(:ERROR :ERROR :ERROR :ERROR :ERROR 3 3)"
    );
}
