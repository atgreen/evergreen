// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! DEFSTRUCT construction through the MAKE-STRUCT opcode (bliss-gg3i1.7.2):
//! the same instances and errors in every tier, after a redefinition, and
//! from a compiled BFASL that encodes the opcode.

use std::process::Command;

const PROGRAM: &str = r#"
(defstruct ms-point (x 0) (y (list 1)))
(defstruct (ms-point3 (:include ms-point)) (z 3 :type fixnum))
(defstruct (ms-boa (:constructor ms-boa (a &optional (b (cons a a))))) a b)
(defun ms-make (i) (make-ms-point3 :x (list i) :y (vector i i) :z i))
(defun ms-raw (i) (egcl::%make-struct 'ms-point (cons i i) (list i)))
(defun ms-sum (n)
  (let ((s 0))
    (dotimes (i n s)
      (let ((p (ms-make i)) (r (ms-raw i)) (b (ms-boa i)))
        (incf s (+ (car (ms-point-x p)) (aref (ms-point-y p) 1) (ms-point3-z p)
                   (cdr (ms-point-x r)) (car (ms-point-y r))
                   (ms-boa-a b) (cdr (ms-boa-b b))))))))
(defun ms-unknown () (egcl::%make-struct 'ms-no-such-structure 1 2))
(dotimes (k 40) (ms-sum 50))
(format t "RESULT ~S ~S ~S ~S~%"
        (ms-sum 100)
        (ms-make 7)
        (handler-case (make-ms-point3 :z "no") (type-error () :type-error))
        (handler-case (ms-unknown) (error () :error)))
(defstruct ms-point (x 0) (y 0) (w 9))
(format t "REDEFINED ~S ~S~%" (ms-raw 4) (make-ms-point :x 1))
"#;

fn run(args: &[&str], tier: &str) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init").args(args);
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_T2",
    ] {
        command.env_remove(name);
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
        .filter(|line| line.starts_with("RESULT ") || line.starts_with("REDEFINED "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn constructors_agree_in_every_tier_and_from_bfasl() {
    let expected = run(&["--eval", &format!("(progn {PROGRAM})")], "interp");
    assert!(
        expected.starts_with(
            "RESULT 34650 #S(MS-POINT3 :X (7) :Y #(7 7) :Z 7) :TYPE-ERROR :ERROR\nREDEFINED "
        ),
        "{expected}"
    );
    for tier in ["t0", "t2", ""] {
        assert_eq!(
            run(&["--eval", &format!("(progn {PROGRAM})")], tier),
            expected,
            "tier {tier:?}"
        );
    }
    let dir = std::env::temp_dir().join(format!("egcl-make-struct-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("make-struct.lisp");
    let fasl = dir.join("make-struct.bfasl");
    std::fs::write(&source, PROGRAM).unwrap();
    run(
        &[
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
        "",
    );
    assert_eq!(run(&["--load", fasl.to_str().unwrap()], ""), expected);
    std::fs::remove_dir_all(dir).unwrap();
}
