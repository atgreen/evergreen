// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// MAKE-ARRAY builds a plain vector natively when its only options are
/// :ELEMENT-TYPE and :INITIAL-ELEMENT and the type upgrades to T. Every result
/// here is what the full option parsing gave before: the defaults (NIL for T,
/// 0 for integer types), option order and duplicates, and the requests that
/// keep the full path (bit, character, double-float, :INITIAL-CONTENTS,
/// :FILL-POINTER, a dimension list, a negative length, an unknown option).
/// :INITIAL-CONTENTS from a list or vector is copied natively too; the wrong
/// length, an improper list, or contents with an element take the full path.
#[test]
fn make_array_fast_path_matches_the_full_option_parsing() {
    let program = r#"
(defun describe-array (a)
  (list (type-of a) (length a) (coerce a 'list)))
(format t "~&RESULT:~S~%"
  (list (describe-array (make-array 3))
        (describe-array (make-array 3 :element-type t))
        (describe-array (make-array 3 :element-type '(unsigned-byte 8)))
        (describe-array (make-array 3 :element-type '(signed-byte 32) :initial-element -1))
        (describe-array (make-array 3 :element-type 'fixnum))
        (describe-array (make-array 3 :element-type 'integer))
        (describe-array (make-array 3 :initial-element :x :element-type '(unsigned-byte 16)))
        (describe-array (make-array 3 :initial-element 1 :initial-element 2))
        (describe-array (make-array 3 :element-type '(unsigned-byte 1)))
        (describe-array (make-array 3 :element-type 'bit))
        (describe-array (make-array 3 :element-type 'character :initial-element #\a))
        (describe-array (make-array 3 :element-type 'double-float))
        (describe-array (make-array '(3) :initial-element 7))
        (describe-array (make-array 3 :initial-contents '(a b c)))
        (array-has-fill-pointer-p (make-array 3 :fill-pointer 1))
        (array-dimensions (make-array '(2 3) :initial-element 0))
        (describe-array (make-array 0))
        (handler-case (make-array -1) (error () :negative))
        (handler-case (make-array 3 :bogus 1) (error () :bad-key))))

(defun d2 (a) (list (type-of a) (length a) (coerce a 'list)))
(format t "~&CONTENTS:~S~%"
  (list (d2 (make-array 3 :initial-contents '(a b c)))
        (d2 (make-array 3 :element-type '(unsigned-byte 8) :initial-contents #(1 2 3)))
        (d2 (make-array 2 :initial-contents "xy"))
        (d2 (make-array 0 :initial-contents nil))
        (d2 (make-array 2 :initial-contents (make-array 2 :element-type 'double-float :initial-element 1d0)))
        (handler-case (make-array 3 :initial-contents '(a b)) (error () :short))
        (handler-case (make-array 2 :initial-contents '(a b . c)) (error () :improper))
        (handler-case (d2 (make-array 2 :initial-contents '(1 2) :initial-element 0)) (error () :both))
        (d2 (make-array 3 :element-type 'character :initial-contents "abc"))
        (d2 (make-array 4 :element-type 'bit :initial-contents '(1 0 1 1)))))    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(
            r#"RESULT:(((SIMPLE-VECTOR 3) 3 (NIL NIL NIL)) ((SIMPLE-VECTOR 3) 3 (NIL NIL NIL)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (-1 -1 -1)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-VECTOR 3) 3 (:X :X :X)) ((SIMPLE-VECTOR 3) 3 (1 1 1)) ((SIMPLE-VECTOR 3) 3 (0 0 0)) ((SIMPLE-BIT-VECTOR 3) 3 (0 0 0)) ((SIMPLE-ARRAY CHARACTER (3)) 3 (#\a #\a #\a)) ((SIMPLE-ARRAY DOUBLE-FLOAT (3)) 3 (0.0d0 0.0d0 0.0d0)) ((SIMPLE-VECTOR 3) 3 (7 7 7)) ((SIMPLE-VECTOR 3) 3 (A B C)) T (2 3) ((SIMPLE-VECTOR 0) 0 NIL) :NEGATIVE #(NIL NIL NIL))"#
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            r#"CONTENTS:(((SIMPLE-VECTOR 3) 3 (A B C)) ((SIMPLE-VECTOR 3) 3 (1 2 3)) ((SIMPLE-VECTOR 2) 2 (#\x #\y)) ((SIMPLE-VECTOR 0) 0 NIL) ((SIMPLE-VECTOR 2) 2 (1.0d0 1.0d0)) :SHORT :IMPROPER ((SIMPLE-VECTOR 2) 2 (1 2)) ((SIMPLE-ARRAY CHARACTER (3)) 3 (#\a #\b #\c)) ((SIMPLE-BIT-VECTOR 4) 4 (1 0 1 1)))"#
        ),
        "{stdout}"
    );
}
