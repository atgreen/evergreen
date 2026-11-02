// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Only reusable portable closure subgraphs become eager; expansion and load
//! effects still happen at their original ordered invocation points.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_egcl");
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "egcl-eager-subgraphs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("subgraphs.lisp");
        fs::write(
            &source,
            r#"
          (defvar *es-expansions* 0)
          (defvar *es-defaults* 0)
          (defvar *es-loads* 0)
          (defvar *es-macro* nil)
          (defvar *es-compiler* nil)
          (defvar *es-setf* nil)
          (incf *es-loads*)
          (defmacro es-macro (&optional (seed (progn (incf *es-defaults*) 10)))
            (incf *es-expansions*)
            (setq *es-macro* (lambda (delta) (+ seed delta)))
            nil)
          (defun es-target (seed) seed)
          (define-compiler-macro es-target (&whole whole seed)
            (incf *es-expansions*)
            (setq *es-compiler* (lambda (delta) (+ seed delta)))
            whole)
          (define-setf-expander es-place (seed)
            (incf *es-expansions*)
            (setq *es-setf* (lambda (delta) (+ seed delta)))
            (values nil nil '(es-store) nil seed))
          (defun es-restart-factory ()
            (restart-case (invoke-restart 'save 40)
              (save (seed) (lambda (delta) (+ seed delta)))))
          (defparameter *es-thunk* (lambda (delta) (+ delta 50)))
          (defparameter *es-ltv* (load-time-value (lambda (delta) (+ delta 60))))
        "#,
        )
        .unwrap();
        let output = directory.join("subgraphs.fasl");
        run(
            &format!(
                "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
                source.to_str().unwrap(),
                output.to_str().unwrap(),
            ),
            &[],
        );
        // Loading must use the portable artifact rather than reconstruct its
        // definitions by reopening source after compilation.
        fs::remove_file(source).unwrap();
        Self(directory)
    }

    fn program(&self, tier: u8) -> String {
        let artifact = self.0.join("subgraphs.fasl");
        format!(
            r#"(progn
          (load {path:?})
          (assert (= *es-loads* 1))
          (assert (= *es-expansions* 0))
          (assert (= *es-defaults* 0))
          (assert (null *es-macro*))
          (assert (null *es-compiler*))
          (assert (null *es-setf*))
          (assert (null (macroexpand-1 '(es-macro))))
          (assert (= *es-expansions* 1))
          (let ((whole '(es-target 20)))
            (assert (eq (funcall (compiler-macro-function 'es-target) whole nil) whole)))
          (assert (= *es-expansions* 2))
          (multiple-value-bind (temps forms stores store access) (get-setf-expansion '(es-place 30))
            (assert (null temps)) (assert (null forms))
            (assert (equal stores '(es-store))) (assert (null store)) (assert (= access 30)))
          (assert (= *es-expansions* 3))
          (assert (= *es-defaults* 1))
          (dolist (function (list *es-macro* *es-compiler* *es-setf* *es-thunk* *es-ltv*))
            (assert (= (egcl-ext:function-invoke-count function) 0))
            (assert (= (egcl-ext:function-tier function) {tier})))
          (let ((restart (es-restart-factory)))
            (assert (= (egcl-ext:function-invoke-count restart) 0))
            (assert (= (egcl-ext:function-tier restart) {tier}))
            (assert (= (funcall restart 2) 42)))
          (defvar *es-saved-macro* *es-macro*)
          (assert (= (funcall *es-macro* 3) 13))
          (assert (= *es-defaults* 1))
          (assert (= (funcall *es-compiler* 2) 22))
          (assert (= (funcall *es-setf* 2) 32))
          (assert (= (funcall *es-thunk* 2) 52))
          (assert (= (funcall *es-ltv* 2) 62))
          (load {path:?})
          (assert (= *es-loads* 2))
          (assert (= *es-expansions* 3))
          (assert (= *es-defaults* 1))
          (macroexpand-1 '(es-macro 100))
          (assert (= *es-expansions* 4))
          (assert (not (eq *es-saved-macro* *es-macro*)))
          (assert (= (egcl-ext:function-invoke-count *es-macro*) 0))
          (assert (= (egcl-ext:function-tier *es-macro*) {tier}))
          (assert (= (funcall *es-saved-macro* 7) 17))
          (assert (= (funcall *es-macro* 7) 107))
          (format t "EAGER-SUBGRAPHS-OK~%"))"#,
            path = artifact.to_str().unwrap()
        )
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
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(name);
    }
    command.env("EGCL_T0_T1_THRESHOLD", "1000000");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .output()
        .expect("run eager portable-subgraph fixture");
    assert!(
        output.status.success(),
        "{envs:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn portable_subgraphs_keep_default_and_forced_t0_policy() {
    let fixture = Fixture::new();
    for flags in [
        vec![],
        vec![("EGCL_FASL_EAGER_T1", "1"), ("EGCL_FORCE_TIER", "t0")],
    ] {
        assert!(run(&fixture.program(0), &flags).contains("EAGER-SUBGRAPHS-OK"));
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn portable_expander_restart_and_thunk_subgraphs_are_eager_without_early_effects() {
    let fixture = Fixture::new();
    for flags in [
        vec![("EGCL_FASL_EAGER_T1", "1")],
        vec![
            ("EGCL_FASL_EAGER_T1", "1"),
            ("EGCL_GC_STRESS", "3"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    ] {
        assert!(run(&fixture.program(1), &flags).contains("EAGER-SUBGRAPHS-OK"));
    }
}
