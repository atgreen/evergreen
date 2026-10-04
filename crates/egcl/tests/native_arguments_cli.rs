// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn native_snapshots_recover_original_arguments_from_managed_homes() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (defun native-args-leaf (value)
            (prog1 (egcl-debug:list-backtrace :count 32) (length value)))
          (defparameter *native-args-leaf* #'native-args-leaf)
          (defun native-args-outer (value)
            (prog1 (funcall *native-args-leaf* value) (length value)))
          (dotimes (i 100) (native-args-outer '(warm)))
          (assert (= {installed} (egcl-ext:function-tier 'native-args-leaf)))
          (assert (= {installed} (egcl-ext:function-tier 'native-args-outer)))
          (format t "NATIVE-ARG-TIERS-CONFIRMED~%")
          (let* ((value (list "original" 42))
                 (frames (native-args-outer value))
                 (found 0))
            (dolist (frame frames)
              (when (member (getf frame :function)
                            '("COMMON-LISP-USER::NATIVE-ARGS-LEAF"
                              "COMMON-LISP-USER::NATIVE-ARGS-OUTER") :test #'equal)
                (incf found)
                (assert (getf frame :arguments-available-p))
                (assert (= 1 (length (getf frame :arguments))))
                (assert (eq value (car (getf frame :arguments))))))
            (assert (= 2 found)))
          (format t "NATIVE-ARGS-PASS~%")
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("NATIVE-ARG-TIERS-CONFIRMED"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("NATIVE-ARGS-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn overwritten_and_variadic_parameters_do_not_masquerade_as_original_arguments() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (defun changed-native-argument (value)
            (setq value (+ value 1))
            (prog1 (egcl-debug:list-backtrace :count 32) (1+ value)))
          (defun variadic-native-argument (&optional value)
            (prog1 (egcl-debug:list-backtrace :count 32) (1+ value)))
          (dotimes (i 100) (changed-native-argument 41) (variadic-native-argument 41))
          (assert (= {installed} (egcl-ext:function-tier 'changed-native-argument)))
          (assert (= {installed} (egcl-ext:function-tier 'variadic-native-argument)))
          (dolist (entry (list (cons "COMMON-LISP-USER::CHANGED-NATIVE-ARGUMENT"
                                    (changed-native-argument 41))
                              (cons "COMMON-LISP-USER::VARIADIC-NATIVE-ARGUMENT"
                                    (variadic-native-argument 41))))
            (let ((found nil))
              (dolist (frame (cdr entry))
                (when (equal (getf frame :function) (car entry))
                  (setq found t)
                  (assert (null (getf frame :arguments-available-p)))
                  (assert (null (getf frame :arguments)))))
              (assert found)))
          (format t "UNAVAILABLE-ARGS-PASS~%")
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("UNAVAILABLE-ARGS-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn loaded_bfasl_retains_native_argument_locations() {
    let directory =
        std::env::temp_dir().join(format!("egcl-native-arguments-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("native-arguments.lisp");
    let compiled = directory.join("native-arguments.bfasl");
    std::fs::write(
        &source,
        r#"
        (defun persisted-native-argument (value)
          (prog1 (egcl-debug:list-backtrace :count 32) (length value)))
    "#,
    )
    .unwrap();
    let compile = format!(
        "(compile-file {:?} :output-file {:?})",
        source.to_str().unwrap(),
        compiled.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &compile])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(compiled.is_file());
    // A fresh process cannot accidentally reuse the source compiler's registry.
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (assert (not (fboundp 'persisted-native-argument)))
          (load {:?})
          (dotimes (i 100) (persisted-native-argument '(warm)))
          (assert (= {installed} (egcl-ext:function-tier 'persisted-native-argument)))
          (let* ((value (list "loaded" 42))
                 (frames (persisted-native-argument value))
                 (found nil))
            (dolist (frame frames)
              (when (equal (getf frame :function) "COMMON-LISP-USER::PERSISTED-NATIVE-ARGUMENT")
                (setq found t)
                (assert (getf frame :arguments-available-p))
                (assert (eq value (car (getf frame :arguments))))))
            (assert found))
          (format t "BFASL-NATIVE-ARGS-PASS~%")
        "#,
            compiled.to_str().unwrap()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("BFASL-NATIVE-ARGS-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn original_arguments_survive_native_deoptimization() {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (defun deopt-native-arguments (number value)
            (+ number 1)
            (prog1 (egcl-debug:list-backtrace :count 32) (length value)))
          (dotimes (i 100) (deopt-native-arguments 1 '(warm)))
          (assert (= {installed} (egcl-ext:function-tier 'deopt-native-arguments)))
          (let* ((number (expt 2 61))
                 (value (list "deoptimized"))
                 (before (egcl-ext:deopt-count))
                 (frames (deopt-native-arguments number value))
                 (found nil))
            (assert (> (egcl-ext:deopt-count) before))
            (dolist (frame frames)
              (when (equal (getf frame :function) "COMMON-LISP-USER::DEOPT-NATIVE-ARGUMENTS")
                (setq found t)
                (assert (getf frame :arguments-available-p))
                (assert (= 2 (length (getf frame :arguments))))
                (assert (eq number (first (getf frame :arguments))))
                (assert (eq value (second (getf frame :arguments))))))
            (assert found))
          (format t "DEOPT-NATIVE-ARGS-PASS~%")
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("DEOPT-NATIVE-ARGS-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn osr_keeps_original_arguments_in_the_logical_frame() {
    let program = r#"
      (defun osr-native-arguments (value)
        (let ((i 0))
          (tagbody again
            (if (= i 100)
                (return-from osr-native-arguments (egcl-debug:list-backtrace :count 32)))
            (length value)
            (setq i (+ i 1))
            (go again))))
      (let* ((value (list "osr")) (frames (osr-native-arguments value)) (found nil))
        (assert (> (egcl-ext:function-osr-count 'osr-native-arguments) 0))
        (dolist (frame frames)
          (when (equal (getf frame :function) "COMMON-LISP-USER::OSR-NATIVE-ARGUMENTS")
            (assert (not found))
            (setq found t)
            (assert (getf frame :arguments-available-p))
            (assert (eq value (car (getf frame :arguments))))))
        (assert found))
      (format t "OSR-NATIVE-ARGS-PASS~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_FORCE_TIER")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T0_T1_THRESHOLD", "1000000")
        .env("EGCL_DISABLE_T2", "1")
        .env("EGCL_OSR_THRESHOLD", "20")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("OSR-NATIVE-ARGS-PASS"),
        "{stdout}\n{stderr}"
    );
}
