// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Methods defined in lexical bindings use the same captured locations as
//! escaping compiled closures, including shared mutation and method context.

use std::path::Path;
use std::process::Command;

const DEFINITIONS: &str = r#"
(defvar *lm-peer* nil)
(let ((held (list :seed)) (count 0))
  (setq *lm-peer* (lambda () (list held count)))
  (defmethod lm-mutate ((amount integer))
    (incf count amount)
    (list held count))
  (defmethod lm-read ((object t))
    (when (eq object :bad) (error "lexical method failure"))
    (list held count)))
(defmethod lm-next ((x t)) (list :base x))
(let ((tag (list :captured)))
  (defmethod lm-next ((x integer))
    (lambda () (list tag (call-next-method)))))
(let ((value (list :macro)))
  (macrolet ((lm-expansion () 'value))
    (defmethod lm-macro ((x t)) (list (lm-expansion) x))))
(let ((value (list :symbol-macro)))
  (symbol-macrolet ((lm-alias value))
    (defmethod lm-symbol-macro ((x t)) (list lm-alias x))))
"#;

const EXERCISE: &str = r#"
(assert (equal (lm-mutate 2) '((:seed) 2)))
(assert (equal (lm-read :ok) '((:seed) 2)))
(assert (equal (funcall *lm-peer*) '((:seed) 2)))
(assert (eq (car (lm-read :ok)) (car (funcall *lm-peer*))))
(assert (handler-case (progn (lm-read :bad) nil) (error () t)))
(assert (equal (lm-mutate 3) '((:seed) 5)))
(let ((continuation (lm-next 7)))
  (lm-next 99)
  (assert (equal (funcall continuation) '((:captured) (:base 7)))))
(assert (equal (lm-macro 8) '((:macro) 8)))
(assert (equal (lm-symbol-macro 9) '((:symbol-macro) 9)))
(format t "RESULT lexical-methods-ok~%")
"#;

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new("timeout");
    cmd.current_dir(dir)
        .args([
            "--kill-after=5",
            "600",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .args(args);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_T2_THRESHOLD",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_HANDOFF_CENSUS",
        "EGCL_NO_METHOD_COMPILE",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(envs.iter().copied());
    let output = cmd.output().expect("run lexical method probe");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "{args:?} {envs:?}: {stdout}\n{stderr}"
    );
    (stdout, stderr)
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "egcl-lexical-methods-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn lexical_methods_share_captures_and_escaped_method_context_in_every_tier() {
    let dir = scratch();
    let program = format!("(progn {DEFINITIONS} {EXERCISE})");
    for tier in ["interp", "t0", "t1", "t2"] {
        let (out, _) = run(&dir, &["--eval", &program], &[("EGCL_FORCE_TIER", tier)]);
        assert!(out.contains("RESULT lexical-methods-ok"), "{out}");
    }
    let census = dir.join("handoffs.txt");
    let census_name = census.to_str().unwrap();
    let (out, _) = run(
        &dir,
        &["--eval", &program],
        &[("EGCL_HANDOFF_CENSUS", census_name)],
    );
    assert!(out.contains("RESULT lexical-methods-ok"), "{out}");
    let table = std::fs::read_to_string(&census).expect("handoff census");
    assert!(
        !table.contains("call:method:nested-in-binding"),
        "methods must compile: {table}"
    );
    for stride in ["1", "7"] {
        run(
            &dir,
            &["--eval", &program],
            &[
                ("EGCL_FORCE_TIER", "t0"),
                ("EGCL_GC_STRESS", stride),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ],
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn lexical_method_captures_survive_an_image_round_trip() {
    let dir = scratch();
    let program = format!(
        "(progn {DEFINITIONS} (lm-mutate 4) (egcl-ext:save-lisp-and-die \"methods.core\"))"
    );
    run(&dir, &["--eval", &program], &[]);
    let (out, _) = run(
        &dir,
        &[
            "--image",
            "methods.core",
            "--eval",
            r#"(progn
      (assert (equal (lm-mutate 2) '((:seed) 6)))
      (assert (equal (funcall *lm-peer*) '((:seed) 6)))
      (assert (equal (funcall (lm-next 7)) '((:captured) (:base 7))))
      (assert (equal (lm-macro 8) '((:macro) 8)))
      (assert (equal (lm-symbol-macro 9) '((:symbol-macro) 9)))
      (format t "RESULT restored-methods-ok~%"))"#,
        ],
        &[],
    );
    assert!(out.contains("RESULT restored-methods-ok"), "{out}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn lexical_method_captures_outlive_the_defining_thread() {
    let dir = scratch();
    let program = r#"(progn
      (egcl-thread:join-thread
        (egcl-thread:make-thread
          (lambda ()
            (let ((held (list :thread-capture)))
              (defmethod lm-from-thread ((x t)) (list held x))))))
      (dotimes (i 50) (assert (equal (lm-from-thread i) (list '(:thread-capture) i))))
      (format t "RESULT thread-method-ok~%"))"#;
    let (out, _) = run(&dir, &["--eval", program], &[]);
    assert!(out.contains("RESULT thread-method-ok"), "{out}");
    std::fs::remove_dir_all(dir).ok();
}
