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

/// A handler must catch the condition type it named, and nothing else that merely
/// shares its bare name (bliss-tnavc).
///
/// Handler matching compared class-hierarchy names as BARE strings, so a handler for
/// one package's `ERROR` caught a plain `CL:ERROR`, and caught an unrelated package's
/// `ERROR` too. Swallowing conditions it never asked for is the worst thing a handler
/// can do, because nothing reports it — the error simply vanishes into the wrong
/// clause.
#[test]
fn a_handler_catches_only_the_type_it_named() {
    let stdout = run(
        r#"
        (defpackage "KLIZ4A" (:use "COMMON-LISP") (:shadow "ERROR"))
        (defpackage "KLIZ4B" (:use "COMMON-LISP") (:shadow "ERROR"))
        (eval (read-from-string "(progn
          (define-condition kliz4a::error (cl:error) ((k :initarg :k :initform 1)))
          (define-condition kliz4b::error (cl:error) ((k :initarg :k :initform 2))))"))
        (format t "MATCH ~S~%"
                (list
                 ;; Must NOT catch a plain CL:ERROR.
                 (handler-case (error "plain")
                   (kliz4a::error () :wrong) (error () :right))
                 ;; Must NOT catch the other package's type of the same bare name.
                 (handler-case (error 'kliz4b::error)
                   (kliz4a::error () :wrong) (error () :right))
                 ;; MUST catch its own.
                 (handler-case (error 'kliz4a::error)
                   (kliz4a::error () :right) (error () :wrong))
                 ;; And CL:ERROR must still catch both, which they inherit from.
                 (handler-case (error 'kliz4a::error) (cl:error () :right))
                 (handler-case (error 'kliz4b::error) (cl:error () :right))))
        "#,
    );
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("MATCH "))
            .unwrap_or(""),
        "MATCH (:RIGHT :RIGHT :RIGHT :RIGHT :RIGHT)"
    );
}

/// The standard condition types, and TorCL's own qualified one, must keep matching —
/// both by their own name and as the supertypes they inherit from.
///
/// This is the half a package-aware matcher can easily break, and the half that
/// matters most: nearly every HANDLER-CASE in existence names one of these.
#[test]
fn standard_and_qualified_condition_types_still_match() {
    let stdout = run(
        r#"
        (format t "STD ~S~%"
                (list
                 (handler-case (error "boom") (simple-error () :ok))
                 (handler-case (error "boom") (error () :ok))
                 (handler-case (error "boom") (serious-condition () :ok))
                 (handler-case (error "boom") (condition () :ok))
                 (handler-case (/ 1 0) (division-by-zero () :ok))
                 (handler-case (/ 1 0) (arithmetic-error () :ok))
                 (handler-case (symbol-value 'no-such-variable-kliz) (unbound-variable () :ok))
                 (handler-case (car 5) (type-error () :ok))
                 ;; A qualified condition TorCL itself signals, by its own name and
                 ;; as the ERROR it inherits from.
                 (handler-case (torcl-ffi:load-foreign-library "/nonexistent-kliz.so")
                   (torcl-ffi:ffi-error () :ok))
                 (handler-case (torcl-ffi:load-foreign-library "/nonexistent-kliz.so")
                   (error () :ok))
                 ;; And T catches anything.
                 (handler-case (error "boom") (t () :ok))))
        "#,
    );
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("STD "))
            .unwrap_or(""),
        "STD (:OK :OK :OK :OK :OK :OK :OK :OK :OK :OK :OK)"
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
