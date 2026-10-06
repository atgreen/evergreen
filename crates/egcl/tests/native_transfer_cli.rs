// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A pending native-to-Lisp transfer must stop subsequent native side effects.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn bounded_native_probe(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(args)
        .env("EGCL_NATIVE_TRANSFER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run EGCL");
    let deadline = Instant::now() + Duration::from_secs(60);
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
    assert!(
        !timed_out,
        "native probe timed out; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn native_invoke_results_keep_loop_phis_conservative() {
    let program = r#"
        (defun scan-native-declarations (forms)
          (let ((result nil))
            (tagbody scan
              (unless (and (consp forms) (consp (car forms))
                           (eq (car (car forms)) 'declare))
                (go done))
              (setq result (cons (car forms) result) forms (cdr forms))
              (go scan)
              done)
            (values (reverse result) forms)))
        (dotimes (i 20)
          (assert (equal '(((declare (ignore x)) (declare (ignore y))) ((body)))
                         (multiple-value-list
                           (scan-native-declarations
                             '((declare (ignore x)) (declare (ignore y)) (body)))))))
        (format t "NATIVE-INVOKE-PHIS-OK~%")
    "#;
    let output = bounded_native_probe(&["--no-bootstrap", "--eval", program]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-INVOKE-PHIS-OK"));
}

#[test]
fn process_level_native_transfer_waits_until_bootstrap_finishes() {
    let output = bounded_native_probe(&["--no-init", "--eval", "(+ 1 2)"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3");
}

#[test]
fn t2_calls_stop_before_later_side_effects() {
    let program = r#"
        (defvar *after-transfer* 0)
        (defun t2-leaf (fail) (when fail (error "expected")) 1)
        (defun t2-middle (fail) (t2-leaf fail) (incf *after-transfer*))
        (defun t2-throw (fail) (when fail (throw 'escape (values 42 43))) 1)
        (defun t2-throw-middle (fail) (t2-throw fail) (incf *after-transfer*))
        (defun t2-builtin (value) (char-code value) (incf *after-transfer*))
        (defun t2-wide-leaf (a b c fail) (declare (ignore a b c)) (t2-leaf fail))
        (defun t2-wide (fail) (t2-wide-leaf 1 2 3 fail) (incf *after-transfer*))
        (defvar *transfer-global* 1)
        (defun t2-global () *transfer-global* (incf *after-transfer*))
        (defun t2-thunk (thunk) (funcall thunk) (incf *after-transfer*))
        (defvar *transfer-recursive-value* #\A)
        (defvar *transfer-recursive-depth* 0)
        (defun t2-self-next ()
          (when (> *transfer-recursive-depth* 0) (decf *transfer-recursive-depth*) t))
        (defun t2-self-leaf () (char-code *transfer-recursive-value*))
        (defun t2-self-after () (incf *after-transfer*))
        ;; No pointer-valued arguments need shadow slots, allowing the direct
        ;; register entry. Its presence is verified in the compiler trace below.
        (defun t2-self ()
          (if (t2-self-next) (t2-self) (t2-self-leaf))
          (t2-self-after))
        (dotimes (i 40)
          (t2-middle nil) (t2-throw-middle nil) (t2-builtin #\A)
          (t2-wide nil) (t2-global) (t2-thunk (lambda () 1))
          (setf *transfer-recursive-depth* 3) (t2-self))
        (dolist (name '(t2-middle t2-throw-middle t2-builtin t2-wide
                       t2-global t2-thunk t2-self))
          (assert (= 2 (egcl-ext:function-tier name))))
        (setf *after-transfer* 0)
        (let ((cleanups 0))
          (assert (eq :caught
            (handler-case (unwind-protect (t2-middle t) (incf cleanups))
              (error () :caught))))
          (assert (= cleanups 1)))
        (assert (= 0 *after-transfer*))
        (assert (equal '(42 43) (multiple-value-list (catch 'escape (t2-throw-middle t)))))
        (assert (= 0 *after-transfer*))
        (assert (eq :caught (handler-case (t2-builtin 1) (type-error () :caught))))
        (assert (= 0 *after-transfer*))
        (assert (eq :caught (handler-case (t2-wide t) (error () :caught))))
        (assert (= 0 *after-transfer*))
        (makunbound '*transfer-global*)
        (assert (eq :caught (handler-case (t2-global) (unbound-variable () :caught))))
        (assert (= 0 *after-transfer*))
        (assert (= 17 (block escape (t2-thunk (lambda () (return-from escape 17))))))
        (assert (= 0 *after-transfer*))
        (setf *transfer-recursive-value* 1)
        (setf *transfer-recursive-depth* 3)
        (assert (eq :caught (handler-case (t2-self) (type-error () :caught))))
        (assert (= 0 *after-transfer*))
        ;; An earlier transfer must not poison later successful calls, and the
        ;; probe must preserve heap-valued primaries and secondary values.
        (defun t2-values-leaf (x) (values (list x) 42))
        (defun t2-values-middle (x)
          (multiple-value-bind (a b) (t2-values-leaf x) (list a b)))
        (dotimes (i 40) (assert (equal '((17) 42) (t2-values-middle 17))))
        (assert (= 2 (egcl-ext:function-tier 't2-values-middle)))
        (format t "T2-TRANSFERS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T2_LOG", "1")
        .output()
        .expect("run EGCL");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("T2-TRANSFERS-OK"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let self_entry = stderr
        .lines()
        .find(|line| line.contains("T2-SELF: T2 INSTALLED"))
        .unwrap_or_else(|| panic!("recursive function must install T2 code:\n{stderr}"));
    assert!(
        !self_entry.contains("compiled_entry=+0,"),
        "recursive test must exercise the direct register entry: {self_entry}"
    );
}

#[test]
fn osr_calls_stop_before_later_side_effects() {
    let program = r#"
        (defvar *after-transfer* 0)
        (defun transfer-loop (n)
          (dotimes (i n)
            (char-code (if (= i 300) 1 #\A))
            (incf *after-transfer*)))
        (assert (eq :caught
          (handler-case (transfer-loop 5000) (type-error () :caught))))
        (assert (= 300 *after-transfer*))
        (assert (> (egcl-ext:function-osr-count 'transfer-loop) 0))
        (format t "OSR-TRANSFERS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_OSR_THRESHOLD", "50")
        .output()
        .expect("run EGCL");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("OSR-TRANSFERS-OK"));
}

#[test]
fn native_calls_stop_at_errors_and_nonlocal_exits() {
    let program = r#"
        (defvar *after-transfer* 0)
        (defun signal-leaf (fail) (when fail (error "expected")) 1)
        (defun throw-leaf (fail) (when fail (throw 'escape 42)) 1)
        (defun code-leaf (value) (char-code value))
        ;; Install the leaf's native entry before compiling its caller, so the
        ;; direct-call variant really bakes a native-to-native call site.
        (dotimes (i 40) (signal-leaf nil) (throw-leaf nil) (code-leaf #\A))
        (defun signal-middle (fail) (signal-leaf fail) (incf *after-transfer*))
        (defun throw-middle (fail) (throw-leaf fail) (incf *after-transfer*))
        (defun code-middle (value) (code-leaf value) (incf *after-transfer*))
        (defun builtin-middle (value) (char-code value) (incf *after-transfer*))
        (dotimes (i 40)
          (signal-middle nil)
          (throw-middle nil)
          (code-middle #\A)
          (builtin-middle #\A))
        (assert (= 1 (egcl-ext:function-tier 'signal-middle)))
        (assert (= 1 (egcl-ext:function-tier 'throw-middle)))
        (assert (= 1 (egcl-ext:function-tier 'builtin-middle)))
        (assert (= 1 (egcl-ext:function-tier 'code-middle)))
        (setf *after-transfer* 0)
        (assert (eq :caught (handler-case (signal-middle t) (error () :caught))))
        (assert (= 0 *after-transfer*))
        (assert (= 42 (catch 'escape (throw-middle t))))
        (assert (= 0 *after-transfer*))
        (assert (eq :caught (handler-case (builtin-middle 1) (type-error () :caught))))
        (assert (= 0 *after-transfer*))
        (assert (eq :caught (handler-case (code-middle 1) (type-error () :caught))))
        (assert (= 0 *after-transfer*))
        (format t "NATIVE-TRANSFERS-OK~%")
    "#;
    for direct in ["0", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T1_THRESHOLD", "1")
            .env("EGCL_T2", "0")
            .env("EGCL_NN_DIRECT", direct)
            .env("EGCL_LOG", "compile=trace")
            .output()
            .expect("run EGCL");
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-TRANSFERS-OK"));
        if direct == "1" {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("CODE-MIDDLE: direct call to CODE-LEAF [T1]"),
                "native-to-native path was not exercised:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn t2_transfer_checks_preserve_errors_across_fiber_yields() {
    let program = r#"
        (defun fiber-transfer-leaf (fail)
          (when (egcl-fiber:current-fiber) (egcl-fiber:fiber-yield))
          (when fail (error "expected fiber error"))
          7)
        (defun fiber-transfer-recursive (n fail)
          (if (= n 0) (fiber-transfer-leaf fail)
              (+ 1 (fiber-transfer-recursive (- n 1) fail))))
        (dotimes (i 40) (assert (= 11 (fiber-transfer-recursive 4 nil))))
        (assert (= 2 (egcl-ext:function-tier 'fiber-transfer-recursive)))
        (dolist (carriers '(1 4))
          (let ((fibers
                  (loop for n below 16 collect
                    (let ((n n))
                      (egcl-fiber:make-fiber
                        (lambda ()
                          (dotimes (i 10)
                            (assert (eq :caught
                              (handler-case (fiber-transfer-recursive 4 t)
                                (error () :caught))))
                            (assert (= 11 (fiber-transfer-recursive 4 nil))))
                          n))))))
            (assert (equal (loop for n below 16 collect n)
                           (egcl-fiber:run-fibers fibers :carrier-count carriers)))))
        (assert (= 11 (fiber-transfer-recursive 4 nil)))
        (format t "FIBER-TRANSFERS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .output()
        .expect("run EGCL");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FIBER-TRANSFERS-OK"));
}

#[test]
fn osr_transfers_preserve_inherited_handlers_and_cleanup_order() {
    // Shared with the s390x smoke suite: an expired CATCH must not catch,
    // unwinding GO must run cleanup exactly once, and outer GO must keep
    // the handler stack inherited from the interpreter consistent.
    let program = include_str!("fixtures/native-transfer-osr.lisp");
    let mut reference = None;
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--no-bootstrap", "--eval", program])
            .env_remove("EGCL_FORCE_TIER")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T0_T1_THRESHOLD", "1000000")
            .env("EGCL_OSR_THRESHOLD", "2")
            .env("EGCL_DISABLE_T2", "1")
            .env("EGCL_OSR_TRAPS", "1")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_POISON");
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run inherited-handler OSR oracle");
        assert!(
            output.status.success(),
            "stress={stress} stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("OSR-TRAP-OK"));
        if let Some(expected) = &reference {
            assert_eq!(
                &output.stdout, expected,
                "stress changed transfer semantics"
            );
        } else {
            reference = Some(output.stdout);
        }
    }
}

#[test]
fn native_reentry_preserves_resumption_and_cleanup_replacement() {
    let program = r#"
        (defvar *transfer-effects* 0)
        (defvar *transfer-binding* :outer)
        (defvar *cleanup-log* nil)
        (defun transfer-invoke (thunk)
          (funcall thunk)
          (incf *transfer-effects*))
        (dotimes (i 40) (transfer-invoke (lambda () 7)))
        (assert (= EXPECTED-TIER (egcl-ext:function-tier 'transfer-invoke)))
        (setq *transfer-effects* 0)
        ;; A selected restart returns normally through nested native reentry.
        ;; Its dynamic context must remain live while the handler runs.
        (let ((handled 0))
          (handler-bind ((simple-condition
                           (lambda (condition)
                             (declare (ignore condition))
                             (incf handled)
                             (assert (eq *transfer-binding* :inner))
                             (invoke-restart 'continue-transfer))))
            (let ((*transfer-binding* :inner))
              (transfer-invoke
                (lambda ()
                  (restart-case
                    (transfer-invoke (lambda () (signal "resume me")))
                    (continue-transfer () :resumed))))))
          (assert (= handled 1)))
        ;; The inner call escaped, the outer call resumed and ran its suffix.
        (assert (= *transfer-effects* 1))
        (assert (eq *transfer-binding* :outer))
        (setq *transfer-effects* 0)
        (assert (equal '(:replacement 43)
          (multiple-value-list
            (catch 'replacement
              (catch 'original
                (let ((*transfer-binding* :inner))
                  (unwind-protect
                    (unwind-protect
                      (transfer-invoke (lambda () (throw 'original :discarded)))
                      (push *transfer-binding* *cleanup-log*)
                      (transfer-invoke
                        (lambda () (throw 'replacement (values :replacement 43)))))
                    (push :outer-cleanup *cleanup-log*))))))))
        (assert (equal '(:outer-cleanup :inner) *cleanup-log*))
        (assert (eq *transfer-binding* :outer))
        (assert (= *transfer-effects* 0))
        (assert (= 1 (transfer-invoke (lambda () :normal))))
        (format t "NATIVE-REENTRY-OK~%")
    "#;
    for (tier, expected) in [("t0", "0"), ("t1", "1"), ("t2", "2")] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args([
                "--no-init",
                "--eval",
                &program.replace("EXPECTED-TIER", expected),
            ])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run native reentry oracle");
        assert!(
            output.status.success(),
            "tier={tier} stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE-REENTRY-OK"));
    }
}

#[test]
fn throw_values_survive_cleanup_in_bytecode_and_eval() {
    let cases = r#"
        (assert (equal nil
          (multiple-value-list
            (catch 'done (unwind-protect (throw 'done (values)) (values 9 10))))))
        (assert (equal '(7)
          (multiple-value-list
            (catch 'done (unwind-protect (throw 'done 7) (values 9 10))))))
        (assert (equal '((:primary) (:secondary))
          (multiple-value-list
            (catch 'done
              (unwind-protect
                (throw 'done (values (list :primary) (list :secondary)))
                (list :cleanup))))))
        (assert (equal '(3 4)
          (multiple-value-list
            (catch 'done
              (unwind-protect (throw 'done (values 1 2))
                (throw 'done (values 3 4)))))))
    "#;
    let program =
        format!("(progn {cases} (eval '(progn {cases})) (format t \"THROW-VALUES-OK~%\"))");
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &program])
        .env("EGCL_FORCE_TIER", "t0")
        .output()
        .expect("run THROW value oracles");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("THROW-VALUES-OK"));
}

#[test]
fn superseding_nested_cleanups_do_not_replay_outer_cleanup_suffixes() {
    let program = r#"
        (defvar *cleanup-effects* nil)
        (defun cleanup-return ()
          (unwind-protect (values (list :primary) (list :secondary))
            (block done (unwind-protect 3 (return-from done 4)))
            (push :suffix *cleanup-effects*)))
        (defun cleanup-go ()
          (unwind-protect (values (list :primary) (list :secondary))
            (tagbody (unwind-protect 3 (go done)) done)
            (push :suffix *cleanup-effects*)))
        (defun cleanup-throw ()
          (unwind-protect (values (list :primary) (list :secondary))
            (catch 'done (unwind-protect 3 (throw 'done 4)))
            (push :suffix *cleanup-effects*)))
        (defun cleanup-pending-throw ()
          (catch 'outer
            (unwind-protect (throw 'outer (values (list :primary) (list :secondary)))
              (block done (unwind-protect 3 (return-from done 4)))
              (push :suffix *cleanup-effects*))))
        (dolist (fn '(cleanup-return cleanup-go cleanup-throw cleanup-pending-throw))
          (setq *cleanup-effects* nil)
          (assert (equal '((:primary) (:secondary)) (multiple-value-list (funcall fn))))
          (assert (equal '(:suffix) *cleanup-effects*)))
        ;; An exit contained inside a cleanup must preserve that cleanup's
        ;; continuation and the outer one; only crossed continuations expire.
        (defun cleanup-contained-return ()
          (unwind-protect (values (list :primary) (list :secondary))
            (unwind-protect 3
              (block local (return-from local 4))
              (push :inner *cleanup-effects*))
            (push :outer *cleanup-effects*)))
        (setq *cleanup-effects* nil)
        (assert (equal '((:primary) (:secondary))
          (multiple-value-list (cleanup-contained-return))))
        (assert (equal '(:outer :inner) *cleanup-effects*))
        (format t "CLEANUP-CONTINUATIONS-OK~%")
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run nested cleanup retirement oracle");
        assert!(
            output.status.success(),
            "tier={tier} stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("CLEANUP-CONTINUATIONS-OK"));
    }
}
