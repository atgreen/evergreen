// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn probe_file_retains_a_file_streams_path_after_close() {
    let root = std::env::temp_dir().join(format!(
        "egcl-probe-stream-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let form = r#"(progn
      (defun probe-stream (stream) (probe-file stream))
      (dolist (direction '(:output :input :io :probe))
        (let ((stream (open "index.txt" :direction direction
                            :if-exists :supersede :if-does-not-exist :create)))
          (assert (equal (probe-file stream) (truename "index.txt")))
          (dotimes (i 500)
            (assert (equal (probe-stream stream) (truename "index.txt"))))
          (close stream)
          (assert (equal (funcall #'probe-file stream) (truename "index.txt")))
          (assert (equal (eval (list 'probe-file stream)) (truename "index.txt")))))
      (let ((stream (open "index.txt")))
        (close stream)
        (delete-file "index.txt")
        (assert (null (probe-file stream))))
      (write-line "PROBE-STREAM-PASS"))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .current_dir(&root)
        .args(["--no-init", "--eval", form])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("PROBE-STREAM-PASS"), "{stdout}\n{stderr}");
}
