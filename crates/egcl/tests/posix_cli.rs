// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(unix)]

use std::process::Command;

#[test]
fn posix_maps_anonymous_and_offset_file_pages() {
    let page = egcl_rt::syscall::page_size();
    let path = std::env::temp_dir().join(format!("egcl-posix-map-{}", std::process::id()));
    let mut bytes = vec![0u8; page * 2];
    bytes[page] = 73;
    std::fs::write(&path, bytes).unwrap();
    let source = format!(
        r#"
      (require :egcl-posix)
      (assert (= (egcl-posix:getpagesize) {page}))
      (let* ((size (egcl-posix:getpagesize))
             (ptr (egcl-posix:mmap nil size
                    (logior egcl-posix:prot-read egcl-posix:prot-write)
                    (logior egcl-posix:map-private egcl-posix:map-anon) -1 0)))
        (assert (egcl-ffi:pointerp ptr))
        (unwind-protect
            (progn
              (assert (= 0 (egcl-ffi:mem-ref ptr :uint8)))
              (egcl-ffi:mem-set 211 ptr :uint8 (1- size))
              (assert (= 211 (egcl-ffi:mem-ref ptr :uint8 (1- size)))))
          (assert (= 0 (egcl-posix:munmap ptr size)))))
      (let ((fd (egcl-posix:open {path:?} egcl-posix:o-rdonly)))
        (unwind-protect
            (let ((ptr (egcl-posix:mmap nil {page} egcl-posix:prot-read
                         egcl-posix:map-shared fd {page})))
              (unwind-protect (assert (= 73 (egcl-ffi:mem-ref ptr :uint8)))
                (assert (= 0 (egcl-posix:munmap ptr {page})))))
          (egcl-posix:close fd)))
      (assert (handler-case
                  (progn (egcl-posix:mmap nil 0 egcl-posix:prot-read
                           (logior egcl-posix:map-private egcl-posix:map-anon) -1 0) nil)
                (egcl-posix:syscall-error (condition)
                  (and (eq :mmap (egcl-posix:syscall-name condition))
                       (plusp (egcl-posix:syscall-errno condition))))))
      (format t "POSIX-MAP-OK~%")
    "#
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source])
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("POSIX-MAP-OK"));
}

#[test]
fn posix_stat_matches_host_metadata() {
    use std::os::unix::fs::{MetadataExt, symlink};
    let directory = std::env::temp_dir().join(format!("egcl-posix-stat-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("data");
    let link = directory.join("link");
    std::fs::write(&path, b"hello").unwrap();
    symlink(&path, &link).unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let source = format!(
        r#"
      (require :egcl-posix)
      (let ((info (egcl-posix:stat #P{path:?})))
        (assert (= (egcl-posix:stat-ino info) {ino}))
        (assert (= (egcl-posix:stat-dev info) {dev}))
        (assert (= (egcl-posix:stat-mode info) {mode}))
        (assert (= (egcl-posix:stat-size info) 5))
        (assert (= (egcl-posix:stat-mtime info) {mtime}))
        (assert (= (egcl-posix:stat-ino (egcl-posix:stat {link:?}))
                   (egcl-posix:stat-ino info))))
      (assert (handler-case
                  (progn (egcl-posix:stat "/dev/null/nonexistent") nil)
                (egcl-posix:syscall-error (condition)
                  (eq :stat (egcl-posix:syscall-name condition)))))
      (format t "POSIX-STAT-OK~%")
    "#,
        ino = metadata.ino(),
        dev = metadata.dev(),
        mode = metadata.mode(),
        mtime = metadata.mtime()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &source])
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("POSIX-STAT-OK"));
}

#[test]
fn posix_file_descriptors_and_errors() {
    let source = r#"
      (require :egcl-posix)
      (let ((fd (egcl-posix:open #P"/dev/null" egcl-posix:o-rdonly 0)))
        (assert (and (integerp fd) (>= fd 0)))
        (assert (= 0 (egcl-posix:close fd)))
        (assert (handler-case (progn (egcl-posix:close fd) nil)
                  (egcl-posix:syscall-error (condition)
                    (and (eq :close (egcl-posix:syscall-name condition))
                         (plusp (egcl-posix:syscall-errno condition)))))))
      (assert (handler-case
                  (progn (egcl-posix:open "/dev/null/nonexistent" egcl-posix:o-rdonly 0) nil)
                (egcl-posix:syscall-error (condition)
                  (eq :open (egcl-posix:syscall-name condition)))))
      (assert (handler-case
                  (progn (egcl-posix:open (format nil "/dev/null~Cignored" (code-char 0))
                                        egcl-posix:o-rdonly 0) nil)
                (error () t)))
      (format t "POSIX-FILES-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("POSIX-FILES-OK"));
}

#[test]
fn posix_process_operations_and_errno_conditions() {
    let source = r#"
      (defparameter cl-user::*posix-caller-package* *package*)
      (require :egcl-posix)
      (assert (eq *package* cl-user::*posix-caller-package*))
      (require :egcl-posix)
      (assert (plusp (egcl-posix:getpid)))
      (assert (plusp (egcl-posix:getppid)))
      (assert (zerop (egcl-posix:kill (egcl-posix:getpid) 0)))
      (assert (handler-case
                  (progn (egcl-posix:kill (egcl-posix:getpid) -1) nil)
                (egcl-posix:syscall-error (condition)
                  (and (plusp (egcl-posix:syscall-errno condition))
                       (eq (egcl-posix:syscall-name condition) :kill)))))
      (assert (handler-case
                  (progn (egcl-posix:waitpid (egcl-posix:getpid) 0) nil)
                (egcl-posix:syscall-error (condition)
                  (plusp (egcl-posix:syscall-errno condition)))))
      (assert (egcl-posix:wifexited 1792))
      (assert (= (egcl-posix:wexitstatus 1792) 7))
      (assert (not (egcl-posix:wifsignaled 1792)))
      (format t "POSIX-PROCESS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("POSIX-PROCESS-OK"));
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn raw_syscall_returns_errno_and_writes_foreign_storage() {
    let source = r#"
      (require :egcl-posix)
      (multiple-value-bind (pid errno) (egcl-posix:raw-syscall 39)
        (assert (= pid (egcl-posix:getpid)))
        (assert (null errno)))
      (multiple-value-bind (result errno) (egcl-posix:raw-syscall 3 -1)
        (assert (= result -1))
        (assert (= errno 9)))
      (multiple-value-bind (result errno) (egcl-posix:raw-syscall -1)
        (assert (= result -1))
        (assert (= errno 38)))
      (let ((fd (egcl-posix:open "/dev/zero" egcl-posix:o-rdonly))
            (ptr (egcl-ffi:foreign-alloc 3)))
        (unwind-protect
            (progn
              (egcl-ffi:mem-set 123 ptr :uint8)
              (multiple-value-bind (count errno) (egcl-posix:raw-syscall 0 fd ptr 3)
                (assert (= count 3))
                (assert (null errno)))
              (assert (= 0 (egcl-ffi:mem-ref ptr :uint8))))
          (egcl-posix:close fd)
          (egcl-ffi:foreign-free ptr)))
      (assert (handler-case
                  (progn (egcl-posix:raw-syscall 39 0 0 0 0 0 0 0) nil)
                (program-error () t)))
      (format t "RAW-SYSCALL-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("RAW-SYSCALL-OK"));
}
