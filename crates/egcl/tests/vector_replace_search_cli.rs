// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// REPLACE between simple vectors and simple strings is copied natively. The
/// cases cover overlap within one vector in both directions, mixed vector and
/// string arguments, and the shapes that keep the Lisp path (lists, a
/// fill-pointer string).
#[test]
fn replace_copies_simple_vectors_and_strings() {
    let stdout = eval(
        r#"
      (let ((up (vector 'a 'b 'c 'd 'e 'f))
            (down (vector 'a 'b 'c 'd 'e 'f))
            (octets (make-array 6 :element-type '(unsigned-byte 8)
                                  :initial-contents '(1 2 3 4 5 6)))
            (s (copy-seq "abcdef"))
            (fp (make-array 6 :element-type 'character :fill-pointer 4
                              :initial-contents "wxyz..")))
        (replace up up :start1 1 :end1 4 :start2 0 :end2 3)
        (replace down down :start1 0 :start2 2)
        (replace octets #(9 8) :start1 4)
        (replace s (vector #\X #\Y) :start1 1)
        (format t "RESULT:~S ~S ~S ~S ~S ~S ~S~%"
                up down octets s
                (replace (list 1 2 3) #(7 8))
                (replace fp "AB" :start1 1)
                (replace (copy-seq "hello") "J" :end2 0)))
    "#,
    );
    assert!(
        stdout.contains(
            r#"RESULT:#(A A B C E F) #(C D E F E F) #(1 2 3 4 9 8) "aXYdef" (7 8 3) "wABz" "hello""#
        ),
        "{stdout}"
    );
}

/// SEARCH with the EQL test and no key scans simple vectors and strings
/// natively; other tests, keys and sequence shapes keep the Lisp path.
#[test]
fn search_scans_simple_vectors_and_strings() {
    let stdout = eval(
        r#"
      (let ((text "--BEGIN-- x --END-- y --END--"))
        (format t "RESULT:~S~%"
                (list (search "--END--" text)
                      (search "--END--" text :from-end t)
                      (search "--END--" text :start2 13)
                      (search "--END--" text :end2 20)
                      (search "missing" text)
                      (search "" text :start2 3)
                      (search "" text :from-end t :end2 5)
                      (search #(2 3) #(1 2 3 2 3))
                      (search #(2 3) #(1 2 3 2 3) :from-end t)
                      (search "end" text :test #'char-equal)
                      (search "END" text :test #'char=)
                      (search '(3) #(1 2 3))
                      (search #(1.0) #(1 1.0))
                      (search "b" (make-array 3 :element-type 'character
                                                :fill-pointer 2
                                                :initial-contents "abb")))))
    "#,
    );
    assert!(
        stdout.contains("RESULT:(12 22 22 12 NIL 3 5 1 3 14 14 2 1 1)"),
        "{stdout}"
    );
}

/// FILL stores natively into simple vectors and simple strings; a list and a
/// fill-pointer string keep the Lisp path, and a character into an octet
/// vector is still an error.
#[test]
fn fill_stores_into_simple_vectors_and_strings() {
    let stdout = eval(
        r#"
      (let ((octets (make-array 6 :element-type '(unsigned-byte 8) :initial-element 1))
            (s (copy-seq "abcdef"))
            (v (vector 1 2 3 4))
            (fp (make-array 5 :element-type 'character :fill-pointer 3
                              :initial-contents "vwxyz")))
        (format t "RESULT:~S ~S ~S ~S ~S ~S~%"
                (fill octets 0 :start 2 :end 5)
                (fill s #\z :start 4)
                (fill v :x)
                (fill (list 1 2 3) 9 :end 2)
                (fill fp #\- )
                (handler-case (fill (copy-seq "abc") 7) (type-error () :type-error))))
    "#,
    );
    assert!(
        stdout
            .contains(r#"RESULT:#(1 1 0 0 0 1) "abcdzz" #(:X :X :X :X) (9 9 3) "---" :TYPE-ERROR"#),
        "{stdout}"
    );
}
