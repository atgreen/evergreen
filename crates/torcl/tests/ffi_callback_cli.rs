//! Per R2.12, Lisp closures are callable through generated C entries, with
//! contained exits and native-thread admission. Per R8.01, sandbox denies entry.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

#[test]
fn callbacks_reuse_live_setf_expanders_across_nested_entries() {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defun callback-cell (cell) (car cell))
          (defun store-callback-cell (cell value) (setf (car cell) value))
          (defsetf callback-cell store-callback-cell)
          (let* ((inner (torcl-ffi:make-callback
                          (lambda ()
                            (eval '(let ((cell (list 0)))
                                     (setf (callback-cell cell) 7)
                                     (callback-cell cell))))
                          :int nil))
                 (outer (torcl-ffi:make-callback
                          (lambda ()
                            (torcl-ffi:foreign-call
                              (torcl-ffi:callback-pointer inner) :int nil nil))
                          :int nil)))
            (dotimes (i 3)
              (assert (= 7 (torcl-ffi:foreign-call
                             (torcl-ffi:callback-pointer outer) :int nil nil))))
            (torcl-ffi:free-callback outer)
            (torcl-ffi:free-callback inner))
          (assert (= 9 (funcall (compile nil
                         '(lambda ()
                            (let ((cell (list 0)))
                              (setf (callback-cell cell) 9)
                              (callback-cell cell)))))))
          (format t "CALLBACK-SETF-OK~%")
        "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("CALLBACK-SETF-OK"));
}

