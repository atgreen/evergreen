// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Methods whose lambda lists have `&rest` or `&key` (GH #958). A variadic
//! binder that evaluates no default forms runs no code, so the method body's
//! lowering alone shows whether it uses its method context, and a body that
//! does not skips building one. These methods do use it, through
//! CALL-NEXT-METHOD with and without arguments, NEXT-METHOD-P, a computed
//! `&key` default that calls CALL-NEXT-METHOD, `&aux` and
//! `&allow-other-keys`; the answers are SBCL 2.6.9's, at every tier after
//! warm-up.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defclass rk-a () ())
  (defclass rk-b (rk-a) ())
  (defgeneric rk-rest (x &rest args))
  (defmethod rk-rest ((x rk-a) &rest args) (list :a args))
  (defmethod rk-rest ((x rk-b) &rest args) (list :b args (call-next-method) (call-next-method x 9)))
  (defgeneric rk-key (x &key k))
  (defmethod rk-key ((x rk-a) &key (k :default)) (list :a k))
  (defmethod rk-key ((x rk-b) &key k) (list :b k (next-method-p) (call-next-method)))
  (defgeneric rk-plain (x &rest args))
  (defmethod rk-plain ((x rk-a) &rest args) (declare (ignore args)) :plain)
  (defgeneric rk-keydef (x &key k))
  (defmethod rk-keydef ((x rk-a) &key k) (list :a k))
  (defmethod rk-keydef ((x rk-b) &key (k (call-next-method))) (list :b k))
  (defgeneric rk-aux (x &rest r))
  (defmethod rk-aux ((x rk-a) &rest r &aux (n (length r))) n)
  (defgeneric rk-aok (x &key))
  (defmethod rk-aok ((x rk-a) &key &allow-other-keys) (next-method-p))
  (let ((a (make-instance 'rk-a)) (b (make-instance 'rk-b)) last)
    (dotimes (i 3000)
      (setq last (list (rk-rest b 1 2) (rk-rest a) (rk-key b :k 3) (rk-key b) (rk-key a)
                       (rk-plain a 1 2 3) (rk-keydef b) (rk-keydef b :k 5)
                       (rk-aux a 1 2 3) (rk-aok a :zz 1))))
    (let ((*print-pretty* nil))
      (format t "RESULT ~S~%" last))))
"#;

const EXPECTED: &str = "RESULT ((:B (1 2) (:A (1 2)) (:A (9))) (:A NIL) (:B 3 T (:A 3)) (:B NIL T (:A :DEFAULT)) (:A :DEFAULT) :PLAIN (:B (:A NIL)) (:B 5) 3 NIL)";

fn run(tier: Option<&str>) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", PROGRAM]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed
stdout: {stdout}
stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn rest_and_key_methods_keep_their_method_context_semantics() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let out = run(tier);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| {
                panic!(
                    "tier {tier:?}: no RESULT line in:
{out}"
                )
            });
        assert_eq!(line, EXPECTED, "tier {tier:?}");
    }
}
