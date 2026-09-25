//! Native workers share the CLOS definitions used by portable thread wrappers.
use std::process::Command;

fn check(program: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "45",
            env!("CARGO_BIN_EXE_torcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env_remove("TORCL_BACKEND")
        .env("TORCL_FORCE_TIER", "t0")
        .output()
        .expect("run bounded native CLOS regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SHARED-CLOS-OK"), "{stdout}\n{stderr}");
}

#[test]
fn accessor_dispatches_explicit_method_on_non_instance() {
    check(
        r#"
        (defclass shared-designator-class ()
          ((value :accessor shared-designator-value)))
        (defmethod shared-designator-value ((object string)) (length object))
        (assert (= 3 (shared-designator-value "abc")))
        (assert (= 3 (eval '(shared-designator-value "abc"))))
        (assert (= 4
          (torcl-thread:join-thread
            (torcl-thread:make-thread
              (lambda () (eval '(shared-designator-value "abcd")))))))
        (format t "SHARED-CLOS-OK~%")
        "#,
    );
}

#[test]
fn worker_finds_the_same_class_metaobject() {
    check(
        r#"
        (defclass shared-thread-class () ())
        (let ((expected (find-class 'shared-thread-class)))
          (assert (eq expected
            (torcl-thread:join-thread
              (torcl-thread:make-thread
                (lambda () (find-class 'shared-thread-class nil)))))))
        (format t "SHARED-CLOS-OK~%")
        "#,
    );
}

#[test]
fn worker_constructs_instance_and_dispatches_accessor() {
    check(
        r#"
        (defclass shared-thread-wrapper ()
          ((value :initarg :value :accessor shared-thread-value)))
        (let ((result
                (torcl-thread:join-thread
                  (torcl-thread:make-thread
                    (lambda ()
                      (handler-case
                          (let ((object (make-instance 'shared-thread-wrapper :value 41)))
                            (incf (shared-thread-value object))
                            object)
                        (error () :worker-error)))))))
          (assert (typep result 'shared-thread-wrapper))
          (assert (= 42 (shared-thread-value result))))
        (format t "SHARED-CLOS-OK~%")
        "#,
    );
}

#[test]
fn worker_updates_slots_in_captured_thread_wrapper() {
    check(
        r#"
        (defclass shared-captured-wrapper ()
          ((result :initform nil :accessor shared-wrapper-result)))
        (let ((wrapper (make-instance 'shared-captured-wrapper)))
          (torcl-thread:join-thread
            (torcl-thread:make-thread
              (lambda ()
                (with-slots (result) wrapper
                  (setf result (list 41 42))))))
          (assert (equal '(41 42) (shared-wrapper-result wrapper))))
        (format t "SHARED-CLOS-OK~%")
        "#,
    );
}

#[test]
fn worker_dispatches_accessor_on_captured_instance() {
    check(
        r#"
        (defclass shared-callback-state ()
          ((value :initform 41 :accessor shared-callback-value)))
        (let* ((state (make-instance 'shared-callback-state))
               (answer
                 (torcl-thread:join-thread
                   (torcl-thread:make-thread
                     (lambda ()
                       (handler-case (shared-callback-value state)
                         (error () :worker-error)))))))
          (format t "WORKER-ACCESSOR=~S~%" answer)
          (assert (eql answer 41)))
        (format t "SHARED-CLOS-OK~%")
        "#,
    );
}

#[test]
fn worker_method_keeps_its_defining_lexical_environment() {
    check(
        r#"
        (defclass shared-method-class () ())
        (let ((value (list 41)))
          (defmethod shared-lexical-method ((object shared-method-class))
            (car value)))
        (let* ((object (make-instance 'shared-method-class))
               (answer (torcl-thread:join-thread
                         (torcl-thread:make-thread
                           (lambda ()
                             (handler-case (shared-lexical-method object)
                               (error () :worker-error)))))))
          (format t "WORKER-LEXICAL-METHOD=~S~%" answer)
          (assert (eql answer 41)))
        (format t "SHARED-CLOS-OK~%")
    "#,
    );
}

#[test]
fn worker_redefinition_invalidates_main_dispatch_cache() {
    check(
        r#"
        (defclass shared-dispatch-class () ())
        (defmethod shared-dispatch-method ((object shared-dispatch-class)) 41)
        (let ((object (make-instance 'shared-dispatch-class)))
          (format t "MAIN-METHOD-BEFORE=~S~%" (shared-dispatch-method object))
          (dotimes (i 3) (assert (= 41 (shared-dispatch-method object))))
          (assert (eql 42
            (torcl-thread:join-thread
              (torcl-thread:make-thread
                (lambda ()
                  (defmethod shared-dispatch-method ((object shared-dispatch-class)) 42)
                  (format t "WORKER-METHOD-AFTER=~S~%" (shared-dispatch-method object))
                  (shared-dispatch-method object))))))
          (assert (= 42 (shared-dispatch-method object))))
        (format t "SHARED-CLOS-OK~%")
    "#,
    );
}

#[test]
fn class_and_accessor_outlive_the_defining_worker() {
    check(
        r#"
        (torcl-thread:join-thread
          (torcl-thread:make-thread
            (lambda ()
              (defclass exited-worker-class ()
                ((value :initform (list 41) :accessor exited-worker-value))))))
        (let ((object (make-instance 'exited-worker-class)))
          (format t "EXITED-WORKER-SLOT=~S ACCESSOR=~S~%"
            (slot-value object 'value) (exited-worker-value object))
          (assert (equal '(41) (exited-worker-value object))))
        (format t "SHARED-CLOS-OK~%")
    "#,
    );
}

#[test]
fn lexical_method_outlives_its_defining_worker() {
    check(
        r#"
        (defclass exited-lexical-method-class () ())
        (torcl-thread:join-thread
          (torcl-thread:make-thread
            (lambda ()
              (let ((value (list 41)))
                (defmethod exited-lexical-method ((object exited-lexical-method-class))
                  (car value))))))
        (let ((answer
                (handler-case
                    (exited-lexical-method (make-instance 'exited-lexical-method-class))
                  (error () :missing-capture))))
          (format t "EXITED-LEXICAL-METHOD=~S~%" answer)
          (assert (eql answer 41)))
        (format t "SHARED-CLOS-OK~%")
    "#,
    );
}
