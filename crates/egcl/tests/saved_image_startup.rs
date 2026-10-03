// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(target_os = "linux")]
use std::{fs, process::Command};

#[test]
fn saved_application_reads_its_image_once() {
    let dir = std::env::temp_dir().join(format!("egcl-startup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let app = dir.join("app");
    let source = format!(
        r#"
      (defun startup-main ()
        (with-open-file (s "/proc/self/io")
          (loop for line = (read-line s nil nil) while line
                when (search "rchar:" line) do (write-line line))))
      (egcl-ext:save-lisp-and-die {app:?} :executable t :application t
                                 :toplevel #'startup-main)
    "#
    );
    let saved = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source])
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    let bytes = fs::read(&app).unwrap();
    let image_len = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
    let output = Command::new(&app).output().unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let read_bytes: u64 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("rchar:"))
        .expect("saved entry point must report process reads")
        .trim()
        .parse()
        .unwrap();
    // /proc reports bytes returned by read(), even with a warm page cache.
    // Allow runtime configuration and proc reads, but not a second payload.
    assert!(
        read_bytes < image_len + image_len / 2 + 262144,
        "read {read_bytes} bytes for a {image_len}-byte image"
    );
}
