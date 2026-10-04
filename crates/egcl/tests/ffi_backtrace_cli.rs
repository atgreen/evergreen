// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Callback failures must survive the C containment boundary with their original
//! Lisp call chain. This does not assert native C internal-frame symbolization.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

#[test]
fn callback_errors_keep_their_original_chain_after_c_returns() {
    let directory = std::env::temp_dir().join(format!("egcl-ffi-backtrace-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("callback.c");
    let library = directory.join("callback.so");
    std::fs::write(
        &source,
        r#"
        int call_lisp_trace(int (*callback)(int), int value, int *returned) {
            int answer = callback(value);
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
    for (tier, installed) in [("interp", 0), ("t0", 0), ("t1", 1), ("t2", 2)] {
        for (failure, keep_home) in [
            ("(error \"callback failure\")", false),
            ("callback-missing-variable", true),
            ("callback-missing-variable", false),
        ] {
            // A call after the failing branch keeps the argument in a managed
            // home. Also retain the optimized register-entry case and require
            // explicit unavailability there, rather than guessing its argument.
            let body = format!("(if *fail-callback* {failure} (+ value 1))");
            let body = if keep_home {
                format!("(prog1 {body} (funcall *trace-keep-value* value))")
            } else {
                body
            };
            let t2_log = directory.join("t2.log");
            std::fs::write(&t2_log, "").unwrap();
            let program = format!(
                r#"
              (defparameter *trace-library* (egcl-ffi:load-foreign-library {:?}))
              (defparameter *trace-caller* (egcl-ffi:foreign-symbol-pointer "call_lisp_trace" *trace-library*))
              (defparameter *trace-returned* (egcl-ffi:foreign-alloc 4))
              (defparameter *fail-callback* nil)
              (defparameter *trace-keep-value* #'identity)
              (defun callback-trace-leaf (value) {body})
              (defparameter *trace-leaf* #'callback-trace-leaf)
              (defun callback-trace-middle (value) (+ (funcall *trace-leaf* value) 1))
              (defparameter *trace-callback* (egcl-ffi:make-callback #'callback-trace-middle :int '(:int)))
              (defun callback-trace-outer (value)
                (egcl-ffi:foreign-call *trace-caller* :int '(:pointer :int :pointer)
                  (list (egcl-ffi:callback-pointer *trace-callback*) value *trace-returned*)))
              (dotimes (i 100) (assert (= 43 (callback-trace-outer 41))))
              (dolist (name '(callback-trace-leaf callback-trace-middle callback-trace-outer))
                (assert (= {installed} (egcl-ext:function-tier name))))
              (format t "CALLBACK-TIERS-CONFIRMED~%")
              (setq *fail-callback* t)
              (handler-bind ((egcl-ffi:ffi-error
                               (lambda (condition)
                                 (assert (= 101 (egcl-ffi:mem-ref *trace-returned* :int)))
                                 (format t "C-RETURNED-BEFORE-SIGNAL~%"))))
                (callback-trace-outer 41))
            "#,
                library.to_str().unwrap()
            );
            let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
                .args(["--no-init", "--eval", &program])
                .env("EGCL_FORCE_TIER", tier)
                .env("EGCL_T2_LOG", &t2_log)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stdout.contains("CALLBACK-TIERS-CONFIRMED"),
                "{tier}/{failure}: {stdout}\n{stderr}"
            );
            assert!(
                stdout.contains("C-RETURNED-BEFORE-SIGNAL"),
                "{tier}/{failure}: {stdout}\n{stderr}"
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "{tier}/{failure}: {stdout}\n{stderr}"
            );
            assert!(
                stderr.contains("FFI error: foreign callback failed:"),
                "{tier}/{failure}: {stderr}"
            );
            let names = [
                "CALLBACK-TRACE-LEAF",
                "CALLBACK-TRACE-MIDDLE",
                "CALLBACK-TRACE-OUTER",
            ];
            let register_entry =
                tier == "t2" && failure == "callback-missing-variable" && !keep_home;
            if tier == "t2" {
                let log = std::fs::read_to_string(&t2_log).unwrap();
                let installed = log
                    .lines()
                    .find(|line| line.contains("CALLBACK-TRACE-LEAF: T2 INSTALLED"))
                    .expect("leaf must have native emission evidence");
                assert_eq!(
                    !installed.contains("compiled_entry=+0,"),
                    register_entry,
                    "{installed}"
                );
            }
            let trace_start = stderr.find("Backtrace (").expect("terminal backtrace");
            let trace = &stderr[trace_start..];
            let boundary = trace
                .find("[foreign] call_lisp_trace <arguments unavailable>")
                .unwrap_or_else(|| panic!("missing C boundary: {tier}/{failure}: {stderr}"));
            assert!(boundary > trace.find("CALLBACK-TRACE-MIDDLE").unwrap());
            assert!(boundary < trace.find("CALLBACK-TRACE-OUTER").unwrap());
            let mut previous = 0;
            for name in names {
                assert_eq!(trace.matches(name).count(), 1, "{tier}/{failure}: {stderr}");
                let argument = if name == "CALLBACK-TRACE-LEAF" && register_entry {
                    "<arguments unavailable>"
                } else {
                    "41"
                };
                let position = trace
                    .find(&format!("{name} {argument})"))
                    .unwrap_or_else(|| panic!("{tier}/{failure}: {stderr}"));
                assert!(position > previous, "{tier}/{failure}: {stderr}");
                previous = position;
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn nested_foreign_boundaries_follow_execution_and_survive_library_close() {
    let directory =
        std::env::temp_dir().join(format!("egcl-ffi-boundaries-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("nested.c");
    let library = directory.join("nested.so");
    std::fs::write(
        &source,
        r#"
        int trace_inner_c(int (*callback)(int), int value) { return callback(value); }
        int trace_outer_c(int (*callback)(int), int value) { return callback(value); }
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
    for (tier, installed) in [("interp", 0), ("t0", 0), ("t1", 1), ("t2", 2)] {
        let program = format!(
            r#"
          (defparameter *boundary-library* (egcl-ffi:load-foreign-library {:?}))
          (defparameter *inner-c* (egcl-ffi:foreign-symbol-pointer "trace_inner_c" *boundary-library*))
          (defparameter *outer-c* (egcl-ffi:foreign-symbol-pointer "trace_outer_c" *boundary-library*))
          (defparameter *capture-boundary* nil)
          (defparameter *boundary-snapshot* nil)
          (defun boundary-leaf (value)
            (when *capture-boundary*
              (setq *boundary-snapshot* (egcl-debug:list-backtrace :count 100)))
            (+ value 1))
          (defparameter *inner-callback* (egcl-ffi:make-callback #'boundary-leaf :int '(:int)))
          (defun boundary-middle (value)
            (egcl-ffi:foreign-call *inner-c* :int '(:pointer :int)
              (list (egcl-ffi:callback-pointer *inner-callback*) value)))
          (defparameter *outer-callback* (egcl-ffi:make-callback #'boundary-middle :int '(:int)))
          (defun boundary-outer (value)
            (egcl-ffi:foreign-call *outer-c* :int '(:pointer :int)
              (list (egcl-ffi:callback-pointer *outer-callback*) value)))
          (dotimes (i 100) (assert (= 42 (boundary-outer 41))))
          (dolist (name '(boundary-leaf boundary-middle boundary-outer))
            (assert (= {installed} (egcl-ext:function-tier name))))
          (defun check-boundary-snapshot ()
            (let ((foreign (remove-if-not
                             (lambda (frame) (eq :foreign (getf frame :origin)))
                             *boundary-snapshot*)))
              (assert (equal '("trace_inner_c" "trace_outer_c")
                             (mapcar (lambda (frame) (getf frame :function)) foreign)))
              (dolist (frame foreign)
                (assert (not (getf frame :arguments-available-p)))
                (assert (null (getf frame :arguments)))))
            (let ((names (mapcar (lambda (frame) (getf frame :function)) *boundary-snapshot*)))
              (let ((expected '("COMMON-LISP-USER::BOUNDARY-LEAF" "trace_inner_c"
                                "COMMON-LISP-USER::BOUNDARY-MIDDLE" "trace_outer_c"
                                "COMMON-LISP-USER::BOUNDARY-OUTER")))
                (assert (equal expected
                               (remove-if-not (lambda (name) (member name expected :test #'equal))
                                              names))))))
          (setq *capture-boundary* t)
          (assert (= 42 (boundary-outer 41)))
          (check-boundary-snapshot)
          (let* ((fiber (egcl-fiber:make-fiber
                          (lambda ()
                            (assert (= 42 (boundary-outer 41)))
                            (check-boundary-snapshot)
                            (egcl-fiber:fiber-yield)
                            (assert (notany (lambda (frame) (eq :foreign (getf frame :origin)))
                                            (egcl-debug:list-backtrace :count 100)))
                            :done)))
                 (group (egcl-fiber:start-fibers (list fiber) :carrier-count 2)))
            (assert (equal '(:done) (egcl-fiber:finish-fibers group))))
          (egcl-ffi:close-foreign-library *boundary-library*)
          (check-boundary-snapshot)
          (assert (notany (lambda (frame) (eq :foreign (getf frame :origin)))
                          (egcl-debug:list-backtrace :count 100)))
          (format t "NESTED-FOREIGN-BOUNDARIES-OK~%")
        "#,
            library.to_str().unwrap()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("NESTED-FOREIGN-BOUNDARIES-OK"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
