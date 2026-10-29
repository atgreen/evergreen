// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::Path, process::Command};

const CONSTRUCTOR: &str = "(defun owner-construct (class) (make-instance class))";
const SETUP: &str = r#"
(defvar *owner-default-effects* 0)
(defvar *owner-init-effects* 0)
(let ((seed (list :old-seed)))
  (defclass owner-parent ()
    ((shared :allocation :class :initform (list :old-shared) :accessor owner-shared)
     (local :initform (progn (incf *owner-init-effects*) (copy-list seed)) :reader owner-local)
     (defaulted :initarg :defaulted :reader owner-defaulted))
    (:default-initargs :defaulted (progn (incf *owner-default-effects*) (copy-list seed)))))
(defclass owner-child (owner-parent) ())
(defparameter *owner-old-parent* (find-class 'owner-parent))
(defparameter *owner-old-child* (find-class 'owner-child))
(defparameter *owner-old-instance* (owner-construct *owner-old-parent*))
(defparameter *owner-child-instance* (owner-construct *owner-old-child*))
(defparameter *owner-old-shared* (owner-shared *owner-old-instance*))
(defparameter *owner-local-slot*
  (find 'local (egcl-mop:class-direct-slots *owner-old-parent*)
        :key #'egcl-mop:slot-definition-name))
(assert (eq *owner-old-shared* (owner-shared *owner-child-instance*)))
(setf (find-class 'owner-parent) nil)
(defclass owner-parent ()
  ((shared :allocation :class :initform (list :new-shared) :accessor owner-new-shared)
   (local :initform :new-local :reader owner-new-local)
   (defaulted :initarg :defaulted :reader owner-new-defaulted))
  (:default-initargs :defaulted :new-default))
(defparameter *owner-new-parent* (find-class 'owner-parent))
(defparameter *owner-new-instance* (owner-construct *owner-new-parent*))
(assert (not (eq *owner-old-parent* *owner-new-parent*)))
(assert (not (eq (owner-new-shared *owner-new-instance*) *owner-old-shared*)))
(defun owner-check-old (class)
  (let ((defaults *owner-default-effects*) (inits *owner-init-effects*))
    (let ((object (owner-construct class)))
      (assert (eq (class-of object) class))
      (assert (= (1+ defaults) *owner-default-effects*))
      (assert (= (1+ inits) *owner-init-effects*))
      (assert (equal (owner-local object) '(:old-seed)))
      (assert (equal (owner-defaulted object) '(:old-seed)))
      (assert (eq (owner-shared object) *owner-old-shared*)))))
(defun owner-check ()
  (owner-check-old *owner-old-parent*)
  (owner-check-old *owner-old-child*)
  (assert (eq *owner-local-slot*
    (find 'local (egcl-mop:class-direct-slots *owner-old-parent*)
          :key #'egcl-mop:slot-definition-name)))
  (assert (equal (funcall (egcl-mop:slot-definition-initfunction *owner-local-slot*)) '(:old-seed)))
  (assert (null (find-method #'owner-shared nil (list *owner-new-parent*) nil)))
  (assert (eq (owner-new-local *owner-new-instance*) :new-local))
  (assert (eq (owner-new-defaulted *owner-new-instance*) :new-default))
  (let ((fresh (owner-construct 'owner-parent)))
    (assert (eq (class-of fresh) *owner-new-parent*))
    (assert (eq (owner-new-shared fresh) (owner-new-shared *owner-new-instance*))))
  (slot-makunbound *owner-old-instance* 'shared)
  (assert (not (slot-boundp *owner-child-instance* 'shared)))
  (setf (owner-shared *owner-child-instance*) *owner-old-shared*)
  (assert (eq (slot-value *owner-old-instance* 'shared) *owner-old-shared*))
  (reinitialize-instance *owner-old-instance* :defaulted (list :reset))
  (assert (equal (owner-defaulted *owner-old-instance*) '(:reset)))
  t)
(owner-check)
"#;
const NATIVE: &str = r#"
(dotimes (i 100) (owner-construct *owner-old-child*) (owner-construct *owner-new-parent*))
(loop repeat 200 until (eql 2 (egcl-ext:function-tier 'owner-construct))
      do (owner-construct *owner-old-child*) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'owner-construct)))
(owner-check)
"#;

fn command(tier: &str, stress: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init"])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1");
    for name in [
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_GC_STRESS",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_POISON",
    ] {
        command.env_remove(name);
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
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

#[test]
fn old_classes_keep_initializers_shared_cells_and_slots_after_name_reuse() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let native = if tier == "t2" { NATIVE } else { "" };
        let (stdout, stderr) = run(command(tier, stress).args([
            "--eval",
            &format!("{CONSTRUCTOR}\n{SETUP}\n{native}\n(format t \"CLASS-OWNER-OK~%\")"),
        ]));
        assert!(stdout.contains("CLASS-OWNER-OK"), "{stdout}\n{stderr}");
        if tier == "t2" {
            assert!(
                stderr.lines().any(|line| line
                    .contains("OWNER-CONSTRUCT: optimized worker result installed with ")
                    && (line.contains("ordinary x86-64 Lisp T2 ABI")
                        || line.contains("mapped T2 ABI"))),
                "actual native constructor helper required: {stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
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

fn sections(bytes: &[u8]) -> Vec<u16> {
    assert!(bytes.len() >= 28, "truncated BFASL header");
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let end = bytes.len() - 4;
    let mut cursor = 24usize;
    let mut kinds = Vec::new();
    for _ in 0..count {
        assert!(cursor + 6 <= end, "truncated section header");
        let kind = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
        let length = u32::from_le_bytes(bytes[cursor + 2..cursor + 6].try_into().unwrap()) as usize;
        cursor = cursor.checked_add(6).unwrap().checked_add(length).unwrap();
        assert!(cursor <= end, "truncated section payload");
        kinds.push(kind);
    }
    kinds
}

#[test]
fn source_free_constructor_and_core_restore_keep_owner_and_cell_aliases() {
    let directory = std::env::temp_dir().join(format!("egcl-class-owner-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let _fixture = Fixture(directory.clone());
    let source = directory.join("constructor.lisp");
    let fasl = directory.join("constructor.bfasl");
    let core = directory.join("owner.core");
    fs::write(&source, CONSTRUCTOR).unwrap();
    run(command("t2", false).args([
        "--eval",
        &format!(
            "(compile-file {} :output-file {}) (load {}) {SETUP} (egcl-ext:save-lisp-and-die {})",
            lisp_path(&source),
            lisp_path(&fasl),
            lisp_path(&fasl),
            lisp_path(&core)
        ),
    ]));
    let kinds = sections(&fs::read(&fasl).unwrap());
    assert!(kinds.contains(&12), "must contain bytecode");
    assert!(!kinds.contains(&11), "must not contain source fallback");
    fs::remove_file(source).unwrap();
    let (stdout, stderr) = run(command("t2", false).arg("--image").arg(&core).args([
        "--eval",
        &format!(
            "(load {}) (owner-check) {NATIVE} (format t \"CLASS-OWNER-IMAGE-OK~%\")",
            lisp_path(&fasl)
        ),
    ]));
    assert!(
        stdout.contains("CLASS-OWNER-IMAGE-OK"),
        "{stdout}\n{stderr}"
    );
}
