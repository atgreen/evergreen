// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native thread entry must preserve closure identity and shared lexical cells.
use std::process::Command;

fn check_shared_captures(tier: &str) {
    let program = r#"
      (let* ((captured (list 5 6))
             (reader (lambda () captured))
             (writer (lambda () (setf captured (list 19 23)))))
        (assert (equal '(5 6) (funcall reader)))
        (format t "CREATOR-CAPTURE-READ-OK~%")
        (assert (equal '(5 6)
          (egcl-thread:join-thread (egcl-thread:make-thread reader))))
        (assert (equal '(19 23)
          (egcl-thread:join-thread (egcl-thread:make-thread writer))))
        (assert (equal '(19 23) captured))
        (assert (equal '(19 23) (funcall reader)))
        (setf captured (list 41 42))
        (assert (equal '(41 42)
          (egcl-thread:join-thread (egcl-thread:make-thread reader)))))
      (format t "THREAD-CAPTURE-SHARED-OK~%")
    "#;
    run(program, "THREAD-CAPTURE-SHARED-OK", tier);
}

fn run(program: &str, marker: &str, tier: &str) {
    run_with_options(program, marker, tier, &[]);
}

fn run_with_options(program: &str, marker: &str, tier: &str, options: &[&str]) {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=5",
        "120",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
    ]);
    command.args(options).args(["--eval", program]);
    if tier != "default" {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let output = command.output().expect("run native closure test");
    assert!(
        output.status.success(),
        "tier {tier}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn native_thread_shared_captures_default() {
    check_shared_captures("default");
}

#[test]
fn native_thread_shared_captures_interpreter() {
    check_shared_captures("interp");
}

#[test]
fn native_thread_shared_captures_t0() {
    check_shared_captures("t0");
}

#[test]
fn native_thread_shared_captures_t1() {
    check_shared_captures("t1");
}

fn check_compiled_thread_image(worker_creates: bool) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "egcl-compiled-thread-image-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("factory.lisp");
    let fasl = directory.join("factory.fasl");
    let image = directory.join("callbacks.bimg");
    std::fs::write(
        &source,
        r#"
      (defun compiled-image-maker ()
        (lambda ()
          (let ((capture (list 7 8)))
            (list (lambda () capture)
                  (lambda () (setq capture (list 13 17)))))))
    "#,
    )
    .unwrap();
    run(
        &format!(
            "(compile-file {:?} :output-file {:?}) (format t \"FACTORY-COMPILED~%\")",
            source, fasl
        ),
        "FACTORY-COMPILED",
        "t0",
    );
    // Loading only the FASL rules out success from interpreting a source DEFUN.
    std::fs::remove_file(source).unwrap();
    let create = if worker_creates {
        "(egcl-thread:join-thread (egcl-thread:make-thread (compiled-image-maker)))"
    } else {
        "(funcall (compiled-image-maker))"
    };
    run(
        &format!(
            r#"
      (load {fasl:?})
      (defvar *compiled-image-callbacks* {create})
      ;; Compiled closures have a private function symbol; interpreter closure
      ;; conses have no name. Require the former, not just a requested tier.
      (assert (nth-value 2 (function-lambda-expression (first *compiled-image-callbacks*))))
      (format t "CHECK-COMPILED-BEFORE-SAVE~%")
      (assert (equal '(7 8) (egcl-thread:join-thread
        (egcl-thread:make-thread (first *compiled-image-callbacks*)))))
      (format t "COMPILED-THREAD-SAVE~%")
      (save-lisp-and-die {image:?})
    "#
        ),
        "COMPILED-THREAD-SAVE",
        "t0",
    );
    run_with_options(
        r#"
      (format t "CHECK-COMPILED-AFTER-RESTORE~%")
      (assert (equal '(7 8) (egcl-thread:join-thread
        (egcl-thread:make-thread (first *compiled-image-callbacks*)))))
      (assert (equal '(13 17) (egcl-thread:join-thread
        (egcl-thread:make-thread (second *compiled-image-callbacks*)))))
      (assert (equal '(13 17) (funcall (first *compiled-image-callbacks*))))
      (format t "COMPILED-THREAD-RESTORED~%")
    "#,
        "COMPILED-THREAD-RESTORED",
        "t0",
        &["--image", image.to_str().unwrap()],
    );
    std::fs::remove_file(fasl).unwrap();
    std::fs::remove_file(image).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn compiled_callbacks_run_on_workers_after_image_restore() {
    check_compiled_thread_image(false);
}

#[test]
fn compiled_callbacks_from_exited_creator_survive_image_restore() {
    check_compiled_thread_image(true);
}

#[test]
fn native_thread_closure_keeps_local_function_scope() {
    let program = r#"
      (let ((callback
              (let ((amount 7))
                (flet ((offset (x) (+ amount x)))
                  (lambda () (offset 5))))))
        (assert (= 12 (funcall callback)))
        (format t "CREATOR-FLET-OK~%")
        (assert (= 12 (egcl-thread:join-thread
                        (egcl-thread:make-thread callback)))))
      (format t "THREAD-FLET-OK~%")
    "#;
    for tier in ["default", "interp", "t0", "t1"] {
        run(program, "THREAD-FLET-OK", tier);
    }
}

#[test]
fn native_thread_labels_function_keeps_identity() {
    let program = r#"
      (let ((callback (labels ((self () #'self)) #'self)))
        (assert (eq callback (funcall callback)))
        (format t "CREATOR-IDENTITY-OK~%")
        (assert (eq callback (egcl-thread:join-thread
                              (egcl-thread:make-thread callback)))))
      (format t "THREAD-IDENTITY-OK~%")
    "#;
    for tier in ["default", "interp", "t0", "t1"] {
        run(program, "THREAD-IDENTITY-OK", tier);
    }
}

fn check_worker_collection(body: &str, tier: &str) {
    let setup = r#"
      (defvar *caller-gate* (egcl-thread:make-mutex))
      (defvar *caller-cv* (egcl-thread:make-condition-variable))
      (defvar *caller-ready* nil)
      (defvar *caller-released* nil)
      (defun wait-in-global-function ()
        (egcl-thread:grab-mutex *caller-gate*)
        (unwind-protect
            (progn
              (setq *caller-ready* t)
              (egcl-thread:condition-broadcast *caller-cv*)
              (loop until *caller-released* do
                (if (egcl-thread:condition-wait *caller-cv* *caller-gate* :timeout 10)
                    nil (error "caller timed out"))))
          (egcl-thread:release-mutex *caller-gate*)))
      (defun collect-on-worker ()
        (egcl-thread:grab-mutex *caller-gate*)
        (unwind-protect
            (progn
              (loop until *caller-ready* do
                (if (egcl-thread:condition-wait *caller-cv* *caller-gate* :timeout 10)
                    nil (error "collector timed out")))
              (egcl::%prune-closures)
              (setq *caller-released* t)
              (egcl-thread:condition-broadcast *caller-cv*)
              42)
          (egcl-thread:release-mutex *caller-gate*)))
    "#;
    // A named worker must not capture and accidentally root the caller's state.
    let program = format!("{setup}\n{body}\n(format t \"THREAD-WORKER-GC-OK~%\")");
    run(&program, "THREAD-WORKER-GC-OK", tier);
}

fn check_suspended_caller(tier: &str) {
    check_worker_collection(
        r#"
      (let ((payload (list 7 11))
            (worker (egcl-thread:make-thread 'collect-on-worker)))
        (wait-in-global-function)
        (if (equal payload '(7 11)) nil (error "suspended caller lost payload"))
        (if (= 42 (egcl-thread:join-thread worker)) nil (error "worker result changed")))
    "#,
        tier,
    );
}

#[test]
fn suspended_local_function_scope_survives_worker_gc() {
    for tier in ["interp", "default", "t0", "t1"] {
        check_worker_collection(
            r#"
          (let ((worker (egcl-thread:make-thread 'collect-on-worker)))
            (flet ((local-wait () (wait-in-global-function))
                   (answer () (list 19 23)))
              (local-wait)
              (if (equal (answer) '(19 23)) nil
                  (error "suspended local function body lost")))
            (if (= 42 (egcl-thread:join-thread worker)) nil
                (error "worker result changed")))
        "#,
            tier,
        );
    }
}

#[test]
fn loop_cursors_and_accumulators_survive_worker_gc() {
    for tier in ["interp", "default", "t0", "t1"] {
        check_worker_collection(
            r#"
          (let ((worker (egcl-thread:make-thread 'collect-on-worker)))
            (let ((result
                    (loop for item in (list (list 7 11) (list 13 17))
                          collect (list (car item))
                          do (if (= (car item) 7) (wait-in-global-function)))))
              (if (equal result '((7) (13))) nil
                  (error "loop state lost during worker GC")))
            (if (= 42 (egcl-thread:join-thread worker)) nil
                (error "worker result changed")))
        "#,
            tier,
        );
    }
}

#[test]
fn suspended_caller_survives_worker_gc_interpreter() {
    check_suspended_caller("interp");
}

#[test]
fn builtin_function_cache_survives_worker_gc() {
    for tier in ["interp", "default", "t0", "t1"] {
        check_worker_collection(
            r#"
          (if (= 17 (funcall #'car (list 17))) nil (error "initial builtin call failed"))
          (let ((worker (egcl-thread:make-thread 'collect-on-worker)))
            (wait-in-global-function)
            (if (= 19 (funcall #'car (list 19))) nil
                (error "cached builtin lost during worker collection"))
            (if (= 42 (egcl-thread:join-thread worker)) nil
                (error "worker result changed")))
        "#,
            tier,
        );
    }
}

#[test]
fn suspended_caller_survives_worker_gc_default() {
    check_suspended_caller("default");
}

#[test]
fn suspended_caller_survives_worker_gc_t0() {
    check_suspended_caller("t0");
}

#[test]
fn suspended_caller_survives_worker_gc_t1() {
    check_suspended_caller("t1");
}

#[test]
fn concurrent_first_local_function_references_keep_identity() {
    let program = r#"
      (loop repeat 8 do
        (let ((gate (egcl-thread:make-mutex))
              (cv (egcl-thread:make-condition-variable))
              (ready 0) (released nil))
          (labels ((answer () 42))
            (let ((callback (lambda ()
                              (egcl-thread:grab-mutex gate)
                              (setq ready (+ ready 1))
                              (egcl-thread:condition-broadcast cv)
                              (loop until released do
                                (if (egcl-thread:condition-wait cv gate :timeout 10)
                                    nil (error "reference worker timed out")))
                              (egcl-thread:release-mutex gate)
                              #'answer))
                  (workers nil))
              (egcl-thread:grab-mutex gate)
              (loop repeat 8 do
                (setq workers (cons (egcl-thread:make-thread callback) workers)))
              (loop until (= ready 8) do
                (if (egcl-thread:condition-wait cv gate :timeout 10) nil
                    (error "reference workers did not become ready")))
              (setq released t)
              (egcl-thread:condition-broadcast cv)
              (egcl-thread:release-mutex gate)
              (let ((references nil))
                (loop while workers do
                  (setq references
                    (cons (egcl-thread:join-thread (car workers)) references))
                  (setq workers (cdr workers)))
                (loop for reference in references do
                  (if (eq reference (first references)) nil
                      (error "local function identity diverged"))
                  (if (= 42 (funcall reference)) nil
                      (error "local function result changed")))
                (if (eq #'answer (first references)) nil
                    (error "creator local function identity diverged")))))))
      (format t "THREAD-FIRST-REFERENCE-OK~%")
    "#;
    for tier in ["interp", "default", "t0", "t1"] {
        run(program, "THREAD-FIRST-REFERENCE-OK", tier);
    }
}

#[test]
fn concurrent_closure_construction_survives_pruning() {
    let program = r#"
      (defvar *prune-gate* (egcl-thread:make-mutex))
      (defvar *prune-cv* (egcl-thread:make-condition-variable))
      (defvar *prune-ready* 0)
      (defvar *prune-start* nil)
      ;; Keep a tree-walker builtin wrapper live in every execution mode.
      ;; A (0 0) pruning result must not hide a failed collection in this test.
      (defvar *prune-sentinel* #'car)
      (defun construct-during-pruning ()
        (egcl-thread:grab-mutex *prune-gate*)
        (setq *prune-ready* (+ *prune-ready* 1))
        (egcl-thread:condition-broadcast *prune-cv*)
        (loop until *prune-start* do
          (if (egcl-thread:condition-wait *prune-cv* *prune-gate* :timeout 10)
              nil (error "prune worker timed out")))
        (egcl-thread:release-mutex *prune-gate*)
        (loop repeat 2000 do
          (let* ((payload (list 41))
                 (callback (lambda () (+ 1 (car payload)))))
            (if (= 42 (funcall callback)) nil
                (error "closure lost during concurrent prune"))))
        42)
      (let ((one (egcl-thread:make-thread 'construct-during-pruning))
            (two (egcl-thread:make-thread 'construct-during-pruning)))
        (egcl-thread:grab-mutex *prune-gate*)
        (loop until (= *prune-ready* 2) do
          (if (egcl-thread:condition-wait *prune-cv* *prune-gate* :timeout 10)
              nil (error "prune workers did not become ready")))
        (setq *prune-start* t)
        (egcl-thread:condition-broadcast *prune-cv*)
        (egcl-thread:release-mutex *prune-gate*)
        (loop repeat 100 do
          (if (> (car (egcl::%prune-closures)) 0) nil
              (error "concurrent pruning did not complete a live-registry collection")))
        (if (= 17 (funcall *prune-sentinel* '(17))) nil
            (error "live builtin wrapper lost during pruning"))
        (if (= 42 (egcl-thread:join-thread one)) nil (error "first worker failed"))
        (if (= 42 (egcl-thread:join-thread two)) nil (error "second worker failed")))
      (format t "THREAD-CONCURRENT-PRUNE-OK~%")
    "#;
    for tier in ["interp", "default", "t0", "t1"] {
        run(program, "THREAD-CONCURRENT-PRUNE-OK", tier);
    }
}

#[test]
fn native_thread_capture_survives_creator_exit() {
    let program = r#"
      (defun make-worker-reader ()
        (let ((capture (list 31 32)))
          (lambda () capture)))
      (let ((callback (egcl-thread:join-thread
                        (egcl-thread:make-thread 'make-worker-reader))))
        (assert (equal '(31 32) (funcall callback)))
        (assert (equal '(31 32) (egcl-thread:join-thread
                                 (egcl-thread:make-thread callback)))))
      (format t "THREAD-CREATOR-EXIT-OK~%")
    "#;
    for tier in ["default", "interp", "t0", "t1"] {
        run(program, "THREAD-CREATOR-EXIT-OK", tier);
    }
}

#[test]
fn native_thread_capture_survives_collection_while_waiting() {
    let program = r#"
      (let* ((mutex (egcl-thread:make-mutex))
             (cv (egcl-thread:make-condition-variable))
             (ready nil) (released nil) (payload (list 3 4))
             (callback
               (lambda ()
                 ;; Use primitives to isolate capture/GC behavior from macro
                 ;; expansion and the higher-level thread wrappers.
                 (egcl-thread:grab-mutex mutex)
                 (unwind-protect
                     (progn
                       (setf ready t)
                       (egcl-thread:condition-broadcast cv)
                       (loop until released do
                         (if (egcl-thread:condition-wait cv mutex :timeout 10)
                             nil (error "worker timed out")))
                       (if (equal '(13 17) payload)
                           (setf payload (list 19 23))
                           (error "worker lost payload")))
                   (egcl-thread:release-mutex mutex)))))
        (let ((worker (egcl-thread:make-thread callback)))
          (egcl-thread:with-mutex (mutex)
            (loop until ready do
              (assert (egcl-thread:condition-wait cv mutex :timeout 10)))
            (setf payload (list 13 17))
            (egcl::%prune-closures)
            (setf released t)
            (egcl-thread:condition-broadcast cv))
          (assert (equal '(19 23) (egcl-thread:join-thread worker)))
          (assert (equal '(19 23) payload))))
      (format t "THREAD-CAPTURE-GC-OK~%")
    "#;
    for tier in ["default", "interp", "t0", "t1"] {
        run(program, "THREAD-CAPTURE-GC-OK", tier);
    }
}

#[test]
fn restored_closures_share_bindings_across_threads() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "egcl-thread-closure-{}-{nonce}.bimg",
        std::process::id()
    ));
    let filename = path.to_str().unwrap();
    run(
        &format!(
            r#"
      (defvar *callbacks*
        (let ((capture (list 7 8)))
          (list (lambda () capture) (lambda () (setf capture (list 13 17))))))
      (format t "THREAD-IMAGE-SAVE~%")
      (save-lisp-and-die "{filename}")
    "#
        ),
        "THREAD-IMAGE-SAVE",
        "t0",
    );
    run_with_options(
        r#"
      (assert (equal '(7 8) (funcall (first *callbacks*))))
      (assert (equal '(13 17) (egcl-thread:join-thread
                              (egcl-thread:make-thread (second *callbacks*)))))
      (assert (equal '(13 17) (funcall (first *callbacks*))))
      (assert (equal '(13 17) (egcl-thread:join-thread
                              (egcl-thread:make-thread (first *callbacks*)))))
      (format t "THREAD-IMAGE-RESTORE-OK~%")
    "#,
        "THREAD-IMAGE-RESTORE-OK",
        "t0",
        &["--image", filename],
    );
    std::fs::remove_file(path).unwrap();
}
