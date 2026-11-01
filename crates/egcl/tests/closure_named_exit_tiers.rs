// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A closure's RETURN-FROM or GO to a block or tag of the function that
//! created it (bliss-gg3i1.7.11).
//!
//! SBCL's compiler uses this shape everywhere -- `(block nil (mapc (lambda (x)
//! (when ... (return x))) list))`, and DOLIST/LOOP bodies whose lambdas
//! return -- and it used to keep the whole enclosing function in T0: the
//! closure's `ReturnFromNamed` had no IR, and native code did not publish the
//! enclosing function's registered BLOCK for the closure to find. The mapped
//! tiers now register such a block like a CATCH (enter/leave requests, a
//! catch landing at its resume point) and start the closure's exit through a
//! transfer request, as THROW does.
//!
//! Every case runs at each forced tier and must print the interpreter's
//! answers. The cases are chosen for the ways the native exit goes wrong:
//! values carried by the exit, an `UNWIND-PROTECT` cleanup and a special
//! binding in an intermediate frame (restored before the block's code runs),
//! the block owner's own cleanup and binding, nested closures, recursion
//! (each closure must reach ITS activation's block, not a same-named one),
//! a local RETURN-FROM out of the registered block (which must unregister
//! it), a CATCH and a HANDLER-CASE beside it, and a closure returning to a
//! block that has already exited.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(defun each (fn list) (dolist (x list) (funcall fn x)))
(defvar *log* nil)
(defvar *sp* :outer)
(defun rf-find (list limit)
  (block nil
    (each (lambda (x) (when (> x limit) (return x))) list)
    :none))
(defun rf-values (list)
  (block found
    (each (lambda (x) (when (evenp x) (return-from found (values x (* x 10) :mv)))) list)
    (values nil nil nil)))
(defun guarded-each (fn list)
  (let ((*sp* :inner))
    (unwind-protect (each fn list)
      (push (list :cleanup *sp*) *log*))))
(defun rf-through (list)
  (block outer
    (guarded-each (lambda (x) (when (= x 3) (return-from outer (list :got x *sp*)))) list)
    :none))
(defun rf-own-cleanup (list)
  (block b
    (unwind-protect
         (let ((*sp* :mine))
           (each (lambda (x) (when (= x 2) (return-from b (list x *sp*)))) list))
      (push (list :own-cleanup *sp*) *log*))
    :none))
(defun rf-nested (lists)
  (block top
    (each (lambda (l)
            (each (lambda (x) (when (eq x :stop) (return-from top l))) l))
          lists)
    :none))
(defun go-find (list limit)
  (let ((r :none))
    (tagbody
       (each (lambda (x) (when (> x limit) (setq r x) (go out))) list)
       (setq r :normal)
     out)
    r))
