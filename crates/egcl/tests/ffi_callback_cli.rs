//! Per R2.12, Lisp closures are callable through generated C entries, with
//! contained exits and native-thread admission. Per R8.01, sandbox denies entry.
#![cfg(all(target_arch = "x86_64", any(target_os = "linux", windows)))]
use std::process::Command;

#[test]
fn raw_runtime_callback_survives_gc_stress_and_poison() {
    let program = r#"
      (let ((callback (egcl::%foreign-callback :make
                        (lambda (x) (let ((values (list x 1))) (+ (car values) (car (cdr values)))))
                        :int '(:int))))
        (print (egcl::%ffi-call (egcl::%foreign-callback :pointer callback)
                               :int '(:int) '(41)))
        (egcl::%foreign-callback :free callback))
    "#;
    let mut baseline = None;
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--no-bootstrap", "--eval", program]);
        command
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_POISON");
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
        if let Some(expected) = baseline.as_ref() {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_dll_calls_and_allocating_callbacks_round_trip_through_lisp() {
    let scalars = std::env::var("EGCL_FFI_SCALARS_DLL")
        .unwrap()
        .replace('\\', "/");
    let callbacks = std::env::var("EGCL_FFI_CALLBACKS_DLL")
        .unwrap()
        .replace('\\', "/");
    let program = format!(
        r#"
      (let* ((scalars (egcl-ffi:load-foreign-library {scalars:?}))
             (callbacks (egcl-ffi:load-foreign-library {callbacks:?}))
             (mixed (egcl-ffi:foreign-symbol-pointer "egcl_ffi_mixed" scalars))
             (variadic (egcl-ffi:foreign-symbol-pointer "egcl_ffi_fixed_float" scalars))
             (invoke (egcl-ffi:foreign-symbol-pointer "egcl_callback_float" callbacks))
             (callback (egcl-ffi:make-callback
               (lambda (x) (let ((numbers (list x 2.5))) (+ (car numbers) (cadr numbers))))
               :float '(:float))))
        (assert (= 29d0 (egcl-ffi:foreign-call mixed :double
                          '(:int64 :double :float :int) '(2 3.25d0 1.5 4))))
        (assert (= 3.75d0 (egcl-ffi:foreign-call variadic :double
                            '(:float :int :float) '(1.25 1 2.5) 2)))
        (assert (= 1086324736 (egcl-ffi:foreign-call invoke :uint64 '(:pointer)
                               (list (egcl-ffi:callback-pointer callback)))))
        (egcl-ffi:free-callback callback)
        (let ((bad (egcl-ffi:make-callback (lambda (x) (error "contained")) :float '(:float))))
          (assert (handler-case
                    (progn (egcl-ffi:foreign-call invoke :uint64 '(:pointer)
                             (list (egcl-ffi:callback-pointer bad))) nil)
                    (egcl-ffi:ffi-error () t)))
          (assert (egcl-ffi:callback-error bad))
          (egcl-ffi:free-callback bad))
        (egcl-ffi:close-foreign-library callbacks)
        (egcl-ffi:close-foreign-library scalars))
      (format t "WINDOWS-FFI-OK~%")
    "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("WINDOWS-FFI-OK"));
}

#[test]
fn callbacks_reuse_live_setf_expanders_across_nested_entries() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defun callback-cell (cell) (car cell))
          (defun store-callback-cell (cell value) (setf (car cell) value))
          (defsetf callback-cell store-callback-cell)
          (let* ((inner (egcl-ffi:make-callback
                          (lambda ()
                            (eval '(let ((cell (list 0)))
                                     (setf (callback-cell cell) 7)
                                     (callback-cell cell))))
                          :int nil))
                 (outer (egcl-ffi:make-callback
                          (lambda ()
                            (egcl-ffi:foreign-call
                              (egcl-ffi:callback-pointer inner) :int nil nil))
                          :int nil)))
            (dotimes (i 3)
              (assert (= 7 (egcl-ffi:foreign-call
                             (egcl-ffi:callback-pointer outer) :int nil nil))))
            (egcl-ffi:free-callback outer)
            (egcl-ffi:free-callback inner))
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

#[cfg(unix)]
#[test]
fn host_qsort_calls_allocating_lisp_comparators_and_contains_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (let* ((qsort (egcl-ffi:foreign-symbol-pointer "qsort"))
                 (array (egcl-ffi:foreign-alloc 40))
                 (calls 0)
                 (compare (egcl-ffi:make-callback
                   (lambda (a b)
                     (incf calls)
                     (let ((pair (list (egcl-ffi:mem-ref a :int)
                                       (egcl-ffi:mem-ref b :int))))
                       (- (car pair) (cadr pair))))
                   :int '(:pointer :pointer))))
            (loop for value in '(7 2 10 4 3 5 1 6 9 8) for i from 0
                  do (egcl-ffi:mem-set value array :int (* 4 i)))
            (egcl-ffi:foreign-call qsort :void '(:pointer :unsigned-long :unsigned-long :pointer)
              (list array 10 4 (egcl-ffi:callback-pointer compare)))
            (assert (> calls 0))
            (assert (equal (loop for i below 10 collect (egcl-ffi:mem-ref array :int (* 4 i)))
                           '(1 2 3 4 5 6 7 8 9 10)))
            (egcl-ffi:free-callback compare)
            (let ((bad (egcl-ffi:make-callback (lambda (a b) (error "comparator failed"))
                                               :int '(:pointer :pointer))))
              (assert (handler-case
                        (progn (egcl-ffi:foreign-call qsort :void
                                 '(:pointer :unsigned-long :unsigned-long :pointer)
                                 (list array 10 4 (egcl-ffi:callback-pointer bad))) nil)
                        (egcl-ffi:ffi-error () t)))
              (egcl-ffi:free-callback bad))
            (egcl-ffi:foreign-free array))
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
        "(egcl-ffi:make-callback (lambda () 1) :int nil)",
        "(funcall #'egcl::%foreign-callback :make (lambda () 1) :int nil)",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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

#[cfg(unix)]
#[test]
fn lisp_closures_cross_real_c_frames_with_explicit_callback_lifetimes() {
    let dir = std::env::temp_dir().join(format!("egcl-callback-cli-{}", std::process::id()));
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
      (let* ((library (egcl-ffi:load-foreign-library {:?}))
             (caller (egcl-ffi:foreign-symbol-pointer "call_lisp" library))
             (returned (egcl-ffi:foreign-alloc 4))
             (bias 2.5d0)
             (callback (egcl-ffi:make-callback (lambda (x) (+ bias x)) :double '(:double))))
        (egcl-ffi:mem-set 0 returned :int)
        (assert (typep callback 'egcl-ffi:foreign-callback))
        (assert (not (egcl-ffi:pointerp callback)))
        (assert (= 3.75d0 (egcl-ffi:foreign-call caller :double '(:pointer :double :pointer)
                          (list (egcl-ffi:callback-pointer callback) 1.25d0 returned))))
        (assert (= 1 (egcl-ffi:mem-ref returned :int)))
        (assert (null (egcl-ffi:callback-error callback)))
        (let ((nested (egcl-ffi:make-callback
                        (lambda (x) (egcl-ffi:foreign-call
                          (egcl-ffi:callback-pointer callback) :double '(:double) (list x)))
                        :double '(:double))))
          (assert (= 4.5d0 (funcall #'egcl-ffi:foreign-call
                             (funcall #'egcl-ffi:callback-pointer nested) :double '(:double) '(2d0))))
          (egcl-ffi:free-callback nested))
        (let ((active nil))
          (setf active (egcl-ffi:make-callback
                         (lambda (x) (assert (handler-case (progn (egcl-ffi:free-callback active) nil)
                                              (egcl-ffi:ffi-error () t))) x)
                         :double '(:double)))
          (assert (= 7d0 (egcl-ffi:foreign-call (egcl-ffi:callback-pointer active) :double '(:double) '(7d0))))
          (egcl-ffi:free-callback active))
        (dolist (function (list (lambda (x) (error "callback failed"))
                               (lambda (x) (throw 'outside x))))
          (let ((bad (egcl-ffi:make-callback function :double '(:double))))
            (assert (eq :contained
              (catch 'outside
                (handler-case
                  (egcl-ffi:foreign-call caller :double '(:pointer :double :pointer)
                    (list (egcl-ffi:callback-pointer bad) 1d0 returned))
                  (egcl-ffi:ffi-error () :contained)))))
            (assert (stringp (egcl-ffi:callback-error bad)))
            (assert (null (egcl-ffi:callback-error bad)))
            (egcl-ffi:free-callback bad)))
        (assert (= 3 (egcl-ffi:mem-ref returned :int)))
        (egcl-ffi:free-callback callback)
        (assert (handler-case (progn (egcl-ffi:callback-pointer callback) nil) (egcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (egcl-ffi:free-callback callback) nil) (egcl-ffi:ffi-error () t)))
        (egcl-ffi:foreign-free returned)
        (egcl-ffi:close-foreign-library library))
      (format t "LISP-CALLBACK-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
      (let ((callback (egcl-ffi:make-callback (lambda () *callback-binding*) :int nil)))
        (let ((*callback-binding* 42))
          (assert (= 42 (egcl-ffi:foreign-call (egcl-ffi:callback-pointer callback) :int nil nil))))
        (assert (= 2 *callback-binding*))
        (egcl-ffi:free-callback callback))
      (assert (eq :contained (block outside
        (let ((callback (egcl-ffi:make-callback (lambda () (return-from outside :escaped)) :int nil)))
          (unwind-protect
            (handler-case (egcl-ffi:foreign-call (egcl-ffi:callback-pointer callback) :int nil nil)
              (egcl-ffi:ffi-error () :contained))
            (egcl-ffi:free-callback callback))))))
      (assert (handler-case (progn (egcl-ffi:make-callback 42 :int nil) nil) (type-error () t)))
      (assert (handler-case (progn (egcl-ffi:make-callback (lambda () 0) :int '(:void)) nil) (egcl-ffi:ffi-error () t)))
      (assert (handler-case (progn (egcl-ffi:make-callback (lambda () 0) :int '(:int . :int)) nil) (egcl-ffi:ffi-error () t)))
      (format t "CALLBACK-CONTROL-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
#[cfg(unix)]
#[test]
fn foreign_created_thread_can_invoke_a_retained_lisp_closure() {
    let dir =
        std::env::temp_dir().join(format!("egcl-callback-thread-cli-{}", std::process::id()));
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
      (let* ((library (egcl-ffi:load-foreign-library {:?}))
             (caller (egcl-ffi:foreign-symbol-pointer "call_thread" library))
             (bias 2.5d0)
             (callback (egcl-ffi:make-callback (lambda (x) (+ bias x)) :double '(:double))))
        (assert (= 3.75d0 (egcl-ffi:foreign-call caller :double '(:pointer)
                           (list (egcl-ffi:callback-pointer callback)))))
        (assert (null (egcl-ffi:callback-error callback)))
        (egcl-ffi:free-callback callback)
        (let ((bad (egcl-ffi:make-callback (lambda (x) (error "foreign-thread callback")) :double '(:double))))
          (assert (= 0d0 (egcl-ffi:foreign-call caller :double '(:pointer) (list (egcl-ffi:callback-pointer bad)))))
          (assert (stringp (egcl-ffi:callback-error bad)))
          (assert (null (egcl-ffi:callback-error bad)))
          (egcl-ffi:free-callback bad))
        (egcl-ffi:close-foreign-library library))
      (format t "FOREIGN-THREAD-CALLBACK-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
