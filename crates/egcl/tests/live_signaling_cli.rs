// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Returning signals and selected restart transfers through protected native bodies.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn check_live_signaling(native: bool, body: &str, expected_values: &str, expected_events: &str) {
    let listing = if native {
        "(disassemble 'live-caller)"
    } else {
        ""
    };
    let program = format!(
        r#"
        (defvar *live-events* nil)
        (defvar *live-context* :outside)
        (defvar *live-result* nil)
        (defun live-mark (event)
          (push (list event *live-context*) *live-events*))
        {body}
        (format t "VALUES ~S~%" *live-result*)
        (format t "EVENTS ~S~%" (reverse *live-events*))
        (format t "CONTEXT ~S~%" *live-context*)
        {listing}
        "#
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", &program])
        .env("EGCL_NATIVE_TRANSFER", if native { "1" } else { "0" })
        .env("EGCL_LAZY_COMPILE", "0")
        .env_remove("EGCL_FORCE_TIER")
        .env("EGCL_T0_T1_THRESHOLD", "10")
        .env("EGCL_T2_THRESHOLD", "4000000000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !native {
        command.env("EGCL_FORCE_TIER", "interp");
    }
    let mut child = command.spawn().expect("run EGCL");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut timed_out = false;
    while child.try_wait().expect("poll EGCL").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop hung EGCL");
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect EGCL output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !timed_out && output.status.success(),
        "native={native}, timeout={timed_out}, {}\n{stdout}\n{stderr}",
        output.status
    );
    if native {
        // This marker is emitted only for NativeCodeStorage::Mapped. The
        // measured call follows warmup, and run_native dispatches this storage
        // directly to TransferCode::run. Correct interpreted output is insufficient.
        assert!(
            stdout.contains("tagged baseline native with mapped exceptional transfers"),
            "protected caller must be installed with the mapped ABI: {stdout}"
        );
    }
    for expected in [
        format!("VALUES {expected_values}"),
        format!("EVENTS {expected_events}"),
        "CONTEXT :OUTSIDE".into(),
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "native={native}, missing {expected:?}\n{stdout}\n{stderr}"
        );
    }
}

const CERROR_CONTINUES: &str = r#"
    (defun live-handler (condition)
      (declare (ignore condition))
      (live-mark :handler)
      (invoke-restart 'continue))
    (defun live-cerror () (cerror "Continue" "correctable error"))
    (defun live-caller ()
      (unwind-protect
          (progn (live-cerror) (live-mark :resumed)
                 (values :continued (list :secondary)))
        (live-mark :cleanup)))
    (dotimes (i 31)
      (setq *live-events* nil)
      (let ((*live-context* :invocation))
        (handler-bind ((error #'live-handler))
          (setq *live-result* (multiple-value-list (live-caller))))))
"#;

fn check_cerror_continues(native: bool) {
    check_live_signaling(
        native,
        CERROR_CONTINUES,
        "(:CONTINUED (:SECONDARY))",
        "((:HANDLER :INVOCATION) (:RESUMED :INVOCATION) (:CLEANUP :INVOCATION))",
    );
}

#[test]
fn interpreter_cerror_continues_before_cleanup() {
    check_cerror_continues(false);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]
fn native_cerror_continues_before_cleanup() {
    check_cerror_continues(true);
}

const OUTER_CONTINUE: &str = r#"
    (defvar *outer-continue* nil)
    (defun live-handler (condition)
      (declare (ignore condition))
      (live-mark :handler)
      (invoke-restart *outer-continue*))
    (defun live-cerror () (cerror "Continue locally" "select outer restart"))
    (defun live-caller ()
      (unwind-protect
          (progn (live-cerror) (live-mark :incorrectly-resumed) :wrong)
        (live-mark :cleanup)))
    (dotimes (i 31)
      (setq *live-events* nil)
      (let ((*live-context* :established))
        (setq *live-result*
          (multiple-value-list
            (restart-case
                (progn
                  (setq *outer-continue* (find-restart 'continue))
                  (let ((*live-context* :invocation))
                    (handler-bind ((error #'live-handler)) (live-caller))))
              (continue () (live-mark :outer-clause)
                (values :outer (list :secondary))))))))
"#;

fn check_outer_continue(native: bool) {
    check_live_signaling(
        native,
        OUTER_CONTINUE,
        "(:OUTER (:SECONDARY))",
        "((:HANDLER :INVOCATION) (:CLEANUP :INVOCATION) (:OUTER-CLAUSE :ESTABLISHED))",
    );
}

#[test]
fn interpreter_cerror_preserves_selected_outer_restart() {
    check_outer_continue(false);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]
fn native_cerror_preserves_selected_outer_restart() {
    check_outer_continue(true);
}

const DECLINING_SIGNAL: &str = r#"
    (defun live-inner (condition)
      (declare (ignore condition)) (live-mark :inner) (values :ignored :secondary))
    (defun live-outer (condition)
      (declare (ignore condition)) (live-mark :outer) (values :also-ignored))
    (defun live-signal () (signal "declining handlers"))
    (defun live-caller ()
      (unwind-protect
          (progn (live-signal) (live-mark :resumed)
                 (values :returned (list :secondary)))
        (live-mark :cleanup)))
    (dotimes (i 31)
      (setq *live-events* nil)
      (let ((*live-context* :invocation))
        (handler-bind ((condition #'live-outer))
          (handler-bind ((condition #'live-inner))
            (setq *live-result* (multiple-value-list (live-caller)))))))
"#;

fn check_declining_signal(native: bool) {
    check_live_signaling(
        native,
        DECLINING_SIGNAL,
        "(:RETURNED (:SECONDARY))",
        "((:INNER :INVOCATION) (:OUTER :INVOCATION) (:RESUMED :INVOCATION) (:CLEANUP :INVOCATION))",
    );
}

#[test]
fn interpreter_declining_handlers_return_before_cleanup() {
    check_declining_signal(false);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux", not(egcl_no_disassembly)))]
fn native_declining_handlers_return_before_cleanup() {
    check_declining_signal(true);
}
