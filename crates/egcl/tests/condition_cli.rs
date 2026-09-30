// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! R13.19: Lisp condition variables use real mutex-backed waits.
use std::process::Command;

fn run(program: &str, marker: &str, options: &[&str]) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .args(options)
        .args(["--eval", program])
        .output()
        .expect("run bounded EGCL");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn condition_api_validates_arguments_and_reacquires_after_timeout() {
    let program = r#"
      (let ((cv (egcl-thread:make-condition-variable :name "test"))
            (m (egcl-thread:make-mutex)))
        (assert (typep cv 'egcl-thread:condition-variable))
        (assert (eq (type-of cv) 'egcl-thread:condition-variable))
        (assert (= 0 (egcl-thread:condition-notify cv)))
        (assert (= 0 (egcl-thread:condition-notify cv (expt 2 100))))
        (assert (= 0 (egcl-thread:condition-broadcast cv)))
        (assert (handler-case (progn (egcl-thread:condition-wait cv m :timeout 0) nil)
                  (program-error () t)))
        (egcl-thread:grab-mutex m)
        (assert (not (egcl-thread:condition-wait cv m :timeout 1/1000)))
        (egcl-thread:release-mutex m)
        (assert (not (egcl-thread:with-mutex (m)
          (funcall #'egcl-thread:condition-wait cv m :timeout 0))))
        (dolist (bad '(-1 "bad" #c(1 1)))
          (assert (handler-case (progn (egcl-thread:condition-wait cv m :timeout bad) nil)
                    (type-error () t))))
        (dolist (bad '(-1 1/2 "bad"))
          (assert (handler-case (progn (egcl-thread:condition-notify cv bad) nil)
                    (type-error () t))))
        (assert (handler-case (progn (egcl-thread:condition-notify m) nil)
                  (type-error () t)))
        (assert (handler-case (progn (egcl-thread:condition-wait cv 17) nil)
                  (type-error () t))))
      (assert (handler-case (progn (egcl-thread:make-condition-variable :name 17) nil)
                (type-error () t)))
      (format t "CONDITION-API-OK~%")
    "#;
    run(program, "CONDITION-API-OK", &[]);
    run(
        &format!("(eval '(progn {program}))"),
        "CONDITION-API-OK",
        &[],
    );
}

#[test]
fn native_lisp_condition_wait_releases_recursive_depth_and_counts_wakes() {
    run(
        r#"
      (defvar *cv* (egcl-thread:make-condition-variable))
      (defvar *cv-mutex* (egcl-thread:make-mutex :recursive t))
      (defvar *cv-ready* 0)
      (defun cv-worker ()
        (egcl-thread:grab-mutex *cv-mutex*)
        (egcl-thread:grab-mutex *cv-mutex*)
        (incf *cv-ready*)
        (let ((woke (egcl-thread:condition-wait *cv* *cv-mutex* :timeout 10)))
          (egcl-thread:release-mutex *cv-mutex*)
          (egcl-thread:release-mutex *cv-mutex*)
          (assert (handler-case (progn (egcl-thread:release-mutex *cv-mutex*) nil)
                    (program-error () t)))
          woke))
      (let ((a (egcl-thread:make-thread 'cv-worker))
            (b (egcl-thread:make-thread 'cv-worker)))
        (loop until (egcl-thread:with-mutex (*cv-mutex*) (= *cv-ready* 2))
              do (sleep 0.001))
        (egcl-thread:with-mutex (*cv-mutex*)
          (assert (= 0 (egcl-thread:condition-notify *cv* 0)))
          (assert (= 1 (egcl-thread:condition-notify *cv*)))
          (assert (= 1 (egcl-thread:condition-broadcast *cv*)))
          (assert (= 0 (egcl-thread:condition-broadcast *cv*))))
        (assert (egcl-thread:join-thread a))
        (assert (egcl-thread:join-thread b)))
      (format t "CONDITION-THREADS-OK~%")
    "#,
        "CONDITION-THREADS-OK",
        &[],
    );
}

#[test]
fn saved_condition_cannot_reuse_process_local_wait_queue() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "egcl-condition-{}-{nonce}.bimg",
        std::process::id()
    ));
    let filename = path.to_str().unwrap();
    run(
        &format!(
            r#"
      (defvar *saved-cv* (egcl-thread:make-condition-variable))
      (format t "CONDITION-SAVE~%")
      (save-lisp-and-die "{filename}")
    "#
        ),
        "CONDITION-SAVE",
        &[],
    );
    run(
        r#"
      (assert (typep *saved-cv* 'egcl-thread:condition-variable))
      (assert (eq (type-of *saved-cv*) 'egcl-thread:condition-variable))
      (assert (handler-case (progn (egcl-thread:condition-notify *saved-cv*) nil)
                (program-error () t)))
      (assert (= 0 (egcl-thread:condition-broadcast (egcl-thread:make-condition-variable))))
      (format t "CONDITION-RESTORE-OK~%")
    "#,
        "CONDITION-RESTORE-OK",
        &["--image", filename],
    );
    std::fs::remove_file(path).unwrap();
}
