// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Public Lisp fibers run on actual runtime carriers (R9.42, R9.43, R13.18).
#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
use std::process::Command;

fn run(program: &str) {
    for backend in ["tree-walker", "bytecode"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_BACKEND", backend)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{backend}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("FIBER-API-OK"),
            "{backend}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn lifecycle_yield_multiple_values_and_repeatable_finish() {
    run(r##"
      (assert (null (egcl-fiber:current-fiber)))
      (let* ((events nil)
             (a (egcl-fiber:make-fiber
                  (lambda (x)
                    (push :a events)
                    (egcl-fiber:fiber-yield)
                    (values (+ x 1) :second))
                  :name "first" :arguments '(40)))
             (b (egcl-fiber:make-fiber
                  (lambda ()
                    (push :b events)
                    (egcl-fiber:fiber-yield)
                    42))))
        (assert (egcl-fiber:fiber-p a))
        (assert (eq :created (egcl-fiber:fiber-state a)))
        (assert (string= "first" (egcl-fiber:fiber-name a)))
        (let ((group (egcl-fiber:start-fibers (list a b) :carrier-count 1)))
          (assert (= 1 (length (egcl-fiber:scheduler-group-carriers group))))
          (assert (equal '(41 42) (egcl-fiber:finish-fibers group)))
          (assert (equal '(41 42) (egcl-fiber:finish-fibers group)))
          (assert (egcl-fiber:fiber-group-done-p group)))
        (assert (equal '(41 :second) (multiple-value-list (egcl-fiber:fiber-join a))))
        (assert (equal '(41 :second) (egcl-fiber:fiber-result a)))
        (assert (eq :dead (egcl-fiber:fiber-state a)))
        (assert (null (egcl-fiber:fiber-alive-p a)))
        (assert (= 2 (length events))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn fiber_control_state_survives_yields_and_unwind() {
    run(r##"
      (defvar *fiber-value* :outside)
      (let* ((fibers (loop for n below 8 collect
                      (let ((n n))
                        (egcl-fiber:make-fiber
                          (lambda ()
                            (let ((*fiber-value* n))
                              (block done
                                (unwind-protect
                                    (return-from done (list *fiber-value* :returned))
                                  (egcl-fiber:fiber-yield)
                                  (assert (= n *fiber-value*)))))))))))
        (assert (equal (loop for n below 8 collect (list n :returned))
                       (egcl-fiber:run-fibers fibers :carrier-count 2)))
        (assert (eq :outside *fiber-value*)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn pin_park_sleep_and_initial_bindings() {
    run(r##"
      (defvar *fiber-probe* :outside)
      (let ((a (egcl-fiber:make-fiber
                 (lambda ()
                   (assert (egcl-fiber:current-fiber))
                   (assert (eq :inside *fiber-probe*))
                   (assert (egcl-fiber:fiber-can-yield-p))
                   (egcl-fiber:with-fiber-pinned ()
                     (assert (not (egcl-fiber:fiber-can-yield-p)))
                     (assert (handler-case (progn (egcl-fiber:fiber-yield) nil)
                               (program-error () t)))
                     (let ((egcl-fiber:*pinned-blocking-action* :error))
                       (assert (handler-case (progn (egcl-fiber:fiber-sleep 0.001) nil)
                                 (program-error () t)))))
                   (assert (egcl-fiber:fiber-can-yield-p))
                   (assert (egcl-fiber:fiber-park (lambda () t) :timeout 0))
                   (assert (not (egcl-fiber:fiber-park (lambda () nil) :timeout 0.002)))
                   (egcl-fiber:fiber-sleep 0.001)
                   :finished)
                 :stack-size 1048576
                 :initial-bindings '((*fiber-probe* . :inside)))))
        (assert (equal '(:finished) (egcl-fiber:run-fibers (list a) :carrier-count 1)))
        (assert (eq :outside *fiber-probe*)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn idle_hook_and_error_drain() {
    run(r##"
      (let* ((seen nil)
             (a (egcl-fiber:make-fiber (lambda () (error "entry failed"))))
             (b (egcl-fiber:make-fiber (lambda () (egcl-fiber:fiber-sleep 0.02) 7)))
             (group (egcl-fiber:start-fibers
                      (list a b) :carrier-count 1
                      :idle-hook (lambda (g) (assert (egcl-fiber:scheduler-group-p g))
                                           (setq seen t)))))
        (assert (handler-case (progn (egcl-fiber:finish-fibers group) nil)
                  (egcl-fiber:fiber-error () t)))
        (assert seen)
        (assert (= 7 (egcl-fiber:fiber-join b)))
        (assert (egcl-fiber:fiber-error-p a))
        (assert (egcl-fiber:fiber-group-done-p group))
        (assert (handler-case (progn (egcl-fiber:finish-fibers group) nil)
                  (egcl-fiber:fiber-error () t))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn saved_fiber_backtraces() {
    run(r##"
      (let* ((fiber (egcl-fiber:make-fiber (lambda () 9) :name "trace-me"))
             (stream (make-string-output-stream)))
        (egcl-fiber:print-fiber-backtrace fiber :stream stream :count 5)
        (assert (search "trace-me" (get-output-stream-string stream)))
        (egcl-fiber:run-fibers (list fiber) :carrier-count 1)
        (egcl-fiber:print-fiber-backtrace fiber :stream stream :count 0)
        (assert (search "trace-me" (get-output-stream-string stream))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn yielding_printer_preserves_circular_structure_across_gc() {
    run(r##"
      (defclass fiber-print-probe () ())
      (defmethod print-object ((object fiber-print-probe) stream)
        (egcl-fiber:fiber-yield)
        (egcl-ext:gc)
        (write-string "probe" stream))
      (let ((fiber (egcl-fiber:make-fiber
                     (lambda ()
                       (let* ((*print-circle* t)
                              (list (list (make-instance 'fiber-print-probe))))
                         (setf (cdr list) list)
                         (let ((text (format nil "~S" list)))
                           (assert (search "probe" text))
                           (assert (search "#1#" text))
                           t))))))
        (assert (equal '(t) (egcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn proclaimed_specials_and_timed_join() {
    run(r##"
      (defvar fiber-special :outside)
      (defun fiber-special-value () fiber-special)
      (let* ((lock (egcl-thread:make-mutex))
             (locked (egcl-thread:grab-mutex lock))
             (fiber (egcl-fiber:make-fiber
                      (lambda ()
                        (let ((fiber-special :inside))
                          (egcl-thread:with-mutex (lock) (fiber-special-value))))))
             (group (egcl-fiber:start-fibers (list fiber) :carrier-count 2)))
        (assert (equal '(nil nil) (multiple-value-list (egcl-fiber:fiber-join fiber :timeout 0))))
        (egcl-thread:release-mutex lock)
        (assert (equal '(:inside) (egcl-fiber:finish-fibers group)))
        (assert (eq :outside fiber-special)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn submission_zero_values_and_closed_groups() {
    run(r##"
      (let* ((a (egcl-fiber:make-fiber (lambda () (values))))
             (b (egcl-fiber:make-fiber (lambda () (values 7 8))))
             (group (egcl-fiber:start-fibers nil :carrier-count 2)))
        (assert (member a (egcl-fiber:list-all-fibers)))
        (assert (eq a (egcl-fiber:submit-fiber group a)))
        (assert (eq b (egcl-fiber:submit-fiber group b)))
        (assert (handler-case (progn (egcl-fiber:submit-fiber group a) nil)
                  (error () t)))
        (assert (equal '(nil 7) (egcl-fiber:finish-fibers group)))
        (assert (null (multiple-value-list (egcl-fiber:fiber-join a))))
        (assert (equal '(7 8) (multiple-value-list (egcl-fiber:fiber-join b))))
        (assert (not (member a (egcl-fiber:list-all-fibers))))
        (assert (handler-case (progn (egcl-fiber:submit-fiber group b) nil)
                  (error () t))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn backtrace_of_suspended_fiber_and_self_join() {
    run(r##"
      (defun fiber-trace-leaf (lock)
        (egcl-thread:with-mutex (lock) nil)
        :done)
      (defun fiber-trace-middle (lock) (fiber-trace-leaf lock))
      (let* ((semaphore (egcl-thread:make-mutex))
             (locked (egcl-thread:grab-mutex semaphore))
             (fiber (egcl-fiber:make-fiber
                      (lambda ()
                        (assert (handler-case
                                  (progn (egcl-fiber:fiber-join (egcl-fiber:current-fiber)) nil)
                                  (error () t)))
                        (assert (null (egcl-fiber:fiber-result (egcl-fiber:current-fiber))))
                        (fiber-trace-middle semaphore)) :name "suspended"))
             (group (egcl-fiber:start-fibers (list fiber) :carrier-count 1)))
        (loop until (eq :suspended (egcl-fiber:fiber-state fiber)) do
          (when (eq :dead (egcl-fiber:fiber-state fiber)) (egcl-fiber:fiber-join fiber)
            (error "Fiber exited before it could be inspected"))
          (sleep 0.001))
        (let ((stream (make-string-output-stream)))
          (egcl-fiber:print-fiber-backtrace fiber :stream stream)
          (let ((text (get-output-stream-string stream)))
            (assert (search "FIBER-TRACE-LEAF" text))
            (assert (search "FIBER-TRACE-MIDDLE" text))))
        (egcl-thread:release-mutex semaphore)
        (assert (equal '(:done) (egcl-fiber:finish-fibers group))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn compiled_entries_and_many_fibers() {
    run(r##"
      (defun fiber-compiled-step (x)
        (egcl-fiber:fiber-yield)
        (list x (+ x 1)))
      (compile 'fiber-compiled-step)
      (let ((fibers (loop for n below 100 collect
                     (egcl-fiber:make-fiber #'fiber-compiled-step :arguments (list n)))))
        (assert (equal (loop for n below 100 collect (list n (+ n 1)))
                       (egcl-fiber:run-fibers fibers :carrier-count 3))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn native_stack_limit_signals_storage_condition() {
    run(r##"
      (defun fiber-recurse (n)
        (list n (fiber-recurse (+ n 1))))
      (let ((fiber (egcl-fiber:make-fiber
                     (lambda ()
                       (handler-case (fiber-recurse 0)
                         (storage-condition () :caught))))))
        (assert (equal '(:caught) (egcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn self_join_does_not_wait_for_another_joiner() {
    run(r##"
      (let ((fiber (egcl-fiber:make-fiber
                     (lambda ()
                       (egcl-fiber:fiber-sleep 0.02)
                       (assert (null (egcl-fiber:fiber-result (egcl-fiber:current-fiber))))
                       (handler-case (egcl-fiber:fiber-join (egcl-fiber:current-fiber))
                         (error () :caught))))))
        (assert (equal '(:caught) (egcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn initial_bindings_are_isolated_across_fibers() {
    run(r##"
(defvar *fiber-initial* :outside)
(let ((fibers (loop for n below 8 collect
 (egcl-fiber:make-fiber (lambda (expected)
   (assert (= expected *fiber-initial*))
   (egcl-fiber:fiber-yield)
   (assert (= expected *fiber-initial*))
   *fiber-initial*) :arguments (list n)
   :initial-bindings (list (cons '*fiber-initial* n))))))
 (assert (equal '(0 1 2 3 4 5 6 7) (egcl-fiber:run-fibers fibers :carrier-count 1)))
 (assert (eq :outside *fiber-initial*)))
(format t "FIBER-API-OK~%")
    "##);
}
