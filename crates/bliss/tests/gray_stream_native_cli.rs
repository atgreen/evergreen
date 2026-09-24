use std::process::Command;

#[test]
fn open_is_a_callable_builtin_without_bootstrap() {
    let path = std::env::temp_dir().join(format!("bliss-open-function-{}.txt", std::process::id()));
    std::fs::write(&path, "from-open\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
        .args([
            "--no-init",
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
          (format t "BOUND ~S~%" (fboundp 'open))
          (let ((s (funcall (fdefinition 'open) {path:?})))
            (format t "READ ~S~%" (read-line s))
            (close s))
        "#
            ),
        ])
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("BOUND T"), "{stdout}");
    assert!(stdout.contains("READ \"from-open\""), "{stdout}");
}

const PROGRAM: &str = r#"
  (defclass native-wrapper () ((stream :initarg :stream :reader wrapped-stream)))
  (defmethod open-stream-p ((s native-wrapper)) (open-stream-p (wrapped-stream s)))
  (defmethod input-stream-p ((s native-wrapper)) (input-stream-p (wrapped-stream s)))
  (defmethod output-stream-p ((s native-wrapper)) (output-stream-p (wrapped-stream s)))
  (defmethod close ((s native-wrapper) &key abort) (close (wrapped-stream s) :abort abort))
  (defun stream-check (s)
    (list (open-stream-p s) (funcall #'input-stream-p s) (apply 'output-stream-p (list s))))
  (let* ((s (make-string-input-stream "abc"))
         (w (make-instance 'native-wrapper :stream s)))
    (format t "NATIVE ~S WRAPPER ~S~%" (stream-check s) (stream-check w))
    (format t "EVAL ~S~%" (eval (list 'open-stream-p (list 'quote w))))
    (close w :abort t)
    (format t "CLOSED ~S ~S~%" (open-stream-p s) (open-stream-p w)))
  (let ((s (make-string-output-stream)))
    (format t "OUTPUT ~S~%" (stream-check s))
    (close s))
"#;

#[test]
fn native_streams_keep_their_methods_when_gray_methods_are_added() {
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("BLISS_FORCE_TIER", tier)
            .env("BLISS_T1_THRESHOLD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{tier}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("NATIVE (T T NIL) WRAPPER (T T NIL)"),
            "{tier}: {stdout}"
        );
        assert!(stdout.contains("CLOSED NIL NIL"), "{tier}: {stdout}");
        assert!(stdout.contains("EVAL T"), "{tier}: {stdout}");
        assert!(stdout.contains("OUTPUT (T NIL T)"), "{tier}: {stdout}");
    }
}

#[test]
fn exported_gray_protocol_symbols_reach_standard_stream_dispatch() {
    let output = Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defclass gray-probe (bliss-gray-streams:fundamental-character-input-stream) ())
          (defmethod bliss-gray-streams:stream-read-char ((s gray-probe)) #\Q)
          (format t "SAME ~S~%" (eq 'stream-read-char 'bliss-gray-streams:stream-read-char))
          (format t "GRAY ~S~%" (read-char (make-instance 'gray-probe)))
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("GRAY #\\Q"), "{stdout}");
    assert!(stdout.contains("SAME T"), "{stdout}");
}
