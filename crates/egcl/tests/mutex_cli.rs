//! R13.19: real Lisp mutex ownership and unwind-safe locking.
use std::process::Command;

#[test]
fn saved_mutex_is_recognized_but_cannot_reuse_native_ownership() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("egcl-mutex-{}-{nonce}.bimg", std::process::id()));
    let filename = path.to_str().unwrap();
    run(
        &format!(
            r#"
      (defvar *saved-mutex* (egcl-thread:make-mutex :recursive t))
      (egcl-thread:grab-mutex *saved-mutex*)
      (format t "MUTEX-IMAGE-SAVE~%")
      (save-lisp-and-die "{filename}")
    "#
        ),
        "MUTEX-IMAGE-SAVE",
    );
    run_with_options(
        r#"
      (assert (egcl-thread:mutex-p *saved-mutex*))
      (assert (eq 'egcl-thread:mutex (type-of *saved-mutex*)))
      (assert (handler-case (progn (egcl-thread:grab-mutex *saved-mutex*) nil)
                (program-error () t)))
      (assert (handler-case (progn (egcl-thread:release-mutex *saved-mutex*) nil)
                (program-error () t)))
      (let ((fresh (egcl-thread:make-mutex)))
        (assert (egcl-thread:grab-mutex fresh))
        (egcl-thread:release-mutex fresh))
      (format t "MUTEX-IMAGE-RESTORE-OK~%")
    "#,
        "MUTEX-IMAGE-RESTORE-OK",
        &["--image", filename],
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn native_threads_contend_on_the_same_lisp_mutex() {
    run(
        r#"
      (defvar *gate* (egcl-thread:make-mutex))
      (defun mutex-contender ()
        (list (egcl-thread:grab-mutex *gate* :waitp nil)
              (egcl-thread:grab-mutex *gate* :timeout 1/100)
              (handler-case (progn (egcl-thread:release-mutex *gate*) nil)
                (program-error () t))
              (egcl-thread:with-mutex (*gate* :waitp nil)
                (error "unacquired mutex body executed"))))
      (egcl-thread:grab-mutex *gate*)
      (assert (equal '(nil nil t nil)
        (egcl-thread:join-thread (egcl-thread:make-thread 'mutex-contender))))
      (egcl-thread:release-mutex *gate*)
      (defvar *mutex-worker-started* nil)
      (defun mutex-waiter ()
        (setf *mutex-worker-started* t)
        (egcl-thread:with-mutex (*gate*) 42))
      (egcl-thread:grab-mutex *gate*)
      (let ((worker (egcl-thread:make-thread 'mutex-waiter)))
        (loop until *mutex-worker-started* do (sleep 0.001))
        (egcl-thread:release-mutex *gate*)
        (assert (= 42 (egcl-thread:join-thread worker))))
      (assert (egcl-thread:grab-mutex *gate* :waitp nil))
      (egcl-thread:release-mutex *gate*)
      (format t "MUTEX-THREADS-OK~%")
    "#,
        "MUTEX-THREADS-OK",
    );
}

#[test]
fn mutex_api_checks_ownership_and_releases_on_nonlocal_exit() {
    let program = r#"
      (let ((m (egcl-thread:make-mutex :name "test")))
        (assert (egcl-thread:mutex-p m))
        (assert (typep m 'egcl-thread:mutex))
        (assert (not (egcl-thread:mutex-p 17)))
        (assert (eq 'egcl-thread:mutex (type-of m)))
        (assert (egcl-thread:grab-mutex m :timeout 0))
        (assert (handler-case (progn (egcl-thread:grab-mutex m) nil)
                  (program-error () t)))
        (assert (null (egcl-thread:release-mutex m)))
        (assert (handler-case (progn (egcl-thread:release-mutex m) nil)
                  (program-error () t)))
        (assert (null (egcl-thread:release-mutex m :if-not-owner :ignore)))
        (let ((warnings 0))
          (handler-bind ((warning (lambda (c) (incf warnings) (muffle-warning c))))
            (egcl-thread:release-mutex m :if-not-owner :warn))
          (assert (= warnings 1)))
        (assert (= 42 (catch 'escape
          (egcl-thread:with-mutex (m) (throw 'escape 42)))))
        (assert (egcl-thread:grab-mutex m :waitp nil))
        (egcl-thread:release-mutex m)
        (let ((evaluations 0))
          (assert (equal '(11 22)
            (multiple-value-list
              (egcl-thread:with-mutex ((progn (incf evaluations) m))
                (values 11 22)))))
          (assert (= evaluations 1)))
        (assert (null (multiple-value-list (egcl-thread:with-mutex (m) (values)))))
        (assert (eq :caught (handler-case
          (egcl-thread:with-mutex (m) (error "expected"))
          (error () :caught))))
        (assert (funcall #'egcl-thread:grab-mutex m :timeout 1/100))
        (funcall #'egcl-thread:release-mutex m)
        (dolist (timeout '(-1 "bad" #c(1 1)))
          (assert (handler-case
            (progn (egcl-thread:grab-mutex m :timeout timeout) nil)
            (type-error () t))))
        (assert (handler-case
          (progn (egcl-thread:release-mutex m :if-not-owner :bogus) nil)
          (type-error () t)))
        (assert (handler-case (progn (egcl-thread:grab-mutex 17) nil)
                  (type-error () t))))
      (let ((m (egcl-thread:make-mutex :recursive t)))
        (assert (egcl-thread:grab-mutex m))
        (assert (egcl-thread:grab-mutex m))
        (egcl-thread:release-mutex m)
        (egcl-thread:release-mutex m)
        (assert (handler-case (progn (egcl-thread:release-mutex m) nil)
                  (program-error () t))))
      (assert (handler-case (progn (egcl-thread:make-mutex :name 17) nil)
                (type-error () t)))
      (format t "MUTEX-API-OK~%")
    "#;
    run(program, "MUTEX-API-OK");
}

fn run(program: &str, marker: &str) {
    run_with_options(program, marker, &[]);
}

fn run_with_options(program: &str, marker: &str, options: &[&str]) {
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
