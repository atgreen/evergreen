// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn t2_named_call_uses_a_memory_indirect_call_slot() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (declaim (notinline slot-target))
          (defun slot-target (x) (list x))
          (defun slot-caller (x) (slot-target x))
          (dotimes (i 30) (slot-caller i))
          (assert (= 2 (egcl-ext:function-tier 'slot-caller)))
          (disassemble 'slot-caller)
        "#])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("call qword [rax]"),
        "named calls must load their executable target from a fixed slot: {stdout}");
    let before_call = stdout.split("call qword [rax]").next().unwrap();
    assert!(!before_call.contains("set SIGSEGV recovery IP"),
        "a named native call must not enter the Rust-helper ABI at its call site: {stdout}");
}

fn run(program: &str, tier: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    if tier != "default" {
        command.env("EGCL_FORCE_TIER", tier);
    }
    command.output().unwrap()
}

#[test]
fn warm_named_calls_honor_replaced_function_cells() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (defun original-target (x) (list :old x))
          (defun target-caller (x) (original-target x))
          (dotimes (i 10000) (target-caller i))
          (let ((saved (symbol-function 'original-target)) (tag :new))
            (unwind-protect
                (progn
                  (setf (symbol-function 'original-target)
                        (lambda (x) (list tag x)))
                  (assert (equal (funcall (symbol-function 'original-target) 5) '(:new 5)))
                  (assert (equal (original-target 4) '(:new 4)))
                  (assert (equal (target-caller 6) '(:new 6)))
                  (assert (equal (funcall saved 8) '(:old 8))))
              (setf (symbol-function 'original-target) saved)))
          (assert (equal (target-caller 7) '(:old 7)))
          (format t "FUNCTION-CELL-REPLACEMENT-OK~%")
        "#])
        .output().unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("FUNCTION-CELL-REPLACEMENT-OK"));
}

#[test]
fn retained_generic_function_bypasses_replaced_function_cell() {
    let program = r#"
      (defgeneric retained-gf (x))
      (defmethod retained-gf ((x integer)) (* x 2))
      (let ((original (symbol-function 'retained-gf))
            (calls 0))
        (setf (fdefinition 'retained-gf)
              (lambda (&rest args)
                (incf calls)
                (list :wrapped (apply original args))))
        (assert (equal (retained-gf 21) '(:wrapped 42)))
        (assert (= calls 1))
        (assert (= (funcall original 7) 14))
        (defmethod retained-gf ((x string)) (length x))
        (assert (= (funcall original "abcd") 4)))
      (format t "RETAINED-GENERIC-OK~%")
    "#;

    for tier in ["interp", "t0", "t1"] {
        let output = run(program, tier);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("RETAINED-GENERIC-OK"), "{tier}: {stdout}");
    }
}

#[test]
fn t2_named_calls_follow_redefinition_for_register_and_slice_arguments() {
    let program = r#"
      (declaim (notinline cell-target cell-wide-target))
      (defun cell-target (x) (list :old x))
      (defun cell-wide-target (a b c d) (list :old a b c d))
      (defun cell-caller (x) (cell-target x))
      (defun cell-wide-caller (a b c d) (cell-wide-target a b c d))
      (dotimes (i 30) (cell-caller i) (cell-wide-caller i 2 3 4))
      (format t "CELL-CALLER-TIERS ~S~%"
        (list (egcl-ext:function-tier 'cell-caller)
              (egcl-ext:function-tier 'cell-wide-caller)))
      (let ((old (symbol-function 'cell-target))
            (old-wide (symbol-function 'cell-wide-target))
            (tag (list :new)))
        (setf (symbol-function 'cell-target) (lambda (x) (list tag x)))
        (setf (symbol-function 'cell-wide-target)
              (lambda (a b c d) (list tag a b c d)))
        (dotimes (i 30)
          (assert (equal (cell-caller i) (list '(:new) i)))
          (assert (equal (cell-wide-caller i 2 3 4) (list '(:new) i 2 3 4))))
        (assert (equal (funcall old 7) '(:old 7)))
        (assert (equal (funcall old-wide 1 2 3 4) '(:old 1 2 3 4)))
        (fmakunbound 'cell-target)
        (assert (handler-case (progn (cell-caller 1) nil)
                  (undefined-function () t)))
        (fmakunbound 'cell-wide-target)
        (assert (handler-case (progn (cell-wide-caller 1 2 3 4) nil)
                  (undefined-function () t)))
        (setf (symbol-function 'cell-target) old)
        (setf (symbol-function 'cell-wide-target) old-wide))
      (assert (equal (cell-caller 8) '(:old 8)))
      (assert (equal (cell-wide-caller 8 2 3 4) '(:old 8 2 3 4)))
      (defun cell-target (x) (list :redefined x))
      (assert (equal (cell-caller 9) '(:redefined 9)))
      (format t "CELL-CALLER-TIERS-AFTER ~S~%"
        (list (egcl-ext:function-tier 'cell-caller)
              (egcl-ext:function-tier 'cell-wide-caller)))
      (format t "T2-CELL-REPLACEMENT-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("CELL-CALLER-TIERS (2 2)"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("T2-CELL-REPLACEMENT-OK"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("CELL-CALLER-TIERS-AFTER (2 2)"),
        "{stdout}\n{stderr}"
    );
}

#[test]
fn unprofile_releases_a_callee_cached_by_a_t2_call_slot() {
    let output = run(r#"
      (declaim (notinline slot-profile-target))
      (defun slot-profile-target (x) (list x))
      (defun slot-profile-caller (x) (slot-profile-target x))
      (dotimes (i 30) (slot-profile-caller i))
      (assert (= 2 (egcl-ext:function-tier 'slot-profile-caller)))
      (egcl-ext:profile 'slot-profile-target)
      (dotimes (i 30) (slot-profile-caller i))
      (assert (= 0 (egcl-ext:function-tier 'slot-profile-target)))
      (egcl-ext:unprofile 'slot-profile-target)
      (dotimes (i 30) (slot-profile-caller i))
      (format t "UNPROFILE-TIER ~D~%" (egcl-ext:function-tier 'slot-profile-target))
      (assert (= 2 (egcl-ext:function-tier 'slot-profile-target)))
    "#, "t2");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("UNPROFILE-TIER 2"), "{stdout}");
}

#[test]
fn a_warmed_worker_call_slot_observes_main_thread_redefinition() {
    let output = run(r#"
      (declaim (notinline thread-slot-target))
      (defun thread-slot-target (x) (list :before x))
      (defun thread-slot-caller (x) (thread-slot-target x))
      (let ((lock (egcl-thread:make-mutex))
            (cv (egcl-thread:make-condition-variable))
            (phase 0) (before nil) (after nil) (worker nil))
        (setq worker (egcl-thread:make-thread
          (lambda ()
            (dotimes (i 30) (thread-slot-caller i))
            (assert (= 2 (egcl-ext:function-tier 'thread-slot-caller)))
            (egcl-thread:grab-mutex lock)
            (unwind-protect
                (progn
                  (setq before (thread-slot-caller 41))
                  (setq phase 1)
                  (egcl-thread:condition-notify cv)
                  (loop until (= phase 2) do
                    (unless (egcl-thread:condition-wait cv lock :timeout 10)
                      (error "main did not replace slot target")))
                  (setq after (thread-slot-caller 42)))
              (egcl-thread:release-mutex lock)))))
        (egcl-thread:grab-mutex lock)
        (unwind-protect
            (progn
              (loop until (= phase 1) do
                (unless (egcl-thread:condition-wait cv lock :timeout 10)
                  (error "worker did not warm slot")))
              (eval '(defun thread-slot-target (x) (list :after x)))
              (setq phase 2)
              (egcl-thread:condition-notify cv))
          (egcl-thread:release-mutex lock))
        (egcl-thread:join-thread worker)
        (assert (equal before '(:before 41)))
        (assert (equal after '(:after 42))))
      (format t "THREAD-CALL-SLOT-OK~%")
    "#, "t2");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("THREAD-CALL-SLOT-OK"), "{stdout}");
}

#[test]
fn active_native_slot_survives_replacement_and_callee_local_deopt() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (declaim (notinline active-slot-target active-slot-hook))
          (defvar *replace-active-slot* nil)
          (defun active-slot-hook (x)
            (when *replace-active-slot*
              (setq *replace-active-slot* nil)
              (eval '(defun active-slot-target (x) (+ x 100)))
              (dotimes (i 30) (active-slot-caller i)))
            x)
          (defun active-slot-target (x) (+ (active-slot-hook x) 1))
          (defun active-slot-caller (x) (active-slot-target x))
          (dotimes (i 30) (assert (= (active-slot-caller i) (+ i 1))))
          (assert (= 2 (egcl-ext:function-tier 'active-slot-target)))
          (assert (= 2 (egcl-ext:function-tier 'active-slot-caller)))
          (setq *replace-active-slot* t)
          (let* ((input (expt 2 61))
                 (before (egcl-ext:deopt-count))
                 (answer (active-slot-caller input)))
            (assert (= answer (+ input 1)))
            (assert (> (egcl-ext:deopt-count) before))
            (assert (= (active-slot-caller input) (+ input 100))))
          (format t "ACTIVE-NATIVE-SLOT-OK~%")
        "#])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ACTIVE-NATIVE-SLOT-OK"), "{stdout}\n{stderr}");
}

#[test]
fn native_optional_entry_preserves_defaults_and_single_values() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (declaim (notinline native-optionals native-identity))
          (defvar *native-defaults* 0)
          (defun native-optionals (a &optional (b (incf *native-defaults*)) (c 20))
            (+ a (+ b c)))
          (defun native-optional-full (a b c) (native-optionals a b c))
          (defun native-optional-short (a) (native-optionals a))
          (defun native-identity (x) x)
          (defun native-single-value (x) (native-identity (values x 99)))
          (dotimes (i 30)
            (assert (= (native-optional-full 1 2 3) 6))
            (assert (equal (multiple-value-list (native-single-value i)) (list i))))
          (assert (= 2 (egcl-ext:function-tier 'native-optionals)))
          (assert (= 2 (egcl-ext:function-tier 'native-identity)))
          (assert (= *native-defaults* 0))
          (assert (= (native-optional-short 1) 22))
          (assert (= (native-optional-short 1) 23))
          (assert (= *native-defaults* 2))
          (assert (= (native-optional-full 1 2 3) 6))
          (assert (= *native-defaults* 2))
          (format t "NATIVE-OPTIONAL-ENTRY-OK~%")
        "#])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("NATIVE-OPTIONAL-ENTRY-OK"), "{stdout}\n{stderr}");
}

#[test]
fn native_slot_callee_recompiles_after_numeric_phase_changes() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (declaim (notinline phase-slot-leaf))
          (defun phase-slot-leaf (x) (* x 5))
          (defun phase-slot-caller (x) (phase-slot-leaf x))
          (dotimes (i 30) (assert (= (phase-slot-caller i) (* i 5))))
          (format t "INITIAL ~D ~D~%" (egcl-ext:function-tier 'phase-slot-leaf) (egcl-ext:function-tier 'phase-slot-caller))
          (assert (= 2 (egcl-ext:function-tier 'phase-slot-caller)))
          (assert (= 2 (egcl-ext:function-tier 'phase-slot-leaf)))
          (dolist (value '(2.5 2305843009213693952 7))
            (dotimes (i 30)
              (assert (= (phase-slot-caller value) (* value 5))))
            (format t "PHASE ~S ~D ~D~%" value (egcl-ext:function-tier 'phase-slot-leaf) (egcl-ext:function-tier 'phase-slot-caller))
            (assert (= 2 (egcl-ext:function-tier 'phase-slot-leaf)))
            (assert (= 2 (egcl-ext:function-tier 'phase-slot-caller))))
          (format t "NATIVE-SLOT-PHASE-OK~%")
        "#])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("NATIVE-SLOT-PHASE-OK"));
}

#[test]
fn cached_gethash_calls_preserve_values_errors_and_redefinition() {
    // Calling a saved builtin after restoring its function cell currently
    // overflows even on the parent revision (bliss-cqtpe). This probe checks
    // replacement invalidation and restores the cell only for cleanup.
    let program = r#"
      (defun cached-get2 (key table) (gethash key table))
      (defun cached-get3 (key table default) (gethash key table default))
      (let ((table (make-hash-table :test 'eql)) (key (list :key)))
        (dotimes (i 40) (cached-get2 key table) (cached-get3 key table :absent))
        (assert (= EXPECTED-TIER (egcl-ext:function-tier 'cached-get2)))
        (assert (= EXPECTED-TIER (egcl-ext:function-tier 'cached-get3)))
        (assert (equal (multiple-value-list (cached-get2 key table)) '(nil nil)))
        (assert (equal (multiple-value-list (cached-get3 key table :absent)) '(:absent nil)))
        (setf (gethash key table) nil)
        (assert (equal (multiple-value-list (cached-get3 key table :absent)) '(nil t)))
        (setf (gethash key table) (list :stored))
        (assert (equal (multiple-value-list (cached-get2 key table)) '((:stored) t)))
        (assert (handler-case (progn (cached-get2 key nil) nil) (type-error () t)))
        (let ((saved (symbol-function 'gethash)))
          (unwind-protect
              (progn
                (setf (symbol-function 'gethash)
                      (lambda (key table &optional default)
                        (declare (ignore key table))
                        (values default :replacement)))
                (assert (equal (multiple-value-list (cached-get2 key table)) '(nil :replacement)))
                (assert (equal (multiple-value-list (cached-get3 key table :changed)) '(:changed :replacement))))
            (setf (symbol-function 'gethash) saved))))
      (format t "GETHASH-CACHE-CORRECT~%")
    "#;
    for (tier, expected) in [("t0", "0"), ("t1", "1"), ("t2", "2")] {
        let program = program.replace("EXPECTED-TIER", expected);
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{tier}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("GETHASH-CACHE-CORRECT"), "{tier}: {stdout}");
    }
}
