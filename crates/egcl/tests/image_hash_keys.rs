// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn checked(command: &mut Command) {
    let output = command.output().expect("run image hash-key probe");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pathname_hash_keys_survive_core_and_executable_restore() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("egcl-hash-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for executable in [false, true] {
        let path = dir.join(if executable { "app" } else { "core.bimg" });
        let setup = r#"
          (defparameter *saved-tables* nil)
          (dolist (test '(eq eql equal equalp))
            (let ((table (make-hash-table :test test)))
              (dotimes (i 40)
                (let ((p (parse-namestring (format nil "/tmp/image-key-~D.lisp" i))))
                  (setf (gethash p table) i)
                  (setf (gethash (list p i) table) i)
                  (setf (gethash (vector (list p i)) table) i)))
              (push table *saved-tables*)))
          (defun verify-image-hash-keys ()
            (dolist (table *saved-tables*)
              (assert (= 120 (hash-table-count table)))
              (maphash (lambda (k v)
                         (multiple-value-bind (actual present) (gethash k table)
                           (assert present)
                           (assert (eql actual v))))
                       table))
            (dolist (table (list (first *saved-tables*) (second *saved-tables*)))
              (dotimes (i 40)
                (let ((p (parse-namestring (format nil "/tmp/image-key-~D.lisp" i))))
                  (assert (eql i (gethash p table)))
                  (assert (eql i (gethash (list p i) table)))))))
          (verify-image-hash-keys)
        "#;
        let save = format!(
            "{setup} (save-lisp-and-die {:?} :executable {})",
            path.to_str().unwrap(),
            if executable { "t" } else { "nil" }
        );
        checked(Command::new("timeout").args([
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &save,
        ]));
        let mut restore = Command::new("timeout");
        restore.arg("240");
        if executable {
            restore.arg(&path);
        } else {
            restore
                .arg(env!("CARGO_BIN_EXE_egcl"))
                .arg("--image")
                .arg(&path);
        }
        checked(restore.args(["--no-init", "--eval", "(verify-image-hash-keys)"]));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
