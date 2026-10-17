// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A built-in class is an object, not an integer (bliss-vsjn1, bliss-rj5o).
//!
//! The built-in classes were negative fixnums -1 ..= -47, so an ordinary
//! arithmetic value WAS a class: `(eql -14 (find-class 'integer))` answered T,
//! `(class-name -14)` answered INTEGER, and a method specialized on
//! `BUILT-IN-CLASS` applied to the integer -14. They are pinned, zero-slot
//! `STANDARD_OBJECT`s now.
//!
//! The metaclass answers must not move while the representation does, which is
//! the real risk in a change like this, so the table below is the one measured
//! before it.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(args: &[&str]) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "180", BIN])
        .args(args)
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

fn expect(text: &str, rows: &[&str]) {
    for row in rows {
        assert!(text.contains(row), "missing {row:?} in:\n{text}");
    }
}

#[test]
fn a_built_in_class_is_disjoint_from_integer_and_symbol() {
    let stdout = run(&[
        "--no-init",
        "--eval",
        r#"
          (format t "INTEGERP ~a~%" (integerp (find-class 'standard-class)))
          (format t "NUMBERP ~a~%" (numberp (find-class 'integer)))
          (format t "SYMBOLP ~a~%" (symbolp (find-class 'integer)))
          ;; -14 was the INTEGER class. It must be an ordinary integer again.
          (format t "EQL-14 ~a~%" (eql -14 (find-class 'integer)))
          (format t "NAME-OF-14 ~a~%" (class-name -14))
          (format t "TYPE-OF-14 ~a~%" (type-of -14))
          ;; and a method on BUILT-IN-CLASS must not apply to it
          (defgeneric k (x))
          (defmethod k ((x built-in-class)) :bic)
          (format t "DISPATCH-14 ~a~%"
                  (handler-case (k -14) (error () :no-applicable-method)))
          (format t "DISPATCH-CLASS ~a~%" (k (find-class 'integer)))
          (format t "BUILTIN-OBJECTS-OK~%")
        "#,
    ]);
    expect(&stdout, &[
        "INTEGERP NIL",
        "NUMBERP NIL",
        "SYMBOLP NIL",
        "EQL-14 NIL",
        "NAME-OF-14 NIL",
        "TYPE-OF-14 FIXNUM",
        "DISPATCH-14 NO-APPLICABLE-METHOD",
        "DISPATCH-CLASS BIC",
        "BUILTIN-OBJECTS-OK",
    ]);
}

/// The metaclass of every kind of class, which is what must NOT change.
/// Measured before the conversion and reproduced here.
#[test]
fn every_kind_of_class_keeps_its_metaclass() {
    let stdout = run(&[
        "--no-init",
        "--eval",
        r#"
          (defclass uc () ())
          (defstruct st x)
          (define-condition cc (error) ())
          (dolist (c '(uc st cc standard-object class standard-class built-in-class
                       structure-class generic-function standard-generic-function
                       method standard-method method-combination
                       integer fixnum string cons symbol t hash-table package
                       stream pathname structure-object))
            (format t "META ~a ~a~%" c (class-name (class-of (find-class c)))))
          ;; The three-way TYPEP split, including the two the dedicated
          ;; metaclass arm of TYPEP exists for.
          (format t "SO-IS-STRUCTURE-CLASS ~a~%"
                  (typep (find-class 'structure-object) 'structure-class))
          (format t "SO-IS-STANDARD-CLASS ~a~%"
                  (typep (find-class 'standard-object) 'standard-class))
          (format t "INT-IS-BUILTIN ~a~%" (typep (find-class 'integer) 'built-in-class))
          (format t "UC-IS-STANDARD ~a~%" (typep (find-class 'uc) 'standard-class))
          (format t "ST-IS-STRUCTURE ~a~%" (typep (find-class 'st) 'structure-class))
          (format t "CLASSES-ARE-CLASSES ~a~%"
                  (every (lambda (c) (typep (find-class c) 'class))
                         '(uc st cc integer standard-class structure-object)))
          (format t "METACLASS-TABLE-OK~%")
        "#,
    ]);
    expect(&stdout, &[
        // user, struct and condition classes
        "META UC STANDARD-CLASS",
        "META ST STRUCTURE-CLASS",
        "META CC STANDARD-CLASS",
        // the ten AMOP metaobject classes are standard classes
        "META STANDARD-OBJECT STANDARD-CLASS",
        "META CLASS STANDARD-CLASS",
        "META STANDARD-CLASS STANDARD-CLASS",
        "META BUILT-IN-CLASS STANDARD-CLASS",
        "META STRUCTURE-CLASS STANDARD-CLASS",
        "META GENERIC-FUNCTION STANDARD-CLASS",
        "META STANDARD-GENERIC-FUNCTION STANDARD-CLASS",
        "META METHOD STANDARD-CLASS",
        "META STANDARD-METHOD STANDARD-CLASS",
        "META METHOD-COMBINATION STANDARD-CLASS",
        // everything else built-in, STRUCTURE-OBJECT included
        "META INTEGER BUILT-IN-CLASS",
        "META FIXNUM BUILT-IN-CLASS",
        "META STRING BUILT-IN-CLASS",
        "META CONS BUILT-IN-CLASS",
        "META SYMBOL BUILT-IN-CLASS",
        "META T BUILT-IN-CLASS",
        "META HASH-TABLE BUILT-IN-CLASS",
        "META PACKAGE BUILT-IN-CLASS",
        "META STREAM BUILT-IN-CLASS",
        "META PATHNAME BUILT-IN-CLASS",
        "META STRUCTURE-OBJECT BUILT-IN-CLASS",
        "SO-IS-STRUCTURE-CLASS T",
        "SO-IS-STANDARD-CLASS T",
        "INT-IS-BUILTIN T",
        "UC-IS-STANDARD T",
        "ST-IS-STRUCTURE T",
        "CLASSES-ARE-CLASSES T",
        "METACLASS-TABLE-OK",
    ]);
}

/// A built-in class must come back from a core as an object, and the class
/// graph must still be usable afterwards — the restored `is_builtin_class`
/// decides from the precedence list, so this also pins that it survives.
#[test]
fn built_in_classes_survive_a_saved_core() {
    let dir = std::env::temp_dir().join(format!("egcl-bi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = dir.join("bi.core").display().to_string();

    let saved = run(&[
        "--no-init",
        "--eval",
        "(format t \"PRE ~a ~a~%\" (class-name (class-of (find-class 'integer))) \
                                   (integerp (find-class 'integer)))",
        "--eval",
        &format!("(egcl-ext:save-lisp-and-die {core:?})"),
    ]);
    expect(&saved, &["PRE BUILT-IN-CLASS NIL"]);

    let restored = run(&[
        "--image",
        &core,
        "--no-init",
        "--eval",
        r#"
          (format t "POST ~a ~a~%" (class-name (class-of (find-class 'integer)))
                                   (integerp (find-class 'integer)))
          (format t "POST-TYPEP ~a~%" (typep (find-class 'integer) 'built-in-class))
          (defclass after-restore () ((q :initform 7)))
          (format t "POST-NEW-CLASS ~a~%" (slot-value (make-instance 'after-restore) 'q))
          (format t "POST-REFUSES-BUILTIN ~a~%"
                  (handler-case (progn (make-instance (find-class 'integer)) :accepted)
                    (error () :refused)))
          (format t "RESTORE-OK~%")
        "#,
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    expect(&restored, &[
        "POST BUILT-IN-CLASS NIL",
        "POST-TYPEP T",
        "POST-NEW-CLASS 7",
        "POST-REFUSES-BUILTIN REFUSED",
        "RESTORE-OK",
    ]);
}
