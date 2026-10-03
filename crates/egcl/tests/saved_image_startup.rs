// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(target_os = "linux")]
use std::{fs, process::Command};

#[test]
fn startup_rejects_legacy_images_but_load_accepts_library_fasls() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("egcl-legacy-startup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("legacy.lisp");
    let fasl = dir.join("legacy.fasl");
    fs::write(&source, "(format t \"LEGACY-PAYLOAD-RAN~%\")").unwrap();
    let compile = format!("(compile-file {source:?} :output-file {fasl:?})");
    let compiled = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &compile])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&fasl)
        .output()
        .unwrap();
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(String::from_utf8_lossy(&loaded.stdout).contains("LEGACY-PAYLOAD-RAN"));
    for path in [&source, &fasl] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--image"])
            .arg(path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted obsolete image {path:?}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("LEGACY-PAYLOAD-RAN"));
    }
    let app = dir.join("legacy-app");
    let mut bytes = fs::read(env!("CARGO_BIN_EXE_egcl")).unwrap();
    let payload = fs::read(&fasl).unwrap();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(b"EGCLAPP\0");
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    fs::write(&app, bytes).unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(&app).output().unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(!output.status.success(), "accepted obsolete embedded image");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("LEGACY-PAYLOAD-RAN"));
}

#[test]
fn saved_application_maps_its_image_without_reading_the_payload() {
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
    assert_eq!((bytes.len() as u64 - 16 - image_len) % 4096, 0);
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
    // Allow runtime configuration and proc reads, but no eager image read.
    assert!(
        read_bytes < 262144,
        "read {read_bytes} bytes for a {image_len}-byte image"
    );
}
