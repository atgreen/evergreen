// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::Path, process::Command};

// SBCL's cross compiler deliberately implements GENSYM with MAKE-SYMBOL.
// These variables have identical names and distinct identities.
const DEFINITIONS: &str = r#"
(defmacro distinct-mvb ()
  (let ((variables (loop repeat 8 collect (make-symbol "G"))))
    `(multiple-value-bind ,variables (values nil nil nil nil nil nil t (list 42))
       (list ,@variables))))
(defmacro distinct-let ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")))
    `(let ((,a 10) (,b 20))
       (setq ,a (+ ,a 1))
       (list ,a ,b (let ((,a 30)) (incf ,a) (list ,a ,b)) ,a ,b))))
(defmacro distinct-lambda ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")))
    `(funcall (lambda (,a ,b) (setq ,a (+ ,a 1)) (list ,a ,b)) 10 20)))
(defmacro distinct-capture ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")))
    `(let ((,a (list 10)) (,b (list 20)))
       (let ((read (lambda () (list (car ,a) (car ,b))))
             (write (lambda (x y) (setq ,a (list x) ,b (list y)))))
         (assert (equal (funcall read) '(10 20)))
         (funcall write 31 41)
         (funcall read)))))
(defmacro distinct-variadic ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")) (c (make-symbol "G")))
    `(let ((function (lambda (,a &optional (,b ,a) &key ((:value ,c) ,b))
                       (list ,a ,b ,c))))
       (list (funcall function 7) (funcall function 7 8 :value 9)))))
(defmacro distinct-retained ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")))
    `(let ((,a (list 10)) (,b (list 20)))
       (lambda (&optional delta)
         (when delta (incf (car ,a) delta) (incf (car ,b) (* 2 delta)))
         (list (car ,a) (car ,b))))))
(defmacro distinct-flet ()
  (let ((a (make-symbol "G")) (b (make-symbol "G")))
    `(let ((,a 10) (,b 20))
       (flet ((capture-read () (list ,a ,b))
              (capture-write () (setq ,a (+ ,a 1) ,b (+ ,b 2))))
         (capture-write) (capture-read)))))
(defmacro distinct-reserved-name ()
  (let ((a (intern (concatenate 'string (string (code-char 0))
                                "egcl-lexical:name:PLAIN")))
        (b (intern "PLAIN")))
    `(let ((,a 10) (,b 20)) (incf ,a) (list ,a ,b))))
(defun distinct-mvb-function () (distinct-mvb))
(defun distinct-let-function () (distinct-let))
(defun distinct-lambda-function () (distinct-lambda))
(defun distinct-capture-function () (distinct-capture))
(defun distinct-variadic-function () (distinct-variadic))
(defun distinct-make-retained () (distinct-retained))
(defun distinct-flet-function () (distinct-flet))
(defun distinct-reserved-function () (distinct-reserved-name))
"#;

const CHECKS: &str = r#"
(dotimes (iteration 20)
  (assert (equal (distinct-mvb-function) '(nil nil nil nil nil nil t (42))))
  (assert (equal (distinct-let-function) '(11 20 (31 20) 11 20)))
  (assert (equal (distinct-lambda-function) '(11 20)))
  (assert (equal (distinct-capture-function) '(31 41)))
  (assert (equal (distinct-flet-function) '(11 22)))
  (assert (equal (distinct-reserved-function) '(11 20)))
  (assert (equal (distinct-variadic-function) '((7 7 7) (7 8 9)))))
(let ((function (distinct-make-retained)))
  (assert (equal (funcall function) '(10 20)))
  (assert (equal (funcall function 3) '(13 26))))
(write-line "DISTINCT-BINDINGS-OK")
"#;

fn run(program: &str, stress: bool, image: Option<&Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    let output = command
        .args(["--eval", &format!("(progn {program})")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("DISTINCT-BINDINGS-OK"), "{stdout}");
    stdout
}

fn directory(label: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "egcl-distinct-bindings-{label}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn same_named_symbols_remain_distinct_in_compiled_bindings_and_captures() {
    let program = format!(
        "{DEFINITIONS}
         (assert (equal (eval '(distinct-mvb)) '(nil nil nil nil nil nil t (42))))
         (let ((function (compile nil '(lambda () (distinct-mvb)))))
           (assert (equal (funcall function) '(nil nil nil nil nil nil t (42)))))
         {CHECKS}"
    );
    assert_eq!(run(&program, false, None), run(&program, true, None));
}

#[test]
fn same_named_bindings_survive_fresh_loading_of_real_bytecode() {
    let directory = directory("compiled");
    let source = directory.join("bindings.lisp");
    let artifact = directory.join("bindings.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
                 (compile-file {source:?} :output-file {artifact:?})
               (declare (ignore warnings)) (assert file) (assert (not failure)))
             (write-line \"DISTINCT-BINDINGS-OK\")"
        ),
        false,
        None,
    );
    let bytes = fs::read(&artifact).unwrap();
    // The SBCL oracle writes its own FASL; EGCL must emit bytecode, with no
    // serialized top-level source to conceal a failed compiler regression.
    if bytes.starts_with(b"BFASL\0") {
        let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..sections {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            assert_ne!(kind, 11, "unexpected source fallback");
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "expected portable BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS}");
    assert_eq!(run(&program, false, None), run(&program, true, None));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn same_named_captured_bindings_survive_saved_core() {
    let feature = run(
        "(write-line (if (member :egcl *features*) \"EGCL\" \"ORACLE\"))
         (write-line \"DISTINCT-BINDINGS-OK\")",
        false,
        None,
    );
    if !feature.lines().any(|line| line == "EGCL") {
        return;
    }
    let directory = directory("image");
    let image = directory.join("bindings.core");
    run(
        &format!(
            "{DEFINITIONS} {CHECKS}
             (defparameter *distinct-retained* (distinct-make-retained))
             (assert (equal (funcall *distinct-retained* 3) '(13 26)))
             (egcl-ext:save-lisp-and-die {image:?})"
        ),
        false,
        None,
    );
    let restored = format!(
        "{CHECKS}
         (assert (equal (funcall *distinct-retained*) '(13 26)))
         (assert (equal (funcall *distinct-retained* 2) '(15 30)))"
    );
    assert_eq!(
        run(&restored, false, Some(&image)),
        run(&restored, true, Some(&image))
    );
    fs::remove_dir_all(directory).unwrap();
}
