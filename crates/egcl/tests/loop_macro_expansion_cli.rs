// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn loop_expansion_rejects_duplicate_variable_bindings() {
    let source = r#"
      (dolist (form '((loop for (x . x) in '((1 . 2)) collect x)
                     (loop for (x . x) = '(1 . 2) repeat 1 collect x)
                     (loop for x across #(1 2) for x from 0 collect x)
                     (loop with x = 1 and x = 2 return x)
                     (loop with x = 1 for x below 2 collect x)
                     (loop for x below 2 sum x into x)))
        (assert (handler-case (progn (macroexpand-1 form) nil)
                  (program-error () t))))
      ;; Ignored positions and uses of an existing accumulator do not
      ;; introduce duplicate bindings.
      (dolist (case '(((loop for (nil nil x) in '((1 2 3)) collect x) (3))
                     ((loop for nil below 3 and nil below 2 collect :x) (:x :x))
                     ((loop for x below 3 sum x into total count t into total
                            finally (return total)) 6)))
        (multiple-value-bind (expanded expandedp) (macroexpand-1 (first case))
          (assert expandedp)
          (assert (equal (eval expanded) (second case)))))
      (format t "LOOP-DUPLICATE-BINDINGS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-DUPLICATE-BINDINGS-OK"));
}

#[test]
fn loop_expansion_rejects_incompatible_implicit_results() {
    let source = r#"
      (dolist (test '(always never thereis))
        (dolist (clauses `((collect x ,test (oddp x))
                          (,test (oddp x) collect x)
                          (sum x ,test (oddp x))
                          (,test (oddp x) sum x)))
          (assert (handler-case
                      (progn (macroexpand-1 `(loop for x below 3 ,@clauses)) nil)
                    (program-error () t)))))
      ;; Named accumulators do not compete for the implicit result.
      (assert (eq (eval (macroexpand-1
                         '(loop for x from 1 to 3 collect x into items
                                always (plusp x)))) t))
      (format t "LOOP-RESULT-COMPATIBILITY-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-RESULT-COMPATIBILITY-OK"));
}

