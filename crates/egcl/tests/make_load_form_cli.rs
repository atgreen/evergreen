// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SHARED_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *mlf-save-calls* 0)
  (defvar *mlf-save-hook* (constantly nil))
  (defstruct mlf-node value next payload)
  (defmethod make-load-form ((object mlf-node) &optional environment)
    (incf *mlf-save-calls*)
    (funcall *mlf-save-hook*)
    (make-load-form-saving-slots object :environment environment)))
(defparameter *mlf-shared-result*
  #+mlf-host
  '#.(let ((node (make-mlf-node :value 42)))
       (setf (mlf-node-next node) node
             (mlf-node-payload node) (vector node (list node)))
       (list node node))
  #-mlf-host (error "An unselected source branch was read again"))
"#;

const CUSTOM_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *mlf-save-calls* 0)
  (defvar *mlf-save-hook* (constantly nil))
  (defvar *mlf-restore-hook* (constantly nil))
  (defparameter *mlf-restore-events* nil)
  (defstruct mlf-custom value next)
  (defmethod make-load-form ((object mlf-custom) &optional environment)
    (declare (ignore environment))
    (incf *mlf-save-calls*)
    (funcall *mlf-save-hook*)
    (let ((value (mlf-custom-value object)) (next (mlf-custom-next object)))
      (values
        `(let ((created (make-mlf-custom :value ,value)))
           (push (list :create ,value) *mlf-restore-events*)
           (funcall *mlf-restore-hook*)
           created)
        `(progn
           (assert (mlf-custom-p ',next))
           (setf (mlf-custom-next ',object) ',next)
           (push (list :initialize ,value) *mlf-restore-events*)
           (funcall *mlf-restore-hook*))))))
(defparameter *mlf-mutual-result*
  '#.(let ((left (make-mlf-custom :value 7))
           (right (make-mlf-custom :value 8)))
       (setf (mlf-custom-next left) right (mlf-custom-next right) left)
       (list left right left)))
"#;

const ERROR_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defstruct mlf-broken value)
  (defmethod make-load-form ((object mlf-broken) &optional environment)
    (declare (ignore object environment))
    (error "The user MAKE-LOAD-FORM method failed")))
