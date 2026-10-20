// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Fixed-arity callers that name a function with `#'f` reach T2 (bliss-j3fv8).

use std::process::Command;

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

const PROGRAM: &str = r#"
  (defun s-apply (l) (apply #'+ l))
  (defun s-mapcar (l) (mapcar #'1+ l))
  (defun s-reduce (l) (reduce #'+ l))
  (defstruct pt x y)
  (defun s-struct-set (p) (setf (pt-x p) (1+ (pt-x p))))
  (defun lf-target (x) (list :old x))
  (defun lf-caller (x) (funcall #'lf-target (list x (make-list 3))))
  (defvar *p* (make-pt :x 0 :y 0))
  (dotimes (i 40)
    (s-apply '(1 2 3)) (s-mapcar '(1 2)) (s-reduce '(1 2 3)) (s-struct-set *p*)
    (lf-caller i))
  (format t "TIERS ~S~%"
    (mapcar #'egcl-ext:function-tier
            '(s-apply s-mapcar s-reduce s-struct-set lf-caller)))
  (format t "RESULT ~S~%"
    (list (s-apply '(1 2 3)) (s-mapcar '(1 2)) (s-reduce '(1 2 3))
          (s-struct-set *p*) (lf-caller 1)))
  ;; The installed caller looks the callable up on every call, so it sees a
  ;; redefinition, including a writer replaced through FDEFINITION.
  (setf (symbol-function 'lf-target) (lambda (x) (list :new x)))
  (setf (fdefinition '(setf pt-x))
        (lambda (value object) (setf (pt-y object) value) :writer))
  (format t "REDEFINED ~S~%" (list (lf-caller 2) (s-struct-set *p*) (pt-y *p*)))
"#;

fn run(envs: &[(&str, &str)]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .envs(envs.iter().copied())
        .output()
        .expect("run LoadFunction T2 regression");
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

#[test]
fn load_function_callers_match_every_tier_and_install_t2() {
    let expected_result = "RESULT (6 (2 3) 6 41 (:OLD (1 (NIL NIL NIL))))";
    let expected_redefined = "REDEFINED ((:NEW (2 (NIL NIL NIL))) :WRITER 42)";
    for (tier, native, installed) in [
        ("interp", "0", 0),
        ("t0", "0", 0),
        ("t1", "0", 1),
        ("t2", "0", 2),
        ("t2", "1", 2),
    ] {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        if tier == "t1" && !cfg!(target_arch = "x86_64") {
            continue;
        }
        let (stdout, _) = run(&[
            ("EGCL_FORCE_TIER", tier),
            ("EGCL_NATIVE_TRANSFER", native),
            ("EGCL_LAZY_COMPILE", "0"),
        ]);
        let tiers = format!("TIERS ({installed} {installed} {installed} {installed} {installed})");
        assert_eq!(
            line(&stdout, "TIERS"),
            tiers,
            "tier={tier}, native={native}"
        );
        assert_eq!(
            line(&stdout, "RESULT"),
            expected_result,
            "tier={tier}, native={native}"
        );
        assert_eq!(
            line(&stdout, "REDEFINED"),
            expected_redefined,
            "tier={tier}, native={native}"
        );
    }
}

/// Automatic promotion installs these bodies with the mapped T2 ABI instead
/// of declining them as an unsupported control-scope shape.
#[test]
fn automatic_promotion_installs_load_function_callers_at_t2() {
    if !MAPPED_LINUX {
        return;
    }
    let (stdout, stderr) = run(&[
        ("EGCL_T0_T1_THRESHOLD", "5"),
        ("EGCL_T2_THRESHOLD", "20"),
        ("EGCL_T2_THREADS", "1"),
        ("EGCL_T2_LOG", "stderr"),
    ]);
    assert_eq!(line(&stdout, "TIERS"), "TIERS (2 2 2 2 2)", "{stderr}");
    for name in [
        "S-APPLY",
        "S-MAPCAR",
        "S-REDUCE",
        "S-STRUCT-SET",
        "LF-CALLER",
    ] {
        assert!(
            stderr.contains(&format!(
                "{name}: optimized worker result installed with mapped T2 ABI"
            )),
            "{name} did not install at T2:\n{stderr}"
        );
        assert!(
            !stderr.contains(&format!("{name}: mapped T2 declined")),
            "{name} was declined:\n{stderr}"
        );
    }
}
