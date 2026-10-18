// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! BASE-CHAR is the element type of an 8-bit base string, consistently.
//!
//! The runtime stores two string widths (spec §1.6.3): an 8-bit
//! SIMPLE-BASE-STRING for code points below 256 and a 32-bit CHARACTER string
//! for anything, and TYPEP tells a BASE-STRING by that width (bliss-ajb3).
//! Three other answers still followed the older "egcl has no extended
//! characters" idea: TYPEP called every character a BASE-CHAR and
//! EXTENDED-CHAR empty, UPGRADED-ARRAY-ELEMENT-TYPE upgraded BASE-CHAR to
//! CHARACTER, and TYPE-OF called every string SIMPLE-BASE-STRING -- while
//! MAKE-STRING ignored :element-type and MAP, COERCE and MAKE-SEQUENCE built
//! 32-bit strings for BASE-STRING. So `(make-array 5 :element-type
//! 'base-char)` and `(map 'base-string ...)` answered strings TYPEP refused as
//! base strings (ansi sequences LENGTH.STRING.6, MAP.SPECIALIZED-STRING.3), and
//! `(typep x (type-of x))` was NIL for a MAKE-STRING result.
//!
//! Every probe runs in both evaluator tiers: as a direct top-level form and
//! inside a DEFUN (memory `acceptance-eval-wrapper-hides-compiled-path`).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defmacro report (label form) `(format t "~a ~s~%" ,label ,form))
  ;; What TYPEP, ARRAY-ELEMENT-TYPE and TYPE-OF say about one string.
  (defun kinds (x)
    (list (typep x 'base-string) (typep x 'simple-base-string) (typep x 'string)
          (array-element-type x) (type-of x)))

  ;; Tier 1: direct forms.
  (report "MA-BASE" (kinds (make-array 5 :element-type 'base-char :initial-element #\a)))
  (report "MA-STD" (kinds (make-array 2 :element-type 'standard-char :initial-element #\a)))
  (report "MA-CHAR" (kinds (make-array 5 :element-type 'character :initial-element #\a)))
  (report "MS-DEFAULT" (kinds (make-string 3 :initial-element #\a)))
  (report "MS-BASE" (kinds (make-string 3 :element-type 'base-char :initial-element #\a)))
  (report "MAP-BASE" (kinds (map 'base-string #'identity '(#\a #\b #\c))))
  (report "MAP-STRING" (kinds (map 'string #'identity '(#\a #\b #\c))))
  (report "CONCAT-BASE" (kinds (concatenate 'simple-base-string "ab" '(#\c))))
  (report "COERCE-BASE" (kinds (coerce '(#\a #\b) 'base-string)))
  (report "COERCE-VBASE" (kinds (coerce '(#\a #\b) '(vector base-char))))
  (report "MAKE-SEQ-BASE" (kinds (make-sequence 'simple-base-string 2 :initial-element #\z)))
  (report "MA-IC-BASE" (kinds (make-array 5 :element-type 'base-char :initial-contents "abcde")))
  (report "LITERAL" (kinds "abc"))
  (report "THE" (length (the (simple-base-string 5)
                           (make-array 5 :element-type 'base-char :initial-element #\a))))
  (report "MA-BASE-MUTABLE"
          (let ((s (make-array 3 :element-type 'base-char :initial-element #\a)))
            (setf (aref s 1) #\b)
            s))
  (report "MA-IC-FRESH"
          (let* ((src "abcde")
                 (v (make-array 5 :element-type 'base-char :initial-contents src)))
            (eq src v)))
  ;; A base string never promotes: a wide character is a TYPE-ERROR at every
  ;; door, not a silent widening.
  (report "WIDE-INTO-BASE"
          (handler-case (let ((s (make-string 2 :element-type 'base-char)))
                          (setf (char s 0) (code-char 955))
                          :stored)
            (type-error () :type-error)))
  (report "MAP-WIDE"
          (handler-case (map 'base-string #'identity (list (code-char 955)))
            (type-error () :type-error)))
  (report "COERCE-WIDE"
          (handler-case (coerce (list (code-char 955)) 'base-string)
            (type-error () :type-error)))
  (report "MS-WIDE-FILL"
          (handler-case (make-string 2 :element-type 'base-char :initial-element (code-char 955))
            (type-error () :type-error)))
  ;; The character types the storage is named after.
  (report "CHARS" (list (typep #\a 'base-char) (typep #\a 'extended-char)
                        (typep (code-char 255) 'base-char) (typep (code-char 256) 'base-char)
                        (typep (code-char 955) 'extended-char)))
  (report "SUBTYPEP" (list (multiple-value-list (subtypep 'base-char 'character))
                           (multiple-value-list (subtypep 'character 'base-char))
                           (multiple-value-list (subtypep 'extended-char 'character))
                           (multiple-value-list (subtypep 'extended-char 'base-char))
                           (multiple-value-list (subtypep 'standard-char 'base-char))))
  (report "UAET" (list (upgraded-array-element-type 'base-char)
                       (upgraded-array-element-type 'standard-char)
                       (upgraded-array-element-type 'character)))
  (report "TYPE-OF-ROUNDTRIP" (let ((s (make-string 3))) (typep s (type-of s))))
  ;; A string displaced onto a base string reads and writes 8-bit storage, so
  ;; it IS a base string (not a simple one) and refuses a wide character; an
  ;; adjustable string's own store is 32-bit, so it stays CHARACTER.
  (report "DISPLACED-BASE"
          (let* ((b (make-array 4 :element-type 'base-char :initial-element #\a))
                 (d (make-array 2 :element-type 'base-char :displaced-to b
                                  :displaced-index-offset 1)))
            (list (array-element-type d) (typep d 'base-string) (typep d 'simple-base-string)
                  (handler-case (progn (setf (char d 0) (code-char 955)) :stored)
                    (type-error () :type-error))
                  ;; and its copies are base strings too
                  (array-element-type (copy-seq d)) (array-element-type (reverse d)))))
  (report "ADJUSTABLE-BASE"
          (let ((a (make-array 3 :element-type 'base-char :adjustable t :initial-element #\a)))
            (list (array-element-type a) (typep a 'base-string)
                  (handler-case (progn (setf (char a 0) (code-char 955)) :stored)
                    (type-error () :type-error)))))
  ;; The copying sequence functions keep a string's width (ansi COPY-SEQ.20,
  ;; REVERSE-STRING.5 and the REMOVE family's string cases compare
  ;; ARRAY-ELEMENT-TYPE of result and input).
  (report "COPIES-KEEP-WIDTH"
          (let ((b (make-string 3 :element-type 'base-char :initial-element #\a))
                (c (make-string 3 :initial-element #\a)))
            (list (array-element-type (copy-seq b)) (array-element-type (copy-seq c))
                  (array-element-type (subseq b 1)) (array-element-type (reverse b))
                  (array-element-type (remove #\a b :count 1))
                  (array-element-type (remove-if #'alpha-char-p b))
                  (array-element-type (remove-duplicates b))
                  (array-element-type (substitute #\b #\a b))
                  (array-element-type (subseq c 1)) (array-element-type (remove #\a c :count 1)))))

  ;; Tier 2: the same questions from inside a compiled function.
  (defun compiled-probe ()
    (list (kinds (make-array 5 :element-type 'base-char :initial-element #\a))
          (kinds (map 'base-string #'identity '(#\a #\b #\c)))
          (kinds (make-string 3))
          (typep (code-char 955) 'base-char)
          (length (the (simple-base-string 5)
                       (make-array 5 :element-type 'base-char :initial-element #\a)))))
  (report "COMPILED" (compiled-probe))
  (format t "BASE-STRING-OK~%")
"#;

fn run(args: &[&str]) -> (String, String, bool) {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", BIN])
        .args(args)
        .output()
        .expect("run egcl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn base_char_is_the_element_type_of_an_8_bit_base_string_consistently() {
    let (stdout, stderr, ok) = run(&["--no-init", "--eval", PROGRAM]);
    assert!(ok, "{stdout}\n{stderr}");

    const BASE: &str = "(T T T BASE-CHAR SIMPLE-BASE-STRING)";
    for expected in [
        // A BASE-CHAR request builds the 8-bit string at every constructor.
        &format!("MA-BASE {BASE}"),
        &format!("MA-STD {BASE}"),
        &format!("MS-BASE {BASE}"),
        &format!("MAP-BASE {BASE}"),
        &format!("CONCAT-BASE {BASE}"),
        &format!("COERCE-BASE {BASE}"),
        &format!("COERCE-VBASE {BASE}"),
        &format!("MAKE-SEQ-BASE {BASE}"),
        &format!("MA-IC-BASE {BASE}"),
        // CHARACTER, and the default, is the 32-bit string, and TYPE-OF says
        // so instead of calling it SIMPLE-BASE-STRING. A literal is one too,
        // as with SBCL's standard readtable (R4.94).
        "LITERAL (NIL NIL T CHARACTER (SIMPLE-ARRAY CHARACTER (3)))",
        "MA-CHAR (NIL NIL T CHARACTER (SIMPLE-ARRAY CHARACTER (5)))",
        "MS-DEFAULT (NIL NIL T CHARACTER (SIMPLE-ARRAY CHARACTER (3)))",
        "MAP-STRING (NIL NIL T CHARACTER (SIMPLE-ARRAY CHARACTER (3)))",
        "TYPE-OF-ROUNDTRIP T",
        // Element type follows the storage the string reads and writes.
        "DISPLACED-BASE (BASE-CHAR T NIL :TYPE-ERROR BASE-CHAR BASE-CHAR)",
        "ADJUSTABLE-BASE (CHARACTER NIL :STORED)",
        // The copying sequence functions keep the width both ways.
        "COPIES-KEEP-WIDTH (BASE-CHAR CHARACTER BASE-CHAR BASE-CHAR BASE-CHAR BASE-CHAR BASE-CHAR BASE-CHAR CHARACTER CHARACTER)",
        // ansi LENGTH.STRING.6's shape, and the 8-bit result is mutable.
        "THE 5",
        "MA-BASE-MUTABLE \"aba\"",
        "MA-IC-FRESH NIL",
        // No promotion.
        "WIDE-INTO-BASE :TYPE-ERROR",
        "MAP-WIDE :TYPE-ERROR",
        "COERCE-WIDE :TYPE-ERROR",
        "MS-WIDE-FILL :TYPE-ERROR",
        // BASE-CHAR is the code points below 256; EXTENDED-CHAR the rest.
        "CHARS (T NIL T NIL T)",
        "SUBTYPEP ((T T) (NIL T) (T T) (NIL T) (T T))",
        "UAET (BASE-CHAR BASE-CHAR CHARACTER)",
        // Tier 2 agrees with tier 1.
        &format!("COMPILED ({BASE} {BASE} (NIL NIL T CHARACTER (SIMPLE-ARRAY CHARACTER (3))) NIL 5)"),
        "BASE-STRING-OK",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}
