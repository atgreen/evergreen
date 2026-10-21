// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! INTERN takes its string as the symbol's NAME, verbatim: a colon in it is an
//! ordinary name character, never a package marker (GitHub #468).

use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--no-init")
        .args(args)
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{args:?}\n{stdout}\n{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    stdout
}

fn check(program: &str) {
    let stdout = run(&["--eval", program]);
    assert!(stdout.lines().any(|line| line == "INTERN-PASS"), "{stdout}");
}

#[test]
fn intern_in_keyword_keeps_colons_in_the_name() {
    check(
        r#"(progn
          (assert (equal (mapcar (lambda (s) (symbol-name (intern s :keyword)))
                                 '(":DECK" "DECK" "::DECK" "A:B"))
                         '(":DECK" "DECK" "::DECK" "A:B")))
          (assert (equal (mapcar (lambda (s) (prin1-to-string (intern s :keyword)))
                                 '(":DECK" "DECK" "::DECK" "A:B"))
                         '(":|:DECK|" ":DECK" ":|::DECK|" ":|A:B|")))
          (assert (eq (intern "DECK" :keyword) :deck))
          (assert (not (eq (intern ":DECK" :keyword) :deck)))
          (assert (eq (intern ":DECK" :keyword) (intern ":DECK" :keyword)))
          (assert (eq (find-symbol "DECK" :keyword) :deck))
          (assert (eq (find-symbol ":DECK" :keyword) (intern ":DECK" :keyword)))
          (assert (equal (symbol-name (find-symbol ":DECK" :keyword)) ":DECK"))
          (assert (null (find-symbol "B" "KEYWORD")))
          (assert (keywordp (intern "A:B" :keyword)))
          (assert (string= (string (intern "A:B" :keyword)) "A:B"))
          (assert (string= (princ-to-string (intern ":DECK" :keyword)) ":DECK"))
          (dolist (name '(":DECK" "::DECK" "A:B"))
            (let ((s (intern name :keyword)))
              (assert (eq s (read-from-string (prin1-to-string s))))))
          (assert (eq ':|A:B| (intern "A:B" :keyword)))
          (assert (eq ':|:DECK| (intern ":DECK" :keyword)))
          (assert (eq 'keyword::|:DECK| (intern ":DECK" :keyword)))
          (assert (eq '::deck :deck))
          (format t "INTERN-PASS~%"))"#,
    );
}

#[test]
fn intern_in_ordinary_packages_keeps_colons_in_the_name() {
    check(
        r#"(progn
          (defpackage :gh468 (:use :cl))
          (let ((x (intern ":X" :gh468)) (yz (intern "Y:Z" :gh468)))
            (assert (string= (symbol-name x) ":X"))
            (assert (string= (symbol-name yz) "Y:Z"))
            (assert (eq (find-symbol "Y:Z" :gh468) yz))
            (assert (null (find-symbol "Z" :gh468)))
            (export yz :gh468)
            (assert (eq (read-from-string "gh468:|Y:Z|") yz))
            (assert (string= (prin1-to-string yz) "GH468:|Y:Z|"))
            (import yz :cl-user)
            (assert (eq (find-symbol "Y:Z" :cl-user) yz))
            (assert (unintern yz :cl-user))
            (assert (null (find-symbol "Y:Z" :cl-user))))
          (let ((xy 'cl-user::|X:Y|))
            (assert (string= (symbol-name xy) "X:Y"))
            (assert (eq xy (intern "X:Y" :cl-user)))
            (let ((*package* (find-package :gh468)))
              (assert (string= (prin1-to-string xy) "COMMON-LISP-USER::|X:Y|"))
              (assert (eq xy (read-from-string (prin1-to-string xy))))))
          (assert (string= (symbol-name '|a:b|) "a:b"))
          (assert (string= (symbol-name (make-symbol "P:Q")) "P:Q"))
          (format t "INTERN-PASS~%"))"#,
    );
}

#[test]
fn colon_names_survive_compile_file_and_load() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("intern-colon-names");
    std::fs::create_dir_all(&dir).expect("tmpdir");
    let source = dir.join("colon-names.lisp");
    let fasl = dir.join("colon-names.fasl");
    std::fs::write(
        &source,
        r#"(defpackage :gh468-file (:use :cl))
(in-package :gh468-file)
(defparameter *syms* (list :|:DECK| :|::DECK| :|A:B| :deck 'cl-user::|X:Y|))
(defun runtime-syms () (list (intern ":DECK" :keyword) (intern "A:B" :keyword)))
"#,
    )
    .expect("write source");
    let _ = std::fs::remove_file(&fasl);
    run(&[
        "--eval",
        &format!(
            "(compile-file {:?} :output-file {:?})",
            source.display().to_string(),
            fasl.display().to_string()
        ),
    ]);
    assert!(fasl.exists(), "compile-file wrote no fasl");
    // Load in a FRESH process, so every symbol comes from the fasl.
    let stdout = run(&[
        "--eval",
        &format!("(load {:?})", fasl.display().to_string()),
        "--eval",
        r#"(let ((s gh468-file::*syms*))
             (assert (equal (mapcar #'symbol-name s) '(":DECK" "::DECK" "A:B" "DECK" "X:Y")))
             (assert (eq (first s) (intern ":DECK" :keyword)))
             (assert (eq (second s) (intern "::DECK" :keyword)))
             (assert (eq (third s) (intern "A:B" :keyword)))
             (assert (eq (fourth s) :deck))
             (assert (eq (fifth s) (intern "X:Y" :cl-user)))
             (assert (equal (gh468-file::runtime-syms) (list (first s) (third s))))
             (format t "INTERN-PASS~%"))"#,
    ]);
    assert!(stdout.lines().any(|line| line == "INTERN-PASS"), "{stdout}");
}
