// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(defvar *boa-effects* nil)
(defstruct (boa-unset (:constructor boa-unset-make (&aux a (b))))
  (a (progn (push :a *boa-effects*) 3) :type integer)
  (b (progn (push :b *boa-effects*) 4) :type integer))
(let ((object (boa-unset-make)))
  ;; Reading either slot before assignment is deliberately not tested.
  (assert (null *boa-effects*))
  (setf (boa-unset-a object) 7 (boa-unset-b object) 8)
  (assert (= 7 (boa-unset-a object)))
  (assert (= 8 (boa-unset-b object)))
  (assert (handler-case (progn (setf (boa-unset-a object) nil) nil)
            (type-error () t)))
  (assert (= 7 (boa-unset-a object))))
(defstruct (boa-explicit-nil (:constructor boa-explicit-nil-make (&aux (a nil))))
  (a 1 :type integer))
(assert (handler-case (progn (boa-explicit-nil-make) nil) (type-error () t)))
(defstruct (boa-explicit-good (:constructor boa-explicit-good-make
                              (&aux a
                               (temporary (progn (push :temporary *boa-effects*) 5))
                               (b (progn (push :b *boa-effects*) (+ temporary 1))))))
  (a (error "suppressed default") :type integer)
  (b 0 :type integer))
(let ((object (boa-explicit-good-make)))
  (assert (= 6 (boa-explicit-good-b object)))
  (setf (boa-explicit-good-a object) 9)
  (assert (= 9 (boa-explicit-good-a object))))
