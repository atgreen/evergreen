// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::PathBuf, process::Command};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("egcl-system-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, contents: &str) {
        fs::write(self.0.join(name), contents).unwrap();
    }
    fn run(&self, program: &str) {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .current_dir(&self.0)
            .args(["--no-init", "--eval", program])
            .output()
            .expect("run EGCL");
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("SYSTEM-CONSTRUCTION-PASS"),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn compile_file_separates_definition_and_explicit_compile_time_effects() {
    let sandbox = Sandbox::new("phases");
    sandbox.write(
        "phase.lisp",
        r##"
(defun phase-hidden () 41)
(defmacro phase-macro (x) `(+ ,x 1))
(defun phase-using-macro () (phase-macro 41))
(eval-when (:compile-toplevel) (defun phase-explicit () 73))
(defstruct phase-record (slot 19))
"##,
    );
    sandbox.run(
        r##"
(assert (not (fboundp 'phase-hidden)))
(assert (not (fboundp 'phase-using-macro)))
(multiple-value-bind (path warnings failure) (compile-file "phase.lisp" :verbose nil)
  (declare (ignore warnings))
  (assert path) (assert (not failure))
  (assert (not (fboundp 'phase-hidden)))
  (assert (not (fboundp 'phase-using-macro)))
  (assert (= (phase-explicit) 73))
  (assert (macro-function 'phase-macro))
  (load path :verbose nil :print nil))
(assert (= (phase-hidden) 41))
(assert (= (phase-using-macro) 42))
(assert (= (phase-record-slot (make-phase-record)) 19))
(format t "SYSTEM-CONSTRUCTION-PASS~%")
"##,
    );
}

#[test]
fn compile_file_and_load_honor_pathnames_readtables_and_output_options() {
    let sandbox = Sandbox::new("io");
    fs::create_dir(sandbox.0.join("nested")).unwrap();
    sandbox.write("nested/input.lisp", "(defun stream-compiled () 29)\n");
    sandbox.write("reader.lisp", "(defparameter *system-read-result* !good)\n");
    sandbox.write("print.lisp", ":load-value\n");
    sandbox.run(
        r##"
(let ((*default-pathname-defaults* (truename "nested/")))
  (with-open-file (source "input.lisp")
    (compile-file source :output-file "relative.fasl" :verbose nil))
  (assert (probe-file "relative.fasl"))
  (load "relative.fasl" :verbose nil)
  (assert (= (stream-compiled) 29))
  (with-open-file (destination "stream.fasl" :direction :output :if-exists :supersede)
    (compile-file "input.lisp" :output-file destination :verbose nil))
  (assert (probe-file "stream.fasl")))
(let ((*readtable* (copy-readtable nil)))
  (set-macro-character #\! (get-macro-character #\'))
  (assert (functionp (get-macro-character #\')))
  (assert (eq (get-macro-character #\!) (get-macro-character #\')))
  (with-input-from-string (stream "good")
    (assert (equal (funcall (get-macro-character #\') stream #\!) '(quote good))))
  (set-macro-character #\[ (get-macro-character #\())
  (assert (equal (read-from-string "[!good)") '((quote good))))
  (let ((cycle (read-from-string "#1=[!good . #1#)")))
    (assert (eq (cdr cycle) cycle))
    (assert (equal (car cycle) '(quote good))))
  (with-input-from-string (stream "!good)")
    (assert (equal (funcall (get-macro-character #\() stream #\[) '((quote good)))))
  (set-macro-character #\% (get-macro-character #\"))
  (assert (equal (read-from-string "%inside%") "inside"))
  (with-input-from-string (stream "inside%")
    (assert (equal (funcall (get-macro-character #\") stream #\%) "inside")))
  (load "reader.lisp" :verbose nil)
  (assert (eq *system-read-result* 'good))
  (setq *system-read-result* nil)
  (compile-file "reader.lisp" :verbose nil)
  (load "reader.fasl" :verbose nil)
  (assert (eq *system-read-result* 'good)))
(let ((*load-verbose* nil) (*load-print* nil) (*compile-verbose* nil))
  (let ((text (with-output-to-string (*standard-output*) (load "print.lisp" :verbose t))))
    (assert (find #\; text)))
  (let ((text (with-output-to-string (*standard-output*) (load "print.lisp" :print t))))
    (assert (search "LOAD-VALUE" text)))
  (let ((*load-print* t))
    (assert (search "LOAD-VALUE" (with-output-to-string (*standard-output*) (load "print.lisp")))))
  (let ((text (with-output-to-string (*standard-output*)
                (compile-file "print.lisp" :verbose t))))
    (assert (find #\; text)))
  (assert (string= "" (with-output-to-string (*standard-output*)
                         (load "print.lisp" :verbose nil :print nil)))))
(format t "SYSTEM-CONSTRUCTION-PASS~%")
"##,
    );
}

#[test]
fn require_honors_single_and_multiple_pathnames_and_existing_modules() {
    let sandbox = Sandbox::new("require");
    sandbox.write(
        "one.lisp",
        "(push :one *module-effects*) (provide \"SYSTEM-ONE\")\n",
    );
    sandbox.write("two-a.lisp", "(push :two-a *module-effects*)\n");
    sandbox.write(
        "two-b.lisp",
        "(push :two-b *module-effects*) (provide \"SYSTEM-TWO\")\n",
    );
    sandbox.run(
        r##"
(defvar *module-effects* nil)
(require "SYSTEM-ONE" #p"one.lisp")
(require "SYSTEM-ONE" #p"missing-file.lisp")
(require "SYSTEM-TWO" '(#p"two-a.lisp" #p"two-b.lisp"))
(assert (equal *module-effects* '(:two-b :two-a :one)))
(assert (handler-case (load) (program-error () t)))
(assert (handler-case (compile-file) (program-error () t)))
(assert (handler-case (load "one.lisp" :unknown t) (program-error () t)))
(format t "SYSTEM-CONSTRUCTION-PASS~%")
"##,
    );
}

#[test]
fn load_print_preserves_source_groups_in_portable_fasl() {
    let sandbox = Sandbox::new("print-groups");
    sandbox.write(
        "groups.lisp",
        r##"
(progn :first :last)
(macrolet ((group-value () :from-macro)) (group-value))
(eval-when (:load-toplevel :execute) :before :after)
(defpackage :system-print-group (:use :cl) (:intern "INTERNAL-A" "INTERNAL-B"))
(progn)
(progn :discard (eval-when (:compile-toplevel) :compile-only))
(eval-when (:compile-toplevel) :only-compile)
(defvar *system-print-group-token* 55)
"##,
    );
    sandbox.run(
        r##"
(let* ((*load-verbose* nil) (*load-print* nil)
       (source (with-output-to-string (*standard-output*)
                 (load "groups.lisp" :print t))))
  (compile-file "groups.lisp" :verbose nil :print nil)
  (let ((compiled (with-output-to-string (*standard-output*)
                    (load "groups.fasl" :print t))))
    (assert (string= source compiled))
    (assert (search "LAST" compiled))
    (assert (search "FROM-MACRO" compiled))
    (assert (search "AFTER" compiled))
    (assert (not (search "INTERNAL-A" compiled)))
    (assert (not (search "INTERNAL-B" compiled)))))
(format t "SYSTEM-CONSTRUCTION-PASS~%")
"##,
    );
    let bytes = fs::read(sandbox.0.join("groups.fasl")).unwrap();
    assert!(
        bytes.windows(4).any(|window| window == b"BBU\0"),
        "print grouping must exercise a portable bytecode unit"
    );
}
