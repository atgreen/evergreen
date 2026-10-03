// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn expanded_places_and_control_forms_survive_a_fresh_fasl_load() {
    let directory =
        std::env::temp_dir().join(format!("egcl-expanded-places-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("places.lisp");
    let fasl = directory.join("places.fasl");
    std::fs::write(
        &source,
        r#"
      (defun cell-reader (cell) (car cell))
      (defun cell-writer (cell value) (rplaca cell value) value)
      (defsetf cell-reader cell-writer)
      ;; Require expansion during compilation, not only evaluator support.
      (eval-when (:compile-toplevel :load-toplevel :execute)
        (dolist (form '((setf (cell-reader x) 3) (when t 1)
                       (unless nil 1) (and t 1) (or nil 1)
                       (dolist (x nil) x) (return 1)))
          (assert (nth-value 1 (macroexpand-1 form)))))
      (defun exercise-expanded-places ()
        (let ((cell (list 0)) (events nil) (plist nil))
          (dolist (value '(1 2 3))
            (when (and value (or nil t))
              (unless (= value 2)
                (setf (cell-reader (progn (push :place events) cell))
                      (progn (push value events) value)))))
          (setf (getf plist :answer) (cell-reader cell))
          (block nil
            (return (values plist (reverse events) (car cell))))))
    "#,
    )
    .unwrap();
    let compile = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!(
            "(multiple-value-bind (path warnings failure) (compile-file {source:?} :output-file {fasl:?}) (declare (ignore warnings)) (assert path) (assert (not failure)))"
        )])
        .output().unwrap();
    assert!(
        compile.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(!String::from_utf8_lossy(&compile.stderr).contains("source-only"));
    // Removing the source ensures the second process really uses the artifact.
    std::fs::remove_file(&source).unwrap();
    let load = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                r#"
          (load {fasl:?})
          (assert (equal (multiple-value-list (exercise-expanded-places))
                         '((:answer 3) (:place 1 :place 3) 3)))
          (format t "EXPANDED-FASL-OK~%")
        "#
            ),
        ])
        .output()
        .unwrap();
    assert!(
        load.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&load.stdout),
        String::from_utf8_lossy(&load.stderr)
    );
    assert!(String::from_utf8_lossy(&load.stdout).contains("EXPANDED-FASL-OK"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn setf_expands_custom_and_builtin_places_without_repeating_subforms() {
    let program = r#"
      (defvar *cell* (list 0))
      (defvar *order* nil)
      (defun cell-value (cell) (car cell))
      (defun set-cell-value (cell value) (setf (car cell) value))
      (defsetf cell-value set-cell-value)
      (multiple-value-bind (expansion expandedp)
          (macroexpand-1 '(setf (cell-value (progn (push :place *order*) *cell*))
                               (progn (push :value *order*) 42)))
        (assert expandedp)
        (assert (= (eval expansion) 42))
        (assert (equal *cell* '(42)))
        (assert (equal *order* '(:value :place))))
      (defvar *plist* nil)
      (dolist (form '((setf (car *cell*) 9)
                     (setf (getf *plist* :key) 10)))
        (multiple-value-bind (expanded expandedp) (macroexpand-1 form)
          (assert expandedp)
          (eval expanded)))
      (assert (equal *cell* '(9)))
      (assert (= (getf *plist* :key) 10))
      (let ((a 0) (b 0))
        (assert (equal (multiple-value-list (setf (values a b) (values 11 12))) '(11 12)))
        (assert (= a 11)) (assert (= b 12)))
      (symbol-macrolet ((cell (car *cell*)))
        (setf cell 13))
      (assert (= (car *cell*) 13))
      (format t "SETF-EXPANSION-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SETF-EXPANSION-OK"), "{stdout}");
}

#[test]
fn internal_store_is_a_special_operator_and_not_a_callable_function() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (assert (special-operator-p 'egcl::%setf))
          (assert (fboundp 'egcl::%setf))
          (assert (fdefinition 'egcl::%setf))
          (assert (null (macro-function 'egcl::%setf)))
          (assert (eq (handler-case (funcall 'egcl::%setf)
                        (undefined-function () :not-callable)) :not-callable))
          (format t "STORE-OPERATOR-OK~%")
        "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STORE-OPERATOR-OK"));
}
