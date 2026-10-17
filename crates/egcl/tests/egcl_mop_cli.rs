// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The EGCL-MOP package: the metaobject introspection Gendl's glisp layer asks
//! the host for, and a `:metaclass` that takes effect (GitHub #371).
//!
//! `docs/egcl-lisp-api.md` specified EGCL-MOP as "not installed by the
//! bootstrap"; it is installed now. The decisive item is the metaclass:
//! Gendl's `define-object` expands to a DEFCLASS with `:metaclass gdl-class`
//! and its kernel tells one of its own classes from a plain one with
//! `(eql (class-of (find-class name)) (find-class 'gdl-class))`. The option was
//! parsed and dropped, so every Gendl class looked plain and the
//! message-passing machinery never engaged.
//!
//! The non-ANSI readers also MOVE here out of COMMON-LISP, which is the
//! tractable half of GitHub #370: a package that does `(:use :cl)` no longer
//! inherits `class-slots`, so a library's own DEFUN of that name cannot
//! redefine the implementation's. COMMON-LISP-USER uses EGCL-MOP, so
//! interactive use and closer-mop's `(cl-user::class-slots ...)` keep working.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defclass probe-meta (standard-class) ())
  (defclass probe-a () ((x)))
  (defclass probe-b (probe-a) ((y)))
  (defclass probe-c () ((z)) (:metaclass probe-meta))
  (defgeneric gf1 (x))
  (defmethod gf1 ((x (eql 5))) :five)
  (defmethod gf1 ((x integer)) :int)

  ;; The deciding item: CLASS-OF answers the metaclass DEFCLASS named, and a
  ;; class without one is unchanged.
  (format t "METACLASS ~a~%" (eql (class-of (find-class 'probe-c))
                                  (find-class 'probe-meta)))
  (format t "PLAIN ~a~%" (eq (class-of (find-class 'probe-b))
                             (find-class 'standard-class)))

  ;; The introspection, through the package.
  (format t "SUPERS ~a~%" (mapcar #'class-name
                                  (egcl-mop:class-direct-superclasses (find-class 'probe-b))))
  (format t "SUBS ~a~%" (mapcar #'class-name
                                (egcl-mop:class-direct-subclasses (find-class 'probe-a))))
  (format t "METHODS ~a~%" (length (egcl-mop:generic-function-methods #'gf1)))
  (format t "FINALIZED ~a ~a~%"
          (egcl-mop:class-finalized-p (find-class 'probe-b))
          (class-name (egcl-mop:finalize-inheritance (find-class 'probe-b))))
  (format t "VALIDATE ~a~%" (egcl-mop:validate-superclass (find-class 'probe-b)
                                                          (find-class 'probe-a)))
  ;; An EQL specializer compares with what METHOD-SPECIALIZERS answers, as on
  ;; other hosts, so portable code can match one against a method's.
  (format t "EQL-SPEC ~a~%"
          (equal (egcl-mop:intern-eql-specializer 5)
                 (first (find-if (lambda (s) (equal s '((eql 5))))
                                 (mapcar #'method-specializers
                                         (egcl-mop:generic-function-methods #'gf1))))))
  (format t "SLOTS ~a~%" (length (egcl-mop:class-slots (find-class 'probe-b))))

  ;; CLASS-PRECEDENCE-LIST still answers class metaobjects, which is what
  ;; portable code walks. It stays in COMMON-LISP (see the test below).
  (format t "CPL ~a~%" (mapcar #'class-name (class-precedence-list (find-class 'probe-b))))

  ;; GitHub #370, the half this fixes: a (:use :cl)-only package gets its OWN
  ;; symbol, so its DEFUN cannot reach the implementation's function.
  (defpackage :mop-clobber (:use :cl))
  (in-package :mop-clobber)
  (defun class-slots (c) (list :mine c))
  (in-package :cl-user)
  (format t "OWN-SYMBOL ~a~%" (not (eq 'mop-clobber::class-slots 'egcl-mop:class-slots)))
  (format t "LIBRARY-OWN ~a~%" (equal (mop-clobber::class-slots :x) '(:mine :x)))
  (format t "IMPL-INTACT ~a~%" (length (egcl-mop:class-slots (find-class 'probe-b))))
  ;; closer-mop's EGCL arm is (defun class-slots (c) (cl-user::class-slots c)),
  ;; so that spelling has to keep resolving.
  (format t "CL-USER-REACHABLE ~a~%" (and (fboundp 'cl-user::class-slots) t))

  ;; #370's other half: CLASS-PRECEDENCE-LIST too. A (:use :cl)-only package
  ;; defining that name must get its OWN symbol, and ours must survive. This
  ;; needs a FILE rather than one --eval form: in-package only affects
  ;; SUBSEQUENT reads, so a single (progn (in-package :p) (defun ...)) is read
  ;; entirely in the old package and cannot test this at all.
  (with-open-file (o "CPLPROBE" :direction :output :if-exists :supersede)
    (write-line "(defpackage :mop-cpl-clob (:use :cl))" o)
    (write-line "(in-package :mop-cpl-clob)" o)
    (write-line "(defun class-precedence-list (c) (list :mine c))" o)
    (write-line "(in-package :cl-user)" o)
    (write-line "(format t \"CPL-OWN ~a~%\" (eq (symbol-package (find-symbol \"CLASS-PRECEDENCE-LIST\" :mop-cpl-clob)) (find-package :mop-cpl-clob)))" o)
    (write-line "(format t \"CPL-IMPL-INTACT ~a~%\" (mapcar #'class-name (class-precedence-list (find-class 'probe-b))))" o))
  (load "CPLPROBE")
  (delete-file "CPLPROBE")

  ;; A program can USE the package, which is the point of having one.
  (defpackage :mop-user (:use :cl :egcl-mop))
  (format t "USABLE ~a~%" (and (find-symbol "CLASS-DIRECT-SUPERCLASSES" :mop-user) t))
  (format t "EGCL-MOP-OK~%")
"#;

#[test]
fn egcl_mop_provides_the_introspection_and_an_effective_metaclass() {
    let probe = std::env::temp_dir().join(format!("egcl-cpl-probe-{}.lisp", std::process::id()));
    let program = PROGRAM.replace("CPLPROBE", &probe.display().to_string());
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&probe);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");

    for expected in [
        "METACLASS T",
        "PLAIN T",
        "SUPERS (PROBE-A)",
        "SUBS (PROBE-B)",
        "METHODS 2",
        "FINALIZED T PROBE-B",
        "VALIDATE T",
        "EQL-SPEC T",
        "SLOTS 2",
        "CPL (PROBE-B PROBE-A STANDARD-OBJECT T)",
        "OWN-SYMBOL T",
        "LIBRARY-OWN T",
        "IMPL-INTACT 2",
        "CL-USER-REACHABLE T",
        "USABLE T",
        "CPL-OWN T",
        "CPL-IMPL-INTACT (PROBE-B PROBE-A STANDARD-OBJECT T)",
        "EGCL-MOP-OK",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}

/// METHOD-SPECIALIZERS deliberately STAYS unowned, and this pins that so the
/// next person does not "finish" the move without the piece it needs.
///
/// CLASS-PRECEDENCE-LIST no longer does: it is owned by EGCL-MOP now, which is
/// what stops a library's own DEFUN of that name from redefining ours (#370).
/// The piece that made it safe is dispatching the EGCL-MOP spelling as well, so
/// the exported symbol is the builtin itself.
///
/// They are name-dispatched Rust builtins: the dispatch matches the bare symbol
/// name and ignores the package, so moving the symbol out of COMMON-LISP does
/// not stop a library's own DEFUN of that name from overriding the
/// implementation's for every caller. Measured: with the symbol moved, a
/// `(defun class-precedence-list ...)` in an unrelated package still made the
/// implementation's own call answer the library's value. Fixing that is
/// bliss-7l7cn, not this change -- and exporting the names from EGCL-MOP before
/// then interned fresh FUNCTIONLESS symbols that shadowed the real builtins and
/// made (class-precedence-list ...) undefined in COMMON-LISP-USER.
#[test]
fn the_builtin_mop_names_stay_in_common_lisp_until_dispatch_respects_packages() {
    let program = r#"
      (format t "CPL-HOME ~a~%" (package-name (symbol-package 'class-precedence-list)))
      (format t "MS-HOME ~a~%" (package-name (symbol-package 'method-specializers)))
      (format t "CPL-CALLABLE ~a~%"
              (and (mapcar #'class-name (class-precedence-list (find-class 'standard-object))) t))
      (format t "NOT-EXPORTED ~a~%"
              (list (nth-value 1 (find-symbol "CLASS-PRECEDENCE-LIST" :egcl-mop))
                    (nth-value 1 (find-symbol "METHOD-SPECIALIZERS" :egcl-mop))))
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("CPL-HOME EGCL-MOP"), "{stdout}");
    assert!(stdout.contains("MS-HOME COMMON-LISP"), "{stdout}");
    assert!(stdout.contains("CPL-CALLABLE T"), "{stdout}");
    // Not accessible in EGCL-MOP at all. They are INTERNAL symbols of
    // COMMON-LISP, so (:use :common-lisp) never inherited them either -- the
    // leak #370 reports comes from the bare-read bootstrap-identity rule, not
    // from package inheritance. What matters here is that EGCL-MOP does not
    // intern its own: listing them in its :export did, and those functionless
    // symbols shadowed the real builtins in COMMON-LISP-USER.
    // CPL is EXTERNAL in EGCL-MOP now; METHOD-SPECIALIZERS is still not
    // accessible there at all.
    assert!(stdout.contains("NOT-EXPORTED (EXTERNAL NIL)"), "{stdout}");
}
