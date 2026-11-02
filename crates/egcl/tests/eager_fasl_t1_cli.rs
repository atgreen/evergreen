// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FASL definitions and their closure templates can enter T1 before invocation.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "egcl-eager-fasl-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("cold.lisp");
        fs::write(
            &source,
            r#"
            (defvar *eager-fasl-default-count* 0)
            (defun eager-fasl-cold (x) (+ x 1))
            (defun eager-fasl-default-factory ()
              (lambda (&optional (x (progn (incf *eager-fasl-default-count*) 17))) x))
            (defun eager-fasl-factory (seed)
              (lambda ()
                (lambda (amount) (incf seed amount))))
            ;; The captured GO requires a published named tag, which the
            ;; mapped native emitter declines; bytecode must still execute it.
            (defun eager-fasl-control (again)
              (tagbody retry
                (funcall (lambda ()
                           (when again (setq again nil) (go retry)))))
              :done)
            "#,
        )
        .unwrap();
        let artifact = directory.join("cold.fasl");
        run(
            &format!(
                "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
                source.to_str().unwrap(),
                artifact.to_str().unwrap()
            ),
            &[],
        );
        Self(directory)
    }

    fn artifact(&self) -> PathBuf {
        self.0.join("cold.fasl")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, envs: &[(&str, &str)]) -> String {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    for name in [
        "EGCL_FASL_EAGER_T1",
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2",
        "EGCL_DISABLE_T2",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(name);
    }
    // No invocation of these cold definitions should reach a normal threshold.
    command.env("EGCL_T0_T1_THRESHOLD", "1000000");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().expect("run egcl");
    assert!(
        output.status.success(),
        "{envs:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn program(artifact: &Path, expected_tier: u8) -> String {
    format!(
        r#"(progn
          (load {:?})
          (assert (= (egcl-ext:function-invoke-count 'eager-fasl-cold) 0))
          (assert (= (egcl-ext:function-tier 'eager-fasl-cold) {expected_tier}))
          (assert (= (eager-fasl-cold 41) 42))
          (assert (= (eager-fasl-cold 1/2) 3/2))
          (assert (= (egcl-ext:function-tier 'eager-fasl-control) 0))
          (assert (eq (eager-fasl-control t) :done))
          (assert (= *eager-fasl-default-count* 0))
          (let ((with-default (eager-fasl-default-factory)))
            (assert (= *eager-fasl-default-count* 0))
            (assert (= (funcall with-default) 17))
            (assert (= *eager-fasl-default-count* 1)))
          (let ((outer (eager-fasl-factory 10)))
            (assert (= (egcl-ext:function-invoke-count outer) 0))
            (assert (= (egcl-ext:function-tier outer) {expected_tier}))
            (let ((inner (funcall outer)))
              (assert (= (egcl-ext:function-invoke-count inner) 0))
              (assert (= (egcl-ext:function-tier inner) {expected_tier}))
              (assert (= (funcall inner 3) 13))
              (assert (= (funcall inner 7) 20))))
          (format t "EAGER-FASL-OK~%"))"#,
        artifact.to_str().unwrap()
    )
}

#[test]
fn fasl_default_and_forced_t0_remain_cold() {
    let fixture = Fixture::new();
    let program = program(&fixture.artifact(), 0);
    for envs in [
        vec![],
        vec![("EGCL_FASL_EAGER_T1", "0")],
        vec![("EGCL_FASL_EAGER_T1", "1"), ("EGCL_FORCE_TIER", "t0")],
        vec![
            ("EGCL_FASL_EAGER_T1", "1"),
            ("EGCL_PROFILING_DISABLED", "1"),
        ],
    ] {
        assert!(run(&program, &envs).contains("EAGER-FASL-OK"));
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn fasl_eager_t1_publishes_before_first_call_and_preserves_results() {
    let fixture = Fixture::new();
    let program = program(&fixture.artifact(), 1);
    assert!(run(&program, &[("EGCL_FASL_EAGER_T1", "1")]).contains("EAGER-FASL-OK"));
    assert!(
        run(
            &program,
            &[
                ("EGCL_FASL_EAGER_T1", "1"),
                ("EGCL_GC_STRESS", "3"),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ]
        )
        .contains("EAGER-FASL-OK")
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn eager_loaded_callable_survives_redefinition_and_reload() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = artifact.to_str().unwrap();
    let program = format!(
        r#"(progn
          (load {path:?})
          (defvar *eager-saved* (symbol-function 'eager-fasl-cold))
          (defun eager-fasl-cold (x) (+ x 100))
          (assert (= (eager-fasl-cold 41) 141))
          (assert (= (funcall *eager-saved* 41) 42))
          (load {path:?})
          (assert (not (eq *eager-saved* (symbol-function 'eager-fasl-cold))))
          (assert (= (egcl-ext:function-tier 'eager-fasl-cold) 1))
          (assert (= (egcl-ext:function-invoke-count 'eager-fasl-cold) 0))
          (assert (= (eager-fasl-cold 41) 42))
          (assert (= (funcall *eager-saved* 41) 42))
          (format t "EAGER-FASL-RELOAD-OK~%"))"#
    );
    assert!(run(&program, &[("EGCL_FASL_EAGER_T1", "1")]).contains("EAGER-FASL-RELOAD-OK"));
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn eager_nested_templates_keep_captures_and_retired_instances_across_reload() {
    let fixture = Fixture::new();
    let artifact = fixture.artifact();
    let path = artifact.to_str().unwrap();
    let program = format!(
        r#"(progn
          (load {path:?})
          (defvar *eager-old-inner* (funcall (eager-fasl-factory 10)))
          (assert (= (egcl-ext:function-tier *eager-old-inner*) 1))
          (assert (= (egcl-ext:function-invoke-count *eager-old-inner*) 0))
          (load {path:?})
          (let ((fresh (funcall (eager-fasl-factory 100))))
            (assert (not (eq fresh *eager-old-inner*)))
            (assert (= (egcl-ext:function-tier fresh) 1))
            (assert (= (egcl-ext:function-invoke-count fresh) 0))
            (assert (= (egcl-ext:function-invoke-count *eager-old-inner*) 0))
            (assert (= (funcall *eager-old-inner* 3) 13))
            (assert (= (funcall fresh 7) 107))
            (assert (= (funcall *eager-old-inner* 2) 15)))
          (format t "EAGER-FASL-NESTED-RELOAD-OK~%"))"#
    );
    for envs in [
        vec![("EGCL_FASL_EAGER_T1", "1")],
        vec![
            ("EGCL_FASL_EAGER_T1", "1"),
            ("EGCL_GC_STRESS", "3"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    ] {
        assert!(run(&program, &envs).contains("EAGER-FASL-NESTED-RELOAD-OK"));
    }
}
