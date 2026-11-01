// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! More hot defun shapes that must leave the tree-walker (bliss-gg3i1.7.17).
//!
//! A lazy-compile bail keeps a function tree-walked for good, and every closure
//! it creates is then a tree-walker closure too. In SBCL's cross-compiler that
//! was %PROCLAIM, PARSE-1-DSD and CONSTANT-SPECIAL-FORM-P (DESTRUCTURING-BIND
//! with &WHOLE / &KEY / &AUX), RECORD-XREF, %DEFKNOWN and IR1-CONVERT-LAMBDA
//! (SETF of a GETF place), STRIP-LAMBDA-LIST and CAREFUL-CALL (HANDLER-CASE
//! with :NO-ERROR), and %CHECK-DEPRECATED-TYPE and SUB-COMPILE-FILE
//! (HANDLER-BIND on a compound condition type). Each shape below must reach T0
//! (FUNCTION-TIER >= 1) once hot, under lazy and eager compilation, with the
//! tree-walker's values at every forced tier.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (define-condition dc-err (error) ())
  (defstruct dc-box (plist nil))
  (defun dc-evenp-condition (c) (typep c 'dc-err))
  (defun dc-key (l)
    (destructuring-bind (a &key b (c 7 c-p) ((:dee d) 9)) l (list a b c c-p d)))
  (defun dc-key-allow (l) (destructuring-bind (&key a &allow-other-keys) l a))
  (defun dc-key-bad (l) (handler-case (destructuring-bind (&key a) l a) (error () :error)))
  (defun dc-whole (l)
    (destructuring-bind (&whole w a &optional (b 2) &rest r) l (list w a b r)))
  (defun dc-aux (l) (destructuring-bind (a &aux (b (* a 2)) c) l (list a b c)))
  (defun dc-getf-var (k v) (let ((pl (list :a 1))) (setf (getf pl k) v) pl))
  (defun dc-getf-slot (box k v) (setf (getf (dc-box-plist box) k) v) (dc-box-plist box))
  (defun dc-getf-order (box)
    (let ((log nil))
      (setf (getf (dc-box-plist box) (progn (push 1 log) :k) (progn (push 2 log) nil))
            (progn (push 3 log) 5))
      (reverse log)))
  (defun dc-no-error (f)
    (handler-case (funcall f)
      (dc-err (c) (declare (ignore c)) (values :caught 2))
      (:no-error (&rest vs) (list :ok vs))))
  (defun dc-no-error-2 (x) (handler-case (floor x 3) (error () :e) (:no-error (q r) (list q r))))
  (defun dc-bind-or (k)
    (block nil
      (handler-bind (((or dc-err warning) (lambda (c) (return (list :hb (type-of c))))))
        (case k
          (0 (error 'dc-err))
          (1 (warn "w") :warned)
          (t (signal 'simple-condition :format-control "x") :none)))))
  (defun dc-bind-satisfies (k)
    (block nil
      (handler-bind (((satisfies dc-evenp-condition)
                      #'(lambda (c) (declare (ignore c)) (return :sat))))
        (signal 'dc-err)
        k)))
  (let ((box (make-dc-box))
        (names '(dc-key dc-key-allow dc-key-bad dc-whole dc-aux dc-getf-var dc-getf-slot
                 dc-getf-order dc-no-error dc-no-error-2 dc-bind-or dc-bind-satisfies)))
    (dotimes (i 30)
      (dc-key '(1 :b 2)) (dc-key-allow '(:a 1 :z 2)) (dc-key-bad '(:z 1)) (dc-whole '(1))
      (dc-aux '(3)) (dc-getf-var :b i) (dc-getf-slot box :x i) (dc-getf-order box)
      (dc-no-error (lambda () 1)) (dc-no-error-2 i) (dc-bind-or (mod i 3)) (dc-bind-satisfies i))
    (format t "TIERS ~S~%"
            (mapcar (lambda (n) (list n (egcl-ext:function-tier n))) names))
    (format t "VALUES ~S~%"
            (list (dc-key '(1 :b 2)) (dc-key '(1 :c nil :dee 4)) (dc-key-allow '(:a 1 :z 2))
                  (dc-key-bad '(:z 1)) (dc-key-bad '(:a)) (dc-key-bad '(:z 1 :allow-other-keys t))
                  (dc-whole '(1)) (dc-whole '(1 2 3 4)) (dc-aux '(3))
                  (dc-getf-var :b 5) (dc-getf-var :a 5) (dc-getf-slot box :y 1)
                  (dc-getf-order box)
                  (dc-no-error (lambda () (values 1 2)))
                  (multiple-value-list (dc-no-error (lambda () (error 'dc-err))))
                  (dc-no-error-2 7) (dc-bind-or 0) (dc-bind-or 1) (dc-bind-or 2)
                  (dc-bind-satisfies 3)))))
"#;

const EXPECTED_VALUES: &str = "VALUES ((1 2 7 NIL 9) (1 NIL NIL T 4) 1 :ERROR :ERROR NIL \
((1) 1 2 NIL) ((1 2 3 4) 1 2 (3 4)) (3 6 NIL) (:B 5 :A 1) (:A 5) (:Y 1 :K 5 :X 29) (1 2 3) \
(:OK (1 2)) (:CAUGHT 2) (2 1) (:HB DC-ERR) (:HB SIMPLE-WARNING) :NONE :SAT)";

fn run(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", PROGRAM]);
    for name in [
        "EGCL_LAZY_COMPILE",
        "EGCL_LAZY_THRESHOLD",
        "EGCL_FORCE_TIER",
        "EGCL_T2",
        "EGCL_PROFILING_DISABLED",
        "EGCL_LOG",
    ] {
        cmd.env_remove(name);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "egcl failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check(envs: &[(&str, &str)]) {
    let out = run(envs);
    let tiers = out
        .lines()
        .find(|l| l.starts_with("TIERS "))
        .unwrap_or_else(|| panic!("no TIERS line: {out}"));
    for entry in tiers
        .trim_start_matches("TIERS ((")
        .trim_end_matches("))")
        .split(") (")
    {
        let (name, tier) = entry.rsplit_once(' ').expect("name tier");
        let tier: i64 = tier
            .parse()
            .unwrap_or_else(|_| panic!("tier of {name}: {tier}"));
        assert!(
            tier >= 1,
            "{name} stayed tree-walked (tier {tier}) under {envs:?}: {out}"
        );
    }
    assert!(
        out.lines().any(|l| l == EXPECTED_VALUES),
        "values changed under {envs:?}: {out}"
    );
}

#[test]
fn destructuring_getf_no_error_and_compound_handler_shapes_leave_the_tree_walker() {
    check(&[]);
}

#[test]
fn the_same_shapes_compile_eagerly_too() {
    check(&[("EGCL_LAZY_COMPILE", "0")]);
}

#[test]
fn tree_walker_and_forced_tiers_give_the_same_values() {
    for tier in ["treewalk", "t0", "t1", "t2"] {
        let out = run(&[("EGCL_FORCE_TIER", tier)]);
        assert!(
            out.lines().any(|l| l == EXPECTED_VALUES),
            "values differ under EGCL_FORCE_TIER={tier}: {out}"
        );
    }
}
