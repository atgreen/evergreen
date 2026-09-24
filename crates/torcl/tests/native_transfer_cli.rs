//! A pending native-to-Lisp transfer must stop subsequent native side effects.

use std::process::Command;

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
          (assert (= 2 (torcl-ext:function-tier name))))
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
        (assert (= 2 (torcl-ext:function-tier 't2-values-middle)))
        (format t "T2-TRANSFERS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .env("TORCL_LAZY_COMPILE", "0")
        .env("TORCL_FORCE_TIER", "t2")
        .env("TORCL_T2_LOG", "1")
        .output()
        .expect("run TorCL");
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
        (assert (> (torcl-ext:function-osr-count 'transfer-loop) 0))
        (format t "OSR-TRANSFERS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .env("TORCL_OSR_THRESHOLD", "50")
        .output()
        .expect("run TorCL");
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
        (assert (= 1 (torcl-ext:function-tier 'signal-middle)))
        (assert (= 1 (torcl-ext:function-tier 'throw-middle)))
        (assert (= 1 (torcl-ext:function-tier 'builtin-middle)))
        (assert (= 1 (torcl-ext:function-tier 'code-middle)))
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
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .args(["--no-init", "--eval", program])
            .env("TORCL_LAZY_COMPILE", "0")
            .env("TORCL_T1_THRESHOLD", "1")
            .env("TORCL_T2", "0")
            .env("TORCL_NN_DIRECT", direct)
            .env("TORCL_LOG", "compile=trace")
            .output()
            .expect("run TorCL");
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
