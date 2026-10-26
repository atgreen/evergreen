// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn native_immediate_vector_stores_preserve_references_values_and_errors() {
    let program = r#"
      (declaim (notinline put-checked))
      (defun put-checked (vector index value) (setf (aref vector index) value))
      (let ((vector (make-array 3 :initial-element 0)))
        (dotimes (i 120) (put-checked vector 0 i))
        (assert (= 2 (egcl-ext:function-tier 'put-checked)))
        (let ((values (list 0 -1 1152921504606846975 -1152921504606846976
                            nil t :symbol #\x 1.5 2.5d0
                            (list :heap :reference) #'identity (ash 1 100))))
          (dolist (old values)
            (dolist (new values)
              (put-checked vector 0 old)
              (assert (eq (put-checked vector 0 new) new))
              (assert (eq (aref vector 0) new)))))
        ;; Repeated collections exercise an old holder receiving young values
        ;; and later replacing those references with immediate integers.
        (dotimes (i 20)
          (let ((young (list :young i)))
            (assert (eq (put-checked vector 1 young) young))
            (egcl-ext:gc)
            (assert (eq (aref vector 1) young))
            (assert (= (put-checked vector 1 i) i))))
        (put-checked vector 0 77)
        (let ((errors 0))
          (dolist (index '(-1 3 1.5))
            (handler-case (put-checked vector index 99)
              (error () (incf errors)))
            (assert (= (aref vector 0) 77)))
          (assert (= errors 3))
          (format t "VECTOR-STORES-OK ERRORS ~D~%" errors)))
    "#;
    let mut results = Vec::new();
    for enabled in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            command.env_remove("EGCL_DISABLE_IMMEDIATE_VECTOR_STORES");
        } else {
            command.env("EGCL_DISABLE_IMMEDIATE_VECTOR_STORES", "1");
        }
        let output = command.output().expect("run native vector stores");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("VECTOR-STORES-OK ERRORS 3"),
            "{stdout}\n{stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|line| line.contains("PUT-CHECKED:") && line.contains("T2 emitted;")),
            "setter did not execute an installed native artifact:\n{stderr}"
        );
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "store fast path changes Lisp behavior"
    );
}
