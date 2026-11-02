// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled `CHAR` and `SCHAR` read a large string correctly (bliss-5fn6m).
//! A string of 2^17 or more CHARACTERs is a large object, whose header is 16
//! bytes rather than 8. T2's inline character read assumed the small layout,
//! so it compared the index with the size word and read 8 bytes early: the
//! first character of a 150,000-character string came back as #\U249F0 (its
//! length), and Babel's UTF-8 round trip of every code point failed.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const FORMS: [&str; 5] = [
    "(defun codes (s) (let ((acc '())) (dotimes (i 4) (push (char-code (schar s i)) acc)) \
       (push (char-code (char s (1- (length s)))) acc) (nreverse acc)))",
    "(defun total (s) (let ((sum 0)) (dotimes (i (length s) sum) (incf sum (char-code (char s i))))))",
    "(defun fill-string (s) (dotimes (i (length s) s) (setf (char s i) (code-char (+ 256 (mod i 1000))))))",
    "(defun code-at (s i) (char-code (schar s i)))",
    "(let ((s (fill-string (make-string 150000)))) (dotimes (k 20) (codes s) (code-at s 0)) (list (codes s) (total s)))",
];

fn run(tier: Option<&str>) -> String {
    let mut command = Command::new(BIN);
    command.arg("--no-init");
    for form in FORMS {
        command.args(["--eval", form]);
    }
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let out = command.output().expect("spawn egcl");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn compiled_char_reads_a_large_character_string() {
    let expected = "((256 257 258 259 1255) 113325000)";
    for tier in [Some("t0"), None, Some("t1"), Some("t2")] {
        assert_eq!(run(tier), expected, "tier {tier:?}");
    }
}

#[test]
#[cfg(target_arch = "x86_64")]
fn large_base_and_character_strings_use_native_accessors() {
    let program = r#"
      (defun large-char (s i) (char-code (char s i)))
      (defun large-schar (s i) (char-code (schar s i)))
      (defun large-elt (s i) (char-code (elt s i)))
      (dolist (s (list (make-string 600000 :element-type 'base-char :initial-element #\a)
                      (make-string 150000 :initial-element #\λ)))
        (let ((expected (if (= (length s) 600000) 97 955)))
         (dolist (fn '(large-char large-schar large-elt))
          (dotimes (i 30) (funcall fn s 0))
          (assert (= (egcl-ext:function-tier fn) 2))
          (assert (= (funcall fn s 0) expected))
          (assert (= (funcall fn s (1- (length s))) expected)))))
      (write-line "LARGE-STRING-NATIVE-OK")
    "#;
    for inline in ["1", "0"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_INLINE_STRING_CHAR", inline)
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "0")
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("spawn egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("LARGE-STRING-NATIVE-OK"), "{stdout}");
    }
}