#[test]
fn host_qsort_calls_allocating_lisp_comparators_and_contains_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (let* ((qsort (torcl-ffi:foreign-symbol-pointer "qsort"))
                 (array (torcl-ffi:foreign-alloc 40))
                 (calls 0)
                 (compare (torcl-ffi:make-callback
                   (lambda (a b)
                     (incf calls)
                     (let ((pair (list (torcl-ffi:mem-ref a :int)
                                       (torcl-ffi:mem-ref b :int))))
                       (- (car pair) (cadr pair))))
                   :int '(:pointer :pointer))))
            (loop for value in '(7 2 10 4 3 5 1 6 9 8) for i from 0
                  do (torcl-ffi:mem-set value array :int (* 4 i)))
            (torcl-ffi:foreign-call qsort :void '(:pointer :unsigned-long :unsigned-long :pointer)
              (list array 10 4 (torcl-ffi:callback-pointer compare)))
            (assert (> calls 0))
            (assert (equal (loop for i below 10 collect (torcl-ffi:mem-ref array :int (* 4 i)))
                           '(1 2 3 4 5 6 7 8 9 10)))
            (torcl-ffi:free-callback compare)
            (let ((bad (torcl-ffi:make-callback (lambda (a b) (error "comparator failed"))
                                               :int '(:pointer :pointer))))
              (assert (handler-case
                        (progn (torcl-ffi:foreign-call qsort :void
                                 '(:pointer :unsigned-long :unsigned-long :pointer)
                                 (list array 10 4 (torcl-ffi:callback-pointer bad))) nil)
                        (torcl-ffi:ffi-error () t)))
              (torcl-ffi:free-callback bad))
            (torcl-ffi:foreign-free array))
          (format t "QSORT-OK~%")
        "#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("QSORT-OK"));
}

#[test]
fn callbacks_are_denied_in_sandbox_in_direct_and_funcall_paths() {
    for form in [
        "(torcl-ffi:make-callback (lambda () 1) :int nil)",
        "(funcall #'torcl::%foreign-callback :make (lambda () 1) :int nil)",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .args(["--no-init", "--sandbox", "--eval", form])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("FFI access denied"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn lisp_closures_cross_real_c_frames_with_explicit_callback_lifetimes() {
    let dir = std::env::temp_dir().join(format!("torcl-callback-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("callback.c");
    let library = dir.join("callback.so");
    std::fs::write(
        &source,
        r#"
        double call_lisp(double (*callback)(double), double value, int *returned) {
            double answer = callback(value);
            *returned += 1;
            return answer;
        }
    "#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let program = format!(
        r#"
      (let* ((library (torcl-ffi:load-foreign-library {:?}))
             (caller (torcl-ffi:foreign-symbol-pointer "call_lisp" library))
             (returned (torcl-ffi:foreign-alloc 4))
             (bias 2.5d0)
             (callback (torcl-ffi:make-callback (lambda (x) (+ bias x)) :double '(:double))))
        (torcl-ffi:mem-set 0 returned :int)
        (assert (typep callback 'torcl-ffi:foreign-callback))
        (assert (not (torcl-ffi:pointerp callback)))
        (assert (= 3.75d0 (torcl-ffi:foreign-call caller :double '(:pointer :double :pointer)
                          (list (torcl-ffi:callback-pointer callback) 1.25d0 returned))))
        (assert (= 1 (torcl-ffi:mem-ref returned :int)))
        (assert (null (torcl-ffi:callback-error callback)))
        (let ((nested (torcl-ffi:make-callback
                        (lambda (x) (torcl-ffi:foreign-call
                          (torcl-ffi:callback-pointer callback) :double '(:double) (list x)))
                        :double '(:double))))
          (assert (= 4.5d0 (funcall #'torcl-ffi:foreign-call
                             (funcall #'torcl-ffi:callback-pointer nested) :double '(:double) '(2d0))))
          (torcl-ffi:free-callback nested))
        (let ((active nil))
          (setf active (torcl-ffi:make-callback
                         (lambda (x) (assert (handler-case (progn (torcl-ffi:free-callback active) nil)
                                              (torcl-ffi:ffi-error () t))) x)
                         :double '(:double)))
          (assert (= 7d0 (torcl-ffi:foreign-call (torcl-ffi:callback-pointer active) :double '(:double) '(7d0))))
          (torcl-ffi:free-callback active))
        (dolist (function (list (lambda (x) (error "callback failed"))
                               (lambda (x) (throw 'outside x))))
          (let ((bad (torcl-ffi:make-callback function :double '(:double))))
            (assert (eq :contained
              (catch 'outside
                (handler-case
                  (torcl-ffi:foreign-call caller :double '(:pointer :double :pointer)
                    (list (torcl-ffi:callback-pointer bad) 1d0 returned))
                  (torcl-ffi:ffi-error () :contained)))))
            (assert (stringp (torcl-ffi:callback-error bad)))
            (assert (null (torcl-ffi:callback-error bad)))
            (torcl-ffi:free-callback bad)))
        (assert (= 3 (torcl-ffi:mem-ref returned :int)))
        (torcl-ffi:free-callback callback)
        (assert (handler-case (progn (torcl-ffi:callback-pointer callback) nil) (torcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (torcl-ffi:free-callback callback) nil) (torcl-ffi:ffi-error () t)))
        (torcl-ffi:foreign-free returned)
        (torcl-ffi:close-foreign-library library))
      (format t "LISP-CALLBACK-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("LISP-CALLBACK-OK"));
}

#[test]
fn callbacks_preserve_dynamic_bindings_and_contain_lexical_exits() {
    let program = r#"
      (defparameter *callback-binding* 2)
      (let ((callback (torcl-ffi:make-callback (lambda () *callback-binding*) :int nil)))
        (let ((*callback-binding* 42))
          (assert (= 42 (torcl-ffi:foreign-call (torcl-ffi:callback-pointer callback) :int nil nil))))
        (assert (= 2 *callback-binding*))
        (torcl-ffi:free-callback callback))
      (assert (eq :contained (block outside
        (let ((callback (torcl-ffi:make-callback (lambda () (return-from outside :escaped)) :int nil)))
          (unwind-protect
            (handler-case (torcl-ffi:foreign-call (torcl-ffi:callback-pointer callback) :int nil nil)
              (torcl-ffi:ffi-error () :contained))
            (torcl-ffi:free-callback callback))))))
      (assert (handler-case (progn (torcl-ffi:make-callback 42 :int nil) nil) (type-error () t)))
      (assert (handler-case (progn (torcl-ffi:make-callback (lambda () 0) :int '(:void)) nil) (torcl-ffi:ffi-error () t)))
      (assert (handler-case (progn (torcl-ffi:make-callback (lambda () 0) :int '(:int . :int)) nil) (torcl-ffi:ffi-error () t)))
      (format t "CALLBACK-CONTROL-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("CALLBACK-CONTROL-OK"));
}

#[cfg(target_env = "gnu")]
#[test]
fn foreign_created_thread_can_invoke_a_retained_lisp_closure() {
    let dir =
        std::env::temp_dir().join(format!("torcl-callback-thread-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("callback.c");
    let library = dir.join("callback.so");
    std::fs::write(
        &source,
        r#"
        #include <pthread.h>
        struct call { double (*callback)(double); double result; };
        static void *invoke(void *data) {
            struct call *call = data;
            call->result = call->callback(1.25);
            return 0;
        }
        double call_thread(double (*callback)(double)) {
            struct call call = {callback, -1};
            pthread_t thread;
            if (pthread_create(&thread, 0, invoke, &call)) return -2;
            if (pthread_join(thread, 0)) return -3;
            return call.result;
        }
    "#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC", "-pthread"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let program = format!(
        r#"
      (let* ((library (torcl-ffi:load-foreign-library {:?}))
             (caller (torcl-ffi:foreign-symbol-pointer "call_thread" library))
             (bias 2.5d0)
             (callback (torcl-ffi:make-callback (lambda (x) (+ bias x)) :double '(:double))))
        (assert (= 3.75d0 (torcl-ffi:foreign-call caller :double '(:pointer)
                           (list (torcl-ffi:callback-pointer callback)))))
        (assert (null (torcl-ffi:callback-error callback)))
        (torcl-ffi:free-callback callback)
        (let ((bad (torcl-ffi:make-callback (lambda (x) (error "foreign-thread callback")) :double '(:double))))
          (assert (= 0d0 (torcl-ffi:foreign-call caller :double '(:pointer) (list (torcl-ffi:callback-pointer bad)))))
          (assert (stringp (torcl-ffi:callback-error bad)))
          (assert (null (torcl-ffi:callback-error bad)))
          (torcl-ffi:free-callback bad))
        (torcl-ffi:close-foreign-library library))
      (format t "FOREIGN-THREAD-CALLBACK-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FOREIGN-THREAD-CALLBACK-OK"));
}
