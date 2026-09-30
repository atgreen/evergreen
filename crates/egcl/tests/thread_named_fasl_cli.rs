//! Named compiled definitions must be visible beyond the loading execution.
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "egcl-thread-named-fasl-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn compile(&self, name: &str, value: i64) -> PathBuf {
        let source = self.0.join(format!("{name}.lisp"));
        let fasl = self.0.join(format!("{name}.fasl"));
        std::fs::write(&source, format!("(defun shared-named-value () {value})")).unwrap();
        run(
            &format!(
                "(compile-file {source:?} :output-file {fasl:?}) \
                 (format t \"NAMED-FASL-COMPILED~%\")"
            ),
            "NAMED-FASL-COMPILED",
            "t0",
            None,
        );
        std::fs::remove_file(source).unwrap();
        fasl
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // This directory is uniquely created and owned by this test.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, marker: &str, tier: &str, image: Option<&Path>) {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=5",
        "120",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
    ]);
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    let output = command
        .args(["--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .expect("run named-FASL worker regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{marker}: {stdout}\n{stderr}");
    assert!(
        stdout.contains(marker),
        "missing {marker}: {stdout}\n{stderr}"
    );
}

#[test]
fn main_loaded_named_fasl_runs_on_fresh_worker() {
    let fixture = Fixture::new();
    let fasl = fixture.compile("main-loaded", 11);
    run(
        &format!(
            r#"
            (load {fasl:?})
            (assert (eql 11 (shared-named-value)))
            (assert (eql 11 (egcl-thread:join-thread
              (egcl-thread:make-thread 'shared-named-value))))
            (format t "NAMED-COLD-WORKER-OK~%")
            "#
        ),
        "NAMED-COLD-WORKER-OK",
        "t0",
        None,
    );
}

#[test]
fn exited_worker_loaded_named_fasl_runs_on_main() {
    let fixture = Fixture::new();
    let fasl = fixture.compile("worker-loaded", 17);
    run(
        &format!(
            r#"
            (egcl-thread:join-thread (egcl-thread:make-thread
              (lambda () (load {fasl:?}))))
            (assert (eql 17 (shared-named-value)))
            (assert (eql 17 (egcl-thread:join-thread
              (egcl-thread:make-thread 'shared-named-value))))
            (format t "NAMED-EXITED-LOADER-OK~%")
            "#
        ),
        "NAMED-EXITED-LOADER-OK",
        "t0",
        None,
    );
}

#[test]
fn warmed_worker_observes_fasl_redefinition() {
    let fixture = Fixture::new();
    let replacement = fixture.compile("replacement", 29);
    run(
        &format!(
            r#"
            (defun shared-named-value () 11)
            (let ((lock (egcl-thread:make-mutex))
                  (cv (egcl-thread:make-condition-variable))
                  (phase 0) (before nil) (after-first nil) (after nil) (worker nil))
              (setq worker (egcl-thread:make-thread
                (lambda ()
                  (dotimes (iteration 2000) (shared-named-value))
                  (egcl-thread:grab-mutex lock)
                  (unwind-protect
                      (progn
                        (setq before (shared-named-value))
                        (setq phase 1)
                        (egcl-thread:condition-notify cv)
                        (loop until (= phase 2) do
                          (unless (egcl-thread:condition-wait cv lock :timeout 10)
                            (error "main did not redefine function")))
                        (setq after-first (shared-named-value))
                        (dotimes (iteration 2000) (shared-named-value))
                        (setq after (shared-named-value)))
                    (egcl-thread:release-mutex lock)))))
              (egcl-thread:grab-mutex lock)
              (unwind-protect
                  (progn
                    (loop until (= phase 1) do
                      (unless (egcl-thread:condition-wait cv lock :timeout 10)
                        (error "worker did not warm function")))
                    (load {replacement:?})
                    (assert (eql 29 (shared-named-value)))
                    (setq phase 2)
                    (egcl-thread:condition-notify cv))
                (egcl-thread:release-mutex lock))
              (egcl-thread:join-thread worker)
              (assert (eql 11 before))
              (assert (eql 29 after-first))
              (assert (eql 29 after)))
            (format t "NAMED-REDEFINITION-OK~%")
            "#
        ),
        "NAMED-REDEFINITION-OK",
        "t1",
        None,
    );
}

#[test]
fn warmed_worker_observes_unbinding_then_fasl_rebinding() {
    let fixture = Fixture::new();
    let original = fixture.compile("before-unbind", 11);
    let replacement = fixture.compile("after-unbind", 29);
    run(
        &format!(
            r#"
            (load {original:?})
            (let ((lock (egcl-thread:make-mutex))
                  (cv (egcl-thread:make-condition-variable))
                  (phase 0) (unbound-result nil) (rebound-result nil) (worker nil))
              (setq worker (egcl-thread:make-thread
                (lambda ()
                  (dotimes (iteration 2000) (shared-named-value))
                  (egcl-thread:grab-mutex lock)
                  (unwind-protect
                      (progn
                        (setq phase 1)
                        (egcl-thread:condition-notify cv)
                        (loop until (= phase 2) do
                          (unless (egcl-thread:condition-wait cv lock :timeout 10)
                            (error "main did not unbind function")))
                        (setq unbound-result
                          (handler-case (shared-named-value)
                            (undefined-function () :undefined)))
                        (setq phase 3)
                        (egcl-thread:condition-notify cv)
                        (loop until (= phase 4) do
                          (unless (egcl-thread:condition-wait cv lock :timeout 10)
                            (error "main did not rebind function")))
                        (setq rebound-result (shared-named-value)))
                    (egcl-thread:release-mutex lock)))))
              (egcl-thread:grab-mutex lock)
              (unwind-protect
                  (progn
                    (loop until (= phase 1) do
                      (unless (egcl-thread:condition-wait cv lock :timeout 10)
                        (error "worker did not warm function")))
                    (fmakunbound 'shared-named-value)
                    (assert (not (fboundp 'shared-named-value)))
                    (setq phase 2)
                    (egcl-thread:condition-notify cv)
                    (loop until (= phase 3) do
                      (unless (egcl-thread:condition-wait cv lock :timeout 10)
                        (error "worker did not check unbinding")))
                    (load {replacement:?})
                    (setq phase 4)
                    (egcl-thread:condition-notify cv))
                (egcl-thread:release-mutex lock))
              (egcl-thread:join-thread worker)
              (format t "UNBOUND=~a REBOUND=~a~%" unbound-result rebound-result)
              (assert (eq :undefined unbound-result))
              (assert (eql 29 rebound-result)))
            (format t "NAMED-UNBIND-REBIND-OK~%")
            "#
        ),
        "NAMED-UNBIND-REBIND-OK",
        "t1",
        None,
    );
}

#[test]
fn worker_loaded_named_fasl_survives_image_restore() {
    let fixture = Fixture::new();
    let fasl = fixture.compile("image-worker", 31);
    let image = fixture.0.join("named.bimg");
    run(
        &format!(
            r#"
            (assert (eql 31 (egcl-thread:join-thread
              (egcl-thread:make-thread (lambda ()
                (load {fasl:?}) (shared-named-value))))))
            (format t "NAMED-IMAGE-SAVE~%")
            (save-lisp-and-die {image:?})
            "#
        ),
        "NAMED-IMAGE-SAVE",
        "t0",
        None,
    );
    std::fs::remove_file(fasl).unwrap();
    run(
        r#"
        (assert (eql 31 (shared-named-value)))
        (assert (eql 31 (egcl-thread:join-thread
          (egcl-thread:make-thread 'shared-named-value))))
        (format t "NAMED-IMAGE-WORKER-OK~%")
        "#,
        "NAMED-IMAGE-WORKER-OK",
        "t0",
        Some(&image),
    );
}
