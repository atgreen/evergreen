// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn file_stream_pathnames_survive_close_and_changed_defaults() {
    let directory =
        std::env::temp_dir().join(format!("egcl-stream-pathname-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let program = r#"
      (let ((stream (open "saved.txt" :direction :output :if-exists :supersede)))
        (unwind-protect
            (let ((expected (truename "saved.txt")))
              (assert (equal (truename stream) expected))
              (assert (equal (pathname stream) expected))
              (close stream)
              (assert (not (open-stream-p stream)))
              ;; The stream retains its opening path, not the new defaults.
              (let ((*default-pathname-defaults* #p"/unlikely-new-defaults/"))
                (assert (equal (truename stream) expected))
                (assert (equal (funcall #'truename stream) expected))
                (assert (equal (probe-file stream) expected))
                (assert (equal (pathname stream) expected))
                (assert (equal (namestring stream) (namestring expected)))
                (assert (equal (pathname-name stream) "saved"))
                (assert (equal (pathname-type stream) "txt"))
                (multiple-value-bind (path position) (parse-namestring stream)
                  (assert (equal path expected))
                  (assert (= position 0)))
                (assert (handler-case
                          (progn (parse-namestring stream "OTHER-HOST") nil)
                          (error () t))))
              (delete-file stream)
              (assert (not (probe-file stream)))
              (assert (equal (pathname stream) expected))
              (assert (handler-case (progn (truename stream) nil)
                        (file-error () t))))
          (close stream)))
      (let ((stream (make-string-output-stream)))
        (close stream)
        (assert (handler-case (progn (pathname stream) nil) (type-error () t))))
      (format t "CLOSED-STREAM-PATHNAMES-OK~%")
    "#;
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .current_dir(&directory)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("CLOSED-STREAM-PATHNAMES-OK"));
    }
    std::fs::remove_dir_all(directory).unwrap();
}
