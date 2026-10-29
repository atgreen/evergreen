// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::Path, process::Command};

const SOURCE: &str = r#"
(defmacro captured-function-literal ()
  (let ((function (let ((state 40)) (lambda () (incf state)))))
    (list 'funcall function)))
(defun verify-function-literal ()
  (assert (= (eval '(captured-function-literal)) 41))
  (let ((function (compile nil '(lambda () (captured-function-literal)))))
    (assert (= (funcall function) 41))
    (assert (= (funcall function) 42))
    (dotimes (index 500)
      (assert (= (funcall function) (+ index 43)))))
  (write-line "FUNCTION-LITERAL-OK"))
;; Inherited defaults supply stored initializer functions to a BOA constructor,
;; matching the SBCL MAKE-RANDOM-TN promotion path.
(defstruct (literal-parent (:constructor nil) (:copier nil)) (number nil))
(defstruct (literal-child (:include literal-parent)
            (:constructor make-literal-child (offset &aux (kind :normal))))
  (kind :missing) (offset nil) (bits (make-array 3 :initial-element 0)))
(defun verify-literal-constructor ()
  (dotimes (index 500)
    (let ((object (make-literal-child index)))
      (assert (null (literal-child-number object)))
      (assert (eq (literal-child-kind object) :normal))
      (assert (= (literal-child-offset object) index))
      (assert (equalp (literal-child-bits object) #(0 0 0)))))
  (let ((first (make-literal-child 1)) (second (make-literal-child 2)))
    (assert (not (eq (literal-child-bits first) (literal-child-bits second))))))
"#;

fn run(program: &str, stress: bool) -> String {
    run_with_image(program, stress, None)
}

fn run_with_image(program: &str, stress: bool, image: Option<&Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    if let Some(image) = image {
        command.args(["--image", image.to_str().unwrap()]);
    }
    let output = command
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .output()
        .expect("run function literal regression");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("FUNCTION-LITERAL-OK"), "stdout={stdout}");
    stdout
}

#[test]
fn captured_function_literals_keep_state_in_compiled_calls() {
    let source = format!("{SOURCE} (verify-function-literal) (verify-literal-constructor)");
    assert_eq!(run(&source, false), run(&source, true));
}

#[test]
fn loaded_bytecode_compiles_function_literals_and_warms_boa_constructors() {
    let directory =
        std::env::temp_dir().join(format!("egcl-function-literal-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("literal.lisp");
    let artifact = directory.join("literal.bfasl");
    fs::write(&source, SOURCE).unwrap();
    run(&format!("(multiple-value-bind (path warnings failure) (compile-file {source:?} :output-file {artifact:?}) (declare (ignore warnings)) (assert path) (assert (not failure))) (write-line \"FUNCTION-LITERAL-OK\")"), false);
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..count {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "expected portable bytecode");
    }
    fs::remove_file(source).unwrap();
    // COMPILE runs after fresh source-free loading. Function constants are
    // process-local here; this does not assert closure serialization support.
    let program =
        format!("(load {artifact:?}) (verify-function-literal) (verify-literal-constructor)");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn compile_file_never_persists_a_process_local_closure_id() {
    let directory =
        std::env::temp_dir().join(format!("egcl-function-literal-save-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("literal.lisp");
    let artifact = directory.join("literal.bfasl");
    fs::write(
        &source,
        r#"
(eval-when (:compile-toplevel)
  (defmacro emit-captured-function (name)
    (let ((function (let ((captured 42)) (lambda () captured))))
      (list 'defun name nil (list 'funcall function)))))
(emit-captured-function saved-function-literal)
"#,
    )
    .unwrap();
    let compile = format!(
        r#"
(handler-case
    (multiple-value-bind (path warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
      (declare (ignore warnings))
      (if failure
          (write-line "FUNCTION-LITERAL-REJECTED")
          (progn (assert path) (write-line "FUNCTION-LITERAL-COMPILED"))))
  (error (condition)
    (assert (search "function literal" (string-downcase (princ-to-string condition))))
    (write-line "FUNCTION-LITERAL-REJECTED")))
#+egcl (write-line "FUNCTION-LITERAL-EGCL")
(write-line "FUNCTION-LITERAL-OK")"#
    );
    let stdout = run(&compile, false);
    fs::remove_file(source).unwrap();
    if stdout.contains("FUNCTION-LITERAL-COMPILED") {
        let program = format!("(load {artifact:?}) (assert (= (saved-function-literal) 42)) (write-line \"FUNCTION-LITERAL-OK\")");
        assert_eq!(run(&program, false), run(&program, true));
    } else {
        assert!(stdout.contains("FUNCTION-LITERAL-REJECTED"));
        // EGCL aborts before its writer; SBCL may emit a FASL that signals
        // the compilation error when loaded, accompanied by FAILURE-P.
        if stdout.contains("FUNCTION-LITERAL-EGCL") {
            assert!(!artifact.exists(), "EGCL rejected before artifact writing");
        }
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn saved_core_preserves_literal_function_identity_and_shared_capture() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-function-literal-image-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let image = directory.join("literal.core");
    let save = format!(
        r#"
#+egcl (progn
 (defparameter *image-function* (eval '(let ((state 40)) (lambda () (incf state)))))
 (defmacro image-function-value () *image-function*)
 (defmacro image-function-call () (list 'funcall *image-function*))
 (setf (symbol-function 'image-function-get) (compile nil '(lambda () (image-function-value))))
 (setf (symbol-function 'image-function-invoke) (compile nil '(lambda () (image-function-call))))
 (assert (eq (image-function-get) *image-function*))
 (assert (= (image-function-invoke) 41))
 (write-line "FUNCTION-LITERAL-OK")
 (egcl-ext:save-lisp-and-die {image:?}))
#-egcl (progn (write-line "FUNCTION-LITERAL-IMAGE-SKIPPED") (write-line "FUNCTION-LITERAL-OK"))
"#
    );
    let stdout = run(&save, false);
    if !stdout.contains("FUNCTION-LITERAL-IMAGE-SKIPPED") {
        assert!(image.exists());
        run_with_image(
            r#"
(assert (eq (image-function-get) *image-function*))
(assert (= (image-function-invoke) 42))
(assert (= (funcall *image-function*) 43))
(assert (eq (image-function-get) *image-function*))
(write-line "FUNCTION-LITERAL-OK")"#,
            true,
            Some(&image),
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
