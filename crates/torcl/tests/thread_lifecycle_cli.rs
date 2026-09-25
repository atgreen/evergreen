//! Native-thread lifecycle introspection and bounded joins through the Lisp API.
use std::process::Command;

#[test]
fn names_liveness_enumeration_yield_and_timed_join_are_real() {
    let program = r#"
      (let* ((lock (torcl-thread:make-mutex :name "lifecycle lock"))
             (condition (torcl-thread:make-condition-variable
                          :name "lifecycle condition"))
             (ready nil)
             (release nil)
             (worker nil))
        (torcl-thread:grab-mutex lock)
        (unwind-protect
            (progn
              (setq worker
                    (torcl-thread:make-thread
                      (lambda ()
                        (torcl-thread:with-mutex (lock)
                          (setq ready t)
                          (torcl-thread:condition-broadcast condition)
                          (loop until release do
                            (assert (torcl-thread:condition-wait
                                      condition lock :timeout 10))))
                        42)
                      :name "lifecycle worker"))
              (loop until ready do
                (assert (torcl-thread:condition-wait condition lock :timeout 10)))
              (format t "LIFECYCLE-BEFORE name=~S alive=~S all=~S current=~S~%"
                      (torcl-thread:thread-name worker)
                      (torcl-thread:thread-alive-p worker)
                      (torcl-thread:all-threads)
                      (torcl-thread:current-thread))
              (assert (string= "lifecycle worker"
                               (torcl-thread:thread-name worker)))
              (assert (torcl-thread:thread-alive-p worker))
              (assert (member worker (torcl-thread:all-threads)))
              (assert (member (torcl-thread:current-thread)
                              (torcl-thread:all-threads)))
              (let ((timed-join
                      (multiple-value-list
                        (torcl-thread:join-thread worker :timeout 0))))
                (format t "LIFECYCLE-TIMED-JOIN ~S~%" timed-join)
                (assert (equal '(nil nil) timed-join)))
              (assert (null (torcl-thread:thread-yield)))
              (setq release t)
              (torcl-thread:condition-broadcast condition))
          (torcl-thread:release-mutex lock))
        (let ((joined (multiple-value-list
                        (torcl-thread:join-thread worker :timeout 10))))
          (format t "LIFECYCLE-JOINED ~S~%" joined)
          (assert (equal '(42 t) joined)))
        (assert (null (torcl-thread:thread-alive-p worker)))
        (assert (null (member worker (torcl-thread:all-threads)))))
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
                    (torcl-thread:join-thread
                      (torcl-thread:current-thread) :timeout -1)
                    nil)
                (type-error () t)))
      (assert (handler-case
                  (progn
                    (torcl-thread:make-thread (lambda () nil) :name 17)
                    nil)
                (type-error () t)))
      (format t "THREAD-LIFECYCLE-NAMESPACE-OK~%")
    "#;

    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_torcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env("TORCL_GC_POISON", "1")
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
