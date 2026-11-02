// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Top-level LET creators install compiled closures while retaining shared
//! lexical locations, definition identity and named-call semantics.

use std::path::Path;
use std::process::Command;

const DEFINITIONS: &str = r#"
(defvar *ld-peer* nil)
(let ((held (list :held)) (count 0))
  (defun ld-add (amount) (declare (fixnum amount)) (incf count amount) (list held count))
  (defun ld-read () "Read the shared counter." (list held count))
  (setq *ld-peer* (lambda () (list held count))))
(let () (defun ld-no-capture () :no-capture))
(defvar *ld-old* nil)
(let ((value (list :first))) (defun ld-version () value))
(setq *ld-old* #'ld-version)
(let* ((base :second) (value (list base))) (progn (defun ld-version () value)))
;; A prior public definition permits the new closure's named recursive call.
(defun ld-recursive (n) n)
(let ((offset 7))
  (defun ld-recursive (n) (if (= n 0) offset (ld-recursive (1- n)))))
;; New recursive names that lowering cannot resolve keep the original path.
(let ((offset 8))
  (defun ld-new-recursive (n) (if (= n 0) offset (ld-new-recursive (1- n)))))
"#;

const EXERCISE: &str = r#"
(assert (equal (ld-add 2) '((:held) 2)))
(assert (equal (ld-read) '((:held) 2)))
(assert (equal (funcall *ld-peer*) '((:held) 2)))
(assert (eq (car (ld-read)) (car (funcall *ld-peer*))))
(assert (equal (funcall *ld-old*) '(:first)))
(assert (equal (ld-version) '(:second)))
(assert (not (eq *ld-old* #'ld-version)))
(assert (eq (nth-value 2 (function-lambda-expression #'ld-read)) 'ld-read))
(assert (eq (nth-value 2 (function-lambda-expression *ld-old*)) 'ld-version))
(assert (eq (nth-value 2 (function-lambda-expression #'ld-version)) 'ld-version))
(assert (eq (nth-value 2 (function-lambda-expression #'ld-no-capture)) 'ld-no-capture))
(assert (eq (ld-no-capture) :no-capture))
(assert (equal (documentation 'ld-read 'function) "Read the shared counter."))
(assert (= (ld-recursive 20) 7))
(assert (= (ld-new-recursive 20) 8))
(dotimes (i 100) (ld-add 1) (ld-read) (ld-version))
(assert (equal (ld-read) '((:held) 102)))
(format t "RESULT compiled-creators-ok~%")
"#;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "egcl-let-defuns-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("definitions.lisp"), DEFINITIONS).unwrap();
    std::fs::write(dir.join("exercise.lisp"), EXERCISE).unwrap();
    dir
}

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
        "EGCL_LOG",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_LET_DEFUN_COMPILATION",
        "EGCL_HANDOFF_CENSUS",
        "EGCL_BYTECODE_TRACE_NAMES",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(envs.iter().copied());
    let output = cmd.output().expect("run compiled creator probe");
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{args:?} {envs:?}: {out}\n{log}");
    (out, log)
}

#[test]
fn compiled_creators_preserve_captures_and_definitions_in_every_tier() {
    let dir = scratch();
    let program = "(progn (load \"definitions.lisp\") (load \"exercise.lisp\"))";
    for tier in ["interp", "t0", "t1", "t2"] {
        let (out, _) = run(&dir, &["--eval", program], &[("EGCL_FORCE_TIER", tier)]);
        assert!(out.contains("RESULT compiled-creators-ok"), "{out}");
    }
    for disabled in [false, true] {
        let envs = if disabled {
            vec![("EGCL_DISABLE_LET_DEFUN_COMPILATION", "1")]
        } else {
            vec![]
        };
        let (out, _) = run(&dir, &["--eval", program], &envs);
        assert!(out.contains("RESULT compiled-creators-ok"), "{out}");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn creator_compilation_runs_and_does_not_lazily_compile_its_defuns_against_host_frames() {
    let dir = scratch();
    let census = dir.join("census.txt");
    let (out, log) = run(
        &dir,
        &[
            "--eval",
            "(progn (load \"definitions.lisp\") (load \"exercise.lisp\"))",
        ],
        &[
            ("EGCL_LOG", "bytecode=trace"),
            ("EGCL_HANDOFF_CENSUS", census.to_str().unwrap()),
        ],
    );
    assert!(out.contains("RESULT compiled-creators-ok"), "{out}");
    assert!(
        log.contains("compiled let-defuns"),
        "creator must actually compile: {log}"
    );
    let table = std::fs::read_to_string(census).expect("handoff census");
    for name in ["LD-ADD", "LD-READ", "LD-VERSION"] {
        assert!(
            !table.contains(&format!("compile:defun-against-runtime-frame {name}")),
            "{table}"
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn compiled_creators_survive_gc_stress_and_fasl_loading() {
    let dir = scratch();
    let program = r#"(progn (load (compile-file "definitions.lisp")) (load "exercise.lisp"))"#;
    for stride in ["1", "7"] {
        let (out, _) = run(
            &dir,
            &["--eval", program],
            &[
                ("EGCL_FORCE_TIER", "t0"),
                ("EGCL_GC_STRESS", stride),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ],
        );
        assert!(out.contains("RESULT compiled-creators-ok"), "{out}");
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn compiled_creators_survive_image_restore() {
    let dir = scratch();
    run(
        &dir,
        &[
            "--eval",
            "(progn (load \"definitions.lisp\") (egcl-ext:save-lisp-and-die \"creators.core\"))",
        ],
        &[],
    );
    let (out, _) = run(
        &dir,
        &["--image", "creators.core", "--load", "exercise.lisp"],
        &[],
    );
    assert!(out.contains("RESULT compiled-creators-ok"), "{out}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn enclosing_declaration_policy_keeps_the_existing_definition_path() {
    let dir = scratch();
    let program = r#"(progn
      (defun ld-policy-helper () :helper)
      (let ((held :captured))
        (declare (notinline ld-policy-helper))
        (defun ld-policy () (list held (ld-policy-helper))))
      (dotimes (i 50) (assert (equal (ld-policy) '(:captured :helper))))
      (format t "RESULT declaration-policy-ok~%"))"#;
    let (out, log) = run(
        &dir,
        &["--eval", program],
        &[("EGCL_LOG", "bytecode=trace")],
    );
    assert!(out.contains("RESULT declaration-policy-ok"), "{out}");
    assert!(
        !log.contains("compiled let-defuns"),
        "declaration policy must retain DEFUN: {log}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn opaque_creator_effects_precede_the_single_definition_expansion() {
    let dir = scratch();
    // Both the initializer and preceding-body forms can change the macro
    // namespace. A failed speculative compile must not expand the DEFUN first.
    for opaque in [
        "(let ((ignored (eval '(defmacro ld-changing () (incf *ld-expansions*) 2)))) (defun ld-after-changing () (ld-changing)))",
        "(let () (eval '(defmacro ld-changing () (incf *ld-expansions*) 2)) (defun ld-after-changing () (ld-changing)))",
    ] {
        let program = format!(
            r#"(progn
              (defvar *ld-expansions* 0)
              (defmacro ld-changing () (incf *ld-expansions*) 1)
              {opaque}
              (assert (= (ld-after-changing) 2))
              (assert (= *ld-expansions* 1))
              (format t "RESULT ordered-definition-ok~%"))"#
        );
        for tier in ["interp", "t0", "t1", "t2"] {
            for disabled in [false, true] {
                let mut envs = vec![("EGCL_FORCE_TIER", tier), ("EGCL_LOG", "bytecode=trace")];
                if disabled {
                    envs.push(("EGCL_DISABLE_LET_DEFUN_COMPILATION", "1"));
                }
                let (out, log) = run(&dir, &["--eval", &program], &envs);
                assert!(out.contains("RESULT ordered-definition-ok"), "{out}");
                assert!(
                    !log.contains("compiled let-defuns"),
                    "opaque creator must retain DEFUN: {log}"
                );
            }
        }
    }
    std::fs::remove_dir_all(dir).ok();
}
