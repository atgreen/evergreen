//! A pending native-to-Lisp transfer must stop subsequent native side effects.

use std::process::Command;

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
