// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Which metaclass a class metaobject belongs to must be decided from the
//! class graph, not from how a class value happens to be represented
//! (bliss-lkf8l, the first stage of bliss-rj5o).
//!
//! `is_standard_class_metaobject` used to short-circuit on
//! `class.is_symbol()`, which is true exactly because a `DEFCLASS` class IS
//! its own name symbol today while a built-in class is a negative fixnum.
//! That makes the representation the discriminator. The moment a class stops
//! being an immediate, a user class instead falls through to a hardcoded list
//! of metaobject-class names that by construction cannot contain it, and
//! answers "built-in" — wrongly, and silently, in four places at once:
//! `CLASS-OF` of a class, `TYPEP` against `STANDARD-CLASS` and against
//! `BUILT-IN-CLASS`, the specializer-distance table, and `DEFCLASS`'s own
//! "X is a system class and cannot be a direct superclass" rejection, which
//! would refuse every user superclass and break `lib/boot.lisp`.
//!
//! These rows are the answers BEFORE the change, measured, so the test pins
//! behaviour rather than the new implementation. The single deliberate
//! difference is `RESTART`, which resolves a contradiction: its precedence
//! list has always been `(RESTART STANDARD-OBJECT T)` while `CLASS-OF`
//! answered `BUILT-IN-CLASS`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Both evaluator tiers: each probe runs as a direct top-level form AND inside
/// a `DEFUN`, which is lowered. A check that holds in the tree-walking
/// evaluator does not cover `cli/bytecode.rs` (memory
/// `acceptance-eval-wrapper-hides-compiled-path`).
const PROGRAM: &str = r#"
  (defclass user-plain () ((a)))
  (defclass user-meta (standard-class) ())
  (defmethod egcl-mop:validate-superclass ((c user-meta) (s standard-class)) t)
  (defclass user-withmeta () () (:metaclass user-meta))
  ;; The rejection that would fire if a user class read as a system class.
  (defclass user-derived (user-plain) ((b)))
  (defstruct a-struct x)
  (define-condition a-cond (error) ())

  (defun kind (c)
    (list (if (typep c 'standard-class) :std nil)
          (if (typep c 'built-in-class) :builtin nil)
          (if (typep c 'structure-class) :struct nil)))
  (defun metaclass-name (c) (class-name (class-of c)))

  (defmacro report (label form) `(format t "~a ~a~%" ,label ,form))

  ;; Tier 1: direct top-level forms.
  (report "DIRECT-USER" (kind (find-class 'user-plain)))
  (report "DIRECT-BUILTIN" (kind (find-class 'integer)))
  (report "DIRECT-STRUCT" (kind (find-class 'a-struct)))
  ;; Tier 2: the same questions from inside a compiled function.
  (defun compiled-user () (kind (find-class 'user-plain)))
  (defun compiled-builtin () (kind (find-class 'integer)))
  (defun compiled-struct () (kind (find-class 'a-struct)))
  (defun compiled-meta () (metaclass-name (find-class 'user-plain)))
  (report "COMPILED-USER" (compiled-user))
  (report "COMPILED-BUILTIN" (compiled-builtin))
  (report "COMPILED-STRUCT" (compiled-struct))
  (report "COMPILED-METANAME" (compiled-meta))

  ;; CLASS-OF names the metaclass for each kind.
  (report "META-USER" (metaclass-name (find-class 'user-plain)))
  (report "META-DERIVED" (metaclass-name (find-class 'user-derived)))
  (report "META-COND" (metaclass-name (find-class 'a-cond)))
  (report "META-STRUCT" (metaclass-name (find-class 'a-struct)))
  (report "META-BUILTIN" (metaclass-name (find-class 'integer)))
  (report "META-STDCLASS" (metaclass-name (find-class 'standard-class)))
  (report "META-WITHMETA" (metaclass-name (find-class 'user-withmeta)))

  ;; A DEFCLASS whose superclass is a user class must be accepted; reading a
  ;; user class as a system class is what used to turn this into an error.
  (report "USER-SUPERCLASS-OK" (and (find-class 'user-derived) t))
  ;; And a genuine system class must still be refused.
  (report "SYSTEM-SUPERCLASS-REFUSED"
          (handler-case (progn (eval '(defclass bad-sub (integer) ())) nil)
            (error () t)))

  ;; The specializer-distance table asked `!class.is_symbol()` for
  ;; BUILT-IN-CLASS while TYPEP asked the standard-class predicate: two
  ;; different questions that agreed only by accident of the representation.
  ;;
  ;; Each probe gets its OWN generic and is called exactly ONCE. That is
  ;; required, not tidy: every symbol-valued class metaobject produces the same
  ;; dispatch-cache key, so the first class dispatched through a generic
  ;; decides the effective method for every later one — with a single method
  ;; defined, `std-only` called on a user class first then answers :STD for a
  ;; structure class too (bliss-xv37u, live on main, not caused by this
  ;; change). One call per generic measures applicability, which is what this
  ;; change touches, instead of that cache.
  (defun applies (fn c)
    (handler-case (funcall fn c) (error () :none)))

  (defgeneric p1 (c)) (defmethod p1 ((c standard-class)) :std)
  (report "STD-ON-USER" (applies #'p1 (find-class 'user-plain)))
  (defgeneric p2 (c)) (defmethod p2 ((c standard-class)) :std)
  (report "STD-ON-BUILTIN" (applies #'p2 (find-class 'integer)))
  (defgeneric p3 (c)) (defmethod p3 ((c standard-class)) :std)
  (report "STD-ON-STRUCT" (applies #'p3 (find-class 'a-struct)))
  (defgeneric p4 (c)) (defmethod p4 ((c built-in-class)) :builtin)
  (report "BUILTIN-ON-USER" (applies #'p4 (find-class 'user-plain)))
  (defgeneric p5 (c)) (defmethod p5 ((c built-in-class)) :builtin)
  (report "BUILTIN-ON-BUILTIN" (applies #'p5 (find-class 'integer)))
  (defgeneric p6 (c)) (defmethod p6 ((c structure-class)) :struct)
  (report "STRUCT-ON-STRUCT" (applies #'p6 (find-class 'a-struct)))
  (defgeneric p7 (c)) (defmethod p7 ((c structure-class)) :struct)
  (report "STRUCT-ON-USER" (applies #'p7 (find-class 'user-plain)))
  (defgeneric p8 (c)) (defmethod p8 ((c standard-class)) :std)
  (defun compiled-dispatch () (applies #'p8 (find-class 'user-plain)))
  (report "COMPILED-DISPATCH" (compiled-dispatch))
  (format t "CLASS-METACLASS-OK~%")
"#;

fn run(args: &[&str]) -> (String, String, bool) {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", BIN])
        .args(args)
        .output()
        .expect("run egcl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn a_class_metaobjects_metaclass_is_decided_by_the_class_graph() {
    let (stdout, stderr, ok) = run(&["--no-init", "--eval", PROGRAM]);
    assert!(ok, "{stdout}\n{stderr}");

    for expected in [
        // A user class is a standard class in both tiers, and in neither of
        // the other two.
        "DIRECT-USER (STD NIL NIL)",
        "COMPILED-USER (STD NIL NIL)",
        "DIRECT-BUILTIN (NIL BUILTIN NIL)",
        "COMPILED-BUILTIN (NIL BUILTIN NIL)",
        "DIRECT-STRUCT (NIL NIL STRUCT)",
        "COMPILED-STRUCT (NIL NIL STRUCT)",
        "COMPILED-METANAME STANDARD-CLASS",
        // CLASS-OF per kind.
        "META-USER STANDARD-CLASS",
        "META-DERIVED STANDARD-CLASS",
        "META-COND STANDARD-CLASS",
        "META-STRUCT STRUCTURE-CLASS",
        "META-BUILTIN BUILT-IN-CLASS",
        "META-STDCLASS STANDARD-CLASS",
        // A named :metaclass still wins (GitHub #371 / #380).
        "META-WITHMETA USER-META",
        // The DEFCLASS gate keeps both of its answers.
        "USER-SUPERCLASS-OK T",
        "SYSTEM-SUPERCLASS-REFUSED T",
        // Dispatch applicability agrees with TYPEP, which is what the shared
        // predicate buys: a BUILT-IN-CLASS method must not reach a user class,
        // and a STANDARD-CLASS method must not reach a built-in or a struct.
        // `~a` princ-es a keyword without its package marker, hence no colon.
        "STD-ON-USER STD",
        "STD-ON-BUILTIN NONE",
        "STD-ON-STRUCT NONE",
        "BUILTIN-ON-USER NONE",
        "BUILTIN-ON-BUILTIN BUILTIN",
        "STRUCT-ON-STRUCT STRUCT",
        "STRUCT-ON-USER NONE",
        "COMPILED-DISPATCH STD",
        "CLASS-METACLASS-OK",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}

/// RESTART is the one deliberate behaviour change, so it gets its own test
/// saying why rather than riding along in the table above.
///
/// `(find-class 'restart)` answered `BUILT-IN-CLASS` while its precedence list
/// was `(RESTART STANDARD-OBJECT T)` — a class whose instances are standard
/// objects but which claimed not to be a standard class. The bootstrap's own
/// comment calls it "a standard class whose instances are the first-class
/// restart objects". Deciding from the class graph makes the two agree.
#[test]
fn restart_is_a_standard_class_consistently_with_its_precedence_list() {
    let (stdout, stderr, ok) = run(&[
        "--no-init",
        "--eval",
        r#"
          (format t "CPL ~a~%" (mapcar #'class-name
                                       (class-precedence-list (find-class 'restart))))
          (format t "META ~a~%" (class-name (class-of (find-class 'restart))))
          (format t "STD ~a~%" (typep (find-class 'restart) 'standard-class))
          (format t "BUILTIN ~a~%" (typep (find-class 'restart) 'built-in-class))
          (format t "SUBTYPEP ~a~%" (multiple-value-list
                                      (subtypep 'restart 'standard-object)))
          ;; STRUCTURE-OBJECT stays built-in and stays NOT a standard object,
          ;; so the RESTART change is not a blanket one.
          (format t "SO-META ~a~%" (class-name (class-of (find-class 'structure-object))))
          (format t "SO-SUBTYPEP ~a~%" (multiple-value-list
                                         (subtypep 'structure-object 'standard-object)))
          (format t "RESTART-OK~%")
        "#,
    ]);
    assert!(ok, "{stdout}\n{stderr}");
    for expected in [
        "CPL (RESTART STANDARD-OBJECT T)",
        "META STANDARD-CLASS",
        "STD T",
        "BUILTIN NIL",
        // The point of the change: all three now agree. SUBTYPEP answered
        // (NIL T) while the precedence list said STANDARD-OBJECT.
        "SUBTYPEP (T T)",
        "SO-META BUILT-IN-CLASS",
        "SO-SUBTYPEP (NIL T)",
        "RESTART-OK",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
}
