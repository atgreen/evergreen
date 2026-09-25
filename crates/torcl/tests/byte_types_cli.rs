//! Full-word counter types must not overflow a host shift or reject bignums.
use std::process::Command;

#[test]
fn byte_type_boundaries_match_in_compiled_and_eval_paths() {
    let body = r#"(progn
      (dolist (width '(1 8 60 61 62 63 64 65 128))
        (let ((limit (ash 1 width)) (type (list 'unsigned-byte width)))
          (assert (typep 0 type))
          (assert (typep (1- limit) type))
          (assert (not (typep limit type)))
          (assert (not (typep -1 type)))
          (assert (not (typep 1/2 type)))
          (assert (not (typep 0.0 type)))))
      (dolist (width '(1 8 60 61 62 63 64 65 128))
        (let ((limit (ash 1 (1- width))) (type (list 'signed-byte width)))
          (assert (typep (- limit) type))
          (assert (typep (1- limit) type))
          (assert (not (typep limit type)))
          (assert (not (typep (1- (- limit)) type)))))
      (dolist (type '((unsigned-byte) (unsigned-byte *) (signed-byte) (signed-byte *)))
        (assert (typep (ash 1 128) type)))
      (dolist (type '((unsigned-byte 0) (signed-byte 0)
                     (unsigned-byte -1) (signed-byte -1)
                     (unsigned-byte 1.5) (signed-byte bad)))
        (assert (handler-case (progn (typep 4 type) nil) (error () t))))
      (assert (typep 4 '(unsigned-byte 64)))
      (format t "BYTE-TYPE-BOUNDARIES-OK~%"))"#;
    for program in [body.to_owned(), format!("(eval '{body})")] {
        let output = Command::new("timeout")
            .args([
                "--kill-after=5",
                "60",
                env!("CARGO_BIN_EXE_torcl"),
                "--no-init",
                "--eval",
                &program,
            ])
            .output()
            .expect("run bounded byte-type regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("BYTE-TYPE-BOUNDARIES-OK"),
            "{stdout}\n{stderr}"
        );
    }
}
