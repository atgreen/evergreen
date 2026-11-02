// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Leaf helpers for the builtins an SBCL compile still sent through the
//! ordinary runtime veneer (bliss-gg3i1.7.15.10.2): ATOM, LISTP, SYMBOLP,
//! PLUSP, MINUSP, BIT, SBIT, GET, TYPEP and EGCL::%DECLARATION-TYPEP of a
//! structure name, and three-argument LOGAND/LOGIOR/LOGXOR. Every tier answers
//! exactly as the interpreter does, including every miss and error, under GC
//! stress too, and a T2 caller really answers them without the veneer.

use std::process::Command;

const PROGRAM: &str = r#"
(defstruct lp-node a)
(defstruct (lp-leaf (:include lp-node)) b)
(defstruct lp-other)
(deftype lp-small () '(integer 0 9))
(setf (get 'lp-sym :k) 7 (get 'lp-sym :n) nil)
(defun lp-preds (x)
  (list (atom x) (listp x) (symbolp x)))
(defun lp-signs (x) (list (plusp x) (minusp x)))
(defun lp-bit (v i) (list (sbit v i) (bit v i)))
(defun lp-get (s k) (get s k))
(defun lp-get-default (s k) (get s k :dflt))
(defun lp-get-values (s k) (multiple-value-list (get s k)))
(defun lp-typep (x ty) (typep x ty))
(defun lp-decl (x ty) (egcl::%declaration-typep x ty))
(defun lp-logic (a b c) (list (logand a b c) (logior a b c) (logxor a b c)))
(defun lp-try (f &rest args)
  (handler-case (apply f args) (error (e) (list :error (type-of e)))))
(defun lp-run ()
  (let* ((leaf (make-lp-leaf :a 1 :b 2))
         (node (make-lp-node :a 3))
         (other (make-lp-other))
         (closure (let ((z 1)) (lambda () z)))
         (bits (make-array 4 :element-type 'bit :initial-contents '(1 0 1 1)))
         (adjustable (make-array 4 :element-type 'bit :adjustable t
                                   :initial-contents '(0 1 0 1)))
         (big (expt 2 70))
         (result nil))
    (dotimes (i 60)
      (setf result
            (list (lp-try #'lp-preds '(1 2)) (lp-try #'lp-preds nil)
                  (lp-try #'lp-preds 'foo) (lp-try #'lp-preds 3)
                  (lp-try #'lp-preds "s") (lp-try #'lp-preds closure)
                  (lp-try #'lp-preds #'car) (lp-try #'lp-preds leaf)
                  (lp-try #'lp-signs 3) (lp-try #'lp-signs -3) (lp-try #'lp-signs 0)
                  (lp-try #'lp-signs big) (lp-try #'lp-signs (- big))
                  (lp-try #'lp-signs 1.5) (lp-try #'lp-signs -2/3)
                  (lp-try #'lp-signs 'x) (lp-try #'lp-signs #c(1 2))
                  (lp-try #'lp-bit bits 2) (lp-try #'lp-bit bits 1)
                  (lp-try #'lp-bit bits 4) (lp-try #'lp-bit bits -1)
                  (lp-try #'lp-bit adjustable 1) (lp-try #'lp-bit "abc" 0)
                  (lp-try #'lp-get 'lp-sym :k) (lp-try #'lp-get 'lp-sym :n)
                  (lp-try #'lp-get 'lp-sym :missing) (lp-try #'lp-get nil :k)
                  (lp-try #'lp-get 3 :k) (lp-try #'lp-get "s" :k)
                  (lp-try #'lp-get-default 'lp-sym :missing)
                  (lp-try #'lp-get-default 'lp-sym :k)
                  (lp-try #'lp-get-values 'lp-sym :k)
                  (lp-try #'lp-typep leaf 'lp-node) (lp-try #'lp-typep node 'lp-leaf)
                  (lp-try #'lp-typep leaf 'lp-leaf) (lp-try #'lp-typep other 'lp-node)
                  (lp-try #'lp-typep nil 'lp-node) (lp-try #'lp-typep 5 'lp-node)
                  (lp-try #'lp-typep '(1) 'lp-node) (lp-try #'lp-typep #\a 'lp-node)
                  (lp-try #'lp-typep 'lp-node 'lp-node)
                  (lp-try #'lp-typep 5 'fixnum) (lp-try #'lp-typep 5 'lp-small)
                  (lp-try #'lp-typep 50 'lp-small) (lp-try #'lp-typep leaf 'structure-object)
                  (lp-try #'lp-typep leaf nil) (lp-try #'lp-typep leaf t)
                  (lp-try #'lp-typep 5 '(values fixnum))
                  (lp-try #'lp-typep 5 'lp-no-such-type)
                  (lp-try #'lp-decl leaf 'lp-node) (lp-try #'lp-decl node 'lp-leaf)
                  (lp-try #'lp-decl nil 'lp-node) (lp-try #'lp-decl 5 'lp-small)
                  (lp-try #'lp-decl 5 '(or null lp-node))
                  (lp-try #'lp-logic 12 10 6) (lp-try #'lp-logic -1 7 3)
                  (lp-try #'lp-logic big 3 1) (lp-try #'lp-logic most-positive-fixnum
                                                     most-positive-fixnum 0)
                  (lp-try #'lp-logic 1 2 'x) (lp-try #'lp-logic 1.0 2 3))))
    result))
(format t "RESULT ~S~%" (lp-run))
(defun lp-hot (x n)
  (let ((c 0))
    (dotimes (i n c)
      (when (and (atom x) (plusp n) (get 'lp-sym :k) (lp-typep* x)) (incf c)))))
(defun lp-typep* (x) (typep x (if (symbolp x) 'lp-node 'lp-leaf)))
(dotimes (i 40) (lp-hot 'foo 100))
(egcl-ext:sprof-start 1)
(format t "HOT ~S~%" (lp-hot (make-lp-leaf) 2000))
(egcl-ext:sprof-stop)
"#;

fn run(tier: &str, stress: bool, census: Option<&std::path::Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .arg("--no-init")
        .args(["--eval", &format!("(progn {PROGRAM})")]);
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_T2",
        "EGCL_NO_LEAF_BUILTINS",
        "EGCL_BUILTIN_CENSUS",
    ] {
        command.env_remove(name);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    if let Some(path) = census {
        command.env("EGCL_BUILTIN_CENSUS", path);
    }
    if tier.is_empty() {
        command.env_remove("EGCL_FORCE_TIER");
    } else {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier {tier:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .filter(|line| line.starts_with("RESULT ") || line.starts_with("HOT "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn predicate_leaf_builtins_agree_with_the_interpreter_in_every_tier() {
    let expected = run("interp", false, None);
    assert!(expected.contains("HOT 2000"), "{expected}");
    assert!(expected.contains("(:ERROR TYPE-ERROR)"), "{expected}");
    for tier in ["t0", "t1", "t2", ""] {
        assert_eq!(run(tier, false, None), expected, "tier {tier:?}");
    }
    assert_eq!(run("t2", true, None), expected, "t2 under GC stress");
}

/// The hot T2 loop answers ATOM, PLUSP, GET and TYPEP without reaching the
/// runtime veneer: the census of veneer calls over that loop (the window
/// between SPROF-START and SPROF-STOP) stays far below its iterations.
#[test]
fn predicate_leaf_builtins_stay_out_of_the_runtime_veneer() {
    let dir = std::env::temp_dir().join(format!("egcl-leaf-preds-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let census = dir.join("census");
    let out = run("t2", false, Some(&census));
    assert!(out.contains("HOT 2000"), "{out}");
    let table =
        std::fs::read_to_string(format!("{}.window", census.display())).expect("census written");
    std::fs::remove_dir_all(&dir).ok();
    for name in [" ATOM |", " PLUSP |", " GET |", " TYPEP |"] {
        let veneer: u64 = table
            .lines()
            .filter(|line| line.contains(name))
            .flat_map(|line| line.split_whitespace())
            .filter_map(|part| part.strip_prefix("ordinary/"))
            .filter_map(|part| part.split_once('=').map(|(_, n)| n.parse::<u64>().unwrap()))
            .sum();
        assert!(
            veneer < 100,
            "{name} reached the veneer {veneer} times\n{table}"
        );
    }
}