#[test]
fn loop_list_step_form_is_evaluated_once() {
    let source = r#"
      (dolist (case
               '(((loop for x on '(1 2 . 3)
                        by (progn (incf calls) #'cdr) collect (car x)) (1 2))
                 ((loop for x in '(1 2 3 4 5)
                        by (progn (incf calls) #'cddr) collect x) (1 3 5))
                 ((loop for x on '(1 2 3 4 5)
                        by (progn (incf calls) #'cddr) collect (car x)) (1 3 5))
                 ((loop for x in nil
                        by (progn (incf calls) #'cdr) collect x) nil)))
        (multiple-value-bind (expanded expandedp) (macroexpand-1 (first case))
          (assert expandedp)
          (assert (equal (eval `(let ((calls 0)) (list ,expanded calls)))
                         (list (second case) 1)))))
      (format t "LOOP-LIST-STEP-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-LIST-STEP-OK"));
}

#[test]
fn loop_arithmetic_options_preserve_source_order() {
    let source = r#"
      (dolist (case
               '(((loop for x by 2 to 8 from 1 collect x) (1 3 5 7))
                 ((loop for x above 3 from 9 by 2 collect x) (9 7 5))
                 ((loop for nil downfrom 6 above 0 by 2 collect :x) (:x :x :x))
                 ((loop for nil from 4 below 4 collect :x) nil)))
        (multiple-value-bind (expanded expandedp) (macroexpand-1 (first case))
          (assert expandedp)
          (assert (equal (eval expanded) (second case)))))
      (dolist (case
               '(((loop for x from (incf n) by (incf n) to (+ n 5) collect x)
                  (1 3 5 7))
                 ((loop for x to (+ n 5) from (incf n) collect x)
                  (1 2 3 4 5))
                 ((loop for x by (incf n) below (+ n 4) from (incf n) collect x)
                  (2 3 4))))
        (let ((expanded (macroexpand-1 (first case))))
          (assert (equal (eval `(let ((n 0)) ,expanded)) (second case)))))
      (let ((function (compile nil '(lambda ()
                         (loop for nil from 10 to 12 collect :ignored)))))
        (assert (equal (funcall function) '(:ignored :ignored :ignored))))
      (format t "LOOP-ARITHMETIC-ORDER-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-ARITHMETIC-ORDER-OK"));
}

#[test]
fn loop_expansion_handles_package_paths_and_parallel_hash_values() {
    let source = r#"
      (let ((table (make-hash-table)))
        (setf (gethash :a table) 10 (gethash :b table) 20)
        (multiple-value-bind (expanded expandedp)
            (macroexpand-1 `(loop for value being the hash-values of ',table
                                 and previous = value collect (list value previous)))
          (assert expandedp)
          (let ((pairs (eval expanded)))
            (assert (= (length pairs) 2))
            (assert (equal (mapcar #'second pairs)
                           (cons nil (butlast (mapcar #'first pairs))))))))
      (let ((package (make-package (symbol-name (gensym "LOOP-PACKAGE")) :use nil)))
        (unwind-protect
            (let ((public (intern "PUBLIC" package)) (private (intern "PRIVATE" package)))
              (export public package)
              (dolist (case '((external-symbols 1) (present-symbols 2) (symbols 2)))
                (multiple-value-bind (expanded expandedp)
                    (macroexpand-1 `(loop for symbol being the ,(first case) in ',package
                                         collect symbol))
                  (assert expandedp)
                  (let ((symbols (eval expanded)))
                    (assert (= (length symbols) (second case)))
                    (assert (member public symbols))
                    (when (= (second case) 2) (assert (member private symbols)))))))
          (delete-package package)))
      (format t "LOOP-PATHS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-PATHS-OK"));
}

#[test]
fn loop_expansion_preserves_parallel_bindings_destructuring_and_types() {
    let source = r#"
      (dolist (case
               '(((loop for a = 0 then b and b = 1 then (+ a b)
                        repeat 6 collect a) (0 1 1 2 3 5))
                 ((loop for a from 0 and b = a repeat 3 collect (list a b))
                  ((0 0) (1 0) (2 1)))
                 ((loop for a from 0 for b = a repeat 3 collect (list a b))
                  ((0 0) (1 1) (2 2)))
                 ((loop for a from 0 and b below 2 finally (return (list a b))) (2 2))
                 ((loop for a from 0 and b in '(10 20) finally (return (list a b))) (1 20))
                 ((loop for (a . b) in '((1 . 2) (3 . 4)) collect (+ a b)) (3 7))
                 ((loop with x of-type fixnum return x) 0)
                 ((loop with x of-type float return x) 0.0)
                 ((loop with (a . b) = '(2 . 3) return (+ a b)) 5)
                 ((loop with (a b) of-type (fixnum float) return (list a b)) (0 0.0))
                 ((loop for x in nil sum x of-type bit) 0)
                 ((loop for x in nil sum x of-type (and integer (real -10.0 10.0))) 0)
                 ((loop for x in nil sum x of-type float) 0.0)
                 ((loop for x in '(nil 2 3) when x collect it) (2 3))
                 ((loop for x from 1 to 4 sum x into total of-type fixnum
                        finally (return total)) 10)))
        (multiple-value-bind (expansion expandedp) (macroexpand-1 (first case))
          (assert expandedp)
          (assert (equal (eval expansion) (second case)))))
      ;; WITH ... AND initializers see the bindings outside their group.
      (let ((outer 10))
        (assert (equal (loop with outer = 1 and other = outer
                            return (list outer other)) '(1 10))))
      (format t "LOOP-BINDINGS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-BINDINGS-OK"));
}

#[test]
fn loop_expansion_preserves_drivers_accumulators_and_lexical_exits() {
    let source = r#"
      (dolist (case
               '(((loop for i below 4 collect i) ((0 1 2 3)))
                 ((loop for nil from 10 to 12 collect :ignored)
                  ((:ignored :ignored :ignored)))
                 ((loop for x across #(1 2 3) sum x) (6))
                 ((loop for x in '(1 2 3) maximize x) (3))
                 ((loop for x in '(1 2 3) minimize x) (1))
                 ((loop for x in '(1 2 3) always (plusp x)) (t))
                 ((loop for x in '(1 2 3) never (minusp x)) (t))
                 ((loop for x in '(nil 7 8) thereis x) (7))
                 ((loop named outer for x from 1
                        do (return-from outer (values x :named))) (1 :named))
                 ((loop (return (values 4 5))) (4 5))
                 ((loop for x from 1
                        do (when (= x 3) (loop-finish))
                        collect x) ((1 2)))))
        (let ((form (first case)) (expected (second case)))
          (multiple-value-bind (expanded expandedp) (macroexpand-1 form)
            (unless expandedp (error "LOOP did not expand: ~S" form))
            (assert (equal (multiple-value-list (eval expanded)) expected)))
          (let ((expanded (funcall (macro-function 'loop) form nil)))
            (assert (not (equal expanded form)))
            (assert (equal (multiple-value-list (eval expanded)) expected)))))
      (let ((table (make-hash-table)))
        (setf (gethash :a table) 2 (gethash :b table) 3)
        (multiple-value-bind (expanded expandedp)
            (macroexpand-1 `(loop for value being the hash-values of ',table sum value))
          (assert expandedp)
          (assert (= (eval expanded) 5))))
      (format t "LOOP-EXPANSION-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-EXPANSION-OK"));
}

#[test]
fn conditional_it_is_limited_to_the_first_following_clause() {
    let source = r#"
      (dolist (case
               '(((let ((it 'z)) (loop for x in '(a b) when x collect it and collect it))
                  (a z b z))
                 ((let ((it 'z)) (loop for x in '(nil b) if x collect it
                                      else collect it and collect it)) (z z b))
                 ((let ((it 'z)) (loop for x in '(a b) when x collect x and collect it))
                  (a z b z))
                 ((let ((it 'z)) (loop for x in '(a b) when x if it collect it
                                      and collect it end and collect it)) (a z z b z z))))
        (let ((actual (eval (first case))))
          (unless (equal actual (second case))
            (error "Expected ~S, got ~S" (second case) actual))))
      (format t "LOOP-CONDITIONAL-IT-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output().unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("LOOP-CONDITIONAL-IT-OK"));
}
