// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn probe_creates_missing_files_and_returns_closed_streams() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let root =
            std::env::temp_dir().join(format!("egcl-open-probe-{}-{tier}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let program = r#"
          (defun probe-mode (path mode)
            (open path :direction :probe :if-does-not-exist mode))
          (let ((path "enabled.txt"))
            (let ((stream (probe-mode path :create)))
              (assert (streamp stream))
              (assert (not (open-stream-p stream))))
            (assert (probe-file path))
            (with-open-file (stream path :direction :output :if-exists :supersede)
              (write-string "keep" stream))
            (dolist (mode '(:create :error nil))
              (let ((stream (probe-mode path mode)))
                (assert (streamp stream))
                (assert (not (open-stream-p stream)))))
            (with-open-file (stream path :direction :probe)
              (assert (streamp stream))
              (assert (not (open-stream-p stream))))
            (let ((stream (funcall #'open path :direction :probe)))
              (assert (not (open-stream-p stream))))
            (with-open-file (stream path)
              (assert (string= (read-line stream) "keep"))))
          (assert (null (open "missing.txt" :direction :probe)))
          (assert (null (probe-mode "missing.txt" nil)))
          (assert (handler-case (progn (probe-mode "missing.txt" :error) nil)
                    (file-error () t)))
          (assert (not (probe-file "missing.txt")))
          (assert (handler-case (progn (probe-mode "no-parent/leaf.txt" :create) nil)
                    (file-error () t)))
          (with-open-file (stream "input.txt" :direction :input :if-does-not-exist :create)
            (assert (open-stream-p stream))
            (assert (eq :eof (read-char stream nil :eof))))
          (assert (probe-file "input.txt"))
          (assert (null (open "missing.txt" :if-does-not-exist nil)))
          (assert (handler-case (progn (open "missing.txt") nil) (file-error () t)))
          (write-line "PROBE-CREATE-PASS")
        "#;
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .current_dir(&root)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_BACKEND")
            .env_remove("EGCL_DISABLE_T2")
            .output()
            .expect("run OPEN probe checks");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        std::fs::remove_dir_all(root).unwrap();
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("PROBE-CREATE-PASS"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn evaluated_if_exists_modes_preserve_file_contents_across_tiers() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let path =
            std::env::temp_dir().join(format!("egcl-open-modes-{}-{tier}.txt", std::process::id()));
        let program = r#"
          (defun write-mode (path mode text)
            (let ((stream (open path :direction :output :if-exists mode :if-does-not-exist :create)))
              (when stream (unwind-protect (write-string text stream) (close stream)))
              (not (null stream))))
          (defun read-text (path)
            (with-open-file (stream path)
              (let ((text (make-string (file-length stream))))
                (read-sequence text stream) text)))
          (let ((path "@PATH@"))
            (assert (write-mode path :supersede "first"))
            (assert (write-mode path :append "+second"))
            (assert (equal "first+second" (read-text path)))
            (assert (write-mode path :overwrite "NEW"))
            (assert (equal "NEWst+second" (read-text path)))
            (assert (null (write-mode path nil "lost")))
            (assert (handler-case (progn (write-mode path :error "lost") nil) (file-error () t)))
            (assert (equal "NEWst+second" (read-text path)))
            (with-open-file (stream path :direction :io :if-exists :append)
              (write-string "+io" stream))
            (assert (equal "NEWst+second+io" (read-text path)))
            (assert (write-mode path :supersede "end"))
            (assert (equal "end" (read-text path))))
          (format t "OPEN-MODES-PASS~%")
        "#.replace("@PATH@", path.to_str().unwrap());
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_BACKEND")
            .env_remove("EGCL_DISABLE_T2")
            .output()
            .expect("run OPEN modes");
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("OPEN-MODES-PASS"));
    }
}
