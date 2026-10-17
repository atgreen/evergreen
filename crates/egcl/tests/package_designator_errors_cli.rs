// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A package designator naming no package is a PACKAGE-ERROR for every package
//! function (CLHS 11.1.1.2), and must not CREATE the package.
//!
//! Several of them created it instead, so a typo in a package name, or a
//! library loaded before the package it names, left a live package of that
//! name: FIND-PACKAGE then answered it, DEFPACKAGE of the real one merged into
//! the mistake, and the fault surfaced far from where it was made (GitHub
//! #375). Others answered as though the lookup had simply found nothing, which
//! is indistinguishable from the name being absent.
//!
//! The reported four were INTERN, EXPORT, USE-PACKAGE and FIND-SYMBOL; probing
//! the neighbours found SHADOW, IMPORT, UNUSE-PACKAGE and PACKAGE-USE-LIST with
//! the same defect, so all of them are pinned here. Verified against SBCL,
//! which signals a PACKAGE-ERROR subtype for every one.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defmacro check (label form)
    `(let ((outcome (handler-case (progn ,form :no-error)
                      (package-error () :package-error)
                      (error (e) (list :wrong-condition (type-of e))))))
       (format t "~a ~a~%" ,label outcome)))
  ;; Each operation signals, and leaves no package of that name behind.
  (check "INTERN"           (intern "Z" "NOSUCH-INTERN"))
  (check "FIND-SYMBOL"      (find-symbol "Z" "NOSUCH-FIND-SYMBOL"))
  (check "EXPORT"           (export 'cl-user::zz "NOSUCH-EXPORT"))
  (check "IMPORT"           (import 'cl-user::zz "NOSUCH-IMPORT"))
  (check "SHADOW"           (shadow 'zz "NOSUCH-SHADOW"))
  (check "USE-PACKAGE"      (use-package "NOSUCH-USED" "CL-USER"))
  (check "USE-PACKAGE-TGT"  (use-package "CL" "NOSUCH-TARGET"))
  (check "UNUSE-PACKAGE"    (unuse-package "NOSUCH-UNUSE"))
  (check "UNINTERN"         (unintern 'cl-user::zz "NOSUCH-UNINTERN"))
  (check "PACKAGE-USE-LIST" (package-use-list "NOSUCH-USE-LIST"))
  ;; NIL is the symbol whose NAME is "NIL", so it designates a package called
  ;; "NIL" -- which does not exist. SBCL signals here too.
  (check "INTERN-NIL"       (intern "Z" nil))
  (format t "CREATED ~a~%"
          (remove nil (mapcar #'find-package
                              '("NOSUCH-INTERN" "NOSUCH-FIND-SYMBOL" "NOSUCH-EXPORT"
                                "NOSUCH-IMPORT" "NOSUCH-SHADOW" "NOSUCH-USED"
                                "NOSUCH-TARGET" "NOSUCH-UNUSE" "NOSUCH-UNINTERN"
                                "NOSUCH-USE-LIST" "NIL"))))

  ;; DEFPACKAGE keeps its forward :use reference: ASDF's package graph relies on
  ;; one resolving later, and that is why the create-on-miss helper exists at
  ;; all. Narrowing the public functions must not take this away.
  (defpackage :pde-forward (:use :cl :pde-not-yet-defined))
  (format t "FORWARD-USE ~a~%" (and (find-package "PDE-FORWARD") t))

  ;; And the ordinary paths still work, so the new check is not simply refusing
  ;; everything.
  (defpackage :pde-real (:use :cl) (:export #:hello))
  (format t "WORKING ~a ~a ~a~%"
          (nth-value 1 (find-symbol "HELLO" "PDE-REAL"))
          (nth-value 1 (intern "FRESH" "PDE-REAL"))
          (and (listp (package-use-list "PDE-REAL")) t))
  (format t "PACKAGE-DESIGNATOR-ERRORS-OK~%")
"#;

#[test]
fn a_designator_naming_no_package_signals_and_creates_nothing() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");

    for op in [
        "INTERN",
        "FIND-SYMBOL",
        "EXPORT",
        "IMPORT",
        "SHADOW",
        "USE-PACKAGE",
        "USE-PACKAGE-TGT",
        "UNUSE-PACKAGE",
        "UNINTERN",
        "PACKAGE-USE-LIST",
        "INTERN-NIL",
    ] {
        assert!(
            stdout.contains(&format!("{op} PACKAGE-ERROR")),
            "{op} did not signal a PACKAGE-ERROR; got:\n{stdout}"
        );
    }
    // The point of the bug: nothing may be left behind.
    assert!(
        stdout.contains("CREATED NIL"),
        "a failed operation created a package:\n{stdout}"
    );
    assert!(
        stdout.contains("FORWARD-USE T"),
        "DEFPACKAGE's forward :use reference regressed:\n{stdout}"
    );
    assert!(
        stdout.contains("WORKING EXTERNAL NIL T"),
        "an ordinary package operation regressed:\n{stdout}"
    );
    assert!(stdout.contains("PACKAGE-DESIGNATOR-ERRORS-OK"), "{stdout}");
}