(assert (equal (reverse *boa-effects*) '(:temporary :b)))
(defstruct (boa-list-unset (:type list) (:constructor boa-list-unset-make (&aux a)))
  (a (error "suppressed list default") :type integer))
(let ((object (boa-list-unset-make)))
  (setf (boa-list-unset-a object) 10)
  (assert (= 10 (boa-list-unset-a object))))
(defstruct (boa-vector-unset (:type vector) (:constructor boa-vector-unset-make (&aux a)))
  (a (error "suppressed vector default") :type integer))
(let ((object (boa-vector-unset-make)))
  (setf (boa-vector-unset-a object) 11)
  (assert (= 11 (boa-vector-unset-a object))))
(defstruct (boa-optional-control (:constructor boa-optional-control-make (&optional a)))
  (a nil :type integer))
(assert (handler-case (progn (boa-optional-control-make) nil) (type-error () t)))
(defvar *boa-seen-nil* nil)
(defvar *boa-special-order* nil)
(defvar *boa-specialized-failures* nil)
(defstruct (boa-special-byte (:type (vector (unsigned-byte 8)))
  (:constructor boa-special-byte-make
    (&aux b (c (progn (push :c *boa-special-order*) 7))
            (later (progn (push (null b) *boa-seen-nil*) nil))
            (d (progn (push :d *boa-special-order*) 7))))
  (:constructor boa-special-byte-nil (&aux (b nil) (c 7) (d 7))))
  (b 7 :type (unsigned-byte 8)) (c 7 :type (unsigned-byte 8)) (d 7 :type (unsigned-byte 8)))
(setq *boa-special-order* nil)
(handler-case
    (let ((object (boa-special-byte-make)))
      (assert (eql (boa-special-byte-c object) 7))
      (assert (eql (boa-special-byte-d object) 7))
      (assert (equal (reverse *boa-special-order*) '(:c :d)))
      (setf (boa-special-byte-b object) 7)
      (assert (eql (boa-special-byte-b object) 7))
      (assert (car *boa-seen-nil*))
      (format t "GH804-SPECIALIZED-BYTE-OK~%"))
  (error (c)
    (push :byte *boa-specialized-failures*)
    (format t "GH804-SPECIALIZED-BYTE-ERROR ~A~%" c)))
(assert (handler-case (progn (boa-special-byte-nil) nil) (type-error () t)))
(defstruct (boa-special-bit (:type (vector bit))
  (:constructor boa-special-bit-make
    (&aux b (c (progn (push :c *boa-special-order*) 1))
            (later (progn (push (null b) *boa-seen-nil*) nil))
            (d (progn (push :d *boa-special-order*) 1))))
  (:constructor boa-special-bit-nil (&aux (b nil) (c 1) (d 1))))
  (b 1 :type bit) (c 1 :type bit) (d 1 :type bit))
(setq *boa-special-order* nil)
(handler-case
    (let ((object (boa-special-bit-make)))
      (assert (eql (boa-special-bit-c object) 1))
      (assert (eql (boa-special-bit-d object) 1))
      (assert (equal (reverse *boa-special-order*) '(:c :d)))
      (setf (boa-special-bit-b object) 1)
      (assert (eql (boa-special-bit-b object) 1))
      (assert (car *boa-seen-nil*))
      (format t "GH804-SPECIALIZED-BIT-OK~%"))
  (error (c)
    (push :bit *boa-specialized-failures*)
    (format t "GH804-SPECIALIZED-BIT-ERROR ~A~%" c)))
(assert (handler-case (progn (boa-special-bit-nil) nil) (type-error () t)))
(defstruct (boa-special-single (:type (vector single-float))
  (:constructor boa-special-single-make
    (&aux b (c (progn (push :c *boa-special-order*) 1.25f0))
            (later (progn (push (null b) *boa-seen-nil*) nil))
            (d (progn (push :d *boa-special-order*) 1.25f0))))
  (:constructor boa-special-single-nil (&aux (b nil) (c 1.25f0) (d 1.25f0))))
  (b 1.25f0 :type single-float) (c 1.25f0 :type single-float) (d 1.25f0 :type single-float))
(setq *boa-special-order* nil)
(handler-case
    (let ((object (boa-special-single-make)))
      (assert (eql (boa-special-single-c object) 1.25f0))
      (assert (eql (boa-special-single-d object) 1.25f0))
      (assert (equal (reverse *boa-special-order*) '(:c :d)))
      (setf (boa-special-single-b object) 1.25f0)
      (assert (eql (boa-special-single-b object) 1.25f0))
      (assert (car *boa-seen-nil*))
      (format t "GH804-SPECIALIZED-SINGLE-OK~%"))
  (error (c)
    (push :single *boa-specialized-failures*)
    (format t "GH804-SPECIALIZED-SINGLE-ERROR ~A~%" c)))
(assert (handler-case (progn (boa-special-single-nil) nil) (type-error () t)))
(defstruct (boa-special-double (:type (vector double-float))
  (:constructor boa-special-double-make
    (&aux b (c (progn (push :c *boa-special-order*) 1.25d0))
            (later (progn (push (null b) *boa-seen-nil*) nil))
            (d (progn (push :d *boa-special-order*) 1.25d0))))
  (:constructor boa-special-double-nil (&aux (b nil) (c 1.25d0) (d 1.25d0))))
  (b 1.25d0 :type double-float) (c 1.25d0 :type double-float) (d 1.25d0 :type double-float))
(setq *boa-special-order* nil)
(handler-case
    (let ((object (boa-special-double-make)))
      (assert (eql (boa-special-double-c object) 1.25d0))
      (assert (eql (boa-special-double-d object) 1.25d0))
      (assert (equal (reverse *boa-special-order*) '(:c :d)))
      (setf (boa-special-double-b object) 1.25d0)
      (assert (eql (boa-special-double-b object) 1.25d0))
      (assert (car *boa-seen-nil*))
      (format t "GH804-SPECIALIZED-DOUBLE-OK~%"))
  (error (c)
    (push :double *boa-specialized-failures*)
    (format t "GH804-SPECIALIZED-DOUBLE-ERROR ~A~%" c)))
(assert (handler-case (progn (boa-special-double-nil) nil) (type-error () t)))
(assert (null *boa-specialized-failures*))
(write-line "BOA-AUX-UNINITIALIZED-OK")
"#;

#[test]
fn omitted_aux_defaults_suppress_initial_slot_checks_only() {
    let mut baseline = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", PROGRAM]);
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(key);
        }
        command.env("EGCL_FORCE_TIER", tier);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("BOA-AUX-UNINITIALIZED-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
