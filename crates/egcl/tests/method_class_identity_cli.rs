// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::Path, process::Command};

const SETUP: &str = r#"
(defclass identity-parent () ())
(defclass identity-original (identity-parent)
  ((value :initarg :value :accessor identity-value)))
(defclass identity-child (identity-original) ())
(defgeneric identity-message (object))
(defmethod identity-message ((object identity-parent)) (list :parent))
(defmethod identity-message ((object identity-original))
  (cons :original (call-next-method)))
(defparameter *identity-old-class* (find-class 'identity-original))
(defparameter *identity-old-method*
  (find-method #'identity-message nil (list *identity-old-class*)))
(defparameter *identity-old* (make-instance 'identity-original :value 71))
(defparameter *identity-child* (make-instance 'identity-child :value 72))
(defun identity-native-call (object) (identity-message object))
(assert (equal (identity-native-call *identity-old*) '(:original :parent)))
;; Reusing a registration name must create a different class and method,
;; while the retained original method remains attached to its actual class.
(setf (find-class 'identity-original) nil)
(defclass identity-original (identity-parent)
  ((value :initarg :value :accessor identity-value)))
(defmethod identity-message ((object identity-original))
  (cons :replacement (call-next-method)))
(defparameter *identity-new-class* (find-class 'identity-original))
(defparameter *identity-new* (make-instance 'identity-original :value 81))
(defun identity-check ()
  (assert (not (eq *identity-old-class* *identity-new-class*)))
  (assert (eq *identity-old-method*
              (find-method #'identity-message nil (list *identity-old-class*))))
  (assert (not (eq *identity-old-method*
                   (find-method #'identity-message nil (list *identity-new-class*)))))
  (assert (eq *identity-old-class*
              (first (egcl-mop:method-specializers *identity-old-method*))))
  (assert (member *identity-old-method*
                  (egcl-mop:specializer-direct-methods *identity-old-class*)))
  (assert (not (member *identity-old-method*
                       (egcl-mop:specializer-direct-methods *identity-new-class*))))
  (assert (equal (identity-native-call *identity-old*) '(:original :parent)))
  (assert (equal (identity-native-call *identity-child*) '(:original :parent)))
  (assert (equal (identity-native-call *identity-new*) '(:replacement :parent)))
  (assert (= (identity-value *identity-old*) 71))
  (assert (= (identity-value *identity-child*) 72))
  (assert (= (identity-value *identity-new*) 81))
  (setf (identity-value *identity-old*) 73)
  (assert (= (identity-value *identity-old*) 73))
  (setf (identity-value *identity-old*) 71)
  t)
(identity-check)
;; A package-local class named INTEGER is not CL:INTEGER, even for immediate
;; arguments whose representation is normally handled by the built-in table.
(defpackage :identity-shadow (:use :cl) (:shadow #:integer))
(defclass identity-shadow::integer () ())
(defgeneric identity-shadow-call (object))
(defmethod identity-shadow-call ((object t)) :other)
(defmethod identity-shadow-call ((object identity-shadow::integer)) :shadow)
(assert (eq :other (identity-shadow-call 7)))
(assert (eq :shadow (identity-shadow-call (make-instance 'identity-shadow::integer))))
;; Aliases of the class T denote the same specializer as an unspecialized arg.
(setf (find-class 'identity-t-alias) (find-class 't))
(defgeneric identity-t-method (object))
(defmethod identity-t-method ((object identity-t-alias)) :first)
(defmethod identity-t-method (object) :second)
(assert (= 1 (length (egcl-mop:generic-function-methods #'identity-t-method))))
(assert (eq :second (identity-t-method 7)))
(assert (handler-case
    (progn (eval '(defmethod identity-undefined ((object identity-no-such-class)) :bad)) nil)
    (program-error () t)))
;; A foreign spelling cannot choose a different builtin predicate. Rebinding
;; the alias later must not retarget the stored INTEGER specializer either.
(defpackage :identity-builtin-alias (:use :cl) (:shadow #:string))
(setf (find-class 'identity-builtin-alias::string) (find-class 'integer))
(defgeneric identity-builtin-alias-call (object))
(defmethod identity-builtin-alias-call ((object t)) :other)
(defmethod identity-builtin-alias-call ((object identity-builtin-alias::string)) :integer)
(defun identity-alias-native-call (object) (identity-builtin-alias-call object))
(setf (find-class 'identity-builtin-alias::string) (find-class 'string))
(defun identity-alias-check ()
  (assert (eq :integer (identity-alias-native-call 7)))
  (assert (eq :other (identity-alias-native-call "not an integer")))
  (assert (find-method #'identity-builtin-alias-call nil (list (find-class 'integer)) nil))
  (assert (null (find-method #'identity-builtin-alias-call nil (list (find-class 'string)) nil))))
(identity-alias-check)

"#;

const READY: &str = r#"
(dotimes (i 120)
  (identity-native-call *identity-old*)
  (identity-native-call *identity-new*)
  (identity-alias-native-call 7)
  (identity-alias-native-call "not an integer"))
(loop repeat 200 until (eql 2 (egcl-ext:function-tier 'identity-native-call))
  do (identity-native-call *identity-old*) (sleep 0.01))
(loop repeat 200 until (eql 2 (egcl-ext:function-tier 'identity-alias-native-call))
  do (identity-alias-native-call 7) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'identity-alias-native-call)))
(assert (= 2 (egcl-ext:function-tier 'identity-native-call)))
(identity-check)
(identity-alias-check)
(format t "METHOD-CLASS-IDENTITY-OK~%")
"#;

fn command(tier: &str, stress: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init"])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_LAZY_COMPILE", "1")
        .env("EGCL_LAZY_THRESHOLD", "10")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
        .env("EGCL_T2_THREADS", "1");
    for key in [
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_GC_STRESS",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_POISON",
    ] {
        command.env_remove(key);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    command
}

fn run(command: &mut Command) -> (String, String) {
    let output = command
        .output()
        .expect("run method class identity regression");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

#[test]
fn methods_and_accessors_keep_their_specializer_after_name_reuse() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let tail = if tier == "t2" {
            READY
        } else {
            "(identity-check) (identity-alias-check) (format t \"METHOD-CLASS-IDENTITY-OK~%\")"
        };
        let (stdout, stderr) =
            run(command(tier, stress).args(["--eval", &format!("{SETUP}\n{tail}")]));
        assert!(
            stdout.contains("METHOD-CLASS-IDENTITY-OK"),
            "{stdout}\n{stderr}"
        );
        if tier == "t2" {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("IDENTITY-NATIVE-CALL")
                        && line.contains("optimized worker result installed with ")),
                "actual installed native helper required: {stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(expected, &stdout);
        } else {
            reference = Some(stdout);
        }
    }
}

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn lisp_path(path: &Path) -> String {
    format!("{:?}", path.to_str().unwrap())
}

#[test]
fn image_keeps_live_method_identity_but_fresh_fasl_resolves_at_load_time() {
    let directory =
        std::env::temp_dir().join(format!("egcl-method-class-identity-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let _fixture = Fixture(directory.clone());
    let source = directory.join("fresh-method.lisp");
    let fasl = directory.join("fresh-method.bfasl");
    let image = directory.join("identity.core");
    fs::write(
        &source,
        "(defmethod identity-fresh ((object identity-original)) :loaded)\n",
    )
    .unwrap();
    run(command("t2", false).args([
        "--eval",
        &format!(
            "{SETUP}\n(compile-file {} :output-file {})\n(egcl-ext:save-lisp-and-die {})",
            lisp_path(&source),
            lisp_path(&fasl),
            lisp_path(&image)
        ),
    ]));
    fs::remove_file(&source).unwrap();
    let check = format!(
        r#"
      (identity-check)
      (identity-alias-check)
      ;; Existing methods keep their exact classes across image restoration.
      ;; A fresh source definition still resolves its names at LOAD time.
      (setf (find-class 'identity-original) *identity-old-class*)
      (load {})
      (assert (eq :loaded (identity-fresh *identity-old*)))
      (assert (null (find-method #'identity-fresh nil (list *identity-new-class*) nil)))
      (setf (find-class 'identity-original) *identity-new-class*)
      (identity-check)
      (format t "METHOD-IDENTITY-IMAGE-OK~%")
    "#,
        lisp_path(&fasl)
    );
    let (stdout, stderr) = run(command("t2", false)
        .arg("--image")
        .arg(&image)
        .args(["--eval", &check]));
    assert!(
        stdout.contains("METHOD-IDENTITY-IMAGE-OK"),
        "{stdout}\n{stderr}"
    );
}
