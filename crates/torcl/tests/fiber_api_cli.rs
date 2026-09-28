//! Public Lisp fibers run on actual runtime carriers (R9.42, R9.43, R13.18).
#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
use std::process::Command;

fn run(program: &str) {
    for backend in ["tree-walker", "bytecode"] {
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .args(["--no-init", "--eval", program])
            .env("TORCL_BACKEND", backend)
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
      (assert (null (torcl-fiber:current-fiber)))
      (let* ((events nil)
             (a (torcl-fiber:make-fiber
                  (lambda (x)
                    (push :a events)
                    (torcl-fiber:fiber-yield)
                    (values (+ x 1) :second))
                  :name "first" :arguments '(40)))
             (b (torcl-fiber:make-fiber
                  (lambda ()
                    (push :b events)
                    (torcl-fiber:fiber-yield)
                    42))))
        (assert (torcl-fiber:fiber-p a))
        (assert (eq :created (torcl-fiber:fiber-state a)))
        (assert (string= "first" (torcl-fiber:fiber-name a)))
        (let ((group (torcl-fiber:start-fibers (list a b) :carrier-count 1)))
          (assert (= 1 (length (torcl-fiber:scheduler-group-carriers group))))
          (assert (equal '(41 42) (torcl-fiber:finish-fibers group)))
          (assert (equal '(41 42) (torcl-fiber:finish-fibers group)))
          (assert (torcl-fiber:fiber-group-done-p group)))
        (assert (equal '(41 :second) (multiple-value-list (torcl-fiber:fiber-join a))))
        (assert (equal '(41 :second) (torcl-fiber:fiber-result a)))
        (assert (eq :dead (torcl-fiber:fiber-state a)))
        (assert (null (torcl-fiber:fiber-alive-p a)))
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
                        (torcl-fiber:make-fiber
                          (lambda ()
                            (let ((*fiber-value* n))
                              (block done
                                (unwind-protect
                                    (return-from done (list *fiber-value* :returned))
                                  (torcl-fiber:fiber-yield)
                                  (assert (= n *fiber-value*)))))))))))
        (assert (equal (loop for n below 8 collect (list n :returned))
                       (torcl-fiber:run-fibers fibers :carrier-count 2)))
        (assert (eq :outside *fiber-value*)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn pin_park_sleep_and_initial_bindings() {
    run(r##"
      (defvar *fiber-probe* :outside)
      (let ((a (torcl-fiber:make-fiber
                 (lambda ()
                   (assert (torcl-fiber:current-fiber))
                   (assert (eq :inside *fiber-probe*))
                   (assert (torcl-fiber:fiber-can-yield-p))
                   (torcl-fiber:with-fiber-pinned ()
                     (assert (not (torcl-fiber:fiber-can-yield-p)))
                     (assert (handler-case (progn (torcl-fiber:fiber-yield) nil)
                               (program-error () t)))
                     (let ((torcl-fiber:*pinned-blocking-action* :error))
                       (assert (handler-case (progn (torcl-fiber:fiber-sleep 0.001) nil)
                                 (program-error () t)))))
                   (assert (torcl-fiber:fiber-can-yield-p))
                   (assert (torcl-fiber:fiber-park (lambda () t) :timeout 0))
                   (assert (not (torcl-fiber:fiber-park (lambda () nil) :timeout 0.002)))
                   (torcl-fiber:fiber-sleep 0.001)
                   :finished)
                 :stack-size 1048576
                 :initial-bindings '((*fiber-probe* . :inside)))))
        (assert (equal '(:finished) (torcl-fiber:run-fibers (list a) :carrier-count 1)))
        (assert (eq :outside *fiber-probe*)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn idle_hook_and_error_drain() {
    run(r##"
      (let* ((seen nil)
             (a (torcl-fiber:make-fiber (lambda () (error "entry failed"))))
             (b (torcl-fiber:make-fiber (lambda () (torcl-fiber:fiber-sleep 0.02) 7)))
             (group (torcl-fiber:start-fibers
                      (list a b) :carrier-count 1
                      :idle-hook (lambda (g) (assert (torcl-fiber:scheduler-group-p g))
                                           (setq seen t)))))
        (assert (handler-case (progn (torcl-fiber:finish-fibers group) nil)
                  (torcl-fiber:fiber-error () t)))
        (assert seen)
        (assert (= 7 (torcl-fiber:fiber-join b)))
        (assert (torcl-fiber:fiber-error-p a))
        (assert (torcl-fiber:fiber-group-done-p group))
        (assert (handler-case (progn (torcl-fiber:finish-fibers group) nil)
                  (torcl-fiber:fiber-error () t))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn saved_fiber_backtraces() {
    run(r##"
      (let* ((fiber (torcl-fiber:make-fiber (lambda () 9) :name "trace-me"))
             (stream (make-string-output-stream)))
        (torcl-fiber:print-fiber-backtrace fiber :stream stream :count 5)
        (assert (search "trace-me" (get-output-stream-string stream)))
        (torcl-fiber:run-fibers (list fiber) :carrier-count 1)
        (torcl-fiber:print-fiber-backtrace fiber :stream stream :count 0)
        (assert (search "trace-me" (get-output-stream-string stream))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn yielding_printer_preserves_circular_structure_across_gc() {
    run(r##"
      (defclass fiber-print-probe () ())
      (defmethod print-object ((object fiber-print-probe) stream)
        (torcl-fiber:fiber-yield)
        (torcl-ext:gc)
        (write-string "probe" stream))
      (let ((fiber (torcl-fiber:make-fiber
                     (lambda ()
                       (let* ((*print-circle* t)
                              (list (list (make-instance 'fiber-print-probe))))
                         (setf (cdr list) list)
                         (let ((text (format nil "~S" list)))
                           (assert (search "probe" text))
                           (assert (search "#1#" text))
                           t))))))
        (assert (equal '(t) (torcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn proclaimed_specials_and_timed_join() {
    run(r##"
      (defvar fiber-special :outside)
      (defun fiber-special-value () fiber-special)
      (let* ((lock (torcl-thread:make-mutex))
             (locked (torcl-thread:grab-mutex lock))
             (fiber (torcl-fiber:make-fiber
                      (lambda ()
                        (let ((fiber-special :inside))
                          (torcl-thread:with-mutex (lock) (fiber-special-value))))))
             (group (torcl-fiber:start-fibers (list fiber) :carrier-count 2)))
        (assert (equal '(nil nil) (multiple-value-list (torcl-fiber:fiber-join fiber :timeout 0))))
        (torcl-thread:release-mutex lock)
        (assert (equal '(:inside) (torcl-fiber:finish-fibers group)))
        (assert (eq :outside fiber-special)))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn submission_zero_values_and_closed_groups() {
    run(r##"
      (let* ((a (torcl-fiber:make-fiber (lambda () (values))))
             (b (torcl-fiber:make-fiber (lambda () (values 7 8))))
             (group (torcl-fiber:start-fibers nil :carrier-count 2)))
        (assert (member a (torcl-fiber:list-all-fibers)))
        (assert (eq a (torcl-fiber:submit-fiber group a)))
        (assert (eq b (torcl-fiber:submit-fiber group b)))
        (assert (handler-case (progn (torcl-fiber:submit-fiber group a) nil)
                  (error () t)))
        (assert (equal '(nil 7) (torcl-fiber:finish-fibers group)))
        (assert (null (multiple-value-list (torcl-fiber:fiber-join a))))
        (assert (equal '(7 8) (multiple-value-list (torcl-fiber:fiber-join b))))
        (assert (not (member a (torcl-fiber:list-all-fibers))))
        (assert (handler-case (progn (torcl-fiber:submit-fiber group b) nil)
                  (error () t))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn backtrace_of_suspended_fiber_and_self_join() {
    run(r##"
      (defun fiber-trace-leaf (lock)
        (torcl-thread:with-mutex (lock) nil)
        :done)
      (defun fiber-trace-middle (lock) (fiber-trace-leaf lock))
      (let* ((semaphore (torcl-thread:make-mutex))
             (locked (torcl-thread:grab-mutex semaphore))
             (fiber (torcl-fiber:make-fiber
                      (lambda ()
                        (assert (handler-case
                                  (progn (torcl-fiber:fiber-join (torcl-fiber:current-fiber)) nil)
                                  (error () t)))
                        (assert (null (torcl-fiber:fiber-result (torcl-fiber:current-fiber))))
                        (fiber-trace-middle semaphore)) :name "suspended"))
             (group (torcl-fiber:start-fibers (list fiber) :carrier-count 1)))
        (loop until (eq :suspended (torcl-fiber:fiber-state fiber)) do
          (when (eq :dead (torcl-fiber:fiber-state fiber)) (torcl-fiber:fiber-join fiber)
            (error "Fiber exited before it could be inspected"))
          (sleep 0.001))
        (let ((stream (make-string-output-stream)))
          (torcl-fiber:print-fiber-backtrace fiber :stream stream)
          (let ((text (get-output-stream-string stream)))
            (assert (search "FIBER-TRACE-LEAF" text))
            (assert (search "FIBER-TRACE-MIDDLE" text))))
        (torcl-thread:release-mutex semaphore)
        (assert (equal '(:done) (torcl-fiber:finish-fibers group))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn compiled_entries_and_many_fibers() {
    run(r##"
      (defun fiber-compiled-step (x)
        (torcl-fiber:fiber-yield)
        (list x (+ x 1)))
      (compile 'fiber-compiled-step)
      (let ((fibers (loop for n below 100 collect
                     (torcl-fiber:make-fiber #'fiber-compiled-step :arguments (list n)))))
        (assert (equal (loop for n below 100 collect (list n (+ n 1)))
                       (torcl-fiber:run-fibers fibers :carrier-count 3))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn native_stack_limit_signals_storage_condition() {
    run(r##"
      (defun fiber-recurse (n)
        (list n (fiber-recurse (+ n 1))))
      (let ((fiber (torcl-fiber:make-fiber
                     (lambda ()
                       (handler-case (fiber-recurse 0)
                         (storage-condition () :caught))))))
        (assert (equal '(:caught) (torcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn self_join_does_not_wait_for_another_joiner() {
    run(r##"
      (let ((fiber (torcl-fiber:make-fiber
                     (lambda ()
                       (torcl-fiber:fiber-sleep 0.02)
                       (assert (null (torcl-fiber:fiber-result (torcl-fiber:current-fiber))))
                       (handler-case (torcl-fiber:fiber-join (torcl-fiber:current-fiber))
                         (error () :caught))))))
        (assert (equal '(:caught) (torcl-fiber:run-fibers (list fiber) :carrier-count 1))))
      (format t "FIBER-API-OK~%")
    "##);
}

#[test]
fn initial_bindings_are_isolated_across_fibers() {
    run(r##"
(defvar *fiber-initial* :outside)
(let ((fibers (loop for n below 8 collect
 (torcl-fiber:make-fiber (lambda (expected)
   (assert (= expected *fiber-initial*))
   (torcl-fiber:fiber-yield)
   (assert (= expected *fiber-initial*))
   *fiber-initial*) :arguments (list n)
   :initial-bindings (list (cons '*fiber-initial* n))))))
 (assert (equal '(0 1 2 3 4 5 6 7) (torcl-fiber:run-fibers fibers :carrier-count 1)))
 (assert (eq :outside *fiber-initial*)))
(format t "FIBER-API-OK~%")
    "##);
}
