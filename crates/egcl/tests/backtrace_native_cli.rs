// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Installed native identity must survive a suspended mixed-tier call chain.
#![cfg(all(
    target_pointer_width = "64",
    any(
        all(unix, any(target_arch = "x86_64", target_arch = "aarch64")),
        all(
            target_vendor = "unknown",
            target_os = "linux",
            target_env = "gnu",
            any(
                all(target_arch = "powerpc64", target_endian = "little"),
                target_arch = "s390x"
            )
        ),
        all(windows, target_arch = "x86_64")
    )
))]
use std::process::Command;

fn native_trace(tier: &str, number: u8, redefine: bool) {
    let redefine = if redefine { "t" } else { "nil" };
    let program = format!(
        r#"
      (defvar *native-trace-ready* nil)
      (defun native-trace-leaf (value lock inspect)
        (when inspect (setq *native-trace-ready* t))
        (egcl-thread:with-mutex (lock) nil)
        (assert (= value 42))
        :done)
      (defun native-trace-middle (value lock inspect)
        (native-trace-leaf (+ value 1) lock inspect))
      (defun replacement-middle (value lock inspect)
        (declare (ignore value lock inspect)) :replacement)
      (let* ((lock (egcl-thread:make-mutex))
             (held (egcl-thread:grab-mutex lock))
             (fiber (egcl-fiber:make-fiber
                      (lambda ()
                        (let ((warm-lock (egcl-thread:make-mutex)))
                          (dotimes (i 100) (native-trace-middle 41 warm-lock nil)))
                        (assert (= {number} (egcl-ext:function-tier 'native-trace-middle)))
                        (native-trace-middle 41 lock t))))
             (group (egcl-fiber:start-fibers (list fiber) :carrier-count 1)))
        (unwind-protect
            (let ((redefined nil)
                  (deadline (+ (get-internal-real-time)
                               (* (if (egcl-ext:getenv "EGCL_GC_STRESS") 120 10)
                                  internal-time-units-per-second))))
              (loop
                (assert (< (get-internal-real-time) deadline))
                (when (eq :dead (egcl-fiber:fiber-state fiber))
                  (egcl-fiber:fiber-join fiber)
                  (error "Fiber exited before capture"))
                (when *native-trace-ready*
                  (when (and {redefine} (not redefined))
                    (setf (symbol-function 'native-trace-middle)
                          (symbol-function 'replacement-middle))
                    (setq redefined t))
                  (let* ((stream (make-string-output-stream))
                         (text (handler-case
                                 (progn
                                   (egcl-fiber:print-fiber-backtrace fiber :stream stream)
                                   (get-output-stream-string stream))
                                 (egcl-fiber:fiber-still-running () nil))))
                    (when text (format t "~A" text) (return))))
                (sleep 0.001)))
          (egcl-thread:release-mutex lock))
        (assert (equal '(:done) (egcl-fiber:finish-fibers group)))
        (when {redefine} (assert (eq :replacement (native-trace-middle 0 nil nil)))))
      (format t "NATIVE-TRACE-OK~%")
    "#
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_T2",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_LAZY_COMPILE",
        "EGCL_PROFILING_DISABLED",
        "EGCL_NATIVE_TRANSFER",
        "EGCL_BACKEND",
    ] {
        command.env_remove(key);
    }
    let output = command
        .args(["--no-init", "--eval", &program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .expect("run native trace");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("NATIVE-TRACE-OK"),
        "{tier}: {stdout}\n{stderr}"
    );
    let native = "(COMMON-LISP-USER::NATIVE-TRACE-MIDDLE 41 ";
    assert_eq!(
        stdout.matches(native).count(),
        1,
        "{tier}: {stdout}\n{stderr}"
    );
    let leaf = stdout
        .find("(COMMON-LISP-USER::NATIVE-TRACE-LEAF 42 ")
        .expect("T0 leaf arguments must be present");
    assert!(leaf < stdout.find(native).unwrap(), "{tier}: {stdout}");
}

#[test]
fn t1_identity_in_suspended_mixed_trace() {
    native_trace("t1", 1, false);
    native_trace("t1", 1, true);
}

#[test]
fn t2_identity_in_suspended_mixed_trace() {
    native_trace("t2", 2, false);
    native_trace("t2", 2, true);
}

#[test]
fn dwarf_string_encoding_does_not_prevent_native_promotion() {
    for (tier, number) in [("t1", 1), ("t2", 2)] {
        // Command arguments cannot contain NUL; load a real source file so the
        // named call goes through the normal compiler and promotion path.
        let program = format!(
            "(defun |DWARF-\0-NAME| (x) (+ x 1))\n\
             (dotimes (i 100) (assert (= 42 (|DWARF-\0-NAME| 41))))\n\
             (assert (= {number} (egcl-ext:function-tier '|DWARF-\0-NAME|)))\n\
             (format t \"DWARF-NUL-NAME-OK~%\")\n"
        );
        let path = std::env::temp_dir().join(format!(
            "egcl-dwarf-name-{}-{tier}.lisp",
            std::process::id()
        ));
        std::fs::write(&path, program).expect("write Lisp source with NUL name");
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--load"])
            .arg(&path)
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_DISABLE_T2")
            .env_remove("EGCL_T2_DISCARD")
            .output()
            .expect("run unusual native function name");
        std::fs::remove_file(path).expect("remove temporary Lisp source");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("DWARF-NUL-NAME-OK"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
