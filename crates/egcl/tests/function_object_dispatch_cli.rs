//! A function reached only through its OBJECT — `(funcall #'f x)`, `(mapcar #'f
//! l)`, a function held in a variable or a slot — must tier up like one called
//! in operator position, and must keep answering correctly once it does
//! (bliss-fyofj). Before, `apply_function`'s heap-function branch consulted the
//! bytecode registry but never lazily compiled into it, so such a function ran
//! the tree-walker forever: measured at 17 Rust heap allocations per
//! `(funcall #'f i)` against 0 for `(f i)`, which is what made a metaprogram
//! built out of higher-order calls unaffordable.
//!
//! The other half of that measurement was the loop back-edge: `dotimes`/`do`/
//! `loop` lower to tagbody + `go`, and a local `go` runs the unwind driver,
//! which cloned the innermost handler. For a tagbody whose body reifies a
//! closure or a `#'function` that handler carries a control token and a tag
//! table, so the clone allocated a String and a Vec on every iteration. The
//! cases below pin the semantics that fast path must preserve: a `go` that
//! really does have something to unwind, and a non-local `go` out of a closure.
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

#[test]
fn a_function_object_called_in_a_hot_loop_keeps_its_results() {
    // 500 calls is well past the lazy-compile threshold (8), so the tail of this
    // loop runs the registered bytecode rather than the tree-walker.
    assert_eq!(
        eval(
            r#"(progn (defun inc (x) (+ x 1))
                 (format t "RESULT:~s"
                   (let ((f #'inc) (acc 0))
                     (dotimes (i 500) (setf acc (funcall f i)))
                     acc)))"#
        ),
        "500"
    );
}

#[test]
fn redefining_a_hot_function_object_callee_takes_effect() {
    // The first loop promotes G; the second must run the NEW definition, not the
    // bytecode registered for the old one.
    assert_eq!(
        eval(
            r#"(progn (defun g (x) (* x 2))
                 (let ((before 0) (after 0))
                   (dotimes (i 300) (setf before (funcall #'g 5)))
                   (defun g (x) (* x 3))
                   (dotimes (i 300) (setf after (funcall #'g 5)))
                   (format t "RESULT:~s" (list before after))))"#
        ),
        "(10 15)"
    );
}

#[test]
fn a_retained_function_object_keeps_running_its_own_code_after_a_redefinition() {
    // The dangerous shape for the lazy compile this branch now performs: the
    // object in *OLD* is no longer what the symbol F names, so compiling its
    // body under F would install the OLD code as the new global definition.
    // The compile is therefore gated on the object still BEING the symbol's
    // definition, and the stale object keeps running interpreted.
    assert_eq!(
        eval(
            r#"(progn (defun f (x) x)
                 (defvar *old* #'f)
                 (defun f (x) (* 2 x))
                 (let ((a 0) (b 0))
                   (dotimes (i 300) (setf a (funcall *old* 5)))
                   (dotimes (i 300) (setf b (f 5)))
                   (format t "RESULT:~s" (list a b))))"#
        ),
        "(5 10)"
    );
}

#[test]
fn a_hot_closure_object_still_sees_its_captured_environment() {
    assert_eq!(
        eval(
            r#"(progn (defun make-adder (k) (lambda (x) (+ x k)))
                 (let ((a (make-adder 10)) (r 0))
                   (dotimes (i 300) (setf r (funcall a 5)))
                   (format t "RESULT:~s" r)))"#
        ),
        "15"
    );
}

#[test]
fn mapcar_over_a_function_object_stays_correct_when_hot() {
    assert_eq!(
        eval(
            r#"(progn (defun dbl (x) (* 2 x))
                 (let ((last nil))
                   (dotimes (i 300) (setf last (mapcar #'dbl (list i 1 2))))
                   (format t "RESULT:~s" last)))"#
        ),
        "(598 2 4)"
    );
}

#[test]
fn a_local_go_out_of_an_unwind_protect_still_runs_the_cleanup() {
    // The innermost live handler at the GO is the UNWIND-PROTECT, not the
    // tagbody, so the unwind driver must take its slow path and run the cleanup.
    assert_eq!(
        eval(
            r#"(let ((log '()))
                 (tagbody
                  top
                    (push :body log)
                    (unwind-protect (go done) (push :cleanup log))
                    (push :unreachable log)
                  done
                    (push :done log))
                 (format t "RESULT:~s" (reverse log)))"#
        ),
        "(:BODY :CLEANUP :DONE)"
    );
}

#[test]
fn a_non_local_go_out_of_a_closure_still_reaches_its_tag() {
    assert_eq!(
        eval(
            r#"(let ((out '()))
                 (tagbody
                   (funcall (lambda () (go skip)))
                   (push :not-skipped out)
                  skip
                   (push :skipped out))
                 (format t "RESULT:~s" out))"#
        ),
        "(:SKIPPED)"
    );
}

#[test]
fn a_loop_whose_body_names_a_function_still_iterates_every_step() {
    // `#'idf` in the body makes this a NAMED tagbody, the shape whose back-edge
    // used to clone a String and a Vec per iteration.
    assert_eq!(
        eval(
            r#"(progn (defun idf (x) x)
                 (let ((n 0))
                   (dotimes (i 1000) (incf n (funcall #'idf 1)))
                   (format t "RESULT:~s" n)))"#
        ),
        "1000"
    );
}
