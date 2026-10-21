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

/// CLASS-PRECEDENCE-LIST is owned by EGCL-MOP, which is what stops a library's
/// own DEFUN of that name from redefining ours (#370). The piece that made it
/// safe is dispatching the EGCL-MOP spelling as well, so the exported symbol is
/// the builtin itself.
///
/// METHOD-SPECIALIZERS is owned by EGCL-MOP too since GitHub #508, as a real
/// function over a %-entry point. The bare builtin of that name stays in
/// COMMON-LISP, internal, and is what a (:use :cl) package's bare read still
/// reaches: those builtins dispatch by bare name and ignore the package, so a
/// library's DEFUN of it still replaces the builtin for every caller -- that is
/// bliss-7l7cn. What this pins is that EGCL-MOP's reader is not reached by it.
#[test]
fn egcl_mop_owns_its_mop_readers_and_a_library_defun_cannot_reach_them() {
    let program = r#"
      (defgeneric owned-gf (x))
      (defmethod owned-gf ((x integer)) x)
      (format t "CPL-HOME ~a~%" (package-name (symbol-package 'class-precedence-list)))
      (format t "MS-HOME ~a~%" (package-name (symbol-package 'method-specializers)))
      (format t "CPL-CALLABLE ~a~%"
              (and (mapcar #'class-name (class-precedence-list (find-class 'standard-object))) t))
      (format t "EXPORTED ~a~%"
              (list (nth-value 1 (find-symbol "CLASS-PRECEDENCE-LIST" :egcl-mop))
                    (nth-value 1 (find-symbol "METHOD-SPECIALIZERS" :egcl-mop))))
      ;; A library package that only uses CL, defining METHOD-SPECIALIZERS. Its
      ;; own symbol, and EGCL-MOP's reader still answers the implementation's
      ;; value. A FILE, because in-package affects only SUBSEQUENT reads.
      (with-open-file (o "MSPROBE" :direction :output :if-exists :supersede)
        (write-line "(defpackage :ms-clob (:use :cl))" o)
        (write-line "(in-package :ms-clob)" o)
        (write-line "(defun method-specializers (m) (list :mine m))" o)
        (write-line "(in-package :cl-user)" o))
      (load "MSPROBE")
      (delete-file "MSPROBE")
      (format t "MS-OWN ~a~%"
              (not (eq (find-symbol "METHOD-SPECIALIZERS" :ms-clob) 'egcl-mop:method-specializers)))
      (format t "MS-IMPL-INTACT ~a~%"
              (mapcar #'class-name
                      (egcl-mop:method-specializers
                       (first (egcl-mop:generic-function-methods #'owned-gf)))))
    "#;
    let probe = std::env::temp_dir().join(format!("egcl-ms-probe-{}.lisp", std::process::id()));
    let program = program.replace("MSPROBE", &probe.display().to_string());
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&probe);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for expected in [
        "CPL-HOME EGCL-MOP",
        "MS-HOME EGCL-MOP",
        "CPL-CALLABLE T",
        "EXPORTED (EXTERNAL EXTERNAL)",
        "MS-OWN T",
        "MS-IMPL-INTACT (INTEGER)",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}

/// Generic function and method introspection (GitHub #508): what a library
/// needs to find every generic function with a method on some class, and to
/// write a congruent method of its own on each -- a geometry kernel binding
/// installs a locking :AROUND method that way. Answers come from the registry
/// dispatch uses, so REMOVE-METHOD and redefinition show through.
const GF_PROGRAM: &str = r#"
  (defclass k () ())
  (defclass k2 (k) ())
  (defgeneric g (x &key tol))
  (defmethod g ((x k) &key tol) tol)
  (defmethod g ((x (eql 5)) &key tol) tol)
  (defgeneric h (a b) (:argument-precedence-order b a))
  (defmethod h ((a k) b) 1)
  (defmethod h (a (b k2)) 2)
  (defmethod (setf sx) (v (o k)) v)
  (defun names (gfs) (mapcar #'egcl-mop:generic-function-name gfs))

  ;; The issue's example.
  ;; In method definition order.
  (format t "SDGF ~s~%" (names (egcl-mop:specializer-direct-generic-functions (find-class 'k))))
  (format t "SDGF-EQ ~s~%"
          (equal (egcl-mop:specializer-direct-generic-functions (find-class 'k))
                 (list #'g #'h #'(setf sx))))
  (format t "SDM ~s~%" (length (egcl-mop:specializer-direct-methods (find-class 'k))))
  (format t "MS ~s~%"
          (mapcar (lambda (m)
                    (mapcar (lambda (s) (if (consp s) s (class-name s)))
                            (egcl-mop:method-specializers m)))
                  (egcl-mop:generic-function-methods #'g)))
  (format t "LL ~s~%" (egcl-mop:generic-function-lambda-list #'g))
  ;; A generic function DEFMETHOD created: &KEY with no keyword names (#507).
  (defmethod implicit-gf ((x k2) &key p) p)
  (format t "IMPLICIT-LL ~s ~s~%" (egcl-mop:generic-function-lambda-list #'implicit-gf)
          (egcl-mop:generic-function-lambda-list #'(setf sx)))
  (format t "NAME ~s ~s~%" (egcl-mop:generic-function-name #'g)
          (egcl-mop:generic-function-name #'(setf sx)))
  (format t "APO ~s ~s~%" (egcl-mop:generic-function-argument-precedence-order #'h)
          (egcl-mop:generic-function-argument-precedence-order #'g))
  (format t "MGF ~s ~s~%"
          (eq #'g (egcl-mop:method-generic-function (first (egcl-mop:generic-function-methods #'g))))
          (eq #'(setf sx) (egcl-mop:method-generic-function
                           (first (egcl-mop:generic-function-methods #'(setf sx))))))
  (format t "MLL ~s~%" (mapcar #'egcl-mop:method-lambda-list (egcl-mop:generic-function-methods #'g)))
  ;; An EQL specializer, as INTERN-EQL-SPECIALIZER makes it.
  (format t "EQL-SDM ~s~%"
          (names (mapcar #'egcl-mop:method-generic-function
                         (egcl-mop:specializer-direct-methods (egcl-mop:intern-eql-specializer 5)))))
  (format t "EQL-OBJ ~s~%" (egcl-mop:eql-specializer-object (egcl-mop:intern-eql-specializer 5)))
  ;; An unspecialized parameter's specializer is the class T.
  (format t "T-SDM ~s~%"
          (and (member (find-if (lambda (m) (equal (egcl-mop:method-specializers m)
                                                   (list (find-class t) (find-class 'k2))))
                                (egcl-mop:generic-function-methods #'h))
                       (egcl-mop:specializer-direct-methods (find-class t)))
               t))

  ;; COMPUTE-APPLICABLE-METHODS-USING-CLASSES: two values, AMOP.
  (defun spec-names (ms)
    (mapcar (lambda (m) (mapcar (lambda (s) (if (consp s) s (class-name s)))
                                (egcl-mop:method-specializers m)))
            ms))
  (format t "CAMUC-K ~s~%"
          (multiple-value-bind (ms ok)
              (egcl-mop:compute-applicable-methods-using-classes #'g (list (find-class 'k)))
            (list (spec-names ms) ok)))
  ;; An INTEGER could be 5, so the classes do not decide it.
  (format t "CAMUC-INT ~s~%"
          (multiple-value-list
           (egcl-mop:compute-applicable-methods-using-classes #'g (list (find-class 'integer)))))
  ;; A STRING cannot, so nothing applies and that is definitive.
  (format t "CAMUC-STR ~s~%"
          (multiple-value-list
           (egcl-mop:compute-applicable-methods-using-classes #'g (list (find-class 'string)))))
  ;; Ordered as dispatch orders them, in the argument precedence order (B A).
  (format t "CAMUC-H ~s~%"
          (multiple-value-bind (ms ok)
              (egcl-mop:compute-applicable-methods-using-classes
               #'h (list (find-class 'k2) (find-class 'k2)))
            (list (spec-names ms) ok)))
  (format t "CAM-H ~s~%"
          (spec-names (compute-applicable-methods #'h (list (make-instance 'k2) (make-instance 'k2)))))

  ;; REMOVE-METHOD and redefinition show through.
  (remove-method #'g (find-if (lambda (m) (equal (egcl-mop:method-specializers m)
                                                 (list (find-class 'k))))
                              (egcl-mop:generic-function-methods #'g)))
  (format t "AFTER-REMOVE ~s~%"
          (names (egcl-mop:specializer-direct-generic-functions (find-class 'k))))
  (defmethod h ((a k) b) 3)
  (format t "AFTER-REDEF ~s~%" (length (egcl-mop:specializer-direct-methods (find-class 'k))))
  (format t "GF-INTROSPECTION-OK~%")
"#;

const GF_EXPECTED: &[&str] = &[
    "SDGF (G H (SETF SX))",
    "SDGF-EQ T",
    "SDM 3",
    "MS ((K) ((EQL 5)))",
    "LL (X &KEY TOL)",
    "IMPLICIT-LL (X &KEY) (V O)",
    "NAME G (SETF SX)",
    "APO (B A) (X)",
    "MGF T T",
    "MLL ((X &KEY TOL) (X &KEY TOL))",
    "EQL-SDM (G)",
    "EQL-OBJ 5",
    "T-SDM T",
    "CAMUC-K (((K)) T)",
    "CAMUC-INT (NIL NIL)",
    "CAMUC-STR (NIL T)",
    "CAMUC-H (((T K2) (K T)) T)",
    "CAM-H ((T K2) (K T))",
    "AFTER-REMOVE (H (SETF SX))",
    "AFTER-REDEF 2",
    "GF-INTROSPECTION-OK",
];

fn run_gf_program(stress: bool) -> String {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", GF_PROGRAM]);
    if stress {
        command
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for expected in GF_EXPECTED {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
    stdout
        .lines()
        .filter(|line| !line.starts_with("[gc-stress]"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn egcl_mop_introspects_generic_functions_and_methods() {
    run_gf_program(false);
}

/// The new readers allocate result lists while holding values read from the
/// method registry; a forced minor collection on every allocation must not
/// change any answer.
#[test]
fn egcl_mop_generic_function_introspection_under_gc_stress() {
    assert_eq!(run_gf_program(true), run_gf_program(false));
}
