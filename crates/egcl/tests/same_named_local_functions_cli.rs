// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::Path, process::Command};

const DEFINITIONS: &str = r#"
(defmacro same-named-function-set (base)
  (let ((a (make-symbol "MATCH-BODY"))
        (b (make-symbol "MATCH-BODY"))
        (c (make-symbol "MATCH-BODY")))
    `(let ((bias ,base))
       (flet ((,a (x y z) (+ bias 1 x y z))
              (,b (x y z w) (+ bias 2 x y z w))
              (,c (x y z) (+ bias 3 x y z)))
         (assert (eq #',a #',a))
         (assert (not (eq #',a #',b)))
         (assert (equal (list (,a 1 2 3) (,b 1 2 3 4) (,c 1 2 3))
                        (list (+ bias 7) (+ bias 12) (+ bias 9))))
         (list #',a #',b #',c (lambda (&optional (answer (,a 1 2 3))) answer))))))
(defun make-same-named-function-set (base) (same-named-function-set base))
(defun invoke-same-named-function-set (functions)
  (list (funcall (first functions) 1 2 3)
        (funcall (second functions) 1 2 3 4)
        (funcall (third functions) 1 2 3)
        (funcall (fourth functions))))
(defmacro same-named-function-controls ()
  (let ((a (make-symbol "RECURSE")) (b (make-symbol "RECURSE"))
        (plain (make-symbol "DIRECT")) (other (make-symbol "DIRECT"))
        (both (make-symbol "BOTH"))
        (reserved (intern (concatenate 'string (string (code-char 0))
                                      "egcl-lexical:function:PLAIN"))))
    `(list
       (flet ((,plain (x) (+ x 10)) (,other (x y) (+ x y 20)))
         (list (,plain 1) (,other 1 3) (funcall #',other 4 5)))
       (let ((bias 10))
         (labels ((,a (n) (if (zerop n) bias (,b (1- n) 1)))
                  (,b (n extra) (if (zerop n) (+ bias extra) (,a (1- n)))))
           (list (,a 1) (,b 0 7))))
       (let ((bias 10))
         (flet ((,a (x) (+ bias x)))
           (flet ((,a (x) (,a (+ x 1)))) (,a 2))))
       (let ((,both 111))
         (flet ((,both (x) (+ ,both x)))
           (list ,both (,both 1) (funcall #',both 2))))
       (let ((|#'PLAIN| 11) (,reserved 22))
         (flet ((plain () (+ |#'PLAIN| ,reserved)))
           (list |#'PLAIN| ,reserved (plain)))))))
(defun check-same-named-function-controls () (same-named-function-controls))
(defun local-function-place (cell) (car cell))
(defun (setf local-function-place) (new cell) (setf (car cell) new))
(defun check-local-function-setf-controls ()
  (let ((cell (list 1)))
    (flet (((setf local-function-place) (new place) (setf (car place) (+ new 10))))
      (funcall #'(setf local-function-place) 5 cell)
      (assert (= (car cell) 15)))
    (setf (local-function-place cell) 7)
    (assert (= (car cell) 7))
    t))
"#;

const CHECKS: &str = r#"
(dotimes (i 20)
  (assert (equal (invoke-same-named-function-set (make-same-named-function-set 100))
                 '(107 112 109 107)))
  (assert (equal (check-same-named-function-controls)
                 '((11 24 29) (11 17) 13 (111 112 113) (11 22 33))))
  (assert (check-local-function-setf-controls)))
(write-line "SAME-NAMED-LOCAL-FUNCTIONS-OK")
"#;

fn run(program: &str, stress: bool, image: Option<&Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
        .env_remove("EGCL_FORCE_TIER")
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
    assert!(stdout.contains("SAME-NAMED-LOCAL-FUNCTIONS-OK"), "{stdout}");
    stdout
}

fn directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "egcl-local-function-identities-{label}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn require_real_bytecode(bytes: &[u8]) {
    assert!(
        bytes.starts_with(b"BFASL\0"),
        "EGCL must emit a binary artifact"
    );
    assert!(bytes.len() >= 24);
    let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut offset = 24;
    let mut bytecode = false;
    for _ in 0..sections {
        assert!(offset + 6 <= bytes.len());
        let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
        offset += 6;
        assert!(offset + size <= bytes.len());
        assert_ne!(kind, 11, "serialized source cannot conceal failed lowering");
        if kind == 12 {
            bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
        }
        offset += size;
    }
    assert!(bytecode, "expected BYTECODE_UNIT");
}

#[test]
fn same_named_local_functions_preserve_identity_in_source_and_compile() {
    let program = format!(
        "{DEFINITIONS}
       (assert (equal (eval '(invoke-same-named-function-set
                              (same-named-function-set 100))) '(107 112 109 107)))
       (let ((function (compile nil '(lambda () (same-named-function-set 100)))))
         (assert (equal (invoke-same-named-function-set (funcall function))
                        '(107 112 109 107))))
       {CHECKS}"
    );
    assert_eq!(run(&program, false, None), run(&program, true, None));
}

#[test]
fn same_named_local_functions_survive_fresh_source_free_bytecode() {
    let directory = directory("compiled");
    let source = directory.join("functions.lisp");
    let artifact = directory.join("functions.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    let output = run(
        &format!(
            "(multiple-value-bind (file warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line (if (member :egcl *features*) \"EGCL-ARTIFACT\" \"ORACLE-ARTIFACT\"))
        (write-line \"SAME-NAMED-LOCAL-FUNCTIONS-OK\")"
        ),
        false,
        None,
    );
    if output.lines().any(|line| line == "EGCL-ARTIFACT") {
        require_real_bytecode(&fs::read(&artifact).unwrap());
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS}");
    assert_eq!(run(&program, false, None), run(&program, true, None));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn same_named_captured_function_names_survive_saved_core() {
    let feature = run(
        "(write-line (if (member :egcl *features*) \"EGCL\" \"ORACLE\"))
        (write-line \"SAME-NAMED-LOCAL-FUNCTIONS-OK\")",
        false,
        None,
    );
    if !feature.lines().any(|line| line == "EGCL") {
        return;
    }
    let directory = directory("core");
    let image = directory.join("functions.core");
    run(
        &format!(
            "{DEFINITIONS} {CHECKS}
        (defparameter *retained-local-functions* (make-same-named-function-set 100))
        (assert (equal (invoke-same-named-function-set *retained-local-functions*)
                       '(107 112 109 107)))
        (write-line \"SAME-NAMED-LOCAL-FUNCTIONS-OK\")
        (egcl-ext:save-lisp-and-die {image:?})"
        ),
        false,
        None,
    );
    let program = format!(
        "{CHECKS}
        (assert (equal (invoke-same-named-function-set *retained-local-functions*)
                       '(107 112 109 107)))"
    );
    assert_eq!(
        run(&program, false, Some(&image)),
        run(&program, true, Some(&image))
    );
    fs::remove_dir_all(directory).unwrap();
}
