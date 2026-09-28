//! A class or condition in one package must not alias COMMON-LISP's of the same
//! bare name (bliss-kliz4).
//!
//! The condition registries and the CLOS class registry were keyed by the
//! package-STRIPPED bare name, so defining `MYPKG:ERROR` put its definition where
//! `ERROR`'s belonged. The consequence was not a wrong answer but a CRASH, and in a
//! place with nothing to do with the offending definition: that definition's parent
//! is `CL:ERROR`, also reduced to `ERROR`, so walking the parents found the same
//! entry, and `MAKE-CONDITION` of ANY condition recursed until the stack was gone.
//!
//!     (define-condition mypkg::error (cl:error) ((k :initarg :k)))
//!     (define-condition later (error) ((k :initarg :k)))
//!     (make-condition 'later :k 5)          => SIGSEGV
//!
//! `HANDLER-CASE` of `CL:ERROR` kept working throughout, because handler matching
//! compares class-hierarchy names as strings rather than going through the registry —
//! so the breakage was invisible from the direction anyone would check first.

use std::process::Command;

/// Run a program and return its stdout, insisting it exited cleanly.
///
/// A crash is the failure this file is about, so the exit status matters as much as
/// the output: a recursion that dies on the stack guard page produces no output at
/// all, which an output-only assertion would report as a mismatch rather than as the
/// crash it is.
fn run(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("the CLI runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "exited {:?} — a stack-guard SIGSEGV here is the bug this test exists for\
         \nstdout: {stdout}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// The original reproducer, reduced to what actually breaks: one condition defined
/// in another package, then an ordinary one, then instantiating the ordinary one.
#[test]
fn a_foreign_condition_named_error_does_not_break_make_condition() {
    let stdout = run(
        r#"
        (defpackage "KLIZ" (:use "COMMON-LISP") (:shadow "ERROR"))
        (eval (read-from-string
               "(define-condition kliz::error (cl:error) ((k :initarg :k)))"))
        (define-condition kliz-later (error) ((k :initarg :k)))
        (format t "MADE ~a~%" (typep (make-condition 'kliz-later :k 5) 'kliz-later))
        "#,
    );
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("MADE "))
            .unwrap_or(""),
        "MADE T"
    );
}

/// Each package's type keeps its own identity, its own slots, and its own place in
/// the hierarchy — and COMMON-LISP's is untouched.
#[test]
fn each_packages_type_of_a_shared_bare_name_stays_its_own() {
    let stdout = run(
        r#"
        (defpackage "KLIZ2" (:use "COMMON-LISP") (:shadow "ERROR" "LIST"))
        (eval (read-from-string "(progn
          (define-condition kliz2::error (cl:error) ((k :initarg :k :reader kliz2::k)))
          (defclass kliz2::list () ()))"))
        (format t "IDENTITY ~S~%"
                (list
                 ;; The foreign condition is catchable as itself, with its own slot.
                 (handler-case (error 'kliz2::error :k 7) (kliz2::error (e) (kliz2::k e)))
                 ;; And is a distinct class from CL's.
                 (not (eq (find-class 'kliz2::error) (find-class 'cl:error)))
                 ;; While still inheriting from it, which is what it was declared to do.
                 (typep (make-condition 'kliz2::error :k 1) 'cl:error)
                 ;; COMMON-LISP's own type is undisturbed.
                 (handler-case (error "plain") (error () :caught))
                 ;; The same for a plain DEFCLASS, which is the other registry.
                 (and (find-class 'cl:list) (typep '(1 2) 'cl:list))
                 (not (eq (find-class 'kliz2::list) (find-class 'cl:list)))))
        "#,
    );
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("IDENTITY "))
            .unwrap_or(""),
        "IDENTITY (7 T T :CAUGHT T T)"
    );
}

/// COMMON-LISP's own names must keep resolving whether written bare or qualified —
/// the normalization that makes `ERROR` and `COMMON-LISP:ERROR` one key.
#[test]
fn a_standard_type_resolves_bare_or_qualified() {
    let stdout = run(
        r#"
        (define-condition kliz3-c (cl:error) ((k :initarg :k)))
        (format t "STANDARD ~S~%"
                (list (eq (find-class 'error) (find-class 'cl:error))
                      (eq (find-class 'common-lisp:error) (find-class 'cl:error))
                      (typep (make-condition 'kliz3-c :k 1) 'error)
                      (typep (make-condition 'kliz3-c :k 1) 'common-lisp:error)
                      (typep (make-condition 'kliz3-c :k 1) 'serious-condition)))
        "#,
    );
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("STANDARD "))
            .unwrap_or(""),
        "STANDARD (T T T T T)"
    );
}
