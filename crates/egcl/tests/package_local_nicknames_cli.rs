// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Package-local nicknames (CDR 10): DEFPACKAGE's :LOCAL-NICKNAMES, the
//! reader, FIND-PACKAGE, INTERN and FIND-SYMBOL relative to the current
//! package, the four EGCL-EXT functions, and the :PACKAGE-LOCAL-NICKNAMES
//! feature. A library's (:local-nicknames (:a :alexandria)) was accepted and
//! then ignored: `a:define-constant` read as "package not found: A".

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defpackage :pln-target (:use :cl) (:export #:greet))
  (in-package :pln-target)
  (defun greet () :hello-from-target)
  (in-package :cl-user)
  (defpackage :pln-user (:use :cl) (:local-nicknames (:tgt :pln-target) (:k :keyword)))
  (in-package :pln-user)
  ;; The reader, in the package that owns the nickname.
  (assert (eq (tgt:greet) :hello-from-target))
  (assert (eq 'tgt:greet (find-symbol "GREET" :pln-target)))
  (assert (eq 'tgt::greet 'tgt:greet))
  (assert (eq k:some-key :some-key))
  ;; FIND-PACKAGE, INTERN and FIND-SYMBOL, relative to *PACKAGE*.
  (assert (eq (find-package "TGT") (find-package :pln-target)))
  (assert (eq (find-package :tgt) (find-package :pln-target)))
  (assert (eq (intern "GREET" :tgt) 'tgt:greet))
  (assert (equal (multiple-value-list (find-symbol "GREET" "TGT"))
                 (list 'tgt:greet :external)))
  ;; Local means local: invisible from another package, and not a global nickname.
  (assert (null (let ((*package* (find-package :cl-user))) (find-package "TGT"))))
  (assert (null (package-nicknames :pln-target)))
  ;; A local nickname shadows a global package name.
  (defpackage :pln-shadowed (:use :cl) (:export #:who))
  (in-package :pln-shadowed)
  (defun who () :global)
  (in-package :pln-user)
  (defpackage :pln-other (:use :cl) (:export #:who))
  (in-package :pln-other)
  (defun who () :local)
  (in-package :pln-user)
  (egcl-ext:add-package-local-nickname "PLN-SHADOWED" :pln-other)
  (assert (eq (find-package "PLN-SHADOWED") (find-package :pln-other)))
  (assert (eq (pln-shadowed:who) :local))
  (assert (eq (let ((*package* (find-package :cl-user)))
                (funcall (find-symbol "WHO" "PLN-SHADOWED")))
              :global))
  ;; The EGCL-EXT functions.
  (assert (equal (egcl-ext:package-local-nicknames :pln-user)
                 (list (cons "K" (find-package :keyword))
                       (cons "PLN-SHADOWED" (find-package :pln-other))
                       (cons "TGT" (find-package :pln-target)))))
  (assert (equal (egcl-ext:package-locally-nicknamed-by-list :pln-target)
                 (list (find-package :pln-user))))
  (assert (eq (egcl-ext:remove-package-local-nickname "PLN-SHADOWED") t))
  (assert (null (egcl-ext:remove-package-local-nickname "PLN-SHADOWED")))
  (assert (eq (find-package "PLN-SHADOWED") (find-package :pln-shadowed)))
  (assert (eq (egcl-ext:add-package-local-nickname :x :pln-target :pln-other)
              (find-package :pln-other)))
  (assert (eq (let ((*package* (find-package :pln-other))) (find-package :x))
              (find-package :pln-target)))
  (assert (null (egcl-ext:package-local-nicknames :pln-target)))
  ;; The same DEFPACKAGE again is a no-op; the errors are PACKAGE-ERRORs.
  (defpackage :pln-user (:use :cl) (:local-nicknames (:tgt :pln-target) (:k :keyword)))
  (assert (eq (tgt:greet) :hello-from-target))
  (assert (handler-case
            (progn (eval '(defpackage :pln-bad (:use :cl)
                            (:local-nicknames (:z :no-such-package-anywhere))))
                   nil)
            (package-error () t)))
  (assert (handler-case
            (progn (eval '(defpackage :pln-bad2 (:use :cl)
                            (:local-nicknames (:cl :pln-target))))
                   nil)
            (package-error () t)))
  (assert (handler-case
            (progn (egcl-ext:add-package-local-nickname "TGT" :pln-other) nil)
            (package-error () t)))
  ;; The functions are symbols with function cells, so UIOP's
  ;; UIOP/PACKAGE-LOCAL-NICKNAMES can import them and call them.
  (defpackage :pln-importer (:use :cl)
    (:import-from :egcl-ext #:package-local-nicknames #:add-package-local-nickname
                  #:remove-package-local-nickname))
  (assert (eq 'pln-importer::package-local-nicknames 'egcl-ext:package-local-nicknames))
  (assert (eq (pln-importer::add-package-local-nickname :imp :pln-target :pln-importer)
              (find-package :pln-importer)))
  (assert (equal (pln-importer::package-local-nicknames :pln-importer)
                 (list (cons "IMP" (find-package :pln-target)))))
  (assert (eq (funcall 'pln-importer::remove-package-local-nickname :imp :pln-importer) t))
  (assert (member :package-local-nicknames *features*))
  (format t "PLN-OK~%")
"#;

#[test]
fn package_local_nicknames_resolve_from_the_owning_package_only() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "package-local nicknames failed: {stdout}\n{stderr}"
    );
    assert!(stdout.contains("PLN-OK"), "missing PLN-OK in: {stdout}");
}

/// The shape that broke cl-smtp: a file whose DEFPACKAGE declares a local
/// nickname and whose later forms use it, through COMPILE-FILE and LOAD.
/// The pre-read must not fall back to a source-only artifact either.
const FILE_SOURCE: &str = r#"
(defpackage :pln-compiled-target (:use :cl) (:export #:greet))
(in-package :pln-compiled-target)
(defun greet () :hello-from-compiled-target)
(defpackage :pln-compiled (:use :cl) (:local-nicknames (:a :pln-compiled-target)))
(in-package :pln-compiled)
(defparameter *greeting* (a:greet))
(defun via-nickname () (a::greet))
"#;

#[test]
fn compile_file_reads_a_local_nickname_declared_earlier_in_the_file() {
    let dir = std::env::temp_dir();
    let source = dir.join(format!("egcl-pln-{}.lisp", std::process::id()));
    let fasl = dir.join(format!("egcl-pln-{}.fasl", std::process::id()));
    std::fs::write(&source, FILE_SOURCE).expect("write source");
    let program = format!(
        r#"(progn
             (compile-file "{src}" :output-file "{out}")
             (load "{out}")
             (assert (eq (symbol-value (find-symbol "*GREETING*" :pln-compiled))
                         :hello-from-compiled-target))
             (assert (eq (funcall (find-symbol "VIA-NICKNAME" :pln-compiled))
                         :hello-from-compiled-target))
             (format t "PLN-COMPILE-OK~%"))"#,
        src = source.display(),
        out = fasl.display()
    );
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&fasl);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "compile-file with a local nickname failed: {stdout}\n{stderr}"
    );
    assert!(stdout.contains("PLN-COMPILE-OK"), "missing PLN-COMPILE-OK in: {stdout}");
    assert!(
        !stdout.contains("could not pre-read") && !stderr.contains("could not pre-read"),
        "the pre-read fell back to a source-only artifact: {stdout}\n{stderr}"
    );
}
