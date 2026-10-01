// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! R2.14: Lisp calls use native aggregate ABI adapters; R8.01: sandbox denial.
#![cfg(all(target_arch = "x86_64", any(target_os = "linux", windows)))]
use std::process::Command;

#[test]
fn aggregate_calls_are_denied_in_both_sandbox_dispatch_paths() {
    for form in [
        "(egcl-ffi:foreign-call-buffered nil :void nil nil nil)",
        "(funcall #'egcl::%ffi-call-buffered nil :void nil nil nil)",
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

#[test]
fn lisp_native_buffers_support_aggregates_and_checked_lifetimes() {
    #[cfg(unix)]
    let library = {
        let dir = std::env::temp_dir().join(format!("egcl-aggregate-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let library = dir.join("aggregates.so");
        // Keep this fixture independent of the host libc: distro compiler
        // defaults can introduce __stack_chk_fail, which the musl ELF loader
        // cannot resolve. The runtime aggregate fixture uses the same flag.
        assert!(Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-fno-stack-protector"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../egcl-rt/tests/fixtures/ffi_aggregates.c"
            ))
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success());
        library
    };
    #[cfg(windows)]
    let library = std::path::PathBuf::from(std::env::var("EGCL_FFI_AGGREGATES_DLL").unwrap());
    let program = format!(
        r#"
      (defun ffi-fails (thunk)
        (assert (handler-case (progn (funcall thunk) nil) (egcl-ffi:ffi-error () t))))
      (let* ((library (egcl-ffi:load-foreign-library {:?}))
             (pair '(:struct :uint64 :double))
             (big '(:struct :uint64 :uint64 :uint64))
             (packed '(:packed-struct :uint8 :double))
             (nested '(:struct (:union :uint64 :double) :float :float))
             (a (egcl-ffi:foreign-alloc 24))
             (b (egcl-ffi:foreign-alloc 8))
             (out (egcl-ffi:foreign-alloc 24)))
        (assert (= 16 (egcl-ffi:foreign-type-size pair)))
        (assert (= 8 (egcl-ffi:foreign-type-alignment pair)))
        (assert (= 9 (egcl-ffi:foreign-type-size packed)))
        (assert (= 1 (egcl-ffi:foreign-type-alignment packed)))
        (assert (= 16 (egcl-ffi:foreign-type-size nested)))
        (egcl-ffi:mem-set 31 a :uint64)
        (egcl-ffi:mem-set 1.25d0 a :double 8)
        (assert (egcl-ffi:pointer-eq out
          (funcall #'egcl-ffi:foreign-call-buffered
            (egcl-ffi:foreign-symbol-pointer "aggregate_is" library)
            pair (list pair) (list a) out)))
        (assert (= 42 (egcl-ffi:mem-ref out :uint64)))
        (assert (= 2.5d0 (egcl-ffi:mem-ref out :double 8)))
        (egcl-ffi:mem-set 1 a :uint64)
        (egcl-ffi:mem-set 2 a :uint64 8)
        (egcl-ffi:mem-set 3 a :uint64 16)
        (egcl-ffi:mem-set 5 b :uint64)
        (egcl-ffi:foreign-call-buffered
          (egcl-ffi:foreign-symbol-pointer "aggregate_big" library)
          big (list big :uint64) (list a b) out)
        (assert (= 6 (egcl-ffi:mem-ref out :uint64)))
        (assert (= 12 (egcl-ffi:mem-ref out :uint64 8)))
        (assert (= 18 (egcl-ffi:mem-ref out :uint64 16)))
        (egcl-ffi:mem-set 7 a :uint8)
        (egcl-ffi:mem-set 2.25d0 a :double 1)
        (egcl-ffi:foreign-call-buffered
          (egcl-ffi:foreign-symbol-pointer "aggregate_packed" library)
          packed (list packed) (list a) out)
        (assert (= 8 (egcl-ffi:mem-ref out :uint8)))
        (assert (= 4.5d0 (egcl-ffi:mem-ref out :double 1)))
        (egcl-ffi:mem-set 9 a :uint64)
        (egcl-ffi:mem-set 1.5 a :float 8)
        (egcl-ffi:mem-set 2.5 a :float 12)
        (egcl-ffi:foreign-call-buffered
          (egcl-ffi:foreign-symbol-pointer "aggregate_union" library)
          nested (list nested) (list a) out)
        (assert (= 10 (egcl-ffi:mem-ref out :uint64)))
        (assert (= 3.5 (egcl-ffi:mem-ref out :float 8)))
        (assert (= 5.5 (egcl-ffi:mem-ref out :float 12)))
        ;; Variadic aggregate plus default-promoted float/short.
        (let ((f (egcl-ffi:foreign-alloc 4)) (s (egcl-ffi:foreign-alloc 2)))
          (egcl-ffi:mem-set 1 b :int)
          (egcl-ffi:mem-set 3 a :uint64)
          (egcl-ffi:mem-set 4d0 a :double 8)
          (egcl-ffi:mem-set 2.5 f :float)
          (egcl-ffi:mem-set -2 s :short)
          (egcl-ffi:foreign-call-buffered
            (egcl-ffi:foreign-symbol-pointer "aggregate_variadic" library)
            :double (list :int pair :float :short) (list b a f s) out 1)
          (assert (= 7.5d0 (egcl-ffi:mem-ref out :double)))
          (egcl-ffi:foreign-free f) (egcl-ffi:foreign-free s))
        ;; Malformed descriptors and buffer lists must fail before entering C.
        (let ((fn (egcl-ffi:foreign-symbol-pointer "aggregate_is" library)))
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) (list b) out)))
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) (list a) b)))
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) nil out)))
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) (list a) out 2)))
          (dolist (type '((:struct) (:struct :void) (:union) (:struct . :int) (:unknown :int)))
            (ffi-fails (lambda () (egcl-ffi:foreign-type-size type))))
          (let ((cycle (list :struct :int)))
            (setf (cdr (cdr cycle)) cycle)
            (ffi-fails (lambda () (egcl-ffi:foreign-type-size cycle))))
          (let ((cycle (list :struct nil)))
            (setf (car (cdr cycle)) cycle)
            (ffi-fails (lambda () (egcl-ffi:foreign-type-size cycle))))
          (let ((cycle (list a)))
            (setf (cdr cycle) cycle)
            (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) cycle out))))
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) (cons a 1) out)))
          (let ((cycle (list pair)))
            (setf (cdr cycle) cycle)
            (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair cycle (list a) out))))
          (let ((deep :int))
            (dotimes (i 66) (setf deep (list :struct deep)))
            (ffi-fails (lambda () (egcl-ffi:foreign-type-size deep))))
          (egcl-ffi:foreign-free b)
          (ffi-fails (lambda () (egcl-ffi:foreign-call-buffered fn pair (list pair) (list b) out))))
        ;; C calls Lisp (allocating under GC stress) then returns an aggregate.
        (let* ((marker (egcl-ffi:foreign-alloc 4))
               (callback-slot (egcl-ffi:foreign-alloc 8))
               (marker-slot (egcl-ffi:foreign-alloc 8))
               (fn (egcl-ffi:foreign-symbol-pointer "aggregate_callback" library)))
          (egcl-ffi:mem-set marker marker-slot :pointer)
          (dolist (mode '(good error free))
            (let ((callback (egcl-ffi:make-callback
                              (lambda (x)
                                (assert (equal (list x 2 3) (list 1.25d0 2 3)))
                                (case mode
                                  (error (error "aggregate callback failed"))
                                  (free (egcl-ffi:foreign-free out)))
                                42d0)
                              :double '(:double))))
              (egcl-ffi:mem-set 0 marker :int)
              (egcl-ffi:mem-set 123 out :uint64)
              (egcl-ffi:mem-set (egcl-ffi:callback-pointer callback) callback-slot :pointer)
              (flet ((invoke () (funcall #'egcl::%ffi-call-buffered fn big '(:pointer :pointer)
                                   (list callback-slot marker-slot) out)))
                (if (eq mode 'good) (assert (egcl-ffi:pointer-eq out (invoke))) (ffi-fails #'invoke)))
              (assert (= 99 (egcl-ffi:mem-ref marker :int)))
              (case mode
                (good (assert (= 42 (egcl-ffi:mem-ref out :uint64))))
                (error (assert (= 123 (egcl-ffi:mem-ref out :uint64)))
                       (assert (stringp (egcl-ffi:callback-error callback)))))
              (egcl-ffi:free-callback callback)))
          (egcl-ffi:foreign-free marker)
          (egcl-ffi:foreign-free callback-slot)
          (egcl-ffi:foreign-free marker-slot))
        (let* ((called nil)
               (callback (egcl-ffi:make-callback (lambda () (setf called (list 1 2 3))) :void nil)))
          (assert (null (egcl-ffi:foreign-call-buffered (egcl-ffi:callback-pointer callback)
                         :void nil nil (egcl-ffi:null-pointer))))
          (assert (equal called '(1 2 3)))
          (egcl-ffi:free-callback callback))
        (egcl-ffi:foreign-free a)
        (egcl-ffi:close-foreign-library library))
      (format t "LISP-AGGREGATE-OK~%")
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("LISP-AGGREGATE-OK"));
}
