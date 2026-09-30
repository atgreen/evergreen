// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native-thread lifecycle introspection and bounded joins through the Lisp API.
use std::process::Command;

#[test]
fn names_liveness_enumeration_yield_and_timed_join_are_real() {
    let program = r#"
      (let* ((lock (egcl-thread:make-mutex :name "lifecycle lock"))
             (condition (egcl-thread:make-condition-variable
                          :name "lifecycle condition"))
             (ready nil)
             (release nil)
             (worker nil))
        (egcl-thread:grab-mutex lock)
        (unwind-protect
            (progn
              (setq worker
                    (egcl-thread:make-thread
                      (lambda ()
                        (egcl-thread:with-mutex (lock)
                          (setq ready t)
                          (egcl-thread:condition-broadcast condition)
                          (loop until release do
                            (assert (egcl-thread:condition-wait
                                      condition lock :timeout 10))))
                        42)
                      :name "lifecycle worker"))
              (loop until ready do
                (assert (egcl-thread:condition-wait condition lock :timeout 10)))
              (format t "LIFECYCLE-BEFORE name=~S alive=~S all=~S current=~S~%"
                      (egcl-thread:thread-name worker)
                      (egcl-thread:thread-alive-p worker)
                      (egcl-thread:all-threads)
                      (egcl-thread:current-thread))
              (assert (string= "lifecycle worker"
                               (egcl-thread:thread-name worker)))
              (assert (egcl-thread:thread-alive-p worker))
              (assert (member worker (egcl-thread:all-threads)))
              (assert (member (egcl-thread:current-thread)
                              (egcl-thread:all-threads)))
              (let ((timed-join
                      (multiple-value-list
                        (egcl-thread:join-thread worker :timeout 0))))
                (format t "LIFECYCLE-TIMED-JOIN ~S~%" timed-join)
                (assert (equal '(nil nil) timed-join)))
              (assert (null (egcl-thread:thread-yield)))
              (setq release t)
              (egcl-thread:condition-broadcast condition))
          (egcl-thread:release-mutex lock))
        (let ((joined (multiple-value-list
                        (egcl-thread:join-thread worker :timeout 10))))
          (format t "LIFECYCLE-JOINED ~S~%" joined)
          (assert (equal '(42 t) joined)))
        (assert (null (egcl-thread:thread-alive-p worker)))
        (assert (null (member worker (egcl-thread:all-threads)))))
      (format t "THREAD-LIFECYCLE-OK~%")

      ;; Extension builtins are package-qualified. A package-local or
      ;; uninterned symbol with the same print-name must remain independently
      ;; definable; Bordeaux Threads' DEFDFUN guard depends on this.
      (let ((local-name (make-symbol "MAKE-THREAD")))
        (assert (null (fboundp local-name)))
        (setf (symbol-function local-name) (lambda () :package-local))
        (assert (eq :package-local (funcall local-name))))
      (assert (handler-case
                  (progn
                    (egcl-thread:join-thread
                      (egcl-thread:current-thread) :timeout -1)
                    nil)
                (type-error () t)))
      (assert (handler-case
                  (progn
                    (egcl-thread:make-thread (lambda () nil) :name 17)
                    nil)
                (type-error () t)))
      (format t "THREAD-LIFECYCLE-NAMESPACE-OK~%")
    "#;

    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env("EGCL_GC_POISON", "1")
        .output()
        .expect("run bounded native-thread lifecycle regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("THREAD-LIFECYCLE-OK"), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("THREAD-LIFECYCLE-NAMESPACE-OK"),
        "{stdout}\n{stderr}"
    );
}