(defparameter *mlf-broken-result* '#.(make-mlf-broken :value 1))
"#;

const ORDER_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *mlf-save-calls* 0)
  (defvar *mlf-save-hook* (constantly nil))
  (defparameter *mlf-order-events* nil)
  (defstruct mlf-ordered kind value dependency)
  (defmethod make-load-form ((object mlf-ordered) &optional environment)
    (declare (ignore environment))
    (incf *mlf-save-calls*)
    (funcall *mlf-save-hook*)
    (let ((dependency (mlf-ordered-dependency object)))
      (case (mlf-ordered-kind object)
        (:leaf
         (values '(make-mlf-ordered :kind :leaf)
                 `(progn (setf (mlf-ordered-value ',object) 1)
                         (push :initialize-leaf *mlf-order-events*))))
        (:dependency
         (values '(make-mlf-ordered :kind :dependency)
                 `(progn (assert (= (mlf-ordered-value ',dependency) 1))
                         (setf (mlf-ordered-value ',object) 42)
                         (push :initialize-dependency *mlf-order-events*))))
        (:consumer
         (values `(let ((created (make-mlf-ordered
                                   :kind :consumer
                                   :value (mlf-ordered-value ',dependency))))
                    (push :create-consumer *mlf-order-events*)
                    created)
                 `(setf (mlf-ordered-dependency ',object) ',dependency)))))))
(defparameter *mlf-ordered-result*
  '#.(let* ((leaf (make-mlf-ordered :kind :leaf :value 1))
            (dependency (make-mlf-ordered :kind :dependency :value 42 :dependency leaf)))
       (make-mlf-ordered :kind :consumer :value 42 :dependency dependency)))
"#;

// Force collection inside user callbacks, where the compiler and loader must
// keep their temporary graphs rooted. The portable fixtures themselves do not
// depend on an implementation's collector API.
const COLLECTOR_HOOK: &str = r#"
(lambda ()
  (let ((package (find-package "EGCL-EXT")))
    (when package (funcall (find-symbol "GC" package)))))
"#;

struct Fixture {
    directory: PathBuf,
    source: PathBuf,
    artifact: PathBuf,
}

impl Fixture {
    fn new(label: &str, source: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "egcl-make-load-form-{label}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let source_path = directory.join("literal.lisp");
        fs::write(&source_path, source).unwrap();
        Self {
            source: source_path,
            artifact: directory.join("literal.bfasl"),
            directory,
        }
    }

    fn compile(&self, minimum_calls: usize) {
        run(
            &format!(
                "(progn
                   (defvar *mlf-save-hook* {COLLECTOR_HOOK})
                   (let ((*features* (cons :mlf-host *features*)))
                     (multiple-value-bind (output warnings failure)
                         (compile-file {source:?} :output-file {artifact:?})
                       (declare (ignore output warnings)) (assert (not failure))))
                   (format t \"MAKE-LOAD-FORM-CALLS ~D~%\" *mlf-save-calls*)
                   (assert (>= *mlf-save-calls* {minimum_calls}))
                   (write-line \"MAKE-LOAD-FORM-OK\"))",
                source = self.source,
                artifact = self.artifact
            ),
            false,
        );
    }

    fn delete_source(&self) {
        fs::remove_file(&self.source).unwrap();
    }

    fn finish(self) {
        fs::remove_dir_all(self.directory).unwrap();
    }
}

fn run(program: &str, stress: bool) {
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
    let output = command.output().expect("run MAKE-LOAD-FORM regression");
    assert!(
        output.status.success(),
        "stress={stress}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("MAKE-LOAD-FORM-OK"));
}

#[test]
fn make_load_form_preserves_shared_cyclic_structure_and_read_features() {
    let fixture = Fixture::new("shared", SHARED_SOURCE);
    fixture.compile(1);
    fixture.delete_source();
    let program = format!(
        "(progn
           (setq *features* (remove :mlf-host *features*))
           (load {artifact:?})
           (assert (zerop *mlf-save-calls*))
           (let ((node (first *mlf-shared-result*)))
             (assert (mlf-node-p node))
             (assert (= (mlf-node-value node) 42))
             (assert (eq node (second *mlf-shared-result*)))
             (assert (eq node (mlf-node-next node)))
             (assert (eq node (aref (mlf-node-payload node) 0)))
             (assert (eq node (car (aref (mlf-node-payload node) 1)))))
           (write-line \"MAKE-LOAD-FORM-OK\"))",
        artifact = fixture.artifact
    );
    run(&program, false);
    run(&program, true);
    fixture.finish();
}

#[test]
fn make_load_form_honors_custom_creation_initialization_and_mutual_cycles() {
    let fixture = Fixture::new("custom", CUSTOM_SOURCE);
    fixture.compile(2);
    fixture.delete_source();
    let program = format!(
        "(progn
           (defvar *mlf-restore-hook* {COLLECTOR_HOOK})
           (load {artifact:?})
           (assert (zerop *mlf-save-calls*))
           (let ((left (first *mlf-mutual-result*)) (right (second *mlf-mutual-result*)))
             (assert (= (mlf-custom-value left) 7))
             (assert (= (mlf-custom-value right) 8))
             (assert (eq left (third *mlf-mutual-result*)))
             (assert (eq right (mlf-custom-next left)))
             (assert (eq left (mlf-custom-next right))))
           (let ((events (reverse *mlf-restore-events*)))
             (assert (= (length events) 4))
             (assert (= (count :create events :key #'first) 2))
             (assert (= (count :initialize events :key #'first) 2))
             (assert (= (position :initialize events :key #'first) 2)))
           (write-line \"MAKE-LOAD-FORM-OK\"))",
        artifact = fixture.artifact
    );
    run(&program, false);
    run(&program, true);
    fixture.finish();
}

#[test]
fn make_load_form_failure_is_not_reported_as_successful_compilation() {
    let fixture = Fixture::new("failure", ERROR_SOURCE);
    run(
        &format!(
            "(progn
               (let ((failed nil))
                 (handler-case
                     (multiple-value-bind (output warnings failure)
                         (compile-file {source:?} :output-file {artifact:?})
                       (declare (ignore output warnings)) (setq failed failure))
                   (error () (setq failed t)))
                 (format t \"MAKE-LOAD-FORM-FAILURE ~S~%\" failed)
                 (assert failed))
               (write-line \"MAKE-LOAD-FORM-OK\"))",
            source = fixture.source,
            artifact = fixture.artifact
        ),
        false,
    );
    fixture.finish();
}

#[test]
fn make_load_form_initializes_independent_dependencies_before_creation_use() {
    let fixture = Fixture::new("order", ORDER_SOURCE);
    fixture.compile(3);
    fixture.delete_source();
    let program = format!(
        "(progn
           (load {artifact:?})
           (assert (= (mlf-ordered-value *mlf-ordered-result*) 42))
           (let ((events (reverse *mlf-order-events*)))
             (assert (< (position :initialize-leaf events)
                        (position :initialize-dependency events)))
             (assert (< (position :initialize-dependency events)
                        (position :create-consumer events))))
           (write-line \"MAKE-LOAD-FORM-OK\"))",
        artifact = fixture.artifact
    );
    run(&program, false);
    run(&program, true);
    fixture.finish();
}

#[test]
fn make_load_form_uses_standard_hook_when_current_package_shadows_its_name() {
    let fixture = Fixture::new(
        "shadow",
        r#"
(defpackage "MLF-SHADOW" (:use "CL") (:shadow "MAKE-LOAD-FORM"))
(in-package "MLF-SHADOW")
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar cl-user::*mlf-save-calls* 0)
  (defstruct shadow-node value)
  (defun make-load-form (&rest arguments)
    (declare (ignore arguments))
    (error "The compiler called a package-local lookalike"))
  (defmethod cl:make-load-form ((object shadow-node) &optional environment)
    (incf cl-user::*mlf-save-calls*)
    (make-load-form-saving-slots object :environment environment)))
(defparameter cl-user::*mlf-shadow-result* '#.(make-shadow-node :value 42))
(in-package "CL-USER")
"#,
    );
    fixture.compile(1);
    fixture.delete_source();
    let program = format!(
        "(progn
           (load {artifact:?})
           (assert (= (funcall (find-symbol \"SHADOW-NODE-VALUE\" \"MLF-SHADOW\")
                              *mlf-shadow-result*) 42))
           (assert (zerop *mlf-save-calls*))
           (write-line \"MAKE-LOAD-FORM-OK\"))",
        artifact = fixture.artifact
    );
    run(&program, false);
    run(&program, true);
    fixture.finish();
}
