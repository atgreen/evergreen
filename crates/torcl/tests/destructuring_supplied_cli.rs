//! Supplied-p bindings must survive portable DESTRUCTURING-BIND lowering.
use std::process::Command;

#[test]
fn optional_supplied_flags_agree_across_execution_tiers() {
    let program = r#"
      (defun unpack (input)
        (destructuring-bind (key &optional (value 42 present)
                                          (dependent (if present :yes :no))) input
          (list key value present dependent)))
      (dotimes (iteration 20)
        (assert (equal (unpack '(:x)) '(:x 42 nil :no)))
        (assert (equal (unpack '(:x 0)) '(:x 0 t :yes)))
        (assert (equal (unpack '(:x nil)) '(:x nil t :yes))))
      (let ((calls 0) (present :outer))
        (destructuring-bind (&optional (value (progn (incf calls) present) present)) nil
          (assert (eq value :outer))
          (assert (null present)))
        (destructuring-bind (&optional (value (incf calls) present)) '(nil)
          (assert (null value))
          (assert present))
        (assert (= calls 1)))
      (destructuring-bind ((&optional (value 42 present))) '((7))
        (assert (equal (list value present) '(7 t))))
      (format t "SUPPLIED-OK~%")
    "#;
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .args(["--no-init", "--eval", program])
            .env("TORCL_FORCE_TIER", tier)
            .env("TORCL_T1_THRESHOLD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("SUPPLIED-OK"));
    }
}
