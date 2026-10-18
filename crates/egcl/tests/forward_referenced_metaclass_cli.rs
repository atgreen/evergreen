// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A class named as a superclass BEFORE it is defined, whose own `DEFCLASS`
//! then names a slot-bearing `:METACLASS` (GitHub #384, bliss-5bg9m).
//!
//! The placeholder metaobject is allocated by `resolve_superclass_metaobject`
//! at the point the forward reference is seen, when the metaclass is not yet
//! knowable — so it is a zero-slot instance of `STANDARD-CLASS`. A class
//! metaobject's slot cells are fixed at allocation, so the real `DEFCLASS`
//! could not simply repoint its wrapper at a metaclass needing more of them:
//! `repoint_class_metaclass` refused, and refused SILENTLY. `CLASS-OF`
//! answered the metaclass (recorded separately) while the OBJECT was still a
//! `STANDARD-CLASS` instance, so every question answered from the object
//! disagreed with `CLASS-OF`.
//!
//! This is not a corner: Gendl reaches it on every `DEFINE-OBJECT`, because
//! `VANILLA-MIXIN` is appended to every superclass list and is itself defined
//! late.
//!
//! The fix migrates rather than refusing — a replacement that IS an instance
//! of the metaclass, the registries rehashed onto it, and the old object
//! forwarded to it. The rows below are what a class defined in the ordinary
//! order has always answered; the point is that a forward-referenced one now
//! answers identically.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// `LATE` is named as `LATE-SUB`'s superclass one form before its own
/// `DEFCLASS`, and `PRE` is specialized on the placeholder — dispatch compares
/// specializers by identity, so a migration that lost that reference would
/// leave `PRE` inapplicable to every instance of the defined class.
///
/// `ORDINARY` is the control: the same shape with no forward reference. Every
/// probe is asked of both, and of a compiled function as well as a top-level
/// form, because a check that holds in the tree-walking evaluator does not
/// cover `cli/bytecode.rs` (memory
/// `acceptance-eval-wrapper-hides-compiled-path`).
const PROGRAM: &str = r#"
  (defclass meta (standard-class) ((x :initform 1 :accessor meta-x)))
  (defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
  (defgeneric on-meta (c))
  (defmethod on-meta ((c meta)) :via-metaclass)
  (defmethod pre ((o late)) :pre)
  (defclass late-sub (late) () (:metaclass meta))
  ;; An instance of the PLACEHOLDER, made before the migration. Its wrapper was
  ;; minted before the replacement existed and is obsolete afterwards, so no
  ;; scanner and no registry rewrite can reach the class copy inside it --
  ;; `class_of` chasing forwarding is the only thing that keeps this instance
  ;; attached to its class.
  (defvar *pre* (make-instance 'late))
  (defclass late () ((v :initform 7 :accessor lv)) (:metaclass meta))
  (defclass ordinary () ((v :initform 7 :accessor ov)) (:metaclass meta))

  (defun probe (c)
    (list (eq (class-of c) (find-class 'meta))
          (typep c 'meta)
          (class-name (class-of c))
          (slot-exists-p c 'x)
          (meta-x c)
          (on-meta c)))

  (defmacro report (label form) `(format t "~a ~a~%" ,label ,form))

  (report "DIRECT-LATE" (probe (find-class 'late)))
  (report "DIRECT-ORDINARY" (probe (find-class 'ordinary)))
  (defun compiled-late () (probe (find-class 'late)))
  (defun compiled-ordinary () (probe (find-class 'ordinary)))
  (report "COMPILED-LATE" (compiled-late))
  (report "COMPILED-ORDINARY" (compiled-ordinary))

  ;; The class's own wrapper carries a plain copy of its class value that
  ;; CLASS-OF reads without chasing forwarding, and every instance shares it.
  ;; An instance made AFTER the migration is what catches a stale copy.
  (defvar *i* (make-instance 'late))
  (report "INST-CLASS" (list (eq (class-of *i*) (find-class 'late))
                             (class-name (class-of *i*))
                             (typep *i* 'late)
                             (lv *i*)
                             (pre *i*)))
  ;; The subclass whose DEFCLASS created the placeholder must still name it.
  (report "SUBCLASS" (list (mapcar #'class-name
                                   (egcl-mop:class-direct-superclasses
                                     (find-class 'late-sub)))
                           (multiple-value-list (subtypep 'late-sub 'late))
                           (typep (make-instance 'late-sub) 'late)))
  (report "PRE-INST" (list (eq (class-of *pre*) (find-class 'late))
                           (class-name (class-of *pre*))
                           (typep *pre* 'late)))
  (report "DONE" :ok)
"#;

fn run_with(env: &[(&str, &str)], args: &[&str]) -> (String, String, bool) {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=10", "300", BIN]).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("run egcl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// Every row identical between the forward-referenced class and the control.
const EXPECTED: &[&str] = &[
    "DIRECT-LATE (T T META T 1 VIA-METACLASS)",
    "DIRECT-ORDINARY (T T META T 1 VIA-METACLASS)",
    "COMPILED-LATE (T T META T 1 VIA-METACLASS)",
    "COMPILED-ORDINARY (T T META T 1 VIA-METACLASS)",
    "INST-CLASS (T LATE T 7 PRE)",
    "SUBCLASS ((LATE) (T T) T)",
    // Answered (NIL NIL NIL) while `class_of` read the wrapper's class word
    // raw: the instance was orphaned from its own class by the migration.
    "PRE-INST (T LATE T)",
    "DONE OK",
];

#[test]
fn a_forward_referenced_class_becomes_an_instance_of_its_declared_metaclass() {
    let (stdout, stderr, ok) = run_with(&[], &["--no-init", "--eval", PROGRAM]);
    assert!(ok, "{stdout}\n{stderr}");
    for expected in EXPECTED {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}

/// The migration allocates and rehashes `class_meta`, which is keyed BY the
/// class value — and `visit_clos_roots` marks those keys through a copy and
/// asserts the copy comes back unchanged, because a `HashMap` key cannot be
/// rewritten without a rehash. Forwarding the old object while leaving it as a
/// key tripped that assertion under forced collections; rehashing outside the
/// collector is what keeps the keys unforwarded. `AFTER_INIT` skips the ~100k
/// bootstrap allocations, which would otherwise cost minutes.
#[test]
fn the_migration_survives_a_collection_on_every_allocation() {
    let (stdout, stderr, ok) = run_with(
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
        &["--no-init", "--eval", PROGRAM],
    );
    assert!(ok, "{stdout}\n{stderr}");
    assert!(
        !stderr.contains("needs rehashing"),
        "class registry key was left forwarded:\n{stderr}"
    );
    // Compared against the same rows, not merely checked for a crash: a value
    // orphaned before it is stored computes a quietly wrong answer with no
    // segfault at all.
    for expected in EXPECTED {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}
