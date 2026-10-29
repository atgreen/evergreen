// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! One accepted start per test process. Run on an app-hosted iOS test runner;
//! future matrix includes post-init GC stress/poison and requested T2 (capped T0).
#![cfg(target_os = "ios")]

use egcl_ios::*;
use std::ffi::CString;
use std::path::Path;
use std::time::{Duration, Instant};

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "cooperative lifecycle deadline expired"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn status() -> Status {
    let mut result = Status::default();
    // SAFETY: complete owned output storage.
    assert_eq!(unsafe { egcl_ios_status(&mut result) }, OK);
    result
}
fn start(source: &Path, writable: &Path) -> i32 {
    let source = CString::new(source.to_str().unwrap()).unwrap();
    let writable = CString::new(writable.to_str().unwrap()).unwrap();
    let entry = c"app.lisp";
    let config = Config {
        abi_version: ABI_VERSION,
        struct_size: std::mem::size_of::<Config>() as u32,
        source_directory: source.as_ptr(),
        writable_directory: writable.as_ptr(),
        entry_file: entry.as_ptr(),
    };
    // SAFETY: all strings/storage stay alive through the call, then deliberately
    // drop immediately: the asynchronous worker must use its copied inputs.
    unsafe { egcl_ios_start(&config) }
}

#[test]
fn stopping_waits_for_descendant_cleanup_and_keeps_child_exit_code() {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Directory(
        std::env::temp_dir().join(format!("egcl-ios-cohort-{}-\"-\\", std::process::id())),
    );
    let source = directory.0.join("source");
    let writable = directory.0.join("data");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&writable).unwrap();
    std::fs::write(
        source.join("app.lisp"),
        r#"
(defun emit-host-marker (name text)
  (with-open-file (out (merge-pathnames name egcl-ios:*writable-directory*)
                      :direction :output :if-exists :append :if-does-not-exist :create)
    (format out "~A~%" text)))
(defun host-descendant ()
  (unwind-protect
      (progn (emit-host-marker "ready" "ready") (loop (sleep 0.01)))
    (emit-host-marker "entered" "entered")
    (loop until (probe-file (merge-pathnames "release" egcl-ios:*writable-directory*))
          do (sleep 0.01))
    (emit-host-marker "child-cleanup" "child")
    ;; Explicit child code is published even if root already returned default 0.
    (egcl-ext:quit 23)))
(defun host-tier-control (n) (+ n 1))
(dotimes (i 80) (assert (= (host-tier-control i) (+ i 1))))
(assert (= (egcl-ext:function-tier 'host-tier-control) 0))
(unwind-protect
    (progn (egcl-thread:make-thread #'host-descendant) (loop (sleep 0.01)))
  (emit-host-marker "root-cleanup" "root"))
"#,
    )
    .unwrap();
    assert_eq!(start(&source, &writable), OK);
    until(|| writable.join("ready").is_file());
    // Must return before the blocked descendant is released, not join on caller.
    assert_eq!(egcl_ios_request_stop(), OK);
    until(|| writable.join("entered").is_file());
    assert_eq!(status().state, STOPPING);
    assert_eq!(egcl_ios_request_stop(), OK); // must not re-inject into cleanup
    std::fs::write(writable.join("release"), "release").unwrap();
    until(|| matches!(status().state, FINISHED | FAILED));
    let completed = status();
    assert_eq!(completed.state, FINISHED);
    assert_eq!(completed.exit_code, 23);
    assert_eq!(
        std::fs::read_to_string(writable.join("root-cleanup")).unwrap(),
        "root\n"
    );
    assert_eq!(
        std::fs::read_to_string(writable.join("child-cleanup")).unwrap(),
        "child\n"
    );
    assert_eq!(start(&source, &writable), ALREADY_STARTED);
    assert_eq!(egcl_ios_request_stop(), OK);
    assert_eq!(status().exit_code, 23);
}
