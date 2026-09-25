//! High-resolution wall-clock values for pure Common Lisp libraries.
use std::process::Command;

#[test]
fn precise_time_is_atomic_normalized_and_tracks_universal_time() {
    let program = r#"
      (multiple-value-bind (seconds nanoseconds)
          (torcl-ext:get-precise-time)
        (assert (integerp seconds))
        (assert (integerp nanoseconds))
        (assert (<= 0 nanoseconds))
        (assert (< nanoseconds 1000000000))
        (assert (<= (abs (- seconds (get-universal-time))) 1))
        (let ((before (+ (* seconds 1000000000) nanoseconds)))
          (sleep 0.01)
          (multiple-value-bind (later-seconds later-nanoseconds)
              (torcl-ext:get-precise-time)
            (let ((after (+ (* later-seconds 1000000000)
                            later-nanoseconds)))
              (assert (< before after))
              (assert (< (- after before) 5000000000))))))
      (format t "PRECISE-TIME-OK~%")
    "#;

    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run precise wall-clock regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("PRECISE-TIME-OK"), "{stdout}\n{stderr}");
}
