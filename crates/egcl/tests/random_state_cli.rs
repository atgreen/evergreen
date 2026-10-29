// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const PROGRAM: &str = r##"
(declaim (notinline opaque-random-step opaque-default-random-step))
(defun opaque-random-step (limit state) (random limit state))
(defun opaque-default-random-step (limit) (random limit))
(let* ((state (make-random-state t)) (copy (make-random-state state))
       (key-hash (sxhash state)) (table (make-hash-table)))
  (assert (random-state-p state))
  (assert (eq (type-of state) 'random-state))
  (assert (eq (class-of state) (find-class 'random-state)))
  (assert (not (eq state copy)))
  (assert (not (equal state copy)))
  (assert (not (equalp state copy)))
  (assert (atom state))
  (assert (not (consp state)))
  (dolist (type '(cons list sequence number symbol array)) (assert (not (typep state type))))
  (assert (typep state 'atom))
  (assert (equal (multiple-value-list (subtypep '(and cons random-state) nil)) '(t t)))
  (assert (equal (multiple-value-list (subtypep '(and cons number) nil)) '(t t)))
  (assert (not (nth-value 0 (subtypep '(and cons list) nil))))
  (assert (handler-case (progn (car state) nil) (type-error () t)))
  (let ((fake (cons :random-state 123)))
    (assert (not (random-state-p fake)))
    (assert (handler-case (progn (random 10 fake) nil) (type-error () t)))
    (assert (handler-case (progn (make-random-state fake) nil) (type-error () t))))
  (setf (gethash state table) :retained)
  (dotimes (i 80)
    (assert (= (opaque-random-step 100000 state) (opaque-random-step 100000 copy))))
  (when *random-required-tier*
    (loop repeat 100 until (= *random-required-tier* (egcl-ext:function-tier 'opaque-random-step))
          do (assert (= (opaque-random-step 100000 state) (opaque-random-step 100000 copy)))
             (sleep 0.01))
    (assert (= *random-required-tier* (egcl-ext:function-tier 'opaque-random-step))))
  (dolist (limit (list 17 1.0f0 1.0d0 (ash 1 90)))
    (dotimes (i 16) (assert (eql (opaque-random-step limit state) (opaque-random-step limit copy)))))
  (let ((third (make-random-state copy)))
    (opaque-random-step 100000 state)
    (dotimes (i 16) (assert (= (random 100000 copy) (random 100000 third)))))
  (assert (= key-hash (sxhash state)))
  (assert (eq (gethash state table) :retained))
  (egcl-ext:gc)
  (assert (= key-hash (sxhash state)))
  (assert (eq (gethash state table) :retained))
  (dolist (seed (list 0 (1- (ash 1 64))))
    (let* ((fresh (egcl::%random-state-from-seed seed))
           (expected (make-random-state fresh)))
      (dolist (text (list (prin1-to-string fresh)
                          (let ((*print-readably* t) (*print-base* 16)) (write-to-string fresh))
                          (format nil "~S" fresh)))
        (let ((restored (read-from-string text)) (reference (make-random-state expected)))
          (assert (random-state-p restored))
          (assert (not (eq fresh restored)))
          (dotimes (i 20) (assert (= (random 10000 restored) (random 10000 reference))))))
      (dotimes (i 20) (assert (= (random 10000 fresh) (random 10000 expected))))))
  (let* ((*print-circle* t) (text (prin1-to-string (vector state state)))
         (loaded (read-from-string text)))
    (assert (eq (aref loaded 0) (aref loaded 1)))))

(let* ((*random-state* (make-random-state t))
       (copy1 (make-random-state)) (copy2 (make-random-state nil)))
  (assert (not (eq copy1 copy2)))
  (dotimes (i 20)
    (let ((expected (random 1000)))
      (assert (= expected (random 1000 copy1)))
      (assert (= expected (random 1000 copy2))))))
;; A DEFUN must consult the current dynamic binding rather than a captured
;; definition environment, including after its body has installed native code.
(let* ((global *random-state*) (global-copy (make-random-state global))
       (local (make-random-state t)) (expected (make-random-state local)))
  (let ((*random-state* local))
    (dotimes (i 80)
      (assert (= (opaque-default-random-step 100000) (random 100000 expected))))
    (when *random-required-tier*
      (loop repeat 100
            until (= *random-required-tier* (egcl-ext:function-tier 'opaque-default-random-step))
            do (assert (= (opaque-default-random-step 100000) (random 100000 expected)))
               (sleep 0.01))
      (assert (= *random-required-tier* (egcl-ext:function-tier 'opaque-default-random-step))))
    (dotimes (i 20)
      (assert (= (opaque-default-random-step 100000) (random 100000 expected)))))
  (assert (eq *random-state* global))
  (dotimes (i 20)
    (assert (= (random 100000 global) (random 100000 global-copy)))))
(write-line "OPAQUE-RANDOM-STATE-OK")
"##;

fn invoke(program: &str, stress: bool, tier: &str, image: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command.args(["--eval", program]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(key);
    }
    command
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_LAZY", "0")
        .env("EGCL_T2_THREADS", "1");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    command.output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn opaque_states_are_disjoint_and_compiled_random_preserves_independent_sequences() {
    let mut baseline = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let output = invoke(
            &format!("(defparameter *random-required-tier* {required}){PROGRAM}"),
            stress,
            tier,
            None,
        );
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("OPAQUE-RANDOM-STATE-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}

#[test]
fn random_state_literals_and_current_state_survive_source_free_load_and_image_restore() {
    let dir = std::env::temp_dir().join(format!("egcl-opaque-random-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("states.lisp");
    let fasl = dir.join("states.bfasl");
    let image = dir.join("states.core");
    let expected = dir.join("expected.lisp");
    fs::write(
        &source,
        r#"
(defparameter *compiled-undrawn*
 '#.(let ((state (make-random-state t))) (setq *undrawn-original* state) state))
(defparameter *compiled-states*
 '#.(let* ((state (make-random-state t)) (copy (make-random-state state)))
      (vector state state copy)))
"#,
    )
    .unwrap();
    success(&invoke(
        &format!(
            r#"(defvar *undrawn-original*)
(compile-file {source:?} :output-file {fasl:?})
(with-open-file (out {expected:?} :direction :output :if-exists :supersede)
 (write (loop repeat 32 collect (random 10000 *undrawn-original*)) :stream out))"#
        ),
        false,
        "interp",
        None,
    ));
    fs::remove_file(&source).unwrap();
    let checks = r#"
(let ((left (aref *compiled-states* 0)) (right (aref *compiled-states* 2)))
  (assert (random-state-p left))
  (assert (eq left (aref *compiled-states* 1)))
  (assert (not (eq left right)))
  (dotimes (i 30) (assert (= (random 10000 left) (random 10000 right)))))
"#;
    for stress in [false, true] {
        let loaded = invoke(
            &format!(
                "(load {fasl:?})
(with-open-file (in {expected:?})
 (dolist (value (read in)) (assert (= value (random 10000 *compiled-undrawn*)))))
{checks}(write-line \"RANDOM-LITERALS-OK\")"
            ),
            stress,
            "t2",
            None,
        );
        success(&loaded);
        assert!(String::from_utf8_lossy(&loaded.stdout).contains("RANDOM-LITERALS-OK"));
    }
    success(&invoke(
        &format!(
            r#"
(load {fasl:?})
(defparameter *image-undrawn* (make-random-state t))
(defparameter *saved-state* (make-random-state t))
(defparameter *saved-copy* (make-random-state *saved-state*))
(setq *random-state* *saved-state*)
(compile 'retained-state (list 'lambda nil (list 'quote *saved-state*)))
(egcl-ext:save-lisp-and-die {image:?})
"#
        ),
        false,
        "interp",
        None,
    ));
    for stress in [false, true] {
        let restored = invoke(
            &format!(
                r#"
{checks}
(let ((copy (make-random-state *image-undrawn*)))
 (dotimes (i 32) (assert (= (random 10000 *image-undrawn*) (random 10000 copy)))))
(assert (eq *saved-state* *random-state*))
(assert (eq *saved-state* (retained-state)))
(dotimes (i 30) (assert (= (random 10000 *saved-state*) (random 10000 *saved-copy*))))
(write-line "RANDOM-IMAGE-OK")
"#
            ),
            stress,
            "t2",
            Some(&image),
        );
        success(&restored);
        assert!(String::from_utf8_lossy(&restored.stdout).contains("RANDOM-IMAGE-OK"));
    }
    fs::remove_dir_all(dir).unwrap();
}