(defun rf-rec (n)
  (block b
    (if (zerop n)
        (each (lambda (x) (return-from b (list :leaf x))) '(1 2))
        (let ((inner (rf-rec (1- n))))
          (each (lambda (x) (when (= x n) (return-from b (list n inner)))) (list n))))
    :fell))
(defun rf-local (x)
  (block b
    (let ((f (lambda () (return-from b :closure))))
      (when (eq x :closure) (funcall f))
      (when (eq x :local) (return-from b :local))
      :end)))
(defun rf-catch (list)
  (block b
    (catch 'tag
      (each (lambda (x) (when (= x 2) (throw 'tag :thrown))
                        (when (= x 4) (return-from b :returned))) list))))
(defun rf-handler (list)
  (block b
    (handler-case
        (each (lambda (x) (when (= x 3) (return-from b (list :x x)))) list)
      (error () :handled))))
;; A local RETURN out of a LET whose binding a closure captures leaves the
;; binding's child environment frame.
(defun rf-captured-let (list)
  (let ((fns nil))
    (dolist (x list)
      (let ((y (* x 2)))
        (push (lambda () y) fns)
        (when (> x 2) (return))))
    (mapcar #'funcall fns)))
(defun run-all ()
  (setq *log* nil)
  (list (rf-find '(1 2 3 4 5 6 7) 5)
        (rf-find '(1 2 3) 5)
        (multiple-value-list (rf-values '(1 3 4 5)))
        (multiple-value-list (rf-values '(1 3)))
        (rf-through '(1 2 3 4))
        *sp*
        (rf-own-cleanup '(1 2 3))
        *sp*
        (rf-nested '((1 2) (3 :stop 4) (5)))
        (rf-nested '((1 2)))
        (go-find '(1 2 3 4 5 6 7) 5)
        (go-find '(1 2 3) 5)
        (rf-rec 3)
        (rf-local :closure) (rf-local :local) (rf-local :end)
        (rf-catch '(1 2 3 4)) (rf-catch '(3 4))
        (rf-handler '(1 2 3 4)) (rf-handler '(1 2))
        (rf-captured-let '(1 2 3 4 5))
        (reverse *log*)))
(let ((first (run-all)))
  (dotimes (i (or (ignore-errors (parse-integer (egcl-ext:getenv "RF_ITERATIONS"))) 60))
    (let ((r (run-all)))
      (unless (equal r first)
        (format t "MISMATCH ~d ~s~%" i r))))
  (format t "RESULT ~s~%" first))
"#;

const EXPECTED: &str = "RESULT (6 :NONE (4 40 :MV) (NIL NIL NIL) (:GOT 3 :INNER) :OUTER (2 :MINE) \
:OUTER (3 :STOP 4) :NONE 6 :NORMAL (3 (2 (1 (:LEAF 1)))) :CLOSURE :LOCAL :END :THROWN :RETURNED \
(:X 3) NIL (6 4 2) ((:CLEANUP :INNER) (:OWN-CLEANUP :OUTER)))";

fn run(tier: Option<&str>, env: &[(&str, &str)], program: &str) -> (String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "tier={tier:?} env={env:?}: {stdout}\n{stderr}"
    );
    (stdout, stderr)
}

fn result_line(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or("")
}

#[test]
fn closure_exits_agree_at_every_tier() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let (stdout, stderr) = run(tier, &[], PROGRAM);
        assert!(
            !stdout.contains("MISMATCH"),
            "tier={tier:?}: {stdout}\n{stderr}"
        );
        assert_eq!(
            result_line(&stdout),
            EXPECTED,
            "tier={tier:?}:\n{stdout}\n{stderr}"
        );
    }
}

/// The interpreter's answer under a minor collection at every allocation,
/// with freed nursery poisoned, at the tiers that run the native exit.
#[test]
fn closure_exits_survive_gc_stress() {
    let stress = [
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ("RF_ITERATIONS", "12"),
    ];
    for tier in [None, Some("t1"), Some("t2")] {
        let (stdout, stderr) = run(tier, &stress, PROGRAM);
        assert!(
            !stdout.contains("MISMATCH"),
            "tier={tier:?}: {stdout}\n{stderr}"
        );
        assert_eq!(
            result_line(&stdout),
            EXPECTED,
            "tier={tier:?}:\n{stdout}\n{stderr}"
        );
    }
}

/// The functions that create the returning closures, and the closures
/// themselves, leave T0 (x86-64 Linux, where the mapped tiers carry the
/// exit). Before bliss-gg3i1.7.11 every one of these stayed interpreted.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn enclosing_functions_and_closures_leave_t0() {
    let program = format!(
        "{PROGRAM}
(defvar *closure* nil)
(defun keep-closure (list)
  (block b
    (setq *closure* (lambda (x) (when (> x 2) (return-from b x))))
    (each *closure* list)
    :none))
(dotimes (i 50) (keep-closure '(1 2 3)))
(format t \"TIERS ~s~%\"
        (mapcar #'egcl-ext:function-tier
                '(rf-find rf-values rf-through rf-own-cleanup rf-nested rf-rec
                  rf-local rf-catch rf-handler rf-captured-let keep-closure)))
(format t \"CLOSURE-TIER ~s~%\" (egcl-ext:function-tier *closure*))"
    );
    for tier in ["t1", "t2"] {
        let (stdout, stderr) = run(Some(tier), &[], &program);
        let line = |prefix: &str| {
            stdout
                .lines()
                .find(|line| line.starts_with(prefix))
                .unwrap_or_else(|| panic!("tier={tier}: no {prefix} in {stdout}\n{stderr}"))
                .to_owned()
        };
        let tiers = line("TIERS ");
        assert!(
            !tiers.contains(" 0") && !tiers.contains("(0"),
            "tier={tier}: an enclosing function stayed in T0: {tiers}"
        );
        let closure = line("CLOSURE-TIER ");
        assert!(
            !closure.ends_with(" 0") && !closure.contains("NIL"),
            "tier={tier}: the returning closure stayed in T0: {closure}"
        );
    }
}

/// A closure that returns to a block whose extent has ended must fail the same
/// way at every tier, without crashing or reaching an unrelated block. (CLHS
/// leaves the consequences undefined; SBCL signals CONTROL-ERROR, which EGCL
/// does not yet do at any tier: bliss-we6jw.)
#[test]
fn exit_to_an_exited_block_fails_alike_at_every_tier() {
    let program = r#"
(defun each (fn list) (dolist (x list) (funcall fn x)))
(defun make-escaper () (block b (lambda (x) (when x (return-from b :late)))))
(defun call-late (f) (block b (each f '(nil t)) :fell-through))
(let ((f (make-escaper)))
  (dotimes (i 30) (funcall f nil))
  (format t "BEFORE~%")
  (finish-output)
  (format t "AFTER ~s~%" (call-late f)))
"#;
    let mut outcomes = Vec::new();
    for tier in [None, Some("t0"), Some("t1"), Some("t2")] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--eval", program]);
        if let Some(tier) = tier {
            command.env("EGCL_FORCE_TIER", tier);
        }
        let out = command.output().expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // A clean Lisp-level failure, never a signal (SIGSEGV/SIGABRT).
        assert_eq!(
            out.status.code(),
            Some(1),
            "tier={tier:?}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("BEFORE"),
            "tier={tier:?}: {stdout}\n{stderr}"
        );
        // The stale exit must not land in CALL-LATE's same-named block.
        assert!(
            !stdout.contains("AFTER"),
            "tier={tier:?}: {stdout}\n{stderr}"
        );
        outcomes.push(stderr.contains("__RETURN_FROM__"));
    }
    assert!(
        outcomes.iter().all(|&o| o == outcomes[0]),
        "tiers disagree on the stale exit: {outcomes:?}"
    );
}
