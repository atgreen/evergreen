// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A hot caller of a small saved helper reaches T2 (bliss-6988n). Mapped T2
//! reconstructs one logical frame per deopt point, so the inliner must not
//! clone a helper that can deoptimize into it: doing so made mapped T2 decline
//! the caller ("multi-frame reconstruction not admitted"), leaving it at T1.
//! Deopts and type errors inside the helper, and redefinitions of it, must
//! still produce the interpreter's results once the caller is native.

use std::process::Command;

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

const PROGRAM: &str = r#"
  (defun pick (x y) (declare (ignore x)) (car y))
  (defun drive (n) (let ((s 0) (c (list 1))) (dotimes (i n) (setq s (+ s (pick nil c)))) s))
  (defun pick-of (x y) (declare (ignore x)) (car y))
  (defun drive-on (n c) (let ((s 0)) (dotimes (i n) (setq s (+ s (pick-of nil c)))) s))
  (defvar *rounds* 200)
  (defun warm ()
    (dotimes (k *rounds*) (drive 1000) (drive-on 100 (list 1)))
    ;; T2 compiles in the background; keep running until both callers have
    ;; installed it, with a bound so a decline cannot hang the test.
    (dotimes (k *rounds*)
      (when (and (eql 2 (egcl-ext:function-tier 'drive))
                 (eql 2 (egcl-ext:function-tier 'drive-on)))
        (return))
      (drive 1000) (drive-on 100 (list 1))))
  (warm)
  (format t "TIERS ~S~%" (mapcar #'egcl-ext:function-tier '(drive drive-on)))
  (format t "RESULT ~S~%" (list (drive 1000) (drive-on 1000 (list 3))))
  (format t "NOT-A-LIST ~S~%"
    (handler-case (drive-on 10 7) (type-error (e) (list :type-error (type-error-datum e)))))
  (format t "NIL-CAR ~S~%"
    (handler-case (drive-on 10 nil) (type-error (e) (list :type-error (type-error-datum e)))))
  (format t "FLOAT ~S~%" (drive-on 4 (list 1.5)))
  (format t "OVERFLOW ~S~%" (drive-on 2 (list most-positive-fixnum)))
  (format t "AGAIN ~S~%" (list (drive 1000) (drive-on 1000 (list 3))))
  (defun pick (x y) (declare (ignore x)) (length y))
  (defun pick-of (x y) (declare (ignore x)) (cdr y))
  (format t "REDEFINED ~S~%" (list (drive 3) (drive-on 3 (cons 1 5))))
"#;

fn run(prefix: &str, envs: &[(&str, &str)]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", prefix, "--eval", PROGRAM])
        .envs(envs.iter().copied())
        .output()
        .expect("run inline-decline regression");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: {}\n{stdout}\n{stderr}",
        out.status
    );
    (stdout, stderr)
}

fn line<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix}: {stdout}"))
}

fn assert_results(stdout: &str, context: &str) {
    for (prefix, expected) in [
        ("RESULT", "RESULT (1000 3000)"),
        ("NOT-A-LIST", "NOT-A-LIST (:TYPE-ERROR 7)"),
        ("NIL-CAR", "NIL-CAR (:TYPE-ERROR NIL)"),
        ("FLOAT", "FLOAT 6.0"),
        ("OVERFLOW", "OVERFLOW 2305843009213693950"),
        ("AGAIN", "AGAIN (1000 3000)"),
        ("REDEFINED", "REDEFINED (3 15)"),
    ] {
        assert_eq!(line(stdout, prefix), expected, "{context}");
    }
}

#[test]
fn caller_of_deoptimizing_helper_reaches_t2() {
    if !MAPPED_LINUX {
        return;
    }
    let (stdout, stderr) = run("nil", &[("EGCL_T2_LOG", "stderr")]);
    assert!(
        !stderr.contains("multi-frame reconstruction not admitted"),
        "a caller was declined for an inlined helper's deopt point:\n{stderr}"
    );
    assert_eq!(line(&stdout, "TIERS"), "TIERS (2 2)", "{stderr}");
    assert_results(&stdout, &stderr);
}

#[test]
fn helper_deopts_match_the_interpreter() {
    // Warmup only matters to the tiers; keep the tree-walked run short.
    let (stdout, _) = run("(defvar *rounds* 1)", &[("EGCL_FORCE_TIER", "interp")]);
    assert_results(&stdout, "interpreter");
}
