// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Surrogate code points (#xD800-#xDFFF) are characters, as in SBCL
//! (bliss-5fn6m). Babel's UTF-8B decoder maps each undecodable octet to one of
//! #\UDC80-#\UDCFF, and its CODE-CHAR-NILNESS test requires CODE-CHAR to answer
//! a character for every code below CHAR-CODE-LIMIT. A Rust `char` cannot hold
//! a surrogate, so these programs exercise every place where EGCL keeps
//! characters in Rust: the expected values are SBCL's answers. Each program is
//! one form whose value `--eval` echoes on its last line of output, run in the
//! interpreter and with compilation forced.

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

/// The program's value in the interpreter, checked to agree under T1 and T2.
fn eval(program: &str) -> String {
    let interpreted = eval_in(None, program);
    for tier in ["t1", "t2"] {
        assert_eq!(
            eval_in(Some(tier), program),
            interpreted,
            "{tier}: {program}"
        );
    }
    interpreted
}

/// CODE-CHAR answers a character for every code below CHAR-CODE-LIMIT, and
/// CHAR-CODE inverts it (Babel CODE-CHAR-NILNESS).
#[test]
fn code_char_and_char_code_round_trip_over_the_whole_range() {
    assert_eq!(
        eval(
            "(list (loop for i below char-code-limit \
                         count (let ((c (code-char i))) (not (and c (= (char-code c) i))))) \
                   (characterp (code-char #xd800)) (char-code (code-char #xdfff)))"
        ),
        "(0 T 57343)"
    );
}

/// A surrogate is stored in, and read back from, every kind of string.
#[test]
fn surrogates_are_stored_in_strings() {
    assert_eq!(
        eval(
            "(let ((s (make-string 3 :initial-element (code-char #xdc80))) \
                   (a (make-array 2 :element-type 'character :adjustable t :fill-pointer 0))) \
               (setf (char s 1) (code-char #xd800)) \
               (vector-push-extend (code-char #xdbff) a) \
               (list (map 'list #'char-code s) \
                     (char-code (aref a 0)) \
                     (map 'list #'char-code (string (code-char #xdfff))) \
                     (map 'list #'char-code (concatenate 'string \"a\" s)) \
                     (map 'list #'char-code (reverse (subseq s 0 2))) \
                     (map 'list #'char-code (copy-seq s)) \
                     (length s)))"
        ),
        "((56448 55296 56448) 56319 (57343) (97 56448 55296 56448) (55296 56448) \
         (56448 55296 56448) 3)"
    );
}

/// COERCE builds strings from lists and vectors of surrogates and back.
#[test]
fn coerce_to_and_from_lists() {
    assert_eq!(
        eval(
            "(let* ((codes '(97 #xd800 #xdcf0 #xdfff)) \
                    (s (coerce (mapcar #'code-char codes) 'string)) \
                    (v (coerce (map 'vector #'code-char codes) 'string))) \
               (list (equal (mapcar #'char-code (coerce s 'list)) codes) \
                     (equal (map 'list #'char-code v) codes) \
                     (string= s v) (stringp s)))"
        ),
        "(T T T T)"
    );
}

/// Comparison sees the code: distinct surrogates are not equal, and they
/// order by code. Surrogates have no case.
#[test]
fn comparison_and_case() {
    assert_eq!(
        eval(
            "(let ((a (string (code-char #xd800))) (b (string (code-char #xd801)))) \
               (list (string= a b) (string= a (copy-seq a)) (string< a b) \
                     (equal a b) (equalp a b) (equalp a (copy-seq a)) \
                     (char= (code-char #xd800) (code-char #xd801)) \
                     (char< (code-char #xd800) (code-char #xd801)) \
                     (char-equal (code-char #xd800) (code-char #xd800)) \
                     (char-code (char-upcase (code-char #xd800))) \
                     (char-code (char-downcase (code-char #xdc00))) \
                     (both-case-p (code-char #xd800)) \
                     (alpha-char-p (code-char #xd800)) \
                     (map 'list #'char-code (string-upcase a)) \
                     (let ((h (make-hash-table :test 'equal))) \
                       (setf (gethash a h) 1 (gethash b h) 2) \
                       (list (gethash (copy-seq a) h) (gethash (copy-seq b) h)))))"
        ),
        "(NIL T 0 NIL NIL T NIL T T 55296 56320 NIL NIL (55296) (1 2))"
    );
}

/// PRIN1 names a surrogate (`#\UD800`, as SBCL does), and the name reads
/// back; PRINC, PRIN1 and WRITE-STRING keep a surrogate in a string stream.
#[test]
fn printing_and_reading() {
    assert_eq!(
        eval(
            "(let ((s (coerce (list #\\a (code-char #xdc80)) 'string))) \
               (list (prin1-to-string (code-char #xd800)) \
                     (char-name (code-char #xdcf0)) \
                     (char-code (read-from-string \"#\\\\UDCF0\")) \
                     (char-code (name-char \"U+D800\")) \
                     (map 'list #'char-code (princ-to-string (code-char #xd800))) \
                     (map 'list #'char-code (princ-to-string s)) \
                     (map 'list #'char-code (prin1-to-string s)) \
                     (map 'list #'char-code \
                          (with-output-to-string (o) (write-string s o) (write-char (code-char #xdfff) o))) \
                     (map 'list #'char-code \
                          (with-input-from-string (i s) (list (read-char i) (read-char i))))))"
        ),
        "(\"#\\\\UD800\" \"UDCF0\" 56560 55296 (55296) (97 56448) (34 97 56448 34) \
         (97 56448 57343) (97 56448))"
    );
}

/// `(format nil "~a" x)` and `~S` -- PRINC-TO-STRING and PRIN1-TO-STRING --
/// keep a surrogate. Other FORMAT output is assembled as Rust text, where a
/// surrogate is U+FFFD (a documented limit, bliss-5fn6m).
#[test]
fn format_of_a_lone_string_or_character() {
    assert_eq!(
        eval(
            "(let ((s (coerce (list #\\a (code-char #xdc80)) 'string))) \
               (list (map 'list #'char-code (format nil \"~a\" s)) \
                     (map 'list #'char-code (format nil \"~S\" s)) \
                     (map 'list #'char-code (format nil \"~A\" (code-char #xd800))) \
                     (format nil \"~s\" (code-char #xd800)) \
                     (map 'list #'char-code \
                          (with-output-to-string (o) (format o \"~a\" s)))))"
        ),
        "((97 56448) (34 97 56448 34) (55296) \"#\\\\UD800\" (97 56448))"
    );
}

/// Terminal output is UTF-8, which cannot encode a surrogate: it is written as
/// U+FFFD, as SBCL writes it to its terminal.
#[test]
fn utf8_output_writes_a_surrogate_as_the_replacement_character() {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            "(progn (write-string (coerce (list #\\a (code-char #xd800) #\\b) 'string)) \
                    (write-char (code-char #xdfff)) (finish-output) (values))",
        ])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.starts_with("a\u{FFFD}b\u{FFFD}".as_bytes()),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
