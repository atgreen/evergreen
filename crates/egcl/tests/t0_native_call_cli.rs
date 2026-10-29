// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A bytecode caller calling T2 callees (GitHub #487). Each scenario runs a
//! caller that executes once, so it stays bytecode, against callees that are
//! promoted to T2 first, and must print exactly what the interpreter prints.

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const PROGRAM: &str = r#"
(defvar *k* 10)
(defun leaf (a b) (+ a b))
(defun mv-leaf (a b) (values (+ a b) (- a b)))
(defun thrower (x) (if (> x 5) (throw :t0-native (* x 100)) x))
(defun car-leaf (x) (car x))
(defun special-leaf (x) (+ x *k*))
(defun special-binder (x) (let ((*k* x)) (special-leaf 1)))
(defun cons-leaf (a) (list a a))
;; Lean root entries reached through builtins and mapped (&key) callers.
(defun elt-leaf (s i) (elt s i))
(defun kw-char (s &key (start 0)) (char s start))
(defun kw-add (a &key (b 1)) (leaf a b))
(defun kw-car (x &key) (car-leaf x))
(defun kw-throw (x &key) (thrower x))
(defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
;; Inline CHAR/SCHAR/ELT on simple strings; everything else calls the real one.
(defun str-char (s i) (char s i))
(defun str-schar (s i) (schar s i))
(defun str-elt (s i) (elt s i))
(defun char-after-values (s i) (values 1 2) (char s i))
(defun char-mvl (s i) (multiple-value-list (char s i)))
;; FLOOR leaves two values pending when the inline read runs.
(defun char-floor (s n) (char s (floor n 2)))
(defun char-sum (s i j) (+ (char-code (char s i)) j))
;; THROW (in THROWER) unwinds through a T2 body with an inline CHAR.
(defun char-throw (s i) (char s (thrower i)))
(defun kw-elt (s &key (start 0)) (elt s start))
(defun rleaf (a b) (+ a b))
(defun warm ()
  (dotimes (i 40)
    (leaf i 1) (mv-leaf i 1) (thrower 1) (car-leaf '(1)) (special-leaf i)
    (special-binder i) (cons-leaf i) (elt-leaf "abc" 1) (kw-char "abc" :start 1)
    (kw-add i :b 1) (kw-car '(1)) (kw-throw 1) (fib 3) (rleaf i 1)
    (str-char "abc" 1) (str-schar "abc" 1) (str-elt "abc" 1) (char-after-values "abc" 1)
    (char-mvl "abc" 1) (char-floor "abc" 3) (char-sum "abc" 1 i) (char-throw "abcd" 1)
    (kw-elt "abc" :start 1)))
(warm)
(defun caller ()
  (let ((sum 0) (mvs nil) (errors 0) (conses 0))
    (dotimes (i 2000) (setq sum (leaf sum i)))
    (dotimes (i 3) (push (multiple-value-list (mv-leaf i 10)) mvs))
    (format t "SUM ~D~%MV ~S~%" sum mvs)
    (format t "THROW ~D~%" (catch :t0-native (dotimes (i 10) (thrower i)) :none))
    (dotimes (i 6)
      (handler-case (car-leaf (if (evenp i) (list i) i))
        (type-error () (incf errors))))
    (format t "ERRORS ~D~%" errors)
    (format t "SPECIAL ~D ~D ~D~%" (special-leaf 1)
            (let ((*k* 1000)) (special-leaf 1)) (special-binder 7))
    (format t "AFTER ~D~%" *k*)
    (dotimes (i 20000) (incf conses (length (cons-leaf i))))
    (format t "CONSES ~D~%" conses)
    ;; Values the warm-up never showed: a guard failure inside the leaf.
    (format t "DEOPT ~S ~S ~S~%" (leaf most-positive-fixnum 1) (leaf 1.5 2) (leaf 1/2 1/3))
    (format t "MVAFTER ~S~%" (multiple-value-list (leaf 1 2)))
    (let ((chars nil) (caught 0))
      (dotimes (i 3) (push (kw-char "hello" :start i) chars))
      (push (elt-leaf '(a b c) 2) chars)
      (dotimes (i 4)
        (handler-case (kw-char "hi" :start (* i 2)) (error () (incf caught)))
        (handler-case (elt-leaf "hi" (+ i 1)) (error () (incf caught))))
      (format t "CHARS ~S ~D~%" chars caught))
    (format t "KWDEOPT ~S ~S~%" (kw-add most-positive-fixnum) (kw-add 1/2 :b 1/2))
    (let ((caught 0))
      (dotimes (i 4) (handler-case (kw-car (if (evenp i) (list i) i)) (type-error () (incf caught))))
      (format t "KWERRORS ~D~%" caught))
    (format t "KWTHROW ~D~%" (catch :t0-native (dotimes (i 10) (kw-throw i)) :none))
    (let ((wide (coerce (list #\a (code-char 955) (code-char 128512)) 'string))
          (filled (make-array 5 :element-type 'character :fill-pointer 2
                                :initial-contents "fills"))
          (adjustable (make-array 3 :element-type 'character :adjustable t
                                    :initial-contents "adj"))
          (out nil))
      (dotimes (i 5) (push (str-char "héllo" i) out))
      (dotimes (i 3) (push (char-code (str-char wide i)) out) (push (str-elt wide i) out))
      (push (list (str-char filled 1) (str-elt filled 0) (str-char adjustable 2)
                  (str-schar "abc" 2) (str-elt '(p q r) 2) (str-elt (vector 7 8) 1))
            out)
      (dolist (form (list (lambda () (str-char "abc" 3)) (lambda () (str-char "abc" -1))
                          (lambda () (str-char "abc" 1.0)) (lambda () (str-char "abc" (expt 2 70)))
                          (lambda () (str-char nil 0)) (lambda () (str-char 12 0))
                          (lambda () (str-elt filled 3)) (lambda () (str-char filled 4))
                          (lambda () (str-schar adjustable 0))
                          (lambda () (str-elt "abc" 3)) (lambda () (kw-elt "abc" :start 9))))
        (push (handler-case (list :ok (funcall form))
                (error (e) (list :error (type-of e))))
              out))
      (format t "STRCHARS ~S~%" (reverse out)))
    (format t "STRMV ~S ~S ~S ~S~%" (multiple-value-list (char-after-values "xyz" 2))
            (char-mvl "xyz" 0) (multiple-value-list (str-char "xyz" 1))
            (multiple-value-list (char-floor "wxyz" 5)))
    (format t "STRDEOPT ~S ~S~%" (char-sum "abc" 0 most-positive-fixnum) (char-sum "abc" 2 1/2))
    (format t "STRTHROW ~S ~S~%" (catch :t0-native (dotimes (i 10) (char-throw "abcdefghij" i)) :none)
            (char-throw "abcdefghij" 5))
    (let* ((s (copy-seq "abc")) (before (str-char s 2)))
      (setf (char s 2) (code-char 955))
      (format t "STRSET ~S ~S ~S~%" before (str-char s 2) s))
    (format t "KWELT ~S ~S ~S~%" (kw-elt "hλ" :start 1) (kw-elt "hλ") (kw-elt '(1 2) :start 1))
    (format t "FIB ~D~%" (fib 20))
    (format t "REDEF ~D " (rleaf 6 7))
    (setf (symbol-function 'rleaf) (lambda (a b) (* a b)))
    (format t "~D~%" (rleaf 6 7))))
(caller)
(format t "TIERS ~D ~D~%" (egcl-ext:function-tier 'mv-leaf) (egcl-ext:function-tier 'caller))
(format t "LEAN-TIERS ~D ~D ~D ~D~%" (egcl-ext:function-tier 'elt-leaf)
        (egcl-ext:function-tier 'kw-char) (egcl-ext:function-tier 'kw-add)
        (egcl-ext:function-tier 'fib))
(format t "STR-TIERS ~D ~D ~D ~D ~D ~D~%" (egcl-ext:function-tier 'str-char)
        (egcl-ext:function-tier 'str-elt) (egcl-ext:function-tier 'char-sum)
        (egcl-ext:function-tier 'char-throw) (egcl-ext:function-tier 'kw-elt)
        (egcl-ext:function-tier 'char-floor))
"#;

fn run(force_interp: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", PROGRAM])
        .env_remove("EGCL_NATIVE_TRANSFER")
        .env_remove("EGCL_FORCE_TIER")
        // Promote every warmed callee to T2 synchronously; CALLER runs once.
        .env("EGCL_T2", "1")
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T2_THRESHOLD", "4");
    if force_interp {
        command.env("EGCL_FORCE_TIER", "interp");
    }
    let output = command.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn bytecode_callers_see_t2_callees_exactly_as_interpreted_ones() {
    let native = run(false);
    let interpreted = run(true);
    assert!(
        native.contains("TIERS 2 0"),
        "callees must be T2 and the caller bytecode: {native}"
    );
    assert!(
        native.contains("LEAN-TIERS 2 2 2 2"),
        "the new callees must be T2: {native}"
    );
    assert!(
        native.contains("STR-TIERS 2 2 2 2 2 2"),
        "the CHAR callers must be T2: {native}"
    );
    let strip = |text: &str| -> String {
        text.lines()
            .filter(|line| !line.contains("TIERS"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&native), strip(&interpreted));
    for expected in [
        "SUM 1999000",
        "MV ((12 -8) (11 -9) (10 -10))",
        "THROW 600",
        "ERRORS 3",
        "SPECIAL 11 1001 8",
        "AFTER 10",
        "CONSES 40000",
        "DEOPT 1152921504606846976 3.5 5/6",
        "MVAFTER (3)",
        "CHARS (C #\\l #\\e #\\h) 6",
        "KWDEOPT 1152921504606846976 1",
        "KWERRORS 2",
        "KWTHROW 600",
        "FIB 6765",
        "REDEF 13 42",
    ] {
        assert!(
            native.contains(expected),
            "missing {expected:?} in {native}"
        );
    }
}
