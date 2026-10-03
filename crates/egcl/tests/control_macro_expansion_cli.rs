// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn standard_control_macros_expand_and_preserve_values_and_scope() {
    let source = r#"
      (dolist (case '(((when t (values 3 4)) (3 4))
                     ((when nil (error "unreachable")) (nil))
                     ((unless nil (values 3 4)) (3 4))
                     ((unless t (error "unreachable")) (nil))
                     ((and) (t))
                     ((and t (values 3 4)) (3 4))
                     ((and nil (error "unreachable")) (nil))
                     ((or) (nil))
                     ((or nil (values 3 4)) (3 4))
                     ((or (values 3 4) (error "unreachable")) (3))
                     ((dolist (i '(1 2 3) (values i :done)) (identity i)) (nil :done))
                     ((dolist (i '(1 2 3)) (return (values i :early))) (1 :early))
                     ((return (values 3 4)) (3 4))))
        (let ((form (first case)) (expected (second case)))
          (multiple-value-bind (expanded expandedp) (macroexpand-1 form)
            (assert expandedp)
            (assert (equal (multiple-value-list (eval (list 'block nil expanded))) expected)))
          (let ((expanded (funcall (macro-function (car form)) form nil)))
            (assert (not (equal expanded form)))
            (assert (equal (multiple-value-list (eval (list 'block nil expanded))) expected)))))
      ;; The list form runs once; body tags and declarations retain their scope.
      (let ((evaluations 0) (total 0))
        (dolist (item (progn (incf evaluations) '(1 2 3)))
          (declare (integer item))
          (when (= item 2) (go next))
          (incf total item)
          next)
        (assert (= evaluations 1))
        (assert (= total 4)))
      ;; A hygienic expansion must not capture an ordinary user variable.
      (let ((tail 7) (top 8) (end 9) (value 10))
        (assert (= (or nil (+ tail top end value)) 34)))
      (format t "CONTROL-EXPANSION-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("CONTROL-EXPANSION-OK"), "{stdout}");
}
