// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Image-embedded files (bliss-vmqe0): `egcl-ext:embed-file` reads a file on
//! the build host and the stream and pathname operations find it, under the
//! recorded path, on any host the image lands on.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("egcl-embed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn egcl(args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=5", "300", BIN, "--no-init"])
        .args(args);
    command.env_remove("EGCL_BACKEND");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn ok(args: &[&str]) -> String {
    let (success, text) = egcl(args, &[]);
    assert!(success, "{text}");
    text
}

/// The run-time directory no host has, so every hit below is the image's.
const ZONE: &str = "/nowhere/egcl-embed/zoneinfo";

#[test]
fn embedded_files_are_found_by_every_file_operation_and_are_read_only() {
    let dir = Dir::new("ops");
    let utc = dir.file("utc.bin", &[84, 90, 105, 102, 0, 255, 128, 10]);
    let init = dir.file(
        "init.lisp",
        b"(defparameter cl-user::*embedded-loaded* :yes)",
    );
    let program = format!(
        r#"
      (egcl-ext:embed-file "{ZONE}/UTC" {utc:?})
      (egcl-ext:embed-file "{ZONE}/init.lisp" {init:?})
      (format t "P ~S~%" (list (egcl-ext:embedded-file-p "{ZONE}/UTC") (egcl-ext:embedded-file-p "{ZONE}/nope")))
      (format t "LIST ~S~%" (egcl-ext:embedded-files))
      (format t "PROBE ~S~%" (list (namestring (probe-file "{ZONE}/UTC")) (namestring (truename "{ZONE}/init.lisp")) (probe-file "{ZONE}/nope")))
      (format t "BYTES ~S~%" (with-open-file (s "{ZONE}/UTC" :element-type '(unsigned-byte 8))
                               (list (file-length s) (loop for b = (read-byte s nil) while b collect b))))
      (format t "CHARS ~S~%" (with-open-file (s "{ZONE}/init.lisp") (list (file-length s) (read-line s))))
      (format t "DIR ~S~%" (list (mapcar #'file-namestring (directory "{ZONE}/*.*"))
                                 (mapcar #'file-namestring (directory "{ZONE}/*.lisp"))
                                 (mapcar #'file-namestring (directory "{ZONE}/"))))
      (format t "DATE ~S~%" (> (file-write-date "{ZONE}/UTC") 3000000000))
      (load "{ZONE}/init")
      (format t "LOAD ~S~%" cl-user::*embedded-loaded*)
      (format t "RO ~S~%" (list (handler-case (progn (delete-file "{ZONE}/UTC") :deleted) (file-error () :refused))
                                (handler-case (progn (rename-file "{ZONE}/UTC" "{ZONE}/UTC2") :renamed) (file-error () :refused))
                                (handler-case (with-open-file (s "{ZONE}/UTC" :direction :output :if-exists :supersede) :wrote) (file-error () :refused))))
      ;; An embedded entry shadows the real file at the same path.
      (egcl-ext:embed-file {init:?} {utc:?})
      (format t "SHADOW ~S~%" (with-open-file (s {init:?} :element-type '(unsigned-byte 8)) (file-length s)))
    "#
    );
    let text = ok(&["--eval", &program]);
    for expected in [
        "P (T NIL)",
        &format!("LIST (\"{ZONE}/UTC\" \"{ZONE}/init.lisp\")"),
        &format!("PROBE (\"{ZONE}/UTC\" \"{ZONE}/init.lisp\" NIL)"),
        "BYTES (8 (84 90 105 102 0 255 128 10))",
        "CHARS (46 \"(defparameter cl-user::*embedded-loaded* :yes)\")",
        "DIR ((\"UTC\" \"init.lisp\") (\"init.lisp\") (\"UTC\" \"init.lisp\"))",
        "DATE T",
        "LOAD :YES",
        "RO (:REFUSED :REFUSED :REFUSED)",
        "SHADOW 8",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

#[test]
fn embedded_files_survive_the_image_and_do_not_need_the_source_host() {
    let dir = Dir::new("image");
    let utc = dir.file("utc.bin", b"TZif2\x00\x01\x02");
    let core = dir.path("app.core");
    ok(&[
        "--eval",
        &format!("(egcl-ext:embed-file \"{ZONE}/UTC\" {utc:?})"),
        "--eval",
        &format!("(egcl-ext:save-lisp-and-die {core:?})"),
    ]);
    // The build host's copy is gone; the image still has it.
    fs::remove_file(&utc).unwrap();
    let text = ok(&[
        "--image",
        &core,
        "--eval",
        &format!(
            "(format t \"RESTORED ~S~%\" (list (egcl-ext:embedded-files) (namestring (probe-file \"{ZONE}/UTC\")) \
             (with-open-file (s \"{ZONE}/UTC\" :element-type '(unsigned-byte 8)) (loop for b = (read-byte s nil) while b collect b))))"
        ),
    ]);
    assert!(
        text.contains(&format!(
            "RESTORED ((\"{ZONE}/UTC\") \"{ZONE}/UTC\" (84 90 105 102 50 0 1 2))"
        )),
        "{text}"
    );
}

#[test]
fn the_shaker_keeps_embedded_files() {
    let dir = Dir::new("shake");
    let utc = dir.file("utc.bin", b"TZif");
    let core = dir.path("app.core");
    let spec = dir.file(
        "app.shake",
        b"version = 1\nentry = EMBED-APP::MAIN\nprune-package = EMBED-APP\ndynamic = explicit\n",
    );
    let exe = dir.path("app");
    ok(&[
        "--eval",
        &format!(
            r#"(progn
                 (defpackage :embed-app (:use :cl))
                 (in-package :embed-app)
                 (defun unused () "pruned")
                 (defun main ()
                   (with-open-file (s "{ZONE}/UTC") (format t "MAIN ~A~%" (read-line s))))
                 (egcl-ext:embed-file "{ZONE}/UTC" {utc:?})
                 (egcl-ext:save-lisp-and-die {core:?}))"#
        ),
    ]);
    fs::remove_file(&utc).unwrap();
    ok(&["--image", &core, "--shake", &spec, "--output", &exe]);
    let output = Command::new(&exe).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(text, "MAIN TZif\n");
    // The shake happened (a manifest names the entry); the executable above
    // proves the registry and its contents came through it.
    let manifest = fs::read_to_string(format!("{exe}.manifest")).unwrap();
    assert!(manifest.contains("entry = EMBED-APP::MAIN"), "{manifest}");
}

#[test]
fn embedding_and_reading_hold_under_gc_stress() {
    let dir = Dir::new("stress");
    let hello = dir.file("h.txt", b"hello");
    let program = format!(
        r#"(progn
             (dotimes (i 40) (egcl-ext:embed-file (format nil "{ZONE}/s/~D.txt" i) {hello:?}))
             (format t "STRESS ~S~%" (list (length (egcl-ext:embedded-files))
                                           (with-open-file (s "{ZONE}/s/7.txt") (read-line s))
                                           (length (directory "{ZONE}/s/*.txt")))))"#
    );
    let (success, text) = egcl(
        &["--eval", &program],
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ("EGCL_GC_POISON", "1"),
        ],
    );
    assert!(success, "{text}");
    assert!(text.contains("STRESS (40 \"hello\" 40)"), "{text}");
}

#[test]
fn a_missing_source_is_a_file_error_and_embeds_nothing() {
    let dir = Dir::new("missing");
    let absent = dir.path("absent.bin");
    let text = ok(&[
        "--eval",
        &format!(
            "(format t \"MISSING ~S~%\" (list (handler-case (egcl-ext:embed-file \"{ZONE}/X\" {absent:?}) (file-error () :refused)) (egcl-ext:embedded-files)))"
        ),
    ]);
    assert!(text.contains("MISSING (:REFUSED NIL)"), "{text}");
}
