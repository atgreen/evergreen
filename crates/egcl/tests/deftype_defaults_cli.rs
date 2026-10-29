// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn eval(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("DEFTYPE-CONTRACT-OK"));
}

const DEFAULTS: &str = r#"
  (deftype optional-atom (&optional size) (list 'eql size))
  (deftype optional-list (&optional (size)) (list 'eql size))
  (deftype keyword-atom (&key size) (list 'eql size))
  (deftype keyword-list (&key (size)) (list 'eql size))
  (deftype keyword-renamed (&key ((:extent size))) (list 'eql size))
  (deftype explicit-nil (&optional (size nil supplied)) (list 'member size supplied))
  (deftype explicit-key-nil (&key ((:extent size) nil supplied)) (list 'member size supplied))
  (deftype aux-nil (&aux size) (list 'eql size))
  (deftype required-rest (first &rest rest) (list 'integer first (car rest)))
  (defvar *default-counter* 0)
  (deftype computed-default (&optional (size (incf *default-counter*) supplied))
    (list 'member size supplied))
"#;

const DEFAULT_CHECKS: &str = r#"
  (dolist (type '(optional-atom optional-list keyword-atom keyword-list keyword-renamed))
    (assert (typep '* type))
    (assert (not (typep nil type))))
  (assert (typep nil '(optional-atom nil)))
  (assert (typep 7 '(optional-list 7)))
  (assert (typep nil '(keyword-atom :size nil)))
  (assert (typep 7 '(keyword-list :size 7)))
  (assert (typep 7 '(keyword-renamed :extent 7)))
  (assert (typep nil 'explicit-nil))
  (assert (typep t '(explicit-nil 3)))
  (assert (typep nil 'explicit-key-nil))
  (assert (typep t '(explicit-key-nil :extent 3)))
  (assert (typep nil 'aux-nil))
  (assert (typep 15 '(required-rest 10 20)))
  (assert (not (typep 5 '(required-rest 10 20))))
  (assert (= *default-counter* 0))
  (assert (typep 1 'computed-default))
  (assert (= *default-counter* 1))
  (assert (typep t '(computed-default 7)))
  (assert (= *default-counter* 1))
"#;

const BLOCKS: &str = r#"
  (deftype early-type () "type documentation"
    (return-from early-type 'integer)
    (error "must not run after RETURN-FROM"))
  (let ((deftype-free-value 'string))
    (deftype free-special-type (choose)
      (declare (special deftype-free-value))
      (if choose deftype-free-value 'character)))
  (deftype special-type (size) "declared special parameter"
    (declare (special size))
    (flet ((read-size () (symbol-value 'size)))
      (return-from special-type (list 'eql (read-size)))))
"#;
const BLOCK_CHECKS: &str = r#"
  (assert (equal (documentation 'early-type 'type) "type documentation"))
  (assert (typep 7 'early-type))
  (assert (not (typep #\a 'early-type)))
  (dotimes (i 30)
    (assert (typep 17 '(special-type 17)))
    (let ((deftype-free-value 'integer))
      (declare (special deftype-free-value))
      (assert (typep 17 '(free-special-type t)))
      (assert (not (typep #\a '(free-special-type t))))))
  (assert (not (boundp 'size)))
  (assert (eq :escaped
    (block escape-type
      (deftype escape-type (&optional (x (return-from escape-type :escaped)))
        (declare (ignore x))
        'integer)
      (typep 7 'escape-type)
      :failed)))
"#;

#[test]
fn deftype_defaults_distinguish_omitted_and_explicit_initializers() {
    eval(&format!(
        "(progn {DEFAULTS} {DEFAULT_CHECKS} (format t \"DEFTYPE-CONTRACT-OK~%\"))"
    ));
}

#[test]
fn deftype_named_block_covers_body_but_not_defaults() {
    eval(&format!(
        "(progn {BLOCKS} {BLOCK_CHECKS} (format t \"DEFTYPE-CONTRACT-OK~%\"))"
    ));
}

#[test]
fn deftype_defaults_and_named_blocks_survive_compiled_file() {
    let dir = std::env::temp_dir().join(format!("egcl-deftype-contract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("types.lisp");
    let artifact = dir.join("types.bfasl");
    std::fs::write(&src, format!("{DEFAULTS}\n{BLOCKS}")).unwrap();
    eval(&format!(
        "(progn (compile-file {src:?} :output-file {artifact:?}) (assert (probe-file {artifact:?})) (format t \"DEFTYPE-CONTRACT-OK~%\"))"
    ));
    std::fs::remove_file(src).unwrap();
    eval(&format!(
        "(progn (load {artifact:?}) {DEFAULT_CHECKS} {BLOCK_CHECKS} (format t \"DEFTYPE-CONTRACT-OK~%\"))"
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compiled_closure_keeps_parameter_declarations_during_capture_expansion() {
    let dir = std::env::temp_dir().join(format!("egcl-closure-declaration-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("closure.lisp");
    let artifact = dir.join("closure.bfasl");
    std::fs::write(
        &src,
        r#"
      (defparameter *declared-closure*
        (lambda (declared-value)
          (declare (special declared-value))
          (flet ((read-value () (symbol-value 'declared-value)))
            (read-value))))
    "#,
    )
    .unwrap();
    eval(&format!(
        "(progn (compile-file {src:?} :output-file {artifact:?}) (assert (probe-file {artifact:?})) (format t \"DEFTYPE-CONTRACT-OK~%\"))"
    ));
    std::fs::remove_file(src).unwrap();
    eval(&format!(
        r#"(progn (load {artifact:?})
      (dotimes (i 30) (assert (= 17 (funcall *declared-closure* 17))))
      (assert (not (boundp 'declared-value)))
      (format t "DEFTYPE-CONTRACT-OK~%"))"#
    ));
    std::fs::remove_dir_all(dir).unwrap();
}
