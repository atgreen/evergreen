// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::PathBuf, process::Command};

const CAPTURE_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defparameter *toplevel-expansion-state* :before)
  (defmacro capture-expansion-state (name)
    (let ((captured *toplevel-expansion-state*))
      `(progn
         (eval-when (:compile-toplevel)
           (setq *toplevel-expansion-state* :after))
         (defun ,name () ',captured)))))
#+retained-expansion-host (capture-expansion-state retained-expansion-value)
#-retained-expansion-host (error "Original source was read in the load environment")
"#;

const REGISTRATION_SOURCE: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defparameter *toplevel-registration-state* nil)
  (defmacro register-expansion (name)
    ;; Registration belongs to emitted code. A later expansion must not
    ;; reinterpret the same call after its compile-time effect has run.
    (when *toplevel-registration-state*
      (error "Expansion encountered its own emitted registration marker"))
    `(progn
       (eval-when (:compile-toplevel)
         (setq *toplevel-registration-state* (cons :category :kind)))
       (defun ,name () :registered))))
#+retained-expansion-host (register-expansion retained-registration-value)
#-retained-expansion-host (error "Original source was read in the load environment")
"#;

fn run(program: &str, stress: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command
        .output()
        .expect("run top-level expansion regression");
    assert!(
        output.status.success(),
        "stress={stress}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("RETAINED-EXPANSION-OK"));
}

fn check(source: &str, label: &str, compile_assertions: &str, load_assertions: &str) {
    for stress in [false, true] {
        let directory: PathBuf = std::env::temp_dir().join(format!(
            "egcl-retained-expansion-{label}-{stress}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let source_path = directory.join("expansion.lisp");
        let artifact = directory.join("expansion.bfasl");
        fs::write(&source_path, source).unwrap();
        run(
            &format!(
                "(progn
                   (let ((*features* (cons :retained-expansion-host *features*)))
                     (multiple-value-bind (path warnings failure)
                         (compile-file {source_path:?} :output-file {artifact:?})
                       (declare (ignore warnings)) (assert path) (assert (not failure))))
                   {compile_assertions}
                   (write-line \"RETAINED-EXPANSION-OK\"))"
            ),
            stress,
        );
        fs::remove_file(source_path).unwrap();
        run(
            &format!(
                "(progn
                   (setq *features* (remove :retained-expansion-host *features*))
                   (load {artifact:?})
                   {load_assertions}
                   (write-line \"RETAINED-EXPANSION-OK\"))"
            ),
            stress,
        );
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn emitted_definition_retains_state_captured_before_compile_time_effect() {
    check(
        CAPTURE_SOURCE,
        "state",
        "(assert (eq *toplevel-expansion-state* :after))",
        "(assert (eq (retained-expansion-value) :before))
         (assert (eq *toplevel-expansion-state* :before))",
    );
}

#[test]
fn emitted_registration_does_not_reinterpret_the_original_macro_call() {
    check(
        REGISTRATION_SOURCE,
        "registration",
        "(assert (equal *toplevel-registration-state* '(:category . :kind)))",
        "(assert (eq (retained-registration-value) :registered))
         (assert (null *toplevel-registration-state*))",
    );
}
