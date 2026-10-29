// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, process::Command};

const READERS: &str = r##"
(defun recursive-list-reader (stream character)
  (declare (ignore character))
  (loop until (eql (peek-char t stream t nil t) #\))
        collect (read-preserving-whitespace stream t nil t) into objects
        finally (read-char stream) (return objects)))
(defun error-p (thunk)
  (handler-case (progn (funcall thunk) nil) (error () t)))
"##;
const CHECKS: &str = r##"
(let ((*readtable* (copy-readtable nil)))
  (set-macro-character #\( #'recursive-list-reader)
  ;; Label definitions and references span separate recursive READ calls.
  (let ((value (read-from-string "(#1=(42) #1#)")))
    (assert (eq (first value) (second value)))
    (assert (= (caar value) 42)))
  ;; An inner recursive read must leave an outer definition's placeholder live.
  (let ((value (read-from-string "#1=(#1#)"))) (assert (eq value (car value))))
  (let ((value (read-from-string "#1=(#2=(#1#) #2#)")))
    (assert (eq (first value) (second value)))
    (assert (eq value (caar value))))
  (let ((value (read-from-string "#1=#(#1#)"))) (assert (eq value (aref value 0))))
  (assert (error-p (lambda () (read-from-string "(#2# #2=(42))"))))
  (assert (error-p (lambda () (read-from-string "#1#"))))
  ;; Direct standard dispatch/ordinary macro functions inherit the active scope.
  (set-macro-character #\@
    (lambda (stream character)
      (declare (ignore character))
      (funcall (get-dispatch-macro-character #\# #\# nil) stream #\# 1)))
  (let ((value (read-from-string "(#1=(42) @)")))
    (assert (eq (first value) (second value))))
  (set-macro-character #\@
    (lambda (stream character)
      (declare (ignore character))
      (funcall (get-macro-character #\( nil) stream #\()))
  (let ((value (read-from-string "(#1=(42) @#1#))")))
    (assert (eq (first value) (car (second value)))))
  ;; Nested nonrecursive READ/READ-FROM-STRING have independent label tables,
  ;; including failed operations. They must restore the outer scope afterwards.
  (set-macro-character #\@
    (lambda (stream character)
      (declare (ignore character))
      (assert (error-p (lambda () (read-from-string "#1#"))))
      (assert (error-p (lambda () (read-from-string "#7=("))))
      (assert (error-p (lambda () (read (make-string-input-stream "#1#")))))
      (assert (= (read (make-string-input-stream "#1=73")) 73))
      (read stream t nil t)))
  (let* ((*readtable* (copy-readtable *readtable*))
         (value (read-from-string "(#1=(42) @#1#)")))
    (assert (eq (first value) (second value))))
  (assert (error-p (lambda () (read-from-string "#1#")))))
(write-line "RECURSIVE-LABELS-OK")
"##;

fn run(program: &str, stress: bool, marker: &str) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command.output().expect("run recursive reader labels");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout={stdout} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(marker));
    stdout
}

#[test]
fn recursive_reads_share_labels_and_independent_reads_restore_scopes() {
    let program = format!("{READERS}\n{CHECKS}");
    let normal = run(&program, false, "RECURSIVE-LABELS-OK");
    let stressed = run(&program, true, "RECURSIVE-LABELS-OK");
    assert_eq!(normal, stressed);
}

#[test]
fn custom_reader_labels_survive_compile_file_and_source_free_load() {
    let directory =
        std::env::temp_dir().join(format!("egcl-recursive-labels-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("labels.lisp");
    let artifact = directory.join("labels.bfasl");
    fs::write(&source, "(defun label-values () '(#1=(42) #1#))\n").unwrap();
    run(
        &format!(
            r#"{READERS}
      (let ((*readtable* (copy-readtable nil)))
        (set-macro-character #\( #'recursive-list-reader)
        (multiple-value-bind (path warnings failure)
            (compile-file {source:?} :output-file {artifact:?})
          (declare (ignore warnings)) (assert path) (assert (not failure))))
      (write-line "LABELS-COMPILED")"#
        ),
        false,
        "LABELS-COMPILED",
    );
    fs::remove_file(source).unwrap();
    let program = format!(
        r#"(progn (load {artifact:?})
        (let ((value (label-values)))
          (assert (eq (first value) (second value))) (assert (= (caar value) 42)))
        (write-line "LABELS-LOADED"))"#
    );
    assert_eq!(
        run(&program, false, "LABELS-LOADED"),
        run(&program, true, "LABELS-LOADED")
    );
    fs::remove_dir_all(directory).unwrap();
}
