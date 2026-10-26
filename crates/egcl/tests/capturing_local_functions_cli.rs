// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(defun captured-read (n)
  (let ((mask 7) (sum 0))
    (labels ((cell (x) (logand x mask)))
      (dotimes (i n sum) (incf sum (cell i))))))
(defun captured-write (n)
  (let ((stamp 0) (sum 0))
    (flet ((cell (x) (incf stamp) (logand x 7)))
      (dotimes (i n (+ stamp sum)) (incf sum (cell i))))))
(defun shared-locals (seed)
  (labels ((read-value () seed)
           (write-value (v) (setq seed v))
           (count-down (n)
             (if (zerop n) (read-value)
                 (progn (write-value (1+ seed)) (count-down (1- n))))))
    (count-down 10)
    (list #'read-value #'write-value)))
(defun shadowed-local (x)
  (flet ((read-x () x))
    (let ((x 900)) (declare (ignore x)) (read-x))))
(defun argument-order (seed)
  (let ((x seed))
    (flet ((pair (x y) (list x y)))
      (list (pair (progn (incf x) x) (progn (incf x) x)) x))))
(defun same-variable-function-name ()
  (let ((cell 17)) (flet ((cell () cell)) (cell))))
(setq free-special-value 101)
(defun free-special-local ()
  (let ((free-special-value 7))
    (flet ((read-special () (declare (special free-special-value)) free-special-value))
      (list (read-special) free-special-value))))
(defmacro hidden-local-return () '(return-from cell :inner))
(defun hidden-local-exit ()
  (block cell
    (flet ((cell () (hidden-local-return)))
      (list (cell) :outer-survived))))
(defmacro hidden-local-read () 'seed)
(defun hidden-local-capture (seed)
  (flet ((reader () (hidden-local-read))) (reader)))
(defun inherited-local-function (seed)
  (flet ((outer (x) (+ x 10)))
    (flet ((inner () (let ((x seed)) (outer x))))
      (inner))))
(defun capturing-flet-function-shadow (seed)
  (flet ((cell () (+ seed 1)))
    (flet ((cell () (+ (cell) seed))) (cell))))
(defun inherited-free-special-parameter ()
  (let ((parameter 7))
    (declare (special parameter))
    (flet ((identity-local (parameter) parameter))
      (list (identity-local 42)
            (let ((parameter 99))
              (declare (special parameter))
              (identity-local 43))))))
(defparameter dynamic-local-parameter 100)
(declaim (special dynamic-local-parameter))
(defun globally-special-local-parameter ()
  (let ((capture 7))
    (flet ((read-parameter () dynamic-local-parameter))
      (flet ((helper (dynamic-local-parameter) (+ capture (read-parameter))))
        (helper 3)))))
(defun compiler-hidden-exit-call () :not-expanded)
(define-compiler-macro compiler-hidden-exit-call () '(return-from cell :inner))
(defun compiler-hidden-local-exit ()
  (block cell
    (flet ((cell () (compiler-hidden-exit-call)))
      (list (cell) :outer-survived))))
(defun five-contextual-captures (a b c d e offset)
  (flet ((outer (x) (+ offset x)))
    (+ (flet ((one () (+ a (outer 1)))) (one))
       (flet ((two () (+ b (outer 1)))) (two))
       (flet ((three () (+ c (outer 1)))) (three))
       (flet ((four () (+ d (outer 1)))) (four))
       (flet ((five () (+ e (outer 1)))) (five)))))
(defun mixed-global-cell (x) (+ x 1000))
(defun mixed-flet-sibling (mask)
  (flet ((mixed-global-cell (x) (logand x mask))
         (work (x) (+ mask (mixed-global-cell x))))
    (work 3)))
(defun mixed-local-factory (mask)
  (labels ((cell (x) (logand x mask))
           ;; MASK is only a transitive free reference, through CELL.
           (work (n)
             (let ((sum 0)) (dotimes (i n sum) (incf sum (cell i))))))
    #'work))
(defun mixed-local-mutation (n)
  (let ((stamp 0))
    (labels ((cell (x) (incf stamp) (logand x 7))
             (work (count)
               (let ((sum 0)) (dotimes (i count (+ sum stamp)) (incf sum (cell i))))))
      (work n))))
(defun sibling-parameter-shadow (mask)
  (labels ((cell (x) (logand x mask))
           (work (mask) (+ (cell 3) mask)))
    (work 100)))
(defun boxed-sibling-parameter-shadow (mask)
  (labels ((cell (x) (logand x mask))
           (work (mask)
             (let ((reader (lambda () mask))) (+ (cell 3) (funcall reader)))))
    (work 100)))
(defun mixed-callsite-shadow (mask)
  (labels ((cell (x) (logand x mask))
           (other (n) (if (zerop n) mask (other (1- n)))))
    (let ((mask 100)) (declare (ignore mask)) (cell 3))))
(defun boxed-shadowing (seed)
  (let ((reader (lambda () seed)))
    (flet ((cell () seed))
      (let ((seed 900))
        (declare (ignore seed))
        (list (cell) (funcall reader))))))
;; Repeated calls exercise opportunistic compilation, not merely cold eval.
(dotimes (warm 80)
  (assert (= (captured-read 1000) 3500))
  (assert (= (captured-write 1000) 4500))
  (assert (= (shadowed-local 42) 42))
  (assert (equal (argument-order 10) '((11 12) 12)))
  (assert (= (same-variable-function-name) 17))
  (assert (equal (free-special-local) '(101 7)))
  (assert (equal (hidden-local-exit) '(:inner :outer-survived)))
  (assert (= (hidden-local-capture 47) 47))
  (assert (= (inherited-local-function 23) 33))
  (assert (= (capturing-flet-function-shadow 5) 11))
  (assert (equal (inherited-free-special-parameter) '(42 43)))
  (assert (= (globally-special-local-parameter) 10))
  (assert (equal (compiler-hidden-local-exit) '(:inner :outer-survived)))
  (assert (equal (boxed-shadowing 53) '(53 53)))
  (assert (= (five-contextual-captures 1 2 3 4 5 10) 70))
  (assert (= (mixed-local-mutation 1000) 4500))
  (assert (= (mixed-flet-sibling 7) 1010))
  (assert (= (sibling-parameter-shadow 7) 103))
  (assert (= (boxed-sibling-parameter-shadow 7) 103))
  (assert (= (mixed-callsite-shadow 7) 3)))
(let* ((a (shared-locals 10)) (b (shared-locals 100)))
  (assert (= (funcall (first a)) 20))
  (assert (= (funcall (first b)) 110))
  (funcall (second a) (list 17 23))
  (egcl-ext:gc)
  (assert (equal (funcall (first a)) '(17 23)))
  (assert (= (funcall (first b)) 110)))
(assert (equal
  (block cell (flet ((cell () (hidden-local-return))) (list (cell) :outer-survived)))
  '(:inner :outer-survived)))
(assert (equal
  (block cell
    (flet ((cell () (compiler-hidden-exit-call))) (list (cell) :outer-survived)))
  '(:inner :outer-survived)))
(let ((work (mixed-local-factory 7)))
  (dotimes (warm 40) (assert (= (funcall work 1000) 3500)))
  (format t "MIXED-WORK-TIER ~S~%" (egcl-ext:function-tier work)))
(format t "FIVE-CAPTURE-TIER ~S~%" (egcl-ext:function-tier 'five-contextual-captures))
(format t "CAPTURED-LOCALS-OK ~S~%"
        (mapcar #'egcl-ext:function-tier '(captured-read captured-write shadowed-local)))
"#;

fn check(tier: &str, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    let program = if stress {
        PROGRAM
            .replace("(warm 80)", "(warm 2)")
            .replace("(warm 40)", "(warm 2)")
    } else {
        PROGRAM.to_owned()
    };
    command
        .args(["--no-init", "--eval", &program])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_BAIL_TRACE", "1");
    if stress {
        command
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{tier}, stress={stress}: {stdout}\n{stderr}"
    );
    assert!(stdout.contains("CAPTURED-LOCALS-OK"), "{stdout}\n{stderr}");
    assert!(
        !stderr.contains("flet:captures-enclosing-lexical"),
        "{stderr}"
    );
    stdout.into_owned()
}

#[test]
fn capturing_local_functions_compile_and_share_bindings() {
    let baseline = check("t0", false);
    assert!(baseline.contains("(0 0 0)"), "{baseline}");
    for tier in ["t1", "t2"] {
        let out = check(tier, false);
        if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
            assert!(
                !out.contains("MIXED-WORK-TIER 0"),
                "sibling caller must compile: {out}"
            );
            assert!(
                !out.contains("FIVE-CAPTURE-TIER 0"),
                "five captures must compile: {out}"
            );
            assert!(
                !out.contains("(0 0 0)"),
                "native enclosing functions required: {out}"
            );
        }
    }
}

#[test]
fn capturing_local_functions_under_gc_stress() {
    let out = check("t1", true);
    assert!(out.contains("CAPTURED-LOCALS-OK"), "{out}");
}
